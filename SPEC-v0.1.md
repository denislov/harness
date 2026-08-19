# Language-Agnostic Harness Specification v0.1 — Combined Edition

> This file concatenates the normative draft documents in `spec/` for convenient review. The individual Markdown files remain the canonical editable units.


---

<!-- BEGIN spec/README.md -->

# Language-Agnostic Harness Specification v0.1

**Status:** Draft specification  
**Core implementation language:** Rust  
**Protocol version:** 1.0 draft  
**Date:** 2026-08-19

## 1. Purpose

This specification defines the stable architectural and protocol contracts for a language-agnostic agent harness whose control plane is implemented in Rust and whose capability providers may be implemented in any language.

The design is inspired by the strongest architectural properties of modern agent harnesses: an event-sourced session log, an explicit agent state machine, a policy-aware tool pipeline, provider-neutral LLM streaming, and replaceable capability providers. It intentionally does not depend on JavaScript module semantics, Rust ABI stability, or any one plugin runtime.

The v0.1 goal is to make the core semantics precise enough that the following can be implemented independently without semantic drift:

- the Rust Harness Core;
- a local durable SessionStore;
- an in-process Tool and LLM test implementation;
- the out-of-process Provider Host;
- Python, TypeScript, Go, and Rust provider SDKs;
- a cross-language conformance test suite.

## 2. Normative language

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHALL NOT**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY**, and **OPTIONAL** are to be interpreted as normative requirements.

## 3. Stable v0.1 decisions

The following decisions are considered part of the v0.1 architecture:

1. Harness Core is implemented in Rust.
2. Harness Core is the single control plane and owns authoritative state and ordering.
3. Each live Agent is an actor with a single active driver.
4. Durable session state is event-sourced and append-only.
5. Model-visible history is derived from durable session state.
6. Every model request is snapshotted before dispatch for exact audit and replay.
7. Agent input is persisted through a two-boundary Inbox: `next-turn` and `next-step`.
8. Tool calls and tool invocations are distinct concepts.
9. Tool side effects are classified as `read-only`, `idempotent-write`, or `non-idempotent-write`.
10. Unknown outcomes of non-idempotent writes block automatic continuation.
11. Capability providers are out-of-process and language-neutral.
12. Provider Protocol v1 uses JSON-RPC 2.0, NDJSON framing, and stdin/stdout transport.
13. Provider stdout is protocol-only; provider logs use stderr.
14. Provider Protocol v1 exposes only two first-class capability families: Tool and LLM.
15. Control-plane middleware remains in-process with the Rust Core in v0.1.
16. LLM streaming chunks are runtime events in v0.1; the final assistant message is durable.
17. Session events and runtime events are distinct event domains.
18. A provider crash is detected by Provider Host; retry and recovery semantics are decided by the owning Core subsystem.

## 4. Specification index

| Document | Scope |
|---|---|
| [architecture.md](architecture.md) | System boundaries, responsibilities, module decomposition, extension model |
| [domain-model.md](domain-model.md) | Core identifiers, messages, content blocks, blobs, scopes and common types |
| [invariants.md](invariants.md) | Cross-cutting safety, ordering and durability invariants |
| [session-events.md](session-events.md) | Durable event envelope, event taxonomy, ordering and projection rules |
| [agent-lifecycle.md](agent-lifecycle.md) | Agent actor, Inbox, turn/step lifecycle, cancellation and execution gate |
| [tool-execution.md](tool-execution.md) | Tool definitions, policy pipeline, invocation identity, parallelism and recovery |
| [llm-protocol.md](llm-protocol.md) | Provider-neutral model request, stream events, request snapshots and failures |
| [provider-protocol.md](provider-protocol.md) | JSON-RPC framing, handshake, Tool/LLM RPC, cancellation and provider lifecycle |
| [storage.md](storage.md) | SessionStore, BlobStore, optimistic concurrency, fork and local backend expectations |
| [error-model.md](error-model.md) | Stable machine-readable error taxonomy and ownership rules |
| [security.md](security.md) | Trust boundaries, policy ownership, credentials, provider permissions and sandboxing |
| [conformance.md](conformance.md) | Required provider and storage conformance tests |
| [rust-core-layout.md](rust-core-layout.md) | Recommended Rust workspace boundaries and dependency direction |

## 5. Explicitly deferred from v0.1

The following are intentionally not specified as stable v0.1 contracts:

- arbitrary dynamic-library plugins (`.so`, `.dll`, `.dylib`);
- out-of-process control middleware;
- distributed Agent Loop ownership;
- cross-provider ACID transactions;
- self-modifying runtime plugins;
- first-class Browser, Filesystem, Shell, Embedding, or Reranker capability protocols; they are represented as Tools in v0.1;
- durable per-token LLM stream replay;
- Subagent protocol and orchestration semantics;
- compaction algorithm and summarization policy;
- UI, Web, IDE, ACP, or MCP presentation contracts;
- remote network transport for providers;
- WASM component transport;
- final production configuration schema.

These may be added in later protocol or architecture revisions without weakening the invariants defined here.

<!-- END spec/README.md -->


---

<!-- BEGIN spec/architecture.md -->

# Architecture

**Status:** Draft v0.1

## 1. Architectural objective

The system separates a single authoritative Rust control plane from a language-neutral capability plane.

```text
Clients: CLI / Web / IDE / API
              |
              v
+----------------------------------------------+
|                HARNESS CORE                  |
|                  Rust                        |
|                                              |
| Agent Registry       Session Event Store     |
| Agent Actors         Session Projector       |
| Agent Loop           Prompt Registry         |
| Inbox                Tool Registry           |
| Policy Engine        LLM Registry            |
| Provider Supervisor  Capability Router       |
| Blob Store           Runtime Event Bus       |
+----------------------+-----------------------+
                       |
              Provider Protocol v1
           JSON-RPC 2.0 / NDJSON / stdio
                       |
        +--------------+--------------+
        |              |              |
        v              v              v
      Rust           Python          Node/Go/...
    Provider         Provider         Provider
```

The architectural boundary is semantic rather than language-specific:

```text
Harness Core                 Capability Provider
------------                 -------------------
owns state                   executes capabilities
owns ordering                streams results
owns policy                  reports failures
owns persistence             obeys cancellation
owns recovery decisions      declares capabilities
```

## 2. Control-plane ownership

Harness Core MUST be the only component that can authoritatively:

- create, resume, and dispose live Agent actors;
- transition Agent phase;
- open and close turns and steps;
- claim Inbox work;
- append authoritative SessionEvents;
- decide which messages enter model-visible history;
- assemble model requests;
- decide whether a Tool invocation is allowed;
- classify retries and crash recovery;
- bind capability calls to providers;
- decide when execution is blocked.

Capability Providers MUST NOT directly mutate any of the above state.

## 3. Single-writer model

Each live Agent is represented by one actor. The actor is the sole in-process owner of mutable Agent state for that session while it is active.

Different Agents MAY run concurrently. A single Agent MUST NOT have more than one active turn driver.

The intended consistency model is:

```text
Agent Actor single writer
          +
SessionStore expected-sequence check
          =
serialized domain state with storage-level conflict detection
```

The system MUST NOT expose shared mutable Agent state as a general-purpose `Arc<Mutex<...>>` API.

## 4. Event-sourced durable state

The SessionEvent log is the durable source of truth for a session.

Model history MUST be projected from SessionEvents. The implementation MUST NOT treat a separate mutable `messages[]` collection as the authoritative durable conversation.

This supports:

- process restart and resume;
- deterministic inspection;
- fork at an event boundary;
- auditing;
- crash analysis;
- future compaction;
- future telemetry projection.

## 5. Exact model-request snapshots

Normal requests are assembled from projections and registries, but the exact provider-neutral `ModelRequest` MUST be snapshotted to BlobStore before provider dispatch.

The corresponding durable `model/requested` event records the snapshot reference. This separates two concerns:

- **Projection** explains how the request was generated from current durable state.
- **Snapshot** records exactly what was dispatched at that point in time.

This makes request auditing independent of future projector or prompt-registry changes.

## 6. Extension model

v0.1 distinguishes two extension categories.

### 6.1 Control extensions

Control extensions can participate in policy, request interception, prompt contribution, or other ordering-sensitive behavior. In v0.1 they remain in-process with the Rust Core.

Examples:

- prompt-section contributors;
- pre-step policy;
- tool pre/post middleware;
- approval policy;
- request configuration policy.

### 6.2 Capability providers

Capability providers execute work on behalf of Core through Provider Protocol v1. They MAY be implemented in any language.

v0.1 defines only:

- Tool capability;
- LLM capability.

Filesystem, shell, web search, embeddings and similar functions are represented as Tools until a later specification creates a first-class capability family.

## 7. Hierarchical scopes

Core supports a hierarchical registration concept:

```text
Global Scope
    |
Workspace Scope
    |
Agent Scope
    |
Invocation Scope
```

Registrations belong to a scope and are automatically invalidated when the scope is disposed.

A scope MAY own:

- tool registrations;
- prompt sections;
- policy registrations;
- provider bindings;
- child scopes;
- in-flight operations.

Scope disposal MUST cancel owned in-flight operations before releasing owned registrations and resources.

The exact public Rust Scope API is not frozen by v0.1, but the lifecycle semantics above are normative.

## 8. Runtime events versus durable events

The system has two event domains.

### Durable SessionEvents

Used for facts that must survive restart and affect reconstruction or recovery.

Examples:

- user/message;
- tool/call;
- tool/result;
- turn/ended.

### RuntimeEvents

Used for transient observation and UI/runtime behavior.

Examples:

- provider/restarted;
- agent/status;
- tool/progress;
- LLM text delta;
- transport latency.

A fact that is required to reconstruct model-visible history or determine crash recovery MUST be represented durably.

## 9. Capability routing

CapabilityRouter maps a resolved capability to a ready provider instance. ProviderHost/Supervisor owns provider process lifecycle and transport.

ProviderHost MUST NOT decide domain retry semantics. For example, process failure during a Tool call is reported upward; Tool Runtime determines whether the operation can safely retry based on `SideEffectClass` and idempotency support.

## 10. Non-goals

v0.1 does not attempt to make every Core component independently distributed. The initial design is a single control-plane process with out-of-process capability workers.

The architecture favors deterministic state ownership over maximal runtime dynamism.

<!-- END spec/architecture.md -->


---

<!-- BEGIN spec/domain-model.md -->

# Domain Model

**Status:** Draft v0.1

## 1. Identifier types

Rust Core MUST use distinct domain identifier types instead of untyped strings internally.

Minimum identifier vocabulary:

```rust
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

The wire format for identifiers is a UTF-8 JSON string. Identifiers are opaque. Consumers MUST NOT infer ordering, creation time, tenancy, or security properties from the lexical form of an identifier.

Recommended human-readable prefixes are:

```text
ses_   session
agt_   live agent instance
evt_   durable event
msg_   message
req_   model request
call_  model-generated tool call
inv_   concrete tool invocation
prv_   provider
blob_  blob
```

Prefixes are a debugging convention, not a semantic contract.

## 2. Ordered counters

Core uses distinct numeric types for ordered positions:

```rust
EventSeq
TurnNo
StepNo
```

Wire values are non-negative JSON integers and MUST remain within the IEEE-754 safe integer range (`0..=9007199254740991`) for interoperability with JavaScript-based SDKs.

`EventSeq` is the authoritative event ordering mechanism. ID lexical ordering MUST NOT be used as a substitute.

## 3. Message

A provider-neutral message contains stable identity, role, source and ordered content blocks.

Conceptual Rust shape:

```rust
pub struct Message {
    pub id: MessageId,
    pub role: Role,
    pub source: MessageSource,
    pub content: Vec<ContentBlock>,
}
```

### 3.1 Role

v0.1 defines:

```text
system
user
assistant
```

Tool results are represented as content blocks in model-visible messages rather than a separate top-level role contract.

### 3.2 MessageSource

A message MUST carry provenance. Minimum source kinds are:

```text
user
model
plugin
system
```

A model source SHOULD include the provider and model that produced the message.

Wire example:

```json
{
  "kind": "model",
  "provider": "deepseek-official",
  "model": "example-model"
}
```

Unknown source metadata MAY be carried in extension fields, but providers MUST NOT use source metadata as authorization evidence.

## 4. ContentBlock

v0.1 defines six block types:

```text
text
reasoning
image
tool-call
tool-result
blob
```

### 4.1 Text

```json
{
  "type": "text",
  "text": "hello"
}
```

### 4.2 Reasoning

```json
{
  "type": "reasoning",
  "text": "provider-neutral reasoning content"
}
```

Reasoning is semantically distinct from visible answer text. Presentation behavior is outside this specification.

### 4.3 Image

```json
{
  "type": "image",
  "blob": {
    "id": "blob_123",
    "sha256": "...",
    "size": 12345,
    "mediaType": "image/png"
  }
}
```

### 4.4 Tool call

```json
{
  "type": "tool-call",
  "id": "call_123",
  "name": "read_file",
  "argumentsJson": "{\"path\":\"README.md\"}"
}
```

`argumentsJson` MUST be a JSON text string containing one complete JSON value. The Harness preserves the raw JSON representation across the model/provider-neutral boundary. Schema validation occurs before tool execution.

### 4.5 Tool result

```json
{
  "type": "tool-result",
  "toolCallId": "call_123",
  "content": [
    {"type": "text", "text": "..."}
  ],
  "isError": false
}
```

The provider-neutral message representation is intentionally simpler than Core's internal `ToolOutcome`; Core maps richer outcomes to model-visible content according to policy.

### 4.6 Blob block

```json
{
  "type": "blob",
  "blob": {
    "id": "blob_123",
    "sha256": "...",
    "size": 50000000,
    "mediaType": "application/octet-stream"
  }
}
```

## 5. BlobRef

Large or binary data MUST be referenced rather than embedded in SessionEvents or Provider Protocol frames.

Conceptual shape:

```rust
pub struct BlobRef {
    pub id: BlobId,
    pub sha256: String,
    pub size: u64,
    pub media_type: Option<String>,
}
```

Requirements:

- `sha256` is lowercase hexadecimal SHA-256 of the stored bytes.
- Blob identity and content digest MAY be the same underlying value but are separate semantic fields.
- A BlobRef MUST NOT be considered trusted solely because it appears in a provider response; Core validates storage ownership and access.
- SessionEvents SHOULD store BlobRefs instead of large inline payloads.

## 6. Time

Wire timestamps MUST use RFC 3339 strings in UTC, normally rendered with `Z`.

Example:

```text
2026-08-19T13:00:00Z
```

Durable ordering MUST use `EventSeq`, not timestamp comparison.

## 7. JSON naming

Provider Protocol and durable JSON examples use lower camel case for object fields.

Rust implementation field names MAY use snake_case with explicit serialization attributes.

Stable string discriminators use lowercase words and hyphens where needed, for example:

```text
read-only
idempotent-write
non-idempotent-write
next-turn
next-step
```

<!-- END spec/domain-model.md -->


---

<!-- BEGIN spec/invariants.md -->

# Core Invariants

**Status:** Draft v0.1

These invariants are architectural requirements. Implementations SHOULD encode them as executable tests and, where practical, local assertions.

## I-01: Single active driver

A live Agent MUST have at most one active turn driver.

Different Agents MAY run concurrently.

## I-02: Single authoritative session writer

A live Agent actor is the sole in-process author of domain mutations for the session it owns. SessionStore additionally enforces an expected-sequence check.

## I-03: Strict event ordering

`SessionEvent.seq` MUST increase strictly by one within a session's committed log unless a storage backend explicitly documents equivalent gap-free semantics.

No two committed events in one session may share the same sequence number.

## I-04: Durable model-visible facts

Any fact required to reconstruct model-visible history MUST be represented in durable session state or by a durable reference from session state.

## I-05: Exact model request auditability

Every successfully dispatched model attempt MUST have a durable `model/requested` event referencing an immutable provider-neutral request snapshot created before dispatch.

## I-06: Turn/step nesting

A step MUST belong to an open turn. An assistant message MUST belong to an open step.

A turn MUST NOT end while a step remains open.

## I-07: Tool call/result integrity

Every authoritative `tool/result` MUST reference a previously recorded `tool/call` in the same session and logical step lineage.

## I-08: One authoritative terminal tool outcome

A logical ToolCall MUST have no more than one authoritative terminal ToolOutcome in durable state.

Provider retries are attempts of the same logical invocation semantics, not additional authoritative outcomes.

## I-09: Unknown non-idempotent outcomes block

If a `non-idempotent-write` Tool may have executed but Core cannot determine its terminal outcome, Core MUST NOT automatically retry or continue model execution past that uncertainty.

The Agent's ExecutionGate becomes blocked until a recovery action records a resolution.

## I-10: Blocked gate prevents new turns

When `ExecutionGate` is blocked, the Agent MUST NOT start a new turn. Recovery/administrative commands MAY still operate.

## I-11: Provider cannot author durable state

Capability Providers MUST NOT be granted an API that directly appends authoritative SessionEvents or mutates Agent phase/Inbox state.

## I-12: Provider crash does not imply retry

ProviderHost reports transport/process failure. Domain subsystems decide retry, failure, or blocked recovery according to operation semantics.

## I-13: Cancellation first-cause-wins

For one active operation, the first cancellation cause accepted by Core is authoritative. Later causes MUST NOT overwrite it.

## I-14: Cancellation is activity-scoped

Cancelling a current turn or capability invocation MUST NOT implicitly cancel future work submitted after convergence unless an explicit shutdown/disposal state applies.

## I-15: Inbox mutations are durable before acknowledgement

A durable Agent input MUST be committed as Inbox state before the sender is told that the input has been accepted.

## I-16: Provider stream termination

An LLM stream MUST emit exactly one terminal `finish` event. No stream event may follow the terminal event.

## I-17: Provider protocol stdout purity

For stdio transport, provider stdout contains only framed Provider Protocol messages. Diagnostic logs go to stderr.

## I-18: Authorization stays in Core

Capability Provider declarations and MessageSource provenance MUST NOT be treated as authorization decisions. Policy and approval are owned by Core.

## I-19: Scope disposal is reversible

Disposing a scope MUST invalidate registrations owned exclusively by that scope and cancel operations owned by that scope before resources are released.

## I-20: IDs are opaque

No semantic behavior may depend on identifier lexical order or prefix.

<!-- END spec/invariants.md -->


---

<!-- BEGIN spec/session-events.md -->

# Session Event Specification

**Status:** Draft v0.1

## 1. Purpose

SessionEvents are the append-only durable facts from which session history, Inbox state, execution recovery, and model-visible context can be reconstructed.

The event log is not merely telemetry. It is authoritative application state.

## 2. Event envelope

Conceptual Rust structure:

```rust
pub struct SessionEvent {
    pub event_id: EventId,
    pub session_id: SessionId,
    pub seq: EventSeq,
    pub timestamp: Timestamp,
    pub turn: Option<TurnNo>,
    pub step: Option<StepNo>,
    pub payload: SessionEventPayload,
}
```

Canonical JSON envelope:

```json
{
  "schemaVersion": 1,
  "eventId": "evt_01...",
  "sessionId": "ses_01...",
  "seq": 42,
  "time": "2026-08-19T13:00:00Z",
  "turn": 3,
  "step": 2,
  "type": "tool/result",
  "data": {}
}
```

`turn` and `step` are omitted when not applicable.

## 3. Ordering

`seq` is assigned by SessionStore at commit time. It is the canonical order within one session.

Timestamp ordering is informational and MUST NOT replace `seq`.

A write operation supplies `expectedSeq`. SessionStore MUST reject the append if the committed head sequence no longer equals the expectation.

## 4. v0.1 event taxonomy

### 4.1 `session/created`

Creates the session's durable identity and immutable initial metadata.

Example data:

```json
{
  "createdBy": "user",
  "workspace": "/workspace/project"
}
```

The exact metadata schema may grow. Security-sensitive ambient process state MUST NOT be copied automatically.

### 4.2 `inbox/enqueued`

Records accepted pending Agent input.

```json
{
  "message": {
    "id": "msg_1",
    "role": "user",
    "source": {"kind": "user"},
    "content": [{"type": "text", "text": "Inspect the repository"}]
  },
  "target": "next-turn",
  "wakeup": true
}
```

The event is durable before the sender receives acceptance.

### 4.3 `inbox/claimed`

Records transfer of one pending input item into a specific turn/step boundary.

```json
{
  "messageId": "msg_1",
  "target": "next-turn"
}
```

### 4.4 `inbox/discarded`

Records removal of pending input without model consumption.

```json
{
  "messageId": "msg_1",
  "reason": "cancelled"
}
```

### 4.5 `turn/started`

```json
{
  "turn": 3
}
```

A turn boundary opens before the initial Inbox claim used to propose its first step.

### 4.6 `step/started`

```json
{
  "turn": 3,
  "step": 1
}
```

### 4.7 `user/message`

Records model-visible user-role input that entered the step.

```json
{
  "message": {"id": "msg_1", "role": "user", "source": {"kind": "user"}, "content": []}
}
```

Messages omitted by pre-step policy are not recorded as `user/message` for that step unless another durable event explicitly represents their model-visible effect.

### 4.8 `model/requested`

Records a dispatched model attempt.

```json
{
  "requestId": "req_1",
  "provider": "provider-a",
  "model": "model-x",
  "historyThroughSeq": 18,
  "requestSnapshot": {
    "id": "blob_req_1",
    "sha256": "...",
    "size": 20480,
    "mediaType": "application/json"
  },
  "attempt": 1
}
```

The referenced snapshot is created before provider dispatch.

### 4.9 `model/failed`

Records a terminal failed model attempt after Core has normalized the failure.

```json
{
  "requestId": "req_1",
  "failure": {
    "code": "PROVIDER_UNAVAILABLE",
    "message": "provider exited"
  }
}
```

A retry produces another `model/requested` attempt according to LLM retry policy.

### 4.10 `assistant/message`

Records the assembled assistant response that is authoritative for model history.

```json
{
  "requestId": "req_1",
  "message": {
    "id": "msg_a1",
    "role": "assistant",
    "source": {"kind": "model", "provider": "provider-a", "model": "model-x"},
    "content": [{"type": "text", "text": "..."}]
  },
  "usage": {
    "inputTokens": 100,
    "outputTokens": 20
  }
}
```

Per-token stream events are runtime-only in v0.1.

### 4.11 `tool/call`

Records the logical tool call before external execution begins.

```json
{
  "callId": "call_1",
  "tool": "read_file",
  "argumentsJson": "{\"path\":\"README.md\"}",
  "sideEffect": "read-only"
}
```

For any operation that may perform external side effects, this durable record MUST be committed before dispatch to the provider.

### 4.12 `tool/result`

Records the single authoritative terminal ToolOutcome.

```json
{
  "callId": "call_1",
  "invocationId": "inv_1",
  "outcome": {
    "kind": "success",
    "content": [{"type": "text", "text": "file contents"}]
  }
}
```

### 4.13 `step/ended`

```json
{
  "reason": "completed"
}
```

Allowed v0.1 reason vocabulary:

```text
completed
model-error
cancelled
blocked
max-tokens
```

### 4.14 `turn/ended`

```json
{
  "reason": "completed"
}
```

Allowed v0.1 reason vocabulary:

```text
completed
blocked
cancelled
error
max-tokens
```

### 4.15 `recovery/blocked`

Records that normal Agent execution cannot safely continue.

```json
{
  "kind": "unknown-tool-outcome",
  "callId": "call_7",
  "invocationId": "inv_7",
  "reason": "provider exited after dispatch of non-idempotent write"
}
```

### 4.16 `recovery/resolved`

Records the durable decision that clears a recovery block.

```json
{
  "blockedEventId": "evt_block_1",
  "resolution": "confirmed-success",
  "note": "verified by provider-specific reconciliation"
}
```

The exact recovery resolution vocabulary may expand, but clearing the ExecutionGate requires a durable resolution event.

## 5. Projection rules

SessionProjector produces at least:

- model-visible `Message[]` history;
- pending `next-turn` Inbox items;
- pending `next-step` Inbox items;
- most recent request context;
- unresolved recovery block, if any.

Projection MUST be a pure function of durable session state plus explicitly versioned projection rules.

## 6. Stream chunks

LLM text/reasoning/tool-call deltas are RuntimeEvents in v0.1. They MAY be displayed live but are not required for session reconstruction.

If a later version makes stream replay durable, it MUST preserve the authoritative final `assistant/message` semantics rather than requiring every consumer to reconstruct the final message from chunks.

<!-- END spec/session-events.md -->


---

<!-- BEGIN spec/agent-lifecycle.md -->

# Agent Lifecycle

**Status:** Draft v0.1

## 1. Agent identity

A durable `SessionId` is distinct from a live `AgentInstanceId`.

A process restart may create a new Agent instance that resumes the same durable session.

```text
Session ses_123
   |
   +-- process lifetime A -> AgentInstance agt_A
   |
   +-- process lifetime B -> AgentInstance agt_B
```

## 2. Actor ownership

Each live Agent is an actor. All state-changing operations are submitted as commands to the actor rather than by exposing mutable state references.

Minimum command semantics:

```text
Send(message, target, wakeup)
Cancel(cause, keepInbox)
Shutdown
```

Maintenance operations may exist internally but are not a stable v0.1 client protocol.

## 3. Agent phase

v0.1 phase model:

```text
Idle
Running
Maintenance
```

Conceptual structure:

```rust
pub enum AgentPhase {
    Idle { last_turn: TurnNo },
    Running { turn: TurnNo, step: StepNo },
    Maintenance,
}
```

The phase model deliberately does not include `WaitingForLlm`, `WaitingForTool`, or `WaitingForApproval`. Those are operations within a Running activity, not additional persistent state-machine phases.

## 4. ExecutionGate

Recovery safety is modeled separately from phase.

```text
Open
Blocked(recovery block)
```

An Agent can be `Idle + Blocked`. This means no normal turn is running, but Core refuses to start new turn execution until the recovery block is resolved.

## 5. Inbox

Inbox has two ordered boundaries:

```text
next-turn
next-step
```

### 5.1 `next-turn`

Contains ordinary future turn inputs, such as:

- user follow-up;
- external task continuation;
- subagent result in a future extension.

At a new turn boundary, Core claims at most the ordinary message(s) required by the chosen batching rule. v0.1 reference behavior is one ordinary `next-turn` message per turn proposal.

### 5.2 `next-step`

Contains context or steering intended for the nearest allowed step boundary.

Examples:

- user steering while an Agent is already running;
- injected runtime context;
- additional context resulting from a tool pipeline.

### 5.3 Convenience semantics

```text
followup = target next-turn, wakeup true
steer    = target next-step, wakeup true
inject   = target next-step, wakeup false
```

The stable primitive is `send(message, target, wakeup)`.

## 6. Wake behavior

A wake request on an Idle, Open Agent starts the driver.

A wake arriving while a driver is active is persisted to Inbox and consumed according to the nearest eligible boundary. It does not create a second driver.

A wake arriving after the active operation is already cancelled MUST NOT join the aborted activity. It is preserved for a later activity unless shutdown/disposal semantics require rejection.

## 7. Turn semantics

A turn is the durable interval opened by one waking input and closed when the Agent owes no immediate additional model work.

A turn contains zero or more steps.

Reference flow:

```text
turn/started
  claim input
  assemble prompt and tool catalog
  run pre-step policy

  if rejected or empty initial proposal:
      turn/ended

  otherwise:
      step/started
      append model-visible input
      model request
      assistant message
      optional tool batch
      step/ended

      if tool continuation or next-step input exists:
          next step
      else:
          turn/ended
```

## 8. Step semantics

A step contains one model request attempt sequence that eventually yields one authoritative assistant message or a terminal step failure, followed by zero or more logical ToolCalls produced by that assistant message.

Tool results that require another model request cause the turn to continue with another step.

## 9. Agent Loop pseudocode

```text
onWake:
    require ExecutionGate == Open
    acquire driver ownership

    while work is available:
        append turn/started
        target = next-turn

        loop:
            claim eligible Inbox work
            assemble prompt and tool definitions
            run pre-step policies

            if rejected:
                append turn/ended(blocked)
                break

            if first proposal is empty:
                append turn/ended(completed)
                break

            append step/started
            append entered user/message events

            derive model history
            build provider-neutral ModelRequest
            snapshot request
            append model/requested
            execute LLM attempt(s)

            if terminal model failure:
                append model/failed
                append step/ended(model-error)
                append turn/ended(error)
                break

            append assistant/message

            if assistant has tool calls:
                execute Tool pipeline and append tool outcomes

            append step/ended

            if ExecutionGate became Blocked:
                append turn/ended(blocked)
                break

            if next-step work exists or tools require continuation:
                target = next-step
                continue

            append turn/ended
            break

    release driver ownership
```

## 10. Cancellation

v0.1 cancellation causes:

```text
user
parent
timeout
policy
shutdown
disposed
```

Rules:

1. First accepted cause wins for the active operation.
2. Cancellation is scoped to the active operation.
3. Future inputs submitted after convergence are not automatically cancelled.
4. `keepInbox=false` MAY discard pending unstarted Inbox work according to the caller's command.
5. `shutdown` and `disposed` may reject future inputs because the Agent lifecycle itself is ending.

## 11. Resume

On resume, Core loads durable session state, reconstructs:

- current Inbox projection;
- last completed turn/step boundary;
- unresolved recovery gate;
- model history projection.

Normal execution starts only if the recovered structural invariants are valid and ExecutionGate is Open.

<!-- END spec/agent-lifecycle.md -->


---

<!-- BEGIN spec/tool-execution.md -->

# Tool Execution Specification

**Status:** Draft v0.1

## 1. ToolDefinition

Every registered Tool has a stable definition.

Conceptual Rust structure:

```rust
pub struct ToolDefinition {
    pub name: String,
    pub version: String,
    pub description: String,
    pub input_schema: JsonSchema,
    pub output_schema: Option<JsonSchema>,
    pub parallel_safe: bool,
    pub side_effect: SideEffectClass,
    pub default_timeout_ms: u64,
}
```

`name` identifies the model-visible tool in its scope. Name collision resolution is a registry concern; the final model-visible catalog MUST contain unique names.

## 2. SideEffectClass

v0.1 defines:

```text
read-only
idempotent-write
non-idempotent-write
```

### 2.1 `read-only`

The operation has no externally visible mutation. Transport failure MAY be automatically retried if policy allows.

### 2.2 `idempotent-write`

The operation mutates state but supports repetition using a stable idempotency key or equivalent provider guarantee.

Automatic retry is permitted only when the resolved provider capability declares compatible idempotency semantics.

### 2.3 `non-idempotent-write`

The operation may produce duplicate external side effects if repeated. If Core cannot prove whether dispatch completed, automatic retry is forbidden.

## 3. ToolCall versus ToolInvocation

A `ToolCall` is the logical request produced by the model.

A `ToolInvocation` is a concrete execution identity assigned by Core.

Conceptual structure:

```rust
pub struct ToolInvocation {
    pub invocation_id: InvocationId,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub arguments_json: String,
    pub attempt: u32,
    pub idempotency_key: String,
}
```

A logical ToolCall can experience multiple provider attempts without creating multiple authoritative terminal ToolOutcomes.

## 4. ToolOutcome

Core uses a richer terminal model than the model-visible `tool-result` block.

```text
success
error
denied
cancelled
unknown
```

Conceptual shape:

```rust
pub enum ToolOutcome {
    Success { content: Vec<ContentBlock> },
    Error { code: String, message: String, content: Vec<ContentBlock> },
    Denied { reason: String },
    Cancelled { cause: CancelCause },
    Unknown { reason: String },
}
```

`Unknown` means Core cannot determine the external effect. It is not equivalent to Error.

## 5. Execution pipeline

The normative logical order is:

```text
ToolCall
   |
resolve ToolDefinition
   |
validate arguments JSON against input schema
   |
record durable tool/call before external side effect
   |
pre-execute middleware
   |
PolicyEngine
   +-- deny
   +-- ask approval
   +-- allow
   |
execution middleware
   |
CapabilityRouter
   |
ProviderHost
   |
Tool Provider
   |
post-execute middleware
   |
normalize ToolOutcome
   |
record durable tool/result or recovery/blocked
```

## 6. PolicyDecision

v0.1 decisions:

```text
allow
deny
ask
```

Example wire-neutral shape:

```json
{
  "kind": "ask",
  "reason": "command writes outside workspace",
  "risk": "filesystem-write"
}
```

A deny decision is monotonic. Once a mandatory guard denies an operation, later middleware MUST NOT convert it to allow.

## 7. Identity protection

The following invocation identity fields are immutable after resolution:

- session identity;
- turn and step coordinates;
- ToolCallId;
- resolved tool name;
- side-effect classification.

Middleware MAY transform arguments or execution options only when explicitly permitted by policy. Middleware MUST NOT silently replace one tool with a different tool identity.

## 8. Argument validation

Before provider dispatch, Core MUST verify that `argumentsJson`:

1. is valid JSON;
2. validates against the resolved ToolDefinition input schema.

Provider-side validation MAY be repeated defensively but does not replace Core validation.

## 9. Parallel execution

An assistant message may contain multiple ToolCalls.

Core MAY execute calls concurrently only if all relevant constraints allow it, including `parallelSafe` and policy.

Durable terminal result ordering SHOULD follow original model ToolCall order even if completion order differs. This provides stable replay and avoids provider timing becoming durable conversation semantics.

Example:

```text
model order: A, B, C
completion:  B, C, A
durable:     result A, result B, result C
```

## 10. Idempotency

Core assigns a stable idempotency key for a logical idempotent operation. Retries of the same logical operation MUST reuse the same key when provider semantics require deduplication.

`attempt` increments for diagnostic and provider-policy purposes.

## 11. Crash recovery

### 11.1 Read-only

If durable `tool/call` exists and no authoritative terminal outcome exists, Core MAY retry automatically.

### 11.2 Idempotent write

Core MAY retry only if the provider contract guarantees use of the supplied stable idempotency key or an equivalent reconciliation mechanism.

### 11.3 Non-idempotent write

If external execution may have occurred and no authoritative result was durably committed, Core MUST:

1. record `recovery/blocked`;
2. set ExecutionGate to Blocked;
3. avoid automatic retry;
4. avoid continuing to a new model step or turn;
5. require explicit reconciliation or human/provider-specific resolution.

## 12. Provider failure ownership

ProviderHost reports process/transport/protocol failures. Tool Runtime maps those failures to retry, Error, or Unknown according to side-effect semantics and observed dispatch boundary.

ProviderHost MUST NOT blindly restart and replay a Tool invocation.

<!-- END spec/tool-execution.md -->


---

<!-- BEGIN spec/llm-protocol.md -->

# LLM Domain and Streaming Specification

**Status:** Draft v0.1

## 1. Provider-neutral ModelRequest

Harness Core builds one provider-neutral model request per attempt.

Conceptual Rust shape:

```rust
pub struct ModelRequest {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub provider: String,
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ModelToolSpec>,
    pub options: ModelOptions,
}
```

Provider-specific API conversion occurs only inside the selected LLM Provider.

## 2. ModelOptions

v0.1 requires only portable options that Core can represent without provider-specific types. Initial fields MAY include:

```text
maxOutputTokens
reasoningEffort
```

Provider-specific options are not part of the stable v0.1 contract unless explicitly standardized later.

An unsupported optional field must produce a structured capability/configuration error rather than silent semantic reinterpretation.

## 3. Tool specifications sent to the model

`ModelToolSpec` contains only model-visible information required for tool calling:

```text
name
description
inputSchema
```

Security policy, provider binding, side-effect classification and credential configuration are not model authority and need not be exposed as model tool schema.

## 4. Request construction

The logical inputs are:

```text
SessionProjector -> Message history
PromptRegistry   -> system prompt
ToolRegistry     -> model-visible tool schemas
Agent config     -> provider/model/options
```

The resulting `ModelRequest` is serialized and written to BlobStore before provider dispatch.

Core then commits `model/requested`, referencing the snapshot.

## 5. Exact request snapshot

The snapshot MUST contain the exact provider-neutral request object supplied to the Provider Host for that attempt.

It MUST be immutable after `model/requested` commit.

The snapshot allows later audit even if:

- prompt assembly rules change;
- tool descriptions change;
- projection code changes;
- provider adapters change.

## 6. StreamEvent

v0.1 stream event vocabulary:

```text
block-start
text-delta
reasoning-delta
tool-call-delta
block-end
usage
finish
```

Conceptual Rust enum:

```rust
pub enum StreamEvent {
    BlockStart { index: u32, block_type: BlockType },
    TextDelta { index: u32, text: String },
    ReasoningDelta { index: u32, text: String },
    ToolCallDelta {
        index: u32,
        call_id: ToolCallId,
        name: Option<String>,
        arguments_delta: String,
    },
    BlockEnd { index: u32, block: ContentBlock },
    Usage(TokenUsage),
    Finish(FinishEvent),
}
```

## 7. Stream ordering rules

For one stream:

1. Provider-assigned stream sequence numbers start at a positive integer and strictly increase by one.
2. Exactly one `finish` event MUST occur.
3. No event may follow `finish`.
4. `usage`, when present, MUST appear before `finish`.
5. `tool-call-delta.argumentsDelta` is raw JSON text fragments; the final ToolCall block contains a complete raw JSON string.
6. A `block-end` event carries the complete assembled block for that block index.

Protocol violations terminate the attempt with `PROVIDER_PROTOCOL_ERROR`.

## 8. Finish reasons

v0.1 finish reasons:

```text
completed
max-tokens
error
cancelled
```

`error` and `cancelled` MUST carry normalized failure information sufficient for Core to distinguish transport/provider failure from caller cancellation.

## 9. TokenUsage

Portable v0.1 usage fields:

```text
inputTokens
outputTokens
cacheReadTokens? 
cacheWriteTokens?
reasoningTokens?
```

When fields are unavailable from a provider they are omitted rather than fabricated.

## 10. Assembling the assistant message

Core owns stream assembly into the authoritative provider-neutral assistant Message.

Provider stream chunks are RuntimeEvents in v0.1. The final assembled message is recorded as durable `assistant/message`.

Core MUST NOT record a normal authoritative assistant message for an attempt that terminates with `error` or `cancelled`.

## 11. Retry ownership

An LLM Provider performs one provider attempt per Core request attempt unless the Provider Protocol explicitly standardizes transparent transport retry in the future.

Core owns logical retry policy. A retry is represented as another `model/requested` attempt and therefore remains observable and auditable.

## 12. Cancellation

Core cancellation propagates to the active provider operation through Provider Protocol. The first accepted cancellation cause remains authoritative.

Provider-side completion that races with cancellation is resolved by Core according to the operation state observed at the authoritative boundary; providers MUST NOT independently mutate durable outcome state.

<!-- END spec/llm-protocol.md -->


---

<!-- BEGIN spec/provider-protocol.md -->

# Provider Protocol v1

**Status:** Draft protocol 1.0

## 1. Purpose

Provider Protocol allows language-neutral out-of-process providers to supply Tool and LLM capabilities to Rust Harness Core without sharing a native ABI.

v1 transport is local stdio. Protocol semantics are designed so that other transports may be added later without changing the domain contracts.

## 2. Transport

Provider Host spawns a provider process and communicates using:

```text
JSON-RPC 2.0
NDJSON framing
stdin/stdout
UTF-8
```

Each line on stdin or stdout is one complete JSON-RPC message.

Provider stdout MUST contain only protocol messages. Provider diagnostics and logs MUST be written to stderr.

Malformed stdout is a protocol violation.

## 3. JSON conventions

- JSON object fields use lower camel case.
- Unknown optional fields SHOULD be ignored for forward compatibility.
- Unknown required method names receive the normal JSON-RPC method-not-found response.
- Stable string enums use lowercase and hyphenated tokens.
- Identifier fields are opaque strings.
- Sequence/counter fields are non-negative safe JSON integers.

## 4. Lifecycle state

ProviderHost models at least:

```text
starting
ready
unhealthy
stopping
stopped
```

Only `ready` providers may receive new capability operations.

## 5. Initialization

Core sends:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_1",
  "method": "provider.initialize",
  "params": {
    "protocolVersion": "1.0",
    "runtime": {
      "name": "harness",
      "version": "0.1.0"
    }
  }
}
```

Provider returns:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_1",
  "result": {
    "providerId": "python-ai",
    "providerVersion": "1.2.0",
    "protocolVersion": "1.0",
    "capabilities": []
  }
}
```

Provider MUST NOT be considered Ready before initialization succeeds and the returned manifest validates.

## 6. ProviderManifest

Conceptual shape:

```text
providerId
providerVersion
protocolVersion
capabilities[]
```

v1 capability kinds:

```text
tool
llm
```

### 6.1 Tool capability descriptor

Example:

```json
{
  "kind": "tool",
  "name": "read_file",
  "version": "1",
  "parallelSafe": true,
  "sideEffect": "read-only",
  "supportsIdempotencyKey": false
}
```

Tool schemas and descriptions MAY be supplied by Core composition or provider manifest according to deployment architecture, but the resolved definition in Core is authoritative before model exposure.

### 6.2 LLM capability descriptor

Example:

```json
{
  "kind": "llm",
  "provider": "provider-a",
  "models": ["model-x", "model-y"]
}
```

A provider MAY expose dynamic model availability; exact discovery policy beyond the manifest is deferred.

## 7. Tool invocation

Core request:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_42",
  "method": "tool.invoke",
  "params": {
    "operationId": "inv_123",
    "invocationId": "inv_123",
    "callId": "call_abc",
    "tool": "read_file",
    "argumentsJson": "{\"path\":\"README.md\"}",
    "attempt": 1,
    "idempotencyKey": "idem_xyz",
    "deadline": "2026-08-19T13:00:30Z"
  }
}
```

Successful protocol response:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_42",
  "result": {
    "outcome": {
      "kind": "success",
      "content": [
        {"type": "text", "text": "..."}
      ]
    }
  }
}
```

Provider-level Tool outcomes may include:

```text
success
error
cancelled
```

`denied` is normally owned by Core policy before dispatch. `unknown` is normally derived by Core from uncertain transport/crash boundaries rather than claimed by a normal provider response.

## 8. LLM start

Core request:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_50",
  "method": "llm.start",
  "params": {
    "operationId": "req_123",
    "request": {
      "requestId": "req_123",
      "sessionId": "ses_123",
      "provider": "provider-a",
      "model": "model-x",
      "messages": [],
      "tools": [],
      "options": {}
    },
    "deadline": "2026-08-19T13:05:00Z"
  }
}
```

Provider responds immediately after accepting the stream:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_50",
  "result": {
    "streamId": "str_123",
    "accepted": true
  }
}
```

The accepted stream then emits notifications.

## 9. LLM stream notification

```json
{
  "jsonrpc": "2.0",
  "method": "llm.event",
  "params": {
    "streamId": "str_123",
    "seq": 1,
    "event": {
      "type": "text-delta",
      "index": 0,
      "text": "Hello"
    }
  }
}
```

The final notification contains a `finish` event. No further event may use that streamId after finish.

## 10. Cancellation

Core sends a notification or request:

```json
{
  "jsonrpc": "2.0",
  "method": "capability.cancel",
  "params": {
    "operationId": "inv_123",
    "cause": {
      "kind": "user"
    }
  }
}
```

For Tools, `operationId` is the InvocationId. For LLM calls, it is the RequestId.

Cancellation is cooperative but provider implementations MUST make a best effort to abort external I/O and stop producing stream events promptly.

## 11. Ping

Core MAY send:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_ping_1",
  "method": "provider.ping",
  "params": {}
}
```

Provider responds successfully if its protocol loop is healthy.

Ping success does not prove downstream capability health.

## 12. Shutdown

Core requests graceful shutdown:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_shutdown_1",
  "method": "provider.shutdown",
  "params": {}
}
```

After acknowledgement, provider should stop accepting operations, cancel/drain owned operations according to implementation policy, and exit.

ProviderHost MAY forcibly terminate a provider that exceeds deployment shutdown limits.

## 13. Provider process failure

Unexpected process exit fails all active operations associated with that process as `PROVIDER_UNAVAILABLE` at the transport boundary.

ProviderHost MUST NOT automatically replay side-effecting operations. The owning Core subsystem interprets the failure.

## 14. Protocol errors

Examples of protocol violations:

- non-JSON bytes on stdout;
- duplicate or decreasing LLM stream sequence number;
- event after `finish`;
- unknown streamId;
- malformed ContentBlock;
- response id that cannot be correlated to an active request;
- invalid manifest;
- Tool result that does not satisfy required protocol structure.

Protocol violations produce `PROVIDER_PROTOCOL_ERROR` for affected operations and MAY mark the provider Unhealthy.

## 15. Versioning

v1 protocol version is `1.0`.

Compatibility policy:

- different major version: incompatible;
- same major with compatible minor revision: feature/capability negotiation may allow operation;
- implementations SHOULD ignore unknown optional fields;
- implementations MUST fail loudly when a required semantic capability is unsupported.

<!-- END spec/provider-protocol.md -->


---

<!-- BEGIN spec/storage.md -->

# Storage Specification

**Status:** Draft v0.1

## 1. Storage roles

v0.1 defines two storage abstractions:

```text
SessionStore
BlobStore
```

SessionStore owns durable ordered domain events. BlobStore owns large or binary immutable payloads referenced by durable events.

## 2. SessionStore semantic API

The exact Rust trait signature may evolve, but the semantic operations are fixed:

```text
create(sessionId, metadata)
append(sessionId, expectedSeq, events) -> newSeq
read(sessionId, fromSeq, limit)
head(sessionId)
fork(sourceSessionId, throughSeq, targetSessionId)
```

## 3. Create

`create` establishes an empty or initial event log for a unique SessionId.

Creating an existing SessionId MUST fail with `CONFLICT`.

A successful creation results in a durable `session/created` event.

## 4. Append and expected sequence

`append` is conditional on the caller's expected head sequence.

Example:

```text
committed head = 100
caller expectedSeq = 100
append events -> success, new head 103
```

If another writer has already advanced the log:

```text
committed head = 101
caller expectedSeq = 100
```

SessionStore MUST reject with `CONFLICT` and MUST NOT partially append the caller's batch.

Append batches MUST be atomic with respect to event visibility.

## 5. Read

`read` returns committed events in strict ascending EventSeq order.

Backends MUST NOT expose partially committed batches.

## 6. Head

`head` returns at least the current highest EventSeq and enough metadata to detect missing/corrupt sessions.

## 7. Fork

`fork(source, throughSeq, target)` creates a new durable session whose initial history is equivalent to the source session through the specified committed boundary.

The physical storage strategy may be:

- copied events;
- shared immutable segments;
- lineage reference plus projection;
- database-native snapshot.

Logical behavior must be equivalent.

Fork MUST NOT include source events after `throughSeq`.

## 8. Corruption handling

SessionStore MUST fail loudly on detected structural corruption, including:

- duplicate EventSeq;
- invalid gap according to backend's gap-free guarantee;
- malformed event envelope;
- impossible session identity mismatch;
- checksum failure when checksums are implemented.

Core surfaces durable structural corruption as `SESSION_CORRUPT` and MUST NOT continue normal Agent execution for that session.

## 9. BlobStore semantic API

Minimum operations:

```text
put(bytes, mediaType?) -> BlobRef
get(BlobId) -> bytes/stream
verify(BlobRef)
```

Additional operations such as delete, retention, leases, and remote URLs are implementation details outside v0.1.

## 10. Blob immutability

A BlobRef used by a committed SessionEvent MUST refer to immutable bytes.

Replacing bytes in place under the same committed BlobRef is forbidden.

## 11. Blob integrity

BlobStore verifies SHA-256 when writing or reading according to backend policy. A digest mismatch is a storage integrity failure.

## 12. Request snapshot durability

Before Core commits `model/requested`, the associated serialized ModelRequest snapshot MUST have been successfully persisted in BlobStore.

The event and blob need not be stored by the same backend, but Core must order operations so a committed event never intentionally references a blob that was never successfully persisted.

## 13. Initial local backend

The reference MVP SHOULD provide:

- an in-memory SessionStore for deterministic tests;
- a local durable SessionStore using SQLite or an equivalent embedded transactional database;
- a filesystem content-addressed BlobStore.

The specification does not require a specific production storage engine.

<!-- END spec/storage.md -->


---

<!-- BEGIN spec/error-model.md -->

# Error Model

**Status:** Draft v0.1

## 1. Purpose

Core may use rich Rust error types internally, but cross-subsystem and Provider Protocol boundaries require stable machine-readable error codes.

Errors are facts about failure, not retry decisions. Retry policy belongs to the owning domain subsystem.

## 2. Stable v0.1 error codes

```text
INVALID_ARGUMENT
NOT_FOUND
CONFLICT
PERMISSION_DENIED
CANCELLED
DEADLINE_EXCEEDED
PROVIDER_UNAVAILABLE
PROVIDER_PROTOCOL_ERROR
TOOL_EXECUTION_FAILED
MODEL_REQUEST_FAILED
SESSION_CORRUPT
UNKNOWN_OUTCOME
INTERNAL
```

## 3. Error payload

Portable error shape:

```json
{
  "code": "PROVIDER_UNAVAILABLE",
  "message": "provider process exited",
  "details": {}
}
```

`message` is diagnostic text and MUST NOT be used for machine routing.

`details` is optional structured data. Consumers SHOULD ignore unknown detail fields.

## 4. Code semantics

### `INVALID_ARGUMENT`

Input failed syntactic, schema, range, or semantic validation before execution.

### `NOT_FOUND`

The requested session, provider, capability, tool, blob or other addressed resource does not exist in the current scope.

### `CONFLICT`

The requested state mutation conflicts with current authoritative state, including SessionStore expected-sequence mismatch or duplicate identity creation.

### `PERMISSION_DENIED`

Core policy denied an operation. A provider should normally not originate this code for Core authorization decisions.

### `CANCELLED`

The operation was cancelled by a recognized cancellation cause before authoritative successful completion.

### `DEADLINE_EXCEEDED`

The operation exceeded the Core-defined deadline.

### `PROVIDER_UNAVAILABLE`

The provider process/service is unavailable, exited, failed initialization, or became unreachable.

### `PROVIDER_PROTOCOL_ERROR`

The provider violated the negotiated protocol.

### `TOOL_EXECUTION_FAILED`

A Tool executed and returned a normal terminal error outcome or Core normalized a safe execution failure to an error.

### `MODEL_REQUEST_FAILED`

A model attempt terminated unsuccessfully for a provider/model reason that is not more precisely represented by a transport-level code.

### `SESSION_CORRUPT`

Durable session state violates structural invariants and cannot safely drive normal execution.

### `UNKNOWN_OUTCOME`

Core cannot determine whether a potentially side-effecting operation completed. This is a recovery state, not a normal retryable error.

### `INTERNAL`

Unexpected Harness Core failure that does not fit a stable public code.

## 5. Retry rules

No error code is globally synonymous with retry.

Examples:

- `PROVIDER_UNAVAILABLE` during a read-only Tool may be retryable.
- the same `PROVIDER_UNAVAILABLE` after dispatch of a non-idempotent write may lead to `UNKNOWN_OUTCOME` and blocked recovery.
- `MODEL_REQUEST_FAILED` may or may not be retryable according to model policy and attempt limits.

## 6. Language exception names

Provider Protocol MUST NOT expose Python exception class names, Rust error type paths, Java stack types, or JavaScript Error subclass names as machine-routing codes.

Such data MAY be included as diagnostic detail when safe, but stable routing uses the error code vocabulary above.

<!-- END spec/error-model.md -->


---

<!-- BEGIN spec/security.md -->

# Security and Trust Boundaries

**Status:** Draft v0.1

## 1. Principle

The model and capability providers are not authorization authorities.

Harness Core owns policy decisions. Security is enforced through explicit boundaries rather than prompt instructions.

## 2. Logical security path

```text
Model-generated ToolCall
        |
        v
ToolRegistry resolution
        |
        v
argument validation
        |
        v
PolicyEngine
   +----+----+
   |         |
 deny       ask approval / allow
             |
             v
Provider boundary
             |
             v
OS/process/network sandbox where applicable
```

## 3. Model trust

Model output MUST be treated as untrusted input.

A system prompt saying that a tool should not perform an unsafe action is not a substitute for policy enforcement.

## 4. Provider trust

Providers are capability executors and may themselves be buggy or malicious.

Core SHOULD assume providers can:

- return malformed data;
- hang;
- crash;
- emit excessive output;
- misuse credentials made available to them;
- attempt filesystem/network actions beyond declared intent.

ProviderHost and deployment sandboxing SHOULD constrain these risks.

## 5. Provider permissions

A provider manifest or deployment configuration may declare required resources such as:

```text
network
filesystem
subprocess
credentials
gpu
```

A declaration is a request, not an authorization grant.

Deployment policy decides what resources the provider process receives.

## 6. Credentials

Credentials SHOULD be represented by Core-owned references rather than inserted directly into model-visible Tool arguments.

Preferred pattern:

```json
{
  "credentialRef": "cred:github/default"
}
```

Core CredentialResolver maps the reference to an actual secret only at the authorized invocation boundary.

Providers receive only credentials required for their resolved operation.

Secrets MUST NOT be copied into SessionEvents, ModelRequest snapshots, logs, or provider manifests unless the explicit product contract requires and protects such storage.

## 7. Message provenance

`MessageSource` records provenance for presentation and context semantics. It MUST NOT be used as sufficient proof of authorization or identity.

## 8. Tool policy

Policy decisions are:

```text
allow
deny
ask
```

Mandatory deny is monotonic. Later middleware cannot override it.

Approval is scoped to the exact invocation or policy-defined grant. A human approval UI is outside v0.1, but Core must retain ownership of the approval decision.

## 9. Side-effect classification

Every Tool declares a side-effect class. This is required for both safety and recovery.

A provider MUST NOT be allowed to downgrade a Core-resolved `non-idempotent-write` into a `read-only` declaration at invocation time.

## 10. Deadlines and resource limits

Core supplies operation deadlines. ProviderHost SHOULD enforce bounded execution and deployment-defined process limits.

Tool-specific OS sandboxing may be implemented by Rust or other providers, but policy ownership remains in Core.

## 11. Protocol isolation

For stdio providers:

- stdout is protocol-only;
- stderr is diagnostic logging;
- provider stdin is protocol input;
- provider process environment SHOULD contain the minimum required secrets;
- providers SHOULD run with least OS privilege practicable.

## 12. Blob access

A provider must not be able to fetch arbitrary BlobIds solely by guessing identifiers. Blob access is mediated by Core/provider-specific transport and scope authorization.

The exact blob transfer mechanism to out-of-process providers is deferred, but access-control ownership is fixed in Core.

<!-- END spec/security.md -->


---

<!-- BEGIN spec/conformance.md -->

# Conformance Requirements

**Status:** Draft v0.1

## 1. Purpose

A language-neutral protocol succeeds only if every SDK and provider interprets the same semantics identically. A shared conformance suite is therefore a required deliverable, not optional test infrastructure.

## 2. Provider conformance

Every Provider SDK and reference provider MUST pass tests covering at least the following.

### Initialization

- successful `provider.initialize`;
- incompatible major protocol version;
- invalid manifest;
- duplicate capability identity;
- provider exit during initialization.

### Framing

- one valid JSON-RPC object per NDJSON line;
- UTF-8 handling;
- malformed JSON on stdout;
- non-protocol debug text on stdout;
- stderr logging does not affect framing.

### Tool invocation

- success outcome;
- normal Tool error;
- cancellation;
- deadline exceeded;
- provider crash before dispatch acknowledgement;
- provider crash after invocation has begun;
- idempotency key preservation across retry attempts;
- raw `argumentsJson` preservation.

### LLM streaming

- ordered text stream;
- reasoning stream;
- interleaved content blocks;
- tool-call argument deltas;
- block-end complete block;
- usage before finish;
- exactly one finish;
- event after finish rejected;
- duplicate/decreasing stream seq rejected;
- unknown streamId rejected;
- provider crash during stream;
- caller cancellation during stream.

### Lifecycle

- ping success;
- graceful shutdown;
- forced termination after failed graceful shutdown;
- no new operations accepted after stopping begins.

## 3. SessionStore conformance

Every SessionStore backend MUST pass:

- create unique session;
- duplicate create returns conflict;
- atomic append batch;
- expected-sequence conflict;
- strict read order;
- no partial committed batch visibility;
- fork through exact boundary;
- fork excludes later source events;
- structural corruption detection where backend can detect corruption.

## 4. BlobStore conformance

Every BlobStore backend MUST pass:

- put/get byte equality;
- stable digest;
- immutable BlobRef behavior;
- digest verification;
- missing blob failure;
- content-type round trip when supplied.

## 5. Agent invariant tests

Reference Core tests MUST prove:

- no two drivers for one Agent;
- cancelled activity cannot consume post-cancel waking input;
- Inbox acknowledgement occurs only after durable enqueue;
- blocked ExecutionGate prevents new turn;
- one ToolCall cannot commit two terminal outcomes;
- provider crash during non-idempotent write cannot trigger automatic retry;
- model request snapshot exists before provider dispatch;
- model-visible history can be rebuilt from durable session state.

## 6. Deterministic fake providers

The repository SHOULD include deterministic fake Tool and LLM providers used by Core tests.

A minimum vertical-slice fixture should produce:

```text
User asks to read foo.txt
 -> fake LLM emits read_file call
 -> fake Tool returns "hello"
 -> fake LLM returns final answer
```

Expected durable event outline:

```text
session/created
inbox/enqueued
turn/started
inbox/claimed
step/started
user/message
model/requested
assistant/message
tool/call
tool/result
step/ended
step/started
model/requested
assistant/message
step/ended
turn/ended
```

Exact EventSeq values are assigned by the test store.

## 7. Cross-language matrix

Before Provider Protocol v1 is considered stable, the conformance suite SHOULD run against at least:

- Rust provider SDK;
- Python provider SDK;
- TypeScript provider SDK.

Go SDK may follow but must pass the same suite before being advertised as compatible.

<!-- END spec/conformance.md -->


---

<!-- BEGIN spec/rust-core-layout.md -->

# Rust Core Workspace Layout

**Status:** Draft v0.1 recommendation

## 1. Workspace

Recommended initial repository structure:

```text
harness/
├── Cargo.toml
├── crates/
│   ├── harness-types/
│   ├── harness-session/
│   ├── harness-tools/
│   ├── harness-llm/
│   ├── harness-agent/
│   ├── harness-provider-protocol/
│   ├── harness-provider-host/
│   ├── harness-runtime/
│   └── harness-storage-local/
├── sdk/
│   ├── python/
│   ├── typescript/
│   ├── go/
│   └── rust/
├── providers/
├── spec/
└── conformance/
```

This decomposition is intentionally smaller than a mature plugin ecosystem. v0.1 prioritizes semantic stability over package count.

## 2. `harness-types`

Contains low-level cross-subsystem value objects with minimal runtime dependencies:

```text
ids.rs
message.rs
blob.rs
error.rs
lib.rs
```

Responsibilities:

- branded identifier newtypes;
- Message and ContentBlock;
- BlobRef;
- stable portable errors;
- common counters/time wrappers.

It SHOULD NOT depend on Agent runtime, SessionStore implementations, process spawning, or Provider Host.

## 3. `harness-session`

Suggested files:

```text
event.rs
projector.rs
store.rs
lib.rs
```

Responsibilities:

- SessionEvent and payload types;
- SessionProjector;
- SessionStore semantic trait;
- reconstruction and structural validation.

It depends on `harness-types`.

## 4. `harness-tools`

Suggested files:

```text
definition.rs
invocation.rs
outcome.rs
policy.rs
registry.rs
lib.rs
```

Responsibilities:

- ToolDefinition;
- SideEffectClass;
- ToolInvocation;
- ToolOutcome;
- Tool registry and Core policy abstractions.

It depends on `harness-types`; it may depend on stable session interfaces only when required, but cyclic dependencies must be avoided.

## 5. `harness-llm`

Suggested files:

```text
request.rs
stream.rs
assembler.rs
registry.rs
lib.rs
```

Responsibilities:

- ModelRequest;
- ModelOptions;
- StreamEvent;
- stream assembly;
- provider-neutral LLM routing interfaces.

## 6. `harness-agent`

Suggested files:

```text
command.rs
inbox.rs
state.rs
loop_driver.rs
actor.rs
lib.rs
```

Responsibilities:

- Agent actor;
- AgentCommand;
- Inbox projection and delivery behavior;
- AgentPhase and ExecutionGate;
- turn/step driver;
- cancellation convergence.

This crate coordinates session, tools and LLM abstractions but should not contain provider process management.

## 7. `harness-provider-protocol`

Contains only wire-domain types and JSON-RPC method schemas.

Important rule:

> Wire types are not aliases of internal Rust domain types.

The crate provides explicit conversion boundaries so internal refactors do not automatically become cross-language breaking changes.

Suggested files:

```text
version.rs
wire.rs
manifest.rs
tool.rs
llm.rs
error.rs
lib.rs
```

## 8. `harness-provider-host`

Suggested files:

```text
process.rs
codec.rs
router.rs
stream_router.rs
supervisor.rs
state.rs
lib.rs
```

Responsibilities:

- process spawn;
- NDJSON framing;
- JSON-RPC correlation;
- initialization;
- operation routing;
- LLM stream routing;
- deadlines and cancellation delivery;
- provider state;
- shutdown and failure reporting.

It must not own Tool retry semantics.

## 9. `harness-runtime`

Application-level composition crate.

Responsibilities:

- registry wiring;
- scope hierarchy;
- provider binding;
- Agent Registry;
- startup/shutdown orchestration;
- application-facing façade.

A future CLI or server binary should depend primarily on this crate.

## 10. `harness-storage-local`

Reference local backends:

- MemorySessionStore for deterministic tests;
- SQLiteSessionStore or equivalent embedded durable store;
- FilesystemBlobStore.

Production remote storage adapters can live in separate crates later.

## 11. Dependency direction

Recommended conceptual direction:

```text
                  harness-types
                /      |       \
               v       v        v
          harness-session  harness-tools  harness-llm
                 \         |        /
                  \        |       /
                   v       v      v
                    harness-agent
                         |
                         v
                   harness-runtime

harness-provider-protocol
            |
            v
harness-provider-host ---------> harness-runtime

harness-storage-local ---------> session/blob abstractions
```

Exact Cargo edges may differ to avoid cycles. The invariant is that low-level domain crates do not depend on high-level runtime composition.

## 12. Async-runtime boundary

Domain/value crates SHOULD avoid binding themselves to a specific async executor unless necessary.

Async/process concerns belong primarily in:

- `harness-agent`;
- `harness-provider-host`;
- `harness-runtime`;
- concrete storage implementations.

This keeps protocol and durable-domain types portable and testable.

## 13. First implementation vertical slice

The first executable slice SHOULD contain only:

```text
harness-types
MemorySessionStore
SessionProjector
Agent actor
Agent Loop
Fake in-process LLM
ToolRegistry
Fake read_file Tool
Memory BlobStore
```

The slice is complete when it can deterministically execute:

```text
user -> LLM -> tool -> LLM -> final answer
```

and produce the expected durable event sequence before any out-of-process provider implementation is added.

<!-- END spec/rust-core-layout.md -->
