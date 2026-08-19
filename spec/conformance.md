# Conformance Requirements

**Status:** Draft v0.1

## 1. Purpose

A language-neutral protocol succeeds only if every SDK and provider interprets the same semantics identically. A shared conformance suite is therefore a required deliverable, not optional test infrastructure.

## 2. Provider conformance

Every Provider SDK and reference provider MUST pass tests covering at least the following.

### Initialization

- successful `provider.initialize`;
- incompatible major protocol version;
- invalid manifest;
- duplicate capability identity;
- provider exit during initialization.

### Framing

- one valid JSON-RPC object per NDJSON line;
- UTF-8 handling;
- malformed JSON on stdout;
- non-protocol debug text on stdout;
- stderr logging does not affect framing.

### Tool invocation

- success outcome;
- normal Tool error;
- cancellation;
- deadline exceeded;
- provider crash before dispatch acknowledgement;
- provider crash after invocation has begun;
- idempotency key preservation across retry attempts;
- raw `argumentsJson` preservation.

### LLM streaming

- ordered text stream;
- reasoning stream;
- interleaved content blocks;
- tool-call argument deltas;
- block-end complete block;
- usage before finish;
- exactly one finish;
- event after finish rejected;
- duplicate/decreasing stream seq rejected;
- unknown streamId rejected;
- provider crash during stream;
- caller cancellation during stream.

### Lifecycle

- ping success;
- graceful shutdown;
- forced termination after failed graceful shutdown;
- no new operations accepted after stopping begins.

## 3. SessionStore conformance

Every SessionStore backend MUST pass:

- create unique session;
- duplicate create returns conflict;
- atomic append batch;
- expected-sequence conflict;
- strict read order;
- no partial committed batch visibility;
- fork through exact boundary;
- fork excludes later source events;
- structural corruption detection where backend can detect corruption.

## 4. BlobStore conformance

Every BlobStore backend MUST pass:

- put/get byte equality;
- stable digest;
- immutable BlobRef behavior;
- digest verification;
- missing blob failure;
- content-type round trip when supplied.

## 5. Agent invariant tests

Reference Core tests MUST prove:

- no two drivers for one Agent;
- cancelled activity cannot consume post-cancel waking input;
- Inbox acknowledgement occurs only after durable enqueue;
- blocked ExecutionGate prevents new turn;
- one ToolCall cannot commit two terminal outcomes;
- provider crash during non-idempotent write cannot trigger automatic retry;
- model request snapshot exists before provider dispatch;
- model-visible history can be rebuilt from durable session state.

## 6. Deterministic fake providers

The repository SHOULD include deterministic fake Tool and LLM providers used by Core tests.

A minimum vertical-slice fixture should produce:

```text
User asks to read foo.txt
 -> fake LLM emits read_file call
 -> fake Tool returns "hello"
 -> fake LLM returns final answer
```

Expected durable event outline:

```text
session/created
inbox/enqueued
turn/started
inbox/claimed
step/started
user/message
model/requested
assistant/message
tool/call
tool/result
step/ended
step/started
model/requested
assistant/message
step/ended
turn/ended
```

Exact EventSeq values are assigned by the test store.

## 7. Cross-language matrix

Before Provider Protocol v1 is considered stable, the conformance suite SHOULD run against at least:

- Rust provider SDK;
- Python provider SDK;
- TypeScript provider SDK.

Go SDK may follow but must pass the same suite before being advertised as compatible.
