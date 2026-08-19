//! Provider-neutral LLM domain types and streaming contracts.
//!
//! This crate deliberately contains no Provider Host process-management logic
//! and no Tokio dependency. Concrete in-process or out-of-process adapters only
//! need to implement [`LlmProvider`] and emit the normalized stream vocabulary.

mod assembler;
mod provider;
mod request;
mod stream;

pub use assembler::{LlmStreamAssembler, LlmStreamOutcome, StreamAssemblyError};
pub use provider::{LlmEventStream, LlmProvider};
pub use request::{
    ModelOptions, ModelRequest, ModelRequestConfig, ModelRequestError, ModelSnapshotError,
    ModelToolSpec,
};
pub use stream::{BlockType, FinishEvent, FinishReason, SequencedStreamEvent, StreamEvent};
