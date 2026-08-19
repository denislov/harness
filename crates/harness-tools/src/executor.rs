use std::{future::Future, pin::Pin};

use harness_types::{PortableError, ProviderId, ToolOutcome};

use crate::ToolInvocation;

pub type ToolExecutionFuture =
    Pin<Box<dyn Future<Output = Result<ToolOutcome, PortableError>> + Send + 'static>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdempotencySupport {
    None,
    Keyed,
}

/// One normalized Tool capability executor.
///
/// `invoke` represents exactly one provider attempt. It must return promptly and
/// place asynchronous work in the returned Future. Retry policy remains owned by
/// Harness Core so every retry is visible through a new durable `tool/dispatched`
/// event.
///
/// `Ok(ToolOutcome)` is an authoritative provider-level terminal outcome.
/// `Err(PortableError)` means the attempt did not produce an authoritative Tool
/// outcome (for example transport/process failure); Core must interpret that
/// ambiguity using the durable dispatch boundary and SideEffectClass.
pub trait ToolExecutor: Send + Sync {
    fn provider_id(&self) -> &ProviderId;

    fn idempotency_support(&self) -> IdempotencySupport {
        IdempotencySupport::None
    }

    fn invoke(&self, invocation: ToolInvocation) -> ToolExecutionFuture;
}
