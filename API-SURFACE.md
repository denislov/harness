# Batch 01 Public API Surface

## `harness-types`

### Identifiers

```text
SessionId
AgentInstanceId
EventId
MessageId
RequestId
ToolCallId
InvocationId
ProviderId
BlobId
```

All IDs are opaque non-empty UTF-8 strings. Prefixes remain conventions only.

### Ordered counters

```text
EventSeq
TurnNo
StepNo
```

All enforce the JavaScript safe-integer ceiling required by Spec v0.1.

### Durable/cross-subsystem values

```text
Timestamp
JsonText
BlobRef
Sha256Digest
Message
MessageSource
Role
ContentBlock
InboxTarget
SideEffectClass
CancelCause
ToolOutcome
TokenUsage
ErrorCode
PortableError
```

`JsonText` validates that the value contains exactly one JSON value while retaining the original text byte-for-byte. On the wire it is still serialized as a JSON string (`argumentsJson`).

## `harness-session`

### Events

```text
NewSessionEvent
SessionEvent
SessionEventPayload
```

`NewSessionEvent` is uncommitted and has no `SessionId` or `EventSeq`. `SessionEvent` is the immutable committed form.

Current payload variants match the v0.1 event taxonomy:

```text
session/created
inbox/enqueued
inbox/claimed
inbox/discarded
turn/started
step/started
user/message
model/requested
model/failed
assistant/message
tool/call
tool/result
step/ended
turn/ended
recovery/blocked
recovery/resolved
```

### Storage contract

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create(&self, request: CreateSession) -> Result<SessionEvent, SessionStoreError>;

    async fn append(
        &self,
        session_id: &SessionId,
        expected_seq: EventSeq,
        events: Vec<NewSessionEvent>,
    ) -> Result<AppendResult, SessionStoreError>;

    async fn read(
        &self,
        session_id: &SessionId,
        from_seq: EventSeq,
        limit: usize,
    ) -> Result<Vec<SessionEvent>, SessionStoreError>;

    async fn head(&self, session_id: &SessionId) -> Result<SessionHead, SessionStoreError>;

    async fn fork(&self, request: ForkSession) -> Result<SessionHead, SessionStoreError>;
}
```

`read` is defined as inclusive: `seq >= from_seq`.

### Projection contract

```text
SessionProjector
SessionProjection
InboxProjection
RecoveryBlock
```

The concrete v0.1 projector is deferred until the model-visible ToolResult mapping is frozen.

# Public API Surface - Batch 02

## `harness-storage-local`

```rust
pub struct MemorySessionStore { /* private */ }

impl MemorySessionStore {
    pub fn new() -> Self;
}

impl Default for MemorySessionStore;
impl Clone for MemorySessionStore;
impl SessionStore for MemorySessionStore;
```

No storage-specific extension methods are exposed in Batch 02. Callers use the existing `harness_session::SessionStore` contract:

```rust
async fn create(&self, request: CreateSession)
    -> Result<SessionEvent, SessionStoreError>;

async fn append(
    &self,
    session_id: &SessionId,
    expected_seq: EventSeq,
    events: Vec<NewSessionEvent>,
) -> Result<AppendResult, SessionStoreError>;

async fn read(
    &self,
    session_id: &SessionId,
    from_seq: EventSeq,
    limit: usize,
) -> Result<Vec<SessionEvent>, SessionStoreError>;

async fn head(&self, session_id: &SessionId)
    -> Result<SessionHead, SessionStoreError>;

async fn fork(&self, request: ForkSession)
    -> Result<SessionHead, SessionStoreError>;
```

Keeping the concrete backend surface this small is intentional. Agent/runtime code should be written against `SessionStore`, not against in-memory implementation details.

# Batch 03 API Surface

## Projection version

```rust
pub const SESSION_PROJECTION_VERSION_V1: u16 = 1;

pub trait SessionProjector: Send + Sync {
    fn version(&self) -> u16;
    fn project(&self, events: &[SessionEvent])
        -> Result<SessionProjection, ProjectionError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct V1SessionProjector;
```

## SessionProjection

```rust
pub struct SessionProjection {
    pub model_messages: Vec<Message>,
    pub inbox: InboxProjection,
    pub lifecycle: LifecycleProjection,
    pub last_model_request: Option<ModelRequested>,
    pub pending_model_request: Option<ModelRequested>,
    pub pending_tool_calls: BTreeMap<ToolCallId, PendingToolCall>,
    pub unresolved_recovery: Option<RecoveryBlock>,
}
```

`last_model_request` is historical context. `pending_model_request` is operational state and is cleared by the matching `assistant/message` or `model/failed` event.

## LifecycleProjection

```rust
pub struct StepPosition {
    pub turn: TurnNo,
    pub step: StepNo,
}

pub struct LifecycleProjection {
    pub open_turn: Option<TurnNo>,
    pub open_step: Option<StepPosition>,
    pub last_started_turn: Option<TurnNo>,
    pub last_ended_turn: Option<TurnNo>,
    pub last_started_step: Option<StepPosition>,
    pub last_ended_step: Option<StepPosition>,
}
```

An open turn/step at the end of a committed log is not automatically corruption. It can represent a process crash between durable boundaries.

## Pending ToolCall

```rust
pub struct PendingToolCall {
    pub call_event_id: EventId,
    pub turn: TurnNo,
    pub step: StepNo,
    pub data: ToolCallRecorded,
}
```

A ToolCall enters this map at `tool/call` and leaves it only after an authoritative `tool/result`.

## RecoveryBlock

```rust
pub struct RecoveryBlock {
    pub blocked_event_id: EventId,
    pub turn: TurnNo,
    pub step: StepNo,
    pub data: RecoveryBlocked,
}
```

The turn/step coordinates are retained because a reconciled `tool/result` may arrive after the blocked Step/Turn has already closed.

## Inbox helpers

```rust
impl InboxProjection {
    pub fn is_empty(&self) -> bool;
    pub fn has_work(&self) -> bool;
}
```

The queues remain public v0.1 state so the later Agent Actor can implement the batching rule without an extra abstraction layer.

# Batch 04 Public API Surface

## `harness-types`

New identifier:

```rust
pub struct IdempotencyKey(String);
```

It has the same opaque identifier contract as the existing branded IDs.

## `harness-session`

New durable event payload:

```rust
pub struct ToolDispatched {
    pub call_id: ToolCallId,
    pub invocation_id: InvocationId,
    pub provider_id: ProviderId,
    pub attempt: u32,
    pub idempotency_key: IdempotencyKey,
}
```

New `SessionEventPayload` variant:

```rust
ToolDispatched(ToolDispatched)
```

Canonical wire type:

```text
tool/dispatched
```

Projection changes:

```rust
pub struct PendingToolCall {
    pub call_event_id: EventId,
    pub call_seq: EventSeq,
    pub turn: TurnNo,
    pub step: StepNo,
    pub data: ToolCallRecorded,
}

pub struct PendingToolDispatch {
    pub dispatch_event_id: EventId,
    pub dispatch_seq: EventSeq,
    pub turn: TurnNo,
    pub step: StepNo,
    pub data: ToolDispatched,
}

pub struct SessionProjection {
    // existing fields ...
    pub pending_tool_calls: BTreeMap<ToolCallId, PendingToolCall>,
    pub pending_tool_dispatches: BTreeMap<ToolCallId, PendingToolDispatch>,
    // ...
}
```

Projector v1 now validates:

- first dispatch attempt is `1`;
- retry attempts increment exactly by one;
- invocation IDs cannot be reused across logical calls;
- retry preserves `providerId` and `idempotencyKey`;
- non-idempotent writes cannot be redispatched automatically;
- `tool/result` must match the latest dispatch when a dispatch exists;
- success/unknown outcomes require a dispatch;
- `recovery/blocked` must reference the active durable dispatch.

## `harness-agent`

### Commands

```rust
pub enum AgentCommand {
    Send {
        message: Message,
        target: InboxTarget,
        wakeup: bool,
    },
    Cancel {
        cause: CancelCause,
        keep_inbox: bool,
    },
    Shutdown,
}
```

Convenience constructors:

```rust
AgentCommand::followup(message)
AgentCommand::steer(message)
AgentCommand::inject(message)
AgentCommand::cancel(cause, keep_inbox)
```

### Recovery classification

```rust
pub enum DurableCursor {
    Quiescent,
    OpenTurn { turn: TurnNo },
    OpenStep { position: StepPosition },
}

pub enum ToolRetryRequirement {
    None,
    ProviderIdempotencyGuarantee,
}

pub enum ToolRecoveryAction {
    StartUndispatched { call: PendingToolCall },
    RetryDispatched {
        call: PendingToolCall,
        previous_dispatch: PendingToolDispatch,
        next_attempt: u32,
        requirement: ToolRetryRequirement,
    },
}

pub enum ResumeDecision {
    Clean,
    ContinueOpenTurn { turn: TurnNo },
    ContinueOpenStep { position: StepPosition },
    RecoverInterruptedModelRequest {
        position: StepPosition,
        request: ModelRequested,
    },
    RecoverToolBatch {
        position: StepPosition,
        actions: Vec<ToolRecoveryAction>,
    },
    PersistRecoveryBlock {
        proposal: RecoveryBlockProposal,
    },
    Blocked {
        block: RecoveryBlock,
        cursor: DurableCursor,
    },
}
```

Analyzer:

```rust
pub struct RecoveryAnalyzer;

impl RecoveryAnalyzer {
    pub fn analyze(
        &self,
        projection: &SessionProjection,
    ) -> Result<ResumeDecision, RecoveryAnalysisError>;
}
```

### Bootstrap

```rust
pub struct AgentBootstrap {
    pub head: SessionHead,
    pub projection: SessionProjection,
    pub resume: ResumeDecision,
}

pub struct AgentBootstrapper<P> { /* private fields */ }

impl<P> AgentBootstrapper<P>
where
    P: SessionProjector,
{
    pub async fn load<S>(
        &self,
        store: &S,
        session_id: &SessionId,
    ) -> Result<AgentBootstrap, AgentBootstrapError>
    where
        S: SessionStore + ?Sized;
}
```

### Process-local state

```rust
pub enum AgentPhase {
    Idle { last_turn: Option<TurnNo> },
    Running { turn: TurnNo, step: Option<StepNo> },
    Maintenance,
}

pub enum ExecutionGate {
    Open,
    Blocked(RecoveryBlock),
}

pub struct AgentState {
    pub instance_id: AgentInstanceId,
    pub session_id: SessionId,
    pub expected_seq: EventSeq,
    pub phase: AgentPhase,
    pub gate: ExecutionGate,
    pub resume: ResumeDecision,
    pub projection: SessionProjection,
}
```

Important behavior:

```rust
state.can_start_new_turn()
```

is true only for `Idle + Open + ResumeDecision::Clean`.

Actor owner skeleton:

```rust
pub struct AgentActor { /* private state */ }

AgentActor::from_bootstrap(instance_id, bootstrap)
actor.state()
actor.into_state()
```

# Batch 05 Public API Surface

## `harness-agent`

### Actor runtime

```rust
pub struct AgentActor { /* single owner */ }

pub enum AgentExitReason {
    ShutdownRequested,
    MailboxClosed,
    Fatal(AgentError),
}

pub struct AgentExit {
    pub reason: AgentExitReason,
    pub final_state: AgentState,
}
```

`AgentActor` deliberately does not implement `Clone`.

### Handle

```rust
#[derive(Clone)]
pub struct AgentHandle { /* mailbox sender */ }

impl AgentHandle {
    pub fn instance_id(&self) -> &AgentInstanceId;
    pub fn session_id(&self) -> &SessionId;

    pub async fn submit(&self, command: AgentCommand)
        -> Result<AgentCommandAck, AgentHandleError>;

    pub async fn send(&self, message: Message, target: InboxTarget, wakeup: bool)
        -> Result<SendReceipt, AgentHandleError>;

    pub async fn followup(&self, message: Message)
        -> Result<SendReceipt, AgentHandleError>;

    pub async fn steer(&self, message: Message)
        -> Result<SendReceipt, AgentHandleError>;

    pub async fn inject(&self, message: Message)
        -> Result<SendReceipt, AgentHandleError>;

    pub async fn cancel(&self, cause: CancelCause, keep_inbox: bool)
        -> Result<(), AgentHandleError>;

    pub async fn snapshot(&self) -> Result<AgentState, AgentHandleError>;
    pub async fn shutdown(&self) -> Result<(), AgentHandleError>;
}
```

`cancel` is present but returns `UnsupportedOperation` in Batch 05.

### Durable send acknowledgement

```rust
pub struct SendReceipt {
    pub message_id: MessageId,
    pub event_id: EventId,
    pub seq: EventSeq,
    pub wake_requested: bool,
}

pub enum AgentCommandAck {
    Send(SendReceipt),
    Cancelled,
    Shutdown,
}
```

### Event source

```rust
pub trait AgentEventSource: Send + Sync {
    fn next_event_id(&self) -> EventId;
    fn now(&self) -> Timestamp;
}
```

The production implementation must generate collision-resistant IDs across restarts.

### Spawn / task supervision

```rust
pub struct AgentActorConfig {
    pub mailbox_capacity: usize,
    pub bootstrap_page_size: usize,
}

pub async fn spawn_agent(
    instance_id: AgentInstanceId,
    session_id: SessionId,
    store: Arc<dyn SessionStore>,
    event_source: Arc<dyn AgentEventSource>,
    config: AgentActorConfig,
) -> Result<SpawnedAgent, AgentSpawnError>;

pub struct SpawnedAgent {
    pub handle: AgentHandle,
    pub task: AgentTask,
}

impl AgentTask {
    pub async fn join(self) -> Result<AgentExit, AgentJoinError>;
    pub fn abort(&self);
    pub fn is_finished(&self) -> bool;
}
```

### Errors

```rust
pub enum AgentError {
    OwnershipLost { session_id, expected, actual },
    Storage { code, message },
    InvalidDurableMutation { message },
    StorageContractViolation { message },
    UnsupportedOperation { operation, reason },
}

pub enum AgentHandleError {
    ActorClosed,
    AcknowledgementDropped,
    AcknowledgementMismatch,
    Command(AgentError),
}
```

### State addition

```rust
pub struct AgentState {
    // existing Batch 04 fields ...
    pub wake_requested: bool,
}
```

### Bootstrap addition

```rust
pub struct AgentBootstrap {
    pub events: Vec<SessionEvent>,
    pub head: SessionHead,
    pub projection: SessionProjection,
    pub resume: ResumeDecision,
}
```

The retained event prefix is the exact snapshot used by the actor for local pre-commit projection.

## `harness-session`

No public type is added. `V1SessionProjector` now enforces uniqueness of `SessionEvent.eventId`
within one Session.
