use harness_session::{PendingInboxItem, StepPosition};
use harness_types::{InboxTarget, StepNo, TurnNo};

use crate::{AgentDriverBoundary, AgentError, AgentState, ResumeDecision};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PlannedInboxInput {
    pub target: InboxTarget,
    pub item: PendingInboxItem,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DriverPlan {
    Dormant,
    Deferred,
    StartNewTurn {
        turn: TurnNo,
        step: StepNo,
        inputs: Vec<PlannedInboxInput>,
    },
    StartStep {
        turn: TurnNo,
        step: StepNo,
        inputs: Vec<PlannedInboxInput>,
    },
    EnterCurrentStep {
        position: StepPosition,
        inputs: Vec<PlannedInboxInput>,
    },
    EndOpenTurn {
        turn: TurnNo,
    },
    Park(AgentDriverBoundary),
}

pub(crate) fn plan_next(state: &AgentState) -> Result<DriverPlan, AgentError> {
    match &state.resume {
        ResumeDecision::Clean => plan_clean(state),
        ResumeDecision::ContinueOpenTurn { turn } => plan_open_turn(state, *turn),
        ResumeDecision::ContinueOpenStep { position } => plan_open_step(state, *position),
        ResumeDecision::RecoverToolBatch { .. } | ResumeDecision::Blocked { .. } => {
            Ok(DriverPlan::Deferred)
        }
        ResumeDecision::RecoverInterruptedModelRequest { .. }
        | ResumeDecision::PersistRecoveryBlock { .. } => Err(AgentError::InvalidDurableMutation {
            message: "startup recovery was not converged before entering the deterministic driver"
                .to_owned(),
        }),
    }
}

fn plan_clean(state: &AgentState) -> Result<DriverPlan, AgentError> {
    if !state.wake_requested {
        return Ok(DriverPlan::Dormant);
    }

    let inputs = initial_step_inputs(state);
    if inputs.is_empty() {
        return Err(AgentError::InvalidDurableMutation {
            message: "wake_requested is true but the durable Inbox projection contains no work"
                .to_owned(),
        });
    }

    let turn = next_turn_number(state)?;
    Ok(DriverPlan::StartNewTurn {
        turn,
        step: StepNo::FIRST,
        inputs,
    })
}

fn plan_open_turn(state: &AgentState, turn: TurnNo) -> Result<DriverPlan, AgentError> {
    let turn_has_started_step = state
        .projection
        .lifecycle
        .last_started_step
        .is_some_and(|position| position.turn == turn);

    let inputs = if turn_has_started_step {
        next_step_inputs(state)
    } else {
        initial_step_inputs(state)
    };

    if inputs.is_empty() {
        return Ok(DriverPlan::EndOpenTurn { turn });
    }

    let step = next_step_number(state, turn)?;
    Ok(DriverPlan::StartStep { turn, step, inputs })
}

fn plan_open_step(state: &AgentState, position: StepPosition) -> Result<DriverPlan, AgentError> {
    if state.projection.open_step_assistant_message.is_some() {
        // The model has already produced the authoritative assistant message for
        // this step. Batch 06 must not reinterpret this state as a second model
        // request. Post-assistant step/turn convergence is introduced next.
        return Ok(DriverPlan::Deferred);
    }

    let inputs = next_step_inputs(state);
    if inputs.is_empty() {
        return Ok(DriverPlan::Park(AgentDriverBoundary::ReadyForModel {
            position,
        }));
    }

    Ok(DriverPlan::EnterCurrentStep { position, inputs })
}

fn initial_step_inputs(state: &AgentState) -> Vec<PlannedInboxInput> {
    let primary_capacity = if state.projection.inbox.next_turn.is_empty() {
        0
    } else {
        1
    };
    let mut inputs = Vec::with_capacity(primary_capacity + state.projection.inbox.next_step.len());

    if let Some(item) = state.projection.inbox.next_turn.front() {
        inputs.push(PlannedInboxInput {
            target: InboxTarget::NextTurn,
            item: item.clone(),
        });
    }

    inputs.extend(next_step_inputs(state));
    inputs
}

fn next_step_inputs(state: &AgentState) -> Vec<PlannedInboxInput> {
    state
        .projection
        .inbox
        .next_step
        .iter()
        .cloned()
        .map(|item| PlannedInboxInput {
            target: InboxTarget::NextStep,
            item,
        })
        .collect()
}

fn next_turn_number(state: &AgentState) -> Result<TurnNo, AgentError> {
    match state.projection.lifecycle.last_started_turn {
        Some(turn) => turn
            .checked_next()
            .map_err(|error| AgentError::InvalidDurableMutation {
                message: format!("cannot allocate next TurnNo: {error}"),
            }),
        None => Ok(TurnNo::FIRST),
    }
}

fn next_step_number(state: &AgentState, turn: TurnNo) -> Result<StepNo, AgentError> {
    match state.projection.lifecycle.last_started_step {
        Some(position) if position.turn == turn => {
            position
                .step
                .checked_next()
                .map_err(|error| AgentError::InvalidDurableMutation {
                    message: format!("cannot allocate next StepNo: {error}"),
                })
        }
        _ => Ok(StepNo::FIRST),
    }
}

#[cfg(test)]
mod tests {
    use harness_session::{PendingInboxItem, SessionProjection, StepPosition};
    use harness_types::{
        AgentInstanceId, ContentBlock, EventSeq, InboxTarget, Message, MessageId, MessageSource,
        Role, SessionId, StepNo, TurnNo,
    };

    use super::{DriverPlan, plan_next};
    use crate::{AgentPhase, AgentState, ExecutionGate, ResumeDecision};

    fn message(id_value: &str) -> Message {
        Message {
            id: MessageId::new(id_value).unwrap(),
            role: Role::User,
            source: MessageSource::user(),
            content: vec![ContentBlock::text(id_value)],
        }
    }

    fn state(projection: SessionProjection, resume: ResumeDecision, wake: bool) -> AgentState {
        AgentState {
            instance_id: AgentInstanceId::new("agt_1").unwrap(),
            session_id: SessionId::new("ses_1").unwrap(),
            expected_seq: EventSeq::FIRST,
            phase: AgentPhase::Idle { last_turn: None },
            gate: ExecutionGate::Open,
            resume,
            projection,
            active_operation: None,
            wake_requested: wake,
        }
    }

    #[test]
    fn clean_wake_claims_one_next_turn_and_all_next_step_inputs() {
        let mut projection = SessionProjection::default();
        projection.inbox.next_turn.push_back(PendingInboxItem {
            message: message("msg_turn_1"),
            target: InboxTarget::NextTurn,
            wakeup: true,
        });
        projection.inbox.next_turn.push_back(PendingInboxItem {
            message: message("msg_turn_2"),
            target: InboxTarget::NextTurn,
            wakeup: true,
        });
        projection.inbox.next_step.push_back(PendingInboxItem {
            message: message("msg_step_1"),
            target: InboxTarget::NextStep,
            wakeup: false,
        });

        let plan = plan_next(&state(projection, ResumeDecision::Clean, true)).unwrap();
        let DriverPlan::StartNewTurn { turn, step, inputs } = plan else {
            panic!("expected StartNewTurn");
        };

        assert_eq!(turn, TurnNo::FIRST);
        assert_eq!(step, StepNo::FIRST);
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].item.message.id.as_str(), "msg_turn_1");
        assert_eq!(inputs[1].item.message.id.as_str(), "msg_step_1");
    }

    #[test]
    fn open_step_parks_when_no_next_step_input_exists() {
        let position = StepPosition {
            turn: TurnNo::FIRST,
            step: StepNo::FIRST,
        };
        let mut projection = SessionProjection::default();
        projection.lifecycle.open_turn = Some(position.turn);
        projection.lifecycle.open_step = Some(position);
        projection.lifecycle.last_started_turn = Some(position.turn);
        projection.lifecycle.last_started_step = Some(position);

        let plan = plan_next(&state(
            projection,
            ResumeDecision::ContinueOpenStep { position },
            false,
        ))
        .unwrap();

        assert!(matches!(plan, DriverPlan::Park(_)));
    }
    #[test]
    fn open_step_with_authoritative_assistant_is_deferred_not_ready_for_model() {
        let position = StepPosition {
            turn: TurnNo::FIRST,
            step: StepNo::FIRST,
        };
        let mut projection = SessionProjection::default();
        projection.lifecycle.open_turn = Some(position.turn);
        projection.lifecycle.open_step = Some(position);
        projection.lifecycle.last_started_turn = Some(position.turn);
        projection.lifecycle.last_started_step = Some(position);
        projection.open_step_assistant_message = Some(MessageId::new("msg_assistant").unwrap());

        let plan = plan_next(&state(
            projection,
            ResumeDecision::ContinueOpenStep { position },
            false,
        ))
        .unwrap();

        assert!(matches!(plan, DriverPlan::Deferred));
    }
}
