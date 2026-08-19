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

## 12. Batch 05 live actor transport

The v0.1 Rust reference actor uses a bounded Tokio `mpsc` mailbox. `AgentHandle`
is cloneable; `AgentActor` is not. The mailbox receiver and all mutable Agent state
remain owned by exactly one Tokio task.

A submitted state-changing command carries a Tokio `oneshot` acknowledgement
channel. Mailbox delivery alone is not durable acceptance. For `Send`, the actor
MUST complete the following order:

```text
receive command
    -> construct inbox/enqueued draft
    -> validate proposed durable history locally
    -> SessionStore.append(expected_seq)
    -> verify committed result
    -> update local projection / resume view
    -> update wake latch
    -> send acknowledgement
```

If the caller receives `SendReceipt`, the corresponding `inbox/enqueued` event is
already committed.

Dropping the acknowledgement receiver after mailbox submission does not cancel the
actor's durable mutation. Caller cancellation before the mailbox accepts the command
means no acceptance guarantee exists.

## 13. Wake latch

`wakeup=true` sets a process-local `wake_requested` latch only after the enqueue is
durable. Multiple wake requests may coalesce into one latch because wake is a request
to run the single driver, not a durable count of driver executions.

On process restart the latch is reconstructed as true when any pending Inbox item was
durably enqueued with `wakeup=true`.

Batch 05 does not yet consume the latch into a new Turn. Driver consumption is part of
the next Turn/Step implementation batch.

## 14. Startup convergence before handle publication

Before a new `AgentHandle` is returned, the Rust reference actor performs recovery
steps that require no external capability call.

### 14.1 Interrupted model attempt

If projection yields `RecoverInterruptedModelRequest`, Core appends a durable
`model/failed` with `MODEL_REQUEST_FAILED`, stating that the process restarted before
a terminal model response became durable. This converts the state to ordinary open-step
continuation.

### 14.2 Unknown non-idempotent Tool outcome

If projection yields `PersistRecoveryBlock`, Core atomically appends:

```text
recovery/blocked
step/ended(blocked)
turn/ended(blocked)
```

The resulting Agent is exposed as `Idle + ExecutionGate::Blocked` with no open durable
turn/step. Resolution still requires a late authoritative `tool/result` followed by
`recovery/resolved` as defined by the recovery rules.

### 14.3 Deferred startup work

The following remain driver/capability work and are not automatically converged by
Batch 05:

- ordinary `ContinueOpenTurn`;
- ordinary `ContinueOpenStep`;
- `RecoverToolBatch` retries;
- an already durable `Blocked` recovery gate.

## 15. Live ownership conflict

A `SessionStore::Conflict` observed by a live Agent append means the actor no longer
has exclusive single-writer ownership of the Session snapshot it bootstrapped from.
The actor MUST return an ownership-lost error for the command and terminate rather
than adopting the competing writer's events into its local state.

A future supervisor may choose to bootstrap a fresh Agent instance explicitly. The
existing actor must not silently rebase itself.

## 16. Batch 05 cancellation boundary

The `Cancel` command remains part of the stable command vocabulary, but Batch 05 does
not implement active-driver cancellation or durable Inbox discard convergence. The
reference actor returns an explicit unsupported-operation error for `Cancel` until the
Turn/Step driver introduces cancellation tokens and activity convergence.
