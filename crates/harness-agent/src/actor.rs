use std::sync::Arc;

use harness_session::{
    InboxEnqueued, ModelFailed, NewSessionEvent, RecoveryBlockKind, RecoveryBlocked, SessionEvent,
    SessionEventPayload, SessionProjector, SessionStore, StepEndReason, StepEnded, TurnEndReason,
    TurnEnded, V1SessionProjector,
};
use harness_types::{AgentInstanceId, ErrorCode, Message, PortableError};
use tokio::sync::mpsc;

use crate::{
    AgentBootstrap, AgentCommand, AgentCommandAck, AgentError, AgentEventSource, AgentPhase,
    AgentState, MailboxMessage, RecoveryAnalyzer, ResumeDecision, SendReceipt,
};

/// Single-owner process-local Agent actor state.
///
/// This type deliberately does not implement `Clone`. Duplicating the owner
/// would violate the Session single-writer invariant. Cloneable access is
/// provided only through `AgentHandle`.
#[derive(Debug)]
pub struct AgentActor {
    state: AgentState,
    history: Vec<SessionEvent>,
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
        }
    }

    pub const fn state(&self) -> &AgentState {
        &self.state
    }

    pub fn into_state(self) -> AgentState {
        self.state
    }

    /// Performs only recovery actions that require no external capability call.
    ///
    /// Batch 05 converges two crash artifacts before exposing the live handle:
    /// - an interrupted model request becomes a durable `model/failed`;
    /// - an unknown non-idempotent Tool outcome becomes a durable recovery gate,
    ///   and the interrupted step/turn are closed as blocked in the same batch.
    ///
    /// Tool retries and ordinary open-step/open-turn continuation remain driver
    /// work for the next batch.
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
                                reason: "process restarted after a non-idempotent Tool dispatch without a durable terminal result".to_owned(),
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

    pub(crate) async fn run(
        mut self,
        store: Arc<dyn SessionStore>,
        event_source: Arc<dyn AgentEventSource>,
        mut rx: mpsc::Receiver<MailboxMessage>,
    ) -> AgentExit {
        while let Some(message) = rx.recv().await {
            match message {
                MailboxMessage::Snapshot { reply } => {
                    let _ = reply.send(self.state.clone());
                }
                MailboxMessage::Command { command, reply } => {
                    if matches!(&command, AgentCommand::Shutdown) {
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
                    let _ = reply.send(result);

                    if terminal {
                        return AgentExit {
                            reason: AgentExitReason::Fatal(
                                fatal.expect("terminal command failure must carry an error"),
                            ),
                            final_state: self.state,
                        };
                    }
                }
            }
        }

        AgentExit {
            reason: AgentExitReason::MailboxClosed,
            final_state: self.state,
        }
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
                reason: "active-driver cancellation and inbox discard convergence are introduced with the Turn/Step driver",
            }),
            AgentCommand::Shutdown => unreachable!("shutdown is handled before command dispatch"),
        }
    }

    async fn handle_send(
        &mut self,
        store: &dyn SessionStore,
        event_source: &dyn AgentEventSource,
        message: Message,
        target: harness_types::InboxTarget,
        wakeup: bool,
    ) -> Result<SendReceipt, AgentError> {
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

        if wakeup {
            self.state.wake_requested = true;
        }

        Ok(SendReceipt {
            message_id,
            event_id: event.event_id().clone(),
            seq: event.seq(),
            wake_requested: self.state.wake_requested,
        })
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
