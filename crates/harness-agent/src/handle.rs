use harness_types::{AgentInstanceId, CancelCause, InboxTarget, Message, SessionId};
use tokio::sync::{mpsc, oneshot};

use crate::{
    AgentCommand, AgentCommandAck, AgentHandleError, AgentState, LlmCompletion, SendReceipt,
    ToolCompletion,
};

pub(crate) enum MailboxMessage {
    Command {
        command: AgentCommand,
        reply: oneshot::Sender<Result<AgentCommandAck, crate::AgentError>>,
    },
    Snapshot {
        reply: oneshot::Sender<AgentState>,
    },
    LlmCompleted(LlmCompletion),
    ToolCompleted(ToolCompletion),
}

/// Cloneable application-facing handle for one live Agent actor.
///
/// The handle never exposes mutable Agent state. All writes are serialized by
/// the actor mailbox. A successful command acknowledgement is the boundary at
/// which the caller may rely on the corresponding durable mutation.
#[derive(Clone)]
pub struct AgentHandle {
    instance_id: AgentInstanceId,
    session_id: SessionId,
    tx: mpsc::Sender<MailboxMessage>,
}

impl AgentHandle {
    pub(crate) fn new(
        instance_id: AgentInstanceId,
        session_id: SessionId,
        tx: mpsc::Sender<MailboxMessage>,
    ) -> Self {
        Self {
            instance_id,
            session_id,
            tx,
        }
    }

    pub fn instance_id(&self) -> &AgentInstanceId {
        &self.instance_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub async fn submit(&self, command: AgentCommand) -> Result<AgentCommandAck, AgentHandleError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(MailboxMessage::Command {
                command,
                reply: reply_tx,
            })
            .await
            .map_err(|_| AgentHandleError::ActorClosed)?;

        reply_rx
            .await
            .map_err(|_| AgentHandleError::AcknowledgementDropped)?
            .map_err(AgentHandleError::Command)
    }

    pub async fn send(
        &self,
        message: Message,
        target: InboxTarget,
        wakeup: bool,
    ) -> Result<SendReceipt, AgentHandleError> {
        match self
            .submit(AgentCommand::send(message, target, wakeup))
            .await?
        {
            AgentCommandAck::Send(receipt) => Ok(receipt),
            _ => Err(AgentHandleError::AcknowledgementMismatch),
        }
    }

    pub async fn followup(&self, message: Message) -> Result<SendReceipt, AgentHandleError> {
        match self.submit(AgentCommand::followup(message)).await? {
            AgentCommandAck::Send(receipt) => Ok(receipt),
            _ => Err(AgentHandleError::AcknowledgementMismatch),
        }
    }

    pub async fn steer(&self, message: Message) -> Result<SendReceipt, AgentHandleError> {
        match self.submit(AgentCommand::steer(message)).await? {
            AgentCommandAck::Send(receipt) => Ok(receipt),
            _ => Err(AgentHandleError::AcknowledgementMismatch),
        }
    }

    pub async fn inject(&self, message: Message) -> Result<SendReceipt, AgentHandleError> {
        match self.submit(AgentCommand::inject(message)).await? {
            AgentCommandAck::Send(receipt) => Ok(receipt),
            _ => Err(AgentHandleError::AcknowledgementMismatch),
        }
    }

    pub async fn cancel(
        &self,
        cause: CancelCause,
        keep_inbox: bool,
    ) -> Result<(), AgentHandleError> {
        match self.submit(AgentCommand::cancel(cause, keep_inbox)).await? {
            AgentCommandAck::Cancelled => Ok(()),
            _ => Err(AgentHandleError::AcknowledgementMismatch),
        }
    }

    pub async fn snapshot(&self) -> Result<AgentState, AgentHandleError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(MailboxMessage::Snapshot { reply: reply_tx })
            .await
            .map_err(|_| AgentHandleError::ActorClosed)?;
        reply_rx
            .await
            .map_err(|_| AgentHandleError::AcknowledgementDropped)
    }

    pub async fn shutdown(&self) -> Result<(), AgentHandleError> {
        match self.submit(AgentCommand::Shutdown).await? {
            AgentCommandAck::Shutdown => Ok(()),
            _ => Err(AgentHandleError::AcknowledgementMismatch),
        }
    }
}
