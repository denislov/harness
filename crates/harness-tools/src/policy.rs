use harness_types::{JsonText, SessionId, SideEffectClass, StepNo, ToolCallId, TurnNo};

#[derive(Clone, Debug, PartialEq)]
pub struct ToolPolicyInput {
    pub session_id: SessionId,
    pub turn: TurnNo,
    pub step: StepNo,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub arguments_json: JsonText,
    pub side_effect: SideEffectClass,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    Ask { reason: String, risk: String },
}

pub trait ToolPolicy: Send + Sync {
    fn evaluate(&self, input: &ToolPolicyInput) -> PolicyDecision;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAllToolPolicy;

impl ToolPolicy for AllowAllToolPolicy {
    fn evaluate(&self, _input: &ToolPolicyInput) -> PolicyDecision {
        PolicyDecision::Allow
    }
}
