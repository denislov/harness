use std::{sync::Arc, time::Duration};

use harness_session::StepPosition;
use harness_tools::{ToolExecutor, ToolInvocation};
use harness_types::{CancelCause, ErrorCode, InvocationId, PortableError, ToolCallId, ToolOutcome};
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
        let invoke_executor = executor.clone();
        let outcome = match tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            invoke_executor.invoke(invocation),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => {
                // Cancellation is advisory. Core still interprets the durable
                // dispatch boundary by side-effect class after this error.
                let _ = executor
                    .cancel(invocation_id.clone(), CancelCause::Timeout)
                    .await;
                Err(PortableError::new(
                    ErrorCode::DeadlineExceeded,
                    format!(
                        "tool invocation {invocation_id} for call {call_id} exceeded timeout of {timeout_ms} ms"
                    ),
                ))
            }
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
