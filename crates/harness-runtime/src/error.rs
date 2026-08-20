use harness_agent::{AgentLlmRuntimeError, AgentSpawnError, AgentToolRuntimeError};
use harness_provider_host::{ProviderAdapterError, ProviderHostError};
use harness_session::SessionStoreError;
use harness_tools::ToolRegistryError;
use harness_types::{ProviderId, SessionId};
use thiserror::Error;

use crate::HarnessRuntimeState;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HarnessRuntimeBuildError {
    #[error("HarnessRuntimeBuilder requires a SessionStore")]
    MissingSessionStore,

    #[error("HarnessRuntimeBuilder requires a BlobStore")]
    MissingBlobStore,

    #[error("HarnessRuntimeBuilder requires an AgentEventSource")]
    MissingEventSource,

    #[error("HarnessRuntimeBuilder requires a RuntimeIdSource")]
    MissingIdSource,

    #[error("Harness runtime name/version must not be empty")]
    InvalidRuntimeInfo,

    #[error("provider {0} is configured more than once")]
    DuplicateProvider(ProviderId),

    #[error("Agent profile name must not be empty")]
    EmptyProfileName,

    #[error("Agent profile {0} is configured more than once")]
    DuplicateProfile(String),

    #[error("failed to start configured provider {expected}: {source}")]
    ProviderStart {
        expected: ProviderId,
        #[source]
        source: ProviderHostError,
    },

    #[error("provider {0} completed initialization without a manifest")]
    ProviderManifestMissing(ProviderId),

    #[error("provider manifest providerId {value:?} cannot be represented by Harness: {message}")]
    InvalidManifestProviderId { value: String, message: String },

    #[error("provider identity mismatch: configured {expected}, manifest declared {actual}")]
    ProviderIdentityMismatch {
        expected: ProviderId,
        actual: ProviderId,
    },

    #[error("failed to construct LLM adapter for provider {provider}: {source}")]
    LlmAdapter {
        provider: ProviderId,
        #[source]
        source: ProviderAdapterError,
    },

    #[error("Agent profile {profile} references unavailable provider {provider}")]
    ProfileProviderNotFound {
        profile: String,
        provider: ProviderId,
    },

    #[error(
        "Agent profile {profile} references model {model:?} not declared by provider {provider}"
    )]
    ProfileModelNotDeclared {
        profile: String,
        provider: ProviderId,
        model: String,
    },

    #[error("Agent profile {profile} has invalid actor configuration: {message}")]
    InvalidActorConfig { profile: String, message: String },

    #[error("Agent profile {profile} failed to bind Tool {tool}: {source}")]
    ToolAdapter {
        profile: String,
        tool: String,
        #[source]
        source: ProviderAdapterError,
    },

    #[error("Agent profile {profile} has an invalid Tool registry: {source}")]
    ToolRegistry {
        profile: String,
        #[source]
        source: ToolRegistryError,
    },

    #[error("Agent profile {profile} has an invalid LLM runtime: {source}")]
    LlmRuntime {
        profile: String,
        #[source]
        source: AgentLlmRuntimeError,
    },

    #[error("Agent profile {profile} has an invalid Tool runtime: {source}")]
    ToolRuntime {
        profile: String,
        #[source]
        source: AgentToolRuntimeError,
    },
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HarnessRuntimeError {
    #[error("HarnessRuntime is {actual:?}; this operation requires Running")]
    NotRunning { actual: HarnessRuntimeState },

    #[error("Agent profile {0:?} was not found")]
    ProfileNotFound(String),

    #[error("Session {0} already has an active or transitioning Agent")]
    AgentAlreadyActive(SessionId),

    #[error("Session {0} does not have a live Agent")]
    AgentNotActive(SessionId),

    #[error("Session {0} Agent is currently transitioning")]
    AgentTransitioning(SessionId),

    #[error("failed to spawn Agent for Session {session_id}: {source}")]
    AgentSpawn {
        session_id: SessionId,
        #[source]
        source: AgentSpawnError,
    },

    #[error("failed to close Agent for Session {session_id}: {failures:?}")]
    AgentCloseFailed {
        session_id: SessionId,
        failures: Vec<String>,
    },

    #[error("HarnessRuntime shutdown completed with failures: {failures:?}")]
    ShutdownFailed { failures: Vec<String> },

    #[error(transparent)]
    SessionStore(#[from] SessionStoreError),
}
