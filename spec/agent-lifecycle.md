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

The phase model deliberately does not include `WaitingForLlm`, `WaitingForTool`, or `WaitingForApproval`. Those are operations within a Running activity, not additional persistent state-machine phases. `step=None` represents the process-local driver being inside a turn but between step boundaries; `last_turn=None` is valid for a newly created Session.

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

On resume, Core loads a point-in-time durable Session snapshot and reconstructs:

- current Inbox projection;
- durable lifecycle cursor (quiescent/open turn/open step);
- pending model request, if any;
- pending logical ToolCalls and their latest durable `tool/dispatched` marker, if any;
- unresolved recovery gate;
- model history projection.

A RecoveryAnalyzer classifies the snapshot before a new turn may start. v0.1 decisions are conceptually:

```text
clean
continue-open-turn
continue-open-step
recover-interrupted-model-request
recover-tool-batch
persist-recovery-block
blocked
```

`tool/call` without `tool/dispatched` is a pre-dispatch interruption and may restart the Tool pipeline. A durable dispatch without a terminal result is interpreted using SideEffectClass. `non-idempotent-write` requires a durable recovery block; `read-only` is retryable; `idempotent-write` is retryable only after the provider's idempotency guarantee is verified.

`AgentPhase` is process-local driver ownership. After process restart the new Agent instance starts `Idle` even if the durable Session contains an unfinished turn/step; the unfinished durable lifecycle is represented by the ResumeDecision and MUST converge before a new turn begins.

Normal new-turn execution starts only if the recovered structural invariants are valid, ExecutionGate is Open, and ResumeDecision is `clean`.
