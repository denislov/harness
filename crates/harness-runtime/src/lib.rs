//! Process-level composition root for the language-agnostic Harness.
//!
//! Batch 14 turns this crate from a scaffold into the owner of static Provider
//! and Agent-profile composition plus dynamic Agent lifecycle. Durable Session
//! truth and Agent state-machine decisions remain in their lower-level crates.

mod agent_registry;
mod builder;
mod config;
mod error;
mod identity;
mod llm_registry;
mod profile;
mod provider_registry;
mod runtime;

pub use agent_registry::AgentRegistry;
pub use builder::HarnessRuntimeBuilder;
pub use config::{HarnessRuntimeInfo, ProviderProcessSpec};
pub use error::{HarnessRuntimeBuildError, HarnessRuntimeError};
pub use identity::RuntimeIdSource;
pub use llm_registry::LlmRegistry;
pub use profile::{AgentProfile, ModelBinding, ProfileRegistry, RuntimeToolBinding};
pub use provider_registry::ProviderRegistry;
pub use runtime::{HarnessRuntime, HarnessRuntimeState};
