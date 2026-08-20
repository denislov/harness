use std::{future::Future, pin::Pin};

use futures_core::Stream;
use harness_types::{CancelCause, PortableError, ProviderId, RequestId};

use crate::{ModelRequest, SequencedStreamEvent};

pub type LlmEventStream =
    Pin<Box<dyn Stream<Item = Result<SequencedStreamEvent, PortableError>> + Send + 'static>>;

/// Best-effort cancellation hook for one provider attempt.
///
/// Cancellation acknowledgement is owned by Agent durable state. This hook only
/// asks the underlying capability implementation to stop external work promptly.
pub type LlmCancelFuture =
    Pin<Box<dyn Future<Output = Result<(), PortableError>> + Send + 'static>>;

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

    /// Requests best-effort cancellation of one live provider attempt.
    ///
    /// The default is intentionally a successful no-op so existing in-process
    /// providers remain source compatible. Out-of-process adapters should
    /// override this hook when their transport exposes cancellation.
    fn cancel(&self, _request_id: RequestId, _cause: CancelCause) -> LlmCancelFuture {
        Box::pin(async { Ok(()) })
    }
}
