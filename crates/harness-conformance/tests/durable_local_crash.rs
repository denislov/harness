use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use harness_agent::{
    AgentActorConfig, AgentExitReason, AgentLlmRuntime, spawn_agent_with_capabilities,
};
use harness_conformance::{
    AppendFault, FaultInjectingSessionStore, ScriptedLlm, TestEventSource, build_tool_runtime,
    create_session, model_config, read_all, text_script, tool_call_script, tool_definition,
    user_message, wait_for_quiescent,
};
use harness_session::SessionEventPayload;
use harness_storage::BlobStore;
use harness_storage_local::DurableLocalStorage;
use harness_tools::{AllowAllToolPolicy, ToolExecutionFuture, ToolExecutor, ToolInvocation};
use harness_types::{
    AgentInstanceId, ContentBlock, ProviderId, SessionId, SideEffectClass, ToolOutcome,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

struct CountingTool {
    provider_id: ProviderId,
    calls: AtomicU64,
}

impl CountingTool {
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

impl ToolExecutor for CountingTool {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn invoke(&self, _invocation: ToolInvocation) -> ToolExecutionFuture {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {
            Ok(ToolOutcome::Success {
                content: vec![ContentBlock::text("durable-tool-ok")],
            })
        })
    }
}

fn temp_root() -> PathBuf {
    let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "harness-batch20-durable-crash-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    root
}

#[tokio::test]
async fn sqlite_reopen_after_dispatch_ack_loss_retries_without_losing_blob_snapshots() {
    let root = temp_root();
    let session_id = SessionId::new("ses_batch20_durable").unwrap();
    let llm_provider_id = ProviderId::new("prv_batch20_durable_llm").unwrap();
    let event_source = Arc::new(TestEventSource::new(70_000));
    let llm = Arc::new(ScriptedLlm::new(
        llm_provider_id.clone(),
        vec![
            tool_call_script("call_batch20_durable", "durable_read", r#"{"value":"x"}"#),
            text_script("durable-finished"),
        ],
    ));
    let tool = Arc::new(CountingTool::new(
        ProviderId::new("prv_batch20_durable_tool").unwrap(),
    ));

    let storage = DurableLocalStorage::open(root.clone()).unwrap();
    create_session(
        storage.session_store().as_ref(),
        event_source.as_ref(),
        session_id.clone(),
    )
    .await;
    let fault_store = Arc::new(FaultInjectingSessionStore::new(
        storage.session_store(),
        AppendFault::after_event_type("tool/dispatched"),
    ));

    let first = spawn_agent_with_capabilities(
        AgentInstanceId::new("agt_batch20_durable_first").unwrap(),
        session_id.clone(),
        fault_store.clone(),
        event_source.clone(),
        AgentLlmRuntime::new(
            model_config(llm_provider_id.clone()),
            llm.clone(),
            storage.blob_store(),
        )
        .unwrap(),
        build_tool_runtime(
            tool_definition("durable_read", SideEffectClass::ReadOnly),
            tool.clone(),
            Arc::new(AllowAllToolPolicy),
            2,
        ),
        AgentActorConfig::default(),
    )
    .await
    .unwrap();
    first
        .handle
        .followup(user_message("msg_batch20_durable", "durable read"))
        .await
        .unwrap();

    for _ in 0..5_000 {
        if first.task.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(first.task.is_finished());
    let exit = first.task.join().await.unwrap();
    assert!(matches!(exit.reason, AgentExitReason::Fatal(_)));
    assert!(fault_store.triggered());
    assert_eq!(
        tool.calls(),
        0,
        "tool invocation must not cross the external boundary when dispatch acknowledgement is lost"
    );

    drop(fault_store);
    drop(storage);

    let reopened = DurableLocalStorage::open(root.clone()).unwrap();
    let resumed = spawn_agent_with_capabilities(
        AgentInstanceId::new("agt_batch20_durable_resumed").unwrap(),
        session_id.clone(),
        reopened.session_store(),
        event_source,
        AgentLlmRuntime::new(
            model_config(llm_provider_id),
            llm.clone(),
            reopened.blob_store(),
        )
        .unwrap(),
        build_tool_runtime(
            tool_definition("durable_read", SideEffectClass::ReadOnly),
            tool.clone(),
            Arc::new(AllowAllToolPolicy),
            2,
        ),
        AgentActorConfig::default(),
    )
    .await
    .unwrap();
    wait_for_quiescent(&resumed.handle, 4).await;

    assert_eq!(tool.calls(), 1);
    assert_eq!(llm.requests().len(), 2);
    let events = read_all(reopened.session_store().as_ref(), &session_id).await;
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
    assert_eq!(dispatches[0].idempotency_key, dispatches[1].idempotency_key);

    let request_snapshots = events
        .iter()
        .filter_map(|event| match event.payload() {
            SessionEventPayload::ModelRequested(data) => Some(data.request_snapshot.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(request_snapshots.len(), 2);
    for snapshot in &request_snapshots {
        reopened.blob_store().verify(snapshot).await.unwrap();
    }

    resumed.handle.shutdown().await.unwrap();
    resumed.task.join().await.unwrap();
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}
