# Batch 09 Control, Timeout, and Approval Amendment

**Status:** Draft v0.1 normative amendment

## 1. Scope

Batch 09 adds three control-plane semantics to the executable Rust reference runtime:

- explicit cancellation convergence;
- Core-owned LLM and Tool attempt timeouts;
- durable Tool approval requests and resolutions.

These rules extend the existing single-writer Session event model. Providers still cannot author durable state.

## 2. Approval identity and decisions

`ApprovalId` is an opaque identifier and MUST be unique within one Session. v0.1 decisions are `allow` and `deny`.

A Tool policy decision of `ask` MUST NOT be silently converted into denial. Core records an approval request and parks the Agent until an explicit resolution is committed.

## 3. Durable approval events

### 3.1 `approval/requested`

```json
{
  "approvalId": "apr_...",
  "callId": "call_...",
  "reason": "explicit approval required",
  "risk": "filesystem-write"
}
```

The event belongs to the open Tool-producing step and MUST precede `tool/dispatched` for that call.

### 3.2 `approval/resolved`

```json
{
  "approvalId": "apr_...",
  "callId": "call_...",
  "decision": "allow",
  "note": "approved by user"
}
```

The event MUST match the currently pending approval. A denied approval MUST NOT be followed by `tool/dispatched`.

The sequential v0.1 scheduler permits at most one pending approval and does not create a new approval request while another Tool dispatch remains unresolved.

A resolved approval is authoritative for that logical ToolCall. Restart between `approval/resolved(allow)` and `tool/dispatched` MUST NOT require the user to approve again.

## 4. Approval runtime boundary

A pending durable approval projects to:

```text
ResumeDecision::AwaitingApproval
AgentDriverBoundary::AwaitingApproval
```

No external capability future is active at this boundary. The Agent mailbox remains responsive to `Send`, `Cancel`, `ResolveApproval`, `Snapshot`, and `Shutdown`.

`AgentHandle::resolve_approval` acknowledges only after `approval/resolved` is durable.

## 5. Cancellation

The first cancellation command dequeued for the current live activity controls its durable convergence. Later cancellation commands do not rewrite the already committed terminal state.

### 5.1 Model cancellation

For a live pending model attempt Core first commits the durable cancellation convergence batch:

```text
model/failed(CANCELLED)
step/ended(cancelled)
turn/ended(cancelled)
```

Only after that append succeeds does Core abort the process-local model task. If durable convergence fails, cancellation is not acknowledged and the live task remains owned.

A provider completion already queued before task abortion may still arrive. If the request is already durably terminal, that stale completion is ignored rather than treated as a second authoritative result.

### 5.2 Tool cancellation before dispatch

Undispatched ToolCalls may terminate as `ToolOutcome::Cancelled`.

### 5.3 Tool cancellation after dispatch

Cancellation does not prove that an external effect was prevented.

- read-only: Core may record a cancelled terminal result because no external mutation must be reconciled;
- idempotent-write: Core records an unknown terminal outcome when cancellation interrupts a dispatched attempt;
- non-idempotent-write: Core records `recovery/blocked` and MUST NOT pretend the invocation was safely cancelled.

Other undispatched ToolCalls in the same sequential batch are terminalized before a non-idempotent recovery block is closed over the step.

### 5.4 Inbox retention

`keepInbox=true` preserves pending unclaimed Inbox work. `keepInbox=false` durably records `inbox/discarded` for all pending next-turn and next-step items.

## 6. Timeouts

Core owns attempt deadlines. The Rust reference implementation uses `tokio::time::timeout`; the hosting Tokio runtime MUST have its time driver enabled.

### 6.1 LLM

`AgentLlmRuntime` has a non-zero timeout. The reference default is 120000 ms. Timeout produces `DEADLINE_EXCEEDED`, which is committed through the normal `model/failed` path.

### 6.2 Tool

Each `ToolDefinition.defaultTimeoutMs` is enforced around exactly one ToolExecutor attempt.

A timeout after `tool/dispatched` is an ambiguous provider-level failure, not an authoritative ToolOutcome. Core therefore reuses the existing side-effect-aware recovery rules:

- read-only may retry;
- idempotent-write may retry only with the required idempotency guarantee;
- non-idempotent-write blocks.

## 7. External task cancellation caveat

Process-local task abortion is a scheduling/cancellation mechanism, not proof of provider-side rollback. ProviderHost adapters added later must map explicit cancellation into their transport while preserving the same durable Core semantics.

## 8. New invariants

- **I-29 Durable approval:** a pending user approval MUST be reconstructable solely from durable Session state.
- **I-30 Approval precedes dispatch:** a ToolCall requiring approval MUST NOT cross `tool/dispatched` until a matching durable allow resolution exists.
- **I-31 Denial is monotonic:** a durable approval denial MUST NOT later become a dispatch for the same logical ToolCall.
- **I-32 Timeout is Core-owned:** provider adapters MUST NOT hide logical attempt timeout/retry decisions from Core.
- **I-33 Cancellation does not erase uncertainty:** cancellation after a non-idempotent dispatch MUST preserve unknown-outcome recovery semantics.
