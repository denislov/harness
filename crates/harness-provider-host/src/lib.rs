//! Out-of-process Provider Protocol host transport.
//!
//! Batch 10 owns subprocess lifecycle, JSON-RPC request correlation, NDJSON
//! framing, and LLM stream demultiplexing. Domain adapters implementing
//! `harness_llm::LlmProvider` and `harness_tools::ToolExecutor` are deliberately
//! deferred so transport correctness can be validated independently.

mod host;

pub use host::{
    LlmStreamHandle, LlmStreamItem, ProviderHost, ProviderHostConfig, ProviderHostError,
    ProviderState, ProviderStreamError,
};
