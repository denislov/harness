use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use harness_agent::{
    AgentActorConfig, AgentExitReason, AgentLlmRuntime, spawn_agent_with_capabilities,
};
use harness_conformance::{
    AppendFault, FaultInjectingSessionStore, ScriptedLlm, TestEventSource, build_tool_runtime,
    create_session, model_config, read_all, text_script, tool_call_script, tool_definition,
    user_message, wait_for_pending_approval, wait_for_quiescent,
};
use harness_session::{SessionEventPayload, SessionProjector, V1SessionProjector};
use harness_storage_local::{MemoryBlobStore, MemorySessionStore};
use harness_tools::{
    PolicyDecision, ToolExecutionFuture, ToolExecutor, ToolInvocation, ToolPolicy, ToolPolicyInput,
};
use harness_types::{
    AgentInstanceId, ApprovalDecision, ContentBlock, ProviderId, SessionId, SideEffectClass,
    ToolOutcome,
};

#[derive(Clone, Copy, Debug)]
struct AskPolicy;

impl ToolPolicy for AskPolicy {
    fn evaluate(&self, _input: &ToolPolicyInput) -> PolicyDecision {
        PolicyDecision::Ask {
            reason: "Batch 20 approval fixture".to_owned(),
            risk: "conformance".to_owned(),
        }
    }

    fn composition_identity(&self) -> String {
        "harness-conformance/ask-policy/v1".to_owned()
    }
}

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
                content: vec![ContentBlock::text("approved-tool-ok")],
            })
        })
    }
}

#[tokio::test]
async fn approval_requested_and_resolved_ack_loss_are_replay_safe() {
    run_approval_case("approval_requested", "approval/requested").await;
    run_approval_case("approval_resolved", "approval/resolved").await;
}

async fn run_approval_case(label: &str, crash_event_type: &str) {
    let session_id = SessionId::new(format!("ses_{label}")).unwrap();
    let llm_provider_id = ProviderId::new("prv_approval_llm").unwrap();
    let tool_provider_id = ProviderId::new("prv_approval_tool").unwrap();
    let inner = Arc::new(MemorySessionStore::new());
    let event_source = Arc::new(TestEventSource::new(
        if crash_event_type == "approval/requested" {
            20_000
        } else {
            30_000
        },
    ));
    create_session(inner.as_ref(), event_source.as_ref(), session_id.clone()).await;

    let blobs = Arc::new(MemoryBlobStore::new());
    let llm = Arc::new(ScriptedLlm::new(
        llm_provider_id.clone(),
        vec![
            tool_call_script("call_approval", "approved_tool", r#"{"value":"x"}"#),
            text_script("approved-done"),
        ],
    ));
    let tool = Arc::new(CountingTool::new(tool_provider_id));
    let fault_store = Arc::new(FaultInjectingSessionStore::new(
        inner.clone(),
        AppendFault::after_event_type(crash_event_type),
    ));

    let first = spawn_agent_with_capabilities(
        AgentInstanceId::new(format!("agt_{label}_first")).unwrap(),
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
            tool_definition("approved_tool", SideEffectClass::ReadOnly),
            tool.clone(),
            Arc::new(AskPolicy),
            2,
        ),
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    first
        .handle
        .followup(user_message("msg_approval", "request approval"))
        .await
        .unwrap();

    if crash_event_type == "approval/requested" {
        for _ in 0..5_000 {
            if first.task.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            first.task.is_finished(),
            "approval/requested crash did not terminate Agent"
        );
        let exit = first.task.join().await.unwrap();
        assert!(matches!(exit.reason, AgentExitReason::Fatal(_)));
    } else {
        let pending = wait_for_pending_approval(&first.handle).await;
        let approval_id = pending
            .projection
            .pending_approval
            .as_ref()
            .unwrap()
            .data
            .approval_id
            .clone();
        let result = first
            .handle
            .resolve_approval(
                approval_id,
                ApprovalDecision::Allow,
                Some("allow".to_owned()),
            )
            .await;
        assert!(
            result.is_err(),
            "post-commit acknowledgement loss must hide command success"
        );
        assert!(fault_store.triggered());
        first.task.abort();
        assert!(first.task.join().await.is_err());
    }

    assert!(fault_store.triggered(), "{label}: fault did not trigger");
    assert_eq!(
        tool.calls(),
        0,
        "{label}: Tool crossed the boundary before recovery"
    );

    let resumed = spawn_agent_with_capabilities(
        AgentInstanceId::new(format!("agt_{label}_resumed")).unwrap(),
        session_id.clone(),
        inner.clone(),
        event_source.clone(),
        AgentLlmRuntime::new(model_config(llm_provider_id), llm.clone(), blobs).unwrap(),
        build_tool_runtime(
            tool_definition("approved_tool", SideEffectClass::ReadOnly),
            tool.clone(),
            Arc::new(AskPolicy),
            2,
        ),
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    if crash_event_type == "approval/requested" {
        let pending = wait_for_pending_approval(&resumed.handle).await;
        let approval_id = pending
            .projection
            .pending_approval
            .as_ref()
            .unwrap()
            .data
            .approval_id
            .clone();
        resumed
            .handle
            .resolve_approval(
                approval_id,
                ApprovalDecision::Allow,
                Some("allow".to_owned()),
            )
            .await
            .unwrap();
    }

    let state = wait_for_quiescent(&resumed.handle, 4).await;
    assert_eq!(
        state.projection.model_messages.last().unwrap().content,
        vec![ContentBlock::text("approved-done")]
    );
    assert_eq!(
        tool.calls(),
        1,
        "{label}: approved Tool executed more than once"
    );
    assert_eq!(llm.requests().len(), 2, "{label}: model work was repeated");

    let events = read_all(inner.as_ref(), &session_id).await;
    let projection = V1SessionProjector.project(&events).unwrap();
    assert!(projection.pending_approval.is_none());
    assert!(projection.pending_tool_calls.is_empty());
    assert!(projection.lifecycle.open_turn.is_none());
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), SessionEventPayload::ApprovalRequested(_)))
            .count(),
        1,
        "{label}: approval/requested was duplicated"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), SessionEventPayload::ApprovalResolved(_)))
            .count(),
        1,
        "{label}: approval/resolved was duplicated"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), SessionEventPayload::ToolDispatched(_)))
            .count(),
        1,
        "{label}: approval recovery duplicated dispatch"
    );

    resumed.handle.shutdown().await.unwrap();
    resumed.task.join().await.unwrap();
}
