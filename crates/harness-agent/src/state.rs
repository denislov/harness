use harness_session::{RecoveryBlock, SessionProjection};
use harness_types::{AgentInstanceId, EventSeq, SessionId, StepNo, TurnNo};

use crate::{AgentBootstrap, ResumeDecision};

/// Live process-local phase. It describes driver ownership, not unfinished durable
/// lifecycle state. Durable interruption is represented by `ResumeDecision`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentPhase {
    Idle { last_turn: Option<TurnNo> },
    Running { turn: TurnNo, step: Option<StepNo> },
    Maintenance,
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
}

impl AgentState {
    pub fn from_bootstrap(instance_id: AgentInstanceId, bootstrap: AgentBootstrap) -> Self {
        let gate = match &bootstrap.resume {
            ResumeDecision::Blocked { block, .. } => ExecutionGate::Blocked(block.clone()),
            _ => ExecutionGate::Open,
        };
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
        }
    }

    /// New turn creation is permitted only after all interrupted durable work has
    /// converged and the durable recovery gate is open.
    pub const fn can_start_new_turn(&self) -> bool {
        matches!(&self.phase, AgentPhase::Idle { .. })
            && self.gate.is_open()
            && self.resume.is_clean()
    }

    pub const fn needs_resume_work(&self) -> bool {
        !self.resume.is_clean()
    }
}

#[cfg(test)]
mod tests {
    use super::AgentState;
    use crate::{AgentBootstrap, ResumeDecision};
    use harness_session::{SessionHead, SessionProjection};
    use harness_types::{EventSeq, TurnNo};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn bootstrap(resume: ResumeDecision) -> AgentBootstrap {
        AgentBootstrap {
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
    }

    #[test]
    fn unfinished_turn_blocks_new_turn_even_with_open_execution_gate() {
        let state = AgentState::from_bootstrap(
            id("agt_1"),
            bootstrap(ResumeDecision::ContinueOpenTurn {
                turn: TurnNo::new(1).unwrap(),
            }),
        );
        assert!(state.gate.is_open());
        assert!(!state.can_start_new_turn());
        assert!(state.needs_resume_work());
    }
}
