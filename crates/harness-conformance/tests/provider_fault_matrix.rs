use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use harness_agent::{AgentActorConfig, AgentLlmRuntime, AgentState, spawn_agent_with_capabilities};
use harness_conformance::{
    ScriptedLlm, TestEventSource, build_tool_runtime, create_session, model_config, read_all,
    text_script, tool_call_script, tool_definition, user_message, wait_for_quiescent,
};
use harness_session::{SessionEventPayload, TurnEndReason};
use harness_storage_local::{MemoryBlobStore, MemorySessionStore};
use harness_tools::{
    AllowAllToolPolicy, IdempotencySupport, ToolExecutionFuture, ToolExecutor, ToolInvocation,
};
use harness_types::{
    AgentInstanceId, ContentBlock, ErrorCode, PortableError, ProviderId, SessionId,
    SideEffectClass, ToolOutcome,
};

struct RetryingReadTool {
    provider_id: ProviderId,
    calls: AtomicU64,
}

impl RetryingReadTool {
    fn new(provider_id: ProviderId) -> Self {
        Self {
            provider_id,
            calls: AtomicU64::new(0),
        }
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }
}

impl ToolExecutor for RetryingReadTool {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn invoke(&self, _invocation: ToolInvocation) -> ToolExecutionFuture {
        let attempt = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        Box::pin(async move {
            if attempt == 1 {
                Err(PortableError::new(
                    ErrorCode::ProviderUnavailable,
                    "simulated Provider crash before authoritative read result",
                ))
            } else {
                Ok(ToolOutcome::Success {
                    content: vec![ContentBlock::text("read-after-retry")],
                })
            }
        })
    }
}

struct DeduplicatingWriteTool {
    provider_id: ProviderId,
    calls: AtomicU64,
    effects: AtomicU64,
    seen_keys: Mutex<HashSet<String>>,
}

impl DeduplicatingWriteTool {
    fn new(provider_id: ProviderId) -> Self {
        Self {
            provider_id,
            calls: AtomicU64::new(0),
            effects: AtomicU64::new(0),
            seen_keys: Mutex::new(HashSet::new()),
        }
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    fn effects(&self) -> u64 {
        self.effects.load(Ordering::Relaxed)
    }
}

impl ToolExecutor for DeduplicatingWriteTool {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn idempotency_support(&self) -> IdempotencySupport {
        IdempotencySupport::Keyed
    }

    fn invoke(&self, invocation: ToolInvocation) -> ToolExecutionFuture {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let key = invocation.idempotency_key.to_string();
        let first_for_key = self
            .seen_keys
            .lock()
            .expect("dedupe key lock poisoned")
            .insert(key);
        if first_for_key {
            self.effects.fetch_add(1, Ordering::Relaxed);
        }
        Box::pin(async move {
            if first_for_key {
                Err(PortableError::new(
                    ErrorCode::ProviderUnavailable,
                    "simulated Provider crash after idempotent side effect but before reply",
                ))
            } else {
                Ok(ToolOutcome::Success {
                    content: vec![ContentBlock::text("write-deduplicated")],
                })
            }
        })
    }
}

struct UncertainWriteTool {
    provider_id: ProviderId,
    calls: AtomicU64,
    effects: AtomicU64,
}

impl UncertainWriteTool {
    fn new(provider_id: ProviderId) -> Self {
        Self {
            provider_id,
            calls: AtomicU64::new(0),
            effects: AtomicU64::new(0),
        }
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    fn effects(&self) -> u64 {
        self.effects.load(Ordering::Relaxed)
    }
}

impl ToolExecutor for UncertainWriteTool {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn invoke(&self, _invocation: ToolInvocation) -> ToolExecutionFuture {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.effects.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {
            Err(PortableError::new(
                ErrorCode::ProviderUnavailable,
                "simulated Provider crash after non-idempotent side effect",
            ))
        })
    }
}

#[tokio::test]
async fn provider_crash_before_read_result_retries_from_durable_dispatch() {
    let session_id = SessionId::new("ses_provider_read_retry").unwrap();
    let llm_provider_id = ProviderId::new("prv_provider_fault_llm").unwrap();
    let store = Arc::new(MemorySessionStore::new());
    let event_source = Arc::new(TestEventSource::new(40_000));
    create_session(store.as_ref(), event_source.as_ref(), session_id.clone()).await;
    let blobs = Arc::new(MemoryBlobStore::new());
    let llm = Arc::new(ScriptedLlm::new(
        llm_provider_id.clone(),
        vec![
            tool_call_script("call_read_retry", "read_retry", r#"{"value":"x"}"#),
            text_script("read-finished"),
        ],
    ));
    let tool = Arc::new(RetryingReadTool::new(
        ProviderId::new("prv_read_tool").unwrap(),
    ));

    let spawned = spawn_agent_with_capabilities(
        AgentInstanceId::new("agt_provider_read_retry").unwrap(),
        session_id.clone(),
        store.clone(),
        event_source,
        AgentLlmRuntime::new(model_config(llm_provider_id), llm.clone(), blobs).unwrap(),
        build_tool_runtime(
            tool_definition("read_retry", SideEffectClass::ReadOnly),
            tool.clone(),
            Arc::new(AllowAllToolPolicy),
            2,
        ),
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    spawned
        .handle
        .followup(user_message("msg_provider_read_retry", "read"))
        .await
        .unwrap();
    wait_for_quiescent(&spawned.handle, 4).await;

    assert_eq!(tool.calls(), 2);
    assert_eq!(llm.requests().len(), 2);
    let events = read_all(store.as_ref(), &session_id).await;
    assert_retry_dispatches_share_key(&events);

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}

#[tokio::test]
async fn provider_crash_after_idempotent_write_reuses_key_and_deduplicates_effect() {
    let session_id = SessionId::new("ses_provider_idempotent_write").unwrap();
    let llm_provider_id = ProviderId::new("prv_provider_fault_llm_write").unwrap();
    let store = Arc::new(MemorySessionStore::new());
    let event_source = Arc::new(TestEventSource::new(50_000));
    create_session(store.as_ref(), event_source.as_ref(), session_id.clone()).await;
    let blobs = Arc::new(MemoryBlobStore::new());
    let llm = Arc::new(ScriptedLlm::new(
        llm_provider_id.clone(),
        vec![
            tool_call_script("call_write_retry", "write_retry", r#"{"value":"x"}"#),
            text_script("write-finished"),
        ],
    ));
    let tool = Arc::new(DeduplicatingWriteTool::new(
        ProviderId::new("prv_write_tool").unwrap(),
    ));

    let spawned = spawn_agent_with_capabilities(
        AgentInstanceId::new("agt_provider_idempotent_write").unwrap(),
        session_id.clone(),
        store.clone(),
        event_source,
        AgentLlmRuntime::new(model_config(llm_provider_id), llm.clone(), blobs).unwrap(),
        build_tool_runtime(
            tool_definition("write_retry", SideEffectClass::IdempotentWrite),
            tool.clone(),
            Arc::new(AllowAllToolPolicy),
            2,
        ),
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    spawned
        .handle
        .followup(user_message("msg_provider_idempotent_write", "write"))
        .await
        .unwrap();
    wait_for_quiescent(&spawned.handle, 4).await;

    assert_eq!(tool.calls(), 2, "Core must retry the ambiguous keyed write");
    assert_eq!(
        tool.effects(),
        1,
        "provider keyed idempotency must suppress the duplicate side effect"
    );
    assert_eq!(llm.requests().len(), 2);
    let events = read_all(store.as_ref(), &session_id).await;
    assert_retry_dispatches_share_key(&events);

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}

#[tokio::test]
async fn provider_crash_after_non_idempotent_write_blocks_without_redispatch() {
    let session_id = SessionId::new("ses_provider_non_idempotent_write").unwrap();
    let llm_provider_id = ProviderId::new("prv_provider_fault_llm_nonidem").unwrap();
    let store = Arc::new(MemorySessionStore::new());
    let event_source = Arc::new(TestEventSource::new(60_000));
    create_session(store.as_ref(), event_source.as_ref(), session_id.clone()).await;
    let blobs = Arc::new(MemoryBlobStore::new());
    let llm = Arc::new(ScriptedLlm::new(
        llm_provider_id.clone(),
        vec![tool_call_script(
            "call_nonidem",
            "dangerous_write",
            r#"{"value":"x"}"#,
        )],
    ));
    let tool = Arc::new(UncertainWriteTool::new(
        ProviderId::new("prv_nonidem_tool").unwrap(),
    ));

    let spawned = spawn_agent_with_capabilities(
        AgentInstanceId::new("agt_provider_non_idempotent_write").unwrap(),
        session_id.clone(),
        store.clone(),
        event_source,
        AgentLlmRuntime::new(model_config(llm_provider_id), llm.clone(), blobs).unwrap(),
        build_tool_runtime(
            tool_definition("dangerous_write", SideEffectClass::NonIdempotentWrite),
            tool.clone(),
            Arc::new(AllowAllToolPolicy),
            2,
        ),
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    spawned
        .handle
        .followup(user_message("msg_provider_nonidem", "dangerous write"))
        .await
        .unwrap();
    let state = wait_for_blocked(&spawned.handle).await;
    assert!(state.projection.unresolved_recovery.is_some());
    assert_eq!(tool.calls(), 1);
    assert_eq!(tool.effects(), 1);
    assert_eq!(llm.requests().len(), 1);

    let events = read_all(store.as_ref(), &session_id).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), SessionEventPayload::ToolDispatched(_)))
            .count(),
        1,
        "non-idempotent ambiguous dispatch must never be retried automatically"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), SessionEventPayload::RecoveryBlocked(_)))
            .count(),
        1
    );
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        SessionEventPayload::TurnEnded(data) if data.reason == TurnEndReason::Blocked
    )));

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}

async fn wait_for_blocked(handle: &harness_agent::AgentHandle) -> AgentState {
    for _ in 0..5_000 {
        let state = handle.snapshot().await.unwrap();
        if state.active_operation.is_none()
            && state.projection.lifecycle.open_turn.is_none()
            && state.projection.unresolved_recovery.is_some()
        {
            return state;
        }
        tokio::task::yield_now().await;
    }
    panic!("non-idempotent Provider fault did not converge to a durable recovery block");
}

fn assert_retry_dispatches_share_key(events: &[harness_session::SessionEvent]) {
    let dispatches = events
        .iter()
        .filter_map(|event| match event.payload() {
            SessionEventPayload::ToolDispatched(data) => Some(data),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(dispatches.len(), 2);
    assert_eq!(dispatches[0].attempt, 1);
    assert_eq!(dispatches[1].attempt, 2);
    assert_eq!(dispatches[0].provider_id, dispatches[1].provider_id);
    assert_eq!(dispatches[0].idempotency_key, dispatches[1].idempotency_key);
}
