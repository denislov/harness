use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use futures_util::stream;
use harness_agent::{AgentEventSource, AgentState, AgentToolRuntime};
use harness_llm::{
    BlockType, FinishEvent, LlmEventStream, LlmProvider, ModelOptions, ModelRequest,
    ModelRequestConfig, SequencedStreamEvent, StreamEvent,
};
use harness_session::{CreateSession, SessionCreated, SessionEvent, SessionStore};
use harness_tools::{
    ToolArgumentValidationError, ToolArgumentValidator, ToolDefinition, ToolExecutor, ToolPolicy,
    ToolRegistration, ToolRegistry,
};
use harness_types::{
    ContentBlock, EventId, EventSeq, JsonText, Message, MessageId, MessageSource, PortableError,
    ProviderId, Role, SessionId, SideEffectClass, StreamSeq, Timestamp, ToolCallId,
};

pub struct TestEventSource {
    next: AtomicU64,
    timestamp: Timestamp,
}

impl TestEventSource {
    pub fn new(start: u64) -> Self {
        Self {
            next: AtomicU64::new(start),
            timestamp: Timestamp::parse("2026-08-20T15:00:00Z")
                .expect("conformance timestamp is valid"),
        }
    }
}

impl AgentEventSource for TestEventSource {
    fn next_event_id(&self) -> EventId {
        let value = self.next.fetch_add(1, Ordering::Relaxed);
        EventId::new(format!("evt_conformance_{value}"))
            .expect("generated conformance EventId is valid")
    }

    fn now(&self) -> Timestamp {
        self.timestamp
    }
}

pub struct ScriptedLlm {
    provider_id: ProviderId,
    scripts: Mutex<VecDeque<Vec<Result<SequencedStreamEvent, PortableError>>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedLlm {
    pub fn new(
        provider_id: ProviderId,
        scripts: Vec<Vec<Result<SequencedStreamEvent, PortableError>>>,
    ) -> Self {
        Self {
            provider_id,
            scripts: Mutex::new(scripts.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .expect("scripted LLM request lock poisoned")
            .clone()
    }

    pub fn remaining_scripts(&self) -> usize {
        self.scripts
            .lock()
            .expect("scripted LLM script lock poisoned")
            .len()
    }
}

impl LlmProvider for ScriptedLlm {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn stream(&self, request: ModelRequest) -> LlmEventStream {
        self.requests
            .lock()
            .expect("scripted LLM request lock poisoned")
            .push(request);
        let script = self
            .scripts
            .lock()
            .expect("scripted LLM script lock poisoned")
            .pop_front()
            .expect("conformance LLM script exhausted");
        Box::pin(stream::iter(script))
    }
}

fn stream_seq(value: u64) -> StreamSeq {
    StreamSeq::new(value).expect("small conformance stream sequence is valid")
}

pub fn text_script(text: &str) -> Vec<Result<SequencedStreamEvent, PortableError>> {
    vec![
        Ok(SequencedStreamEvent::new(
            stream_seq(1),
            StreamEvent::BlockStart {
                index: 0,
                block_type: BlockType::Text,
            },
        )),
        Ok(SequencedStreamEvent::new(
            stream_seq(2),
            StreamEvent::TextDelta {
                index: 0,
                text: text.to_owned(),
            },
        )),
        Ok(SequencedStreamEvent::new(
            stream_seq(3),
            StreamEvent::BlockEnd {
                index: 0,
                block: ContentBlock::text(text),
            },
        )),
        Ok(SequencedStreamEvent::new(
            stream_seq(4),
            StreamEvent::Finish(FinishEvent::completed()),
        )),
    ]
}

pub fn tool_call_script(
    call_id: &str,
    name: &str,
    arguments_json: &str,
) -> Vec<Result<SequencedStreamEvent, PortableError>> {
    let call_id = ToolCallId::new(call_id).expect("conformance ToolCallId is valid");
    let arguments_json =
        JsonText::new(arguments_json.to_owned()).expect("conformance arguments are valid JSON");
    vec![
        Ok(SequencedStreamEvent::new(
            stream_seq(1),
            StreamEvent::BlockStart {
                index: 0,
                block_type: BlockType::ToolCall,
            },
        )),
        Ok(SequencedStreamEvent::new(
            stream_seq(2),
            StreamEvent::ToolCallDelta {
                index: 0,
                call_id: call_id.clone(),
                name: Some(name.to_owned()),
                arguments_delta: arguments_json.as_str().to_owned(),
            },
        )),
        Ok(SequencedStreamEvent::new(
            stream_seq(3),
            StreamEvent::BlockEnd {
                index: 0,
                block: ContentBlock::ToolCall {
                    id: call_id,
                    name: name.to_owned(),
                    arguments_json,
                },
            },
        )),
        Ok(SequencedStreamEvent::new(
            stream_seq(4),
            StreamEvent::Finish(FinishEvent::completed()),
        )),
    ]
}

pub fn model_config(provider: ProviderId) -> ModelRequestConfig {
    ModelRequestConfig {
        provider,
        model: "conformance-model".to_owned(),
        system: Some("Run the conformance scenario exactly.".to_owned()),
        tools: Vec::new(),
        options: ModelOptions {
            max_output_tokens: Some(256),
        },
    }
}

pub fn user_message(message_id: &str, text: &str) -> Message {
    Message {
        id: MessageId::new(message_id).expect("conformance MessageId is valid"),
        role: Role::User,
        source: MessageSource::user(),
        content: vec![ContentBlock::text(text)],
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ObjectValidator;

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
                "conformance tools require JSON object arguments",
            ))
        }
    }

    fn composition_identity(&self) -> String {
        "harness-conformance/object-validator/v1".to_owned()
    }
}

pub fn tool_definition(name: &str, side_effect: SideEffectClass) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        version: "1".to_owned(),
        description: format!("Batch 20 conformance tool {name}"),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "additionalProperties": true
        }),
        output_schema: None,
        parallel_safe: true,
        side_effect,
        default_timeout_ms: 5_000,
    }
}

pub fn build_tool_runtime(
    definition: ToolDefinition,
    executor: Arc<dyn ToolExecutor>,
    policy: Arc<dyn ToolPolicy>,
    max_automatic_attempts: u32,
) -> AgentToolRuntime {
    let registration = ToolRegistration::new(definition, executor, Arc::new(ObjectValidator))
        .expect("conformance ToolRegistration is valid");
    let registry =
        Arc::new(ToolRegistry::new(vec![registration]).expect("conformance ToolRegistry is valid"));
    AgentToolRuntime::new(registry, policy, max_automatic_attempts)
        .expect("conformance AgentToolRuntime is valid")
}

pub async fn create_session(
    store: &dyn SessionStore,
    event_source: &dyn AgentEventSource,
    session_id: SessionId,
) {
    store
        .create(CreateSession {
            session_id,
            event_id: event_source.next_event_id(),
            timestamp: event_source.now(),
            data: SessionCreated::default(),
        })
        .await
        .expect("conformance Session creation must succeed");
}

pub async fn read_all(store: &dyn SessionStore, session_id: &SessionId) -> Vec<SessionEvent> {
    let head = store
        .head(session_id)
        .await
        .expect("conformance Session head must be readable");
    let mut events = Vec::new();
    let mut from = EventSeq::FIRST;
    while from <= head.seq {
        let page = store
            .read(session_id, from, 64)
            .await
            .expect("conformance Session page must be readable");
        assert!(!page.is_empty(), "Session read ended before the known head");
        for event in page {
            let seq = event.seq();
            events.push(event);
            if seq == head.seq {
                return events;
            }
            from = seq
                .checked_next()
                .expect("conformance Session sequence must not overflow");
        }
    }
    events
}

pub async fn wait_for_quiescent(
    handle: &harness_agent::AgentHandle,
    expected_messages: usize,
) -> AgentState {
    for _ in 0..5_000 {
        let state = handle
            .snapshot()
            .await
            .expect("conformance Agent must remain reachable");
        if state.active_operation.is_none()
            && state.projection.lifecycle.open_turn.is_none()
            && state.projection.lifecycle.open_step.is_none()
            && state.projection.model_messages.len() == expected_messages
        {
            return state;
        }
        tokio::task::yield_now().await;
    }
    panic!("conformance Agent did not reach quiescence within the yield budget");
}

pub async fn wait_for_pending_approval(handle: &harness_agent::AgentHandle) -> AgentState {
    for _ in 0..5_000 {
        let state = handle
            .snapshot()
            .await
            .expect("conformance Agent must remain reachable");
        if state.projection.pending_approval.is_some() {
            return state;
        }
        tokio::task::yield_now().await;
    }
    panic!("conformance Agent did not reach an approval boundary within the yield budget");
}
