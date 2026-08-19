use harness_types::{CancelCause, EventId, EventSeq, InboxTarget, Message, MessageId};

/// State-changing commands accepted by a live Agent actor.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum AgentCommand {
    Send {
        message: Message,
        target: InboxTarget,
        wakeup: bool,
    },
    Cancel {
        cause: CancelCause,
        keep_inbox: bool,
    },
    Shutdown,
}

impl AgentCommand {
    pub fn send(message: Message, target: InboxTarget, wakeup: bool) -> Self {
        Self::Send {
            message,
            target,
            wakeup,
        }
    }

    pub fn followup(message: Message) -> Self {
        Self::send(message, InboxTarget::NextTurn, true)
    }

    pub fn steer(message: Message) -> Self {
        Self::send(message, InboxTarget::NextStep, true)
    }

    pub fn inject(message: Message) -> Self {
        Self::send(message, InboxTarget::NextStep, false)
    }

    pub const fn cancel(cause: CancelCause, keep_inbox: bool) -> Self {
        Self::Cancel { cause, keep_inbox }
    }
}

/// Durable acknowledgement for `AgentCommand::Send`.
///
/// Receipt delivery occurs only after `inbox/enqueued` has been committed and
/// incorporated into the actor's local projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendReceipt {
    pub message_id: MessageId,
    pub event_id: EventId,
    pub seq: EventSeq,
    pub wake_requested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentCommandAck {
    Send(SendReceipt),
    Cancelled,
    Shutdown,
}
