use harness_session::{PendingToolCall, StepPosition, ToolCallRecorded};
use harness_tools::{IdempotencySupport, PolicyDecision, ToolPolicyInput, ToolRegistration};
use harness_types::{ApprovalDecision, IdempotencyKey, SideEffectClass, ToolCallId, ToolOutcome};

use crate::{AgentError, AgentState, AgentToolRuntime};

#[derive(Clone)]
pub(crate) enum ToolDriverPlan {
    RecordCalls {
        position: StepPosition,
        calls: Vec<ToolCallRecorded>,
    },
    RequestApproval {
        position: StepPosition,
        call_id: ToolCallId,
        reason: String,
        risk: String,
    },
    CompleteWithoutDispatch {
        position: StepPosition,
        call_id: ToolCallId,
        outcome: ToolOutcome,
    },
    CompleteAfterDispatch {
        position: StepPosition,
        call_id: ToolCallId,
        invocation_id: harness_types::InvocationId,
        outcome: ToolOutcome,
    },
    Dispatch {
        position: StepPosition,
        call: PendingToolCall,
        registration: Box<ToolRegistration>,
        attempt: u32,
        idempotency_key: Option<IdempotencyKey>,
    },
    EndStep {
        position: StepPosition,
    },
    Deferred,
}

pub(crate) fn plan_tool_boundary(
    state: &AgentState,
    runtime: &AgentToolRuntime,
    position: StepPosition,
) -> Result<ToolDriverPlan, AgentError> {
    let projection = &state.projection.open_step_tools;
    if projection.announced.is_empty() {
        return Err(AgentError::InvalidToolRuntime {
            message: "ReadyForTools boundary has no announced ToolCall blocks".to_owned(),
        });
    }

    let unrecorded: Vec<_> = projection
        .announced
        .iter()
        .filter(|call| !projection.recorded.contains(&call.call_id))
        .map(|call| {
            let side_effect = runtime
                .resolve(&call.name)
                .map(|registration| registration.definition().side_effect)
                .unwrap_or(SideEffectClass::NonIdempotentWrite);
            ToolCallRecorded {
                call_id: call.call_id.clone(),
                tool: call.name.clone(),
                arguments_json: call.arguments_json.clone(),
                side_effect,
            }
        })
        .collect();
    if !unrecorded.is_empty() {
        return Ok(ToolDriverPlan::RecordCalls {
            position,
            calls: unrecorded,
        });
    }

    if projection
        .announced
        .iter()
        .all(|call| projection.completed.contains(&call.call_id))
    {
        return Ok(ToolDriverPlan::EndStep { position });
    }

    let announced = projection
        .announced
        .iter()
        .find(|call| !projection.completed.contains(&call.call_id))
        .expect("not all announced ToolCalls were completed");
    let pending = state
        .projection
        .pending_tool_calls
        .get(&announced.call_id)
        .cloned()
        .ok_or_else(|| AgentError::InvalidToolRuntime {
            message: format!(
                "ToolCall {} is recorded but is neither completed nor pending",
                announced.call_id
            ),
        })?;

    if state
        .projection
        .pending_approval
        .as_ref()
        .is_some_and(|approval| approval.data.call_id == pending.data.call_id)
    {
        return Ok(ToolDriverPlan::Deferred);
    }

    let Some(registration) = runtime.resolve(&pending.data.tool).cloned() else {
        if state
            .projection
            .pending_tool_dispatches
            .contains_key(&pending.data.call_id)
        {
            return Ok(ToolDriverPlan::Deferred);
        }
        return Ok(ToolDriverPlan::CompleteWithoutDispatch {
            position,
            call_id: pending.data.call_id,
            outcome: ToolOutcome::Error {
                code: "NOT_FOUND".to_owned(),
                message: format!("tool {} is not registered", pending.data.tool),
                content: Vec::new(),
            },
        });
    };

    if registration.definition().side_effect != pending.data.side_effect {
        return Err(AgentError::InvalidToolRuntime {
            message: format!(
                "registered side-effect class for tool {} changed while call {} is pending",
                pending.data.tool, pending.data.call_id
            ),
        });
    }

    let previous_dispatch = state
        .projection
        .pending_tool_dispatches
        .get(&pending.data.call_id)
        .cloned();

    if let Some(previous) = previous_dispatch {
        if previous.data.attempt >= runtime.max_automatic_attempts() {
            let outcome = match pending.data.side_effect {
                SideEffectClass::ReadOnly => ToolOutcome::Error {
                    code: "TOOL_RETRY_EXHAUSTED".to_owned(),
                    message: format!(
                        "automatic retry budget exhausted after {} durable dispatch attempt(s)",
                        previous.data.attempt
                    ),
                    content: Vec::new(),
                },
                SideEffectClass::IdempotentWrite => ToolOutcome::Unknown {
                    reason: format!(
                        "automatic retry budget exhausted after {} durable dispatch attempt(s)",
                        previous.data.attempt
                    ),
                },
                SideEffectClass::NonIdempotentWrite => {
                    return Err(AgentError::InvalidToolRuntime {
                        message: "dispatched non-idempotent ToolCall reached retry exhaustion instead of recovery blocking"
                            .to_owned(),
                    });
                }
            };
            return Ok(ToolDriverPlan::CompleteAfterDispatch {
                position,
                call_id: pending.data.call_id,
                invocation_id: previous.data.invocation_id,
                outcome,
            });
        }
        if registration.executor().provider_id() != &previous.data.provider_id {
            return Err(AgentError::InvalidToolRuntime {
                message: format!(
                    "provider binding for tool {} changed from {} to {} while call {} is pending",
                    pending.data.tool,
                    previous.data.provider_id,
                    registration.executor().provider_id(),
                    pending.data.call_id
                ),
            });
        }
        if pending.data.side_effect == SideEffectClass::NonIdempotentWrite {
            return Err(AgentError::InvalidToolRuntime {
                message: "dispatched non-idempotent ToolCall reached retry planning instead of recovery blocking"
                    .to_owned(),
            });
        }
        if pending.data.side_effect == SideEffectClass::IdempotentWrite
            && registration.executor().idempotency_support() != IdempotencySupport::Keyed
        {
            return Err(AgentError::InvalidToolRuntime {
                message: format!(
                    "idempotent-write tool {} no longer has keyed idempotency support",
                    pending.data.tool
                ),
            });
        }

        if registration
            .validator()
            .validate(registration.definition(), &pending.data.arguments_json)
            .is_err()
        {
            return Ok(ToolDriverPlan::Deferred);
        }

        let policy_allows_retry = match projection.approvals.get(&pending.data.call_id) {
            Some(resolution) => resolution.decision == ApprovalDecision::Allow,
            None => matches!(
                runtime.policy().evaluate(&policy_input(state, &pending)),
                PolicyDecision::Allow
            ),
        };
        if !policy_allows_retry {
            return Ok(ToolDriverPlan::Deferred);
        }

        let attempt =
            previous
                .data
                .attempt
                .checked_add(1)
                .ok_or_else(|| AgentError::InvalidToolRuntime {
                    message: format!(
                        "tool dispatch attempt overflow for call {}",
                        pending.data.call_id
                    ),
                })?;
        return Ok(ToolDriverPlan::Dispatch {
            position,
            call: pending,
            registration: Box::new(registration),
            attempt,
            idempotency_key: Some(previous.data.idempotency_key),
        });
    }

    if let Err(error) = registration
        .validator()
        .validate(registration.definition(), &pending.data.arguments_json)
    {
        return Ok(ToolDriverPlan::CompleteWithoutDispatch {
            position,
            call_id: pending.data.call_id,
            outcome: ToolOutcome::Error {
                code: "INVALID_ARGUMENT".to_owned(),
                message: error.to_string(),
                content: Vec::new(),
            },
        });
    }

    if let Some(resolution) = projection.approvals.get(&pending.data.call_id) {
        return Ok(match resolution.decision {
            ApprovalDecision::Allow => ToolDriverPlan::Dispatch {
                position,
                call: pending,
                registration: Box::new(registration),
                attempt: 1,
                idempotency_key: None,
            },
            ApprovalDecision::Deny => ToolDriverPlan::CompleteWithoutDispatch {
                position,
                call_id: pending.data.call_id,
                outcome: ToolOutcome::Denied {
                    reason: approval_denial_reason(resolution.note.as_deref()),
                },
            },
        });
    }

    match runtime.policy().evaluate(&policy_input(state, &pending)) {
        PolicyDecision::Allow => Ok(ToolDriverPlan::Dispatch {
            position,
            call: pending,
            registration: Box::new(registration),
            attempt: 1,
            idempotency_key: None,
        }),
        PolicyDecision::Deny { reason } => Ok(ToolDriverPlan::CompleteWithoutDispatch {
            position,
            call_id: pending.data.call_id,
            outcome: ToolOutcome::Denied { reason },
        }),
        PolicyDecision::Ask { reason, risk } => Ok(ToolDriverPlan::RequestApproval {
            position,
            call_id: pending.data.call_id,
            reason,
            risk,
        }),
        _ => Err(AgentError::InvalidToolRuntime {
            message: "tool policy returned an unsupported decision variant".to_owned(),
        }),
    }
}

fn approval_denial_reason(note: Option<&str>) -> String {
    match note {
        Some(note) if !note.is_empty() => format!("tool execution denied by approval: {note}"),
        _ => "tool execution denied by approval".to_owned(),
    }
}

fn policy_input(state: &AgentState, pending: &PendingToolCall) -> ToolPolicyInput {
    ToolPolicyInput {
        session_id: state.session_id.clone(),
        turn: pending.turn,
        step: pending.step,
        call_id: pending.data.call_id.clone(),
        tool_name: pending.data.tool.clone(),
        arguments_json: pending.data.arguments_json.clone(),
        side_effect: pending.data.side_effect,
    }
}
