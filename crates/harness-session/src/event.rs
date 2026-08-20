use std::collections::BTreeMap;

use harness_types::{
    ApprovalDecision, ApprovalId, BlobRef, EventId, EventSeq, IdempotencyKey, InboxTarget,
    InvocationId, Message, MessageId, PortableError, ProviderId, RequestId, SessionId,
    SideEffectClass, StepNo, Timestamp, TokenUsage, ToolCallId, ToolOutcome, TurnNo,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const SESSION_EVENT_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct SessionEvent {
    schema_version: u16,
    event_id: EventId,
    session_id: SessionId,
    seq: EventSeq,
    timestamp: Timestamp,
    turn: Option<TurnNo>,
    step: Option<StepNo>,
    payload: SessionEventPayload,
}

impl Serialize for SessionEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("schemaVersion", &self.schema_version)?;
        map.serialize_entry("eventId", &self.event_id)?;
        map.serialize_entry("sessionId", &self.session_id)?;
        map.serialize_entry("seq", &self.seq)?;
        map.serialize_entry("time", &self.timestamp)?;
        if let Some(turn) = self.turn {
            map.serialize_entry("turn", &turn)?;
        }
        if let Some(step) = self.step {
            map.serialize_entry("step", &step)?;
        }
        map.serialize_entry("type", self.payload.event_type())?;
        self.payload.serialize_data(&mut map)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for SessionEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireEvent {
            schema_version: u16,
            event_id: EventId,
            session_id: SessionId,
            seq: EventSeq,
            #[serde(rename = "time")]
            timestamp: Timestamp,
            #[serde(default)]
            turn: Option<TurnNo>,
            #[serde(default)]
            step: Option<StepNo>,
            #[serde(rename = "type")]
            event_type: String,
            data: Value,
        }

        let wire = WireEvent::deserialize(deserializer)?;
        let payload_json = serde_json::json!({
            "type": wire.event_type,
            "data": wire.data,
        });
        let payload = serde_json::from_value(payload_json).map_err(D::Error::custom)?;

        let event = Self {
            schema_version: wire.schema_version,
            event_id: wire.event_id,
            session_id: wire.session_id,
            seq: wire.seq,
            timestamp: wire.timestamp,
            turn: wire.turn,
            step: wire.step,
            payload,
        };
        event.validate().map_err(D::Error::custom)?;
        Ok(event)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewSessionEvent {
    event_id: EventId,
    timestamp: Timestamp,
    turn: Option<TurnNo>,
    step: Option<StepNo>,
    payload: SessionEventPayload,
}

impl NewSessionEvent {
    pub fn new(event_id: EventId, timestamp: Timestamp, payload: SessionEventPayload) -> Self {
        Self {
            event_id,
            timestamp,
            turn: None,
            step: None,
            payload,
        }
    }

    pub fn in_turn(mut self, turn: TurnNo) -> Self {
        self.turn = Some(turn);
        self
    }

    pub fn in_step(mut self, turn: TurnNo, step: StepNo) -> Self {
        self.turn = Some(turn);
        self.step = Some(step);
        self
    }

    pub fn event_id(&self) -> &EventId {
        &self.event_id
    }

    pub fn timestamp(&self) -> Timestamp {
        self.timestamp
    }

    pub fn turn(&self) -> Option<TurnNo> {
        self.turn
    }

    pub fn step(&self) -> Option<StepNo> {
        self.step
    }

    pub fn payload(&self) -> &SessionEventPayload {
        &self.payload
    }

    pub fn validate(&self) -> Result<(), EventValidationError> {
        validate_event_shape(self.turn, self.step, &self.payload)
    }
}

impl SessionEvent {
    pub fn committed(
        session_id: SessionId,
        seq: EventSeq,
        draft: NewSessionEvent,
    ) -> Result<Self, EventValidationError> {
        draft.validate()?;
        Ok(Self {
            schema_version: SESSION_EVENT_SCHEMA_VERSION,
            event_id: draft.event_id,
            session_id,
            seq,
            timestamp: draft.timestamp,
            turn: draft.turn,
            step: draft.step,
            payload: draft.payload,
        })
    }

    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    pub fn event_id(&self) -> &EventId {
        &self.event_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn seq(&self) -> EventSeq {
        self.seq
    }

    pub const fn timestamp(&self) -> Timestamp {
        self.timestamp
    }

    pub const fn turn(&self) -> Option<TurnNo> {
        self.turn
    }

    pub const fn step(&self) -> Option<StepNo> {
        self.step
    }

    pub fn payload(&self) -> &SessionEventPayload {
        &self.payload
    }

    pub fn validate(&self) -> Result<(), EventValidationError> {
        if self.schema_version != SESSION_EVENT_SCHEMA_VERSION {
            return Err(EventValidationError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        validate_event_shape(self.turn, self.step, &self.payload)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[non_exhaustive]
pub enum SessionEventPayload {
    #[serde(rename = "session/created")]
    SessionCreated(SessionCreated),
    #[serde(rename = "inbox/enqueued")]
    InboxEnqueued(InboxEnqueued),
    #[serde(rename = "inbox/claimed")]
    InboxClaimed(InboxClaimed),
    #[serde(rename = "inbox/discarded")]
    InboxDiscarded(InboxDiscarded),
    #[serde(rename = "turn/started")]
    TurnStarted(TurnStarted),
    #[serde(rename = "step/started")]
    StepStarted(StepStarted),
    #[serde(rename = "user/message")]
    UserMessage(UserMessage),
    #[serde(rename = "model/requested")]
    ModelRequested(ModelRequested),
    #[serde(rename = "model/failed")]
    ModelFailed(ModelFailed),
    #[serde(rename = "assistant/message")]
    AssistantMessage(AssistantMessage),
    #[serde(rename = "tool/call")]
    ToolCall(ToolCallRecorded),
    #[serde(rename = "approval/requested")]
    ApprovalRequested(ApprovalRequested),
    #[serde(rename = "approval/resolved")]
    ApprovalResolved(ApprovalResolved),
    #[serde(rename = "tool/dispatched")]
    ToolDispatched(ToolDispatched),
    #[serde(rename = "tool/result")]
    ToolResult(ToolResultRecorded),
    #[serde(rename = "step/ended")]
    StepEnded(StepEnded),
    #[serde(rename = "turn/ended")]
    TurnEnded(TurnEnded),
    #[serde(rename = "recovery/blocked")]
    RecoveryBlocked(RecoveryBlocked),
    #[serde(rename = "recovery/resolved")]
    RecoveryResolved(RecoveryResolved),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionCreated {
    #[serde(default, flatten)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxEnqueued {
    pub message: Message,
    pub target: InboxTarget,
    pub wakeup: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxClaimed {
    pub message_id: MessageId,
    pub target: InboxTarget,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxDiscarded {
    pub message_id: MessageId,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStarted {
    pub turn: TurnNo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepStarted {
    pub turn: TurnNo,
    pub step: StepNo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    pub message: Message,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequested {
    pub request_id: RequestId,
    pub provider: ProviderId,
    pub model: String,
    pub history_through_seq: EventSeq,
    pub request_snapshot: BlobRef,
    pub attempt: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFailed {
    pub request_id: RequestId,
    pub failure: PortableError,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub request_id: RequestId,
    pub message: Message,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallRecorded {
    pub call_id: ToolCallId,
    pub tool: String,
    pub arguments_json: harness_types::JsonText,
    pub side_effect: SideEffectClass,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequested {
    pub approval_id: ApprovalId,
    pub call_id: ToolCallId,
    pub reason: String,
    pub risk: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResolved {
    pub approval_id: ApprovalId,
    pub call_id: ToolCallId,
    pub decision: ApprovalDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Durable provider-dispatch boundary for a concrete Tool attempt.
///
/// This event MUST be committed before Core allows the provider call to cross
/// the process/capability boundary. Its presence therefore means the external
/// effect may have occurred, even if no `tool/result` was committed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDispatched {
    pub call_id: ToolCallId,
    pub invocation_id: InvocationId,
    pub provider_id: ProviderId,
    pub attempt: u32,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultRecorded {
    pub call_id: ToolCallId,
    pub invocation_id: InvocationId,
    pub outcome: ToolOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum StepEndReason {
    Completed,
    ToolContinuation,
    ModelError,
    Cancelled,
    Blocked,
    MaxTokens,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepEnded {
    pub reason: StepEndReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum TurnEndReason {
    Completed,
    Blocked,
    Cancelled,
    Error,
    MaxTokens,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnEnded {
    pub reason: TurnEndReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RecoveryBlockKind {
    UnknownToolOutcome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryBlocked {
    pub kind: RecoveryBlockKind,
    pub call_id: ToolCallId,
    pub invocation_id: InvocationId,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryResolved {
    pub blocked_event_id: EventId,
    pub resolution: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EventValidationError {
    #[error("unsupported SessionEvent schema version {0}")]
    UnsupportedSchemaVersion(u16),

    #[error("step may not be set when turn is absent")]
    StepWithoutTurn,

    #[error("event {event_type} requires a turn")]
    TurnRequired { event_type: &'static str },

    #[error("event {event_type} requires a step")]
    StepRequired { event_type: &'static str },

    #[error("event {event_type} may not carry a turn")]
    TurnForbidden { event_type: &'static str },

    #[error("event {event_type} may not carry a step")]
    StepForbidden { event_type: &'static str },

    #[error("event envelope turn does not match payload turn")]
    TurnMismatch,

    #[error("event envelope step does not match payload step")]
    StepMismatch,
}

fn validate_event_shape(
    turn: Option<TurnNo>,
    step: Option<StepNo>,
    payload: &SessionEventPayload,
) -> Result<(), EventValidationError> {
    if step.is_some() && turn.is_none() {
        return Err(EventValidationError::StepWithoutTurn);
    }

    match payload {
        SessionEventPayload::SessionCreated(_)
        | SessionEventPayload::InboxEnqueued(_)
        | SessionEventPayload::InboxDiscarded(_)
        | SessionEventPayload::RecoveryResolved(_) => {
            if turn.is_some() {
                return Err(EventValidationError::TurnForbidden {
                    event_type: payload.event_type(),
                });
            }
            if step.is_some() {
                return Err(EventValidationError::StepForbidden {
                    event_type: payload.event_type(),
                });
            }
        }
        SessionEventPayload::TurnStarted(data) => {
            let envelope_turn = turn.ok_or(EventValidationError::TurnRequired {
                event_type: payload.event_type(),
            })?;
            if envelope_turn != data.turn {
                return Err(EventValidationError::TurnMismatch);
            }
            if step.is_some() {
                return Err(EventValidationError::StepForbidden {
                    event_type: payload.event_type(),
                });
            }
        }
        SessionEventPayload::StepStarted(data) => {
            let envelope_turn = turn.ok_or(EventValidationError::TurnRequired {
                event_type: payload.event_type(),
            })?;
            let envelope_step = step.ok_or(EventValidationError::StepRequired {
                event_type: payload.event_type(),
            })?;
            if envelope_turn != data.turn {
                return Err(EventValidationError::TurnMismatch);
            }
            if envelope_step != data.step {
                return Err(EventValidationError::StepMismatch);
            }
        }
        SessionEventPayload::UserMessage(_)
        | SessionEventPayload::ModelRequested(_)
        | SessionEventPayload::ModelFailed(_)
        | SessionEventPayload::AssistantMessage(_)
        | SessionEventPayload::ToolCall(_)
        | SessionEventPayload::ApprovalRequested(_)
        | SessionEventPayload::ApprovalResolved(_)
        | SessionEventPayload::ToolDispatched(_)
        | SessionEventPayload::ToolResult(_)
        | SessionEventPayload::StepEnded(_) => {
            turn.ok_or(EventValidationError::TurnRequired {
                event_type: payload.event_type(),
            })?;
            step.ok_or(EventValidationError::StepRequired {
                event_type: payload.event_type(),
            })?;
        }
        SessionEventPayload::InboxClaimed(_) => {
            turn.ok_or(EventValidationError::TurnRequired {
                event_type: payload.event_type(),
            })?;
        }
        SessionEventPayload::RecoveryBlocked(_) => {
            turn.ok_or(EventValidationError::TurnRequired {
                event_type: payload.event_type(),
            })?;
            step.ok_or(EventValidationError::StepRequired {
                event_type: payload.event_type(),
            })?;
        }
        SessionEventPayload::TurnEnded(_) => {
            turn.ok_or(EventValidationError::TurnRequired {
                event_type: payload.event_type(),
            })?;
            if step.is_some() {
                return Err(EventValidationError::StepForbidden {
                    event_type: payload.event_type(),
                });
            }
        }
    }

    Ok(())
}

impl SessionEventPayload {
    fn serialize_data<M>(&self, map: &mut M) -> Result<(), M::Error>
    where
        M: serde::ser::SerializeMap,
    {
        match self {
            Self::SessionCreated(data) => map.serialize_entry("data", data),
            Self::InboxEnqueued(data) => map.serialize_entry("data", data),
            Self::InboxClaimed(data) => map.serialize_entry("data", data),
            Self::InboxDiscarded(data) => map.serialize_entry("data", data),
            Self::TurnStarted(data) => map.serialize_entry("data", data),
            Self::StepStarted(data) => map.serialize_entry("data", data),
            Self::UserMessage(data) => map.serialize_entry("data", data),
            Self::ModelRequested(data) => map.serialize_entry("data", data),
            Self::ModelFailed(data) => map.serialize_entry("data", data),
            Self::AssistantMessage(data) => map.serialize_entry("data", data),
            Self::ToolCall(data) => map.serialize_entry("data", data),
            Self::ApprovalRequested(data) => map.serialize_entry("data", data),
            Self::ApprovalResolved(data) => map.serialize_entry("data", data),
            Self::ToolDispatched(data) => map.serialize_entry("data", data),
            Self::ToolResult(data) => map.serialize_entry("data", data),
            Self::StepEnded(data) => map.serialize_entry("data", data),
            Self::TurnEnded(data) => map.serialize_entry("data", data),
            Self::RecoveryBlocked(data) => map.serialize_entry("data", data),
            Self::RecoveryResolved(data) => map.serialize_entry("data", data),
        }
    }
}

impl SessionEventPayload {
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::SessionCreated(_) => "session/created",
            Self::InboxEnqueued(_) => "inbox/enqueued",
            Self::InboxClaimed(_) => "inbox/claimed",
            Self::InboxDiscarded(_) => "inbox/discarded",
            Self::TurnStarted(_) => "turn/started",
            Self::StepStarted(_) => "step/started",
            Self::UserMessage(_) => "user/message",
            Self::ModelRequested(_) => "model/requested",
            Self::ModelFailed(_) => "model/failed",
            Self::AssistantMessage(_) => "assistant/message",
            Self::ToolCall(_) => "tool/call",
            Self::ApprovalRequested(_) => "approval/requested",
            Self::ApprovalResolved(_) => "approval/resolved",
            Self::ToolDispatched(_) => "tool/dispatched",
            Self::ToolResult(_) => "tool/result",
            Self::StepEnded(_) => "step/ended",
            Self::TurnEnded(_) => "turn/ended",
            Self::RecoveryBlocked(_) => "recovery/blocked",
            Self::RecoveryResolved(_) => "recovery/resolved",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_types::{MessageSource, Role};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    #[test]
    fn event_serializes_to_canonical_envelope() {
        let event = SessionEvent {
            schema_version: SESSION_EVENT_SCHEMA_VERSION,
            event_id: id("evt_1"),
            session_id: id("ses_1"),
            seq: EventSeq::FIRST,
            timestamp: Timestamp::parse("2026-08-19T13:00:00Z").unwrap(),
            turn: None,
            step: None,
            payload: SessionEventPayload::InboxEnqueued(InboxEnqueued {
                message: Message {
                    id: id("msg_1"),
                    role: Role::User,
                    source: MessageSource::user(),
                    content: vec![],
                },
                target: InboxTarget::NextTurn,
                wakeup: true,
            }),
        };

        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["type"], "inbox/enqueued");
        assert_eq!(value["data"]["target"], "next-turn");
        assert!(value.get("payload").is_none());
    }
}
