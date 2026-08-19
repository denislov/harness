use std::collections::{BTreeMap, BTreeSet, VecDeque};

use harness_types::{
    CancelCause, ContentBlock, EventId, EventSeq, InboxTarget, InvocationId, JsonText, Message,
    MessageId, MessageSource, ProviderId, Role, SideEffectClass, StepNo, ToolCallId, ToolOutcome,
    TurnNo,
};
use thiserror::Error;

use crate::{
    ModelRequested, RecoveryBlocked, SessionEvent, SessionEventPayload, StepEndReason,
    ToolCallRecorded, ToolDispatched, TurnEndReason,
};

pub const SESSION_PROJECTION_VERSION_V1: u16 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct PendingInboxItem {
    pub message: Message,
    pub target: InboxTarget,
    pub wakeup: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InboxProjection {
    pub next_turn: VecDeque<PendingInboxItem>,
    pub next_step: VecDeque<PendingInboxItem>,
}

impl InboxProjection {
    pub fn is_empty(&self) -> bool {
        self.next_turn.is_empty() && self.next_step.is_empty()
    }

    pub fn has_work(&self) -> bool {
        !self.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepPosition {
    pub turn: TurnNo,
    pub step: StepNo,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LifecycleProjection {
    pub open_turn: Option<TurnNo>,
    pub open_step: Option<StepPosition>,
    pub last_started_turn: Option<TurnNo>,
    pub last_ended_turn: Option<TurnNo>,
    pub last_started_step: Option<StepPosition>,
    pub last_ended_step: Option<StepPosition>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryBlock {
    pub blocked_event_id: EventId,
    pub turn: TurnNo,
    pub step: StepNo,
    pub data: RecoveryBlocked,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingToolCall {
    pub call_event_id: EventId,
    pub call_seq: EventSeq,
    pub turn: TurnNo,
    pub step: StepNo,
    pub data: ToolCallRecorded,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingToolDispatch {
    pub dispatch_event_id: EventId,
    pub dispatch_seq: EventSeq,
    pub turn: TurnNo,
    pub step: StepNo,
    pub data: ToolDispatched,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionProjection {
    pub model_messages: Vec<Message>,
    pub inbox: InboxProjection,
    pub lifecycle: LifecycleProjection,
    pub last_model_request: Option<ModelRequested>,
    pub pending_model_request: Option<ModelRequested>,
    pub pending_tool_calls: BTreeMap<ToolCallId, PendingToolCall>,
    pub pending_tool_dispatches: BTreeMap<ToolCallId, PendingToolDispatch>,
    pub unresolved_recovery: Option<RecoveryBlock>,
}

#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error("session event sequence is invalid: {0}")]
    InvalidSequence(String),

    #[error("session event structure is invalid: {0}")]
    InvalidEvent(String),

    #[error("inbox item {0} was claimed or discarded but is not pending")]
    MissingInboxItem(MessageId),

    #[error("inbox item {0} was enqueued more than once")]
    DuplicateInboxItem(MessageId),

    #[error("tool call {0} was recorded more than once")]
    DuplicateToolCall(ToolCallId),

    #[error("tool result references missing tool call {0}")]
    MissingToolCall(ToolCallId),

    #[error("projection rule is not defined for this durable state: {0}")]
    Unsupported(String),
}

pub trait SessionProjector: Send + Sync {
    fn version(&self) -> u16;

    fn project(&self, events: &[SessionEvent]) -> Result<SessionProjection, ProjectionError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct V1SessionProjector;

impl SessionProjector for V1SessionProjector {
    fn version(&self) -> u16 {
        SESSION_PROJECTION_VERSION_V1
    }

    fn project(&self, events: &[SessionEvent]) -> Result<SessionProjection, ProjectionError> {
        ProjectorState::project(events)
    }
}

#[derive(Clone, Debug)]
struct AnnouncedToolCall {
    name: String,
    arguments_json: JsonText,
}

#[derive(Default)]
struct ProjectorState {
    projection: SessionProjection,
    seen_inbox_ids: BTreeSet<MessageId>,
    seen_tool_calls: BTreeSet<ToolCallId>,
    invocation_owners: BTreeMap<InvocationId, ToolCallId>,
    terminal_invocations: BTreeSet<InvocationId>,
    current_step_assistant_seen: bool,
    current_step_announced_calls: BTreeMap<ToolCallId, AnnouncedToolCall>,
    current_step_recorded_calls: BTreeSet<ToolCallId>,
}

impl ProjectorState {
    fn project(events: &[SessionEvent]) -> Result<SessionProjection, ProjectionError> {
        validate_stream_envelope(events)?;

        let mut state = Self::default();
        for event in events {
            event
                .validate()
                .map_err(|error| ProjectionError::InvalidEvent(error.to_string()))?;
            state.apply(event)?;
        }

        Ok(state.projection)
    }

    fn apply(&mut self, event: &SessionEvent) -> Result<(), ProjectionError> {
        match event.payload() {
            SessionEventPayload::SessionCreated(_) => self.apply_session_created(event),
            SessionEventPayload::InboxEnqueued(data) => {
                let message_id = data.message.id.clone();
                if !self.seen_inbox_ids.insert(message_id.clone()) {
                    return Err(ProjectionError::DuplicateInboxItem(message_id));
                }

                let item = PendingInboxItem {
                    message: data.message.clone(),
                    target: data.target,
                    wakeup: data.wakeup,
                };
                self.inbox_queue_mut(data.target).push_back(item);
                Ok(())
            }
            SessionEventPayload::InboxClaimed(data) => {
                self.require_open_turn(event)?;
                if event.step().is_some() {
                    self.require_open_step(event)?;
                }
                remove_pending_inbox_item(self.inbox_queue_mut(data.target), &data.message_id)?;
                Ok(())
            }
            SessionEventPayload::InboxDiscarded(data) => {
                if remove_pending_inbox_item_if_present(
                    &mut self.projection.inbox.next_turn,
                    &data.message_id,
                )
                .is_none()
                    && remove_pending_inbox_item_if_present(
                        &mut self.projection.inbox.next_step,
                        &data.message_id,
                    )
                    .is_none()
                {
                    return Err(ProjectionError::MissingInboxItem(data.message_id.clone()));
                }
                Ok(())
            }
            SessionEventPayload::TurnStarted(data) => {
                if self.projection.lifecycle.open_turn.is_some() {
                    return self.invalid(
                        event,
                        "turn/started encountered while a turn is already open",
                    );
                }
                if self.projection.lifecycle.open_step.is_some() {
                    return self.invalid(
                        event,
                        "turn/started encountered while a step is already open",
                    );
                }
                if self.projection.unresolved_recovery.is_some() {
                    return self.invalid(
                        event,
                        "turn/started encountered while ExecutionGate is blocked",
                    );
                }

                self.projection.lifecycle.open_turn = Some(data.turn);
                self.projection.lifecycle.last_started_turn = Some(data.turn);
                Ok(())
            }
            SessionEventPayload::StepStarted(data) => {
                let open_turn = self.require_open_turn(event)?;
                if open_turn != data.turn {
                    return self.invalid(event, "step/started turn does not match the open turn");
                }
                if self.projection.lifecycle.open_step.is_some() {
                    return self.invalid(
                        event,
                        "step/started encountered while a step is already open",
                    );
                }

                let position = StepPosition {
                    turn: data.turn,
                    step: data.step,
                };
                self.projection.lifecycle.open_step = Some(position);
                self.projection.lifecycle.last_started_step = Some(position);
                self.current_step_assistant_seen = false;
                self.current_step_announced_calls.clear();
                self.current_step_recorded_calls.clear();
                Ok(())
            }
            SessionEventPayload::UserMessage(data) => {
                self.require_open_step(event)?;
                if data.message.role != Role::User {
                    return self.invalid(event, "user/message must contain a user-role Message");
                }
                self.projection.model_messages.push(data.message.clone());
                Ok(())
            }
            SessionEventPayload::ModelRequested(data) => {
                self.require_open_step(event)?;
                if self.projection.pending_model_request.is_some() {
                    return self.invalid(
                        event,
                        "model/requested encountered while another model request is pending",
                    );
                }
                if data.attempt == 0 {
                    return self
                        .invalid(event, "model/requested attempt must be greater than zero");
                }
                if data.history_through_seq >= event.seq() {
                    return self.invalid(
                        event,
                        "model/requested historyThroughSeq must precede the request event",
                    );
                }

                self.projection.last_model_request = Some(data.clone());
                self.projection.pending_model_request = Some(data.clone());
                Ok(())
            }
            SessionEventPayload::ModelFailed(data) => {
                self.require_open_step(event)?;
                let pending = self
                    .projection
                    .pending_model_request
                    .as_ref()
                    .ok_or_else(|| {
                        invalid_event_at(event, "model/failed has no pending model request")
                    })?;
                if pending.request_id != data.request_id {
                    return self.invalid(
                        event,
                        "model/failed requestId does not match pending request",
                    );
                }
                self.projection.pending_model_request = None;
                Ok(())
            }
            SessionEventPayload::AssistantMessage(data) => {
                self.require_open_step(event)?;
                if self.current_step_assistant_seen {
                    return self.invalid(
                        event,
                        "a step may contain at most one authoritative assistant/message",
                    );
                }

                let pending = self
                    .projection
                    .pending_model_request
                    .as_ref()
                    .ok_or_else(|| {
                        invalid_event_at(event, "assistant/message has no pending model request")
                    })?;
                if pending.request_id != data.request_id {
                    return self.invalid(
                        event,
                        "assistant/message requestId does not match pending request",
                    );
                }
                validate_assistant_message_source(
                    event,
                    &data.message,
                    &pending.provider,
                    &pending.model,
                )?;

                self.current_step_announced_calls =
                    collect_announced_tool_calls(event, &data.message)?;
                self.current_step_assistant_seen = true;
                self.projection.pending_model_request = None;
                self.projection.model_messages.push(data.message.clone());
                Ok(())
            }
            SessionEventPayload::ToolCall(data) => self.apply_tool_call(event, data),
            SessionEventPayload::ToolDispatched(data) => self.apply_tool_dispatched(event, data),
            SessionEventPayload::ToolResult(data) => self.apply_tool_result(event, data),
            SessionEventPayload::StepEnded(data) => {
                let position = self.require_open_step(event)?;
                if self.projection.pending_model_request.is_some() {
                    return self
                        .invalid(event, "step/ended encountered with a pending model request");
                }

                for call_id in self.current_step_announced_calls.keys() {
                    if !self.current_step_recorded_calls.contains(call_id) {
                        return self.invalid(
                            event,
                            &format!(
                                "assistant announced tool call {call_id} but no tool/call event was recorded"
                            ),
                        );
                    }
                }

                let has_pending_for_step = self
                    .projection
                    .pending_tool_calls
                    .values()
                    .any(|call| call.turn == position.turn && call.step == position.step);
                if has_pending_for_step
                    && !(data.reason == StepEndReason::Blocked
                        && self.projection.unresolved_recovery.is_some())
                {
                    return self.invalid(
                        event,
                        "step/ended has unresolved tool calls without an active recovery block",
                    );
                }

                self.projection.lifecycle.open_step = None;
                self.projection.lifecycle.last_ended_step = Some(position);
                self.current_step_assistant_seen = false;
                self.current_step_announced_calls.clear();
                self.current_step_recorded_calls.clear();
                Ok(())
            }
            SessionEventPayload::TurnEnded(data) => {
                let open_turn = self.require_open_turn(event)?;
                if self.projection.lifecycle.open_step.is_some() {
                    return self
                        .invalid(event, "turn/ended encountered while a step is still open");
                }

                let has_pending_for_turn = self
                    .projection
                    .pending_tool_calls
                    .values()
                    .any(|call| call.turn == open_turn);
                if has_pending_for_turn && data.reason != TurnEndReason::Blocked {
                    return self.invalid(
                        event,
                        "turn/ended has unresolved tool calls but reason is not blocked",
                    );
                }
                if self.projection.unresolved_recovery.is_some()
                    && data.reason != TurnEndReason::Blocked
                {
                    return self.invalid(
                        event,
                        "turn/ended must use blocked while ExecutionGate is blocked",
                    );
                }

                self.projection.lifecycle.open_turn = None;
                self.projection.lifecycle.last_ended_turn = Some(open_turn);
                Ok(())
            }
            SessionEventPayload::RecoveryBlocked(data) => {
                let position = self.require_open_step(event)?;
                if self.projection.unresolved_recovery.is_some() {
                    return self.invalid(event, "a recovery block is already unresolved");
                }
                let pending = self
                    .projection
                    .pending_tool_calls
                    .get(&data.call_id)
                    .ok_or_else(|| ProjectionError::MissingToolCall(data.call_id.clone()))?;
                if pending.turn != position.turn || pending.step != position.step {
                    return self.invalid(
                        event,
                        "recovery/blocked does not refer to a tool call in the open step",
                    );
                }
                if pending.data.side_effect != SideEffectClass::NonIdempotentWrite {
                    return self.invalid(
                        event,
                        "recovery/blocked unknown-tool-outcome requires a non-idempotent-write tool",
                    );
                }
                let dispatch = self
                    .projection
                    .pending_tool_dispatches
                    .get(&data.call_id)
                    .ok_or_else(|| {
                        invalid_event_at(
                            event,
                            "recovery/blocked requires a prior durable tool/dispatched event",
                        )
                    })?;
                if dispatch.data.invocation_id != data.invocation_id {
                    return self.invalid(
                        event,
                        "recovery/blocked invocationId does not match the latest durable dispatch",
                    );
                }

                self.projection.unresolved_recovery = Some(RecoveryBlock {
                    blocked_event_id: event.event_id().clone(),
                    turn: position.turn,
                    step: position.step,
                    data: data.clone(),
                });
                Ok(())
            }
            SessionEventPayload::RecoveryResolved(data) => {
                if self.projection.lifecycle.open_turn.is_some()
                    || self.projection.lifecycle.open_step.is_some()
                {
                    return self.invalid(
                        event,
                        "recovery/resolved may only clear a gate outside an active turn",
                    );
                }
                let block = self
                    .projection
                    .unresolved_recovery
                    .as_ref()
                    .ok_or_else(|| {
                        invalid_event_at(
                            event,
                            "recovery/resolved has no unresolved recovery block",
                        )
                    })?;
                if block.blocked_event_id != data.blocked_event_id {
                    return self.invalid(
                        event,
                        "recovery/resolved blockedEventId does not match the active block",
                    );
                }
                if self
                    .projection
                    .pending_tool_calls
                    .contains_key(&block.data.call_id)
                {
                    return self.invalid(
                        event,
                        "recovery/resolved requires an authoritative tool/result for the blocked call first",
                    );
                }

                self.projection.unresolved_recovery = None;
                Ok(())
            }
        }
    }

    fn apply_session_created(&self, event: &SessionEvent) -> Result<(), ProjectionError> {
        if event.seq() != EventSeq::FIRST {
            return self.invalid(event, "session/created must be the first committed event");
        }
        Ok(())
    }

    fn apply_tool_call(
        &mut self,
        event: &SessionEvent,
        data: &ToolCallRecorded,
    ) -> Result<(), ProjectionError> {
        let position = self.require_open_step(event)?;
        if !self.current_step_assistant_seen {
            return self.invalid(
                event,
                "tool/call requires an authoritative assistant/message first",
            );
        }
        if !self.seen_tool_calls.insert(data.call_id.clone()) {
            return Err(ProjectionError::DuplicateToolCall(data.call_id.clone()));
        }

        let announced = self
            .current_step_announced_calls
            .get(&data.call_id)
            .ok_or_else(|| {
                invalid_event_at(
                    event,
                    &format!(
                        "tool/call {} was not announced by the step's assistant/message",
                        data.call_id
                    ),
                )
            })?;
        if announced.name != data.tool || announced.arguments_json != data.arguments_json {
            return self.invalid(
                event,
                "tool/call name or argumentsJson does not match assistant/message",
            );
        }

        self.current_step_recorded_calls
            .insert(data.call_id.clone());
        self.projection.pending_tool_calls.insert(
            data.call_id.clone(),
            PendingToolCall {
                call_event_id: event.event_id().clone(),
                call_seq: event.seq(),
                turn: position.turn,
                step: position.step,
                data: data.clone(),
            },
        );
        Ok(())
    }

    fn apply_tool_dispatched(
        &mut self,
        event: &SessionEvent,
        data: &ToolDispatched,
    ) -> Result<(), ProjectionError> {
        let position = self.require_open_step(event)?;
        if self.projection.unresolved_recovery.is_some() {
            return self.invalid(
                event,
                "tool/dispatched encountered while ExecutionGate is blocked",
            );
        }
        if data.attempt == 0 {
            return self.invalid(event, "tool/dispatched attempt must be greater than zero");
        }

        let pending = self
            .projection
            .pending_tool_calls
            .get(&data.call_id)
            .cloned()
            .ok_or_else(|| ProjectionError::MissingToolCall(data.call_id.clone()))?;
        if pending.turn != position.turn || pending.step != position.step {
            return self.invalid(
                event,
                "tool/dispatched does not refer to a tool call in the open step",
            );
        }

        if let Some(owner) = self.invocation_owners.get(&data.invocation_id) {
            return self.invalid(
                event,
                &format!(
                    "invocationId {} is already owned by tool call {owner}",
                    data.invocation_id
                ),
            );
        }

        if let Some(previous) = self.projection.pending_tool_dispatches.get(&data.call_id) {
            if pending.data.side_effect == SideEffectClass::NonIdempotentWrite {
                return self.invalid(
                    event,
                    "non-idempotent-write tool may not be durably redispatched automatically",
                );
            }
            let expected_attempt = previous
                .data
                .attempt
                .checked_add(1)
                .ok_or_else(|| invalid_event_at(event, "tool dispatch attempt overflow"))?;
            if data.attempt != expected_attempt {
                return self.invalid(
                    event,
                    "tool/dispatched retry attempt must increment by exactly one",
                );
            }
            if data.idempotency_key != previous.data.idempotency_key {
                return self.invalid(event, "tool/dispatched retry must preserve idempotencyKey");
            }
            if data.provider_id != previous.data.provider_id {
                return self.invalid(
                    event,
                    "tool/dispatched retry must preserve providerId in v0.1",
                );
            }
        } else if data.attempt != 1 {
            return self.invalid(event, "first tool/dispatched attempt must equal one");
        }

        self.invocation_owners
            .insert(data.invocation_id.clone(), data.call_id.clone());
        self.projection.pending_tool_dispatches.insert(
            data.call_id.clone(),
            PendingToolDispatch {
                dispatch_event_id: event.event_id().clone(),
                dispatch_seq: event.seq(),
                turn: position.turn,
                step: position.step,
                data: data.clone(),
            },
        );
        Ok(())
    }

    fn apply_tool_result(
        &mut self,
        event: &SessionEvent,
        data: &crate::ToolResultRecorded,
    ) -> Result<(), ProjectionError> {
        let pending = self
            .projection
            .pending_tool_calls
            .get(&data.call_id)
            .cloned()
            .ok_or_else(|| ProjectionError::MissingToolCall(data.call_id.clone()))?;

        let event_turn = event.turn().ok_or_else(|| {
            invalid_event_at(event, "tool/result requires the original tool call turn")
        })?;
        let event_step = event.step().ok_or_else(|| {
            invalid_event_at(event, "tool/result requires the original tool call step")
        })?;
        if event_turn != pending.turn || event_step != pending.step {
            return self.invalid(
                event,
                "tool/result turn/step does not match the original tool/call",
            );
        }

        let dispatch = self
            .projection
            .pending_tool_dispatches
            .get(&data.call_id)
            .cloned();
        if let Some(dispatch) = &dispatch {
            if dispatch.data.invocation_id != data.invocation_id {
                return self.invalid(
                    event,
                    "tool/result invocationId does not match the latest durable dispatch",
                );
            }
        } else if matches!(
            &data.outcome,
            ToolOutcome::Success { .. } | ToolOutcome::Unknown { .. }
        ) {
            return self.invalid(
                event,
                "successful or unknown tool/result requires a prior tool/dispatched event",
            );
        }

        if let Some(owner) = self.invocation_owners.get(&data.invocation_id) {
            if owner != &data.call_id {
                return self.invalid(
                    event,
                    "tool/result invocationId belongs to a different tool call",
                );
            }
        } else {
            self.invocation_owners
                .insert(data.invocation_id.clone(), data.call_id.clone());
        }
        if !self.terminal_invocations.insert(data.invocation_id.clone()) {
            return self.invalid(event, "invocationId already has a terminal tool/result");
        }

        let in_open_step = self.projection.lifecycle.open_step
            == Some(StepPosition {
                turn: pending.turn,
                step: pending.step,
            });
        if !in_open_step {
            let block = self
                .projection
                .unresolved_recovery
                .as_ref()
                .ok_or_else(|| {
                    invalid_event_at(
                        event,
                        "late tool/result is only legal for the active recovery block",
                    )
                })?;
            if block.data.call_id != data.call_id
                || block.data.invocation_id != data.invocation_id
                || block.turn != pending.turn
                || block.step != pending.step
            {
                return self.invalid(
                    event,
                    "late tool/result does not match the active recovery block",
                );
            }
            if dispatch.is_none() {
                return self.invalid(
                    event,
                    "late recovery tool/result requires the original durable dispatch",
                );
            }
        }

        self.projection.pending_tool_calls.remove(&data.call_id);
        self.projection
            .pending_tool_dispatches
            .remove(&data.call_id);
        self.projection
            .model_messages
            .push(project_tool_result_message(event, data)?);
        Ok(())
    }

    fn require_open_turn(&self, event: &SessionEvent) -> Result<TurnNo, ProjectionError> {
        let open_turn = self.projection.lifecycle.open_turn.ok_or_else(|| {
            invalid_event_at(
                event,
                "event requires an open turn in the projected lifecycle",
            )
        })?;
        if event.turn() != Some(open_turn) {
            return self.invalid(event, "event turn does not match the open turn");
        }
        Ok(open_turn)
    }

    fn require_open_step(&self, event: &SessionEvent) -> Result<StepPosition, ProjectionError> {
        let open_turn = self.require_open_turn(event)?;
        let open_step = self.projection.lifecycle.open_step.ok_or_else(|| {
            invalid_event_at(
                event,
                "event requires an open step in the projected lifecycle",
            )
        })?;
        if open_step.turn != open_turn || event.step() != Some(open_step.step) {
            return self.invalid(event, "event step does not match the open step");
        }
        Ok(open_step)
    }

    fn inbox_queue_mut(&mut self, target: InboxTarget) -> &mut VecDeque<PendingInboxItem> {
        match target {
            InboxTarget::NextTurn => &mut self.projection.inbox.next_turn,
            InboxTarget::NextStep => &mut self.projection.inbox.next_step,
        }
    }

    fn invalid<T>(&self, event: &SessionEvent, reason: &str) -> Result<T, ProjectionError> {
        Err(invalid_event_at(event, reason))
    }
}

fn validate_stream_envelope(events: &[SessionEvent]) -> Result<(), ProjectionError> {
    let first = events.first().ok_or_else(|| {
        ProjectionError::InvalidSequence("a committed session must contain session/created".into())
    })?;
    if !matches!(first.payload(), SessionEventPayload::SessionCreated(_)) {
        return Err(ProjectionError::InvalidSequence(
            "the first committed event must be session/created".into(),
        ));
    }
    if first.seq() != EventSeq::FIRST {
        return Err(ProjectionError::InvalidSequence(format!(
            "the first event must have seq {}, got {}",
            EventSeq::FIRST,
            first.seq()
        )));
    }

    let session_id = first.session_id().clone();
    let mut expected = EventSeq::FIRST;
    for (index, event) in events.iter().enumerate() {
        if event.session_id() != &session_id {
            return Err(ProjectionError::InvalidSequence(format!(
                "event {} belongs to session {}, expected {}",
                event.event_id(),
                event.session_id(),
                session_id
            )));
        }
        if event.seq() != expected {
            return Err(ProjectionError::InvalidSequence(format!(
                "event {} has seq {}, expected {}",
                event.event_id(),
                event.seq(),
                expected
            )));
        }
        if index > 0 && matches!(event.payload(), SessionEventPayload::SessionCreated(_)) {
            return Err(ProjectionError::InvalidSequence(
                "session/created may only appear once at seq 1".into(),
            ));
        }
        if index + 1 < events.len() {
            expected = expected.checked_next().map_err(|error| {
                ProjectionError::InvalidSequence(format!("event sequence overflow: {error}"))
            })?;
        }
    }
    Ok(())
}

fn validate_assistant_message_source(
    event: &SessionEvent,
    message: &Message,
    expected_provider: &ProviderId,
    expected_model: &str,
) -> Result<(), ProjectionError> {
    if message.role != Role::Assistant {
        return Err(invalid_event_at(
            event,
            "assistant/message must contain an assistant-role Message",
        ));
    }
    match &message.source {
        MessageSource::Model { provider, model }
            if provider == expected_provider && model == expected_model =>
        {
            Ok(())
        }
        MessageSource::Model { .. } => Err(invalid_event_at(
            event,
            "assistant/message model source does not match model/requested",
        )),
        _ => Err(invalid_event_at(
            event,
            "assistant/message source must be model",
        )),
    }
}

fn collect_announced_tool_calls(
    event: &SessionEvent,
    message: &Message,
) -> Result<BTreeMap<ToolCallId, AnnouncedToolCall>, ProjectionError> {
    let mut calls = BTreeMap::new();
    for block in &message.content {
        if let ContentBlock::ToolCall {
            id,
            name,
            arguments_json,
        } = block
        {
            let previous = calls.insert(
                id.clone(),
                AnnouncedToolCall {
                    name: name.clone(),
                    arguments_json: arguments_json.clone(),
                },
            );
            if previous.is_some() {
                return Err(invalid_event_at(
                    event,
                    &format!("assistant/message contains duplicate tool call id {id}"),
                ));
            }
        }
    }
    Ok(calls)
}

fn remove_pending_inbox_item(
    queue: &mut VecDeque<PendingInboxItem>,
    message_id: &MessageId,
) -> Result<PendingInboxItem, ProjectionError> {
    remove_pending_inbox_item_if_present(queue, message_id)
        .ok_or_else(|| ProjectionError::MissingInboxItem(message_id.clone()))
}

fn remove_pending_inbox_item_if_present(
    queue: &mut VecDeque<PendingInboxItem>,
    message_id: &MessageId,
) -> Option<PendingInboxItem> {
    let index = queue
        .iter()
        .position(|item| &item.message.id == message_id)?;
    queue.remove(index)
}

fn project_tool_result_message(
    event: &SessionEvent,
    data: &crate::ToolResultRecorded,
) -> Result<Message, ProjectionError> {
    let message_id = MessageId::new(format!(
        "msg_projected_tool_result_{}",
        event.event_id().as_str()
    ))
    .map_err(|error| ProjectionError::InvalidEvent(error.to_string()))?;

    let (content, is_error) = project_tool_outcome(&data.outcome)?;
    Ok(Message {
        id: message_id,
        role: Role::User,
        source: MessageSource::plugin(),
        content: vec![ContentBlock::ToolResult {
            tool_call_id: data.call_id.clone(),
            content,
            is_error,
        }],
    })
}

fn project_tool_outcome(
    outcome: &ToolOutcome,
) -> Result<(Vec<ContentBlock>, bool), ProjectionError> {
    match outcome {
        ToolOutcome::Success { content } => Ok((content.clone(), false)),
        ToolOutcome::Error {
            code,
            message,
            content,
        } => {
            let mut projected = Vec::with_capacity(content.len() + 1);
            projected.push(ContentBlock::text(format!(
                "Tool error [{code}]: {message}"
            )));
            projected.extend(content.clone());
            Ok((projected, true))
        }
        ToolOutcome::Denied { reason } => Ok((
            vec![ContentBlock::text(format!(
                "Tool execution denied: {reason}"
            ))],
            true,
        )),
        ToolOutcome::Cancelled { cause } => Ok((
            vec![ContentBlock::text(format!(
                "Tool execution cancelled: {}",
                cancel_cause_label(*cause)
            ))],
            true,
        )),
        ToolOutcome::Unknown { reason } => Ok((
            vec![ContentBlock::text(format!(
                "Tool execution outcome is unknown: {reason}"
            ))],
            true,
        )),
        _ => Err(ProjectionError::Unsupported(
            "ToolOutcome variant is not supported by projection version 1".to_owned(),
        )),
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

fn invalid_event_at(event: &SessionEvent, reason: &str) -> ProjectionError {
    ProjectionError::InvalidEvent(format!(
        "seq {} ({}): {reason}",
        event.seq(),
        event.payload().event_type()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssistantMessage, InboxClaimed, InboxEnqueued, ModelRequested, NewSessionEvent,
        RecoveryBlockKind, RecoveryResolved, SessionCreated, StepEnded, StepStarted,
        ToolDispatched, ToolResultRecorded, TurnEnded, TurnStarted, UserMessage,
    };
    use harness_types::{BlobId, BlobRef, SessionId, Sha256Digest, Timestamp};

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

    fn turn(value: u64) -> TurnNo {
        TurnNo::new(value).unwrap()
    }

    fn step(value: u64) -> StepNo {
        StepNo::new(value).unwrap()
    }

    fn ts() -> Timestamp {
        Timestamp::parse("2026-08-19T13:00:00Z").unwrap()
    }

    fn blob() -> BlobRef {
        BlobRef {
            id: BlobId::new("blob_request").unwrap(),
            sha256: Sha256Digest::new("0".repeat(64)).unwrap(),
            size: 2,
            media_type: Some("application/json".to_owned()),
        }
    }

    fn commit(
        session_id: &SessionId,
        seq_no: u64,
        payload: SessionEventPayload,
        position: Option<(TurnNo, Option<StepNo>)>,
    ) -> SessionEvent {
        let mut draft = NewSessionEvent::new(id(&format!("evt_{seq_no}")), ts(), payload);
        if let Some((turn_no, step_no)) = position {
            draft = match step_no {
                Some(step_no) => draft.in_step(turn_no, step_no),
                None => draft.in_turn(turn_no),
            };
        }
        SessionEvent::committed(session_id.clone(), seq(seq_no), draft).unwrap()
    }

    fn user_message(message_id: &str, text: &str) -> Message {
        Message {
            id: id(message_id),
            role: Role::User,
            source: MessageSource::user(),
            content: vec![ContentBlock::text(text)],
        }
    }

    fn assistant_tool_call_message(
        message_id: &str,
        request_provider: &str,
        tool_call_id: &str,
        arguments: &str,
    ) -> Message {
        Message {
            id: id(message_id),
            role: Role::Assistant,
            source: MessageSource::model(id(request_provider), "model-x"),
            content: vec![ContentBlock::ToolCall {
                id: id(tool_call_id),
                name: "read_file".to_owned(),
                arguments_json: JsonText::new(arguments.to_owned()).unwrap(),
            }],
        }
    }

    #[test]
    fn projects_inbox_lifecycle_and_model_visible_tool_result() {
        let session_id: SessionId = id("ses_1");
        let user = user_message("msg_user", "read foo.txt");
        let assistant = assistant_tool_call_message(
            "msg_assistant",
            "prv_llm",
            "call_1",
            r#"{"path":"foo.txt"}"#,
        );
        let events = vec![
            commit(
                &session_id,
                1,
                SessionEventPayload::SessionCreated(SessionCreated::default()),
                None,
            ),
            commit(
                &session_id,
                2,
                SessionEventPayload::InboxEnqueued(InboxEnqueued {
                    message: user.clone(),
                    target: InboxTarget::NextTurn,
                    wakeup: true,
                }),
                None,
            ),
            commit(
                &session_id,
                3,
                SessionEventPayload::TurnStarted(TurnStarted { turn: turn(1) }),
                Some((turn(1), None)),
            ),
            commit(
                &session_id,
                4,
                SessionEventPayload::InboxClaimed(InboxClaimed {
                    message_id: user.id.clone(),
                    target: InboxTarget::NextTurn,
                }),
                Some((turn(1), None)),
            ),
            commit(
                &session_id,
                5,
                SessionEventPayload::StepStarted(StepStarted {
                    turn: turn(1),
                    step: step(1),
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                6,
                SessionEventPayload::UserMessage(UserMessage {
                    message: user.clone(),
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                7,
                SessionEventPayload::ModelRequested(ModelRequested {
                    request_id: id("req_1"),
                    provider: id("prv_llm"),
                    model: "model-x".to_owned(),
                    history_through_seq: seq(6),
                    request_snapshot: blob(),
                    attempt: 1,
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                8,
                SessionEventPayload::AssistantMessage(AssistantMessage {
                    request_id: id("req_1"),
                    message: assistant.clone(),
                    usage: None,
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                9,
                SessionEventPayload::ToolCall(ToolCallRecorded {
                    call_id: id("call_1"),
                    tool: "read_file".to_owned(),
                    arguments_json: JsonText::new(r#"{"path":"foo.txt"}"#.to_owned()).unwrap(),
                    side_effect: SideEffectClass::ReadOnly,
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                10,
                SessionEventPayload::ToolDispatched(ToolDispatched {
                    call_id: id("call_1"),
                    invocation_id: id("inv_1"),
                    provider_id: id("prv_tools"),
                    attempt: 1,
                    idempotency_key: id("idem_1"),
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                11,
                SessionEventPayload::ToolResult(ToolResultRecorded {
                    call_id: id("call_1"),
                    invocation_id: id("inv_1"),
                    outcome: ToolOutcome::Success {
                        content: vec![ContentBlock::text("hello")],
                    },
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                12,
                SessionEventPayload::StepEnded(StepEnded {
                    reason: StepEndReason::Completed,
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                13,
                SessionEventPayload::TurnEnded(TurnEnded {
                    reason: TurnEndReason::Completed,
                }),
                Some((turn(1), None)),
            ),
        ];

        let projection = V1SessionProjector.project(&events).unwrap();
        assert!(projection.inbox.is_empty());
        assert_eq!(projection.model_messages.len(), 3);
        assert_eq!(projection.model_messages[0], user);
        assert_eq!(projection.model_messages[1], assistant);

        let tool_result_message = &projection.model_messages[2];
        assert_eq!(tool_result_message.role, Role::User);
        assert_eq!(tool_result_message.source, MessageSource::plugin());
        match &tool_result_message.content[0] {
            ContentBlock::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_call_id, &ToolCallId::new("call_1").unwrap());
                assert_eq!(content, &vec![ContentBlock::text("hello")]);
                assert!(!is_error);
            }
            other => panic!("expected tool-result block, got {other:?}"),
        }

        assert_eq!(projection.lifecycle.open_turn, None);
        assert_eq!(projection.lifecycle.open_step, None);
        assert_eq!(projection.lifecycle.last_ended_turn, Some(turn(1)));
        assert_eq!(
            projection.lifecycle.last_ended_step,
            Some(StepPosition {
                turn: turn(1),
                step: step(1),
            })
        );
        assert!(projection.pending_model_request.is_none());
        assert!(projection.pending_tool_calls.is_empty());
        assert!(projection.unresolved_recovery.is_none());
    }

    #[test]
    fn keeps_unclaimed_inbox_items_in_fifo_order() {
        let session_id: SessionId = id("ses_1");
        let first = user_message("msg_1", "first");
        let second = user_message("msg_2", "second");
        let events = vec![
            commit(
                &session_id,
                1,
                SessionEventPayload::SessionCreated(SessionCreated::default()),
                None,
            ),
            commit(
                &session_id,
                2,
                SessionEventPayload::InboxEnqueued(InboxEnqueued {
                    message: first.clone(),
                    target: InboxTarget::NextTurn,
                    wakeup: true,
                }),
                None,
            ),
            commit(
                &session_id,
                3,
                SessionEventPayload::InboxEnqueued(InboxEnqueued {
                    message: second.clone(),
                    target: InboxTarget::NextTurn,
                    wakeup: true,
                }),
                None,
            ),
        ];

        let projection = V1SessionProjector.project(&events).unwrap();
        assert_eq!(projection.inbox.next_turn.len(), 2);
        assert_eq!(projection.inbox.next_turn[0].message, first);
        assert_eq!(projection.inbox.next_turn[1].message, second);
    }

    #[test]
    fn rejects_sequence_gaps() {
        let session_id: SessionId = id("ses_1");
        let events = vec![
            commit(
                &session_id,
                1,
                SessionEventPayload::SessionCreated(SessionCreated::default()),
                None,
            ),
            commit(
                &session_id,
                3,
                SessionEventPayload::InboxEnqueued(InboxEnqueued {
                    message: user_message("msg_1", "hello"),
                    target: InboxTarget::NextTurn,
                    wakeup: true,
                }),
                None,
            ),
        ];

        assert!(matches!(
            V1SessionProjector.project(&events),
            Err(ProjectionError::InvalidSequence(_))
        ));
    }

    #[test]
    fn error_outcome_is_projected_as_model_visible_error() {
        let outcome = ToolOutcome::Error {
            code: "EACCES".to_owned(),
            message: "permission denied".to_owned(),
            content: vec![ContentBlock::text("provider detail")],
        };
        let (content, is_error) = project_tool_outcome(&outcome).unwrap();
        assert!(is_error);
        assert_eq!(
            content,
            vec![
                ContentBlock::text("Tool error [EACCES]: permission denied"),
                ContentBlock::text("provider detail"),
            ]
        );
    }

    #[test]
    fn late_reconciled_tool_result_must_precede_recovery_resolution() {
        let session_id: SessionId = id("ses_1");
        let user = user_message("msg_user", "send it");
        let assistant = Message {
            id: id("msg_assistant"),
            role: Role::Assistant,
            source: MessageSource::model(id("prv_llm"), "model-x"),
            content: vec![ContentBlock::ToolCall {
                id: id("call_send"),
                name: "send_email".to_owned(),
                arguments_json: JsonText::new(r#"{"to":"a@example.com"}"#.to_owned()).unwrap(),
            }],
        };

        let mut events = vec![
            commit(
                &session_id,
                1,
                SessionEventPayload::SessionCreated(SessionCreated::default()),
                None,
            ),
            commit(
                &session_id,
                2,
                SessionEventPayload::TurnStarted(TurnStarted { turn: turn(1) }),
                Some((turn(1), None)),
            ),
            commit(
                &session_id,
                3,
                SessionEventPayload::StepStarted(StepStarted {
                    turn: turn(1),
                    step: step(1),
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                4,
                SessionEventPayload::UserMessage(UserMessage { message: user }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                5,
                SessionEventPayload::ModelRequested(ModelRequested {
                    request_id: id("req_1"),
                    provider: id("prv_llm"),
                    model: "model-x".to_owned(),
                    history_through_seq: seq(4),
                    request_snapshot: blob(),
                    attempt: 1,
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                6,
                SessionEventPayload::AssistantMessage(AssistantMessage {
                    request_id: id("req_1"),
                    message: assistant,
                    usage: None,
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                7,
                SessionEventPayload::ToolCall(ToolCallRecorded {
                    call_id: id("call_send"),
                    tool: "send_email".to_owned(),
                    arguments_json: JsonText::new(r#"{"to":"a@example.com"}"#.to_owned()).unwrap(),
                    side_effect: SideEffectClass::NonIdempotentWrite,
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                8,
                SessionEventPayload::ToolDispatched(ToolDispatched {
                    call_id: id("call_send"),
                    invocation_id: id("inv_send"),
                    provider_id: id("prv_tools"),
                    attempt: 1,
                    idempotency_key: id("idem_send"),
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                9,
                SessionEventPayload::RecoveryBlocked(RecoveryBlocked {
                    kind: RecoveryBlockKind::UnknownToolOutcome,
                    call_id: id("call_send"),
                    invocation_id: id("inv_send"),
                    reason: "provider exited after dispatch".to_owned(),
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                10,
                SessionEventPayload::StepEnded(StepEnded {
                    reason: StepEndReason::Blocked,
                }),
                Some((turn(1), Some(step(1)))),
            ),
            commit(
                &session_id,
                11,
                SessionEventPayload::TurnEnded(TurnEnded {
                    reason: TurnEndReason::Blocked,
                }),
                Some((turn(1), None)),
            ),
        ];

        let blocked = V1SessionProjector.project(&events).unwrap();
        assert!(blocked.unresolved_recovery.is_some());
        assert!(blocked.pending_tool_calls.contains_key(&id("call_send")));
        assert!(
            blocked
                .pending_tool_dispatches
                .contains_key(&id("call_send"))
        );

        events.push(commit(
            &session_id,
            12,
            SessionEventPayload::RecoveryResolved(RecoveryResolved {
                blocked_event_id: id("evt_9"),
                resolution: "confirmed-success".to_owned(),
                note: None,
            }),
            None,
        ));
        assert!(matches!(
            V1SessionProjector.project(&events),
            Err(ProjectionError::InvalidEvent(_))
        ));
        events.pop();

        events.push(commit(
            &session_id,
            12,
            SessionEventPayload::ToolResult(ToolResultRecorded {
                call_id: id("call_send"),
                invocation_id: id("inv_send"),
                outcome: ToolOutcome::Success {
                    content: vec![ContentBlock::text("reconciled as delivered")],
                },
            }),
            Some((turn(1), Some(step(1)))),
        ));
        events.push(commit(
            &session_id,
            13,
            SessionEventPayload::RecoveryResolved(RecoveryResolved {
                blocked_event_id: id("evt_9"),
                resolution: "confirmed-success".to_owned(),
                note: Some("provider reconciliation".to_owned()),
            }),
            None,
        ));

        let resolved = V1SessionProjector.project(&events).unwrap();
        assert!(resolved.unresolved_recovery.is_none());
        assert!(resolved.pending_tool_calls.is_empty());
        assert!(resolved.pending_tool_dispatches.is_empty());
        assert_eq!(resolved.model_messages.len(), 3);
    }
}
