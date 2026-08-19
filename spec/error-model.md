# Error Model

**Status:** Draft v0.1

## 1. Purpose

Core may use rich Rust error types internally, but cross-subsystem and Provider Protocol boundaries require stable machine-readable error codes.

Errors are facts about failure, not retry decisions. Retry policy belongs to the owning domain subsystem.

## 2. Stable v0.1 error codes

```text
INVALID_ARGUMENT
NOT_FOUND
CONFLICT
PERMISSION_DENIED
CANCELLED
DEADLINE_EXCEEDED
PROVIDER_UNAVAILABLE
PROVIDER_PROTOCOL_ERROR
TOOL_EXECUTION_FAILED
MODEL_REQUEST_FAILED
SESSION_CORRUPT
UNKNOWN_OUTCOME
INTERNAL
```

## 3. Error payload

Portable error shape:

```json
{
  "code": "PROVIDER_UNAVAILABLE",
  "message": "provider process exited",
  "details": {}
}
```

`message` is diagnostic text and MUST NOT be used for machine routing.

`details` is optional structured data. Consumers SHOULD ignore unknown detail fields.

## 4. Code semantics

### `INVALID_ARGUMENT`

Input failed syntactic, schema, range, or semantic validation before execution.

### `NOT_FOUND`

The requested session, provider, capability, tool, blob or other addressed resource does not exist in the current scope.

### `CONFLICT`

The requested state mutation conflicts with current authoritative state, including SessionStore expected-sequence mismatch or duplicate identity creation.

### `PERMISSION_DENIED`

Core policy denied an operation. A provider should normally not originate this code for Core authorization decisions.

### `CANCELLED`

The operation was cancelled by a recognized cancellation cause before authoritative successful completion.

### `DEADLINE_EXCEEDED`

The operation exceeded the Core-defined deadline.

### `PROVIDER_UNAVAILABLE`

The provider process/service is unavailable, exited, failed initialization, or became unreachable.

### `PROVIDER_PROTOCOL_ERROR`

The provider violated the negotiated protocol.

### `TOOL_EXECUTION_FAILED`

A Tool executed and returned a normal terminal error outcome or Core normalized a safe execution failure to an error.

### `MODEL_REQUEST_FAILED`

A model attempt terminated unsuccessfully for a provider/model reason that is not more precisely represented by a transport-level code.

### `SESSION_CORRUPT`

Durable session state violates structural invariants and cannot safely drive normal execution.

### `UNKNOWN_OUTCOME`

Core cannot determine whether a potentially side-effecting operation completed. This is a recovery state, not a normal retryable error.

### `INTERNAL`

Unexpected Harness Core failure that does not fit a stable public code.

## 5. Retry rules

No error code is globally synonymous with retry.

Examples:

- `PROVIDER_UNAVAILABLE` during a read-only Tool may be retryable.
- the same `PROVIDER_UNAVAILABLE` after dispatch of a non-idempotent write may lead to `UNKNOWN_OUTCOME` and blocked recovery.
- `MODEL_REQUEST_FAILED` may or may not be retryable according to model policy and attempt limits.

## 6. Language exception names

Provider Protocol MUST NOT expose Python exception class names, Rust error type paths, Java stack types, or JavaScript Error subclass names as machine-routing codes.

Such data MAY be included as diagnostic detail when safe, but stable routing uses the error code vocabulary above.
