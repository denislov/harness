mod event;
mod projector;
mod store;

pub use event::{
    ApprovalRequested, ApprovalResolved, AssistantMessage, EventValidationError, InboxClaimed,
    InboxDiscarded, InboxEnqueued, ModelFailed, ModelRequested, NewSessionEvent, RecoveryBlockKind,
    RecoveryBlocked, RecoveryResolved, SESSION_EVENT_SCHEMA_VERSION, SessionCreated, SessionEvent,
    SessionEventPayload, StepEndReason, StepEnded, StepStarted, ToolCallRecorded, ToolDispatched,
    ToolResultRecorded, TurnEndReason, TurnEnded, TurnStarted, UserMessage,
};
pub use projector::{
    InboxProjection, LifecycleProjection, OpenStepToolCall, OpenStepToolProjection,
    PendingApproval, PendingInboxItem, PendingToolCall, PendingToolDispatch, ProjectionError,
    RecoveryBlock, SESSION_PROJECTION_VERSION_V1, SessionProjection, SessionProjector,
    StepPosition, ToolApprovalResolution, V1SessionProjector,
};
pub use store::{
    AppendResult, CreateSession, ForkSession, SessionHead, SessionStore, SessionStoreError,
};
