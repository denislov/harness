use std::{sync::Arc, time::Duration};

use harness_session::StepPosition;
use harness_tools::{ToolExecutor, ToolInvocation};
use harness_types::{ErrorCode, InvocationId, PortableError, ToolCallId, ToolOutcome};
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
    timeout_ms: u64,
    tx: mpsc::Sender<MailboxMessage>,
) -> JoinHandle<()> {
    let call_id = invocation.call_id.clone();
    let invocation_id = invocation.invocation_id.clone();
    tokio::spawn(async move {
        let outcome = match tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            executor.invoke(invocation),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => Err(PortableError::new(
                ErrorCode::DeadlineExceeded,
                format!(
                    "tool invocation {invocation_id} for call {call_id} exceeded timeout of {timeout_ms} ms"
                ),
            )),
        };
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
