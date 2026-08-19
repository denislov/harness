use harness_types::{
    IdempotencyKey, InvocationId, JsonText, SessionId, StepNo, ToolCallId, TurnNo,
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolInvocationPosition {
    pub turn: TurnNo,
    pub step: StepNo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolInvocation {
    pub invocation_id: InvocationId,
    pub call_id: ToolCallId,
    pub session_id: SessionId,
    pub position: ToolInvocationPosition,
    pub tool_name: String,
    pub arguments_json: JsonText,
    pub attempt: u32,
    pub idempotency_key: IdempotencyKey,
}

impl ToolInvocation {
    pub fn validate(&self) -> Result<(), ToolInvocationError> {
        if self.tool_name.is_empty() {
            return Err(ToolInvocationError::EmptyToolName);
        }
        if self.attempt == 0 {
            return Err(ToolInvocationError::ZeroAttempt);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ToolInvocationError {
    #[error("tool invocation name must not be empty")]
    EmptyToolName,

    #[error("tool invocation attempt must be greater than zero")]
    ZeroAttempt,
}
