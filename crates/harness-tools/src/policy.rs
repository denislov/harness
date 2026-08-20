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

    /// Stable identity included in durable execution-composition snapshots.
    ///
    /// The default preserves source compatibility by identifying the concrete
    /// Rust type. Policies whose behavior depends on runtime configuration
    /// should override this with a stable semantic version/config identity.
    fn composition_identity(&self) -> String {
        format!("rust-type:{}", std::any::type_name::<Self>())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAllToolPolicy;

impl ToolPolicy for AllowAllToolPolicy {
    fn evaluate(&self, _input: &ToolPolicyInput) -> PolicyDecision {
        PolicyDecision::Allow
    }

    fn composition_identity(&self) -> String {
        "harness-tools/allow-all/v1".to_owned()
    }
}
