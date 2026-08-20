# Batch 12 Amendment — Python Provider SDK v0.1

**Status:** Normative amendment for the v0.1 implementation line  
**Protocol compatibility:** Provider Protocol v1 (`1.0`) unchanged

## 1. Purpose

Batch 12 adds the first provider-authoring SDK. The Python SDK removes JSON-RPC/NDJSON and lifecycle boilerplate from provider implementations while preserving the language-neutral Provider Protocol as the only Core/provider contract.

The SDK is not part of Harness Core and does not change the Rust domain model. A provider implemented without the SDK remains equally valid if it conforms to Provider Protocol v1.

## 2. Package boundary

The reference package lives at:

```text
sdk/python/harness_provider_sdk
```

It has no runtime third-party dependencies and targets Python 3.11+.

The SDK MUST NOT require Rust libraries, native ABI bindings, or generated Rust types.

## 3. SDK-owned responsibilities

`ProviderApp` owns:

- JSON-RPC 2.0 request/response envelopes;
- UTF-8 NDJSON framing on stdin/stdout;
- `provider.initialize`, `provider.ping`, and `provider.shutdown`;
- manifest generation from registered Tool and LLM capabilities;
- `tool.invoke` dispatch and provider-level outcome encoding;
- `llm.start` acceptance and `llm.event` notification framing;
- per-stream sequence assignment beginning at 1;
- active operation registration;
- `capability.cancel` routing;
- graceful shutdown cancellation of active operations.

Provider author code SHOULD NOT manually write protocol frames when using the SDK.

## 4. Registration model

Tool registration declares the same execution semantics already present in ProviderManifest:

```text
name
version
parallelSafe
sideEffect
supportsIdempotencyKey
```

An `idempotent-write` Tool MUST declare `supportsIdempotencyKey=true`; registration fails otherwise.

LLM registration is model-name based. Duplicate Tool names and duplicate model names are rejected before the process begins serving protocol traffic.

## 5. Handler model

### 5.1 Tool

A Tool handler receives `ToolContext`, including decoded arguments, original `argumentsJson`, attempt, idempotency key, deadline and cancellation token.

A Tool handler returns one `ToolResult`:

```text
success
error
cancelled
```

Synchronous and asynchronous Tool handlers are supported. Synchronous handlers execute via `asyncio.to_thread` so the protocol loop remains responsive.

### 5.2 LLM

An LLM model handler MUST be declared with `async def` and receives `ModelContext`.

The context owns `LlmStreamWriter`, which assigns stream sequence numbers and provides helpers for text, reasoning, ToolCall, usage and finish events.

If a model handler returns without emitting `finish`, the SDK emits `finish(completed)` exactly once.

## 6. Cancellation

Each active capability operation owns a `CancellationToken`.

On `capability.cancel` the SDK:

1. records the first cancellation cause in the token;
2. cancels the owning asyncio task;
3. for an LLM operation, emits terminal `finish(cancelled)` if no terminal event exists;
4. for a Tool operation, completes the original `tool.invoke` request with provider outcome `cancelled`.

Cancellation remains best-effort with respect to external side effects.

In particular, Python cannot forcibly stop a worker thread created for a synchronous Tool handler. Cancellation of the awaiting asyncio task therefore MUST NOT be interpreted as proof that the underlying synchronous code stopped or that an external side effect was rolled back.

Harness Core remains authoritative for unknown-outcome and side-effect recovery semantics.

## 7. stdout / stderr invariant

SDK protocol output uses stdout exclusively. SDK diagnostics use stderr.

Provider author code MUST NOT print application diagnostics to stdout.

## 8. Reference provider

`providers/example-python/provider.py` is migrated from hand-written JSON-RPC/NDJSON handling to the Python SDK.

Its externally observable capabilities remain:

```text
tool: echo
models: echo-model, agent-model
```

This means the existing Rust out-of-process Agent acceptance test continues to exercise the same provider behavior while now also proving the SDK layer.

## 9. Compatibility

Batch 12 does not change Provider Protocol v1 wire schemas or methods.

The SDK is one implementation convenience layer:

```text
Provider author code
        ↓
Python SDK
        ↓
Provider Protocol v1
        ↓
ProviderHost
        ↓
Rust Core
```

A future TypeScript, Go or Rust SDK MUST target the same wire contract rather than depending on Python SDK behavior.
