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

# Batch 06 API Surface

**Baseline repository:** `denislov/harness`

**Baseline commit:** `228aa80798d0c0c8b26c64ea674073124df7aef9`

**Scope:** deterministic Agent Turn/Step driver through the first external model boundary.

## Public additions

### `harness-agent`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentDriverBoundary {
    ReadyForModel {
        position: harness_session::StepPosition,
    },
}
```

```rust
impl AgentState {
    pub fn driver_boundary(&self) -> Option<AgentDriverBoundary>;
}
```

`driver_boundary()` returns `ReadyForModel` only when all of the following are true:

```text
AgentPhase == Running at turn/step
ResumeDecision == ContinueOpenStep at the same turn/step
SessionProjection.open_step_assistant_message == None
```

The boundary is process-local. No new SessionEvent is introduced.

### `harness-session`

`SessionProjection` gains one replay-derived field:

```rust
pub open_step_assistant_message: Option<MessageId>
```

The projector sets it when the authoritative `assistant/message` for the open step is observed and clears it when that step ends.

This field prevents an ambiguous `ContinueOpenStep` from being interpreted as another model request after an assistant message was already committed.

## Behavioral changes

### `spawn_agent`

Before publishing `AgentHandle`, startup now performs two convergence passes:

```text
bootstrap
  -> recovery-only convergence
  -> deterministic driver convergence
  -> publish handle
```

The deterministic pass may:

- start a new turn from durable waking Inbox work;
- continue an open turn;
- start a step;
- durably claim Inbox input;
- append model-visible `user/message` events;
- close an exhausted open turn;
- park at `ReadyForModel`.

It does not invoke an LLM, Tool, ProviderHost, network service, or approval system.

### `AgentCommand::Send`

The Rust v0.1 Agent Inbox now rejects messages whose `role` is not `Role::User` before durable enqueue.

`MessageSource` remains independent from `Role`; plugin/runtime provenance may still enter the Inbox as a user-role message.

### Wake latch

`AgentState::wake_requested` is refreshed from the durable pending Inbox projection after each committed mutation instead of being manually left set after a claim.

A consumed waking input therefore clears the latch unless another pending Inbox item still has `wakeup=true`.

## Deterministic driver batching

For a new turn's first step, the actor commits one atomic batch in this order:

```text
turn/started
inbox/claimed(next-turn)?
step/started
user/message(primary next-turn)?
[inbox/claimed(next-step), user/message(next-step)]*
```

Selection rule:

```text
at most one next-turn item
+
all currently pending next-step items
```

Model-visible order is primary `next-turn` first, then `next-step` FIFO.

When an already open pre-model step receives more `next-step` input, the same step receives:

```text
[inbox/claimed(next-step), user/message(next-step)]*
```

without another `step/started` event.

## Internal additions

`harness-agent/src/loop_driver.rs` introduces crate-private planning types:

```text
DriverPlan
PlannedInboxInput
plan_next(AgentState)
```

These are intentionally not public API. They separate deterministic planning from actor-owned durable mutation.

## Explicitly deferred

Batch 06 does not implement:

- actual LLM invocation;
- `model/requested` creation from the live driver;
- post-assistant step finalization;
- Tool execution or Tool recovery retries;
- active-operation cancellation;
- prompt/tool catalog assembly.

A post-assistant open step is deliberately deferred rather than mapped to `ReadyForModel`.

## Durable compatibility

Batch 06 introduces **no new SessionEvent type and no schema-version change**.

`open_step_assistant_message` is projection state derived from existing `assistant/message` and `step/ended` facts.

# Batch 07 API Surface

## `harness-types`

New counter:

```rust
pub struct StreamSeq(u64);
```

It has the same cross-language safe-integer rules as `EventSeq`, `TurnNo`, and `StepNo`.

## `harness-storage`

New crate:

```rust
#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(
        &self,
        bytes: Vec<u8>,
        media_type: Option<String>,
    ) -> Result<BlobRef, BlobStoreError>;

    async fn get(&self, blob_id: &BlobId) -> Result<Vec<u8>, BlobStoreError>;

    async fn verify(&self, blob: &BlobRef) -> Result<(), BlobStoreError>;
}
```

`BlobStoreError` variants:

```text
NotFound
Integrity
Backend
```

## `harness-storage-local`

New reference backend:

```rust
pub struct MemoryBlobStore;

impl MemoryBlobStore {
    pub fn new() -> Self;
    pub fn len(&self) -> Result<usize, BlobStoreError>;
    pub fn is_empty(&self) -> Result<bool, BlobStoreError>;
}
```

It implements `BlobStore` with SHA-256 content addressing and byte deduplication.

## `harness-llm`

### Request domain

```rust
pub struct ModelOptions {
    pub max_output_tokens: Option<u32>,
}

pub struct ModelToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

pub struct ModelRequestConfig {
    pub provider: ProviderId,
    pub model: String,
    pub system: Option<String>,
    pub tools: Vec<ModelToolSpec>,
    pub options: ModelOptions,
}

pub struct ModelRequest {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub provider: ProviderId,
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ModelToolSpec>,
    pub options: ModelOptions,
}
```

Important methods:

```rust
ModelRequestConfig::validate()
ModelRequestConfig::build(...)
ModelRequest::validate()
ModelRequest::snapshot_bytes()
```

### Provider seam

```rust
pub type LlmEventStream = Pin<
    Box<dyn Stream<Item = Result<SequencedStreamEvent, PortableError>> + Send + 'static>,
>;

pub trait LlmProvider: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    fn stream(&self, request: ModelRequest) -> LlmEventStream;
}
```

This is a Core domain seam, not the cross-language Provider Protocol. A later Provider Host adapter will implement this trait after translating JSON-RPC/wire events.

### Stream domain

```rust
pub enum BlockType {
    Text,
    Reasoning,
    ToolCall,
}

pub enum FinishReason {
    Completed,
    MaxTokens,
    Error,
    Cancelled,
}

pub struct SequencedStreamEvent {
    pub seq: StreamSeq,
    pub event: StreamEvent,
}
```

`StreamEvent` variants:

```text
BlockStart
TextDelta
ReasoningDelta
ToolCallDelta
BlockEnd
Usage
Finish
```

### Stream assembler

```rust
pub struct LlmStreamAssembler;

impl LlmStreamAssembler {
    pub fn new() -> Self;
    pub fn push(&mut self, item: SequencedStreamEvent)
        -> Result<(), StreamAssemblyError>;
    pub fn finish(self)
        -> Result<LlmStreamOutcome, StreamAssemblyError>;
}
```

Outcomes:

```rust
pub enum LlmStreamOutcome {
    Assistant {
        content: Vec<ContentBlock>,
        usage: Option<TokenUsage>,
        finish_reason: FinishReason,
    },
    Failure {
        failure: PortableError,
        finish_reason: FinishReason,
    },
}
```

## `harness-agent`

### LLM runtime binding

```rust
pub struct AgentLlmRuntime;

impl AgentLlmRuntime {
    pub fn new(
        request_config: ModelRequestConfig,
        provider: Arc<dyn LlmProvider>,
        blob_store: Arc<dyn BlobStore>,
    ) -> Result<Self, AgentLlmRuntimeError>;
}
```

The constructor rejects a configured `ProviderId` that does not match the attached `LlmProvider`.

### Live operation state

```rust
pub enum ActiveAgentOperation {
    Model {
        position: StepPosition,
        request_id: RequestId,
        attempt: u32,
    },
}
```

New field:

```rust
pub struct AgentState {
    // existing fields ...
    pub active_operation: Option<ActiveAgentOperation>,
    pub wake_requested: bool,
}
```

`active_operation` is explicitly process-local and is never reconstructed as durable state after restart.

### Spawn entrypoint

Existing detached mode remains:

```rust
spawn_agent(...)
```

New LLM-enabled mode:

```rust
pub async fn spawn_agent_with_llm(
    instance_id: AgentInstanceId,
    session_id: SessionId,
    store: Arc<dyn SessionStore>,
    event_source: Arc<dyn AgentEventSource>,
    llm_runtime: AgentLlmRuntime,
    config: AgentActorConfig,
) -> Result<SpawnedAgent, AgentSpawnError>;
```

The original `spawn_agent` is intentionally retained so Batch 06 deterministic driver tests and detached runtimes remain valid.

### Internal mailbox

The actor mailbox gains an internal-only completion variant:

```text
MailboxMessage::LlmCompleted(...)
```

This is not exposed through `AgentHandle`.

# Batch 08 Public API Surface

## `harness-tools`

```rust
pub struct ToolDefinition {
    pub name: String,
    pub version: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,
    pub parallel_safe: bool,
    pub side_effect: SideEffectClass,
    pub default_timeout_ms: u64,
}

pub struct ToolInvocationPosition {
    pub turn: TurnNo,
    pub step: StepNo,
}

pub struct ToolInvocation {
    pub invocation_id: InvocationId,
    pub call_id: ToolCallId,
    pub session_id: SessionId,
    pub position: ToolInvocationPosition,
    pub tool_name: String,
    pub arguments_json: JsonText,
    pub attempt: u32,
    pub idempotency_key: IdempotencyKey,
}

pub enum IdempotencySupport {
    None,
    Keyed,
}

pub trait ToolExecutor: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    fn idempotency_support(&self) -> IdempotencySupport;
    fn invoke(&self, invocation: ToolInvocation) -> ToolExecutionFuture;
}

pub trait ToolArgumentValidator: Send + Sync {
    fn validate(
        &self,
        definition: &ToolDefinition,
        arguments_json: &JsonText,
    ) -> Result<(), ToolArgumentValidationError>;
}

pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    Ask { reason: String, risk: String },
}

pub trait ToolPolicy: Send + Sync {
    fn evaluate(&self, input: &ToolPolicyInput) -> PolicyDecision;
}

pub struct ToolRegistration { /* definition + executor + validator */ }
pub struct ToolRegistry { /* unique name -> registration */ }
```

An `idempotent-write` registration is rejected unless its executor declares `IdempotencySupport::Keyed`.

## `harness-session`

New durable enum value:

```rust
StepEndReason::ToolContinuation
```

New replay-derived projection types:

```rust
pub struct OpenStepToolCall {
    pub call_id: ToolCallId,
    pub name: String,
    pub arguments_json: JsonText,
}

pub struct OpenStepToolProjection {
    pub announced: Vec<OpenStepToolCall>,
    pub recorded: BTreeSet<ToolCallId>,
    pub completed: BTreeSet<ToolCallId>,
}
```

`SessionProjection` adds:

```rust
pub open_step_tools: OpenStepToolProjection
```

`LifecycleProjection` adds:

```rust
pub last_ended_step_reason: Option<StepEndReason>
```

These fields are projections only; no new SessionEvent is introduced for `ReadyForTools` or tool scheduling state.

## `harness-agent`

```rust
pub struct AgentToolRuntime { /* registry + policy + retry budget */ }

impl AgentToolRuntime {
    pub fn new(
        registry: Arc<ToolRegistry>,
        policy: Arc<dyn ToolPolicy>,
        max_automatic_attempts: u32,
    ) -> Result<Self, AgentToolRuntimeError>;

    pub fn model_tool_specs(&self) -> Vec<ModelToolSpec>;
}
```

New process-local boundary:

```rust
AgentDriverBoundary::ReadyForTools { position: StepPosition }
```

New process-local active operation:

```rust
ActiveAgentOperation::Tool {
    position: StepPosition,
    call_id: ToolCallId,
    invocation_id: InvocationId,
    attempt: u32,
}
```

New composition entrypoint:

```rust
pub async fn spawn_agent_with_capabilities(
    instance_id: AgentInstanceId,
    session_id: SessionId,
    store: Arc<dyn SessionStore>,
    event_source: Arc<dyn AgentEventSource>,
    llm_runtime: AgentLlmRuntime,
    tool_runtime: AgentToolRuntime,
    config: AgentActorConfig,
) -> Result<SpawnedAgent, AgentSpawnError>;
```

In this composition mode `ModelRequestConfig.tools` must be empty because the ToolRegistry is the authoritative model-visible catalog.

## Internal Agent seams

The following are intentionally crate-private in Batch 08:

```text
ToolDriverPlan
ToolCompletion
spawn_tool_operation
AgentActor::advance_tool_boundary
AgentActor::handle_tool_completion
```

They are implementation seams, not stable application APIs yet.

# Batch 09 Public API Surface

## `harness-types`

```rust
pub struct ApprovalId(/* opaque string */);

pub enum ApprovalDecision {
    Allow,
    Deny,
}
```

`ApprovalId` follows the same non-empty opaque-ID contract as the existing Session/Request/Tool IDs.

## `harness-session`

New durable payloads:

```rust
pub struct ApprovalRequested {
    pub approval_id: ApprovalId,
    pub call_id: ToolCallId,
    pub reason: String,
    pub risk: String,
}

pub struct ApprovalResolved {
    pub approval_id: ApprovalId,
    pub call_id: ToolCallId,
    pub decision: ApprovalDecision,
    pub note: Option<String>,
}
```

New event types:

```text
approval/requested
approval/resolved
```

New projection values:

```rust
pub struct PendingApproval {
    pub request_event_id: EventId,
    pub request_seq: EventSeq,
    pub turn: TurnNo,
    pub step: StepNo,
    pub data: ApprovalRequested,
}

pub struct ToolApprovalResolution {
    pub approval_id: ApprovalId,
    pub decision: ApprovalDecision,
    pub note: Option<String>,
}
```

`SessionProjection` adds:

```rust
pub pending_approval: Option<PendingApproval>
```

`OpenStepToolProjection` adds:

```rust
pub approvals: BTreeMap<ToolCallId, ToolApprovalResolution>
```

## `harness-agent`

### Commands and acknowledgements

```rust
pub enum AgentCommand {
    // existing Send / Cancel / Shutdown ...
    ResolveApproval {
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        note: Option<String>,
    },
}

pub struct ApprovalReceipt {
    pub approval_id: ApprovalId,
    pub decision: ApprovalDecision,
    pub event_id: EventId,
    pub seq: EventSeq,
}

pub enum AgentCommandAck {
    Send(SendReceipt),
    Cancelled,
    ApprovalResolved(ApprovalReceipt),
    Shutdown,
}
```

### Handle

```rust
impl AgentHandle {
    pub async fn cancel(
        &self,
        cause: CancelCause,
        keep_inbox: bool,
    ) -> Result<(), AgentHandleError>;

    pub async fn resolve_approval(
        &self,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        note: Option<String>,
    ) -> Result<ApprovalReceipt, AgentHandleError>;
}
```

### LLM timeout

```rust
pub const DEFAULT_LLM_TIMEOUT_MS: u64 = 120_000;

impl AgentLlmRuntime {
    pub fn with_timeout_ms(self, timeout_ms: u64)
        -> Result<Self, AgentLlmRuntimeError>;

    pub const fn timeout_ms(&self) -> u64;
}
```

Zero LLM timeout is rejected by `AgentLlmRuntimeError::ZeroTimeout`.

### Recovery / driver state

```rust
pub enum ResumeDecision {
    // existing variants ...
    AwaitingApproval {
        position: StepPosition,
        approval: PendingApproval,
    },
}

pub enum AgentDriverBoundary {
    ReadyForModel { position: StepPosition },
    ReadyForTools { position: StepPosition },
    AwaitingApproval { position: StepPosition },
}
```

`AwaitingApproval` is derived from durable projection and is not a new SessionEvent itself.

## Internal execution changes

The following remain crate-private implementation details:

```text
ToolDriverPlan::RequestApproval
actor/control_support.rs
spawn_llm_operation(..., timeout_ms, ...)
spawn_tool_operation(..., timeout_ms, ...)
stale LLM/Tool completion filtering
```

# Batch 10 API Surface

## `harness-provider-protocol`

### Constants

```rust
JSONRPC_VERSION: &str = "2.0"
PROTOCOL_VERSION: &str = "1.0"
MAX_JSON_SAFE_INTEGER: u64

METHOD_PROVIDER_INITIALIZE
METHOD_PROVIDER_PING
METHOD_PROVIDER_SHUTDOWN
METHOD_TOOL_INVOKE
METHOD_LLM_START
METHOD_LLM_EVENT
METHOD_CAPABILITY_CANCEL
```

### JSON-RPC / NDJSON

```rust
RpcId
RpcRequest<P>
RpcNotification<P>
RpcSuccessResponse<R>
RpcErrorResponse
RpcErrorObject
InboundMessage
RpcResponseEnvelope
RpcResponseOutcome
RpcNotificationEnvelope
RpcRequestEnvelope

encode_ndjson(...)
decode_inbound_line(...)
```

Protocol v1 narrows JSON-RPC ids to non-empty strings.

### Manifest

```rust
ProviderManifest
CapabilityDescriptor::{Tool, Llm}
WireSideEffectClass
ManifestValidationError
```

### Common wire vocabulary

```rust
WireRole
WireMessageSource
WireMessage
WireContentBlock
WireBlobRef
WireErrorCode
WirePortableError
WireTokenUsage
WireCancelCause
WireCancelCauseKind
CommonWireValidationError
```

`WireBlobRef`, `WireContentBlock`, `WireMessage`, `WirePortableError`, and `WireTokenUsage` expose semantic `validate()` methods. Numeric wire counters/sizes that use `u64` are restricted to the maximum safe JSON integer where applicable.

### Tool

```rust
ToolInvokeParams
ToolInvokeResult
ProviderToolOutcome::{Success, Error, Cancelled}
ToolInvokeValidationError
```

`ToolInvokeParams::validate()` checks ids, JSON arguments, attempt number, and UTC RFC3339 deadline. `ToolInvokeResult::validate()` checks provider outcome content before Host accepts it.

Provider outcomes deliberately exclude Core-derived `Denied` and `Unknown`.

### LLM

```rust
WireModelOptions
WireModelToolSpec
WireModelRequest
LlmStartParams
LlmStartResult
LlmEventParams
WireBlockType
WireFinishReason
WireLlmStreamEvent
LlmWireValidationError
```

`LlmStartParams.stream_id` is Core allocated. The Provider must echo it in `LlmStartResult`. LLM request messages, block-end content, usage counters, failure payloads, stream sequence, and UTC RFC3339 deadline are validated at the wire boundary.

## `harness-provider-host`

```rust
ProviderState {
    Starting,
    Ready,
    Unhealthy,
    Stopping,
    Stopped,
}

ProviderHostConfig
ProviderHost
ProviderHostError
ProviderStreamError
LlmStreamItem
LlmStreamHandle
```

Principal methods:

```rust
ProviderHost::start(config).await
host.state().await
host.manifest().await
host.recent_stderr().await
host.ping().await
host.invoke_tool(params).await
host.start_llm(operation_id, request, deadline).await
host.cancel(operation_id, cause).await
host.shutdown().await

stream.recv().await
```

The Host is a transport multiplexer, not yet a Harness LLM/Tool domain adapter. Before `tool.invoke` or `llm.start`, it checks the initialized manifest for the requested capability. LLM requests must also use the manifest provider id. Manifest validation rejects `idempotent-write` Tool descriptors that do not advertise keyed idempotency support.


## Conformance

```text
conformance/provider_protocol_v1_smoke.py
providers/example-python/provider.py
```

The smoke test launches the reference provider over real stdin/stdout pipes and verifies initialization, Tool RPC, Core-owned LLM stream routing, ordered events, and shutdown.

# Batch 11 API Surface

## harness-llm

```rust
pub type LlmCancelFuture =
    Pin<Box<dyn Future<Output = Result<(), PortableError>> + Send + 'static>>;

pub trait LlmProvider: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    fn stream(&self, request: ModelRequest) -> LlmEventStream;
    fn cancel(&self, request_id: RequestId, cause: CancelCause) -> LlmCancelFuture;
}
```

`cancel` has a default successful no-op implementation.

## harness-tools

```rust
pub type ToolCancelFuture =
    Pin<Box<dyn Future<Output = Result<(), PortableError>> + Send + 'static>>;

pub trait ToolExecutor: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    fn idempotency_support(&self) -> IdempotencySupport;
    fn invoke(&self, invocation: ToolInvocation) -> ToolExecutionFuture;
    fn cancel(&self, invocation_id: InvocationId, cause: CancelCause) -> ToolCancelFuture;
}
```

`cancel` has a default successful no-op implementation.

## harness-provider-host

```rust
#[derive(Clone)]
pub struct ProviderHostLlmAdapter { /* private */ }

impl ProviderHostLlmAdapter {
    pub async fn new(host: ProviderHost) -> Result<Self, ProviderAdapterError>;
    pub fn host(&self) -> &ProviderHost;
}

impl LlmProvider for ProviderHostLlmAdapter { /* ... */ }
```

```rust
#[derive(Clone)]
pub struct ProviderHostToolAdapter { /* private */ }

impl ProviderHostToolAdapter {
    pub async fn new(
        host: ProviderHost,
        tool_name: impl Into<String>,
    ) -> Result<Self, ProviderAdapterError>;

    pub async fn from_definition(
        host: ProviderHost,
        definition: &ToolDefinition,
    ) -> Result<Self, ProviderAdapterError>;

    pub fn host(&self) -> &ProviderHost;
    pub fn tool_name(&self) -> &str;
    pub fn manifest_side_effect(&self) -> SideEffectClass;

    pub fn validate_definition(
        &self,
        definition: &ToolDefinition,
    ) -> Result<(), ProviderAdapterError>;
}

impl ToolExecutor for ProviderHostToolAdapter { /* ... */ }
```

```rust
#[non_exhaustive]
pub enum ProviderAdapterError {
    ManifestUnavailable,
    InvalidProviderId { value: String, message: String },
    EmptyToolName,
    ToolNotDeclared(String),
    DefinitionMismatch {
        tool: String,
        field: &'static str,
        core: String,
        provider: String,
    },
}
```

## Agent cancellation bridge

`AgentActor::handle_cancel` receives both LLM and Tool runtimes, captures the live capability cancellation target before durable mutation, commits Batch 09 cancellation/recovery events, then invokes the corresponding domain cancellation hook before aborting the local task.

Agent-owned timeout paths invoke the same hooks with `CancelCause::Timeout`.

## Batch 12 — Python Provider SDK

New source package: `sdk/python/harness_provider_sdk`.

Primary authoring surface: `ProviderApp`, `SideEffect`, `ToolResult`, `ToolContext`, `ModelContext`, `LlmStreamWriter`, `CancellationToken`, `CancelCause`, `last_text`, and `trailing_tool_result_text`.

The SDK generates ProviderManifest from decorators and owns Provider Protocol v1 lifecycle/dispatch framing. Rust public APIs are unchanged except for the implementation-only Clippy cleanup in `harness-provider-host/src/adapter.rs`.

## Batch 13 — Provider SDK Conformance Contract v1

Batch 13 adds no Rust public API.

New conformance surfaces:

```text
conformance/provider-sdk-v1/contract.json
conformance/provider-sdk-v1/fixture.schema.json
conformance/provider-sdk-v1/fixtures/*.json
conformance/provider_sdk_v1_runner.py
conformance/run_python_sdk_v1.py
conformance/providers/python_sdk_v1.py
```

The generic runner treats a provider as an opaque subprocess and compares parsed JSON frames using exact structural equality. Each fixture gets a fresh process plus automatic initialize/manifest verification and graceful shutdown verification.

The canonical conformance manifest and behavior are normative test data. Future Provider SDKs implement the same conformance provider and run the same fixtures rather than maintaining language-specific expected outputs.

## Batch 14 — Harness Runtime Composition Root

### `harness-runtime`

New public composition surface:

```rust
HarnessRuntime
HarnessRuntimeBuilder
HarnessRuntimeState
HarnessRuntimeInfo
HarnessRuntimeBuildError
HarnessRuntimeError

ProviderProcessSpec
ProviderRegistry
LlmRegistry

AgentProfile
ModelBinding
RuntimeToolBinding
ProfileRegistry

AgentRegistry
RuntimeIdSource
```

Primary lifecycle API:

```rust
HarnessRuntime::builder()
HarnessRuntimeBuilder::in_memory(event_source, id_source)
HarnessRuntimeBuilder::provider(...)
HarnessRuntimeBuilder::profile(...)
HarnessRuntimeBuilder::build().await

HarnessRuntime::create_session().await
HarnessRuntime::create_session_with_data(...).await
HarnessRuntime::create_session_with_id(...).await
HarnessRuntime::open_agent(session_id, profile_name).await
HarnessRuntime::agent_handle(&session_id).await
HarnessRuntime::close_agent(&session_id).await
HarnessRuntime::shutdown().await
```

Provider membership and Profile membership are immutable after `build()` in Batch 14. Agent membership is dynamic.

`RuntimeToolBinding` keeps `ToolDefinition` and `ToolArgumentValidator` Core-authoritative while binding execution to one provider manifest.

`RuntimeIdSource` is intentionally injected; Batch 14 does not choose a UUID/ULID dependency for production identity generation.

## Batch 15 — Durable Local Storage

### `harness-storage-local`

```rust
pub struct SqliteSessionStore { /* ... */ }

impl SqliteSessionStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, SessionStoreError>;
    pub fn path(&self) -> &Path;
}

impl SessionStore for SqliteSessionStore { /* existing trait surface */ }

pub struct FilesystemBlobStore { /* ... */ }

impl FilesystemBlobStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, BlobStoreError>;
    pub fn root(&self) -> &Path;
}

impl BlobStore for FilesystemBlobStore { /* existing trait surface */ }

pub struct DurableLocalStorage { /* ... */ }

impl DurableLocalStorage {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, DurableLocalStorageError>;
    pub fn root(&self) -> &Path;
    pub fn session_store(&self) -> Arc<SqliteSessionStore>;
    pub fn blob_store(&self) -> Arc<FilesystemBlobStore>;
}

pub enum DurableLocalStorageError { /* non_exhaustive */ }
```

### `harness-runtime`

```rust
impl HarnessRuntimeBuilder {
    pub fn durable_local(
        root: impl Into<PathBuf>,
        event_source: Arc<dyn AgentEventSource>,
        id_source: Arc<dyn RuntimeIdSource>,
    ) -> Result<Self, HarnessRuntimeBuildError>;
}
```

`HarnessRuntimeBuildError` keeps the same semantic variants but source-heavy build failures now store boxed sources. It also gains `DurableLocalStorage` for the convenience composition path.

`HarnessRuntime::from_parts` remains crate-private and now receives one crate-private `HarnessRuntimeParts` value.
# Batch 16 Public API Surface

## `harness-config`

```text
HARNESS_CONFIG_SCHEMA_VERSION = 1
HarnessConfig
RuntimeConfig
ProviderConfig
ProfileConfig
ModelConfig
ToolConfig
PolicyConfig::AllowAll
LoadedHarnessConfig
RuntimePlan
HarnessConfigError
```

Key entry points:

```rust
let loaded = LoadedHarnessConfig::load("harness.toml")?;
let plan = loaded.compile()?;
let builder = plan.runtime_builder(event_source, id_source)?;
let runtime = builder.build().await?;
```

`RuntimePlan` exposes resolved runtime info, durable data directory, provider/profile counts, profile names, and the optional default profile.

## `harness-cli`

Binary name:

```text
harness
```

Commands:

```text
harness --config <FILE> config check
harness --config <FILE> session create
harness --config <FILE> run <SESSION_ID> [--profile <NAME>]
harness --config <FILE> inspect <SESSION_ID> [--pretty]
```

Batch 16 CLI identity generation uses UUID v4 while preserving existing opaque prefixes (`ses_`, `agt_`, `evt_`, `msg_`).

# Batch 17 Public API Surface

## `harness-runtime`

New public credential types:

```rust
CredentialKey
CredentialKeyError
SecretValue
CredentialResolveError
CredentialResolver
RejectingCredentialResolver
```

`HarnessRuntimeBuilder` gains:

```rust
fn credential_resolver(self, Arc<dyn CredentialResolver>) -> Self
fn runtime_event_bus(self, RuntimeEventBus) -> Self
```

`ProviderProcessSpec` gains:

```rust
fn credential_env(self, key: impl Into<OsString>, credential: CredentialKey) -> Self
```

New public operational event surface:

```rust
RuntimeEvent
RuntimeEventKind
RuntimeBuildStage
RuntimeEventBus
RuntimeEventBusError
RUNTIME_EVENT_SCHEMA_VERSION
DEFAULT_RUNTIME_EVENT_CAPACITY
```

`HarnessRuntime` gains:

```rust
fn events(&self) -> &RuntimeEventBus
```

## `harness-config`

New configuration DTOs:

```rust
CredentialConfig
ObservabilityConfig
EnvironmentCredentialResolver
```

`RuntimePlan` gains:

```rust
fn credential_count(&self) -> usize
fn runtime_events_jsonl(&self) -> Option<&Path>
```

`RuntimePlan::runtime_builder` now wires its compiled `CredentialResolver` into `HarnessRuntimeBuilder`.

## `harness-cli`

No new command is introduced. `run` optionally starts the configured RuntimeEvent JSONL recorder; `config check`, `session create`, and `inspect` remain offline with respect to Provider and credential resolution.

## Batch 18 API surface

### `harness-config`

New public configuration types:

```rust
ScopeConfig
ScopeModelConfig
CapabilityScopeConfig
SessionScopeConfig
PromptMode
```

New resolution API:

```rust
ScopeSelection
ResolvedScope
ScopeResolutionTrace
PromptFragmentTrace
ResolvedModelTrace

RuntimePlan::default_workspace()
RuntimePlan::workspace_count()
RuntimePlan::session_scope_count()
RuntimePlan::contains_workspace(...)
RuntimePlan::session_profile(...)
RuntimePlan::session_workspace(...)
RuntimePlan::resolve_scope(...)
RuntimePlan::runtime_builder_for_scope(...)
```

`ProfileConfig.policy` and `ProfileConfig.max_automatic_tool_attempts` are optional overlays whose final values are resolved across scopes. `ModelConfig.timeout_ms` is likewise an optional overlay. `ToolConfig.enabled` can explicitly override broader capability visibility.

### `harness-cli`

```text
harness config resolve [--profile NAME] [--workspace NAME] [--session SESSION_ID] [--json]
harness run SESSION_ID [--profile NAME] [--workspace NAME]
```


## Batch 19 API surface

### `harness-session`

New durable event payload and projection:

```rust
CompositionActivated { profile, snapshot }
ActiveComposition
SessionProjection::active_composition
```

The event type is `composition/activated` and is valid only at a quiescent durable boundary.

### `harness-tools`

`ToolPolicy` and `ToolArgumentValidator` gain source-compatible default methods:

```rust
fn composition_identity(&self) -> String
```

Built-in file-configured validation and `AllowAllToolPolicy` return explicit stable identities.

### `harness-runtime`

New durable snapshot vocabulary:

```rust
ExecutionCompositionSnapshot
ExecutionModelComposition
ExecutionToolComposition
EXECUTION_COMPOSITION_SCHEMA_VERSION
EXECUTION_COMPOSITION_MEDIA_TYPE
```

`HarnessRuntime::open_agent` now reconciles the requested compiled profile against the latest durable composition before Agent spawn. New error variants include `CompositionBootstrap`, `CompositionSnapshotVerify`, `LegacyCompositionUnbound`, `CompositionDrift`, and `CompositionInvariant`.


## Batch 20 API surface

Batch 20 introduces no production Harness API or wire-protocol change.

### `harness-conformance` (workspace-only, unpublished)

Reusable conformance fixtures:

```rust
AppendFault
FaultInjectingSessionStore
ObservedAppend
TestEventSource
ScriptedLlm
```

`FaultInjectingSessionStore` commits through the wrapped `SessionStore` first, then can deliberately return an error for one configured committed event occurrence. It is test infrastructure for the post-commit/pre-ack crash cut and is not used by production crates.

### Conformance assets

```text
conformance/crash-matrix-v1.json
conformance/validate_crash_matrix.py
```

The matrix records crash/fault cuts, their atomic append batches, expected recovery properties, and owning Cargo test commands.


## Batch 21 API surface

### `harness-provider-host`

```rust
ProviderGeneration
ProviderSlot
ProviderSlotStatus
ProviderSlotError
ProviderSlotLlmAdapter
ProviderSlotToolAdapter
```

Slot-bound adapters resolve the current Ready provider generation per new operation. An in-flight operation remains pinned to the generation it started on.

### `harness-runtime`

```rust
ProviderSupervisorConfig
ProviderSupervisorConfigError
ProviderQuarantineReason
ProviderRegistry::status(...)
ProviderRegistry::generation(...)
HarnessRuntimeBuilder::provider_supervisor_config(...)
```

New RuntimeEvent kinds:

```text
provider/unhealthy
provider/restarting
provider/restart-failed
provider/restarted
provider/quarantined
```

Runtime composition continues to use the immutable Runtime-build baseline manifest for composition snapshots and restart compatibility.
