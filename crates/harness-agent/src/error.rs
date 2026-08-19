use harness_session::SessionStoreError;
use harness_types::{ErrorCode, EventSeq, SessionId};
use thiserror::Error;

/// Stable Agent-layer failure surfaced to command callers and task supervision.
#[derive(Clone, Debug, PartialEq, Error)]
#[non_exhaustive]
pub enum AgentError {
    #[error(
        "agent lost single-writer ownership of session {session_id}: expected head {expected}, actual {actual}"
    )]
    OwnershipLost {
        session_id: SessionId,
        expected: EventSeq,
        actual: EventSeq,
    },

    #[error("session storage failure ({code:?}): {message}")]
    Storage { code: ErrorCode, message: String },

    #[error("invalid durable mutation: {message}")]
    InvalidDurableMutation { message: String },

    #[error("SessionStore contract violation: {message}")]
    StorageContractViolation { message: String },

    #[error("operation {operation} is not available in this Agent runtime stage: {reason}")]
    UnsupportedOperation {
        operation: &'static str,
        reason: &'static str,
    },
}

impl AgentError {
    pub(crate) fn from_store(error: SessionStoreError) -> Self {
        match error {
            SessionStoreError::Conflict {
                session_id,
                expected,
                actual,
            } => Self::OwnershipLost {
                session_id,
                expected,
                actual,
            },
            other => Self::Storage {
                code: other.code(),
                message: other.to_string(),
            },
        }
    }

    pub const fn is_terminal_for_actor(&self) -> bool {
        matches!(
            self,
            Self::OwnershipLost { .. } | Self::StorageContractViolation { .. }
        )
    }
}

#[derive(Debug, Error)]
pub enum AgentHandleError {
    #[error("agent actor mailbox is closed")]
    ActorClosed,

    #[error("agent actor dropped the command acknowledgement")]
    AcknowledgementDropped,

    #[error("agent actor returned an acknowledgement that does not match the submitted command")]
    AcknowledgementMismatch,

    #[error(transparent)]
    Command(#[from] AgentError),
}
