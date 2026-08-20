//! Provider-neutral Tool domain and in-process execution seams.
//!
//! This crate owns Tool definitions, registry resolution, Core-side argument
//! validation, policy decisions, and the one-attempt executor interface. It does
//! not own Session mutation, Agent state, Provider process supervision, or retry
//! policy.

mod definition;
mod executor;
mod invocation;
mod policy;
mod registry;
mod validation;

pub use definition::{ToolDefinition, ToolDefinitionError};
pub use executor::{IdempotencySupport, ToolCancelFuture, ToolExecutionFuture, ToolExecutor};
pub use invocation::{ToolInvocation, ToolInvocationError, ToolInvocationPosition};
pub use policy::{AllowAllToolPolicy, PolicyDecision, ToolPolicy, ToolPolicyInput};
pub use registry::{ToolRegistration, ToolRegistry, ToolRegistryError};
pub use validation::{ToolArgumentValidationError, ToolArgumentValidator};
