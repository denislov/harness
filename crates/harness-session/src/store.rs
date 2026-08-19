use async_trait::async_trait;
use harness_types::{ErrorCode, EventId, EventSeq, PortableError, SessionId, Timestamp};
use thiserror::Error;

use crate::{NewSessionEvent, SessionCreated, SessionEvent};

#[derive(Clone, Debug, PartialEq)]
pub struct CreateSession {
    pub session_id: SessionId,
    pub event_id: EventId,
    pub timestamp: Timestamp,
    pub data: SessionCreated,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppendResult {
    pub new_head: EventSeq,
    pub committed: Vec<SessionEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionHead {
    pub session_id: SessionId,
    pub seq: EventSeq,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkSession {
    pub source_session_id: SessionId,
    pub through_seq: EventSeq,
    pub target_session_id: SessionId,
}

#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error("session {0} was not found")]
    NotFound(SessionId),

    #[error("session {session_id} already exists")]
    AlreadyExists { session_id: SessionId },

    #[error("session {session_id} head conflict: expected {expected}, actual {actual}")]
    Conflict {
        session_id: SessionId,
        expected: EventSeq,
        actual: EventSeq,
    },

    #[error("session {session_id} is corrupt: {reason}")]
    Corrupt {
        session_id: SessionId,
        reason: String,
    },

    #[error("invalid session operation: {0}")]
    InvalidArgument(String),

    #[error("session storage failure: {0}")]
    Internal(String),
}

impl SessionStoreError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::AlreadyExists { .. } | Self::Conflict { .. } => ErrorCode::Conflict,
            Self::Corrupt { .. } => ErrorCode::SessionCorrupt,
            Self::InvalidArgument(_) => ErrorCode::InvalidArgument,
            Self::Internal(_) => ErrorCode::Internal,
        }
    }

    pub fn to_portable(&self) -> PortableError {
        PortableError::new(self.code(), self.to_string())
    }
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create(&self, request: CreateSession) -> Result<SessionEvent, SessionStoreError>;

    async fn append(
        &self,
        session_id: &SessionId,
        expected_seq: EventSeq,
        events: Vec<NewSessionEvent>,
    ) -> Result<AppendResult, SessionStoreError>;

    /// Reads committed events with `seq >= from_seq`, in strictly ascending order.
    async fn read(
        &self,
        session_id: &SessionId,
        from_seq: EventSeq,
        limit: usize,
    ) -> Result<Vec<SessionEvent>, SessionStoreError>;

    async fn head(&self, session_id: &SessionId) -> Result<SessionHead, SessionStoreError>;

    async fn fork(&self, request: ForkSession) -> Result<SessionHead, SessionStoreError>;
}
