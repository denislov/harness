use std::sync::Arc;

use harness_session::StepPosition;
use harness_tools::{ToolExecutor, ToolInvocation};
use harness_types::{InvocationId, PortableError, ToolCallId, ToolOutcome};
use tokio::{sync::mpsc, task::JoinHandle};

use crate::MailboxMessage;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToolCompletion {
    pub position: StepPosition,
    pub call_id: ToolCallId,
    pub invocation_id: InvocationId,
    pub outcome: Result<ToolOutcome, PortableError>,
}

pub(crate) fn spawn_tool_operation(
    executor: Arc<dyn ToolExecutor>,
    invocation: ToolInvocation,
    position: StepPosition,
    tx: mpsc::Sender<MailboxMessage>,
) -> JoinHandle<()> {
    let call_id = invocation.call_id.clone();
    let invocation_id = invocation.invocation_id.clone();
    tokio::spawn(async move {
        let outcome = executor.invoke(invocation).await;
        let _ = tx
            .send(MailboxMessage::ToolCompleted(ToolCompletion {
                position,
                call_id,
                invocation_id,
                outcome,
            }))
            .await;
    })
}
