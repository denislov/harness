use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use harness_session::{
    AppendResult, CreateSession, ForkSession, NewSessionEvent, SessionEvent, SessionEventPayload,
    SessionHead, SessionStore, SessionStoreError,
};
use harness_types::{EventId, EventSeq, SessionId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const SCHEMA_VERSION: i64 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY NOT NULL,
    head_seq INTEGER NOT NULL CHECK (head_seq >= 1)
);

CREATE TABLE IF NOT EXISTS session_events (
    session_id TEXT NOT NULL,
    seq INTEGER NOT NULL CHECK (seq >= 1),
    event_id TEXT NOT NULL,
    event_json TEXT NOT NULL,
    PRIMARY KEY (session_id, seq),
    UNIQUE (session_id, event_id),
    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
);
"#;

/// Durable SQLite implementation of [`SessionStore`].
///
/// Every mutating operation uses an IMMEDIATE transaction. `append` reads and
/// checks the durable head before validating the proposed batch, preserving the
/// SessionStore rule that a stale writer observes CONFLICT before event-shape
/// errors. The complete batch and head update commit atomically.
#[derive(Clone, Debug)]
pub struct SqliteSessionStore {
    path: Arc<PathBuf>,
}

impl SqliteSessionStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, SessionStoreError> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                SessionStoreError::Internal(format!(
                    "SqliteSessionStore cannot create database directory: {error}"
                ))
            })?;
        }
        let mut connection = open_connection(&path)?;
        initialize_schema(&mut connection)?;
        Ok(Self {
            path: Arc::new(path),
        })
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn create(&self, request: CreateSession) -> Result<SessionEvent, SessionStoreError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || create_sync(path.as_path(), request))
            .await
            .map_err(join_error)?
    }

    async fn append(
        &self,
        session_id: &SessionId,
        expected_seq: EventSeq,
        events: Vec<NewSessionEvent>,
    ) -> Result<AppendResult, SessionStoreError> {
        let path = self.path.clone();
        let session_id = session_id.clone();
        tokio::task::spawn_blocking(move || {
            append_sync(path.as_path(), session_id, expected_seq, events)
        })
        .await
        .map_err(join_error)?
    }

    async fn read(
        &self,
        session_id: &SessionId,
        from_seq: EventSeq,
        limit: usize,
    ) -> Result<Vec<SessionEvent>, SessionStoreError> {
        let path = self.path.clone();
        let session_id = session_id.clone();
        tokio::task::spawn_blocking(move || {
            let connection = open_connection(path.as_path())?;
            let (_, log) = load_validated_log(&connection, &session_id)?;
            if limit == 0 {
                return Ok(Vec::new());
            }
            Ok(log
                .into_iter()
                .filter(|event| event.seq() >= from_seq)
                .take(limit)
                .collect())
        })
        .await
        .map_err(join_error)?
    }

    async fn head(&self, session_id: &SessionId) -> Result<SessionHead, SessionStoreError> {
        let path = self.path.clone();
        let session_id = session_id.clone();
        tokio::task::spawn_blocking(move || {
            let connection = open_connection(path.as_path())?;
            let (seq, _) = load_validated_log(&connection, &session_id)?;
            Ok(SessionHead { session_id, seq })
        })
        .await
        .map_err(join_error)?
    }

    async fn fork(&self, request: ForkSession) -> Result<SessionHead, SessionStoreError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || fork_sync(path.as_path(), request))
            .await
            .map_err(join_error)?
    }
}

fn create_sync(path: &Path, request: CreateSession) -> Result<SessionEvent, SessionStoreError> {
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
    let encoded = encode_event(&event)?;

    let mut connection = open_connection(path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_internal("begin create transaction"))?;
    if session_exists(&transaction, &session_id)? {
        return Err(SessionStoreError::AlreadyExists { session_id });
    }
    transaction
        .execute(
            "INSERT INTO sessions(session_id, head_seq) VALUES (?1, ?2)",
            params![session_id.as_str(), seq_to_i64(EventSeq::FIRST)],
        )
        .map_err(sql_internal("insert session"))?;
    insert_event(&transaction, &event, &encoded)?;
    transaction
        .commit()
        .map_err(sql_internal("commit create transaction"))?;
    Ok(event)
}

fn append_sync(
    path: &Path,
    session_id: SessionId,
    expected_seq: EventSeq,
    events: Vec<NewSessionEvent>,
) -> Result<AppendResult, SessionStoreError> {
    let mut connection = open_connection(path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_internal("begin append transaction"))?;

    // Validate the existing committed prefix before accepting another write.
    // This mirrors MemorySessionStore::log_head(), which treats durable-log
    // corruption as authoritative before evaluating the caller's new batch.
    let (actual, _) = load_validated_log(&transaction, &session_id)?;
    if actual != expected_seq {
        return Err(SessionStoreError::Conflict {
            session_id,
            expected: expected_seq,
            actual,
        });
    }

    // Match MemorySessionStore ordering: stale-writer conflict is authoritative;
    // only a writer at the current head validates its proposed batch.
    let mut batch_event_ids = HashSet::new();
    for (index, draft) in events.iter().enumerate() {
        draft.validate().map_err(|error| {
            SessionStoreError::InvalidArgument(format!(
                "invalid event at append batch index {index}: {error}"
            ))
        })?;
        if !batch_event_ids.insert(draft.event_id().clone())
            || event_id_exists(&transaction, &session_id, draft.event_id())?
        {
            return Err(SessionStoreError::InvalidArgument(format!(
                "duplicate event id {} at append batch index {index}",
                draft.event_id()
            )));
        }
    }

    let mut next_seq = actual;
    let mut committed = Vec::with_capacity(events.len());
    for draft in events {
        next_seq = next_seq.checked_next().map_err(|error| {
            SessionStoreError::InvalidArgument(format!(
                "cannot allocate the next event sequence: {error}"
            ))
        })?;
        committed.push(
            SessionEvent::committed(session_id.clone(), next_seq, draft).map_err(|error| {
                SessionStoreError::InvalidArgument(format!("invalid event: {error}"))
            })?,
        );
    }

    for event in &committed {
        let encoded = encode_event(event)?;
        insert_event(&transaction, event, &encoded)?;
    }
    if committed.is_empty() {
        transaction
            .commit()
            .map_err(sql_internal("commit empty append transaction"))?;
        return Ok(AppendResult {
            new_head: actual,
            committed,
        });
    }
    let updated = transaction
        .execute(
            "UPDATE sessions SET head_seq = ?1 WHERE session_id = ?2 AND head_seq = ?3",
            params![
                seq_to_i64(next_seq),
                session_id.as_str(),
                seq_to_i64(actual)
            ],
        )
        .map_err(sql_internal("update session head"))?;
    if updated != 1 {
        return Err(SessionStoreError::Internal(format!(
            "SqliteSessionStore append lost head ownership for {session_id} inside an IMMEDIATE transaction"
        )));
    }
    transaction
        .commit()
        .map_err(sql_internal("commit append transaction"))?;

    Ok(AppendResult {
        new_head: next_seq,
        committed,
    })
}

fn fork_sync(path: &Path, request: ForkSession) -> Result<SessionHead, SessionStoreError> {
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

    let mut connection = open_connection(path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_internal("begin fork transaction"))?;
    if session_exists(&transaction, &target_session_id)? {
        return Err(SessionStoreError::AlreadyExists {
            session_id: target_session_id,
        });
    }

    let (source_head, source) = load_validated_log(&transaction, &source_session_id)?;
    if through_seq > source_head {
        return Err(SessionStoreError::InvalidArgument(format!(
            "fork through_seq {through_seq} exceeds source head {source_head}"
        )));
    }
    let source_prefix: Vec<_> = source
        .iter()
        .take_while(|event| event.seq() <= through_seq)
        .collect();
    if source_prefix
        .last()
        .map(|event| event.seq())
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
        target_log.push(rebind_session(event, &target_session_id)?);
    }
    validate_log(&target_session_id, &target_log)?;

    transaction
        .execute(
            "INSERT INTO sessions(session_id, head_seq) VALUES (?1, ?2)",
            params![target_session_id.as_str(), seq_to_i64(through_seq)],
        )
        .map_err(sql_internal("insert fork target session"))?;
    for event in &target_log {
        let encoded = encode_event(event)?;
        insert_event(&transaction, event, &encoded)?;
    }
    transaction
        .commit()
        .map_err(sql_internal("commit fork transaction"))?;

    Ok(SessionHead {
        session_id: target_session_id,
        seq: through_seq,
    })
}

fn initialize_schema(connection: &mut Connection) -> Result<(), SessionStoreError> {
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(sql_internal("enable WAL"))?;
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sql_internal("read schema version"))?;
    match version {
        0 => {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_internal("begin schema transaction"))?;
            transaction
                .execute_batch(SCHEMA_SQL)
                .map_err(sql_internal("create schema"))?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(sql_internal("set schema version"))?;
            transaction
                .commit()
                .map_err(sql_internal("commit schema transaction"))?;
        }
        SCHEMA_VERSION => {
            connection
                .execute_batch(SCHEMA_SQL)
                .map_err(sql_internal("verify schema objects"))?;
        }
        other => {
            return Err(SessionStoreError::Internal(format!(
                "SqliteSessionStore schema version {other} is unsupported; expected {SCHEMA_VERSION}"
            )));
        }
    }
    Ok(())
}

fn open_connection(path: &Path) -> Result<Connection, SessionStoreError> {
    let connection = Connection::open(path).map_err(sql_internal("open database"))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(sql_internal("configure busy timeout"))?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(sql_internal("enable foreign keys"))?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(sql_internal("set synchronous=FULL"))?;
    Ok(connection)
}

fn session_exists(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<bool, SessionStoreError> {
    connection
        .query_row(
            "SELECT 1 FROM sessions WHERE session_id = ?1",
            params![session_id.as_str()],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(sql_internal("check session existence"))
}

fn event_id_exists(
    connection: &Connection,
    session_id: &SessionId,
    event_id: &EventId,
) -> Result<bool, SessionStoreError> {
    connection
        .query_row(
            "SELECT 1 FROM session_events WHERE session_id = ?1 AND event_id = ?2",
            params![session_id.as_str(), event_id.as_str()],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(sql_internal("check event id existence"))
}

fn read_head(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<EventSeq, SessionStoreError> {
    let raw: Option<i64> = connection
        .query_row(
            "SELECT head_seq FROM sessions WHERE session_id = ?1",
            params![session_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_internal("read session head"))?;
    let raw = raw.ok_or_else(|| SessionStoreError::NotFound(session_id.clone()))?;
    seq_from_i64(session_id, raw, "sessions.head_seq")
}

fn load_validated_log(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<(EventSeq, Vec<SessionEvent>), SessionStoreError> {
    let head = read_head(connection, session_id)?;
    let mut statement = connection
        .prepare(
            "SELECT seq, event_id, event_json FROM session_events WHERE session_id = ?1 ORDER BY seq ASC",
        )
        .map_err(sql_internal("prepare session log read"))?;
    let mut rows = statement
        .query(params![session_id.as_str()])
        .map_err(sql_internal("query session log"))?;
    let mut log = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(sql_internal("advance session log row"))?
    {
        let raw_seq: i64 = row.get(0).map_err(sql_internal("read event seq"))?;
        let stored_event_id: String = row.get(1).map_err(sql_internal("read event id"))?;
        let encoded: String = row.get(2).map_err(sql_internal("read event json"))?;
        let event: SessionEvent =
            serde_json::from_str(&encoded).map_err(|error| SessionStoreError::Corrupt {
                session_id: session_id.clone(),
                reason: format!("cannot decode committed event JSON: {error}"),
            })?;
        let column_seq = seq_from_i64(session_id, raw_seq, "session_events.seq")?;
        if event.seq() != column_seq {
            return Err(SessionStoreError::Corrupt {
                session_id: session_id.clone(),
                reason: format!(
                    "event {} JSON seq {} disagrees with SQLite seq {column_seq}",
                    event.event_id(),
                    event.seq()
                ),
            });
        }
        if event.event_id().as_str() != stored_event_id {
            return Err(SessionStoreError::Corrupt {
                session_id: session_id.clone(),
                reason: format!(
                    "event JSON id {} disagrees with SQLite event_id {stored_event_id}",
                    event.event_id()
                ),
            });
        }
        log.push(event);
    }
    validate_log(session_id, &log)?;
    let observed = log.last().expect("validated log is non-empty").seq();
    if observed != head {
        return Err(SessionStoreError::Corrupt {
            session_id: session_id.clone(),
            reason: format!("sessions.head_seq is {head}, event log ends at {observed}"),
        });
    }
    Ok((head, log))
}

fn insert_event(
    connection: &Connection,
    event: &SessionEvent,
    encoded: &str,
) -> Result<(), SessionStoreError> {
    connection
        .execute(
            "INSERT INTO session_events(session_id, seq, event_id, event_json) VALUES (?1, ?2, ?3, ?4)",
            params![
                event.session_id().as_str(),
                seq_to_i64(event.seq()),
                event.event_id().as_str(),
                encoded
            ],
        )
        .map_err(sql_internal("insert session event"))?;
    Ok(())
}

fn encode_event(event: &SessionEvent) -> Result<String, SessionStoreError> {
    serde_json::to_string(event).map_err(|error| {
        SessionStoreError::Internal(format!(
            "SqliteSessionStore cannot serialize event {}: {error}",
            event.event_id()
        ))
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
            Err(_) if event.seq() == log.last().expect("log is non-empty").seq() => break,
            Err(_) => {
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

fn seq_to_i64(seq: EventSeq) -> i64 {
    i64::try_from(seq.get()).expect("EventSeq is bounded below i64::MAX")
}

fn seq_from_i64(
    session_id: &SessionId,
    raw: i64,
    column: &str,
) -> Result<EventSeq, SessionStoreError> {
    let value = u64::try_from(raw).map_err(|_| SessionStoreError::Corrupt {
        session_id: session_id.clone(),
        reason: format!("{column} contains negative sequence {raw}"),
    })?;
    EventSeq::new(value).map_err(|error| SessionStoreError::Corrupt {
        session_id: session_id.clone(),
        reason: format!("{column} contains invalid sequence: {error}"),
    })
}

fn sql_internal(context: &'static str) -> impl FnOnce(rusqlite::Error) -> SessionStoreError {
    move |error| SessionStoreError::Internal(format!("SqliteSessionStore {context}: {error}"))
}

fn join_error(error: tokio::task::JoinError) -> SessionStoreError {
    SessionStoreError::Internal(format!("SqliteSessionStore blocking task failed: {error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use harness_session::{SessionCreated, SessionEventPayload, TurnStarted};
    use harness_types::{EventId, Timestamp, TurnNo};

    use super::*;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    fn test_db(label: &str) -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "harness-sqlite-session-{label}-{}-{id}.sqlite3",
            std::process::id()
        ))
    }

    fn timestamp() -> Timestamp {
        Timestamp::parse("2026-08-20T08:30:00Z").unwrap()
    }

    fn session(value: &str) -> SessionId {
        SessionId::new(value).unwrap()
    }

    #[tokio::test]
    async fn empty_append_preserves_head() {
        let path = test_db("empty-append");
        let id = session("ses_sqlite_empty");
        let store = SqliteSessionStore::open(&path).unwrap();
        store
            .create(CreateSession {
                session_id: id.clone(),
                event_id: EventId::new("evt_sqlite_empty_create").unwrap(),
                timestamp: timestamp(),
                data: SessionCreated::default(),
            })
            .await
            .unwrap();
        let result = store
            .append(&id, EventSeq::FIRST, Vec::new())
            .await
            .unwrap();
        assert_eq!(result.new_head, EventSeq::FIRST);
        assert!(result.committed.is_empty());
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn stale_writer_conflict_precedes_batch_validation() {
        use harness_session::StepStarted;
        use harness_types::StepNo;

        let path = test_db("conflict-order");
        let id = session("ses_sqlite_conflict_order");
        let store = SqliteSessionStore::open(&path).unwrap();
        store
            .create(CreateSession {
                session_id: id.clone(),
                event_id: EventId::new("evt_sqlite_conflict_create").unwrap(),
                timestamp: timestamp(),
                data: SessionCreated::default(),
            })
            .await
            .unwrap();

        let turn = TurnNo::new(1).unwrap();
        let step = StepNo::new(1).unwrap();
        let invalid = NewSessionEvent::new(
            EventId::new("evt_sqlite_invalid_step").unwrap(),
            timestamp(),
            SessionEventPayload::StepStarted(StepStarted { turn, step }),
        );
        let stale = store
            .append(&id, EventSeq::ZERO, vec![invalid.clone()])
            .await;
        assert!(matches!(stale, Err(SessionStoreError::Conflict { .. })));
        let current = store.append(&id, EventSeq::FIRST, vec![invalid]).await;
        assert!(matches!(
            current,
            Err(SessionStoreError::InvalidArgument(_))
        ));
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn session_log_and_fork_survive_reopen() {
        let path = test_db("reopen");
        let source = session("ses_sqlite_source");
        let target = session("ses_sqlite_target");
        let first = SqliteSessionStore::open(&path).unwrap();
        first
            .create(CreateSession {
                session_id: source.clone(),
                event_id: EventId::new("evt_sqlite_create").unwrap(),
                timestamp: timestamp(),
                data: SessionCreated::default(),
            })
            .await
            .unwrap();
        let turn = TurnNo::new(1).unwrap();
        first
            .append(
                &source,
                EventSeq::FIRST,
                vec![
                    NewSessionEvent::new(
                        EventId::new("evt_sqlite_turn").unwrap(),
                        timestamp(),
                        SessionEventPayload::TurnStarted(TurnStarted { turn }),
                    )
                    .in_turn(turn),
                ],
            )
            .await
            .unwrap();
        drop(first);

        let reopened = SqliteSessionStore::open(&path).unwrap();
        assert_eq!(reopened.head(&source).await.unwrap().seq.get(), 2);
        let events = reopened.read(&source, EventSeq::FIRST, 16).await.unwrap();
        assert_eq!(events.len(), 2);
        reopened
            .fork(ForkSession {
                source_session_id: source,
                through_seq: EventSeq::new(2).unwrap(),
                target_session_id: target.clone(),
            })
            .await
            .unwrap();
        drop(reopened);

        let second_reopen = SqliteSessionStore::open(&path).unwrap();
        let forked = second_reopen
            .read(&target, EventSeq::FIRST, 16)
            .await
            .unwrap();
        assert_eq!(forked.len(), 2);
        assert!(forked.iter().all(|event| event.session_id() == &target));

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("sqlite3-wal"));
        let _ = fs::remove_file(path.with_extension("sqlite3-shm"));
    }
}
