//! Internal conformance infrastructure for crash/fault-injection testing.
//!
//! This crate is a workspace-only test harness. It deliberately does not own
//! production Agent semantics; it supplies deterministic fixtures around the
//! public Core contracts so crash boundaries can be exercised repeatably.

#![forbid(unsafe_code)]

mod fault_store;
mod fixture;

pub use fault_store::{AppendFault, FaultInjectingSessionStore, ObservedAppend};
pub use fixture::{
    ObjectValidator, ScriptedLlm, TestEventSource, build_tool_runtime, create_session,
    model_config, read_all, text_script, tool_call_script, tool_definition, user_message,
    wait_for_pending_approval, wait_for_quiescent,
};
