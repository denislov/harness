use std::pin::Pin;

use futures_core::Stream;
use harness_types::{PortableError, ProviderId};

use crate::{ModelRequest, SequencedStreamEvent};

pub type LlmEventStream =
    Pin<Box<dyn Stream<Item = Result<SequencedStreamEvent, PortableError>> + Send + 'static>>;

/// One normalized LLM capability provider.
///
/// `stream` represents exactly one provider attempt. Logical retry policy is
/// owned by Harness Core and therefore remains visible through additional
/// durable `model/requested` attempts.
///
/// `stream` itself must return promptly and must not perform blocking I/O. Any
/// asynchronous setup belongs in the returned Stream. This keeps the capability
/// seam usable on single-threaded as well as multi-threaded async executors.
pub trait LlmProvider: Send + Sync {
    fn provider_id(&self) -> &ProviderId;

    fn stream(&self, request: ModelRequest) -> LlmEventStream;
}
