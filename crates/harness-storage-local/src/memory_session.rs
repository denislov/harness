use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use harness_session::{
    AppendResult, CreateSession, ForkSession, NewSessionEvent, SessionEvent, SessionEventPayload,
    SessionHead, SessionStore, SessionStoreError,
};
use harness_types::{EventId, EventSeq, SessionId};

/// In-memory reference implementation of [`SessionStore`].
///
/// The entire session map is protected by a single `RwLock`. This is deliberate
/// for the first reference backend: mutating operations are linearizable, an
/// append batch is atomically visible to readers, and the implementation keeps
/// the storage semantics easy to audit before introducing a transactional local
/// database backend.
#[derive(Clone, Debug, Default)]
pub struct MemorySessionStore {
    inner: Arc<RwLock<HashMap<SessionId, Vec<SessionEvent>>>>,
}

impl MemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionStore for MemorySessionStore {
    async fn create(&self, request: CreateSession) -> Result<SessionEvent, SessionStoreError> {
        let CreateSession {
            session_id,
            event_id,
            timestamp,
            data,
        } = request;

        let draft = NewSessionEvent::new(
            event_id,
            timestamp,
            SessionEventPayload::SessionCreated(data),
        );
        let event = SessionEvent::committed(session_id.clone(), EventSeq::FIRST, draft)
            .map_err(|error| SessionStoreError::InvalidArgument(error.to_string()))?;

        let mut sessions = self.inner.write().map_err(|_| lock_poisoned("create"))?;

        if sessions.contains_key(&session_id) {
            return Err(SessionStoreError::AlreadyExists { session_id });
        }

        sessions.insert(session_id, vec![event.clone()]);
        Ok(event)
    }

    async fn append(
        &self,
        session_id: &SessionId,
        expected_seq: EventSeq,
        events: Vec<NewSessionEvent>,
    ) -> Result<AppendResult, SessionStoreError> {
        let mut sessions = self.inner.write().map_err(|_| lock_poisoned("append"))?;
        let log = sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionStoreError::NotFound(session_id.clone()))?;

        let actual = log_head(session_id, log)?;
        if actual != expected_seq {
            return Err(SessionStoreError::Conflict {
                session_id: session_id.clone(),
                expected: expected_seq,
                actual,
            });
        }

        // The optimistic concurrency check is authoritative. Only after the
        // caller has demonstrated that it is appending to the current head do
        // we validate the proposed batch. A stale writer therefore observes
        // CONFLICT even if its proposed events are themselves invalid.
        let mut event_ids: HashSet<EventId> =
            log.iter().map(|event| event.event_id().clone()).collect();
        for (index, event) in events.iter().enumerate() {
            event.validate().map_err(|error| {
                SessionStoreError::InvalidArgument(format!(
                    "invalid event at append batch index {index}: {error}"
                ))
            })?;
            if !event_ids.insert(event.event_id().clone()) {
                return Err(SessionStoreError::InvalidArgument(format!(
                    "duplicate event id {} at append batch index {index}",
                    event.event_id()
                )));
            }
        }

        // Build the complete committed batch before mutating the log. If event
        // validation or sequence allocation fails, no part of the batch becomes
        // visible.
        let mut next_seq = actual;
        let mut committed = Vec::with_capacity(events.len());
        for draft in events {
            next_seq = next_seq.checked_next().map_err(|error| {
                SessionStoreError::InvalidArgument(format!(
                    "cannot allocate the next event sequence: {error}"
                ))
            })?;

            let event =
                SessionEvent::committed(session_id.clone(), next_seq, draft).map_err(|error| {
                    SessionStoreError::InvalidArgument(format!("invalid event: {error}"))
                })?;
            committed.push(event);
        }

        log.extend(committed.iter().cloned());

        Ok(AppendResult {
            new_head: next_seq,
            committed,
        })
    }

    async fn read(
        &self,
        session_id: &SessionId,
        from_seq: EventSeq,
        limit: usize,
    ) -> Result<Vec<SessionEvent>, SessionStoreError> {
        let sessions = self.inner.read().map_err(|_| lock_poisoned("read"))?;
        let log = sessions
            .get(session_id)
            .ok_or_else(|| SessionStoreError::NotFound(session_id.clone()))?;

        // Validate the in-memory representation before exposing it through the
        // SessionStore contract. This also makes corruption handling explicit in
        // the reference backend rather than relying on construction invariants.
        validate_log(session_id, log)?;

        if limit == 0 {
            return Ok(Vec::new());
        }

        Ok(log
            .iter()
            .filter(|event| event.seq() >= from_seq)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn head(&self, session_id: &SessionId) -> Result<SessionHead, SessionStoreError> {
        let sessions = self.inner.read().map_err(|_| lock_poisoned("head"))?;
        let log = sessions
            .get(session_id)
            .ok_or_else(|| SessionStoreError::NotFound(session_id.clone()))?;
        let seq = log_head(session_id, log)?;

        Ok(SessionHead {
            session_id: session_id.clone(),
            seq,
        })
    }

    async fn fork(&self, request: ForkSession) -> Result<SessionHead, SessionStoreError> {
        let ForkSession {
            source_session_id,
            through_seq,
            target_session_id,
        } = request;

        if through_seq < EventSeq::FIRST {
            return Err(SessionStoreError::InvalidArgument(
                "fork through_seq must include the source session/created event".to_owned(),
            ));
        }

        let mut sessions = self.inner.write().map_err(|_| lock_poisoned("fork"))?;

        if sessions.contains_key(&target_session_id) {
            return Err(SessionStoreError::AlreadyExists {
                session_id: target_session_id,
            });
        }

        let source = sessions
            .get(&source_session_id)
            .ok_or_else(|| SessionStoreError::NotFound(source_session_id.clone()))?;
        let source_head = log_head(&source_session_id, source)?;

        if through_seq > source_head {
            return Err(SessionStoreError::InvalidArgument(format!(
                "fork through_seq {through_seq} exceeds source head {source_head}"
            )));
        }

        let source_prefix: Vec<SessionEvent> = source
            .iter()
            .take_while(|event| event.seq() <= through_seq)
            .cloned()
            .collect();

        if source_prefix
            .last()
            .map(SessionEvent::seq)
            .filter(|seq| *seq == through_seq)
            .is_none()
        {
            return Err(SessionStoreError::Corrupt {
                session_id: source_session_id,
                reason: format!("event sequence does not contain fork boundary {through_seq}"),
            });
        }

        let mut target_log = Vec::with_capacity(source_prefix.len());
        for event in source_prefix {
            target_log.push(rebind_session(&event, &target_session_id)?);
        }

        validate_log(&target_session_id, &target_log)?;
        sessions.insert(target_session_id.clone(), target_log);

        Ok(SessionHead {
            session_id: target_session_id,
            seq: through_seq,
        })
    }
}

fn log_head(session_id: &SessionId, log: &[SessionEvent]) -> Result<EventSeq, SessionStoreError> {
    validate_log(session_id, log)?;
    log.last()
        .map(SessionEvent::seq)
        .ok_or_else(|| SessionStoreError::Corrupt {
            session_id: session_id.clone(),
            reason: "session log is empty".to_owned(),
        })
}

fn validate_log(session_id: &SessionId, log: &[SessionEvent]) -> Result<(), SessionStoreError> {
    if log.is_empty() {
        return Err(SessionStoreError::Corrupt {
            session_id: session_id.clone(),
            reason: "session log is empty".to_owned(),
        });
    }

    let mut expected = EventSeq::FIRST;
    let mut event_ids = HashSet::new();
    for event in log {
        if !event_ids.insert(event.event_id().clone()) {
            return Err(SessionStoreError::Corrupt {
                session_id: session_id.clone(),
                reason: format!("duplicate event id {}", event.event_id()),
            });
        }
        if event.session_id() != session_id {
            return Err(SessionStoreError::Corrupt {
                session_id: session_id.clone(),
                reason: format!(
                    "event {} belongs to session {}",
                    event.event_id(),
                    event.session_id()
                ),
            });
        }

        if event.seq() != expected {
            return Err(SessionStoreError::Corrupt {
                session_id: session_id.clone(),
                reason: format!("expected event sequence {expected}, found {}", event.seq()),
            });
        }

        event
            .validate()
            .map_err(|error| SessionStoreError::Corrupt {
                session_id: session_id.clone(),
                reason: format!("event {} is invalid: {error}", event.event_id()),
            })?;

        expected = match expected.checked_next() {
            Ok(next) => next,
            Err(_) => {
                // The final legal sequence value has no representable successor.
                // That is not corruption if it belongs to the final event.
                if event.seq() == log.last().expect("log is non-empty").seq() {
                    break;
                }
                return Err(SessionStoreError::Corrupt {
                    session_id: session_id.clone(),
                    reason: "event sequence exceeds the cross-language maximum".to_owned(),
                });
            }
        };
    }

    match log.first().map(SessionEvent::payload) {
        Some(SessionEventPayload::SessionCreated(_)) => Ok(()),
        _ => Err(SessionStoreError::Corrupt {
            session_id: session_id.clone(),
            reason: "first event is not session/created".to_owned(),
        }),
    }
}

/// Copies a committed source event into a fork target while preserving the
/// source event's logical identity and committed position.
///
/// Batch 02 intentionally preserves `event_id`, `seq`, `timestamp`, turn/step,
/// and payload. Only the envelope `session_id` is rebound. The v0.1 spec does
/// not yet define a dedicated `session/forked` event or fork-lineage envelope.
fn rebind_session(
    event: &SessionEvent,
    target_session_id: &SessionId,
) -> Result<SessionEvent, SessionStoreError> {
    let mut draft = NewSessionEvent::new(
        event.event_id().clone(),
        event.timestamp(),
        event.payload().clone(),
    );

    draft = match (event.turn(), event.step()) {
        (Some(turn), Some(step)) => draft.in_step(turn, step),
        (Some(turn), None) => draft.in_turn(turn),
        (None, None) => draft,
        (None, Some(_)) => {
            return Err(SessionStoreError::Corrupt {
                session_id: event.session_id().clone(),
                reason: format!("event {} has a step without a turn", event.event_id()),
            });
        }
    };

    SessionEvent::committed(target_session_id.clone(), event.seq(), draft).map_err(|error| {
        SessionStoreError::Corrupt {
            session_id: event.session_id().clone(),
            reason: format!(
                "cannot copy event {} into fork target: {error}",
                event.event_id()
            ),
        }
    })
}

fn lock_poisoned(operation: &str) -> SessionStoreError {
    SessionStoreError::Internal(format!(
        "MemorySessionStore lock was poisoned during {operation}"
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use futures::executor::block_on;
    use harness_session::{SessionCreated, SessionEventPayload, StepStarted, TurnStarted};
    use harness_types::{EventId, StepNo, Timestamp, TurnNo};

    use super::*;

    fn session_id(value: &str) -> SessionId {
        SessionId::new(value).unwrap()
    }

    fn event_id(value: &str) -> EventId {
        EventId::new(value).unwrap()
    }

    fn timestamp() -> Timestamp {
        Timestamp::parse("2026-08-19T13:00:00Z").unwrap()
    }

    fn create_request(id: &str) -> CreateSession {
        CreateSession {
            session_id: session_id(id),
            event_id: event_id(&format!("evt_create_{id}")),
            timestamp: timestamp(),
            data: SessionCreated::default(),
        }
    }

    fn turn_started(event: &str, turn: u64) -> NewSessionEvent {
        let turn = TurnNo::new(turn).unwrap();
        NewSessionEvent::new(
            event_id(event),
            timestamp(),
            SessionEventPayload::TurnStarted(TurnStarted { turn }),
        )
        .in_turn(turn)
    }

    fn step_started(event: &str, turn: u64, step: u64) -> NewSessionEvent {
        let turn = TurnNo::new(turn).unwrap();
        let step = StepNo::new(step).unwrap();
        NewSessionEvent::new(
            event_id(event),
            timestamp(),
            SessionEventPayload::StepStarted(StepStarted { turn, step }),
        )
        .in_step(turn, step)
    }

    #[test]
    fn create_establishes_session_created_at_seq_one() {
        block_on(async {
            let store = MemorySessionStore::new();
            let id = session_id("ses_create");

            let event = store.create(create_request("ses_create")).await.unwrap();

            assert_eq!(event.session_id(), &id);
            assert_eq!(event.seq(), EventSeq::FIRST);
            assert!(matches!(
                event.payload(),
                SessionEventPayload::SessionCreated(_)
            ));
            assert_eq!(store.head(&id).await.unwrap().seq, EventSeq::FIRST);
        });
    }

    #[test]
    fn creating_existing_session_returns_conflict_without_mutation() {
        block_on(async {
            let store = MemorySessionStore::new();
            let id = session_id("ses_duplicate");
            store.create(create_request("ses_duplicate")).await.unwrap();

            let error = store
                .create(create_request("ses_duplicate"))
                .await
                .unwrap_err();

            assert!(matches!(error, SessionStoreError::AlreadyExists { .. }));
            assert_eq!(store.head(&id).await.unwrap().seq, EventSeq::FIRST);
        });
    }

    #[test]
    fn append_assigns_contiguous_sequences_and_returns_committed_events() {
        block_on(async {
            let store = MemorySessionStore::new();
            let id = session_id("ses_append");
            store.create(create_request("ses_append")).await.unwrap();

            let result = store
                .append(
                    &id,
                    EventSeq::FIRST,
                    vec![turn_started("evt_turn", 1), step_started("evt_step", 1, 1)],
                )
                .await
                .unwrap();

            assert_eq!(result.new_head, EventSeq::new(3).unwrap());
            assert_eq!(result.committed.len(), 2);
            assert_eq!(result.committed[0].seq(), EventSeq::new(2).unwrap());
            assert_eq!(result.committed[1].seq(), EventSeq::new(3).unwrap());
            assert!(
                result
                    .committed
                    .iter()
                    .all(|event| event.session_id() == &id)
            );
        });
    }

    #[test]
    fn append_conflict_does_not_mutate_the_log() {
        block_on(async {
            let store = MemorySessionStore::new();
            let id = session_id("ses_conflict");
            store.create(create_request("ses_conflict")).await.unwrap();

            store
                .append(&id, EventSeq::FIRST, vec![turn_started("evt_first", 1)])
                .await
                .unwrap();

            let error = store
                .append(&id, EventSeq::FIRST, vec![turn_started("evt_stale", 2)])
                .await
                .unwrap_err();

            assert!(matches!(
                error,
                SessionStoreError::Conflict {
                    expected,
                    actual,
                    ..
                } if expected == EventSeq::FIRST && actual == EventSeq::new(2).unwrap()
            ));
            assert_eq!(
                store.head(&id).await.unwrap().seq,
                EventSeq::new(2).unwrap()
            );
        });
    }

    #[test]
    fn stale_expected_head_wins_over_payload_validation() {
        block_on(async {
            let store = MemorySessionStore::new();
            let id = session_id("ses_conflict_precedence");
            store
                .create(create_request("ses_conflict_precedence"))
                .await
                .unwrap();
            store
                .append(&id, EventSeq::FIRST, vec![turn_started("evt_committed", 1)])
                .await
                .unwrap();

            let invalid_turn = TurnNo::new(2).unwrap();
            let invalid = NewSessionEvent::new(
                event_id("evt_invalid_stale"),
                timestamp(),
                SessionEventPayload::TurnStarted(TurnStarted { turn: invalid_turn }),
            );

            let error = store
                .append(&id, EventSeq::FIRST, vec![invalid])
                .await
                .unwrap_err();

            assert!(matches!(error, SessionStoreError::Conflict { .. }));
        });
    }

    #[test]
    fn invalid_append_batch_is_rejected_atomically() {
        block_on(async {
            let store = MemorySessionStore::new();
            let id = session_id("ses_invalid_batch");
            store
                .create(create_request("ses_invalid_batch"))
                .await
                .unwrap();

            let invalid_turn = TurnNo::FIRST;
            let invalid = NewSessionEvent::new(
                event_id("evt_invalid"),
                timestamp(),
                SessionEventPayload::TurnStarted(TurnStarted { turn: invalid_turn }),
            );

            let error = store
                .append(
                    &id,
                    EventSeq::FIRST,
                    vec![turn_started("evt_valid", 1), invalid],
                )
                .await
                .unwrap_err();

            assert!(matches!(error, SessionStoreError::InvalidArgument(_)));
            assert_eq!(store.head(&id).await.unwrap().seq, EventSeq::FIRST);
            assert_eq!(store.read(&id, EventSeq::ZERO, 100).await.unwrap().len(), 1);
        });
    }

    #[test]
    fn empty_append_is_a_checked_noop() {
        block_on(async {
            let store = MemorySessionStore::new();
            let id = session_id("ses_empty_append");
            store
                .create(create_request("ses_empty_append"))
                .await
                .unwrap();

            let result = store
                .append(&id, EventSeq::FIRST, Vec::new())
                .await
                .unwrap();

            assert_eq!(result.new_head, EventSeq::FIRST);
            assert!(result.committed.is_empty());

            let stale = store
                .append(&id, EventSeq::ZERO, Vec::new())
                .await
                .unwrap_err();
            assert!(matches!(stale, SessionStoreError::Conflict { .. }));
        });
    }

    #[test]
    fn read_is_inclusive_ordered_and_limited() {
        block_on(async {
            let store = MemorySessionStore::new();
            let id = session_id("ses_read");
            store.create(create_request("ses_read")).await.unwrap();
            store
                .append(
                    &id,
                    EventSeq::FIRST,
                    vec![turn_started("evt_t", 1), step_started("evt_s", 1, 1)],
                )
                .await
                .unwrap();

            let page = store.read(&id, EventSeq::new(2).unwrap(), 1).await.unwrap();
            assert_eq!(page.len(), 1);
            assert_eq!(page[0].seq(), EventSeq::new(2).unwrap());

            assert!(store.read(&id, EventSeq::ZERO, 0).await.unwrap().is_empty());
        });
    }

    #[test]
    fn missing_session_operations_return_not_found() {
        block_on(async {
            let store = MemorySessionStore::new();
            let id = session_id("ses_missing");

            assert!(matches!(
                store.head(&id).await.unwrap_err(),
                SessionStoreError::NotFound(_)
            ));
            assert!(matches!(
                store.read(&id, EventSeq::ZERO, 1).await.unwrap_err(),
                SessionStoreError::NotFound(_)
            ));
            assert!(matches!(
                store
                    .append(&id, EventSeq::ZERO, Vec::new())
                    .await
                    .unwrap_err(),
                SessionStoreError::NotFound(_)
            ));
        });
    }

    #[test]
    fn fork_copies_exact_prefix_and_rebinds_session_identity() {
        block_on(async {
            let store = MemorySessionStore::new();
            let source = session_id("ses_source");
            let target = session_id("ses_target");

            store.create(create_request("ses_source")).await.unwrap();
            store
                .append(
                    &source,
                    EventSeq::FIRST,
                    vec![turn_started("evt_t1", 1), step_started("evt_s1", 1, 1)],
                )
                .await
                .unwrap();

            let head = store
                .fork(ForkSession {
                    source_session_id: source.clone(),
                    through_seq: EventSeq::new(2).unwrap(),
                    target_session_id: target.clone(),
                })
                .await
                .unwrap();

            assert_eq!(head.session_id, target);
            assert_eq!(head.seq, EventSeq::new(2).unwrap());

            let source_prefix = store.read(&source, EventSeq::ZERO, 2).await.unwrap();
            let target_events = store.read(&target, EventSeq::ZERO, 100).await.unwrap();
            assert_eq!(target_events.len(), 2);

            for (source_event, target_event) in source_prefix.iter().zip(&target_events) {
                assert_eq!(source_event.seq(), target_event.seq());
                assert_eq!(source_event.event_id(), target_event.event_id());
                assert_eq!(source_event.timestamp(), target_event.timestamp());
                assert_eq!(source_event.turn(), target_event.turn());
                assert_eq!(source_event.step(), target_event.step());
                assert_eq!(source_event.payload(), target_event.payload());
                assert_eq!(target_event.session_id(), &target);
            }

            // The source event at seq 3 is beyond the fork boundary.
            assert_eq!(
                store.head(&source).await.unwrap().seq,
                EventSeq::new(3).unwrap()
            );
            assert_eq!(
                store.head(&target).await.unwrap().seq,
                EventSeq::new(2).unwrap()
            );
        });
    }

    #[test]
    fn fork_validates_boundary_and_target_uniqueness() {
        block_on(async {
            let store = MemorySessionStore::new();
            let source = session_id("ses_fork_source");
            let target = session_id("ses_fork_target");
            store
                .create(create_request("ses_fork_source"))
                .await
                .unwrap();

            let zero_error = store
                .fork(ForkSession {
                    source_session_id: source.clone(),
                    through_seq: EventSeq::ZERO,
                    target_session_id: target.clone(),
                })
                .await
                .unwrap_err();
            assert!(matches!(zero_error, SessionStoreError::InvalidArgument(_)));

            let beyond_error = store
                .fork(ForkSession {
                    source_session_id: source.clone(),
                    through_seq: EventSeq::new(2).unwrap(),
                    target_session_id: target.clone(),
                })
                .await
                .unwrap_err();
            assert!(matches!(
                beyond_error,
                SessionStoreError::InvalidArgument(_)
            ));

            store
                .fork(ForkSession {
                    source_session_id: source.clone(),
                    through_seq: EventSeq::FIRST,
                    target_session_id: target.clone(),
                })
                .await
                .unwrap();

            let duplicate_target = store
                .fork(ForkSession {
                    source_session_id: source,
                    through_seq: EventSeq::FIRST,
                    target_session_id: target,
                })
                .await
                .unwrap_err();
            assert!(matches!(
                duplicate_target,
                SessionStoreError::AlreadyExists { .. }
            ));
        });
    }

    #[test]
    fn duplicate_event_id_is_rejected_before_commit() {
        block_on(async {
            let store = MemorySessionStore::new();
            let id = session_id("ses_duplicate_event_id");
            store
                .create(create_request("ses_duplicate_event_id"))
                .await
                .unwrap();

            let duplicate = NewSessionEvent::new(
                event_id("evt_create_ses_duplicate_event_id"),
                timestamp(),
                SessionEventPayload::InboxEnqueued(harness_session::InboxEnqueued {
                    message: harness_types::Message {
                        id: harness_types::MessageId::new("msg_dup").unwrap(),
                        role: harness_types::Role::User,
                        source: harness_types::MessageSource::user(),
                        content: vec![harness_types::ContentBlock::text("hello")],
                    },
                    target: harness_types::InboxTarget::NextTurn,
                    wakeup: true,
                }),
            );

            let error = store
                .append(&id, EventSeq::FIRST, vec![duplicate])
                .await
                .unwrap_err();
            assert!(matches!(error, SessionStoreError::InvalidArgument(_)));
            assert_eq!(store.head(&id).await.unwrap().seq, EventSeq::FIRST);
        });
    }

    #[test]
    fn concurrent_writers_with_same_expected_head_cannot_both_commit() {
        let store = MemorySessionStore::new();
        let id = session_id("ses_race");
        block_on(store.create(create_request("ses_race"))).unwrap();

        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();

        for index in 0..2 {
            let store = store.clone();
            let id = id.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                block_on(store.append(
                    &id,
                    EventSeq::FIRST,
                    vec![turn_started(&format!("evt_race_{index}"), index + 1)],
                ))
            }));
        }

        barrier.wait();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(SessionStoreError::Conflict { .. })))
                .count(),
            1
        );
        assert_eq!(
            block_on(store.head(&id)).unwrap().seq,
            EventSeq::new(2).unwrap()
        );
    }
}
