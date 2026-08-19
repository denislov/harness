use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use harness_session::{
    CreateSession, InboxEnqueued, NewSessionEvent, SessionCreated, SessionEventPayload,
    SessionStore,
};
use harness_storage_local::MemorySessionStore;
use harness_types::{
    AgentInstanceId, ContentBlock, EventId, EventSeq, InboxTarget, Message, MessageId,
    MessageSource, Role, SessionId, StepNo, Timestamp, TurnNo,
};

use crate::{
    AgentActorConfig, AgentDriverBoundary, AgentEventSource, AgentPhase, ResumeDecision,
    spawn_agent,
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
        EventId::new(format!("evt_driver_{value}")).unwrap()
    }

    fn now(&self) -> Timestamp {
        self.timestamp
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

fn user_message(message_id: &str, text: &str) -> Message {
    Message {
        id: MessageId::new(message_id).unwrap(),
        role: Role::User,
        source: MessageSource::user(),
        content: vec![ContentBlock::text(text)],
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

async fn spawn(
    session_id: SessionId,
    store: Arc<MemorySessionStore>,
    event_start: u64,
) -> crate::SpawnedAgent {
    spawn_agent(
        AgentInstanceId::new(format!("agt_{event_start}")).unwrap(),
        session_id,
        store,
        Arc::new(TestEventSource::new(event_start)),
        AgentActorConfig::default(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn followup_advances_to_ready_for_model_boundary() {
    let session_id: SessionId = id("ses_driver_followup");
    let store = create_store(&session_id).await;
    let spawned = spawn(session_id.clone(), store.clone(), 100).await;

    let receipt = spawned
        .handle
        .followup(user_message("msg_primary", "hello"))
        .await
        .unwrap();
    assert_eq!(receipt.seq, EventSeq::new(2).unwrap());
    assert!(receipt.wake_requested);

    let snapshot = spawned.handle.snapshot().await.unwrap();
    assert_eq!(snapshot.expected_seq, EventSeq::new(6).unwrap());
    assert!(snapshot.projection.inbox.is_empty());
    assert!(!snapshot.wake_requested);
    assert_eq!(snapshot.projection.model_messages.len(), 1);
    assert!(matches!(
        snapshot.phase,
        AgentPhase::Running {
            turn,
            step: Some(step)
        } if turn == TurnNo::FIRST && step == StepNo::FIRST
    ));
    assert_eq!(
        snapshot.driver_boundary(),
        Some(AgentDriverBoundary::ReadyForModel {
            position: harness_session::StepPosition {
                turn: TurnNo::FIRST,
                step: StepNo::FIRST,
            }
        })
    );
    assert!(matches!(
        snapshot.resume,
        ResumeDecision::ContinueOpenStep { .. }
    ));

    let events = store
        .read(&session_id, EventSeq::new(3).unwrap(), 4)
        .await
        .unwrap();
    assert!(matches!(
        events[0].payload(),
        SessionEventPayload::TurnStarted(_)
    ));
    assert!(matches!(
        events[1].payload(),
        SessionEventPayload::InboxClaimed(_)
    ));
    assert!(matches!(
        events[2].payload(),
        SessionEventPayload::StepStarted(_)
    ));
    assert!(matches!(
        events[3].payload(),
        SessionEventPayload::UserMessage(_)
    ));

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}

#[tokio::test]
async fn idle_inject_is_consumed_with_the_next_waking_turn() {
    let session_id: SessionId = id("ses_driver_inject");
    let store = create_store(&session_id).await;
    let spawned = spawn(session_id, store, 200).await;

    spawned
        .handle
        .inject(user_message("msg_context", "context"))
        .await
        .unwrap();
    let idle = spawned.handle.snapshot().await.unwrap();
    assert!(matches!(idle.phase, AgentPhase::Idle { .. }));
    assert_eq!(idle.projection.inbox.next_step.len(), 1);

    spawned
        .handle
        .followup(user_message("msg_primary", "question"))
        .await
        .unwrap();
    let ready = spawned.handle.snapshot().await.unwrap();

    assert!(ready.projection.inbox.is_empty());
    assert_eq!(ready.projection.model_messages.len(), 2);
    assert_eq!(
        ready.projection.model_messages[0].id.as_str(),
        "msg_primary"
    );
    assert_eq!(
        ready.projection.model_messages[1].id.as_str(),
        "msg_context"
    );
    assert!(matches!(
        ready.driver_boundary(),
        Some(AgentDriverBoundary::ReadyForModel { .. })
    ));

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}

#[tokio::test]
async fn steer_at_model_boundary_enters_the_same_open_step() {
    let session_id: SessionId = id("ses_driver_steer");
    let store = create_store(&session_id).await;
    let spawned = spawn(session_id, store, 300).await;

    spawned
        .handle
        .followup(user_message("msg_primary", "question"))
        .await
        .unwrap();
    let before = spawned.handle.snapshot().await.unwrap();
    assert_eq!(before.expected_seq, EventSeq::new(6).unwrap());

    spawned
        .handle
        .steer(user_message("msg_steer", "additional constraint"))
        .await
        .unwrap();
    let after = spawned.handle.snapshot().await.unwrap();

    assert_eq!(after.expected_seq, EventSeq::new(9).unwrap());
    assert_eq!(after.projection.model_messages.len(), 2);
    assert_eq!(
        after.projection.lifecycle.open_step,
        Some(harness_session::StepPosition {
            turn: TurnNo::FIRST,
            step: StepNo::FIRST,
        })
    );
    assert!(after.projection.inbox.next_step.is_empty());

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}

#[tokio::test]
async fn next_turn_work_waits_while_current_step_is_parked_for_model() {
    let session_id: SessionId = id("ses_driver_future_turn");
    let store = create_store(&session_id).await;
    let spawned = spawn(session_id, store, 400).await;

    spawned
        .handle
        .followup(user_message("msg_first", "first"))
        .await
        .unwrap();
    spawned
        .handle
        .followup(user_message("msg_second", "second"))
        .await
        .unwrap();

    let snapshot = spawned.handle.snapshot().await.unwrap();
    assert_eq!(snapshot.expected_seq, EventSeq::new(7).unwrap());
    assert_eq!(snapshot.projection.inbox.next_turn.len(), 1);
    assert_eq!(
        snapshot.projection.inbox.next_turn[0].message.id.as_str(),
        "msg_second"
    );
    assert!(snapshot.wake_requested);
    assert!(matches!(
        snapshot.driver_boundary(),
        Some(AgentDriverBoundary::ReadyForModel { .. })
    ));

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}

#[tokio::test]
async fn startup_pending_wakeup_converges_before_handle_publication() {
    let session_id: SessionId = id("ses_driver_restart_wake");
    let store = create_store(&session_id).await;
    store
        .append(
            &session_id,
            EventSeq::FIRST,
            vec![NewSessionEvent::new(
                EventId::new("evt_pending_input").unwrap(),
                ts(),
                SessionEventPayload::InboxEnqueued(InboxEnqueued {
                    message: user_message("msg_restart", "resume me"),
                    target: InboxTarget::NextTurn,
                    wakeup: true,
                }),
            )],
        )
        .await
        .unwrap();

    let spawned = spawn(session_id, store, 500).await;
    let snapshot = spawned.handle.snapshot().await.unwrap();

    assert_eq!(snapshot.expected_seq, EventSeq::new(6).unwrap());
    assert!(snapshot.projection.inbox.is_empty());
    assert!(matches!(
        snapshot.driver_boundary(),
        Some(AgentDriverBoundary::ReadyForModel { .. })
    ));

    spawned.handle.shutdown().await.unwrap();
    spawned.task.join().await.unwrap();
}
