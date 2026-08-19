use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use futures_util::stream;
use harness_llm::{
    BlockType, FinishEvent, LlmEventStream, LlmProvider, ModelOptions, ModelRequest,
    ModelRequestConfig, SequencedStreamEvent, StreamEvent,
};
use harness_session::{
    CreateSession, SessionCreated, SessionEventPayload, SessionStore, StepEndReason,
};
use harness_storage_local::{MemoryBlobStore, MemorySessionStore};
use harness_tools::{
    AllowAllToolPolicy, IdempotencySupport, ToolArgumentValidationError, ToolArgumentValidator,
    ToolDefinition, ToolExecutionFuture, ToolExecutor, ToolInvocation, ToolRegistration,
    ToolRegistry,
};
use harness_types::{
    AgentInstanceId, ContentBlock, EventId, EventSeq, JsonText, Message, MessageId, MessageSource,
    PortableError, ProviderId, Role, SessionId, SideEffectClass, StreamSeq, Timestamp, ToolCallId,
    ToolOutcome,
};

use crate::{
    AgentActorConfig, AgentEventSource, AgentLlmRuntime, AgentState, AgentToolRuntime,
    spawn_agent_with_capabilities,
};

struct TestEventSource {
    next: AtomicU64,
    timestamp: Timestamp,
}

impl TestEventSource {
    fn new(start: u64) -> Self {
        Self {
            next: AtomicU64::new(start),
            timestamp: Timestamp::parse("2026-08-19T13:00:00Z").unwrap(),
        }
    }
}

impl AgentEventSource for TestEventSource {
    fn next_event_id(&self) -> EventId {
        let value = self.next.fetch_add(1, Ordering::Relaxed);
        EventId::new(format!("evt_tool_{value}")).unwrap()
    }

    fn now(&self) -> Timestamp {
        self.timestamp
    }
}

struct ScriptedLlm {
    provider_id: ProviderId,
    scripts: Mutex<VecDeque<Vec<Result<SequencedStreamEvent, PortableError>>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedLlm {
    fn new(
        provider_id: ProviderId,
        scripts: Vec<Vec<Result<SequencedStreamEvent, PortableError>>>,
    ) -> Self {
        Self {
            provider_id,
            scripts: Mutex::new(scripts.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl LlmProvider for ScriptedLlm {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn stream(&self, request: ModelRequest) -> LlmEventStream {
        self.requests.lock().unwrap().push(request);
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("test LLM script exhausted");
        Box::pin(stream::iter(script))
    }
}

struct ReadFileTool {
    provider_id: ProviderId,
    calls: AtomicU64,
}

impl ReadFileTool {
    fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }
}

impl ToolExecutor for ReadFileTool {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn idempotency_support(&self) -> IdempotencySupport {
        IdempotencySupport::Keyed
    }

    fn invoke(&self, invocation: ToolInvocation) -> ToolExecutionFuture {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            assert_eq!(invocation.tool_name, "read_file");
            Ok(ToolOutcome::Success {
                content: vec![ContentBlock::text("hello from foo.txt")],
            })
        })
    }
}

struct ReadFileValidator;

impl ToolArgumentValidator for ReadFileValidator {
    fn validate(
        &self,
        _definition: &ToolDefinition,
        arguments_json: &JsonText,
    ) -> Result<(), ToolArgumentValidationError> {
        let value: serde_json::Value = serde_json::from_str(arguments_json.as_str())
            .map_err(|error| ToolArgumentValidationError::new(error.to_string()))?;
        if value
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_none()
        {
            return Err(ToolArgumentValidationError::new("path must be a string"));
        }
        Ok(())
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn seq(value: u64) -> StreamSeq {
    StreamSeq::new(value).unwrap()
}

fn user_message(message_id: &str, text: &str) -> Message {
    Message {
        id: MessageId::new(message_id).unwrap(),
        role: Role::User,
        source: MessageSource::user(),
        content: vec![ContentBlock::text(text)],
    }
}

fn tool_call_script() -> Vec<Result<SequencedStreamEvent, PortableError>> {
    let call_id = ToolCallId::new("call_read_1").unwrap();
    let arguments = JsonText::new(r#"{"path":"foo.txt"}"#.to_owned()).unwrap();
    vec![
        Ok(SequencedStreamEvent::new(
            seq(1),
            StreamEvent::BlockStart {
                index: 0,
                block_type: BlockType::ToolCall,
            },
        )),
        Ok(SequencedStreamEvent::new(
            seq(2),
            StreamEvent::ToolCallDelta {
                index: 0,
                call_id: call_id.clone(),
                name: Some("read_file".to_owned()),
                arguments_delta: arguments.as_str().to_owned(),
            },
        )),
        Ok(SequencedStreamEvent::new(
            seq(3),
            StreamEvent::BlockEnd {
                index: 0,
                block: ContentBlock::ToolCall {
                    id: call_id,
                    name: "read_file".to_owned(),
                    arguments_json: arguments,
                },
            },
        )),
        Ok(SequencedStreamEvent::new(
            seq(4),
            StreamEvent::Finish(FinishEvent::completed()),
        )),
    ]
}

fn text_script(text: &str) -> Vec<Result<SequencedStreamEvent, PortableError>> {
    vec![
        Ok(SequencedStreamEvent::new(
            seq(1),
            StreamEvent::BlockStart {
                index: 0,
                block_type: BlockType::Text,
            },
        )),
        Ok(SequencedStreamEvent::new(
            seq(2),
            StreamEvent::TextDelta {
                index: 0,
                text: text.to_owned(),
            },
        )),
        Ok(SequencedStreamEvent::new(
            seq(3),
            StreamEvent::BlockEnd {
                index: 0,
                block: ContentBlock::text(text),
            },
        )),
        Ok(SequencedStreamEvent::new(
            seq(4),
            StreamEvent::Finish(FinishEvent::completed()),
        )),
    ]
}

fn model_config(provider: ProviderId) -> ModelRequestConfig {
    ModelRequestConfig {
        provider,
        model: "model-x".to_owned(),
        system: Some("You are a test model.".to_owned()),
        tools: Vec::new(),
        options: ModelOptions {
            max_output_tokens: Some(256),
        },
    }
}

fn tool_runtime(executor: Arc<ReadFileTool>) -> AgentToolRuntime {
    let definition = ToolDefinition {
        name: "read_file".to_owned(),
        version: "1".to_owned(),
        description: "Read a UTF-8 file".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
            "additionalProperties": false
        }),
        output_schema: None,
        parallel_safe: true,
        side_effect: SideEffectClass::ReadOnly,
        default_timeout_ms: 30_000,
    };
    let registration =
        ToolRegistration::new(definition, executor, Arc::new(ReadFileValidator)).unwrap();
    let registry = Arc::new(ToolRegistry::new(vec![registration]).unwrap());
    AgentToolRuntime::new(registry, Arc::new(AllowAllToolPolicy), 1).unwrap()
}

async fn create_store(session_id: &SessionId) -> Arc<MemorySessionStore> {
    let store = Arc::new(MemorySessionStore::new());
    store
        .create(CreateSession {
            session_id: session_id.clone(),
            event_id: EventId::new("evt_create").unwrap(),
            timestamp: Timestamp::parse("2026-08-19T13:00:00Z").unwrap(),
            data: SessionCreated::default(),
        })
        .await
        .unwrap();
    store
}

async fn wait_for_state(
    handle: &crate::AgentHandle,
    predicate: impl Fn(&AgentState) -> bool,
) -> AgentState {
    for _ in 0..2000 {
        let state = handle.snapshot().await.unwrap();
        if predicate(&state) {
            return state;
        }
        tokio::task::yield_now().await;
    }
    panic!("agent state did not converge within test yield budget");
}

#[tokio::test]
async fn user_llm_tool_llm_final_answer_vertical_slice() {
    let session_id: SessionId = id("ses_tool_vertical");
    let llm_provider_id: ProviderId = id("prv_llm");
    let store = create_store(&session_id).await;
    let blobs = Arc::new(MemoryBlobStore::new());
    let llm = Arc::new(ScriptedLlm::new(
        llm_provider_id.clone(),
        vec![tool_call_script(), text_script("foo.txt says hello")],
    ));
    let llm_runtime =
        AgentLlmRuntime::new(model_config(llm_provider_id), llm.clone(), blobs).unwrap();
    let tool = Arc::new(ReadFileTool {
        provider_id: id("prv_tools"),
        calls: AtomicU64::new(0),
    });

    let spawned = spawn_agent_with_capabilities(
        AgentInstanceId::new("agt_tool_vertical").unwrap(),
        session_id.clone(),
        store.clone(),
        Arc::new(TestEventSource::new(100)),
        llm_runtime,
        tool_runtime(tool.clone()),
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    spawned
        .handle
        .followup(user_message("msg_user", "read foo.txt"))
        .await
        .unwrap();

    let state = wait_for_state(&spawned.handle, |state| {
        state.active_operation.is_none()
            && state.projection.lifecycle.open_turn.is_none()
            && state.projection.model_messages.len() == 4
    })
    .await;
    assert_eq!(tool.calls(), 1);
    assert_eq!(
        state.projection.model_messages.last().unwrap().content,
        vec![ContentBlock::text("foo.txt says hello")]
    );

    let events = store.read(&session_id, EventSeq::FIRST, 64).await.unwrap();
    let event_types: Vec<_> = events
        .iter()
        .map(|event| event.payload().event_type())
        .collect();
    assert_eq!(
        event_types,
        vec![
            "session/created",
            "inbox/enqueued",
            "turn/started",
            "inbox/claimed",
            "step/started",
            "user/message",
            "model/requested",
            "assistant/message",
            "tool/call",
            "tool/dispatched",
            "tool/result",
            "step/ended",
            "step/started",
            "model/requested",
            "assistant/message",
            "step/ended",
            "turn/ended",
        ]
    );
    assert!(matches!(
        events[11].payload(),
        SessionEventPayload::StepEnded(data)
            if data.reason == StepEndReason::ToolContinuation
    ));

    let requests = llm.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools.len(), 1);
    assert_eq!(requests[0].tools[0].name, "read_file");
    assert_eq!(requests[1].messages.len(), 3);
    assert!(matches!(
        requests[1].messages[2].content.as_slice(),
        [ContentBlock::ToolResult { tool_call_id, .. }]
            if tool_call_id.as_str() == "call_read_1"
    ));

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}

struct FailingWriteTool {
    provider_id: ProviderId,
}

impl ToolExecutor for FailingWriteTool {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn invoke(&self, _invocation: ToolInvocation) -> ToolExecutionFuture {
        Box::pin(async {
            Err::<ToolOutcome, PortableError>(PortableError::new(
                harness_types::ErrorCode::ProviderUnavailable,
                "simulated transport loss after dispatch",
            ))
        })
    }
}

struct AcceptObjectValidator;

impl ToolArgumentValidator for AcceptObjectValidator {
    fn validate(
        &self,
        _definition: &ToolDefinition,
        arguments_json: &JsonText,
    ) -> Result<(), ToolArgumentValidationError> {
        let value: serde_json::Value = serde_json::from_str(arguments_json.as_str())
            .map_err(|error| ToolArgumentValidationError::new(error.to_string()))?;
        if !value.is_object() {
            return Err(ToolArgumentValidationError::new(
                "arguments must be an object",
            ));
        }
        Ok(())
    }
}

fn named_tool_call_script(
    call_id: &str,
    name: &str,
    arguments: &str,
) -> Vec<Result<SequencedStreamEvent, PortableError>> {
    let call_id = ToolCallId::new(call_id).unwrap();
    let arguments = JsonText::new(arguments.to_owned()).unwrap();
    vec![
        Ok(SequencedStreamEvent::new(
            seq(1),
            StreamEvent::BlockStart {
                index: 0,
                block_type: BlockType::ToolCall,
            },
        )),
        Ok(SequencedStreamEvent::new(
            seq(2),
            StreamEvent::ToolCallDelta {
                index: 0,
                call_id: call_id.clone(),
                name: Some(name.to_owned()),
                arguments_delta: arguments.as_str().to_owned(),
            },
        )),
        Ok(SequencedStreamEvent::new(
            seq(3),
            StreamEvent::BlockEnd {
                index: 0,
                block: ContentBlock::ToolCall {
                    id: call_id,
                    name: name.to_owned(),
                    arguments_json: arguments,
                },
            },
        )),
        Ok(SequencedStreamEvent::new(
            seq(4),
            StreamEvent::Finish(FinishEvent::completed()),
        )),
    ]
}

#[tokio::test]
async fn unknown_non_idempotent_outcome_blocks_without_redispatch() {
    let session_id: SessionId = id("ses_tool_non_idempotent_block");
    let llm_provider_id: ProviderId = id("prv_llm_block");
    let store = create_store(&session_id).await;
    let llm = Arc::new(ScriptedLlm::new(
        llm_provider_id.clone(),
        vec![named_tool_call_script(
            "call_send_1",
            "send_email",
            r#"{"to":"a@example.com"}"#,
        )],
    ));
    let llm_runtime = AgentLlmRuntime::new(
        model_config(llm_provider_id),
        llm,
        Arc::new(MemoryBlobStore::new()),
    )
    .unwrap();
    let executor = Arc::new(FailingWriteTool {
        provider_id: id("prv_write"),
    });
    let registration = ToolRegistration::new(
        ToolDefinition {
            name: "send_email".to_owned(),
            version: "1".to_owned(),
            description: "Send one email".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            parallel_safe: false,
            side_effect: SideEffectClass::NonIdempotentWrite,
            default_timeout_ms: 30_000,
        },
        executor,
        Arc::new(AcceptObjectValidator),
    )
    .unwrap();
    let tool_runtime = AgentToolRuntime::new(
        Arc::new(ToolRegistry::new(vec![registration]).unwrap()),
        Arc::new(AllowAllToolPolicy),
        3,
    )
    .unwrap();

    let spawned = spawn_agent_with_capabilities(
        id("agt_tool_non_idempotent_block"),
        session_id.clone(),
        store.clone(),
        Arc::new(TestEventSource::new(500)),
        llm_runtime,
        tool_runtime,
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    spawned
        .handle
        .followup(user_message("msg_write", "send it"))
        .await
        .unwrap();

    let state = wait_for_state(&spawned.handle, |state| {
        state.active_operation.is_none()
            && state.projection.unresolved_recovery.is_some()
            && state.projection.lifecycle.open_turn.is_none()
    })
    .await;
    assert!(matches!(state.gate, crate::ExecutionGate::Blocked(_)));

    let events = store.read(&session_id, EventSeq::FIRST, 64).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), SessionEventPayload::ToolDispatched(_)))
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload(), SessionEventPayload::RecoveryBlocked(_)))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload(), SessionEventPayload::ToolResult(_)))
    );

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}
