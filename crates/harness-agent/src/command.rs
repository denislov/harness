use harness_types::{CancelCause, InboxTarget, Message};

/// State-changing commands accepted by a live Agent actor.
///
/// Batch 04 freezes only the command vocabulary. Command transport, acknowledgement,
/// and driver execution are intentionally deferred to the next batch.
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
