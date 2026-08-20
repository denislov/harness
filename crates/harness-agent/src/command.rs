use harness_types::{
    ApprovalDecision, ApprovalId, CancelCause, EventId, EventSeq, InboxTarget, Message, MessageId,
};

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
    ResolveApproval {
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        note: Option<String>,
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

    pub fn resolve_approval(
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        note: Option<String>,
    ) -> Self {
        Self::ResolveApproval {
            approval_id,
            decision,
            note,
        }
    }
}

/// Durable acknowledgement for `AgentCommand::Send`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendReceipt {
    pub message_id: MessageId,
    pub event_id: EventId,
    pub seq: EventSeq,
    pub wake_requested: bool,
}

/// Durable acknowledgement for an approval resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalReceipt {
    pub approval_id: ApprovalId,
    pub decision: ApprovalDecision,
    pub event_id: EventId,
    pub seq: EventSeq,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentCommandAck {
    Send(SendReceipt),
    Cancelled,
    ApprovalResolved(ApprovalReceipt),
    Shutdown,
}
