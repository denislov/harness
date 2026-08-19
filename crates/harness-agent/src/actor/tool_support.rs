use harness_session::{
    NewSessionEvent, SessionEventPayload, SessionStore, StepEndReason, StepEnded, StepPosition,
    ToolDispatched, ToolResultRecorded,
};
use harness_tools::{ToolInvocation, ToolInvocationPosition};
use harness_types::{IdempotencyKey, InvocationId, ToolCallId, ToolOutcome};
use tokio::sync::mpsc;

use super::AgentActor;
use crate::tool_driver::{ToolDriverPlan, plan_tool_boundary};
use crate::tool_operation::spawn_tool_operation;
use crate::{
    ActiveAgentOperation, AgentError, AgentEventSource, AgentToolRuntime, MailboxMessage,
    ToolCompletion,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolAdvance {
    Progressed,
    Started,
    Deferred,
}

impl AgentActor {
    pub(super) async fn advance_tool_boundary(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
        runtime: &AgentToolRuntime,
        position: StepPosition,
        self_tx: &mpsc::Sender<MailboxMessage>,
    ) -> Result<ToolAdvance, AgentError> {
        match plan_tool_boundary(&self.state, runtime, position)? {
            ToolDriverPlan::RecordCalls { position, calls } => {
                let drafts = calls
                    .into_iter()
                    .map(|call| {
                        NewSessionEvent::new(
                            event_source.next_event_id(),
                            event_source.now(),
                            SessionEventPayload::ToolCall(call),
                        )
                        .in_step(position.turn, position.step)
                    })
                    .collect();
                self.append_validated(store, drafts).await?;
                Ok(ToolAdvance::Progressed)
            }
            ToolDriverPlan::CompleteWithoutDispatch {
                position,
                call_id,
                outcome,
            } => {
                let result_event_id = event_source.next_event_id();
                let invocation_id = invocation_id_from_event(&result_event_id)?;
                let draft = NewSessionEvent::new(
                    result_event_id,
                    event_source.now(),
                    SessionEventPayload::ToolResult(ToolResultRecorded {
                        call_id,
                        invocation_id,
                        outcome,
                    }),
                )
                .in_step(position.turn, position.step);
                self.append_validated(store, vec![draft]).await?;
                Ok(ToolAdvance::Progressed)
            }
            ToolDriverPlan::CompleteAfterDispatch {
                position,
                call_id,
                invocation_id,
                outcome,
            } => {
                let draft = NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::ToolResult(ToolResultRecorded {
                        call_id,
                        invocation_id,
                        outcome,
                    }),
                )
                .in_step(position.turn, position.step);
                self.append_validated(store, vec![draft]).await?;
                Ok(ToolAdvance::Progressed)
            }
            ToolDriverPlan::Dispatch {
                position,
                call,
                registration,
                attempt,
                idempotency_key,
            } => {
                let dispatch_event_id = event_source.next_event_id();
                let invocation_id = invocation_id_from_event(&dispatch_event_id)?;
                let idempotency_key = idempotency_key.unwrap_or_else(|| {
                    idempotency_key_for_call(&self.state.session_id, &call.data.call_id)
                });
                let invocation = ToolInvocation {
                    invocation_id: invocation_id.clone(),
                    call_id: call.data.call_id.clone(),
                    session_id: self.state.session_id.clone(),
                    position: ToolInvocationPosition {
                        turn: position.turn,
                        step: position.step,
                    },
                    tool_name: call.data.tool.clone(),
                    arguments_json: call.data.arguments_json.clone(),
                    attempt,
                    idempotency_key: idempotency_key.clone(),
                };
                invocation
                    .validate()
                    .map_err(|error| AgentError::InvalidToolRuntime {
                        message: error.to_string(),
                    })?;

                let draft = NewSessionEvent::new(
                    dispatch_event_id,
                    event_source.now(),
                    SessionEventPayload::ToolDispatched(ToolDispatched {
                        call_id: call.data.call_id.clone(),
                        invocation_id: invocation_id.clone(),
                        provider_id: registration.executor().provider_id().clone(),
                        attempt,
                        idempotency_key,
                    }),
                )
                .in_step(position.turn, position.step);
                self.append_validated(store, vec![draft]).await?;

                self.state.active_operation = Some(ActiveAgentOperation::Tool {
                    position,
                    call_id: call.data.call_id,
                    invocation_id: invocation_id.clone(),
                    attempt,
                });
                self.active_tool_task = Some(spawn_tool_operation(
                    registration.executor().clone(),
                    invocation,
                    position,
                    self_tx.clone(),
                ));
                Ok(ToolAdvance::Started)
            }
            ToolDriverPlan::EndStep { position } => {
                let draft = NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::StepEnded(StepEnded {
                        reason: StepEndReason::ToolContinuation,
                    }),
                )
                .in_step(position.turn, position.step);
                self.append_validated(store, vec![draft]).await?;
                Ok(ToolAdvance::Progressed)
            }
            ToolDriverPlan::Deferred => Ok(ToolAdvance::Deferred),
        }
    }

    pub(super) async fn handle_tool_completion(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
        completion: ToolCompletion,
    ) -> Result<(), AgentError> {
        let Some(ActiveAgentOperation::Tool {
            position,
            call_id,
            invocation_id,
            attempt: _,
        }) = self.state.active_operation.clone()
        else {
            return Err(AgentError::UnexpectedToolCompletion {
                call_id: completion.call_id,
                message: "no Tool operation is active".to_owned(),
            });
        };

        if position != completion.position
            || call_id != completion.call_id
            || invocation_id != completion.invocation_id
        {
            return Err(AgentError::UnexpectedToolCompletion {
                call_id: completion.call_id,
                message: format!(
                    "active Tool operation is call {call_id}, invocation {invocation_id} at turn {}, step {}",
                    position.turn, position.step
                ),
            });
        }

        match completion.outcome {
            Ok(outcome) if !matches!(&outcome, ToolOutcome::Unknown { .. }) => {
                let draft = NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::ToolResult(ToolResultRecorded {
                        call_id,
                        invocation_id,
                        outcome,
                    }),
                )
                .in_step(position.turn, position.step);
                self.append_validated(store, vec![draft]).await?;
            }
            Ok(_) | Err(_) => {
                // No terminal SessionEvent is fabricated for an ambiguous provider
                // outcome. After the live operation overlay is removed,
                // RecoveryAnalyzer regains authority over the durable dispatch.
            }
        }

        self.state.active_operation = None;
        let _ = self.active_tool_task.take();
        Ok(())
    }

    pub(super) fn abort_active_tool_task(&mut self) {
        if let Some(task) = self.active_tool_task.take() {
            task.abort();
        }
        if matches!(
            &self.state.active_operation,
            Some(ActiveAgentOperation::Tool { .. })
        ) {
            self.state.active_operation = None;
        }
    }
}

fn invocation_id_from_event(event_id: &harness_types::EventId) -> Result<InvocationId, AgentError> {
    InvocationId::new(format!("inv:tool:{event_id}")).map_err(|error| {
        AgentError::InvalidToolRuntime {
            message: format!("failed to derive InvocationId from EventId: {error}"),
        }
    })
}

fn idempotency_key_for_call(
    session_id: &harness_types::SessionId,
    call_id: &ToolCallId,
) -> IdempotencyKey {
    IdempotencyKey::new(format!("idem:tool:{session_id}:{call_id}"))
        .expect("SessionId and ToolCallId are non-empty, so the derived key is non-empty")
}
