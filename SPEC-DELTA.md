
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

# Specification Delta - Batch 05

This file lists normative changes introduced after Batch 04.

## D-01: Tokio reference runtime selection

The Rust reference live Agent implementation uses Tokio for task execution and in-process
mailbox primitives. This does not make Tokio part of the cross-language architecture or durable
protocol. `harness-types` and `harness-session` remain executor-neutral.

## D-02: Single owner / cloneable handle

`AgentActor` is the singular mutable owner and must not be cloneable. Clients clone
`AgentHandle`, which only contains message-passing capability.

## D-03: Durable acknowledgement boundary

For `Send`, acknowledgement occurs after:

```text
inbox/enqueued validated
-> conditional SessionStore append committed
-> Store commit result verified
-> actor projection/resume view updated
-> wake latch updated
-> SendReceipt returned
```

Mailbox insertion by itself is not durable acceptance.

## D-04: Bootstrap snapshot retention

`AgentBootstrap` retains the exact `Vec<SessionEvent>` prefix used to derive its projection.
The actor validates future batches against this local snapshot. It does not re-read after every
write because a read could silently adopt events from a competing writer.

A competing write is detected only through `expected_seq` and is treated as ownership loss.

## D-05: Ownership loss is terminal

A live actor that observes `SessionStore::Conflict` terminates after returning the command error.
It does not rebase itself. A supervisor may explicitly create a fresh Agent instance later.

## D-06: EventId uniqueness

Committed EventIds must be unique within one Session. `V1SessionProjector` rejects duplicates,
and the reference `MemorySessionStore` rejects a duplicate EventId before commit. This also lets
the actor reject a broken EventId source before storage commit during local prevalidation.

## D-07: Wake latch

`wakeup=true` becomes `AgentState.wake_requested = true` only after durable enqueue. The latch is
coalescing and is reconstructed from pending Inbox items whose durable enqueue carried
`wakeup=true`.

Batch 05 does not consume the latch; Batch 06 will connect it to the Turn/Step driver.

## D-08: Startup recovery convergence

Before publishing `AgentHandle`, Batch 05 automatically performs recovery steps that do not call
external capabilities.

### Interrupted model request

Append `model/failed(MODEL_REQUEST_FAILED)` and re-project. This removes the ambiguous pending model
attempt while leaving the open step for the future driver.

### Unknown non-idempotent Tool outcome

Atomically append:

```text
recovery/blocked
step/ended(blocked)
turn/ended(blocked)
```

The live actor is then exposed with a blocked ExecutionGate and quiescent durable lifecycle.

## D-09: Cancel remains deferred

`AgentCommand::Cancel` remains in the command vocabulary but returns an explicit unsupported
operation until the driver owns cancellation tokens and durable convergence semantics.

# Batch 06 Spec Delta

This file records only decisions added or tightened by Batch 06 relative to the Batch 05 specification baseline.

## 1. Deterministic driver is separated from capability execution

The Agent driver is split conceptually into:

```text
deterministic durable transition
        |
        v
external-operation boundary
        |
        v
capability execution
```

Batch 06 implements only the first part and stops at `ReadyForModel`.

The deterministic part may write SessionEvents but MUST NOT invoke an LLM, Tool provider, ProviderHost, approval service, or other external capability.

## 2. `ReadyForModel` is not durable state

No `ready-for-model` SessionEvent is added.

The Rust implementation derives the boundary from:

```text
AgentPhase::Running(turn, step)
ResumeDecision::ContinueOpenStep(same turn, same step)
SessionProjection.open_step_assistant_message == None
```

This allows process restart to rediscover the same boundary by replay.

## 3. Projection gains an open-step assistant marker

`SessionProjection` now exposes:

```rust
pub open_step_assistant_message: Option<MessageId>
```

The marker is set by `assistant/message` and cleared by `step/ended`.

This is not a durable schema addition. It is replay-derived state required to distinguish these two structurally different cases:

```text
open step before assistant
    -> eligible for ReadyForModel

open step after assistant
    -> must not issue another model request
```

Batch 06 defers the second case to the post-assistant convergence batch.

## 4. Step-entry Inbox mutation is atomic with model-visible entry

When Inbox input is consumed into a step, `inbox/claimed` and the corresponding `user/message` MUST be committed in the same atomic SessionStore append batch.

For a newly opened turn:

```text
turn/started
inbox/claimed(next-turn)?
step/started
user/message(next-turn)?
[inbox/claimed(next-step), user/message(next-step)]*
```

This eliminates the crash state:

```text
inbox item durably claimed
but model-visible user/message missing
```

## 5. Inbox batching rule is frozen for v0.1 reference behavior

At a new turn's first step:

```text
claim <= 1 next-turn
claim all currently pending next-step
```

At an already open pre-model step:

```text
claim all currently pending next-step
claim 0 next-turn
```

Within model-visible history, the primary next-turn message precedes next-step messages; next-step FIFO order is preserved.

## 6. Open-turn continuation rule

For `ContinueOpenTurn`:

- if the turn has never started a step, one `next-turn` message may be used as the primary input;
- if the turn has already completed at least one step, only `next-step` work can continue that turn;
- if no such continuation exists, Core appends `turn/ended(completed)`;
- a pending `next-turn` item remains for a future turn.

## 7. Wake latch becomes fully projection-derived after commits

`wake_requested` is refreshed after every committed actor mutation from remaining pending Inbox items whose `wakeup` flag is true.

Consequences:

```text
accepted waking input
    -> wake_requested = true

same input claimed and no other wake pending
    -> wake_requested = false

future next-turn wake remains queued while current step runs
    -> wake_requested remains true
```

## 8. Handle publication occurs after deterministic startup convergence

`spawn_agent` now performs:

```text
bootstrap
recovery convergence
deterministic driver convergence
publish AgentHandle
```

A restarted Session with durable waking Inbox input may therefore already be at `ReadyForModel` when the caller first receives its handle.

No external provider call occurs before handle publication.

## 9. Agent Inbox role constraint

The Rust v0.1 actor rejects `Send` messages whose role is not `Role::User` before `inbox/enqueued` is committed.

This prevents a durably accepted Inbox item from later poisoning `user/message` projection during driver entry.

## 10. Mailbox responsiveness is now an architectural requirement

Future external LLM/Tool waits MUST NOT require the mutable actor owner to remain unavailable for mailbox processing for the duration of the wait.

Batch 06 establishes the park boundary necessary for later capability operations to run without redefining Turn/Step persistence.

## 11. Deferred semantics

Batch 06 intentionally does not decide:

- how `ModelRequest` is assembled;
- how an LLM operation is spawned and correlated back to the actor;
- how a post-assistant open step is finalized;
- how Tool-result continuation opens the next step;
- how active operation cancellation converges durable state.

These remain explicit future work rather than hidden assumptions in the first driver implementation.