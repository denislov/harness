use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use harness_agent::{AgentEventSource, AgentState};
use harness_llm::ModelOptions;
use harness_runtime::{
    AgentProfile, HarnessRuntime, HarnessRuntimeBuilder, ModelBinding, ProviderProcessSpec,
    RuntimeIdSource, RuntimeToolBinding,
};
use harness_session::{SessionEventPayload, SessionStore};
use harness_storage::BlobStore;
use harness_tools::{
    AllowAllToolPolicy, ToolArgumentValidationError, ToolArgumentValidator, ToolDefinition,
};
use harness_types::{
    AgentInstanceId, ContentBlock, EventId, EventSeq, JsonText, Message, MessageId, MessageSource,
    ProviderId, Role, SessionId, SideEffectClass, Timestamp,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

struct TestIdentitySource {
    next: AtomicU64,
    timestamp: Timestamp,
}

impl TestIdentitySource {
    fn new(start: u64, timestamp: &str) -> Self {
        Self {
            next: AtomicU64::new(start),
            timestamp: Timestamp::parse(timestamp).unwrap(),
        }
    }

    fn next_value(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

impl AgentEventSource for TestIdentitySource {
    fn next_event_id(&self) -> EventId {
        EventId::new(format!("evt_restart_{}", self.next_value())).unwrap()
    }

    fn now(&self) -> Timestamp {
        self.timestamp
    }
}

impl RuntimeIdSource for TestIdentitySource {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(format!("ses_restart_{}", self.next_value())).unwrap()
    }

    fn next_agent_instance_id(&self) -> AgentInstanceId {
        AgentInstanceId::new(format!("agt_restart_{}", self.next_value())).unwrap()
    }
}

struct ObjectValidator;

impl ToolArgumentValidator for ObjectValidator {
    fn validate(
        &self,
        _definition: &ToolDefinition,
        arguments_json: &JsonText,
    ) -> Result<(), ToolArgumentValidationError> {
        let value: serde_json::Value = serde_json::from_str(arguments_json.as_str())
            .map_err(|error| ToolArgumentValidationError::new(error.to_string()))?;
        if value.is_object() {
            Ok(())
        } else {
            Err(ToolArgumentValidationError::new(
                "arguments must be a JSON object",
            ))
        }
    }
}

fn provider_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../providers/example-python/provider.py")
}

fn python_program() -> String {
    std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_owned())
}

fn temp_root() -> PathBuf {
    let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "harness-runtime-restart-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    path
}

fn user_message(message_id: &str, text: &str) -> Message {
    Message {
        id: MessageId::new(message_id).unwrap(),
        role: Role::User,
        source: MessageSource::user(),
        content: vec![ContentBlock::text(text)],
    }
}

fn profile(provider_id: &ProviderId) -> AgentProfile {
    let tool_definition = ToolDefinition {
        name: "echo".to_owned(),
        version: "1".to_owned(),
        description: "Echo a JSON object through the Python provider".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
            "additionalProperties": false
        }),
        output_schema: None,
        parallel_safe: true,
        side_effect: SideEffectClass::ReadOnly,
        default_timeout_ms: 5_000,
    };
    AgentProfile::new(
        ModelBinding::new(provider_id.clone(), "agent-model")
            .with_system("Use the echo tool, then answer with its result.")
            .with_options(ModelOptions {
                max_output_tokens: Some(256),
            })
            .with_timeout_ms(5_000),
        Arc::new(AllowAllToolPolicy),
    )
    .with_tool(RuntimeToolBinding::new(
        tool_definition,
        provider_id.clone(),
        Arc::new(ObjectValidator),
    ))
}

fn provider_spec(provider_id: &ProviderId) -> ProviderProcessSpec {
    ProviderProcessSpec::new(provider_id.clone(), python_program())
        .arg(provider_script().into_os_string())
        .request_timeout(Duration::from_secs(5))
        .shutdown_timeout(Duration::from_secs(2))
}

async fn build_runtime(root: &PathBuf, identity: Arc<TestIdentitySource>) -> HarnessRuntime {
    let provider_id = ProviderId::new("example-python").unwrap();
    HarnessRuntimeBuilder::durable_local(root.clone(), identity.clone(), identity)
        .unwrap()
        .provider(provider_spec(&provider_id))
        .profile("python-agent", profile(&provider_id))
        .build()
        .await
        .unwrap()
}

async fn wait_for_messages(
    handle: &harness_agent::AgentHandle,
    expected_messages: usize,
) -> AgentState {
    for _ in 0..5000 {
        let state = handle.snapshot().await.unwrap();
        if state.active_operation.is_none()
            && state.projection.lifecycle.open_turn.is_none()
            && state.projection.model_messages.len() == expected_messages
        {
            return state;
        }
        tokio::task::yield_now().await;
    }
    panic!("durable HarnessRuntime Agent did not converge within test yield budget");
}

#[tokio::test]
async fn durable_runtime_reopens_session_blobs_and_agent_projection() {
    let script = provider_script();
    assert!(script.is_file(), "missing provider at {}", script.display());
    let root = temp_root();

    let runtime = build_runtime(
        &root,
        Arc::new(TestIdentitySource::new(100, "2026-08-20T08:40:00Z")),
    )
    .await;
    let session_id = runtime.create_session().await.unwrap();
    let handle = runtime
        .open_agent(session_id.clone(), "python-agent")
        .await
        .unwrap();
    handle
        .followup(user_message("msg_restart_before", "before restart"))
        .await
        .unwrap();
    let first_state = wait_for_messages(&handle, 4).await;
    assert_eq!(
        first_state
            .projection
            .model_messages
            .last()
            .unwrap()
            .content,
        vec![ContentBlock::text("final: {\"text\": \"before restart\"}")]
    );
    runtime.close_agent(&session_id).await.unwrap();
    runtime.shutdown().await.unwrap();
    drop(runtime);

    // A new Runtime with new process-local identity sources opens the same local
    // database/blob directory and replays the existing Session projection.
    let reopened = build_runtime(
        &root,
        Arc::new(TestIdentitySource::new(10_000, "2026-08-20T08:41:00Z")),
    )
    .await;
    let events = SessionStore::read(
        reopened.session_store().as_ref(),
        &session_id,
        EventSeq::FIRST,
        128,
    )
    .await
    .unwrap();
    let snapshots: Vec<_> = events
        .iter()
        .filter_map(|event| match event.payload() {
            SessionEventPayload::ModelRequested(requested) => {
                Some(requested.request_snapshot.clone())
            }
            _ => None,
        })
        .collect();
    assert!(!snapshots.is_empty());
    for snapshot in &snapshots {
        reopened.blob_store().verify(snapshot).await.unwrap();
    }

    let handle = reopened
        .open_agent(session_id.clone(), "python-agent")
        .await
        .unwrap();
    let replayed = handle.snapshot().await.unwrap();
    assert_eq!(replayed.projection.model_messages.len(), 4);

    handle
        .followup(user_message("msg_restart_after", "after restart"))
        .await
        .unwrap();
    let second_state = wait_for_messages(&handle, 8).await;
    assert_eq!(
        second_state
            .projection
            .model_messages
            .last()
            .unwrap()
            .content,
        vec![ContentBlock::text("final: {\"text\": \"after restart\"}")]
    );

    reopened.shutdown().await.unwrap();
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}
