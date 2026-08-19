
# Batch 01 Spec Delta

This file records implementation-level refinements discovered while translating Spec v0.1 into Rust APIs.

## D-01: Persisted cross-subsystem values live in `harness-types`

The original workspace recommendation assigned `SideEffectClass` and `ToolOutcome` to `harness-tools` and token usage to `harness-llm`.

Batch 01 moves the following value objects to `harness-types`:

- `InboxTarget`
- `SideEffectClass`
- `CancelCause`
- `ToolOutcome`
- `TokenUsage`

Reason: these values are embedded in durable `SessionEvent` payloads. Keeping them in execution crates would force `harness-session` to depend upward on `harness-tools` / `harness-llm`. Execution logic, registries, retry policy, stream assembly, and provider routing remain outside `harness-types`.

## D-02: Distinguish uncommitted and committed events

The storage spec states that `EventSeq` is assigned at commit time. Therefore Rust uses two types:

```text
NewSessionEvent
    no session_id
    no seq

SessionEvent
    session_id
    seq
    schema_version
```

`SessionEvent::committed` is the conversion boundary used by storage implementations after sequence allocation.

## D-03: `SessionStore::append` returns committed events

The semantic spec only requires `append(...) -> newSeq`. The Rust API returns:

```text
AppendResult {
    new_head,
    committed,
}
```

This lets the Agent actor update in-memory projections from the exact committed event identities and assigned sequences without synthesizing them a second time.

## D-04: Canonical SessionEvent JSON uses explicit Serde implementation

Canonical durable JSON remains:

```json
{
  "schemaVersion": 1,
  "eventId": "evt_...",
  "sessionId": "ses_...",
  "seq": 1,
  "time": "2026-08-19T13:00:00Z",
  "type": "session/created",
  "data": {}
}
```

The Rust implementation does not expose `payload` as an additional JSON nesting level.

## D-05: Concrete `SessionProjector` is intentionally deferred

The current spec freezes that model-visible state must be reconstructable, but it does not yet completely freeze the exact durable mapping from rich `ToolOutcome` to the provider-neutral model-visible `tool-result` message.

Batch 01 therefore freezes:

- `SessionProjector` trait;
- `SessionProjection` output shape;
- Inbox and recovery projection types;

but does not invent a concrete v0.1 projector rule for ToolResult.

This item should be resolved before the first Tool-enabled Agent vertical slice.

# Spec Delta — Batch 03

This file records concrete v0.1 projection rules that were intentionally left open in the original specification.

## 1. Projection is explicitly versioned

The concrete initial rules are named `V1SessionProjector` and expose projection version `1`.

A future change that alters model-visible history reconstruction MUST introduce a new projection version rather than silently changing old-session semantics.

## 2. Model-visible message projection

`user/message` contributes its durable Message directly to model history.

`assistant/message` contributes its durable Message directly to model history.

`tool/result` contributes a deterministic synthetic Message:

```text
role   = user
source = plugin
id     = "msg_projected_tool_result_" + tool-result EventId
```

Its only top-level block is a provider-neutral `tool-result` block for the original `ToolCallId`.

The synthetic ID is deterministic projection state, not a new durable event identity. Consumers MUST treat it as opaque.

## 3. ToolOutcome -> ToolResult rendering

V1 mapping is fixed as follows.

### Success

- preserve `content` exactly;
- `isError = false`.

### Error

- prepend text `Tool error [<code>]: <message>`;
- append durable outcome content unchanged;
- `isError = true`.

### Denied

- content: `Tool execution denied: <reason>`;
- `isError = true`.

### Cancelled

- content: `Tool execution cancelled: <cause>` where cause uses the v0.1 kebab-case vocabulary;
- `isError = true`.

### Unknown

- content: `Tool execution outcome is unknown: <reason>`;
- `isError = true`.

Policy sanitization of provider/tool errors MUST happen before committing `ToolOutcome` if the raw text must not become model-visible.

## 4. ToolCall structural correlation

A durable `tool/call` MUST correspond to a `tool-call` ContentBlock in the authoritative `assistant/message` of the same open step.

The following MUST match exactly:

- `ToolCallId`;
- tool name;
- `argumentsJson` raw JSON text.

Every tool call announced by an assistant message MUST have a corresponding durable `tool/call` before the step can close.

## 5. Pending operations are projected durable state

`SessionProjection` includes:

- `pending_model_request`;
- `pending_tool_calls`.

An open pending model request or open turn/step at end-of-log is not by itself corruption; it can represent crash interruption.

A `step/ended` event, however, MUST NOT close a step while a model request is still pending.

## 6. Recovery block requirements

V1 `recovery/blocked(kind = unknown-tool-outcome)` requires:

- an existing pending ToolCall in the same turn/step;
- `sideEffect = non-idempotent-write`;
- no previously unresolved recovery block.

While a recovery block exists, a new `turn/started` is invalid.

## 7. Late reconciled ToolResult

A blocked step/turn may be closed before the external side effect is reconciled.

Therefore V1 permits a `tool/result` to arrive after its original Step/Turn has ended only when all of the following hold:

- the ToolCall is still pending;
- there is an active `recovery/blocked` for that ToolCall;
- `ToolCallId`, `InvocationId`, turn and step all match the recovery block/original call.

The late result remains logically attached to the original turn/step even though its `seq` is later than `turn/ended`.

## 8. Clearing ExecutionGate requires ToolResult first

`recovery/resolved` does not itself invent a model-visible ToolResult.

Before a recovery block may be cleared, Core MUST commit an authoritative `tool/result` for the blocked ToolCall. The reconciled outcome may be Success, Error, Denied, Cancelled, or Unknown as appropriate.

This rule prevents model history from containing an assistant tool-call with no terminal tool-result after recovery has been declared complete.

Canonical recovery order:

```text
tool/call
provider dispatch
<crash>
recovery/blocked
step/ended(blocked)
turn/ended(blocked)

<reconciliation>
tool/result        # tagged with original turn/step
recovery/resolved
```

## 9. Stream structural validation

V1 projector rejects:

- empty committed sessions;
- first event other than `session/created`;
- non-contiguous EventSeq;
- mixed SessionIds;
- repeated `session/created`;
- nested turns or steps;
- events whose turn/step do not match the currently open lifecycle boundary;
- duplicate Inbox enqueues for one MessageId;
- ToolResult without prior ToolCall;
- duplicate durable ToolCallId;
- duplicate terminal InvocationId in `tool/result` events;
- assistant/model failure whose RequestId does not match the pending request.

These checks make SessionProjector an early corruption boundary before Agent resume.

# Batch 04 Specification Delta

This file records the normative changes introduced while turning the v0.1 design into a recoverable Rust implementation. Full updated versions of affected spec files are included under `spec/`.

## 1. `tool/call` is not a dispatch marker

Previous drafts said `tool/call` must be committed before provider execution but did not durably identify the exact dispatch boundary.

That is insufficient for crash recovery. A process may crash after committing `tool/call` but before dispatch, or after dispatch but before `tool/result`; those states require different behavior for non-idempotent operations.

Batch 04 therefore freezes:

```text
tool/call
    = logical model ToolCall is durable

tool/dispatched
    = provider/capability boundary may have been crossed
```

## 2. New durable event: `tool/dispatched`

Data:

```json
{
  "callId": "call_1",
  "invocationId": "inv_1",
  "providerId": "prv_tools",
  "attempt": 1,
  "idempotencyKey": "idem_call_1"
}
```

The event MUST be committed before provider dispatch.

## 3. Tool retry identity

One logical ToolCall has stable `ToolCallId` and `IdempotencyKey`.

Each retry:

- uses a new `InvocationId`;
- increments `attempt` exactly by one;
- retains the same `ProviderId` in v0.1;
- retains the same `IdempotencyKey`.

`non-idempotent-write` may not be automatically redispatched.

## 4. Crash recovery interpretation

### No dispatch marker

```text
tool/call
(no tool/dispatched)
(no tool/result)
```

Core may restart the Tool pipeline from before dispatch, including for a non-idempotent Tool, because no external dispatch is durably known.

### Read-only dispatch without result

Automatic retry is allowed by recovery semantics.

### Idempotent-write dispatch without result

Retry is only a candidate until the resolved provider capability verifies a compatible idempotency guarantee.

### Non-idempotent-write dispatch without result

Core must persist `recovery/blocked` and must not auto-retry.

## 5. Single unresolved non-idempotent dispatch in v0.1

The scheduler must ensure at most one unresolved `non-idempotent-write` provider dispatch for one Agent. This keeps the current single `RecoveryBlock` model sound.

This is intentionally conservative. A future version may generalize ExecutionGate to a collection of recovery blocks.

## 6. Recovery ordering must not depend on opaque IDs

`PendingToolCall` now includes `call_seq` and `PendingToolDispatch` includes `dispatch_seq`.

Recovery actions are ordered by durable event sequence, never by `ToolCallId` lexical ordering. This enforces invariant I-20.

## 7. AgentPhase is process-local

`AgentPhase` represents live driver ownership, not the durable turn/step cursor.

After a process restart, the new Agent instance begins process-locally Idle. If the durable log contains an unfinished turn or step, that state is represented by `ResumeDecision`.

Therefore normal new-turn permission is:

```text
AgentPhase == Idle
AND ExecutionGate == Open
AND ResumeDecision == Clean
```

## 8. ResumeDecision

Batch 04 freezes the initial recovery classification vocabulary:

```text
Clean
ContinueOpenTurn
ContinueOpenStep
RecoverInterruptedModelRequest
RecoverToolBatch
PersistRecoveryBlock
Blocked
```

This is an internal Rust domain contract, not Provider Protocol wire vocabulary.

## 9. Bootstrap snapshot boundary

`AgentBootstrapper` reads `SessionStore.head()` first. That head is the snapshot boundary for projection and recovery analysis.

Events appended concurrently after that head are ignored by the bootstrap snapshot. A later actor append using `expected_seq` detects the ownership conflict. Cross-process Agent leasing remains future work.

## 10. Projection version

`SESSION_PROJECTION_VERSION_V1` remains `1` because no production Agent runtime has emitted the previous incomplete Tool dispatch semantics. This project is still within the draft v0.1 contract stabilization window.

Once a released runtime persists v1 Sessions, future breaking interpretation changes must use an explicit projection/schema migration rather than silently modifying v1.
