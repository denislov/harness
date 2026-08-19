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


## I-21: Durable Tool dispatch boundary

`tool/call` records logical model intent. Core MUST commit `tool/dispatched` before a Tool invocation crosses the provider/capability boundary. Crash recovery MUST use `tool/dispatched`, not `tool/call`, to decide whether an external side effect may have occurred.

## I-22: Stable retry identity

A retry of one logical ToolCall uses a new `InvocationId` and an incremented attempt number, but MUST preserve its durable `IdempotencyKey` and `ProviderId` in v0.1. A `non-idempotent-write` Tool MUST NOT be automatically redispatched.

## I-23: Non-idempotent uncertainty is singular in v0.1

The v0.1 scheduler MUST keep at most one unresolved `non-idempotent-write` dispatch per Agent. This preserves the single active RecoveryBlock model.

## I-24: Resume converges before new turn

A process-local Agent MAY be Idle while its durable Session contains an unfinished turn/step. Core MUST resolve the resulting ResumeDecision before starting a new turn. `ExecutionGate == Open` alone is not sufficient permission to start a new turn.
