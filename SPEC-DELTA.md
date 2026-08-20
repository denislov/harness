
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

# Batch 07 Spec Delta

## 1. New generic BlobStore crate

The architecture previously specified `BlobStore` semantically but did not assign it a clean Rust crate. Batch 07 introduces `harness-storage` rather than placing the abstraction in `harness-llm` or making `harness-agent` depend on the concrete `harness-storage-local` crate.

`SessionStore` remains in `harness-session` for v0.1. This avoids a disruptive migration of the already-stable event-log API.

## 2. Stream sequence becomes a branded counter

`StreamSeq` is added to `harness-types` and follows the same JavaScript-safe integer bound as other cross-language counters. Normalized LLM streams start at 1 and increment by exactly one.

## 3. In-process LLM seam is not the wire protocol

`LlmProvider` is the Rust Core capability seam. It returns a runtime-neutral `futures_core::Stream` of normalized events. The future JSON-RPC Provider Host will adapt process/wire messages into this seam.

No JSON-RPC types are introduced in Batch 07.

## 4. Exact request snapshot ordering is executable

The Batch 07 Agent enforces:

```text
ModelRequest build
    -> JSON snapshot serialization
    -> BlobStore.put
    -> model/requested commit
    -> provider task spawn
```

A committed `model/requested` therefore never intentionally references a snapshot that Core failed to persist.

## 5. Durable recovery vs live operation overlay

Immediately after `model/requested`, the durable projection correctly says the request would require interrupted-request recovery after process loss. The live actor additionally records:

```text
ActiveAgentOperation::Model
```

The deterministic driver does nothing while this overlay exists. On process restart the overlay is gone, so the existing RecoveryAnalyzer behavior becomes authoritative without introducing a new durable event.

## 6. Mailbox responsiveness is now an invariant

The Agent actor never awaits provider stream I/O in its receive loop. One spawned task consumes and validates the model stream and submits one internal completion message.

This is the architectural prerequisite for later `steer`, cancellation, timeout, and shutdown races.

## 7. Batch 07 model failure policy

A provider stream failure or normalized `finish(error)` produces:

```text
model/failed
step/ended(model-error)
turn/ended(error)
```

A normalized cancelled finish produces the cancelled Step/Turn reasons.

Batch 07 does not implement ordinary intra-process model retry policy. An interrupted request recovered after process restart may enter a new attempt because the open step becomes ReadyForModel again; its attempt number is incremented from durable history.

## 8. Tool-call assistant boundary

If the assistant contains no ToolCall block, Batch 07 can end the step immediately.

If ToolCall blocks are present, only `assistant/message` is committed and the open step is left for Batch 08. This is intentional: side-effect classification and `tool/call` persistence require `ToolDefinition` / ToolRegistry, which do not belong in the LLM layer.

The existing `open_step_assistant_message` projection prevents duplicate model requests while this state is deferred.

## 9. Known deferred items

Batch 07 intentionally does not freeze:

- cancellation signal transport into `LlmProvider`;
- timeout policy;
- runtime chunk/UI event fanout;
- model retry policy beyond restart convergence;
- dynamic PromptRegistry assembly;
- ToolRegistry-derived model tool schemas;
- out-of-process Provider Protocol;
- durable representation of provider-specific response metadata.

These omissions do not weaken the durable ordering or single-writer invariants introduced here.

## 10. Batch 06 clippy cleanup

The unused test helper `id<T>()` in `harness-agent/src/loop_driver.rs` is removed. Batch 07 expects `cargo clippy --workspace --all-targets -- -D warnings` to be part of acceptance.

## 11. Finish/cancellation normalization

`finish(cancelled)` is valid only with `ErrorCode::Cancelled`; `finish(error)` may carry other normalized error codes but not `Cancelled`. The in-process provider stream is required to terminate after its terminal Finish event. Timeout enforcement is still deferred.

# Batch 08 Spec Delta

Batch 08 is based on `denislov/harness` commit `3743501c59baa8be891c7f22f3482ab5b07a92c3`.

## 1. Tool runtime becomes executable

`harness-tools` now owns provider-neutral Tool definitions and execution seams. The crate remains below Agent and does not gain SessionStore or Tokio authority.

A Tool registration combines:

```text
ToolDefinition
+ ToolExecutor
+ ToolArgumentValidator
```

Policy remains separately composed through `ToolPolicy`.

## 2. Model-visible Tool catalog authority

When both LLM and Tool runtimes are attached, ToolRegistry is the sole source for `ModelToolSpec` generation. `ModelRequestConfig.tools` must be empty in this mode.

Only:

```text
name
description
inputSchema
```

are model-visible. Provider binding, side-effect class, idempotency support and policy remain Core authority.

## 3. Logical calls precede dispatch

For one authoritative assistant message, Core persists every announced `tool/call` before dispatching the first Tool.

This freezes the logical call set before external side effects begin and allows recovery to reconstruct the batch without relying on process memory.

## 4. Dispatch boundary

For an allowed Tool:

```text
validate
policy
assign InvocationId / idempotencyKey
commit tool/dispatched
set ActiveAgentOperation::Tool
spawn Tool future
```

The Tool future has no SessionStore access. It returns one normalized completion through the Agent mailbox.

## 5. Ordering

Batch 08 executes ToolCalls sequentially in authoritative assistant-message order. This deliberately sacrifices parallel throughput while the first durable Tool semantics are being exercised.

`parallelSafe` remains in ToolDefinition for a later scheduler.

## 6. Policy and validation

Unknown Tool, validation failure, and policy denial terminate before provider dispatch and produce a model-visible Tool result.

`PolicyDecision::Ask` fails closed in Batch 08 because approval UI/service semantics are not implemented yet. It is represented as a denied Tool outcome and MUST NOT dispatch.

## 7. Idempotency contract

`idempotent-write` Tool registration requires keyed idempotency support from its executor.

Automatic retry preserves:

```text
ToolCallId
providerId
idempotencyKey
```

and changes:

```text
InvocationId
attempt += 1
```

`non-idempotent-write` remains non-retryable after a durable dispatch without terminal result.

## 8. Retry exhaustion

After the configured automatic attempt budget:

```text
read-only
    -> ToolOutcome::Error(TOOL_RETRY_EXHAUSTED)

idempotent-write
    -> ToolOutcome::Unknown

non-idempotent-write
    -> recovery block path, never automatic retry exhaustion
```

A terminal result after a previous dispatch uses the latest dispatch InvocationId.

## 9. Tool continuation

A new `StepEndReason` value is introduced:

```text
tool-continuation
```

It is durable and means:

> this Turn owes another model step because Tool results have become model-visible.

Therefore:

```text
tool/result*
step/ended(tool-continuation)
step/started(next)
model/requested
```

is valid even when no Inbox item is pending.

## 10. Projection additions

SessionProjector reconstructs open-step ToolCall order plus recorded/completed sets. Tool scheduling never sorts opaque ToolCallIds to determine execution order.

`LifecycleProjection.last_ended_step_reason` allows the deterministic driver to distinguish an ordinary completed step from an immediate Tool-continuation boundary.

## 11. Spec housekeeping

The Batch 07 additions had duplicated section/invariant numbers in `agent-lifecycle.md` and `invariants.md`. Batch 08 renumbers those appendices before adding new Tool sections; this is editorial and does not change Batch 07 semantics.

# Batch 09 Spec Delta

## D-01 — Approval is durable state

A Tool policy `ask` decision creates `approval/requested`. `ApprovalId` is unique within one Session. The Agent parks until a matching `approval/resolved` is committed.

Rationale: approval state affects authorization and execution ordering, so it must survive process restart and be replayable from Session history.

## D-02 — Durable approval resolution is authoritative

`approval/resolved(allow)` authorizes the same logical ToolCall after restart without re-running an interactive approval prompt. `approval/resolved(deny)` is monotonic and may never be followed by `tool/dispatched` for that call.

## D-03 — Approval is a driver boundary, not an Agent phase

The live phase remains `Running`. Approval waiting is expressed through:

```text
ResumeDecision::AwaitingApproval
AgentDriverBoundary::AwaitingApproval
```

No external capability future is owned at this boundary.

## D-04 — Cancellation acknowledgement is a durable boundary

The actor builds and commits its cancellation convergence batch before aborting the process-local LLM/Tool task. If the append fails, cancellation is not acknowledged and the live operation remains owned.

This prevents a successful API acknowledgement from representing only a best-effort task abort with no durable terminal state.

## D-05 — Cancellation preserves Tool side-effect uncertainty

After `tool/dispatched`:

```text
read-only            -> terminal cancelled result is permitted
idempotent-write     -> terminal unknown result when caller stops retrying
non-idempotent-write -> recovery/blocked
```

The non-idempotent path never treats task cancellation as proof that the external mutation did not occur.

## D-06 — Inbox retention is explicit

`Cancel { keep_inbox: false }` emits `inbox/discarded` for every pending unclaimed next-turn and next-step item. `keep_inbox: true` leaves those events pending.

## D-07 — Core owns attempt timeout

LLM and Tool provider attempts are wrapped by the Rust Core runtime timeout layer.

```text
LLM: AgentLlmRuntime.timeout_ms
Tool: ToolDefinition.default_timeout_ms
```

Tool timeout is an ambiguous provider-attempt failure after `tool/dispatched`, so existing recovery logic decides retry/block behavior. The Tokio runtime hosting `harness-agent` must enable the Tokio time driver.

## D-08 — Stale completions are filtered by durable eligibility first

A completion may race with cancellation/timeout and remain queued after a later operation starts. Therefore completion handling first checks whether the referenced durable request/call is still pending. Already-terminal completions are ignored before comparing them with the current process-local `ActiveAgentOperation`.

## D-09 — Sequential approval constraint

The v0.1 sequential Tool scheduler permits at most one pending approval and does not allow `approval/requested` to coexist with an unresolved `tool/dispatched` attempt. This keeps authorization recovery deterministic before parallel Tool execution is introduced.

## D-10 — Main spec consolidation deferred

Batch 09 adds `spec/batch-09-control-and-approval.md` as a normative amendment rather than rewriting the existing main spec set. The amendment should be folded into the v0.1 consolidated specs before the Provider Protocol implementation is frozen.

# Batch 10 Spec Delta

## D-10.1 — Provider Protocol becomes executable and normative

The previous `spec/provider-protocol.md` was a draft. Batch 10 replaces it with the implemented v1 wire contract.

## D-10.2 — Wire crate remains domain-independent

`harness-provider-protocol` may depend on serialization/error libraries, but not on Harness domain/runtime crates. Equivalent wire vocabulary is defined locally.

## D-10.3 — JSON-RPC id profile

v1 accepts non-empty string ids only. Core allocates request ids and Provider echoes them exactly.

## D-10.4 — Core allocates LLM stream ids

The earlier draft allowed Provider to allocate `streamId` in the `llm.start` response. Batch 10 changes this: Host allocates and installs routing before the request, Provider echoes the id. This removes a start/event routing race.

## D-10.5 — Provider Tool outcomes are narrower than Core Tool outcomes

Provider may return `success`, `error`, or `cancelled`. `denied` remains Core policy output. `unknown` remains Core recovery analysis after an uncertain durable dispatch.

## D-10.6 — Late timed-out RPC response

A request timeout retires its RpcId in a bounded Host cache. A late response for a retained retired id is ignored instead of marking the provider unhealthy.

## D-10.7 — Protocol violation scope

The Batch 10 reference Host marks the Provider process `Unhealthy` on malformed stdout, uncorrelated non-retired response ids, provider-to-Core requests, unsupported notifications, unknown LLM streams, or invalid/non-contiguous LLM stream events.

## D-10.8 — Transport failure does not imply rollback

Provider process failure only reports transport unavailability. It does not authorize replay of side-effecting Tool work. Agent/Tool recovery continues to own replay decisions.

## D-10.9 — Domain adapters deferred

`ProviderHost -> harness_llm::LlmProvider` and `ProviderHost -> harness_tools::ToolExecutor` are explicitly Batch 11 work. Batch 10 validates the process/wire seam in isolation.


## D-10.10 — Wire semantic validation

The protocol layer validates opaque-id non-emptiness where structurally required, `argumentsJson`, SHA-256 blob digests, safe JSON integer bounds for wire sizes/counters, portable error shape, and RFC3339 UTC deadlines. Malformed provider results/events are protocol faults rather than domain-adapter inputs.

## D-10.11 — Process-level conformance smoke

A dependency-free Python conformance runner launches the reference provider through real pipes and validates the Batch 10 handshake/Tool/LLM/shutdown path independently from Rust domain adapters.

## D-10.12 — Manifest is an executable capability fence

The initialized manifest is not informational only. Host refuses undeclared Tool names and LLM models before sending a capability request, requires LLM provider identity to match the manifest provider id, and rejects `idempotent-write` Tool descriptors without keyed-idempotency support.

# Batch 11 Spec Delta

Batch 11 does not change the SessionEvent schema or Provider Protocol version. It connects two previously independent contracts.

## D-11.1 — Provider Host owns domain adapters

The protocol crate remains language-neutral and free of Harness domain dependencies. `harness-provider-host` is the dependency seam allowed to translate between wire and domain values.

## D-11.2 — No adapter retries

`ProviderHostLlmAdapter` and `ProviderHostToolAdapter` represent one provider attempt. They MUST NOT perform logical retries. Retry and recovery remain Core responsibilities.

## D-11.3 — Tool manifest binding is semantic

A Tool adapter binds to one named manifest capability. Core should reject mismatches in name, version, parallel-safety, or side-effect class before registration.

## D-11.4 — Capability cancellation becomes a domain hook

LLM and Tool provider-neutral traits gain best-effort cancellation hooks with source-compatible no-op defaults. Out-of-process adapters map them to `capability.cancel`.

## D-11.5 — Durable state precedes cancellation signaling

Explicit Agent cancellation commits the durable Batch 09 terminal/recovery result before signaling the external provider. Cancellation transport failure does not roll back durable state.

## D-11.6 — Timeout signals Provider cancellation

LLM/Tool timeout tasks issue best-effort `CancelCause::Timeout` through the domain hook before returning `DEADLINE_EXCEEDED` to the actor.

## D-11.7 — Python subprocess is the first polyglot acceptance target

`agent-model` on the reference Python provider deterministically emits an `echo` ToolCall on the first model step and a final text response after ToolResult is present. The Rust integration test must exercise the complete Agent lifecycle through this subprocess.

## Batch 12 — Python Provider SDK v0.1

Provider SDKs are formally defined as optional authoring layers above Provider Protocol v1. The Python reference SDK owns stdio JSON-RPC/NDJSON plumbing, generated capability manifests, Tool/LLM operation dispatch, LLM stream sequencing and best-effort cancellation. The wire protocol remains unchanged at version `1.0`; Core remains authoritative for durable cancellation and unknown-outcome recovery semantics.

## Batch 13 — Provider SDK Conformance Contract v1

Batch 13 introduces a conformance-suite version independent from Provider Protocol versioning. Provider Protocol remains `1.0`; Provider SDK Conformance suite `1.0` freezes one canonical provider manifest and exact JSON transcript fixtures.

Normative decisions:

1. SDK conformance is distinct from wire-protocol conformance and Agent E2E acceptance.
2. Every fixture runs in a fresh provider process.
3. The runner automatically verifies the exact canonical manifest before every scenario and graceful shutdown afterward.
4. Golden outputs use exact structural JSON equality; v1 has no wildcard or partial matcher.
5. Active `operationId` ownership is process-wide across Tool and LLM capabilities.
6. Tool/LLM cancellation must preserve the cancellation cause at the provider protocol boundary.
7. Future language SDKs MUST run the same golden fixtures; implementation differences do not justify per-language expected-output forks.

See `spec/batch-13-provider-sdk-conformance.md`.

## Batch 14 — Harness Runtime Composition Root

Batch 14 establishes the process-level composition boundary that had remained intentionally empty since the original Rust workspace scaffold.

Normative refinements:

1. `harness-runtime` owns composition and process lifecycle, not Agent Turn/Step decisions.
2. Provider and Profile membership is frozen at successful Runtime build in Batch 14.
3. Configured provider identity must exactly match manifest `providerId`.
4. Runtime build is transactional from the caller's perspective: any failure after provider startup triggers best-effort rollback before returning.
5. `LlmRegistry` is manifest-derived and rejects profile model bindings that the selected provider does not declare.
6. Core `ToolDefinition` remains authoritative; provider manifest metadata is compatibility attestation.
7. `AgentRegistry` reserves `SessionId` before asynchronous spawn and enforces one live/transitioning Agent per Session.
8. Runtime lifecycle states are process-local and never become SessionEvents.
9. The lifecycle gate lets in-progress open/close/create operations finish before shutdown enters `ShuttingDown`.
10. Normal shutdown order is Agents first, Providers second.
11. Shutdown continues through later phases after individual failures and reports aggregated failures.
12. Storage and identity generation remain injected seams. In-memory storage is a Batch 14 convenience, not a durability claim.
13. Dynamic provider/profile mutation and provider restart policy remain deferred.

See `spec/batch-14-runtime-composition.md` for the complete contract.

## Batch 15 — Durable Local Storage

- Add `SqliteSessionStore` as the first durable implementation of the existing Session event-log contract.
- SQLite schema v1 stores canonical committed event JSON plus indexed Session/sequence/EventId columns and validates both representations on read.
- Mutations use IMMEDIATE transactions; append preserves conflict-before-batch-validation ordering and commits the complete batch/head atomically.
- Add `FilesystemBlobStore`, content-addressed by SHA-256, with temporary-file + fsync + atomic hard-link publish and BlobRef integrity verification.
- Add `DurableLocalStorage` conventional layout and `HarnessRuntimeBuilder::durable_local` composition helper.
- Add Runtime restart acceptance: previous Session projection and model-request BlobRefs survive store/Runtime reopen and a subsequent Turn executes normally.
- Fix Batch 14 `result_large_err` by boxing build-error sources and `too_many_arguments` by introducing crate-private `HarnessRuntimeParts`; no lint suppression is added.
- Provider Protocol, SessionEvent schema, Agent state machine, Tool recovery semantics, and Python SDK conformance remain unchanged.
# Batch 16 Spec Delta

1. `harness.toml` MUST declare `schema_version = 1`; unsupported versions are rejected.
2. `harness.toml` is application composition input, not durable Agent state. Changing it does not rewrite prior SessionEvent history.
3. Relative `runtime.data_dir`, provider `cwd`, and path-like provider programs are resolved relative to the canonical configuration-file directory. A provider with no explicit `cwd` runs with the config directory as its working directory.
4. Provider subprocesses retain the existing `ProviderHost` parent-environment inheritance semantics; configured `env` entries add or override values. Secret isolation/injection is not modeled in Batch 16; `CredentialResolver` remains Batch 17 work.
5. `config check` MUST NOT start Provider processes. It validates only file syntax and static composition references/limits. Runtime build remains authoritative for Provider manifest identity and capability compatibility.
6. Batch 16 file configuration supports only an explicitly selected `allow-all` ToolPolicy. Approval interactions are not silently auto-resolved by the CLI.
7. Configured Tool schemas are model-visible metadata. Batch 16 does not select a JSON Schema execution engine; CLI-composed Tools use a Core-side JSON-object argument validator.
8. `harness run` is a foreground process. It owns one live Runtime and one live Agent for the selected Session, waits for each submitted turn to converge, prints model text, and performs Agent-before-Provider shutdown on exit.
9. `harness session create` and `harness inspect` operate directly on the configured durable local SessionStore and do not require Provider startup.
10. CLI-generated opaque identifiers use UUID v4 values with the existing recommended Harness prefixes.
11. Daemon/server operation, remote control, full credentials, RuntimeEvent observability, and dynamic Provider membership remain outside Batch 16.

# Batch 17 Spec Delta

1. `CredentialResolver` becomes the process-level seam for converting a non-secret credential key into secret material immediately before Provider start.
2. Resolved secrets are never SessionEvents or RuntimeEvents and `SecretValue` has a redacted `Debug` representation.
3. Plain Provider environment values and credential-backed environment values may not target the same environment key.
4. Static config compilation never resolves credentials. Environment-backed credentials are resolved only during Runtime Provider composition.
5. `RuntimeEvent` is explicitly non-durable operational observation and cannot be used as replay input.
6. RuntimeEvent `seq` is process-local and independent from SessionEvent `seq`.
7. Runtime/Provider/Agent lifecycle transitions publish typed RuntimeEvents; arbitrary underlying error strings are omitted from those events.
8. The CLI may append RuntimeEvents to JSONL, but observer lag/failure does not change Agent or Session semantics.
9. Batch 16 `HarnessRuntimeInfo` construction is changed to struct-update initialization to satisfy `clippy::field_reassign_with_default` without lint suppression.

## Batch 18 spec delta — Scope / Prompt / Capability Configuration

- Adds deterministic configuration resolution order `global -> workspace -> profile -> session`.
- Keeps profile/Core ToolDefinitions authoritative; scope capability directives only enable/disable profile-declared Tools.
- Defines prompt `append`/`replace` semantics and exact `\n\n` fragment joining.
- Adds workspace defaults and session-scoped profile/workspace bindings to `harness.toml` schema version 1.
- Adds `ScopeSelection`, `ResolvedScope`, and a serializable non-durable resolution trace.
- Adds scoped Runtime composition while preserving the existing unscoped `RuntimePlan::runtime_builder()` compatibility path.
- Adds offline `harness config resolve` and `harness run --workspace`.
- Does not add dynamic config hot reload or a durable scope-snapshot SessionEvent.


## Batch 19 spec delta — Durable Execution Composition Epochs

- Adds durable `composition/activated { profile, snapshot }` SessionEvent at quiescent execution boundaries.
- Adds immutable `ExecutionCompositionSnapshot` v1 Blobs; Blob SHA-256/size, not BlobId, identify equal composition bytes.
- Snapshots cover resolved model semantics, enabled Core Tool definitions, provider manifest versions, keyed-idempotency support, validator/policy identities, and automatic Tool retry budget.
- `HarnessRuntime::open_agent` must verify the active snapshot and reconcile composition before spawning the Agent.
- Same composition may resume unfinished work. Composition drift while work is unfinished fails closed and performs no Session mutation.
- Quiescent Sessions may durably activate a new composition before new execution starts.
- Quiescent pre-Batch-19 Sessions may receive their first activation; unfinished legacy Sessions without an activation are rejected as unbound.
- Credentials and secret values remain non-durable and are excluded from composition snapshots.
- Provider supervision/restart remains deferred; provider availability recovery must not own Tool retry safety.

See `spec/batch-19-durable-execution-composition.md`.
