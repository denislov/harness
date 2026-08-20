use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use harness_agent::{
    AgentActorConfig, AgentEventSource, AgentLlmRuntime, AgentState, AgentToolRuntime,
    spawn_agent_with_capabilities,
};
use harness_llm::{LlmProvider, ModelOptions, ModelRequestConfig};
use harness_provider_host::{
    ProviderHost, ProviderHostConfig, ProviderHostLlmAdapter, ProviderHostToolAdapter,
};
use harness_provider_protocol::RuntimeInfo;
use harness_session::{CreateSession, SessionCreated, SessionEventPayload, SessionStore};
use harness_storage_local::{MemoryBlobStore, MemorySessionStore};
use harness_tools::{
    AllowAllToolPolicy, ToolArgumentValidationError, ToolArgumentValidator, ToolDefinition,
    ToolRegistration, ToolRegistry,
};
use harness_types::{
    AgentInstanceId, ContentBlock, EventId, EventSeq, JsonText, Message, MessageId, MessageSource,
    Role, SessionId, SideEffectClass, Timestamp,
};

struct TestEventSource {
    next: AtomicU64,
    timestamp: Timestamp,
}

impl TestEventSource {
    fn new(start: u64) -> Self {
        Self {
            next: AtomicU64::new(start),
            timestamp: Timestamp::parse("2026-08-20T04:00:00Z").unwrap(),
        }
    }
}

impl AgentEventSource for TestEventSource {
    fn next_event_id(&self) -> EventId {
        let value = self.next.fetch_add(1, Ordering::Relaxed);
        EventId::new(format!("evt_python_{value}")).unwrap()
    }

    fn now(&self) -> Timestamp {
        self.timestamp
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
    panic!("out-of-process Agent state did not converge within test yield budget");
}

#[tokio::test]
async fn python_provider_runs_user_llm_tool_llm_final_answer() {
    let script = provider_script();
    assert!(
        script.is_file(),
        "missing example provider at {}",
        script.display()
    );

    let host = ProviderHost::start(
        ProviderHostConfig::new(
            python_program(),
            RuntimeInfo {
                name: "harness-batch-11-test".to_owned(),
                version: "0.1.0".to_owned(),
            },
        )
        .arg(script.into_os_string())
        .request_timeout(Duration::from_secs(5))
        .shutdown_timeout(Duration::from_secs(2)),
    )
    .await
    .unwrap();

    let llm_adapter = ProviderHostLlmAdapter::new(host.clone()).await.unwrap();
    let provider_id = llm_adapter.provider_id().clone();
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
    let tool_adapter = ProviderHostToolAdapter::from_definition(host.clone(), &tool_definition)
        .await
        .unwrap();

    let registration = ToolRegistration::new(
        tool_definition,
        Arc::new(tool_adapter),
        Arc::new(ObjectValidator),
    )
    .unwrap();
    let tool_runtime = AgentToolRuntime::new(
        Arc::new(ToolRegistry::new(vec![registration]).unwrap()),
        Arc::new(AllowAllToolPolicy),
        2,
    )
    .unwrap();

    let llm_runtime = AgentLlmRuntime::new(
        ModelRequestConfig {
            provider: provider_id,
            model: "agent-model".to_owned(),
            system: Some("Use the echo tool, then answer with its result.".to_owned()),
            tools: Vec::new(),
            options: ModelOptions {
                max_output_tokens: Some(256),
            },
        },
        Arc::new(llm_adapter),
        Arc::new(MemoryBlobStore::new()),
    )
    .unwrap();

    let session_id = SessionId::new("ses_python_vertical").unwrap();
    let store = Arc::new(MemorySessionStore::new());
    store
        .create(CreateSession {
            session_id: session_id.clone(),
            event_id: EventId::new("evt_python_create").unwrap(),
            timestamp: Timestamp::parse("2026-08-20T04:00:00Z").unwrap(),
            data: SessionCreated::default(),
        })
        .await
        .unwrap();

    let spawned = spawn_agent_with_capabilities(
        AgentInstanceId::new("agt_python_vertical").unwrap(),
        session_id.clone(),
        store.clone(),
        Arc::new(TestEventSource::new(100)),
        llm_runtime,
        tool_runtime,
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    spawned
        .handle
        .followup(user_message("msg_python_user", "hello from rust core"))
        .await
        .unwrap();

    let state = wait_for_state(&spawned.handle, |state| {
        state.active_operation.is_none()
            && state.projection.lifecycle.open_turn.is_none()
            && state.projection.model_messages.len() == 4
    })
    .await;

    assert_eq!(
        state.projection.model_messages.last().unwrap().content,
        vec![ContentBlock::text(
            "final: {\"text\": \"hello from rust core\"}"
        )]
    );

    let events = store.read(&session_id, EventSeq::FIRST, 64).await.unwrap();
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
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload(), SessionEventPayload::ToolResult(_)))
    );

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
    host.shutdown().await.unwrap();
}
