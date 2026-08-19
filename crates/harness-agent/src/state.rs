use harness_session::{RecoveryBlock, SessionProjection, StepPosition};
use harness_types::{
    AgentInstanceId, EventSeq, InvocationId, RequestId, SessionId, StepNo, ToolCallId, TurnNo,
};

use crate::{AgentBootstrap, ResumeDecision};

/// Live process-local phase. It describes driver ownership, not unfinished durable
/// lifecycle state. Durable interruption is represented by `ResumeDecision`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentPhase {
    Idle { last_turn: Option<TurnNo> },
    Running { turn: TurnNo, step: Option<StepNo> },
    Maintenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentDriverBoundary {
    ReadyForModel { position: StepPosition },
    ReadyForTools { position: StepPosition },
}

/// Process-local external operation owned by the live actor.
///
/// This state is deliberately not durable. If the process disappears, durable
/// `model/requested` / `tool/dispatched` facts are interpreted by
/// `RecoveryAnalyzer` instead.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ActiveAgentOperation {
    Model {
        position: StepPosition,
        request_id: RequestId,
        attempt: u32,
    },
    Tool {
        position: StepPosition,
        call_id: ToolCallId,
        invocation_id: InvocationId,
        attempt: u32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecutionGate {
    Open,
    Blocked(RecoveryBlock),
}

impl ExecutionGate {
    pub const fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentState {
    pub instance_id: AgentInstanceId,
    pub session_id: SessionId,
    pub expected_seq: EventSeq,
    pub phase: AgentPhase,
    pub gate: ExecutionGate,
    pub resume: ResumeDecision,
    pub projection: SessionProjection,

    /// Process-local operation currently crossing an external capability seam.
    pub active_operation: Option<ActiveAgentOperation>,

    /// Process-local wake latch derived from durable pending Inbox items.
    pub wake_requested: bool,
}

impl AgentState {
    pub fn from_bootstrap(instance_id: AgentInstanceId, bootstrap: AgentBootstrap) -> Self {
        let wake_requested = projection_has_pending_wakeup(&bootstrap.projection);
        let gate = gate_from_resume(&bootstrap.resume);
        let phase = AgentPhase::Idle {
            last_turn: bootstrap.projection.lifecycle.last_ended_turn,
        };

        Self {
            instance_id,
            session_id: bootstrap.head.session_id,
            expected_seq: bootstrap.head.seq,
            phase,
            gate,
            resume: bootstrap.resume,
            projection: bootstrap.projection,
            active_operation: None,
            wake_requested,
        }
    }

    pub(crate) fn replace_durable_view(
        &mut self,
        expected_seq: EventSeq,
        projection: SessionProjection,
        resume: ResumeDecision,
    ) {
        let wake_requested = projection_has_pending_wakeup(&projection);
        self.expected_seq = expected_seq;
        self.gate = gate_from_resume(&resume);
        self.resume = resume;
        self.projection = projection;
        self.wake_requested = wake_requested;
    }

    pub const fn can_start_new_turn(&self) -> bool {
        matches!(&self.phase, AgentPhase::Idle { .. })
            && self.gate.is_open()
            && self.resume.is_clean()
            && self.active_operation.is_none()
    }

    pub const fn needs_resume_work(&self) -> bool {
        !self.resume.is_clean()
    }

    pub const fn has_active_operation(&self) -> bool {
        self.active_operation.is_some()
    }

    /// Returns the external-operation boundary at which the deterministic actor
    /// driver is currently parked.
    pub fn driver_boundary(&self) -> Option<AgentDriverBoundary> {
        if self.active_operation.is_some() {
            return None;
        }

        let AgentPhase::Running {
            turn,
            step: Some(step),
        } = &self.phase
        else {
            return None;
        };

        let live_position = StepPosition {
            turn: *turn,
            step: *step,
        };

        match &self.resume {
            ResumeDecision::ContinueOpenStep { position } if *position == live_position => {
                if self.projection.open_step_assistant_message.is_none() {
                    Some(AgentDriverBoundary::ReadyForModel {
                        position: *position,
                    })
                } else if !self.projection.open_step_tools.announced.is_empty() {
                    Some(AgentDriverBoundary::ReadyForTools {
                        position: *position,
                    })
                } else {
                    None
                }
            }
            ResumeDecision::RecoverToolBatch { position, .. } if *position == live_position => {
                Some(AgentDriverBoundary::ReadyForTools {
                    position: *position,
                })
            }
            _ => None,
        }
    }
}

fn gate_from_resume(resume: &ResumeDecision) -> ExecutionGate {
    match resume {
        ResumeDecision::Blocked { block, .. } => ExecutionGate::Blocked(block.clone()),
        _ => ExecutionGate::Open,
    }
}

fn projection_has_pending_wakeup(projection: &SessionProjection) -> bool {
    projection
        .inbox
        .next_turn
        .iter()
        .chain(projection.inbox.next_step.iter())
        .any(|item| item.wakeup)
}

#[cfg(test)]
mod tests {
    use super::{ActiveAgentOperation, AgentDriverBoundary, AgentState};
    use crate::{AgentBootstrap, AgentPhase, ResumeDecision};
    use harness_session::{OpenStepToolCall, SessionHead, SessionProjection, StepPosition};
    use harness_types::{EventSeq, JsonText, RequestId, StepNo, ToolCallId, TurnNo};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn bootstrap(resume: ResumeDecision) -> AgentBootstrap {
        AgentBootstrap {
            events: Vec::new(),
            head: SessionHead {
                session_id: id("ses_1"),
                seq: EventSeq::FIRST,
            },
            projection: SessionProjection::default(),
            resume,
        }
    }

    #[test]
    fn clean_bootstrap_may_start_new_turn() {
        let state = AgentState::from_bootstrap(id("agt_1"), bootstrap(ResumeDecision::Clean));
        assert!(state.can_start_new_turn());
        assert!(!state.wake_requested);
    }

    #[test]
    fn running_continue_open_step_exposes_ready_for_model_boundary() {
        let position = StepPosition {
            turn: TurnNo::FIRST,
            step: StepNo::FIRST,
        };
        let mut state = AgentState::from_bootstrap(
            id("agt_1"),
            bootstrap(ResumeDecision::ContinueOpenStep { position }),
        );
        state.phase = AgentPhase::Running {
            turn: position.turn,
            step: Some(position.step),
        };

        assert_eq!(
            state.driver_boundary(),
            Some(AgentDriverBoundary::ReadyForModel { position })
        );
    }

    #[test]
    fn authoritative_tool_call_assistant_exposes_tool_boundary() {
        let position = StepPosition {
            turn: TurnNo::FIRST,
            step: StepNo::FIRST,
        };
        let mut bootstrap = bootstrap(ResumeDecision::ContinueOpenStep { position });
        bootstrap.projection.lifecycle.open_turn = Some(position.turn);
        bootstrap.projection.lifecycle.open_step = Some(position);
        bootstrap.projection.open_step_assistant_message = Some(id("msg_assistant"));
        bootstrap
            .projection
            .open_step_tools
            .announced
            .push(OpenStepToolCall {
                call_id: ToolCallId::new("call_1").unwrap(),
                name: "read_file".to_owned(),
                arguments_json: JsonText::new("{}".to_owned()).unwrap(),
            });
        let mut state = AgentState::from_bootstrap(id("agt_1"), bootstrap);
        state.phase = AgentPhase::Running {
            turn: position.turn,
            step: Some(position.step),
        };

        assert_eq!(
            state.driver_boundary(),
            Some(AgentDriverBoundary::ReadyForTools { position })
        );
    }

    #[test]
    fn active_model_operation_hides_boundary() {
        let position = StepPosition {
            turn: TurnNo::FIRST,
            step: StepNo::FIRST,
        };
        let mut state = AgentState::from_bootstrap(
            id("agt_1"),
            bootstrap(ResumeDecision::ContinueOpenStep { position }),
        );
        state.phase = AgentPhase::Running {
            turn: position.turn,
            step: Some(position.step),
        };
        state.active_operation = Some(ActiveAgentOperation::Model {
            position,
            request_id: RequestId::new("req_1").unwrap(),
            attempt: 1,
        });

        assert!(state.driver_boundary().is_none());
    }
}
