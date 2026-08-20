use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use harness_llm::{LlmProvider, LlmStreamAssembler, LlmStreamOutcome, ModelRequest};
use harness_session::StepPosition;
use harness_types::{CancelCause, ErrorCode, PortableError, RequestId};
use tokio::{sync::mpsc, task::JoinHandle};

use crate::MailboxMessage;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LlmCompletion {
    pub position: StepPosition,
    pub request_id: RequestId,
    pub outcome: Result<LlmStreamOutcome, PortableError>,
}

pub(crate) fn spawn_llm_operation(
    provider: Arc<dyn LlmProvider>,
    request: ModelRequest,
    position: StepPosition,
    timeout_ms: u64,
    tx: mpsc::Sender<MailboxMessage>,
) -> JoinHandle<()> {
    let request_id = request.request_id.clone();
    tokio::spawn(async move {
        let stream_provider = provider.clone();
        let operation = async move {
            let mut stream = stream_provider.stream(request);
            let mut assembler = LlmStreamAssembler::new();

            loop {
                match stream.next().await {
                    Some(Ok(event)) => {
                        if let Err(error) = assembler.push(event) {
                            break Err(PortableError::new(
                                ErrorCode::ProviderProtocolError,
                                error.to_string(),
                            ));
                        }
                    }
                    Some(Err(error)) => break Err(error),
                    None => {
                        break assembler.finish().map_err(|error| {
                            PortableError::new(ErrorCode::ProviderProtocolError, error.to_string())
                        });
                    }
                }
            }
        };

        let outcome = match tokio::time::timeout(Duration::from_millis(timeout_ms), operation).await
        {
            Ok(outcome) => outcome,
            Err(_) => {
                // Provider cancellation is best effort. The durable timeout
                // outcome remains authoritative even if the transport is gone.
                let _ = provider
                    .cancel(request_id.clone(), CancelCause::Timeout)
                    .await;
                Err(PortableError::new(
                    ErrorCode::DeadlineExceeded,
                    format!("model request {request_id} exceeded timeout of {timeout_ms} ms"),
                ))
            }
        };

        let _ = tx
            .send(MailboxMessage::LlmCompleted(LlmCompletion {
                position,
                request_id,
                outcome,
            }))
            .await;
    })
}
