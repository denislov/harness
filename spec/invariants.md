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

## I-21: EventId uniqueness within a Session

Every committed `SessionEvent.eventId` MUST be unique within its Session. Projection MUST reject duplicate EventIds even when sequence numbers are otherwise valid.

## I-22: Ownership conflict terminates the live actor

If a live Agent's conditional append observes a Session head conflict, that Agent instance MUST stop authoring the Session. It MUST NOT silently load and adopt the competing writer's events.

## I-23: Actor owner is singular

The process-local mutable `AgentActor` owner MUST NOT be cloneable. Cloneable access is provided through message-passing handles only.

## I-24: Wake is visible only after durable enqueue

A `wakeup=true` command MUST NOT make work eligible for the driver before the corresponding `inbox/enqueued` event has committed successfully.

## I-25: Inbox claim and model-visible entry are atomic

When the deterministic driver consumes an Inbox item into a step, the `inbox/claimed` event and corresponding `user/message` event MUST commit in the same atomic SessionStore append batch.

A crash MUST NOT leave an input durably claimed without preserving the model-visible fact that the claim was intended to enter.

## I-26: Queue target semantics are preserved

A running open step MAY consume pending `next-step` input at the current pre-model boundary. It MUST NOT consume pending `next-turn` input into that open step.

When an open turn has already completed at least one step and has no immediate `next-step` work, pending `next-turn` work belongs to a future turn.

## I-27: ReadyForModel is derived, not durable

`ReadyForModel` MUST NOT be represented by a dedicated SessionEvent in v0.1. It is a process-local boundary derived from a structurally valid open step with no pending model request, no Tool recovery work, and no authoritative assistant message already recorded for that step.

A restart MUST rediscover the boundary by replay and recovery analysis rather than by trusting pre-crash process state.

## I-28: External waits must preserve mailbox progress

An external model, Tool, approval, or provider wait MUST NOT require the single mutable Agent actor owner to remain unavailable for mailbox processing for the duration of that wait.

The actor must remain able to durably accept eligible future input and, once implemented, cancellation/shutdown commands while external capability work is in flight.
