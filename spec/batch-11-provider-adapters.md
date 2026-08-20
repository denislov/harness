# Batch 11 — Provider Host Domain Adapters

**Status:** normative v0.1 supplement

## 1. Purpose

Batch 11 connects the out-of-process Provider Protocol v1 transport introduced in Batch 10 to the existing provider-neutral Harness domain seams.

The resulting dependency direction is:

```text
harness-agent
  -> harness-llm / harness-tools
       ^
       |
harness-provider-host adapters
       |
       v
harness-provider-protocol
       |
       v
foreign-language provider process
```

Agent Core MUST NOT depend on Provider Protocol wire types.

## 2. Adapter ownership

`harness-provider-host` owns the protocol/domain mapping layer because it is the first crate that is allowed to depend on both:

- protocol transport/wire vocabulary; and
- Harness LLM/Tool domain vocabulary.

`harness-provider-protocol` remains wire-only and MUST NOT depend on `harness-types`, `harness-llm`, `harness-tools`, or `harness-agent`.

## 3. LLM adapter

`ProviderHostLlmAdapter` implements `harness_llm::LlmProvider`.

For one logical provider attempt:

1. Core supplies a validated `ModelRequest`.
2. The adapter converts that request to `WireModelRequest` without rewriting opaque identifiers or `argumentsJson` strings.
3. `ProviderHost::start_llm` allocates/routes the protocol stream.
4. Wire stream events are mapped to `SequencedStreamEvent`.
5. Provider transport/protocol failures are normalized to `PortableError`.
6. Retry policy remains owned by Agent Core; the adapter MUST NOT retry model attempts.

The adapter does not own the Agent timeout. Agent timeout remains the authoritative execution deadline in v0.1.

## 4. Tool adapter

`ProviderHostToolAdapter` implements `harness_tools::ToolExecutor` for one named Tool capability.

The adapter MUST bind to a Tool capability declared by the initialized provider manifest.

Before the adapter is registered in Core, `ProviderHostToolAdapter::from_definition()` SHOULD be used (or `validate_definition()` MUST be called explicitly) to verify that the Core-authoritative `ToolDefinition` agrees with the provider manifest for:

- name;
- version;
- `parallelSafe`;
- side-effect class.

The Core definition remains authoritative for model exposure and policy, but a semantic mismatch MUST be rejected rather than silently coerced.

`supportsIdempotencyKey=true` maps to `IdempotencySupport::Keyed`; otherwise it maps to `None`.

The adapter represents exactly one provider dispatch attempt and MUST NOT retry internally.

## 5. Domain/wire conversion

Opaque identifiers remain strings at the protocol boundary. Conversion back into Harness strong IDs MUST validate non-empty identifier invariants.

`argumentsJson` MUST remain one JSON string containing one complete JSON value. The adapter MUST NOT parse and reserialize it as part of ordinary conversion.

Content blocks, portable errors, token usage, Tool outcomes, and cancellation causes MUST retain their semantic values across the adapter boundary.

Provider-only Tool outcomes remain limited to:

- success;
- error;
- cancelled.

`denied` remains a Core policy outcome. `unknown` remains a Core recovery interpretation.

## 6. Error mapping

Protocol framing/shape/mapping failures become `PROVIDER_PROTOCOL_ERROR`.

Provider transport loss becomes `PROVIDER_UNAVAILABLE`.

Provider Host request timeout becomes `DEADLINE_EXCEEDED`.

Provider-declared RPC/stream rejection is normalized to the owning domain operation error (`MODEL_REQUEST_FAILED` or `TOOL_EXECUTION_FAILED`) unless a more specific stable mapping exists.

These mappings describe one provider attempt only. Agent Core still decides durable retry/recovery semantics.

## 7. Cancellation hook

Batch 11 extends the provider-neutral seams with default no-op cancellation hooks:

```text
LlmProvider::cancel(RequestId, CancelCause)
ToolExecutor::cancel(InvocationId, CancelCause)
```

Existing in-process providers remain source compatible because the default implementation succeeds without action.

Out-of-process Provider Host adapters map these hooks to `capability.cancel`.

Cancellation remains best effort. A successful cancellation-hook call does not prove an external side effect was rolled back.

## 8. Cancellation ordering

For explicit Agent cancellation, Core MUST first commit the durable terminal/recovery state determined by Batch 09 semantics. Only after that commit may it invoke the capability cancellation hook and abort the process-local task.

Therefore:

```text
durable cancellation / recovery decision
    -> capability.cancel best effort
    -> local task abort
    -> command acknowledgement already rests on durable state
```

A failure to deliver `capability.cancel` MUST NOT roll back the durable cancellation result.

## 9. Timeout propagation

When an Agent-owned LLM or Tool timeout fires, the operation task SHOULD invoke the domain cancellation hook with `CancelCause::Timeout` before reporting `DEADLINE_EXCEEDED` back to the actor.

For Tools, the durable `tool/dispatched` boundary remains authoritative. A timeout after dispatch can still represent an unknown external outcome, especially for non-idempotent writes.

## 10. Python acceptance provider

The Batch 10 `echo-model` behavior remains available for low-level protocol conformance.

Batch 11 adds an `agent-model` capability to the same Python reference provider:

```text
first model request
  -> ToolCall(echo)

echo Tool result
  -> second model request
  -> final text answer
```

This provider is deliberately deterministic. Its purpose is architecture conformance, not model quality.

## 11. End-to-end acceptance invariant

The Batch 11 integration test MUST execute this path through a real subprocess:

```text
user/message
  -> model/requested
  -> Rust ProviderHostLlmAdapter
  -> JSON-RPC/NDJSON
  -> Python agent-model
  -> assistant/message(ToolCall)
  -> tool/call
  -> tool/dispatched
  -> Rust ProviderHostToolAdapter
  -> JSON-RPC/NDJSON
  -> Python echo Tool
  -> tool/result
  -> step/ended(tool-continuation)
  -> next model/requested
  -> Python agent-model
  -> assistant/message(final text)
  -> step/ended(completed)
  -> turn/ended(completed)
```

The test MUST NOT use an in-process fake LLM or fake Tool executor.

## 12. Non-goals

Batch 11 does not introduce:

- Node, Go, or TypeScript SDKs;
- provider pools or load balancing;
- provider hot reload;
- remote TCP/gRPC transport;
- automatic provider restart;
- parallel Tool scheduling;
- cross-provider failover.
