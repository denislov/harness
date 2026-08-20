use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use harness_agent::{
    AgentActorConfig, AgentExit, AgentExitReason, AgentLlmRuntime, AgentTask,
    spawn_agent_with_capabilities,
};
use harness_conformance::{
    AppendFault, FaultInjectingSessionStore, ScriptedLlm, TestEventSource, build_tool_runtime,
    create_session, model_config, read_all, text_script, tool_call_script, tool_definition,
    user_message, wait_for_quiescent,
};
use harness_session::{SessionEventPayload, SessionProjector, V1SessionProjector};
use harness_storage_local::{MemoryBlobStore, MemorySessionStore};
use harness_tools::{AllowAllToolPolicy, ToolExecutionFuture, ToolExecutor, ToolInvocation};
use harness_types::{
    AgentInstanceId, ContentBlock, ProviderId, SessionId, SideEffectClass, ToolOutcome,
};

#[derive(Clone, Copy, Debug)]
struct CrashBoundary {
    label: &'static str,
    event_type: &'static str,
    occurrence: usize,
    expected_dispatches: usize,
    expected_model_requested: usize,
    expected_model_failed: usize,
}

const CRASH_BOUNDARIES: &[CrashBoundary] = &[
    CrashBoundary {
        label: "step_entry",
        event_type: "user/message",
        occurrence: 1,
        expected_dispatches: 1,
        expected_model_requested: 2,
        expected_model_failed: 0,
    },
    CrashBoundary {
        label: "model_requested",
        event_type: "model/requested",
        occurrence: 1,
        expected_dispatches: 1,
        expected_model_requested: 3,
        expected_model_failed: 1,
    },
    CrashBoundary {
        label: "assistant_message",
        event_type: "assistant/message",
        occurrence: 1,
        expected_dispatches: 1,
        expected_model_requested: 2,
        expected_model_failed: 0,
    },
    CrashBoundary {
        label: "terminal_assistant",
        event_type: "assistant/message",
        occurrence: 2,
        expected_dispatches: 1,
        expected_model_requested: 2,
        expected_model_failed: 0,
    },
    CrashBoundary {
        label: "tool_call",
        event_type: "tool/call",
        occurrence: 1,
        expected_dispatches: 1,
        expected_model_requested: 2,
        expected_model_failed: 0,
    },
    CrashBoundary {
        label: "tool_dispatched",
        event_type: "tool/dispatched",
        occurrence: 1,
        expected_dispatches: 2,
        expected_model_requested: 2,
        expected_model_failed: 0,
    },
    CrashBoundary {
        label: "tool_result",
        event_type: "tool/result",
        occurrence: 1,
        expected_dispatches: 1,
        expected_model_requested: 2,
        expected_model_failed: 0,
    },
    CrashBoundary {
        label: "step_ended",
        event_type: "step/ended",
        occurrence: 1,
        expected_dispatches: 1,
        expected_model_requested: 2,
        expected_model_failed: 0,
    },
    CrashBoundary {
        label: "turn_ended",
        event_type: "turn/ended",
        occurrence: 1,
        expected_dispatches: 1,
        expected_model_requested: 2,
        expected_model_failed: 0,
    },
];

struct CountingSuccessTool {
    provider_id: ProviderId,
    calls: AtomicU64,
}

impl CountingSuccessTool {
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

impl ToolExecutor for CountingSuccessTool {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn invoke(&self, _invocation: ToolInvocation) -> ToolExecutionFuture {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {
            Ok(ToolOutcome::Success {
                content: vec![ContentBlock::text("tool-ok")],
            })
        })
    }
}

async fn await_fatal(task: AgentTask, label: &str) -> AgentExit {
    for _ in 0..5_000 {
        if task.is_finished() {
            let exit = task
                .join()
                .await
                .unwrap_or_else(|error| panic!("{label}: Agent task join failed: {error}"));
            assert!(
                matches!(exit.reason, AgentExitReason::Fatal(_)),
                "{label}: injected acknowledgement loss did not terminate the Agent: {:?}",
                exit.reason
            );
            return exit;
        }
        tokio::task::yield_now().await;
    }
    task.abort();
    let _ = task.join().await;
    panic!("{label}: Agent did not terminate after the injected acknowledgement loss");
}

#[tokio::test]
async fn post_commit_crash_matrix_converges_without_duplicate_tool_effect() {
    for (index, boundary) in CRASH_BOUNDARIES.iter().copied().enumerate() {
        run_boundary(index as u64, boundary).await;
    }
}

async fn run_boundary(index: u64, boundary: CrashBoundary) {
    let session_id = SessionId::new(format!("ses_crash_{}", boundary.label)).unwrap();
    let llm_provider_id = ProviderId::new("prv_conformance_llm").unwrap();
    let tool_provider_id = ProviderId::new("prv_conformance_tool").unwrap();
    let inner = Arc::new(MemorySessionStore::new());
    let event_source = Arc::new(TestEventSource::new(1_000 + index * 1_000));
    create_session(inner.as_ref(), event_source.as_ref(), session_id.clone()).await;

    let blobs = Arc::new(MemoryBlobStore::new());
    let llm = Arc::new(ScriptedLlm::new(
        llm_provider_id.clone(),
        vec![
            tool_call_script("call_crash_matrix", "read_value", r#"{"value":"x"}"#),
            text_script("done"),
        ],
    ));
    let tool = Arc::new(CountingSuccessTool::new(tool_provider_id));
    let fault_store = Arc::new(FaultInjectingSessionStore::new(
        inner.clone(),
        AppendFault::after_event_type_occurrence(boundary.event_type, boundary.occurrence),
    ));

    let first = spawn_agent_with_capabilities(
        AgentInstanceId::new(format!("agt_crash_{}_first", boundary.label)).unwrap(),
        session_id.clone(),
        fault_store.clone(),
        event_source.clone(),
        AgentLlmRuntime::new(
            model_config(llm_provider_id.clone()),
            llm.clone(),
            blobs.clone(),
        )
        .unwrap(),
        build_tool_runtime(
            tool_definition("read_value", SideEffectClass::ReadOnly),
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
        .followup(user_message("msg_crash_matrix", "run the tool"))
        .await
        .unwrap();
    let _ = await_fatal(first.task, boundary.label).await;
    assert!(
        fault_store.triggered(),
        "{}: configured crash boundary was never reached",
        boundary.label
    );

    let resumed = spawn_agent_with_capabilities(
        AgentInstanceId::new(format!("agt_crash_{}_resumed", boundary.label)).unwrap(),
        session_id.clone(),
        inner.clone(),
        event_source.clone(),
        AgentLlmRuntime::new(model_config(llm_provider_id), llm.clone(), blobs.clone()).unwrap(),
        build_tool_runtime(
            tool_definition("read_value", SideEffectClass::ReadOnly),
            tool.clone(),
            Arc::new(AllowAllToolPolicy),
            2,
        ),
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    let state = wait_for_quiescent(&resumed.handle, 4).await;
    assert_eq!(
        state.projection.model_messages.last().unwrap().content,
        vec![ContentBlock::text("done")],
        "{}: resumed final answer changed",
        boundary.label
    );
    assert_eq!(
        tool.calls(),
        1,
        "{}: the logical ToolCall produced a duplicate external invocation",
        boundary.label
    );
    assert_eq!(
        llm.requests().len(),
        2,
        "{}: provider-visible model calls were duplicated",
        boundary.label
    );
    assert_eq!(llm.remaining_scripts(), 0, "{}", boundary.label);

    let events = read_all(inner.as_ref(), &session_id).await;
    let projection = V1SessionProjector.project(&events).unwrap();
    assert!(
        projection.lifecycle.open_turn.is_none(),
        "{}",
        boundary.label
    );
    assert!(
        projection.lifecycle.open_step.is_none(),
        "{}",
        boundary.label
    );
    assert!(
        projection.pending_model_request.is_none(),
        "{}",
        boundary.label
    );
    assert!(
        projection.pending_tool_calls.is_empty(),
        "{}",
        boundary.label
    );
    assert!(
        projection.pending_tool_dispatches.is_empty(),
        "{}",
        boundary.label
    );
    assert!(
        projection.unresolved_recovery.is_none(),
        "{}",
        boundary.label
    );

    let dispatches = events
        .iter()
        .filter_map(|event| match event.payload() {
            SessionEventPayload::ToolDispatched(data) => Some(data.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        dispatches.len(),
        boundary.expected_dispatches,
        "{}: unexpected durable dispatch count",
        boundary.label
    );
    if dispatches.len() == 2 {
        assert_eq!(dispatches[0].attempt, 1);
        assert_eq!(dispatches[1].attempt, 2);
        assert_eq!(dispatches[0].idempotency_key, dispatches[1].idempotency_key);
    }

    let model_requested = events
        .iter()
        .filter(|event| matches!(event.payload(), SessionEventPayload::ModelRequested(_)))
        .count();
    let model_failed = events
        .iter()
        .filter(|event| matches!(event.payload(), SessionEventPayload::ModelFailed(_)))
        .count();
    assert_eq!(
        model_requested, boundary.expected_model_requested,
        "{}: unexpected durable model/requested count",
        boundary.label
    );
    assert_eq!(
        model_failed, boundary.expected_model_failed,
        "{}: unexpected durable model/failed count",
        boundary.label
    );

    resumed.handle.shutdown().await.unwrap();
    resumed.task.join().await.unwrap();
}
