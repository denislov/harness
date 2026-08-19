# Tool Execution Specification

**Status:** Draft v0.1

## 1. ToolDefinition

Every registered Tool has a stable definition.

Conceptual Rust structure:

```rust
pub struct ToolDefinition {
    pub name: String,
    pub version: String,
    pub description: String,
    pub input_schema: JsonSchema,
    pub output_schema: Option<JsonSchema>,
    pub parallel_safe: bool,
    pub side_effect: SideEffectClass,
    pub default_timeout_ms: u64,
}
```

`name` identifies the model-visible tool in its scope. Name collision resolution is a registry concern; the final model-visible catalog MUST contain unique names.

## 2. SideEffectClass

v0.1 defines:

```text
read-only
idempotent-write
non-idempotent-write
```

### 2.1 `read-only`

The operation has no externally visible mutation. Transport failure MAY be automatically retried if policy allows.

### 2.2 `idempotent-write`

The operation mutates state but supports repetition using a stable idempotency key or equivalent provider guarantee.

Automatic retry is permitted only when the resolved provider capability declares compatible idempotency semantics.

### 2.3 `non-idempotent-write`

The operation may produce duplicate external side effects if repeated. If Core cannot prove whether dispatch completed, automatic retry is forbidden.

## 3. ToolCall versus ToolInvocation

A `ToolCall` is the logical request produced by the model.

A `ToolInvocation` is a concrete execution identity assigned by Core.

Conceptual structure:

```rust
pub struct ToolInvocation {
    pub invocation_id: InvocationId,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub arguments_json: String,
    pub attempt: u32,
    pub idempotency_key: String,
}
```

A logical ToolCall can experience multiple provider attempts without creating multiple authoritative terminal ToolOutcomes.

## 4. ToolOutcome

Core uses a richer terminal model than the model-visible `tool-result` block.

```text
success
error
denied
cancelled
unknown
```

Conceptual shape:

```rust
pub enum ToolOutcome {
    Success { content: Vec<ContentBlock> },
    Error { code: String, message: String, content: Vec<ContentBlock> },
    Denied { reason: String },
    Cancelled { cause: CancelCause },
    Unknown { reason: String },
}
```

`Unknown` means Core cannot determine the external effect. It is not equivalent to Error.

## 5. Execution pipeline

The normative logical order is:

```text
ToolCall
   |
resolve ToolDefinition
   |
validate arguments JSON against input schema
   |
record durable tool/call
   |
pre-execute middleware
   |
PolicyEngine
   +-- deny
   +-- ask approval
   +-- allow
   |
execution middleware
   |
CapabilityRouter
   |
assign InvocationId + stable idempotency key
   |
record durable tool/dispatched
   |
ProviderHost
   |
Tool Provider
   |
post-execute middleware
   |
normalize ToolOutcome
   |
record durable tool/result or recovery/blocked
```

## 6. PolicyDecision

v0.1 decisions:

```text
allow
deny
ask
```

Example wire-neutral shape:

```json
{
  "kind": "ask",
  "reason": "command writes outside workspace",
  "risk": "filesystem-write"
}
```

A deny decision is monotonic. Once a mandatory guard denies an operation, later middleware MUST NOT convert it to allow.

## 7. Identity protection

The following invocation identity fields are immutable after resolution:

- session identity;
- turn and step coordinates;
- ToolCallId;
- resolved tool name;
- side-effect classification.

Middleware MAY transform arguments or execution options only when explicitly permitted by policy. Middleware MUST NOT silently replace one tool with a different tool identity.

## 8. Argument validation

Before provider dispatch, Core MUST verify that `argumentsJson`:

1. is valid JSON;
2. validates against the resolved ToolDefinition input schema.

Provider-side validation MAY be repeated defensively but does not replace Core validation.

## 9. Parallel execution

An assistant message may contain multiple ToolCalls.

Core MAY execute calls concurrently only if all relevant constraints allow it, including `parallelSafe` and policy.

Durable terminal result ordering SHOULD follow original model ToolCall order even if completion order differs. This provides stable replay and avoids provider timing becoming durable conversation semantics.

Example:

```text
model order: A, B, C
completion:  B, C, A
durable:     result A, result B, result C
```

## 10. Idempotency

Core assigns a stable `IdempotencyKey` before the first provider dispatch. Retries of the same logical ToolCall MUST reuse the same key.

Every provider dispatch is durably represented by `tool/dispatched`. `attempt` starts at `1` and increments exactly by one. A retry uses a new `InvocationId` while retaining the same logical `ToolCallId`, `providerId`, and `idempotencyKey` in v0.1.

## 11. Crash recovery

### 11.1 Read-only

If durable `tool/call` exists without `tool/dispatched`, Core may restart the Tool pipeline because no provider dispatch is durably known.

If `tool/dispatched` exists and no authoritative terminal outcome exists, Core MAY create a new dispatch attempt automatically.

### 11.2 Idempotent write

If `tool/call` exists without `tool/dispatched`, Core may restart the Tool pipeline.

If `tool/dispatched` exists without an authoritative terminal outcome, Core MAY retry only if the resolved provider contract guarantees the supplied stable idempotency key or an equivalent reconciliation mechanism. The retry MUST preserve the original `providerId` and `idempotencyKey` in v0.1.

### 11.3 Non-idempotent write

If `tool/call` exists without `tool/dispatched`, Core may restart from the pre-dispatch boundary.

If `tool/dispatched` exists and no authoritative result was durably committed, external execution may have occurred. Core MUST:

1. record `recovery/blocked`;
2. set ExecutionGate to Blocked;
3. avoid automatic retry;
4. avoid continuing to a new model step or turn;
5. require explicit reconciliation or human/provider-specific resolution.

## 12. Provider failure ownership

ProviderHost reports process/transport/protocol failures. Tool Runtime maps those failures to retry, Error, or Unknown according to side-effect semantics and observed dispatch boundary.

ProviderHost MUST NOT blindly restart and replay a Tool invocation.


## 13. v0.1 non-idempotent dispatch serialization

The v0.1 Agent Tool scheduler MUST ensure that at most one `non-idempotent-write` provider dispatch is unresolved for one Agent at a time. This keeps the durable ExecutionGate representable by a single active recovery block. Read-only and idempotent Tool work may still be parallelized when their ToolDefinition permits it.
