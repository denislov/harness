# Provider Protocol v1

**Status:** Normative v1 wire contract  
**Protocol version:** `1.0`

## 1. Purpose

Provider Protocol is the language-neutral process boundary between Harness Core and out-of-process capability providers.

The v1 contract covers:

- provider initialization and manifest negotiation;
- Tool invocation;
- LLM stream startup and event delivery;
- cooperative cancellation;
- health ping;
- graceful shutdown;
- process/protocol failure semantics.

Rust domain crates are not part of the wire contract. A provider implementation MUST be able to implement this document without linking Rust code.

## 2. Transport

v1 uses:

```text
JSON-RPC 2.0
UTF-8
NDJSON framing
Core -> provider stdin
provider -> Core stdout
provider diagnostics -> stderr
```

Each stdin/stdout physical line is exactly one complete JSON-RPC message.

Provider stdout MUST contain protocol messages only. Logs, stack traces, debug prints, progress bars, and diagnostics MUST go to stderr.

An empty frame, invalid UTF-8, invalid JSON, or non-protocol stdout line is a protocol violation.

## 3. JSON-RPC profile

Provider Protocol v1 intentionally narrows JSON-RPC 2.0:

- `jsonrpc` MUST equal `"2.0"`;
- request/response `id` MUST be a non-empty JSON string;
- Core allocates request ids;
- Provider MUST echo the exact request id in its response;
- Provider-to-Core JSON-RPC requests are not supported in v1;
- Provider-to-Core notifications are limited to negotiated protocol notifications such as `llm.event`;
- a response MUST contain exactly one of `result` or `error`;
- object fields use lower camel case;
- stable enum tokens use lowercase kebab case unless explicitly specified otherwise;
- portable Harness error codes use `SCREAMING_SNAKE_CASE`;
- opaque identifiers have no parseable semantics.

Unknown optional object fields SHOULD be ignored for forward compatibility. Unknown required methods or unsupported required semantics MUST fail loudly.

## 4. Process lifecycle

ProviderHost exposes the process-local states:

```text
starting
ready
unhealthy
stopping
stopped
```

Only `ready` providers receive new Tool or LLM operations.

`unhealthy` means the process or protocol transport can no longer be trusted for new work. It does not imply that an already-dispatched external side effect did not occur.

## 5. Initialization

Immediately after spawn, Core sends:

```json
{"jsonrpc":"2.0","id":"rpc_1","method":"provider.initialize","params":{"protocolVersion":"1.0","runtime":{"name":"harness","version":"0.1.0"}}}
```

Provider returns a manifest as the JSON-RPC result:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_1",
  "result": {
    "providerId": "example-python",
    "providerVersion": "1.0.0",
    "protocolVersion": "1.0",
    "capabilities": []
  }
}
```

A provider is not `ready` until the manifest validates.

### 5.1 Version rule

Batch 10 implements strict protocol `1.0` equality. Major/minor negotiation is intentionally deferred until there is a second protocol revision to negotiate against.

## 6. Provider manifest

Manifest fields:

```text
providerId        non-empty opaque string
providerVersion   non-empty provider-defined version string
protocolVersion   exactly "1.0"
capabilities[]
```

### 6.1 Tool descriptor

```json
{
  "kind": "tool",
  "name": "read_file",
  "version": "1",
  "parallelSafe": true,
  "sideEffect": "read-only",
  "supportsIdempotencyKey": false
}
```

`sideEffect` is one of:

```text
read-only
idempotent-write
non-idempotent-write
```

Provider metadata is capability discovery metadata. Core's resolved `ToolDefinition` remains authoritative before model exposure and dispatch.

A Tool descriptor with `sideEffect: "idempotent-write"` MUST set `supportsIdempotencyKey: true`. A Host MUST reject a manifest that claims an idempotent write without keyed-idempotency support.

Before dispatch, Host MUST verify that the requested Tool name or LLM model is declared by the initialized manifest. For LLM calls, `request.provider` MUST also equal the manifest `providerId`.

### 6.2 LLM descriptor

```json
{"kind":"llm","models":["model-x","model-y"]}
```

Model names MUST be non-empty and unique within one manifest.

## 7. Common wire vocabulary

Provider Protocol defines its own wire schemas for messages, content blocks, blob references, portable errors, cancellation causes, and usage. These schemas intentionally mirror the provider-neutral domain vocabulary but are not Rust types.

### 7.1 Content blocks

Supported v1 `type` values:

```text
text
reasoning
image
tool-call
tool-result
blob
```

`argumentsJson` is a JSON string containing one complete JSON value. It is not an embedded JSON object and MUST NOT be normalized or reserialized by transport code. Blob references use a non-empty opaque id and a 64-character lowercase hexadecimal SHA-256 digest.

### 7.2 Portable error code

Portable error `code` uses the stable `SCREAMING_SNAKE_CASE` vocabulary already defined by Harness Core, including `CANCELLED`, `DEADLINE_EXCEEDED`, `PROVIDER_UNAVAILABLE`, and `PROVIDER_PROTOCOL_ERROR`.

## 8. Tool invocation

Core sends one request per provider attempt:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_42",
  "method": "tool.invoke",
  "params": {
    "operationId": "inv_123",
    "invocationId": "inv_123",
    "callId": "call_abc",
    "sessionId": "ses_123",
    "tool": "read_file",
    "argumentsJson": "{\"path\":\"README.md\"}",
    "attempt": 1,
    "idempotencyKey": "idem_xyz",
    "deadline": "2026-08-20T03:00:00Z"
  }
}
```

v1 requires:

```text
operationId == invocationId
attempt >= 1
argumentsJson parses as exactly one JSON value
```

`deadline` is optional. When supplied it is an RFC3339 UTC deadline owned by Core.

### 8.1 Tool result

Provider may return only authoritative provider-level outcomes:

```text
success
error
cancelled
```

Example:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_42",
  "result": {
    "outcome": {
      "kind": "success",
      "content": [{"type":"text","text":"..."}]
    }
  }
}
```

Provider MUST NOT return Core policy outcome `denied` or crash-analysis outcome `unknown` as ordinary successful protocol results. Those states are derived by Core.

A JSON-RPC transport error is not a Tool terminal outcome. Core interprets transport ambiguity using the durable `tool/dispatched` boundary and `SideEffectClass`.

## 9. LLM startup

### 9.1 Core-owned stream id

Provider Protocol v1 finalizes one change from the earlier draft: **Core allocates `streamId` before sending `llm.start`.**

This removes the race in which a Provider could emit the first `llm.event` after accepting the request but before Host had registered the Provider-allocated stream id.

Core first installs local routing for `streamId`, then sends:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_50",
  "method": "llm.start",
  "params": {
    "operationId": "req_123",
    "streamId": "str_9",
    "request": {
      "requestId": "req_123",
      "sessionId": "ses_123",
      "provider": "provider-a",
      "model": "model-x",
      "messages": [],
      "tools": [],
      "options": {}
    },
    "deadline": "2026-08-20T03:05:00Z"
  }
}
```

v1 requires:

```text
operationId == request.requestId
```

Provider accepts by echoing the Core stream id:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_50",
  "result": {
    "accepted": true,
    "streamId": "str_9"
  }
}
```

If `accepted=false`, `reason` MAY be included. Provider MUST NOT emit stream events for a rejected stream.

## 10. LLM event notification

Provider emits:

```json
{
  "jsonrpc": "2.0",
  "method": "llm.event",
  "params": {
    "streamId": "str_9",
    "seq": 1,
    "event": {
      "type": "text-delta",
      "index": 0,
      "text": "Hello"
    }
  }
}
```

For each stream:

- `seq` starts at 1;
- every event increments by exactly 1;
- `finish` occurs exactly once;
- no event follows `finish`;
- an unknown `streamId` is a protocol violation.

Supported event types:

```text
block-start
text-delta
reasoning-delta
tool-call-delta
block-end
usage
finish
```

A `finish` reason is one of:

```text
completed
max-tokens
error
cancelled
```

`completed` and `max-tokens` carry no failure. `error` carries a failure. `cancelled` carries a `CANCELLED` failure.

## 11. Cancellation

Core sends a notification:

```json
{
  "jsonrpc": "2.0",
  "method": "capability.cancel",
  "params": {
    "operationId": "inv_123",
    "cause": {"kind":"user"}
  }
}
```

Tool `operationId` is the InvocationId. LLM `operationId` is the RequestId.

Cancellation is cooperative. It is never proof that an already-dispatched side effect did not occur. Durable Agent cancellation/recovery semantics remain owned by Core.

## 12. Ping

Core request:

```json
{"jsonrpc":"2.0","id":"rpc_70","method":"provider.ping","params":{}}
```

Provider result:

```json
{"jsonrpc":"2.0","id":"rpc_70","result":{"ok":true}}
```

Ping checks the provider protocol loop, not downstream service health.

## 13. Shutdown

Core requests:

```json
{"jsonrpc":"2.0","id":"rpc_80","method":"provider.shutdown","params":{}}
```

Provider returns:

```json
{"jsonrpc":"2.0","id":"rpc_80","result":{"accepted":true}}
```

Host then closes provider stdin and waits for process exit. Deployment policy may forcibly terminate a provider that exceeds the shutdown timeout.

## 14. Failure semantics

Unexpected process exit or stdout transport failure:

- marks the provider unavailable/unhealthy;
- fails active JSON-RPC requests at the transport boundary;
- fails active LLM stream routes;
- MUST NOT automatically replay Tool side effects.

The Agent/Tool recovery layer remains responsible for deciding whether a durable dispatch may be retried.

## 15. Protocol violations

Examples include:

- non-JSON stdout;
- invalid JSON-RPC version;
- non-string or empty response id;
- response id not correlated to an active or recently timed-out request;
- provider-to-Core request in v1;
- unsupported provider notification;
- invalid manifest;
- invalid Tool result structure;
- unknown LLM stream id;
- non-contiguous LLM stream sequence;
- malformed stream event;
- event after finish.

A Host MAY mark the entire Provider process unhealthy after a protocol violation. Batch 10's reference Host does so.

## 16. Request timeout and late response

A Host request timeout retires the JSON-RPC request id. A later response using that retired id is ignored as a late response rather than misclassified as a new protocol violation.

Retired-id memory is implementation bounded. Once an id ages out of that bounded memory, a still-later response may be treated as an uncorrelated response.

## 17. Forward compatibility

Protocol `1.0` freezes the semantics in this document. Future compatibility rules will be introduced with an actual subsequent protocol revision rather than speculative negotiation code.
