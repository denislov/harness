//! Process-level composition root for the language-agnostic Harness.
//!
//! `HarnessRuntime` owns static capability composition plus process-local Agent,
//! Provider, credential and observability lifecycles. Durable Session truth and
//! Agent state-machine decisions remain in their lower-level crates.

mod agent_registry;
mod builder;
mod composition;
mod config;
mod credential;
mod error;
mod identity;
mod llm_registry;
mod profile;
mod provider_registry;
mod runtime;
mod runtime_event;

pub use agent_registry::AgentRegistry;
pub use builder::HarnessRuntimeBuilder;
pub use composition::{
    EXECUTION_COMPOSITION_MEDIA_TYPE, EXECUTION_COMPOSITION_SCHEMA_VERSION,
    ExecutionCompositionSnapshot, ExecutionModelComposition, ExecutionToolComposition,
};
pub use config::{
    HarnessRuntimeInfo, ProviderProcessSpec, ProviderSupervisorConfig,
    ProviderSupervisorConfigError,
};
pub use credential::{
    CredentialKey, CredentialKeyError, CredentialResolveError, CredentialResolver,
    RejectingCredentialResolver, SecretValue,
};
pub use error::{HarnessRuntimeBuildError, HarnessRuntimeError};
pub use harness_provider_host::ProviderSlotStatus;
pub use identity::RuntimeIdSource;
pub use llm_registry::LlmRegistry;
pub use profile::{AgentProfile, ModelBinding, ProfileRegistry, RuntimeToolBinding};
pub use provider_registry::ProviderRegistry;
pub use runtime::{HarnessRuntime, HarnessRuntimeState};
pub use runtime_event::{
    DEFAULT_RUNTIME_EVENT_CAPACITY, ProviderQuarantineReason, RUNTIME_EVENT_SCHEMA_VERSION,
    RuntimeBuildStage, RuntimeEvent, RuntimeEventBus, RuntimeEventBusError, RuntimeEventKind,
};
