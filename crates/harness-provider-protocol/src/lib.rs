//! Language-neutral Provider Protocol v1 wire types.
//!
//! This crate deliberately does not depend on Harness domain crates. The wire
//! contract must remain implementable by Python, TypeScript, Go, Rust, or any
//! other runtime that can speak JSON-RPC 2.0 over UTF-8 NDJSON.

mod codec;
mod common;
mod jsonrpc;
mod lifecycle;
mod llm;
mod manifest;
mod tool;

pub use codec::{ProtocolCodecError, decode_inbound_line, encode_ndjson};
pub use common::{
    CommonWireValidationError, WireBlobRef, WireCancelCause, WireCancelCauseKind, WireContentBlock,
    WireErrorCode, WireMessage, WireMessageSource, WirePortableError, WireRole, WireTokenUsage,
};
pub use jsonrpc::{
    InboundMessage, RpcErrorObject, RpcErrorResponse, RpcId, RpcIdError, RpcNotification,
    RpcNotificationEnvelope, RpcRequest, RpcRequestEnvelope, RpcResponseEnvelope,
    RpcResponseOutcome, RpcSuccessResponse,
};
pub use lifecycle::{
    CapabilityCancelParams, InitializeParams, PingParams, PingResult, RuntimeInfo, ShutdownParams,
    ShutdownResult,
};
pub use llm::{
    LlmEventParams, LlmStartParams, LlmStartResult, LlmWireValidationError, WireBlockType,
    WireFinishReason, WireLlmStreamEvent, WireModelOptions, WireModelRequest, WireModelToolSpec,
};
pub use manifest::{
    CapabilityDescriptor, ManifestValidationError, ProviderManifest, WireSideEffectClass,
};
pub use tool::{
    ProviderToolOutcome, ToolInvokeParams, ToolInvokeResult, ToolInvokeValidationError,
};

pub const JSONRPC_VERSION: &str = "2.0";
pub const PROTOCOL_VERSION: &str = "1.0";
pub const MAX_JSON_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub const METHOD_PROVIDER_INITIALIZE: &str = "provider.initialize";
pub const METHOD_PROVIDER_PING: &str = "provider.ping";
pub const METHOD_PROVIDER_SHUTDOWN: &str = "provider.shutdown";
pub const METHOD_TOOL_INVOKE: &str = "tool.invoke";
pub const METHOD_LLM_START: &str = "llm.start";
pub const METHOD_LLM_EVENT: &str = "llm.event";
pub const METHOD_CAPABILITY_CANCEL: &str = "capability.cancel";
