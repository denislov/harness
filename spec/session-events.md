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

Records the logical ToolCall emitted by the model. It does **not** mean a provider dispatch has occurred.

```json
{
  "callId": "call_1",
  "tool": "read_file",
  "argumentsJson": "{\"path\":\"README.md\"}",
  "sideEffect": "read-only"
}
```

This event MUST be committed before Core starts policy/execution work for the logical ToolCall. If process recovery sees `tool/call` without `tool/dispatched`, Core may safely restart the tool pipeline from a pre-dispatch boundary.

### 4.12 `tool/dispatched`

Records the durable external-dispatch boundary for one concrete Tool attempt. This event MUST be committed **before** Core allows the invocation to cross the provider/capability boundary.

```json
{
  "callId": "call_1",
  "invocationId": "inv_1",
  "providerId": "prv_tools",
  "attempt": 1,
  "idempotencyKey": "idem_call_1"
}
```

The presence of `tool/dispatched` means the external effect **may have occurred** even if no terminal `tool/result` exists. This event is therefore the authoritative crash-recovery dispatch marker.

For retries in v0.1:

- `attempt` starts at `1` and increments exactly by one;
- every retry uses a new `InvocationId`;
- retries preserve `providerId`;
- retries preserve the same `idempotencyKey`;
- a `non-idempotent-write` Tool MUST NOT be automatically redispatched.

### 4.13 `tool/result`

Records the single authoritative terminal ToolOutcome. When a `tool/dispatched` event exists, `invocationId` MUST match the latest durable dispatch for the logical ToolCall.

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

Pre-dispatch outcomes such as policy denial, cancellation, or execution setup failure MAY terminate a ToolCall without `tool/dispatched`. `success` and `unknown` outcomes require a prior durable dispatch.

### 4.14 `step/ended`

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

### 4.15 `turn/ended`

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

### 4.16 `recovery/blocked`

Records that normal Agent execution cannot safely continue.

```json
{
  "kind": "unknown-tool-outcome",
  "callId": "call_7",
  "invocationId": "inv_7",
  "reason": "provider exited after dispatch of non-idempotent write"
}
```

### 4.17 `recovery/resolved`

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
- pending logical ToolCalls and their latest durable dispatches;
- unresolved recovery block, if any.

Projection MUST be a pure function of durable session state plus explicitly versioned projection rules.

## 6. Stream chunks

LLM text/reasoning/tool-call deltas are RuntimeEvents in v0.1. They MAY be displayed live but are not required for session reconstruction.

If a later version makes stream replay durable, it MUST preserve the authoritative final `assistant/message` semantics rather than requiring every consumer to reconstruct the final message from chunks.
