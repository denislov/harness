use std::sync::Arc;

use futures_util::StreamExt;
use harness_llm::{LlmProvider, LlmStreamAssembler, LlmStreamOutcome, ModelRequest};
use harness_session::StepPosition;
use harness_types::{ErrorCode, PortableError, RequestId};
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
    tx: mpsc::Sender<MailboxMessage>,
) -> JoinHandle<()> {
    let request_id = request.request_id.clone();
    tokio::spawn(async move {
        let mut stream = provider.stream(request);
        let mut assembler = LlmStreamAssembler::new();

        let outcome = loop {
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
