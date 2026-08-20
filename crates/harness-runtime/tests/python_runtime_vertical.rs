use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use harness_agent::{AgentEventSource, AgentState};
use harness_llm::ModelOptions;
use harness_provider_host::ProviderState;
use harness_runtime::{
    AgentProfile, HarnessRuntimeBuilder, HarnessRuntimeError, ModelBinding, ProviderProcessSpec,
    RuntimeIdSource, RuntimeToolBinding,
};
use harness_session::{SessionEventPayload, SessionStore};
use harness_tools::{
    AllowAllToolPolicy, ToolArgumentValidationError, ToolArgumentValidator, ToolDefinition,
};
use harness_types::{
    AgentInstanceId, ContentBlock, EventId, EventSeq, JsonText, Message, MessageId, MessageSource,
    ProviderId, Role, SessionId, SideEffectClass, Timestamp,
};

struct TestIdentitySource {
    next: AtomicU64,
    timestamp: Timestamp,
}

impl TestIdentitySource {
    fn new(start: u64) -> Self {
        Self {
            next: AtomicU64::new(start),
            timestamp: Timestamp::parse("2026-08-20T07:00:00Z").unwrap(),
        }
    }

    fn next_value(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

impl AgentEventSource for TestIdentitySource {
    fn next_event_id(&self) -> EventId {
        EventId::new(format!("evt_runtime_{}", self.next_value())).unwrap()
    }

    fn now(&self) -> Timestamp {
        self.timestamp
    }
}

impl RuntimeIdSource for TestIdentitySource {
    fn next_session_id(&self) -> SessionId {
        SessionId::new(format!("ses_runtime_{}", self.next_value())).unwrap()
    }

    fn next_agent_instance_id(&self) -> AgentInstanceId {
        AgentInstanceId::new(format!("agt_runtime_{}", self.next_value())).unwrap()
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
        if !value.is_object() {
            return Err(ToolArgumentValidationError::new(
                "arguments must be a JSON object",
            ));
        }
        Ok(())
    }
}

fn provider_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../providers/example-python/provider.py")
}

fn python_program() -> String {
    std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_owned())
}

fn user_message(message_id: &str, text: &str) -> Message {
    Message {
        id: MessageId::new(message_id).unwrap(),
        role: Role::User,
        source: MessageSource::user(),
        content: vec![ContentBlock::text(text)],
    }
}

async fn wait_for_state(
    handle: &harness_agent::AgentHandle,
    predicate: impl Fn(&AgentState) -> bool,
) -> AgentState {
    for _ in 0..5000 {
        let state = handle.snapshot().await.unwrap();
        if predicate(&state) {
            return state;
        }
        tokio::task::yield_now().await;
    }
    panic!("HarnessRuntime Agent did not converge within test yield budget");
}

#[tokio::test]
async fn runtime_composes_python_provider_profile_and_agent_lifecycle() {
    let script = provider_script();
    assert!(
        script.is_file(),
        "missing example provider at {}",
        script.display()
    );

    let identity = Arc::new(TestIdentitySource::new(100));
    let provider_id = ProviderId::new("example-python").unwrap();
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
    let profile = AgentProfile::new(
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
    ));

    let runtime = HarnessRuntimeBuilder::in_memory(identity.clone(), identity)
        .provider(
            ProviderProcessSpec::new(provider_id.clone(), python_program())
                .arg(script.into_os_string())
                .request_timeout(Duration::from_secs(5))
                .shutdown_timeout(Duration::from_secs(2)),
        )
        .profile("python-agent", profile)
        .build()
        .await
        .unwrap();

    assert_eq!(runtime.providers().len(), 1);
    assert!(runtime.profiles().contains("python-agent"));
    assert!(runtime.llms().supports(&provider_id, "agent-model"));

    let session_id = runtime.create_session().await.unwrap();
    let handle = runtime
        .open_agent(session_id.clone(), "python-agent")
        .await
        .unwrap();

    assert!(matches!(
        runtime
            .open_agent(session_id.clone(), "python-agent")
            .await,
        Err(HarnessRuntimeError::AgentAlreadyActive(id)) if id == session_id
    ));

    handle
        .followup(user_message("msg_runtime_user", "hello from runtime"))
        .await
        .unwrap();

    let state = wait_for_state(&handle, |state| {
        state.active_operation.is_none()
            && state.projection.lifecycle.open_turn.is_none()
            && state.projection.model_messages.len() == 4
    })
    .await;
    assert_eq!(
        state.projection.model_messages.last().unwrap().content,
        vec![ContentBlock::text(
            "final: {\"text\": \"hello from runtime\"}"
        )]
    );

    let events = SessionStore::read(
        runtime.session_store().as_ref(),
        &session_id,
        EventSeq::FIRST,
        64,
    )
    .await
    .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), SessionEventPayload::ToolDispatched(_)))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), SessionEventPayload::AssistantMessage(_)))
            .count(),
        2
    );

    runtime.close_agent(&session_id).await.unwrap();
    assert!(!runtime.agents().contains(&session_id).await);

    let reopened = runtime
        .open_agent(session_id.clone(), "python-agent")
        .await
        .unwrap();
    assert_eq!(reopened.session_id(), &session_id);

    runtime.shutdown().await.unwrap();
    assert!(runtime.agents().is_empty().await);
    assert_eq!(
        runtime.providers().state(&provider_id).await,
        Some(ProviderState::Stopped)
    );
}
