use harness_session::{
    ModelRequested, PendingApproval, PendingToolCall, PendingToolDispatch, RecoveryBlock,
    SessionProjection, StepPosition,
};
use harness_types::{SideEffectClass, ToolCallId, TurnNo};
use thiserror::Error;

/// Durable lifecycle position found while bootstrapping an Agent.
///
/// This is deliberately separate from `AgentPhase`: after process restart no live
/// driver exists yet, even if the durable log contains an unfinished turn/step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurableCursor {
    Quiescent,
    OpenTurn { turn: TurnNo },
    OpenStep { position: StepPosition },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRetryRequirement {
    None,
    ProviderIdempotencyGuarantee,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolRecoveryAction {
    /// `tool/call` is durable, but no provider-dispatch boundary was committed.
    /// The Core may restart the tool pipeline from before dispatch.
    StartUndispatched { call: PendingToolCall },

    /// A provider dispatch may have occurred. Retrying creates a new invocation
    /// attempt while preserving the logical ToolCall and idempotency key.
    RetryDispatched {
        call: PendingToolCall,
        previous_dispatch: PendingToolDispatch,
        next_attempt: u32,
        requirement: ToolRetryRequirement,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryBlockProposal {
    pub position: StepPosition,
    pub call: PendingToolCall,
    pub dispatch: PendingToolDispatch,
}

/// Decision produced from one structurally valid SessionProjection at process startup.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ResumeDecision {
    /// No durable activity is incomplete. A new turn may be started when Inbox work exists.
    Clean,

    /// A turn was durably opened but no step is currently open.
    ContinueOpenTurn { turn: TurnNo },

    /// A step is open with no unresolved external operation. The future driver must
    /// continue/finalize that step before opening a new turn.
    ContinueOpenStep { position: StepPosition },

    /// A durable Tool approval request is waiting for an explicit caller decision.
    AwaitingApproval {
        position: StepPosition,
        approval: PendingApproval,
    },

    /// `model/requested` was committed but neither assistant/message nor model/failed
    /// was committed. The future driver must first durably fail the interrupted attempt,
    /// then may create a new model attempt according to retry policy.
    RecoverInterruptedModelRequest {
        position: StepPosition,
        request: ModelRequested,
    },

    /// One or more logical tool calls remain incomplete and are safe or conditionally
    /// safe to resume according to their side-effect classification.
    RecoverToolBatch {
        position: StepPosition,
        actions: Vec<ToolRecoveryAction>,
    },

    /// A non-idempotent provider dispatch may have executed, but the process crashed
    /// before `recovery/blocked` became durable. Normal execution must not continue;
    /// the first recovery action is to persist the recovery block.
    PersistRecoveryBlock { proposal: RecoveryBlockProposal },

    /// A durable recovery block already exists. Recovery/administrative work may run,
    /// but normal Agent execution remains gated.
    Blocked {
        block: RecoveryBlock,
        cursor: DurableCursor,
    },
}

impl ResumeDecision {
    pub const fn is_clean(&self) -> bool {
        matches!(self, Self::Clean)
    }

    pub const fn blocks_new_turn(&self) -> bool {
        !self.is_clean()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecoveryAnalysisError {
    #[error("pending model request exists without an open step")]
    ModelRequestWithoutOpenStep,

    #[error("pending model request and pending tool calls coexist in one projection")]
    ModelAndToolWorkOverlap,

    #[error("pending approval exists without an open step")]
    ApprovalWithoutOpenStep,

    #[error("pending approval and pending model request coexist in one projection")]
    ApprovalAndModelOverlap,

    #[error("pending approval and unresolved tool dispatch coexist in one projection")]
    ApprovalAndDispatchOverlap,

    #[error("pending approval does not belong to the open step")]
    ApprovalOutsideOpenStep,

    #[error("pending approval references missing tool call {0}")]
    ApprovalWithoutTool(ToolCallId),

    #[error("pending tool work exists without an open step")]
    ToolWorkWithoutOpenStep,

    #[error("pending tool dispatch {0} has no matching pending tool call")]
    DispatchWithoutCall(ToolCallId),

    #[error("pending tool call {0} does not belong to the open step")]
    ToolCallOutsideOpenStep(ToolCallId),

    #[error("tool call {0} dispatch attempt cannot be incremented")]
    AttemptOverflow(ToolCallId),

    #[error("more than one non-idempotent-write dispatch is unresolved; v0.1 permits at most one")]
    MultipleUnknownNonIdempotentDispatches,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RecoveryAnalyzer;

impl RecoveryAnalyzer {
    pub fn analyze(
        &self,
        projection: &SessionProjection,
    ) -> Result<ResumeDecision, RecoveryAnalysisError> {
        let cursor = durable_cursor(projection);

        if let Some(block) = &projection.unresolved_recovery {
            return Ok(ResumeDecision::Blocked {
                block: block.clone(),
                cursor,
            });
        }

        if let Some(approval) = &projection.pending_approval {
            if projection.pending_model_request.is_some() {
                return Err(RecoveryAnalysisError::ApprovalAndModelOverlap);
            }
            if !projection.pending_tool_dispatches.is_empty() {
                return Err(RecoveryAnalysisError::ApprovalAndDispatchOverlap);
            }
            let position = projection
                .lifecycle
                .open_step
                .ok_or(RecoveryAnalysisError::ApprovalWithoutOpenStep)?;
            if approval.turn != position.turn || approval.step != position.step {
                return Err(RecoveryAnalysisError::ApprovalOutsideOpenStep);
            }
            if !projection
                .pending_tool_calls
                .contains_key(&approval.data.call_id)
            {
                return Err(RecoveryAnalysisError::ApprovalWithoutTool(
                    approval.data.call_id.clone(),
                ));
            }
            return Ok(ResumeDecision::AwaitingApproval {
                position,
                approval: approval.clone(),
            });
        }

        for call_id in projection.pending_tool_dispatches.keys() {
            if !projection.pending_tool_calls.contains_key(call_id) {
                return Err(RecoveryAnalysisError::DispatchWithoutCall(call_id.clone()));
            }
        }

        if let Some(request) = &projection.pending_model_request {
            if !projection.pending_tool_calls.is_empty() {
                return Err(RecoveryAnalysisError::ModelAndToolWorkOverlap);
            }
            let position = projection
                .lifecycle
                .open_step
                .ok_or(RecoveryAnalysisError::ModelRequestWithoutOpenStep)?;
            return Ok(ResumeDecision::RecoverInterruptedModelRequest {
                position,
                request: request.clone(),
            });
        }

        if !projection.pending_tool_calls.is_empty() {
            let position = projection
                .lifecycle
                .open_step
                .ok_or(RecoveryAnalysisError::ToolWorkWithoutOpenStep)?;

            let mut calls: Vec<_> = projection.pending_tool_calls.values().cloned().collect();
            calls.sort_by_key(|call| call.call_seq);

            for call in &calls {
                if call.turn != position.turn || call.step != position.step {
                    return Err(RecoveryAnalysisError::ToolCallOutsideOpenStep(
                        call.data.call_id.clone(),
                    ));
                }
            }

            let mut unknown_non_idempotent: Vec<(PendingToolCall, PendingToolDispatch)> = calls
                .iter()
                .filter_map(|call| {
                    let dispatch = projection
                        .pending_tool_dispatches
                        .get(&call.data.call_id)?
                        .clone();
                    (call.data.side_effect == SideEffectClass::NonIdempotentWrite)
                        .then(|| (call.clone(), dispatch))
                })
                .collect();

            if unknown_non_idempotent.len() > 1 {
                return Err(RecoveryAnalysisError::MultipleUnknownNonIdempotentDispatches);
            }
            if let Some((call, dispatch)) = unknown_non_idempotent.pop() {
                return Ok(ResumeDecision::PersistRecoveryBlock {
                    proposal: RecoveryBlockProposal {
                        position,
                        call,
                        dispatch,
                    },
                });
            }

            let mut actions = Vec::with_capacity(calls.len());
            for call in calls {
                let Some(dispatch) = projection
                    .pending_tool_dispatches
                    .get(&call.data.call_id)
                    .cloned()
                else {
                    actions.push(ToolRecoveryAction::StartUndispatched { call });
                    continue;
                };

                let next_attempt = dispatch.data.attempt.checked_add(1).ok_or_else(|| {
                    RecoveryAnalysisError::AttemptOverflow(call.data.call_id.clone())
                })?;
                let requirement = match call.data.side_effect {
                    SideEffectClass::ReadOnly => ToolRetryRequirement::None,
                    SideEffectClass::IdempotentWrite => {
                        ToolRetryRequirement::ProviderIdempotencyGuarantee
                    }
                    SideEffectClass::NonIdempotentWrite => unreachable!(
                        "dispatched non-idempotent write was handled before action construction"
                    ),
                };
                actions.push(ToolRecoveryAction::RetryDispatched {
                    call,
                    previous_dispatch: dispatch,
                    next_attempt,
                    requirement,
                });
            }

            return Ok(ResumeDecision::RecoverToolBatch { position, actions });
        }

        if !projection.pending_tool_dispatches.is_empty() {
            let call_id = projection
                .pending_tool_dispatches
                .keys()
                .next()
                .expect("map was checked non-empty")
                .clone();
            return Err(RecoveryAnalysisError::DispatchWithoutCall(call_id));
        }

        Ok(match cursor {
            DurableCursor::Quiescent => ResumeDecision::Clean,
            DurableCursor::OpenTurn { turn } => ResumeDecision::ContinueOpenTurn { turn },
            DurableCursor::OpenStep { position } => ResumeDecision::ContinueOpenStep { position },
        })
    }
}

fn durable_cursor(projection: &SessionProjection) -> DurableCursor {
    if let Some(position) = projection.lifecycle.open_step {
        DurableCursor::OpenStep { position }
    } else if let Some(turn) = projection.lifecycle.open_turn {
        DurableCursor::OpenTurn { turn }
    } else {
        DurableCursor::Quiescent
    }
}

#[cfg(test)]
mod tests {
    use super::{RecoveryAnalyzer, ResumeDecision, ToolRecoveryAction, ToolRetryRequirement};
    use harness_session::{
        PendingToolCall, PendingToolDispatch, SessionProjection, StepPosition, ToolCallRecorded,
        ToolDispatched,
    };
    use harness_types::{EventSeq, JsonText, SideEffectClass, StepNo, ToolCallId, TurnNo};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn seq(value: u64) -> EventSeq {
        EventSeq::new(value).unwrap()
    }

    fn position() -> StepPosition {
        StepPosition {
            turn: TurnNo::new(1).unwrap(),
            step: StepNo::new(1).unwrap(),
        }
    }

    fn pending_call(call_id: &str, call_seq: u64, side_effect: SideEffectClass) -> PendingToolCall {
        let pos = position();
        PendingToolCall {
            call_event_id: id(&format!("evt_call_{call_seq}")),
            call_seq: seq(call_seq),
            turn: pos.turn,
            step: pos.step,
            data: ToolCallRecorded {
                call_id: id(call_id),
                tool: "tool".to_owned(),
                arguments_json: JsonText::new("{}".to_owned()).unwrap(),
                side_effect,
            },
        }
    }

    fn pending_dispatch(call_id: &str, dispatch_seq: u64, attempt: u32) -> PendingToolDispatch {
        let pos = position();
        PendingToolDispatch {
            dispatch_event_id: id(&format!("evt_dispatch_{dispatch_seq}")),
            dispatch_seq: seq(dispatch_seq),
            turn: pos.turn,
            step: pos.step,
            data: ToolDispatched {
                call_id: id(call_id),
                invocation_id: id(&format!("inv_{call_id}_{attempt}")),
                provider_id: id("prv_tools"),
                attempt,
                idempotency_key: id(&format!("idem_{call_id}")),
            },
        }
    }

    fn open_step_projection() -> SessionProjection {
        let mut projection = SessionProjection::default();
        projection.lifecycle.open_turn = Some(position().turn);
        projection.lifecycle.open_step = Some(position());
        projection
    }

    #[test]
    fn clean_projection_is_ready_for_new_turn() {
        let decision = RecoveryAnalyzer
            .analyze(&SessionProjection::default())
            .unwrap();
        assert_eq!(decision, ResumeDecision::Clean);
    }

    #[test]
    fn undispatched_non_idempotent_call_is_safe_to_restart_before_dispatch() {
        let mut projection = open_step_projection();
        let call = pending_call("call_z", 10, SideEffectClass::NonIdempotentWrite);
        projection
            .pending_tool_calls
            .insert(call.data.call_id.clone(), call);

        let decision = RecoveryAnalyzer.analyze(&projection).unwrap();
        assert!(matches!(
            decision,
            ResumeDecision::RecoverToolBatch { actions, .. }
                if matches!(actions.as_slice(), [ToolRecoveryAction::StartUndispatched { .. }])
        ));
    }

    #[test]
    fn dispatched_read_only_call_is_retryable() {
        let mut projection = open_step_projection();
        let call = pending_call("call_read", 10, SideEffectClass::ReadOnly);
        let dispatch = pending_dispatch("call_read", 11, 1);
        projection
            .pending_tool_calls
            .insert(call.data.call_id.clone(), call);
        projection
            .pending_tool_dispatches
            .insert(dispatch.data.call_id.clone(), dispatch);

        let decision = RecoveryAnalyzer.analyze(&projection).unwrap();
        match decision {
            ResumeDecision::RecoverToolBatch { actions, .. } => match actions.as_slice() {
                [
                    ToolRecoveryAction::RetryDispatched {
                        next_attempt,
                        requirement,
                        ..
                    },
                ] => {
                    assert_eq!(*next_attempt, 2);
                    assert_eq!(*requirement, ToolRetryRequirement::None);
                }
                other => panic!("unexpected actions: {other:?}"),
            },
            other => panic!("unexpected decision: {other:?}"),
        }
    }

    #[test]
    fn dispatched_idempotent_write_requires_provider_guarantee() {
        let mut projection = open_step_projection();
        let call = pending_call("call_write", 10, SideEffectClass::IdempotentWrite);
        let dispatch = pending_dispatch("call_write", 11, 3);
        projection
            .pending_tool_calls
            .insert(call.data.call_id.clone(), call);
        projection
            .pending_tool_dispatches
            .insert(dispatch.data.call_id.clone(), dispatch);

        let decision = RecoveryAnalyzer.analyze(&projection).unwrap();
        assert!(matches!(
            decision,
            ResumeDecision::RecoverToolBatch { actions, .. }
                if matches!(
                    actions.as_slice(),
                    [ToolRecoveryAction::RetryDispatched {
                        next_attempt: 4,
                        requirement: ToolRetryRequirement::ProviderIdempotencyGuarantee,
                        ..
                    }]
                )
        ));
    }

    #[test]
    fn dispatched_non_idempotent_write_requires_durable_recovery_block() {
        let mut projection = open_step_projection();
        let call = pending_call("call_send", 10, SideEffectClass::NonIdempotentWrite);
        let dispatch = pending_dispatch("call_send", 11, 1);
        projection
            .pending_tool_calls
            .insert(call.data.call_id.clone(), call);
        projection
            .pending_tool_dispatches
            .insert(dispatch.data.call_id.clone(), dispatch);

        let decision = RecoveryAnalyzer.analyze(&projection).unwrap();
        assert!(matches!(
            decision,
            ResumeDecision::PersistRecoveryBlock { proposal }
                if proposal.call.data.call_id == ToolCallId::new("call_send").unwrap()
        ));
    }

    #[test]
    fn tool_recovery_order_uses_event_sequence_not_identifier_order() {
        let mut projection = open_step_projection();
        let first = pending_call("call_z", 10, SideEffectClass::ReadOnly);
        let second = pending_call("call_a", 20, SideEffectClass::ReadOnly);
        projection
            .pending_tool_calls
            .insert(first.data.call_id.clone(), first);
        projection
            .pending_tool_calls
            .insert(second.data.call_id.clone(), second);

        let decision = RecoveryAnalyzer.analyze(&projection).unwrap();
        let ResumeDecision::RecoverToolBatch { actions, .. } = decision else {
            panic!("expected tool recovery batch");
        };
        let ids: Vec<_> = actions
            .iter()
            .map(|action| match action {
                ToolRecoveryAction::StartUndispatched { call }
                | ToolRecoveryAction::RetryDispatched { call, .. } => call.data.call_id.as_str(),
            })
            .collect();
        assert_eq!(ids, vec!["call_z", "call_a"]);
    }
}
