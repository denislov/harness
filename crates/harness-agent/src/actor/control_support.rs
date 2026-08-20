use harness_session::{
    ApprovalResolved, InboxDiscarded, ModelFailed, NewSessionEvent, RecoveryBlockKind,
    RecoveryBlocked, SessionEventPayload, SessionStore, StepEndReason, StepEnded, StepPosition,
    ToolCallRecorded, ToolResultRecorded, TurnEndReason, TurnEnded,
};
use harness_types::{
    ApprovalDecision, ApprovalId, CancelCause, ErrorCode, PortableError, SideEffectClass,
    ToolOutcome,
};

use super::AgentActor;
use super::tool_support::invocation_id_from_event;
use crate::{AgentError, AgentEventSource, AgentPhase, ApprovalReceipt};

impl AgentActor {
    pub(super) async fn handle_resolve_approval(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        note: Option<String>,
    ) -> Result<ApprovalReceipt, AgentError> {
        let pending = self
            .state
            .projection
            .pending_approval
            .as_ref()
            .cloned()
            .ok_or_else(|| AgentError::InvalidDurableMutation {
                message: "no Tool approval is pending".to_owned(),
            })?;
        if pending.data.approval_id != approval_id {
            return Err(AgentError::InvalidDurableMutation {
                message: format!(
                    "approval {approval_id} does not match pending approval {}",
                    pending.data.approval_id
                ),
            });
        }

        let draft = NewSessionEvent::new(
            event_source.next_event_id(),
            event_source.now(),
            SessionEventPayload::ApprovalResolved(ApprovalResolved {
                approval_id: approval_id.clone(),
                call_id: pending.data.call_id,
                decision,
                note,
            }),
        )
        .in_step(pending.turn, pending.step);
        let committed = self.append_validated(store, vec![draft]).await?;
        let event = committed
            .last()
            .expect("single approval resolution append must commit one event");
        Ok(ApprovalReceipt {
            approval_id,
            decision,
            event_id: event.event_id().clone(),
            seq: event.seq(),
        })
    }

    pub(super) async fn handle_cancel(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
        llm_runtime: Option<&crate::AgentLlmRuntime>,
        tool_runtime: Option<&crate::AgentToolRuntime>,
        cause: CancelCause,
        keep_inbox: bool,
    ) -> Result<(), AgentError> {
        let active_operation = self.state.active_operation.clone();
        let cancel_target =
            self.capability_cancel_target(llm_runtime, tool_runtime, active_operation.as_ref());
        let mut drafts = self.cancel_activity_drafts(event_source, tool_runtime, cause)?;
        if !keep_inbox {
            drafts.extend(self.discard_pending_inbox_drafts(event_source, cause));
        }
        if !drafts.is_empty() {
            // Durable convergence is the cancellation acknowledgement boundary.
            // Only after it commits do we signal/abort the process-local task. If
            // the append fails, the live operation remains owned and may continue.
            self.append_validated(store, drafts).await?;
        }

        // Provider cancellation is advisory and deliberately occurs only after
        // the durable terminal/recovery state is committed. A transport failure
        // here cannot roll back the authoritative cancellation decision.
        if let Some(target) = cancel_target {
            target.signal(cause).await;
        }

        match active_operation {
            Some(crate::ActiveAgentOperation::Model { .. }) => self.abort_active_llm_task(),
            Some(crate::ActiveAgentOperation::Tool { .. }) => self.abort_active_tool_task(),
            None => {}
        }
        self.state.active_operation = None;
        self.state.phase = if let Some(position) = self.state.projection.lifecycle.open_step {
            AgentPhase::Running {
                turn: position.turn,
                step: Some(position.step),
            }
        } else if let Some(turn) = self.state.projection.lifecycle.open_turn {
            AgentPhase::Running { turn, step: None }
        } else {
            AgentPhase::Idle {
                last_turn: self.state.projection.lifecycle.last_ended_turn,
            }
        };
        Ok(())
    }

    fn capability_cancel_target(
        &self,
        llm_runtime: Option<&crate::AgentLlmRuntime>,
        tool_runtime: Option<&crate::AgentToolRuntime>,
        active: Option<&crate::ActiveAgentOperation>,
    ) -> Option<CapabilityCancelTarget> {
        match active? {
            crate::ActiveAgentOperation::Model { request_id, .. } => {
                let provider = llm_runtime?.provider().clone();
                Some(CapabilityCancelTarget::Model {
                    provider,
                    request_id: request_id.clone(),
                })
            }
            crate::ActiveAgentOperation::Tool {
                call_id,
                invocation_id,
                ..
            } => {
                let pending = self.state.projection.pending_tool_calls.get(call_id)?;
                let executor = tool_runtime?
                    .resolve(&pending.data.tool)?
                    .executor()
                    .clone();
                Some(CapabilityCancelTarget::Tool {
                    executor,
                    invocation_id: invocation_id.clone(),
                })
            }
        }
    }

    fn cancel_activity_drafts(
        &self,
        event_source: &dyn AgentEventSource,
        tool_runtime: Option<&crate::AgentToolRuntime>,
        cause: CancelCause,
    ) -> Result<Vec<NewSessionEvent>, AgentError> {
        let Some(position) = self.state.projection.lifecycle.open_step else {
            return Ok(self
                .state
                .projection
                .lifecycle
                .open_turn
                .map(|turn| {
                    vec![
                        NewSessionEvent::new(
                            event_source.next_event_id(),
                            event_source.now(),
                            SessionEventPayload::TurnEnded(TurnEnded {
                                reason: TurnEndReason::Cancelled,
                            }),
                        )
                        .in_turn(turn),
                    ]
                })
                .unwrap_or_default());
        };

        if let Some(request) = self.state.projection.pending_model_request.as_ref() {
            return Ok(vec![
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::ModelFailed(ModelFailed {
                        request_id: request.request_id.clone(),
                        failure: PortableError::new(
                            ErrorCode::Cancelled,
                            format!("model request cancelled by {}", cancel_cause_label(cause)),
                        ),
                    }),
                )
                .in_step(position.turn, position.step),
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::StepEnded(StepEnded {
                        reason: StepEndReason::Cancelled,
                    }),
                )
                .in_step(position.turn, position.step),
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::TurnEnded(TurnEnded {
                        reason: TurnEndReason::Cancelled,
                    }),
                )
                .in_turn(position.turn),
            ]);
        }

        if !self.state.projection.open_step_tools.announced.is_empty()
            || !self.state.projection.pending_tool_calls.is_empty()
        {
            return self.cancel_tool_step_drafts(event_source, tool_runtime, position, cause);
        }

        Ok(vec![
            NewSessionEvent::new(
                event_source.next_event_id(),
                event_source.now(),
                SessionEventPayload::StepEnded(StepEnded {
                    reason: StepEndReason::Cancelled,
                }),
            )
            .in_step(position.turn, position.step),
            NewSessionEvent::new(
                event_source.next_event_id(),
                event_source.now(),
                SessionEventPayload::TurnEnded(TurnEnded {
                    reason: TurnEndReason::Cancelled,
                }),
            )
            .in_turn(position.turn),
        ])
    }

    fn cancel_tool_step_drafts(
        &self,
        event_source: &dyn AgentEventSource,
        tool_runtime: Option<&crate::AgentToolRuntime>,
        position: StepPosition,
        cause: CancelCause,
    ) -> Result<Vec<NewSessionEvent>, AgentError> {
        let mut drafts = Vec::new();

        if let Some(approval) = self.state.projection.pending_approval.as_ref() {
            drafts.push(
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::ApprovalResolved(ApprovalResolved {
                        approval_id: approval.data.approval_id.clone(),
                        call_id: approval.data.call_id.clone(),
                        decision: ApprovalDecision::Deny,
                        note: Some(format!(
                            "approval cancelled by {}",
                            cancel_cause_label(cause)
                        )),
                    }),
                )
                .in_step(position.turn, position.step),
            );
        }

        let mut blocking = None;
        for announced in &self.state.projection.open_step_tools.announced {
            if self
                .state
                .projection
                .open_step_tools
                .completed
                .contains(&announced.call_id)
            {
                continue;
            }

            let pending = self
                .state
                .projection
                .pending_tool_calls
                .get(&announced.call_id);
            let side_effect = pending
                .map(|call| call.data.side_effect)
                .unwrap_or_else(|| {
                    tool_runtime
                        .and_then(|runtime| runtime.resolve(&announced.name))
                        .map(|registration| registration.definition().side_effect)
                        // Cancellation may arrive with no Tool runtime attached. The
                        // conservative class keeps an unresolvable model ToolCall from
                        // ever becoming retry-safer than its durable history proves.
                        .unwrap_or(SideEffectClass::NonIdempotentWrite)
                });
            if pending.is_none() {
                drafts.push(
                    NewSessionEvent::new(
                        event_source.next_event_id(),
                        event_source.now(),
                        SessionEventPayload::ToolCall(ToolCallRecorded {
                            call_id: announced.call_id.clone(),
                            tool: announced.name.clone(),
                            arguments_json: announced.arguments_json.clone(),
                            side_effect,
                        }),
                    )
                    .in_step(position.turn, position.step),
                );
            }

            let dispatch = self
                .state
                .projection
                .pending_tool_dispatches
                .get(&announced.call_id);

            if side_effect == SideEffectClass::NonIdempotentWrite
                && let Some(dispatch) = dispatch
            {
                if blocking.is_some() {
                    return Err(AgentError::InvalidDurableMutation {
                        message: "more than one unresolved non-idempotent Tool dispatch exists during cancellation"
                            .to_owned(),
                    });
                }
                blocking = Some((announced.call_id.clone(), dispatch.clone()));
                continue;
            }

            let result_event_id = event_source.next_event_id();
            let invocation_id = match dispatch {
                Some(dispatch) => dispatch.data.invocation_id.clone(),
                None => invocation_id_from_event(&result_event_id)?,
            };
            let outcome = if dispatch.is_some() && side_effect == SideEffectClass::IdempotentWrite {
                ToolOutcome::Unknown {
                    reason: format!(
                        "idempotent Tool dispatch was interrupted by {} cancellation",
                        cancel_cause_label(cause)
                    ),
                }
            } else {
                ToolOutcome::Cancelled { cause }
            };
            drafts.push(
                NewSessionEvent::new(
                    result_event_id,
                    event_source.now(),
                    SessionEventPayload::ToolResult(ToolResultRecorded {
                        call_id: announced.call_id.clone(),
                        invocation_id,
                        outcome,
                    }),
                )
                .in_step(position.turn, position.step),
            );
        }

        if let Some((call_id, dispatch)) = blocking {
            drafts.extend([
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::RecoveryBlocked(RecoveryBlocked {
                        kind: RecoveryBlockKind::UnknownToolOutcome,
                        call_id,
                        invocation_id: dispatch.data.invocation_id,
                        reason: format!(
                            "non-idempotent Tool dispatch was interrupted by {} cancellation; external outcome is unknown",
                            cancel_cause_label(cause)
                        ),
                    }),
                )
                .in_step(position.turn, position.step),
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::StepEnded(StepEnded {
                        reason: StepEndReason::Blocked,
                    }),
                )
                .in_step(position.turn, position.step),
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::TurnEnded(TurnEnded {
                        reason: TurnEndReason::Blocked,
                    }),
                )
                .in_turn(position.turn),
            ]);
        } else {
            drafts.extend([
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::StepEnded(StepEnded {
                        reason: StepEndReason::Cancelled,
                    }),
                )
                .in_step(position.turn, position.step),
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::TurnEnded(TurnEnded {
                        reason: TurnEndReason::Cancelled,
                    }),
                )
                .in_turn(position.turn),
            ]);
        }
        Ok(drafts)
    }

    fn discard_pending_inbox_drafts(
        &self,
        event_source: &dyn AgentEventSource,
        cause: CancelCause,
    ) -> Vec<NewSessionEvent> {
        self.state
            .projection
            .inbox
            .next_turn
            .iter()
            .chain(self.state.projection.inbox.next_step.iter())
            .map(|item| {
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::InboxDiscarded(InboxDiscarded {
                        message_id: item.message.id.clone(),
                        reason: format!("cancelled:{}", cancel_cause_label(cause)),
                    }),
                )
            })
            .collect()
    }
}

enum CapabilityCancelTarget {
    Model {
        provider: std::sync::Arc<dyn harness_llm::LlmProvider>,
        request_id: harness_types::RequestId,
    },
    Tool {
        executor: std::sync::Arc<dyn harness_tools::ToolExecutor>,
        invocation_id: harness_types::InvocationId,
    },
}

impl CapabilityCancelTarget {
    async fn signal(self, cause: CancelCause) {
        match self {
            Self::Model {
                provider,
                request_id,
            } => {
                let _ = provider.cancel(request_id, cause).await;
            }
            Self::Tool {
                executor,
                invocation_id,
            } => {
                let _ = executor.cancel(invocation_id, cause).await;
            }
        }
    }
}

fn cancel_cause_label(cause: CancelCause) -> &'static str {
    match cause {
        CancelCause::User => "user",
        CancelCause::Parent => "parent",
        CancelCause::Timeout => "timeout",
        CancelCause::Policy => "policy",
        CancelCause::Shutdown => "shutdown",
        CancelCause::Disposed => "disposed",
    }
}
