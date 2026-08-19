# LLM Domain and Streaming Specification

**Status:** Draft v0.1

## 1. Provider-neutral ModelRequest

Harness Core builds one provider-neutral model request per attempt.

Conceptual Rust shape:

```rust
pub struct ModelRequest {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub provider: String,
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ModelToolSpec>,
    pub options: ModelOptions,
}
```

Provider-specific API conversion occurs only inside the selected LLM Provider.

## 2. ModelOptions

v0.1 requires only portable options that Core can represent without provider-specific types. Initial fields MAY include:

```text
maxOutputTokens
reasoningEffort
```

Provider-specific options are not part of the stable v0.1 contract unless explicitly standardized later.

An unsupported optional field must produce a structured capability/configuration error rather than silent semantic reinterpretation.

## 3. Tool specifications sent to the model

`ModelToolSpec` contains only model-visible information required for tool calling:

```text
name
description
inputSchema
```

Security policy, provider binding, side-effect classification and credential configuration are not model authority and need not be exposed as model tool schema.

## 4. Request construction

The logical inputs are:

```text
SessionProjector -> Message history
PromptRegistry   -> system prompt
ToolRegistry     -> model-visible tool schemas
Agent config     -> provider/model/options
```

The resulting `ModelRequest` is serialized and written to BlobStore before provider dispatch.

Core then commits `model/requested`, referencing the snapshot.

## 5. Exact request snapshot

The snapshot MUST contain the exact provider-neutral request object supplied to the Provider Host for that attempt.

It MUST be immutable after `model/requested` commit.

The snapshot allows later audit even if:

- prompt assembly rules change;
- tool descriptions change;
- projection code changes;
- provider adapters change.

## 6. StreamEvent

v0.1 stream event vocabulary:

```text
block-start
text-delta
reasoning-delta
tool-call-delta
block-end
usage
finish
```

Conceptual Rust enum:

```rust
pub enum StreamEvent {
    BlockStart { index: u32, block_type: BlockType },
    TextDelta { index: u32, text: String },
    ReasoningDelta { index: u32, text: String },
    ToolCallDelta {
        index: u32,
        call_id: ToolCallId,
        name: Option<String>,
        arguments_delta: String,
    },
    BlockEnd { index: u32, block: ContentBlock },
    Usage(TokenUsage),
    Finish(FinishEvent),
}
```

## 7. Stream ordering rules

For one stream:

1. Provider-assigned stream sequence numbers start at a positive integer and strictly increase by one.
2. Exactly one `finish` event MUST occur.
3. No event may follow `finish`.
4. `usage`, when present, MUST appear before `finish`.
5. `tool-call-delta.argumentsDelta` is raw JSON text fragments; the final ToolCall block contains a complete raw JSON string.
6. A `block-end` event carries the complete assembled block for that block index.

Protocol violations terminate the attempt with `PROVIDER_PROTOCOL_ERROR`.

## 8. Finish reasons

v0.1 finish reasons:

```text
completed
max-tokens
error
cancelled
```

`error` and `cancelled` MUST carry normalized failure information sufficient for Core to distinguish transport/provider failure from caller cancellation.

## 9. TokenUsage

Portable v0.1 usage fields:

```text
inputTokens
outputTokens
cacheReadTokens? 
cacheWriteTokens?
reasoningTokens?
```

When fields are unavailable from a provider they are omitted rather than fabricated.

## 10. Assembling the assistant message

Core owns stream assembly into the authoritative provider-neutral assistant Message.

Provider stream chunks are RuntimeEvents in v0.1. The final assembled message is recorded as durable `assistant/message`.

Core MUST NOT record a normal authoritative assistant message for an attempt that terminates with `error` or `cancelled`.

## 11. Retry ownership

An LLM Provider performs one provider attempt per Core request attempt unless the Provider Protocol explicitly standardizes transparent transport retry in the future.

Core owns logical retry policy. A retry is represented as another `model/requested` attempt and therefore remains observable and auditable.

## 12. Cancellation

Core cancellation propagates to the active provider operation through Provider Protocol. The first accepted cancellation cause remains authoritative.

Provider-side completion that races with cancellation is resolved by Core according to the operation state observed at the authoritative boundary; providers MUST NOT independently mutate durable outcome state.

## 13. Batch 07 Rust reference contract

The Rust reference implementation introduces `harness-llm` as a runtime-neutral domain crate. It does not depend on Tokio and does not manage provider processes.

The normalized in-process provider seam is conceptually:

```rust
pub trait LlmProvider: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    fn stream(&self, request: ModelRequest) -> LlmEventStream;
}
```

`LlmEventStream` yields `SequencedStreamEvent` values or normalized `PortableError` failures. Provider Host implementations added later MUST adapt their wire protocol into the same domain stream rather than exposing transport-specific events to Agent Core.

`StreamSeq` is a positive cross-language-safe integer. The first emitted event MUST use sequence 1 and each later event MUST increment by exactly one.

`LlmStreamAssembler` validates sequence order, block lifecycle, delta/block-end consistency, usage multiplicity, finish payload shape, exactly one terminal finish, and the prohibition on post-finish events. A stream protocol violation becomes `PROVIDER_PROTOCOL_ERROR` at the Agent boundary.

## 14. Live LLM operation boundary

Before one live LLM operation starts, Core MUST perform this order:

```text
build provider-neutral ModelRequest
    -> serialize exact request snapshot
    -> BlobStore.put(snapshot)
    -> commit model/requested(snapshot BlobRef)
    -> mark process-local ActiveAgentOperation::Model
    -> spawn provider stream future
```

The provider stream future MUST NOT be awaited by the single-owner Agent mailbox loop. It runs as an external task and reports one normalized completion back through the same actor mailbox.

This separation keeps `followup`, `steer`, snapshot, shutdown, and future cancellation commands responsive while model I/O is pending.

A process-local active model operation is not durable. If the process disappears after `model/requested`, normal startup recovery interprets the pending request as an interrupted attempt.

## 15. Batch 07 post-assistant boundary

For an assistant response without ToolCall blocks, Core may immediately append `step/ended` and deterministically continue or close the turn.

For an assistant response containing ToolCall blocks, Batch 07 commits the authoritative `assistant/message` and leaves the step open. Tool definition resolution, `tool/call` persistence, policy, dispatch, and continuation are Batch 08 responsibilities. The existing open-step assistant projection prevents a restart or later driver pass from issuing a duplicate model request.

## 16. Finish normalization details

A normalized `finish(cancelled)` MUST carry a `PortableError` whose code is `CANCELLED`. A normalized `finish(error)` MUST NOT carry `CANCELLED`; cancellation therefore cannot be accidentally reclassified as an ordinary provider failure by the Agent layer.

An `LlmProvider` stream MUST terminate after emitting its terminal `Finish` event. The reference operation consumes the stream through that terminal boundary and treats a missing `Finish` before stream termination as `PROVIDER_PROTOCOL_ERROR`. Timeout enforcement for a provider that never terminates remains deferred to the capability timeout layer.

`LlmProvider::stream` MUST return promptly and MUST NOT perform blocking I/O before returning the Stream. Asynchronous provider setup belongs in the returned stream implementation. This preserves actor/runtime responsiveness even when the reference runtime is configured with a single Tokio worker.
