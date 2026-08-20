use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CliError {
    #[error("Harness configuration failed: {0}")]
    Config(#[source] Box<harness_config::HarnessConfigError>),

    #[error("failed to compose Harness runtime: {0}")]
    RuntimeBuild(#[source] Box<harness_runtime::HarnessRuntimeBuildError>),

    #[error("Harness runtime operation failed: {0}")]
    Runtime(#[source] Box<harness_runtime::HarnessRuntimeError>),

    #[error("failed to open durable local storage: {0}")]
    Storage(#[source] Box<harness_storage_local::DurableLocalStorageError>),

    #[error("SessionStore operation failed: {0}")]
    Session(#[source] Box<harness_session::SessionStoreError>),

    #[error("Agent handle operation failed: {0}")]
    Agent(#[source] Box<harness_agent::AgentHandleError>),

    #[error("invalid SessionId {value:?}: {message}")]
    InvalidSessionId { value: String, message: String },

    #[error("no profile was supplied and runtime.default_profile is not configured")]
    MissingProfile,

    #[error("Agent profile {0:?} is not configured")]
    ProfileNotFound(String),

    #[error("workspace {0:?} is not configured")]
    WorkspaceNotFound(String),

    #[error("failed to serialize resolved scope: {0}")]
    ScopeSerialize(#[source] Box<serde_json::Error>),

    #[error("session is waiting for an approval; CLI run cannot continue interactively")]
    ApprovalPending,

    #[error("session execution is recovery-blocked: {0}")]
    RecoveryBlocked(String),

    #[error("event sequence cannot advance while reading Session history: {0}")]
    EventSequence(String),

    #[error("runtime event observer lagged and skipped {skipped} event(s)")]
    ObservabilityLagged { skipped: u64 },

    #[error("runtime event observer task failed: {0}")]
    ObservabilityTask(#[source] Box<tokio::task::JoinError>),

    #[error("failed to serialize RuntimeEvent for {path}: {source}")]
    RuntimeEventSerialize {
        path: PathBuf,
        #[source]
        source: Box<serde_json::Error>,
    },

    #[error("I/O failure while {context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: Box<std::io::Error>,
    },

    #[error("failed to serialize SessionEvent: {0}")]
    Serialize(#[source] Box<serde_json::Error>),
}
