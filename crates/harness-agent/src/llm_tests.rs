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
use harness_session::{CreateSession, SessionCreated, SessionEventPayload, SessionStore};
use harness_storage::BlobStore;
use harness_storage_local::{MemoryBlobStore, MemorySessionStore};
use harness_types::{
    AgentInstanceId, ContentBlock, ErrorCode, EventId, EventSeq, Message, MessageId, MessageSource,
    PortableError, ProviderId, Role, SessionId, StreamSeq, Timestamp,
};

use crate::{
    ActiveAgentOperation, AgentActorConfig, AgentEventSource, AgentLlmRuntime, AgentState,
    spawn_agent_with_llm,
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
        EventId::new(format!("evt_llm_{value}")).unwrap()
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

struct PendingLlm {
    provider_id: ProviderId,
}

impl LlmProvider for PendingLlm {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn stream(&self, _request: ModelRequest) -> LlmEventStream {
        Box::pin(stream::pending())
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn user_message(message_id: &str, text: &str) -> Message {
    Message {
        id: MessageId::new(message_id).unwrap(),
        role: Role::User,
        source: MessageSource::user(),
        content: vec![ContentBlock::text(text)],
    }
}

fn seq(value: u64) -> StreamSeq {
    StreamSeq::new(value).unwrap()
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

async fn wait_for_state(
    handle: &crate::AgentHandle,
    predicate: impl Fn(&AgentState) -> bool,
) -> AgentState {
    for _ in 0..1000 {
        let state = handle.snapshot().await.unwrap();
        if predicate(&state) {
            return state;
        }
        tokio::task::yield_now().await;
    }
    panic!("agent state did not converge within test yield budget");
}

#[tokio::test]
async fn model_round_trip_persists_snapshot_and_final_answer() {
    let session_id: SessionId = id("ses_llm_round_trip");
    let provider_id: ProviderId = id("prv_fake");
    let store = create_store(&session_id).await;
    let blobs = Arc::new(MemoryBlobStore::new());
    let llm = Arc::new(ScriptedLlm::new(
        provider_id.clone(),
        vec![text_script("hello")],
    ));
    let runtime =
        AgentLlmRuntime::new(model_config(provider_id), llm.clone(), blobs.clone()).unwrap();

    let spawned = spawn_agent_with_llm(
        AgentInstanceId::new("agt_llm_round_trip").unwrap(),
        session_id.clone(),
        store.clone(),
        Arc::new(TestEventSource::new(100)),
        runtime,
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    spawned
        .handle
        .followup(user_message("msg_user", "say hello"))
        .await
        .unwrap();

    let state = wait_for_state(&spawned.handle, |state| {
        state.active_operation.is_none()
            && state.projection.lifecycle.open_turn.is_none()
            && state.projection.model_messages.len() == 2
    })
    .await;
    assert!(state.projection.inbox.is_empty());
    assert_eq!(
        state.projection.model_messages[1].content,
        vec![ContentBlock::text("hello")]
    );

    let events = store.read(&session_id, EventSeq::FIRST, 64).await.unwrap();
    let requested = events
        .iter()
        .find_map(|event| match event.payload() {
            SessionEventPayload::ModelRequested(data) => Some(data.clone()),
            _ => None,
        })
        .expect("model/requested must be durable");
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload(), SessionEventPayload::AssistantMessage(_)))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload(), SessionEventPayload::TurnEnded(_)))
    );

    let bytes = blobs.get(&requested.request_snapshot.id).await.unwrap();
    blobs.verify(&requested.request_snapshot).await.unwrap();
    let snapshotted: ModelRequest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(snapshotted.request_id, requested.request_id);
    assert_eq!(snapshotted.messages.len(), 1);
    assert_eq!(snapshotted.messages[0].id.as_str(), "msg_user");

    let requests = llm.requests();
    assert_eq!(requests, vec![snapshotted]);

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}

#[tokio::test]
async fn mailbox_remains_responsive_while_model_stream_is_pending() {
    let session_id: SessionId = id("ses_llm_pending");
    let provider_id: ProviderId = id("prv_pending");
    let store = create_store(&session_id).await;
    let blobs = Arc::new(MemoryBlobStore::new());
    let llm = Arc::new(PendingLlm {
        provider_id: provider_id.clone(),
    });
    let runtime = AgentLlmRuntime::new(model_config(provider_id), llm, blobs).unwrap();

    let spawned = spawn_agent_with_llm(
        id("agt_llm_pending"),
        session_id,
        store,
        Arc::new(TestEventSource::new(200)),
        runtime,
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    spawned
        .handle
        .followup(user_message("msg_primary", "work"))
        .await
        .unwrap();
    let state = wait_for_state(&spawned.handle, |state| {
        matches!(
            state.active_operation,
            Some(ActiveAgentOperation::Model { .. })
        )
    })
    .await;
    assert!(state.projection.pending_model_request.is_some());

    spawned
        .handle
        .steer(user_message("msg_steer", "additional context"))
        .await
        .unwrap();
    let state = spawned.handle.snapshot().await.unwrap();
    assert!(matches!(
        state.active_operation,
        Some(ActiveAgentOperation::Model { .. })
    ));
    assert_eq!(state.projection.inbox.next_step.len(), 1);

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}

#[tokio::test]
async fn stream_protocol_violation_becomes_durable_model_failure() {
    let session_id: SessionId = id("ses_llm_protocol_error");
    let provider_id: ProviderId = id("prv_bad_stream");
    let store = create_store(&session_id).await;
    let blobs = Arc::new(MemoryBlobStore::new());
    let llm = Arc::new(ScriptedLlm::new(
        provider_id.clone(),
        vec![vec![Ok(SequencedStreamEvent::new(
            seq(2),
            StreamEvent::Finish(FinishEvent::completed()),
        ))]],
    ));
    let runtime = AgentLlmRuntime::new(model_config(provider_id), llm, blobs).unwrap();

    let spawned = spawn_agent_with_llm(
        id("agt_llm_protocol_error"),
        session_id.clone(),
        store.clone(),
        Arc::new(TestEventSource::new(300)),
        runtime,
        AgentActorConfig::default(),
    )
    .await
    .unwrap();

    spawned
        .handle
        .followup(user_message("msg_bad", "trigger"))
        .await
        .unwrap();
    wait_for_state(&spawned.handle, |state| {
        state.active_operation.is_none() && state.projection.lifecycle.open_turn.is_none()
    })
    .await;

    let events = store.read(&session_id, EventSeq::FIRST, 64).await.unwrap();
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        SessionEventPayload::ModelFailed(data)
            if data.failure.code == ErrorCode::ProviderProtocolError
    )));

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}
