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
    Idle { last_turn: Option<TurnNo> },
    Running { turn: TurnNo, step: Option<StepNo> },
    Maintenance,
}
```

The phase model deliberately does not include `WaitingForLlm`, `WaitingForTool`, or `WaitingForApproval`. Those are operations or boundaries within a Running activity, not additional durable state-machine phases.

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

At a new turn boundary, Core claims at most the ordinary message(s) required by the chosen batching rule. v0.1 reference behavior is one ordinary `next-turn` message per new turn proposal.

### 5.2 `next-step`

Contains context or steering intended for the nearest allowed step boundary.

Examples:

- user steering while an Agent is already running;
- injected runtime context;
- additional context resulting from a tool pipeline.

At an eligible step boundary the v0.1 reference driver claims all `next-step` items currently visible in the actor's durable projection, preserving FIFO order within that queue.

### 5.3 Convenience semantics

```text
followup = target next-turn, wakeup true
steer    = target next-step, wakeup true
inject   = target next-step, wakeup false
```

The stable primitive is `send(message, target, wakeup)`.

The v0.1 Rust Agent Inbox accepts only user-role `Message` values. Provenance may still be `user`, `plugin`, or another allowed `MessageSource`; role controls how the message enters model history.

## 6. Wake behavior

A wake request on an Idle, Open Agent starts the deterministic driver.

A wake arriving while a driver is active is persisted to Inbox and consumed according to the nearest eligible boundary. It does not create a second driver.

A wake arriving after the active operation is already cancelled MUST NOT join the aborted activity. It is preserved for a later activity unless shutdown/disposal semantics require rejection.

`wakeup` is not a durable counter. The live `wake_requested` latch is reconstructed from pending durable Inbox items with `wakeup=true`. Once such an item is claimed, the latch disappears unless another pending item still carries a wake request.

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

Batch 06 implements only the deterministic prefix through model-visible input entry. Model request execution begins at the `ReadyForModel` boundary introduced below.

## 8. Step semantics

A step contains one model request attempt sequence that eventually yields one authoritative assistant message or a terminal step failure, followed by zero or more logical ToolCalls produced by that assistant message.

Tool results that require another model request cause the turn to continue with another step.

A step may receive multiple model-visible user-role inputs before its model request begins. One ordinary `next-turn` input is the primary input of a newly opened turn; `next-step` inputs are appended after that primary input in FIFO order.

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

On resume, Core loads durable session state and reconstructs:

- current Inbox projection;
- last completed and currently open turn/step boundaries;
- unresolved recovery gate;
- model history projection;
- incomplete model or Tool operation state.

Normal execution starts only if recovered structural invariants are valid and ExecutionGate is Open.

The process-local `AgentPhase` is never reconstructed as if a pre-crash task were still alive. A new process first owns the Session as `Idle` or `Maintenance`, analyzes durable state, and then explicitly reacquires driver ownership when it continues an unfinished turn/step.

## 12. Live actor transport

The v0.1 Rust reference actor uses a bounded Tokio `mpsc` mailbox. `AgentHandle` is cloneable; `AgentActor` is not. The mailbox receiver and all mutable Agent state remain owned by exactly one Tokio task.

A submitted state-changing command carries a Tokio `oneshot` acknowledgement channel. Mailbox delivery alone is not durable acceptance. For `Send`, the actor MUST complete the following order:

```text
receive command
    -> validate command-level message constraints
    -> construct inbox/enqueued draft
    -> validate proposed durable history locally
    -> SessionStore.append(expected_seq)
    -> verify committed result
    -> update local projection / resume view
    -> refresh wake latch from durable Inbox projection
    -> send acknowledgement
```

If the caller receives `SendReceipt`, the corresponding `inbox/enqueued` event is already committed.

Dropping the acknowledgement receiver after mailbox submission does not cancel the actor's durable mutation. Caller cancellation before the mailbox accepts the command means no acceptance guarantee exists.

## 13. Wake latch and driver consumption

`wakeup=true` becomes visible to the driver only after `inbox/enqueued` commits. Multiple wake requests may coalesce because wake is a request to run the single driver, not a count of driver executions.

On process restart the latch is reconstructed as true when any pending Inbox item was durably enqueued with `wakeup=true`.

Beginning in Batch 06, the deterministic driver consumes this latch by durably claiming eligible Inbox work. After the claim batch commits, `wake_requested` is refreshed from the remaining pending Inbox projection. A queued future `next-turn` message can therefore keep the latch true while the current step is already parked at `ReadyForModel`.

## 14. Startup convergence before handle publication

Before a new `AgentHandle` is returned, the Rust reference actor performs all recovery and deterministic driver steps that require no external capability call.

### 14.1 Interrupted model attempt

If projection yields `RecoverInterruptedModelRequest`, Core appends a durable `model/failed` with `MODEL_REQUEST_FAILED`, stating that the process restarted before a terminal model response became durable. This converts the state to ordinary open-step continuation.

### 14.2 Unknown non-idempotent Tool outcome

If projection yields `PersistRecoveryBlock`, Core atomically appends:

```text
recovery/blocked
step/ended(blocked)
turn/ended(blocked)
```

The resulting Agent is exposed as `Idle + ExecutionGate::Blocked` with no open durable turn/step. Resolution still requires a late authoritative `tool/result` followed by `recovery/resolved` as defined by the recovery rules.

### 14.3 Deferred capability recovery

The following remain capability-driver work and are not automatically executed by Batch 06:

- `RecoverToolBatch` retries;
- an already durable `Blocked` recovery gate.

### 14.4 Deterministic lifecycle convergence

After recovery-only convergence, Batch 06 continues any deterministic turn/step work before publishing the handle:

- a pending waking Inbox item may open a new turn and step;
- `ContinueOpenTurn` may start its next eligible step or close an exhausted turn;
- a pre-assistant `ContinueOpenStep` may absorb pending `next-step` inputs;
- a post-assistant open step is deferred for step/turn finalization rather than being mistaken for another model request;
- the actor parks at `ReadyForModel` only when the open step has no authoritative assistant message.

No provider call occurs before `AgentHandle` publication.

## 15. Live ownership conflict

A `SessionStore::Conflict` observed by a live Agent append means the actor no longer has exclusive single-writer ownership of the Session snapshot it bootstrapped from. The actor MUST return an ownership-lost error for the command and terminate rather than adopting the competing writer's events into its local state.

A future supervisor may choose to bootstrap a fresh Agent instance explicitly. The existing actor must not silently rebase itself.

## 16. Cancellation boundary

The `Cancel` command remains part of the stable command vocabulary, but Batch 06 does not yet implement cancellation of an external model/tool operation or durable Inbox discard convergence. The reference actor returns an explicit unsupported-operation error for `Cancel` until the capability driver introduces cancellation tokens and activity convergence.

## 17. Deterministic driver boundary

The Batch 06 driver performs only state transitions that can be decided from the actor's current durable projection. It MUST stop before an external capability invocation.

The first exposed boundary is:

```text
ReadyForModel { turn, step }
```

`ReadyForModel` is process-local and MUST NOT be persisted as a new SessionEvent. In the Rust reference implementation it is derived from the conjunction:

```text
AgentPhase::Running { turn, step: Some(step) }
+
ResumeDecision::ContinueOpenStep { same turn, same step }
+
SessionProjection.open_step_assistant_message == None
```

This keeps the durable model minimal: after restart, replay plus recovery analysis is sufficient to rediscover the same boundary. `SessionProjection.open_step_assistant_message` is itself replay-derived and is not a durable field.

An open step with an authoritative assistant message is not `ReadyForModel`; Batch 06 defers that post-assistant state so a restart cannot accidentally issue a duplicate model request.

## 18. Atomic step-entry batching

The v0.1 reference driver uses one atomic `SessionStore::append` batch for the deterministic entry into a step. This prevents a crash from durably claiming an Inbox item without also preserving its model-visible `user/message` fact.

For the first step of a newly opened turn, event order is:

```text
turn/started
inbox/claimed(next-turn)?
step/started
user/message(primary next-turn)?
[inbox/claimed(next-step), user/message(next-step)]*
```

The driver claims at most one `next-turn` item and all `next-step` items visible at that boundary. The model-visible order is primary `next-turn` first, followed by `next-step` items in queue order.

For a new step inside an already open turn:

```text
inbox/claimed(next-turn)?    # only when resuming a turn that never started its first step
step/started
user/message(primary next-turn)?
[inbox/claimed(next-step), user/message(next-step)]*
```

For additional `next-step` input arriving while the current step is parked before model dispatch:

```text
[inbox/claimed(next-step), user/message(next-step)]*
```

No second `step/started` event is emitted.

## 19. Open-turn continuation rule

`ContinueOpenTurn` has two cases.

If the open turn has never started a step, the driver treats one pending `next-turn` item as its primary input and may also absorb all pending `next-step` inputs.

If the open turn has already completed at least one step, only `next-step` work may continue that turn. A pending `next-turn` item belongs to a future turn. When no `next-step` work remains, Core appends `turn/ended(completed)`; the still-pending waking `next-turn` item may then open the next turn.

## 20. Mailbox responsiveness boundary

Batch 06 deliberately parks before model execution rather than awaiting an LLM from inside the deterministic driver loop.

The architectural rule for the next capability batch is:

> External capability waits MUST NOT require mutable Agent ownership to remain borrowed across the await in a way that prevents the actor mailbox from accepting steering, future-turn input, cancellation, or shutdown.

Batch 06 establishes the durable and process-local boundary needed to implement that rule without changing Session semantics later.

## 17. Batch 07 active external operation

The live Agent state now distinguishes durable recovery interpretation from a process-local operation that is actually in flight.

```text
Durable projection:
    pending model/requested
    -> ResumeDecision::RecoverInterruptedModelRequest

Live actor overlay:
    ActiveAgentOperation::Model(requestId, position, attempt)
```

While the live overlay exists, Core MUST NOT execute startup-style interrupted-request recovery and MUST NOT issue a second model request. If the process disappears, the overlay disappears and the durable recovery decision becomes authoritative.

## 18. LLM completion mailbox

One LLM provider future runs outside the Agent mailbox loop. It emits a single normalized completion into the actor mailbox after consuming and validating the provider stream.

The actor alone translates that completion into durable `assistant/message` or `model/failed` state. The provider task has no SessionStore authority.

Commands accepted while the LLM future is pending are still serialized and persisted by the actor. In particular, a `next-step` steer accepted before model completion remains queued and may become input to the following step after the current assistant response converges.

Shutdown aborts the process-local LLM task. An already committed `model/requested` without a terminal durable result is intentionally recoverable on the next process start.
