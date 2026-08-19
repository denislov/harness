mod actor;
mod bootstrap;
mod command;
mod error;
mod event_source;
mod handle;
mod llm_operation;
mod llm_runtime;
mod loop_driver;
mod recovery;
mod runtime;
mod state;

#[cfg(test)]
mod driver_tests;
#[cfg(test)]
mod llm_tests;

pub use actor::{AgentActor, AgentExit, AgentExitReason};
pub use bootstrap::{AgentBootstrap, AgentBootstrapError, AgentBootstrapper};
pub use command::{AgentCommand, AgentCommandAck, SendReceipt};
pub use error::{AgentError, AgentHandleError};
pub use event_source::AgentEventSource;
pub use handle::AgentHandle;
pub(crate) use handle::MailboxMessage;
pub(crate) use llm_operation::LlmCompletion;
pub use llm_runtime::{AgentLlmRuntime, AgentLlmRuntimeError};
pub use recovery::{
    DurableCursor, RecoveryAnalysisError, RecoveryAnalyzer, RecoveryBlockProposal, ResumeDecision,
    ToolRecoveryAction, ToolRetryRequirement,
};
pub use runtime::{
    AgentActorConfig, AgentJoinError, AgentSpawnError, AgentTask, SpawnedAgent, spawn_agent,
    spawn_agent_with_llm,
};
pub use state::{ActiveAgentOperation, AgentDriverBoundary, AgentPhase, AgentState, ExecutionGate};
