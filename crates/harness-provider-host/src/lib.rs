//! Out-of-process Provider Protocol host transport and Harness domain adapters.
//!
//! The subprocess transport remains isolated in [`ProviderHost`]. Batch 11 adds
//! adapters that implement the provider-neutral `harness-llm` and `harness-tools`
//! seams without leaking Provider Protocol wire types into Agent Core.

mod adapter;
mod host;
mod slot;

pub use adapter::{ProviderAdapterError, ProviderHostLlmAdapter, ProviderHostToolAdapter};
pub use host::{
    LlmStreamHandle, LlmStreamItem, ProviderHost, ProviderHostConfig, ProviderHostError,
    ProviderState, ProviderStreamError,
};

pub use slot::{
    ProviderGeneration, ProviderSlot, ProviderSlotError, ProviderSlotLlmAdapter,
    ProviderSlotStatus, ProviderSlotToolAdapter,
};
