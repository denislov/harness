use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_session::{
    AppendResult, CreateSession, ForkSession, NewSessionEvent, SessionEvent, SessionHead,
    SessionStore, SessionStoreError,
};
use harness_types::{EventSeq, SessionId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppendFault {
    event_type: String,
    occurrence: usize,
}

impl AppendFault {
    pub fn after_event_type(event_type: impl Into<String>) -> Self {
        Self::after_event_type_occurrence(event_type, 1)
    }

    pub fn after_event_type_occurrence(event_type: impl Into<String>, occurrence: usize) -> Self {
        assert!(occurrence > 0, "fault occurrence must be greater than zero");
        Self {
            event_type: event_type.into(),
            occurrence,
        }
    }

    pub fn event_type(&self) -> &str {
        &self.event_type
    }

    pub const fn occurrence(&self) -> usize {
        self.occurrence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedAppend {
    pub expected_seq: EventSeq,
    pub new_head: EventSeq,
    pub event_types: Vec<String>,
}

#[derive(Debug)]
struct FaultState {
    spec: AppendFault,
    seen: usize,
    triggered: bool,
}

/// SessionStore wrapper that simulates a process death after an append has
/// committed but before the caller can observe the successful acknowledgement.
///
/// The inner store is always called first. When the configured event occurrence
/// appears in the committed batch, the wrapper records the successful append and
/// then returns `SessionStoreError::Internal`. Durable state therefore advances
/// while the Agent's process-local projection does not, which is the exact cut
/// Batch 20 needs to exercise.
pub struct FaultInjectingSessionStore {
    inner: Arc<dyn SessionStore>,
    state: Mutex<FaultState>,
    observed: Mutex<Vec<ObservedAppend>>,
}

impl FaultInjectingSessionStore {
    pub fn new(inner: Arc<dyn SessionStore>, fault: AppendFault) -> Self {
        Self {
            inner,
            state: Mutex::new(FaultState {
                spec: fault,
                seen: 0,
                triggered: false,
            }),
            observed: Mutex::new(Vec::new()),
        }
    }

    pub fn inner(&self) -> Arc<dyn SessionStore> {
        self.inner.clone()
    }

    pub fn triggered(&self) -> bool {
        self.state
            .lock()
            .expect("fault state lock poisoned")
            .triggered
    }

    pub fn observed_appends(&self) -> Vec<ObservedAppend> {
        self.observed
            .lock()
            .expect("observed append lock poisoned")
            .clone()
    }

    fn should_drop_ack(&self, event_types: &[String]) -> bool {
        let mut state = self.state.lock().expect("fault state lock poisoned");
        if state.triggered {
            return false;
        }

        for event_type in event_types {
            if event_type == &state.spec.event_type {
                state.seen += 1;
                if state.seen == state.spec.occurrence {
                    state.triggered = true;
                    return true;
                }
            }
        }
        false
    }
}

#[async_trait]
impl SessionStore for FaultInjectingSessionStore {
    async fn create(&self, request: CreateSession) -> Result<SessionEvent, SessionStoreError> {
        self.inner.create(request).await
    }

    async fn append(
        &self,
        session_id: &SessionId,
        expected_seq: EventSeq,
        events: Vec<NewSessionEvent>,
    ) -> Result<AppendResult, SessionStoreError> {
        let result = self.inner.append(session_id, expected_seq, events).await?;
        let event_types = result
            .committed
            .iter()
            .map(|event| event.payload().event_type().to_owned())
            .collect::<Vec<_>>();
        self.observed
            .lock()
            .expect("observed append lock poisoned")
            .push(ObservedAppend {
                expected_seq,
                new_head: result.new_head,
                event_types: event_types.clone(),
            });

        if self.should_drop_ack(&event_types) {
            return Err(SessionStoreError::Internal(format!(
                "Batch 20 injected post-commit acknowledgement loss after append [{}]",
                event_types.join(", ")
            )));
        }

        Ok(result)
    }

    async fn read(
        &self,
        session_id: &SessionId,
        from_seq: EventSeq,
        limit: usize,
    ) -> Result<Vec<SessionEvent>, SessionStoreError> {
        self.inner.read(session_id, from_seq, limit).await
    }

    async fn head(&self, session_id: &SessionId) -> Result<SessionHead, SessionStoreError> {
        self.inner.head(session_id).await
    }

    async fn fork(&self, request: ForkSession) -> Result<SessionHead, SessionStoreError> {
        self.inner.fork(request).await
    }
}
