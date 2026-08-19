# Provider Protocol v1

**Status:** Draft protocol 1.0

## 1. Purpose

Provider Protocol allows language-neutral out-of-process providers to supply Tool and LLM capabilities to Rust Harness Core without sharing a native ABI.

v1 transport is local stdio. Protocol semantics are designed so that other transports may be added later without changing the domain contracts.

## 2. Transport

Provider Host spawns a provider process and communicates using:

```text
JSON-RPC 2.0
NDJSON framing
stdin/stdout
UTF-8
```

Each line on stdin or stdout is one complete JSON-RPC message.

Provider stdout MUST contain only protocol messages. Provider diagnostics and logs MUST be written to stderr.

Malformed stdout is a protocol violation.

## 3. JSON conventions

- JSON object fields use lower camel case.
- Unknown optional fields SHOULD be ignored for forward compatibility.
- Unknown required method names receive the normal JSON-RPC method-not-found response.
- Stable string enums use lowercase and hyphenated tokens.
- Identifier fields are opaque strings.
- Sequence/counter fields are non-negative safe JSON integers.

## 4. Lifecycle state

ProviderHost models at least:

```text
starting
ready
unhealthy
stopping
stopped
```

Only `ready` providers may receive new capability operations.

## 5. Initialization

Core sends:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_1",
  "method": "provider.initialize",
  "params": {
    "protocolVersion": "1.0",
    "runtime": {
      "name": "harness",
      "version": "0.1.0"
    }
  }
}
```

Provider returns:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_1",
  "result": {
    "providerId": "python-ai",
    "providerVersion": "1.2.0",
    "protocolVersion": "1.0",
    "capabilities": []
  }
}
```

Provider MUST NOT be considered Ready before initialization succeeds and the returned manifest validates.

## 6. ProviderManifest

Conceptual shape:

```text
providerId
providerVersion
protocolVersion
capabilities[]
```

v1 capability kinds:

```text
tool
llm
```

### 6.1 Tool capability descriptor

Example:

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

Tool schemas and descriptions MAY be supplied by Core composition or provider manifest according to deployment architecture, but the resolved definition in Core is authoritative before model exposure.

### 6.2 LLM capability descriptor

Example:

```json
{
  "kind": "llm",
  "provider": "provider-a",
  "models": ["model-x", "model-y"]
}
```

A provider MAY expose dynamic model availability; exact discovery policy beyond the manifest is deferred.

## 7. Tool invocation

Core request:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_42",
  "method": "tool.invoke",
  "params": {
    "operationId": "inv_123",
    "invocationId": "inv_123",
    "callId": "call_abc",
    "tool": "read_file",
    "argumentsJson": "{\"path\":\"README.md\"}",
    "attempt": 1,
    "idempotencyKey": "idem_xyz",
    "deadline": "2026-08-19T13:00:30Z"
  }
}
```

Successful protocol response:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_42",
  "result": {
    "outcome": {
      "kind": "success",
      "content": [
        {"type": "text", "text": "..."}
      ]
    }
  }
}
```

Provider-level Tool outcomes may include:

```text
success
error
cancelled
```

`denied` is normally owned by Core policy before dispatch. `unknown` is normally derived by Core from uncertain transport/crash boundaries rather than claimed by a normal provider response.

## 8. LLM start

Core request:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_50",
  "method": "llm.start",
  "params": {
    "operationId": "req_123",
    "request": {
      "requestId": "req_123",
      "sessionId": "ses_123",
      "provider": "provider-a",
      "model": "model-x",
      "messages": [],
      "tools": [],
      "options": {}
    },
    "deadline": "2026-08-19T13:05:00Z"
  }
}
```

Provider responds immediately after accepting the stream:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_50",
  "result": {
    "streamId": "str_123",
    "accepted": true
  }
}
```

The accepted stream then emits notifications.

## 9. LLM stream notification

```json
{
  "jsonrpc": "2.0",
  "method": "llm.event",
  "params": {
    "streamId": "str_123",
    "seq": 1,
    "event": {
      "type": "text-delta",
      "index": 0,
      "text": "Hello"
    }
  }
}
```

The final notification contains a `finish` event. No further event may use that streamId after finish.

## 10. Cancellation

Core sends a notification or request:

```json
{
  "jsonrpc": "2.0",
  "method": "capability.cancel",
  "params": {
    "operationId": "inv_123",
    "cause": {
      "kind": "user"
    }
  }
}
```

For Tools, `operationId` is the InvocationId. For LLM calls, it is the RequestId.

Cancellation is cooperative but provider implementations MUST make a best effort to abort external I/O and stop producing stream events promptly.

## 11. Ping

Core MAY send:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_ping_1",
  "method": "provider.ping",
  "params": {}
}
```

Provider responds successfully if its protocol loop is healthy.

Ping success does not prove downstream capability health.

## 12. Shutdown

Core requests graceful shutdown:

```json
{
  "jsonrpc": "2.0",
  "id": "rpc_shutdown_1",
  "method": "provider.shutdown",
  "params": {}
}
```

After acknowledgement, provider should stop accepting operations, cancel/drain owned operations according to implementation policy, and exit.

ProviderHost MAY forcibly terminate a provider that exceeds deployment shutdown limits.

## 13. Provider process failure

Unexpected process exit fails all active operations associated with that process as `PROVIDER_UNAVAILABLE` at the transport boundary.

ProviderHost MUST NOT automatically replay side-effecting operations. The owning Core subsystem interprets the failure.

## 14. Protocol errors

Examples of protocol violations:

- non-JSON bytes on stdout;
- duplicate or decreasing LLM stream sequence number;
- event after `finish`;
- unknown streamId;
- malformed ContentBlock;
- response id that cannot be correlated to an active request;
- invalid manifest;
- Tool result that does not satisfy required protocol structure.

Protocol violations produce `PROVIDER_PROTOCOL_ERROR` for affected operations and MAY mark the provider Unhealthy.

## 15. Versioning

v1 protocol version is `1.0`.

Compatibility policy:

- different major version: incompatible;
- same major with compatible minor revision: feature/capability negotiation may allow operation;
- implementations SHOULD ignore unknown optional fields;
- implementations MUST fail loudly when a required semantic capability is unsupported.
