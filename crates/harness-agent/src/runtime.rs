use std::sync::Arc;

use harness_session::{SessionStore, V1SessionProjector};
use harness_types::{AgentInstanceId, SessionId};
use thiserror::Error;
use tokio::{sync::mpsc, task::JoinHandle};

use crate::{
    AgentActor, AgentBootstrapError, AgentBootstrapper, AgentError, AgentEventSource, AgentExit,
    AgentHandle,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentActorConfig {
    pub mailbox_capacity: usize,
    pub bootstrap_page_size: usize,
}

impl Default for AgentActorConfig {
    fn default() -> Self {
        Self {
            mailbox_capacity: 64,
            bootstrap_page_size: 256,
        }
    }
}

#[derive(Debug, Error)]
pub enum AgentSpawnError {
    #[error("agent mailbox capacity must be greater than zero")]
    InvalidMailboxCapacity,

    #[error(transparent)]
    Bootstrap(#[from] AgentBootstrapError),

    #[error("startup recovery convergence failed: {0}")]
    Convergence(#[from] AgentError),
}

#[derive(Debug, Error)]
pub enum AgentJoinError {
    #[error("agent task failed to join: {0}")]
    Tokio(#[from] tokio::task::JoinError),
}

pub struct AgentTask {
    join: JoinHandle<AgentExit>,
}

impl AgentTask {
    pub async fn join(self) -> Result<AgentExit, AgentJoinError> {
        Ok(self.join.await?)
    }

    /// Emergency process-local abort. This does not perform durable convergence.
    /// Normal shutdown should use `AgentHandle::shutdown()`.
    pub fn abort(&self) {
        self.join.abort();
    }

    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }
}

pub struct SpawnedAgent {
    pub handle: AgentHandle,
    pub task: AgentTask,
}

/// Bootstraps one Session, performs recovery actions that are safe without
/// external capability execution, then starts the single-owner Tokio actor task.
pub async fn spawn_agent(
    instance_id: AgentInstanceId,
    session_id: SessionId,
    store: Arc<dyn SessionStore>,
    event_source: Arc<dyn AgentEventSource>,
    config: AgentActorConfig,
) -> Result<SpawnedAgent, AgentSpawnError> {
    if config.mailbox_capacity == 0 {
        return Err(AgentSpawnError::InvalidMailboxCapacity);
    }

    let bootstrapper = AgentBootstrapper::new(V1SessionProjector, config.bootstrap_page_size);
    let bootstrap = bootstrapper.load(store.as_ref(), &session_id).await?;
    let mut actor = AgentActor::from_bootstrap(instance_id.clone(), bootstrap);

    // Convergence happens before the handle is published. Callers therefore never
    // observe a live actor that still needs to persist an interrupted model failure
    // or an unknown non-idempotent recovery gate.
    actor
        .converge_startup(store.as_ref(), event_source.as_ref())
        .await?;

    let (tx, rx) = mpsc::channel(config.mailbox_capacity);
    let handle = AgentHandle::new(instance_id, session_id, tx);
    let join = tokio::spawn(actor.run(store, event_source, rx));

    Ok(SpawnedAgent {
        handle,
        task: AgentTask { join },
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use harness_session::{
        AssistantMessage, CreateSession, InboxEnqueued, ModelRequested, NewSessionEvent,
        SessionCreated, SessionEventPayload, SessionStore, StepStarted, ToolCallRecorded,
        ToolDispatched, TurnStarted,
    };
    use harness_storage_local::MemorySessionStore;
    use harness_types::{
        BlobId, BlobRef, ContentBlock, EventId, EventSeq, IdempotencyKey, InboxTarget,
        InvocationId, JsonText, Message, MessageId, MessageSource, ProviderId, RequestId, Role,
        SessionId, Sha256Digest, SideEffectClass, StepNo, Timestamp, ToolCallId, TurnNo,
    };

    use crate::{
        AgentActorConfig, AgentError, AgentEventSource, AgentExitReason, AgentHandleError,
        ExecutionGate, ResumeDecision, spawn_agent,
    };

    struct TestEventSource {
        next: AtomicU64,
        timestamp: Timestamp,
    }

    impl TestEventSource {
        fn new(start: u64) -> Self {
            Self {
                next: AtomicU64::new(start),
                timestamp: ts(),
            }
        }
    }

    impl AgentEventSource for TestEventSource {
        fn next_event_id(&self) -> EventId {
            let value = self.next.fetch_add(1, Ordering::Relaxed);
            EventId::new(format!("evt_agent_{value}")).unwrap()
        }

        fn now(&self) -> Timestamp {
            self.timestamp
        }
    }

    struct DuplicateEventSource;

    impl AgentEventSource for DuplicateEventSource {
        fn next_event_id(&self) -> EventId {
            EventId::new("evt_create").unwrap()
        }

        fn now(&self) -> Timestamp {
            ts()
        }
    }

    fn ts() -> Timestamp {
        Timestamp::parse("2026-08-19T13:00:00Z").unwrap()
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn turn(value: u64) -> TurnNo {
        TurnNo::new(value).unwrap()
    }

    fn step(value: u64) -> StepNo {
        StepNo::new(value).unwrap()
    }

    fn user_message(message_id: &str, text: &str) -> Message {
        Message {
            id: MessageId::new(message_id).unwrap(),
            role: Role::User,
            source: MessageSource::user(),
            content: vec![ContentBlock::text(text)],
        }
    }

    fn request_blob() -> BlobRef {
        BlobRef {
            id: BlobId::new("blob_request").unwrap(),
            sha256: Sha256Digest::new("0".repeat(64)).unwrap(),
            size: 2,
            media_type: Some("application/json".to_owned()),
        }
    }

    async fn create_store(session_id: &SessionId) -> Arc<MemorySessionStore> {
        let store = Arc::new(MemorySessionStore::new());
        store
            .create(CreateSession {
                session_id: session_id.clone(),
                event_id: EventId::new("evt_create").unwrap(),
                timestamp: ts(),
                data: SessionCreated::default(),
            })
            .await
            .unwrap();
        store
    }

    fn draft(
        event_id: &str,
        payload: SessionEventPayload,
        position: Option<(TurnNo, Option<StepNo>)>,
    ) -> NewSessionEvent {
        let mut event = NewSessionEvent::new(EventId::new(event_id).unwrap(), ts(), payload);
        if let Some((turn_no, step_no)) = position {
            event = match step_no {
                Some(step_no) => event.in_step(turn_no, step_no),
                None => event.in_turn(turn_no),
            };
        }
        event
    }

    #[tokio::test]
    async fn send_ack_is_returned_after_durable_enqueue() {
        let session_id: SessionId = id("ses_send");
        let store = create_store(&session_id).await;
        let spawned = spawn_agent(
            id("agt_send"),
            session_id.clone(),
            store.clone(),
            Arc::new(TestEventSource::new(100)),
            AgentActorConfig::default(),
        )
        .await
        .unwrap();

        let receipt = spawned
            .handle
            .followup(user_message("msg_1", "hello"))
            .await
            .unwrap();

        assert_eq!(receipt.seq, EventSeq::new(2).unwrap());
        assert!(receipt.wake_requested);

        let committed = store
            .read(&session_id, EventSeq::new(2).unwrap(), 1)
            .await
            .unwrap();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].event_id(), &receipt.event_id);
        assert!(matches!(
            committed[0].payload(),
            SessionEventPayload::InboxEnqueued(_)
        ));

        let snapshot = spawned.handle.snapshot().await.unwrap();
        assert_eq!(snapshot.expected_seq, EventSeq::new(2).unwrap());
        assert_eq!(snapshot.projection.inbox.next_turn.len(), 1);
        assert!(snapshot.wake_requested);

        spawned.handle.shutdown().await.unwrap();
        let exit = spawned.task.join().await.unwrap();
        assert_eq!(exit.reason, AgentExitReason::ShutdownRequested);
    }

    #[tokio::test]
    async fn inject_is_durable_but_does_not_set_wake_latch() {
        let session_id: SessionId = id("ses_inject");
        let store = create_store(&session_id).await;
        let spawned = spawn_agent(
            id("agt_inject"),
            session_id,
            store,
            Arc::new(TestEventSource::new(200)),
            AgentActorConfig::default(),
        )
        .await
        .unwrap();

        let receipt = spawned
            .handle
            .inject(user_message("msg_inject", "context"))
            .await
            .unwrap();
        assert!(!receipt.wake_requested);
        assert!(!spawned.handle.snapshot().await.unwrap().wake_requested);

        spawned.handle.shutdown().await.unwrap();
        spawned.task.join().await.unwrap();
    }

    #[tokio::test]
    async fn startup_converges_interrupted_model_request_to_durable_failure() {
        let session_id: SessionId = id("ses_model_recovery");
        let store = create_store(&session_id).await;
        let request_id: RequestId = id("req_1");
        let provider_id: ProviderId = id("prv_llm");
        let events = vec![
            draft(
                "evt_turn",
                SessionEventPayload::TurnStarted(TurnStarted { turn: turn(1) }),
                Some((turn(1), None)),
            ),
            draft(
                "evt_step",
                SessionEventPayload::StepStarted(StepStarted {
                    turn: turn(1),
                    step: step(1),
                }),
                Some((turn(1), Some(step(1)))),
            ),
            draft(
                "evt_model",
                SessionEventPayload::ModelRequested(ModelRequested {
                    request_id: request_id.clone(),
                    provider: provider_id,
                    model: "model-x".to_owned(),
                    history_through_seq: EventSeq::new(3).unwrap(),
                    request_snapshot: request_blob(),
                    attempt: 1,
                }),
                Some((turn(1), Some(step(1)))),
            ),
        ];
        store
            .append(&session_id, EventSeq::FIRST, events)
            .await
            .unwrap();

        let spawned = spawn_agent(
            id("agt_model_recovery"),
            session_id.clone(),
            store.clone(),
            Arc::new(TestEventSource::new(300)),
            AgentActorConfig::default(),
        )
        .await
        .unwrap();

        let snapshot = spawned.handle.snapshot().await.unwrap();
        assert_eq!(snapshot.expected_seq, EventSeq::new(5).unwrap());
        assert!(matches!(
            snapshot.resume,
            ResumeDecision::ContinueOpenStep { .. }
        ));
        assert!(snapshot.projection.pending_model_request.is_none());

        let tail = store
            .read(&session_id, EventSeq::new(5).unwrap(), 1)
            .await
            .unwrap();
        assert!(matches!(
            tail[0].payload(),
            SessionEventPayload::ModelFailed(data) if data.request_id == request_id
        ));

        spawned.handle.shutdown().await.unwrap();
        spawned.task.join().await.unwrap();
    }

    #[tokio::test]
    async fn startup_persists_unknown_non_idempotent_gate_and_closes_lifecycle() {
        let session_id: SessionId = id("ses_tool_recovery");
        let store = create_store(&session_id).await;
        let request_id: RequestId = id("req_tool");
        let provider_id: ProviderId = id("prv_llm");
        let tool_call_id: ToolCallId = id("call_1");
        let invocation_id: InvocationId = id("inv_1");
        let args = JsonText::new(r#"{"path":"x"}"#.to_owned()).unwrap();
        let assistant = Message {
            id: id("msg_assistant"),
            role: Role::Assistant,
            source: MessageSource::model(provider_id.clone(), "model-x"),
            content: vec![ContentBlock::ToolCall {
                id: tool_call_id.clone(),
                name: "dangerous_write".to_owned(),
                arguments_json: args.clone(),
            }],
        };
        let events = vec![
            draft(
                "evt_turn",
                SessionEventPayload::TurnStarted(TurnStarted { turn: turn(1) }),
                Some((turn(1), None)),
            ),
            draft(
                "evt_step",
                SessionEventPayload::StepStarted(StepStarted {
                    turn: turn(1),
                    step: step(1),
                }),
                Some((turn(1), Some(step(1)))),
            ),
            draft(
                "evt_model",
                SessionEventPayload::ModelRequested(ModelRequested {
                    request_id: request_id.clone(),
                    provider: provider_id,
                    model: "model-x".to_owned(),
                    history_through_seq: EventSeq::new(3).unwrap(),
                    request_snapshot: request_blob(),
                    attempt: 1,
                }),
                Some((turn(1), Some(step(1)))),
            ),
            draft(
                "evt_assistant",
                SessionEventPayload::AssistantMessage(AssistantMessage {
                    request_id,
                    message: assistant,
                    usage: None,
                }),
                Some((turn(1), Some(step(1)))),
            ),
            draft(
                "evt_call",
                SessionEventPayload::ToolCall(ToolCallRecorded {
                    call_id: tool_call_id.clone(),
                    tool: "dangerous_write".to_owned(),
                    arguments_json: args,
                    side_effect: SideEffectClass::NonIdempotentWrite,
                }),
                Some((turn(1), Some(step(1)))),
            ),
            draft(
                "evt_dispatch",
                SessionEventPayload::ToolDispatched(ToolDispatched {
                    call_id: tool_call_id,
                    invocation_id,
                    provider_id: id("prv_tools"),
                    attempt: 1,
                    idempotency_key: IdempotencyKey::new("idem_1").unwrap(),
                }),
                Some((turn(1), Some(step(1)))),
            ),
        ];
        store
            .append(&session_id, EventSeq::FIRST, events)
            .await
            .unwrap();

        let spawned = spawn_agent(
            id("agt_tool_recovery"),
            session_id.clone(),
            store.clone(),
            Arc::new(TestEventSource::new(400)),
            AgentActorConfig::default(),
        )
        .await
        .unwrap();

        let snapshot = spawned.handle.snapshot().await.unwrap();
        assert_eq!(snapshot.expected_seq, EventSeq::new(10).unwrap());
        assert!(matches!(snapshot.resume, ResumeDecision::Blocked { .. }));
        assert!(matches!(snapshot.gate, ExecutionGate::Blocked(_)));
        assert!(snapshot.projection.lifecycle.open_turn.is_none());
        assert!(snapshot.projection.lifecycle.open_step.is_none());

        let tail = store
            .read(&session_id, EventSeq::new(8).unwrap(), 3)
            .await
            .unwrap();
        assert!(matches!(
            tail[0].payload(),
            SessionEventPayload::RecoveryBlocked(_)
        ));
        assert!(matches!(
            tail[1].payload(),
            SessionEventPayload::StepEnded(_)
        ));
        assert!(matches!(
            tail[2].payload(),
            SessionEventPayload::TurnEnded(_)
        ));

        spawned.handle.shutdown().await.unwrap();
        spawned.task.join().await.unwrap();
    }

    #[tokio::test]
    async fn competing_writer_conflict_is_terminal_for_live_actor() {
        let session_id: SessionId = id("ses_conflict");
        let store = create_store(&session_id).await;
        let spawned = spawn_agent(
            id("agt_conflict"),
            session_id.clone(),
            store.clone(),
            Arc::new(TestEventSource::new(500)),
            AgentActorConfig::default(),
        )
        .await
        .unwrap();

        store
            .append(
                &session_id,
                EventSeq::FIRST,
                vec![draft(
                    "evt_external",
                    SessionEventPayload::InboxEnqueued(InboxEnqueued {
                        message: user_message("msg_external", "external"),
                        target: InboxTarget::NextTurn,
                        wakeup: true,
                    }),
                    None,
                )],
            )
            .await
            .unwrap();

        let error = spawned
            .handle
            .followup(user_message("msg_actor", "actor"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AgentHandleError::Command(AgentError::OwnershipLost { .. })
        ));

        let exit = spawned.task.join().await.unwrap();
        assert!(matches!(
            exit.reason,
            AgentExitReason::Fatal(AgentError::OwnershipLost { .. })
        ));
    }

    #[tokio::test]
    async fn duplicate_generated_event_id_is_rejected_before_storage_commit() {
        let session_id: SessionId = id("ses_duplicate_event");
        let store = create_store(&session_id).await;
        let spawned = spawn_agent(
            id("agt_duplicate_event"),
            session_id.clone(),
            store.clone(),
            Arc::new(DuplicateEventSource),
            AgentActorConfig::default(),
        )
        .await
        .unwrap();

        let error = spawned
            .handle
            .followup(user_message("msg_dup", "hello"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AgentHandleError::Command(AgentError::InvalidDurableMutation { .. })
        ));
        assert_eq!(store.head(&session_id).await.unwrap().seq, EventSeq::FIRST);

        spawned.handle.shutdown().await.unwrap();
        spawned.task.join().await.unwrap();
    }
}
