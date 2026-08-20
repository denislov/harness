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
mod tool_driver;
mod tool_operation;
mod tool_runtime;

#[cfg(test)]
mod driver_tests;
#[cfg(test)]
mod llm_tests;
#[cfg(test)]
mod tool_tests;

pub use actor::{AgentActor, AgentExit, AgentExitReason};
pub use bootstrap::{AgentBootstrap, AgentBootstrapError, AgentBootstrapper};
pub use command::{AgentCommand, AgentCommandAck, ApprovalReceipt, SendReceipt};
pub use error::{AgentError, AgentHandleError};
pub use event_source::AgentEventSource;
pub use handle::AgentHandle;
pub(crate) use handle::MailboxMessage;
pub(crate) use llm_operation::LlmCompletion;
pub use llm_runtime::{AgentLlmRuntime, AgentLlmRuntimeError, DEFAULT_LLM_TIMEOUT_MS};
pub use recovery::{
    DurableCursor, RecoveryAnalysisError, RecoveryAnalyzer, RecoveryBlockProposal, ResumeDecision,
    ToolRecoveryAction, ToolRetryRequirement,
};
pub use runtime::{
    AgentActorConfig, AgentJoinError, AgentSpawnError, AgentTask, SpawnedAgent, spawn_agent,
    spawn_agent_with_capabilities, spawn_agent_with_llm,
};
pub use state::{ActiveAgentOperation, AgentDriverBoundary, AgentPhase, AgentState, ExecutionGate};
pub(crate) use tool_operation::ToolCompletion;
pub use tool_runtime::{AgentToolRuntime, AgentToolRuntimeError};
