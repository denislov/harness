use std::sync::Arc;

use harness_llm::{FinishReason, LlmStreamOutcome};
use harness_session::{
    AssistantMessage, InboxClaimed, InboxEnqueued, ModelFailed, ModelRequested, NewSessionEvent,
    RecoveryBlockKind, RecoveryBlocked, SessionEvent, SessionEventPayload, SessionProjector,
    SessionStore, StepEndReason, StepEnded, StepPosition, StepStarted, TurnEndReason, TurnEnded,
    TurnStarted, UserMessage, V1SessionProjector,
};
use harness_types::{
    AgentInstanceId, ContentBlock, ErrorCode, EventId, InboxTarget, Message, MessageId,
    MessageSource, PortableError, RequestId, Role, StepNo, TurnNo,
};
use tokio::{sync::mpsc, task::JoinHandle};

use crate::llm_operation::spawn_llm_operation;
use crate::loop_driver::{DriverPlan, PlannedInboxInput, plan_next};
use crate::{
    ActiveAgentOperation, AgentBootstrap, AgentCommand, AgentCommandAck, AgentError,
    AgentEventSource, AgentLlmRuntime, AgentPhase, AgentState, AgentToolRuntime, LlmCompletion,
    MailboxMessage, RecoveryAnalyzer, ResumeDecision, SendReceipt,
};

mod tool_support;

/// Single-owner process-local Agent actor state.
///
/// This type deliberately does not implement `Clone`. Duplicating the owner
/// would violate the Session single-writer invariant. Cloneable access is
/// provided only through `AgentHandle`.
pub struct AgentActor {
    state: AgentState,
    history: Vec<SessionEvent>,
    active_llm_task: Option<JoinHandle<()>>,
    active_tool_task: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum AgentExitReason {
    ShutdownRequested,
    MailboxClosed,
    Fatal(AgentError),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentExit {
    pub reason: AgentExitReason,
    pub final_state: AgentState,
}

impl AgentActor {
    pub fn from_bootstrap(instance_id: AgentInstanceId, bootstrap: AgentBootstrap) -> Self {
        let history = bootstrap.events.clone();
        Self {
            state: AgentState::from_bootstrap(instance_id, bootstrap),
            history,
            active_llm_task: None,
            active_tool_task: None,
        }
    }

    pub const fn state(&self) -> &AgentState {
        &self.state
    }

    pub fn into_state(self) -> AgentState {
        self.state
    }

    /// Performs only recovery actions that require no external capability call.
    pub(crate) async fn converge_startup(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
    ) -> Result<(), AgentError> {
        self.state.phase = AgentPhase::Maintenance;

        loop {
            let drafts = match self.state.resume.clone() {
                ResumeDecision::RecoverInterruptedModelRequest { position, request } => {
                    vec![NewSessionEvent::new(
                        event_source.next_event_id(),
                        event_source.now(),
                        SessionEventPayload::ModelFailed(ModelFailed {
                            request_id: request.request_id,
                            failure: PortableError::new(
                                ErrorCode::ModelRequestFailed,
                                "model request was interrupted by process restart before a durable terminal response",
                            ),
                        }),
                    )
                    .in_step(position.turn, position.step)]
                }
                ResumeDecision::PersistRecoveryBlock { proposal } => {
                    let position = proposal.position;
                    vec![
                        NewSessionEvent::new(
                            event_source.next_event_id(),
                            event_source.now(),
                            SessionEventPayload::RecoveryBlocked(RecoveryBlocked {
                                kind: RecoveryBlockKind::UnknownToolOutcome,
                                call_id: proposal.call.data.call_id,
                                invocation_id: proposal.dispatch.data.invocation_id,
                                reason: "a non-idempotent Tool dispatch has no durable terminal result; the external outcome is unknown".to_owned(),
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
                    ]
                }
                _ => break,
            };

            self.append_validated(store, drafts).await?;
        }

        self.state.phase = AgentPhase::Idle {
            last_turn: self.state.projection.lifecycle.last_ended_turn,
        };
        Ok(())
    }

    /// Advances deterministic Turn/Step state until an external-operation boundary
    /// is reached or no currently supported driver work remains.
    pub(crate) async fn converge_driver_boundary(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
    ) -> Result<(), AgentError> {
        if self.state.active_operation.is_some() {
            return Ok(());
        }

        loop {
            match plan_next(&self.state)? {
                DriverPlan::Dormant => {
                    self.state.phase = AgentPhase::Idle {
                        last_turn: self.state.projection.lifecycle.last_ended_turn,
                    };
                    return Ok(());
                }
                DriverPlan::Deferred => {
                    self.state.phase =
                        if let Some(position) = self.state.projection.lifecycle.open_step {
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
                    return Ok(());
                }
                DriverPlan::StartNewTurn { turn, step, inputs } => {
                    let drafts =
                        self.build_step_entry_drafts(event_source, Some(turn), turn, step, &inputs);
                    self.append_validated(store, drafts).await?;
                    self.state.phase = AgentPhase::Running {
                        turn,
                        step: Some(step),
                    };
                }
                DriverPlan::StartStep { turn, step, inputs } => {
                    let drafts =
                        self.build_step_entry_drafts(event_source, None, turn, step, &inputs);
                    self.append_validated(store, drafts).await?;
                    self.state.phase = AgentPhase::Running {
                        turn,
                        step: Some(step),
                    };
                }
                DriverPlan::EnterCurrentStep { position, inputs } => {
                    let drafts =
                        self.build_current_step_input_drafts(event_source, position, &inputs);
                    if !drafts.is_empty() {
                        self.append_validated(store, drafts).await?;
                    }
                    self.state.phase = AgentPhase::Running {
                        turn: position.turn,
                        step: Some(position.step),
                    };
                }
                DriverPlan::EndOpenTurn { turn } => {
                    let draft = NewSessionEvent::new(
                        event_source.next_event_id(),
                        event_source.now(),
                        SessionEventPayload::TurnEnded(TurnEnded {
                            reason: TurnEndReason::Completed,
                        }),
                    )
                    .in_turn(turn);
                    self.append_validated(store, vec![draft]).await?;
                    self.state.phase = AgentPhase::Idle {
                        last_turn: self.state.projection.lifecycle.last_ended_turn,
                    };
                }
                DriverPlan::Park(boundary) => {
                    match boundary {
                        crate::AgentDriverBoundary::ReadyForModel { position }
                        | crate::AgentDriverBoundary::ReadyForTools { position } => {
                            self.state.phase = AgentPhase::Running {
                                turn: position.turn,
                                step: Some(position.step),
                            };
                        }
                    }
                    return Ok(());
                }
            }
        }
    }

    /// Advances deterministic Core state and starts at most one external capability
    /// operation. Provider/Tool futures report completion back through the mailbox;
    /// the actor never awaits them while it owns command processing.
    pub(crate) async fn advance_runtime(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
        llm_runtime: Option<&AgentLlmRuntime>,
        tool_runtime: Option<&AgentToolRuntime>,
        self_tx: &mpsc::Sender<MailboxMessage>,
    ) -> Result<(), AgentError> {
        if self.state.active_operation.is_some() {
            return Ok(());
        }

        loop {
            if matches!(
                &self.state.resume,
                ResumeDecision::RecoverInterruptedModelRequest { .. }
                    | ResumeDecision::PersistRecoveryBlock { .. }
            ) {
                self.converge_startup(store, event_source).await?;
            }

            self.converge_driver_boundary(store, event_source).await?;

            match self.state.driver_boundary() {
                Some(crate::AgentDriverBoundary::ReadyForModel { position }) => {
                    let Some(llm_runtime) = llm_runtime else {
                        return Ok(());
                    };
                    self.start_model_operation(
                        store,
                        event_source,
                        llm_runtime,
                        tool_runtime,
                        position,
                        self_tx,
                    )
                    .await?;
                    return Ok(());
                }
                Some(crate::AgentDriverBoundary::ReadyForTools { position }) => {
                    let Some(tool_runtime) = tool_runtime else {
                        return Ok(());
                    };
                    match self
                        .advance_tool_boundary(store, event_source, tool_runtime, position, self_tx)
                        .await?
                    {
                        tool_support::ToolAdvance::Progressed => continue,
                        tool_support::ToolAdvance::Started
                        | tool_support::ToolAdvance::Deferred => {
                            return Ok(());
                        }
                    }
                }
                None => return Ok(()),
            }
        }
    }

    async fn start_model_operation(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
        llm_runtime: &AgentLlmRuntime,
        tool_runtime: Option<&AgentToolRuntime>,
        position: StepPosition,
        self_tx: &mpsc::Sender<MailboxMessage>,
    ) -> Result<(), AgentError> {
        if self.state.active_operation.is_some() {
            return Err(AgentError::InvalidDurableMutation {
                message: "attempted to start a model operation while another external operation is active"
                    .to_owned(),
            });
        }

        let attempt = self.next_model_attempt(position)?;
        let request_event_id = event_source.next_event_id();
        let request_id = request_id_from_event(&request_event_id)?;
        let mut request_config = llm_runtime.request_config().clone();
        if let Some(tool_runtime) = tool_runtime {
            request_config.tools = tool_runtime.model_tool_specs();
        }
        let request = request_config
            .build(
                request_id.clone(),
                self.state.session_id.clone(),
                self.state.projection.model_messages.clone(),
            )
            .map_err(|error| AgentError::InvalidModelRequest {
                message: error.to_string(),
            })?;
        let snapshot_bytes =
            request
                .snapshot_bytes()
                .map_err(|error| AgentError::InvalidModelRequest {
                    message: error.to_string(),
                })?;
        let request_snapshot = llm_runtime
            .blob_store()
            .put(snapshot_bytes, Some("application/json".to_owned()))
            .await
            .map_err(|error| AgentError::BlobStorage {
                message: error.to_string(),
            })?;

        let history_through_seq = self.state.expected_seq;
        let draft = NewSessionEvent::new(
            request_event_id,
            event_source.now(),
            SessionEventPayload::ModelRequested(ModelRequested {
                request_id: request_id.clone(),
                provider: request.provider.clone(),
                model: request.model.clone(),
                history_through_seq,
                request_snapshot,
                attempt,
            }),
        )
        .in_step(position.turn, position.step);
        self.append_validated(store, vec![draft]).await?;

        self.state.active_operation = Some(ActiveAgentOperation::Model {
            position,
            request_id: request_id.clone(),
            attempt,
        });
        self.active_llm_task = Some(spawn_llm_operation(
            llm_runtime.provider().clone(),
            request,
            position,
            self_tx.clone(),
        ));
        Ok(())
    }

    fn next_model_attempt(&self, position: StepPosition) -> Result<u32, AgentError> {
        let mut previous = None;
        for event in self.history.iter().rev() {
            if event.turn() == Some(position.turn)
                && event.step() == Some(position.step)
                && let SessionEventPayload::ModelRequested(data) = event.payload()
            {
                previous = Some(data.attempt);
                break;
            }
        }

        match previous {
            Some(attempt) => {
                attempt
                    .checked_add(1)
                    .ok_or_else(|| AgentError::InvalidModelRequest {
                        message: "model request attempt overflow".to_owned(),
                    })
            }
            None => Ok(1),
        }
    }

    async fn handle_llm_completion(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
        llm_runtime: Option<&AgentLlmRuntime>,
        completion: LlmCompletion,
    ) -> Result<(), AgentError> {
        let Some(ActiveAgentOperation::Model {
            position,
            request_id,
            attempt: _,
        }) = self.state.active_operation.clone()
        else {
            return Err(AgentError::UnexpectedModelCompletion {
                request_id: completion.request_id,
                message: "no model operation is active".to_owned(),
            });
        };
        if position != completion.position || request_id != completion.request_id {
            return Err(AgentError::UnexpectedModelCompletion {
                request_id: completion.request_id,
                message: format!(
                    "active model operation is request {request_id} at turn {}, step {}",
                    position.turn, position.step
                ),
            });
        }
        let llm_runtime = llm_runtime.ok_or_else(|| AgentError::UnexpectedModelCompletion {
            request_id: request_id.clone(),
            message: "completion arrived without an attached LLM runtime".to_owned(),
        })?;

        let drafts = match completion.outcome {
            Ok(LlmStreamOutcome::Assistant {
                content,
                usage,
                finish_reason,
            }) => {
                let has_tool_calls = content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolCall { .. }));
                let assistant_event_id = event_source.next_event_id();
                let message_id = message_id_from_event(&assistant_event_id)?;
                let message = Message {
                    id: message_id,
                    role: Role::Assistant,
                    source: MessageSource::model(
                        llm_runtime.request_config().provider.clone(),
                        llm_runtime.request_config().model.clone(),
                    ),
                    content,
                };
                let mut drafts = vec![
                    NewSessionEvent::new(
                        assistant_event_id,
                        event_source.now(),
                        SessionEventPayload::AssistantMessage(AssistantMessage {
                            request_id: request_id.clone(),
                            message,
                            usage,
                        }),
                    )
                    .in_step(position.turn, position.step),
                ];

                match finish_reason {
                    FinishReason::Completed if !has_tool_calls => drafts.push(
                        NewSessionEvent::new(
                            event_source.next_event_id(),
                            event_source.now(),
                            SessionEventPayload::StepEnded(StepEnded {
                                reason: StepEndReason::Completed,
                            }),
                        )
                        .in_step(position.turn, position.step),
                    ),
                    FinishReason::MaxTokens if !has_tool_calls => {
                        drafts.push(
                            NewSessionEvent::new(
                                event_source.next_event_id(),
                                event_source.now(),
                                SessionEventPayload::StepEnded(StepEnded {
                                    reason: StepEndReason::MaxTokens,
                                }),
                            )
                            .in_step(position.turn, position.step),
                        );
                        drafts.push(
                            NewSessionEvent::new(
                                event_source.next_event_id(),
                                event_source.now(),
                                SessionEventPayload::TurnEnded(TurnEnded {
                                    reason: TurnEndReason::MaxTokens,
                                }),
                            )
                            .in_turn(position.turn),
                        );
                    }
                    FinishReason::Completed | FinishReason::MaxTokens => {
                        // Tool-call assistants remain in the open step. Batch 08
                        // will resolve ToolDefinition metadata, persist tool/call,
                        // and continue the tool pipeline without a duplicate model
                        // request.
                    }
                    FinishReason::Error | FinishReason::Cancelled => {
                        return Err(AgentError::UnexpectedModelCompletion {
                            request_id,
                            message: "stream assembler returned an assistant outcome with a failure finish reason"
                                .to_owned(),
                        });
                    }
                    _ => {
                        return Err(AgentError::UnexpectedModelCompletion {
                            request_id,
                            message:
                                "stream assembler returned an unsupported assistant finish reason"
                                    .to_owned(),
                        });
                    }
                }
                drafts
            }
            Ok(LlmStreamOutcome::Failure {
                failure,
                finish_reason,
            }) => Self::model_failure_drafts(
                event_source,
                position,
                request_id.clone(),
                failure,
                finish_reason,
            )?,
            Ok(_) => {
                return Err(AgentError::UnexpectedModelCompletion {
                    request_id,
                    message: "stream assembler returned an unsupported outcome variant".to_owned(),
                });
            }
            Err(failure) => {
                let finish_reason = if failure.code == ErrorCode::Cancelled {
                    FinishReason::Cancelled
                } else {
                    FinishReason::Error
                };
                Self::model_failure_drafts(
                    event_source,
                    position,
                    request_id.clone(),
                    failure,
                    finish_reason,
                )?
            }
        };

        self.append_validated(store, drafts).await?;
        self.state.active_operation = None;
        drop(self.active_llm_task.take());
        Ok(())
    }

    fn model_failure_drafts(
        event_source: &dyn AgentEventSource,
        position: StepPosition,
        request_id: RequestId,
        failure: PortableError,
        finish_reason: FinishReason,
    ) -> Result<Vec<NewSessionEvent>, AgentError> {
        let (step_reason, turn_reason) = match finish_reason {
            FinishReason::Error => (StepEndReason::ModelError, TurnEndReason::Error),
            FinishReason::Cancelled => (StepEndReason::Cancelled, TurnEndReason::Cancelled),
            FinishReason::Completed | FinishReason::MaxTokens => {
                return Err(AgentError::UnexpectedModelCompletion {
                    request_id,
                    message: "failure outcome carried a non-failure finish reason".to_owned(),
                });
            }
            _ => {
                return Err(AgentError::UnexpectedModelCompletion {
                    request_id,
                    message: "failure outcome carried an unsupported finish reason".to_owned(),
                });
            }
        };

        Ok(vec![
            NewSessionEvent::new(
                event_source.next_event_id(),
                event_source.now(),
                SessionEventPayload::ModelFailed(ModelFailed {
                    request_id,
                    failure,
                }),
            )
            .in_step(position.turn, position.step),
            NewSessionEvent::new(
                event_source.next_event_id(),
                event_source.now(),
                SessionEventPayload::StepEnded(StepEnded {
                    reason: step_reason,
                }),
            )
            .in_step(position.turn, position.step),
            NewSessionEvent::new(
                event_source.next_event_id(),
                event_source.now(),
                SessionEventPayload::TurnEnded(TurnEnded {
                    reason: turn_reason,
                }),
            )
            .in_turn(position.turn),
        ])
    }

    pub(crate) async fn run(
        mut self,
        store: Arc<dyn SessionStore>,
        event_source: Arc<dyn AgentEventSource>,
        llm_runtime: Option<AgentLlmRuntime>,
        tool_runtime: Option<AgentToolRuntime>,
        self_tx: mpsc::Sender<MailboxMessage>,
        mut rx: mpsc::Receiver<MailboxMessage>,
    ) -> AgentExit {
        while let Some(message) = rx.recv().await {
            match message {
                MailboxMessage::Snapshot { reply } => {
                    let _ = reply.send(self.state.clone());
                }
                MailboxMessage::LlmCompleted(completion) => {
                    if let Err(error) = self
                        .handle_llm_completion(
                            store.as_ref(),
                            event_source.as_ref(),
                            llm_runtime.as_ref(),
                            completion,
                        )
                        .await
                    {
                        self.abort_active_tasks();
                        return AgentExit {
                            reason: AgentExitReason::Fatal(error),
                            final_state: self.state,
                        };
                    }
                    if let Err(error) = self
                        .advance_runtime(
                            store.as_ref(),
                            event_source.as_ref(),
                            llm_runtime.as_ref(),
                            tool_runtime.as_ref(),
                            &self_tx,
                        )
                        .await
                    {
                        self.abort_active_tasks();
                        return AgentExit {
                            reason: AgentExitReason::Fatal(error),
                            final_state: self.state,
                        };
                    }
                }
                MailboxMessage::ToolCompleted(completion) => {
                    if let Err(error) = self
                        .handle_tool_completion(store.as_ref(), event_source.as_ref(), completion)
                        .await
                    {
                        self.abort_active_tasks();
                        return AgentExit {
                            reason: AgentExitReason::Fatal(error),
                            final_state: self.state,
                        };
                    }
                    if let Err(error) = self
                        .advance_runtime(
                            store.as_ref(),
                            event_source.as_ref(),
                            llm_runtime.as_ref(),
                            tool_runtime.as_ref(),
                            &self_tx,
                        )
                        .await
                    {
                        self.abort_active_tasks();
                        return AgentExit {
                            reason: AgentExitReason::Fatal(error),
                            final_state: self.state,
                        };
                    }
                }
                MailboxMessage::Command { command, reply } => {
                    if matches!(&command, AgentCommand::Shutdown) {
                        self.abort_active_tasks();
                        let _ = reply.send(Ok(AgentCommandAck::Shutdown));
                        return AgentExit {
                            reason: AgentExitReason::ShutdownRequested,
                            final_state: self.state,
                        };
                    }

                    let result = self
                        .handle_command(store.as_ref(), event_source.as_ref(), command)
                        .await;
                    let terminal = result
                        .as_ref()
                        .err()
                        .is_some_and(|error| error.is_terminal_for_actor());
                    let fatal = result.as_ref().err().cloned();
                    let should_drive = result.is_ok();
                    let _ = reply.send(result);

                    if terminal {
                        self.abort_active_tasks();
                        return AgentExit {
                            reason: AgentExitReason::Fatal(
                                fatal.expect("terminal command failure must carry an error"),
                            ),
                            final_state: self.state,
                        };
                    }

                    // Command acknowledgement is deliberately sent before accepted
                    // Inbox work is consumed or an external model operation starts.
                    if should_drive
                        && let Err(error) = self
                            .advance_runtime(
                                store.as_ref(),
                                event_source.as_ref(),
                                llm_runtime.as_ref(),
                                tool_runtime.as_ref(),
                                &self_tx,
                            )
                            .await
                    {
                        self.abort_active_tasks();
                        return AgentExit {
                            reason: AgentExitReason::Fatal(error),
                            final_state: self.state,
                        };
                    }
                }
            }
        }

        self.abort_active_tasks();
        AgentExit {
            reason: AgentExitReason::MailboxClosed,
            final_state: self.state,
        }
    }

    fn abort_active_tasks(&mut self) {
        if let Some(task) = self.active_llm_task.take() {
            task.abort();
        }
        self.abort_active_tool_task();
        self.state.active_operation = None;
    }

    async fn handle_command(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
        command: AgentCommand,
    ) -> Result<AgentCommandAck, AgentError> {
        match command {
            AgentCommand::Send {
                message,
                target,
                wakeup,
            } => self
                .handle_send(store, event_source, message, target, wakeup)
                .await
                .map(AgentCommandAck::Send),
            AgentCommand::Cancel {
                cause: _,
                keep_inbox: _,
            } => Err(AgentError::UnsupportedOperation {
                operation: "cancel",
                reason: "active-operation cancellation and inbox discard convergence are introduced after the first LLM vertical slice",
            }),
            AgentCommand::Shutdown => unreachable!("shutdown is handled before command dispatch"),
        }
    }

    async fn handle_send(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
        message: Message,
        target: InboxTarget,
        wakeup: bool,
    ) -> Result<SendReceipt, AgentError> {
        if message.role != Role::User {
            return Err(AgentError::InvalidDurableMutation {
                message: "Agent Inbox accepts only user-role Messages in v0.1".to_owned(),
            });
        }

        let message_id = message.id.clone();
        let draft = NewSessionEvent::new(
            event_source.next_event_id(),
            event_source.now(),
            SessionEventPayload::InboxEnqueued(InboxEnqueued {
                message,
                target,
                wakeup,
            }),
        );

        let committed = self.append_validated(store, vec![draft]).await?;
        let event = committed
            .last()
            .expect("single-event append must return one committed event");

        Ok(SendReceipt {
            message_id,
            event_id: event.event_id().clone(),
            seq: event.seq(),
            wake_requested: self.state.wake_requested,
        })
    }

    fn build_step_entry_drafts(
        &self,
        event_source: &dyn AgentEventSource,
        new_turn: Option<TurnNo>,
        turn: TurnNo,
        step: StepNo,
        inputs: &[PlannedInboxInput],
    ) -> Vec<NewSessionEvent> {
        let mut drafts = Vec::with_capacity(2 + inputs.len() * 2);

        if let Some(new_turn) = new_turn {
            drafts.push(
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::TurnStarted(TurnStarted { turn: new_turn }),
                )
                .in_turn(new_turn),
            );
        }

        for input in inputs
            .iter()
            .filter(|input| input.target == InboxTarget::NextTurn)
        {
            drafts.push(
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::InboxClaimed(InboxClaimed {
                        message_id: input.item.message.id.clone(),
                        target: input.target,
                    }),
                )
                .in_turn(turn),
            );
        }

        drafts.push(
            NewSessionEvent::new(
                event_source.next_event_id(),
                event_source.now(),
                SessionEventPayload::StepStarted(StepStarted { turn, step }),
            )
            .in_step(turn, step),
        );

        for input in inputs {
            if input.target == InboxTarget::NextStep {
                drafts.push(
                    NewSessionEvent::new(
                        event_source.next_event_id(),
                        event_source.now(),
                        SessionEventPayload::InboxClaimed(InboxClaimed {
                            message_id: input.item.message.id.clone(),
                            target: input.target,
                        }),
                    )
                    .in_step(turn, step),
                );
            }

            drafts.push(
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::UserMessage(UserMessage {
                        message: input.item.message.clone(),
                    }),
                )
                .in_step(turn, step),
            );
        }

        drafts
    }

    fn build_current_step_input_drafts(
        &self,
        event_source: &dyn AgentEventSource,
        position: StepPosition,
        inputs: &[PlannedInboxInput],
    ) -> Vec<NewSessionEvent> {
        let mut drafts = Vec::with_capacity(inputs.len() * 2);

        for input in inputs {
            debug_assert_eq!(input.target, InboxTarget::NextStep);
            drafts.push(
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::InboxClaimed(InboxClaimed {
                        message_id: input.item.message.id.clone(),
                        target: input.target,
                    }),
                )
                .in_step(position.turn, position.step),
            );
            drafts.push(
                NewSessionEvent::new(
                    event_source.next_event_id(),
                    event_source.now(),
                    SessionEventPayload::UserMessage(UserMessage {
                        message: input.item.message.clone(),
                    }),
                )
                .in_step(position.turn, position.step),
            );
        }

        drafts
    }

    /// Validates a proposed batch against the actor's exact local snapshot before
    /// crossing the storage boundary, then verifies that the SessionStore returned
    /// exactly the committed events predicted by the contract.
    async fn append_validated(
        &mut self,
        store: &dyn SessionStore,
        drafts: Vec<NewSessionEvent>,
    ) -> Result<Vec<SessionEvent>, AgentError> {
        let mut preview_head = self.state.expected_seq;
        let mut preview_committed = Vec::with_capacity(drafts.len());
        let mut preview_history = self.history.clone();

        for draft in drafts.iter().cloned() {
            preview_head = preview_head.checked_next().map_err(|error| {
                AgentError::InvalidDurableMutation {
                    message: format!("cannot allocate next SessionEvent sequence: {error}"),
                }
            })?;
            let event = SessionEvent::committed(self.state.session_id.clone(), preview_head, draft)
                .map_err(|error| AgentError::InvalidDurableMutation {
                    message: error.to_string(),
                })?;
            preview_history.push(event.clone());
            preview_committed.push(event);
        }

        let preview_projection = V1SessionProjector
            .project(&preview_history)
            .map_err(|error| AgentError::InvalidDurableMutation {
                message: error.to_string(),
            })?;
        let preview_resume = RecoveryAnalyzer
            .analyze(&preview_projection)
            .map_err(|error| AgentError::InvalidDurableMutation {
                message: error.to_string(),
            })?;

        let result = store
            .append(&self.state.session_id, self.state.expected_seq, drafts)
            .await
            .map_err(AgentError::from_store)?;

        if result.new_head != preview_head || result.committed != preview_committed {
            return Err(AgentError::StorageContractViolation {
                message: format!(
                    "append result did not match the prevalidated commit: expected head {preview_head} and {} events, got head {} and {} events",
                    preview_committed.len(),
                    result.new_head,
                    result.committed.len()
                ),
            });
        }

        self.history = preview_history;
        self.state
            .replace_durable_view(preview_head, preview_projection, preview_resume);

        Ok(preview_committed)
    }
}

fn request_id_from_event(event_id: &EventId) -> Result<RequestId, AgentError> {
    RequestId::new(format!("req:model:{event_id}")).map_err(|error| {
        AgentError::InvalidModelRequest {
            message: format!("failed to derive RequestId from EventId: {error}"),
        }
    })
}

fn message_id_from_event(event_id: &EventId) -> Result<MessageId, AgentError> {
    MessageId::new(format!("msg:model:{event_id}")).map_err(|error| {
        AgentError::InvalidDurableMutation {
            message: format!("failed to derive assistant MessageId from EventId: {error}"),
        }
    })
}
