# Rust Core Workspace Layout

**Status:** Draft v0.1 recommendation

## 1. Workspace

Recommended initial repository structure:

```text
harness/
├── Cargo.toml
├── crates/
│   ├── harness-types/
│   ├── harness-session/
│   ├── harness-tools/
│   ├── harness-llm/
│   ├── harness-agent/
│   ├── harness-provider-protocol/
│   ├── harness-provider-host/
│   ├── harness-runtime/
│   └── harness-storage-local/
├── sdk/
│   ├── python/
│   ├── typescript/
│   ├── go/
│   └── rust/
├── providers/
├── spec/
└── conformance/
```

This decomposition is intentionally smaller than a mature plugin ecosystem. v0.1 prioritizes semantic stability over package count.

## 2. `harness-types`

Contains low-level cross-subsystem value objects with minimal runtime dependencies:

```text
ids.rs
message.rs
blob.rs
error.rs
lib.rs
```

Responsibilities:

- branded identifier newtypes;
- Message and ContentBlock;
- BlobRef;
- stable portable errors;
- common counters/time wrappers.

It SHOULD NOT depend on Agent runtime, SessionStore implementations, process spawning, or Provider Host.

## 3. `harness-session`

Suggested files:

```text
event.rs
projector.rs
store.rs
lib.rs
```

Responsibilities:

- SessionEvent and payload types;
- SessionProjector;
- SessionStore semantic trait;
- reconstruction and structural validation.

It depends on `harness-types`.

## 4. `harness-tools`

Suggested files:

```text
definition.rs
invocation.rs
outcome.rs
policy.rs
registry.rs
lib.rs
```

Responsibilities:

- ToolDefinition;
- SideEffectClass;
- ToolInvocation;
- ToolOutcome;
- Tool registry and Core policy abstractions.

It depends on `harness-types`; it may depend on stable session interfaces only when required, but cyclic dependencies must be avoided.

## 5. `harness-llm`

Suggested files:

```text
request.rs
stream.rs
assembler.rs
registry.rs
lib.rs
```

Responsibilities:

- ModelRequest;
- ModelOptions;
- StreamEvent;
- stream assembly;
- provider-neutral LLM routing interfaces.

## 6. `harness-agent`

Current v0.1 Rust layout:

```text
command.rs
error.rs
event_source.rs
handle.rs
state.rs
bootstrap.rs
recovery.rs
actor.rs
runtime.rs
loop_driver.rs
lib.rs
```

Responsibilities:

- single-owner Agent actor;
- cloneable AgentHandle and bounded command mailbox;
- AgentCommand plus durable acknowledgements;
- injected AgentEventSource for durable event identity/time;
- bootstrap and startup recovery convergence;
- Inbox projection and wake behavior;
- AgentPhase and ExecutionGate;
- deterministic turn/step preparation;
- external-operation driver boundaries;
- future cancellation convergence.

### 6.1 `loop_driver.rs`

`loop_driver.rs` contains the deterministic planning layer introduced in Batch 06. It reads `AgentState` and produces the next process-local driver plan without performing storage or provider I/O.

Conceptually:

```text
AgentState + SessionProjection
          |
          v
      plan_next()
          |
          +-- Dormant
          +-- StartNewTurn
          +-- StartStep
          +-- EnterCurrentStep
          +-- EndOpenTurn
          +-- Park(ReadyForModel)
          +-- Deferred
```

`AgentActor` owns execution of the plan. It converts plans into `NewSessionEvent` batches, validates them against the exact local history, commits them through `SessionStore`, and refreshes projection/recovery state.

The planner MUST NOT call LLM, Tool, ProviderHost, approval, or network interfaces.

### 6.2 Deterministic versus external driver work

Batch 06 establishes a strict split:

```text
actor deterministic work
    turn/step numbering
    inbox selection
    inbox claim
    user/message entry
    exhausted-turn closure
           |
           v
    ReadyForModel boundary
----------- external boundary -----------
           |
           v
future LLM operation
```

This crate coordinates session, tools and LLM abstractions but should not contain provider process management.

## 7. `harness-provider-protocol`

Contains only wire-domain types and JSON-RPC method schemas.

Important rule:

> Wire types are not aliases of internal Rust domain types.

The crate provides explicit conversion boundaries so internal refactors do not automatically become cross-language breaking changes.

Suggested files:

```text
version.rs
wire.rs
manifest.rs
tool.rs
llm.rs
error.rs
lib.rs
```

## 8. `harness-provider-host`

Suggested files:

```text
process.rs
codec.rs
router.rs
stream_router.rs
supervisor.rs
state.rs
lib.rs
```

Responsibilities:

- process spawn;
- NDJSON framing;
- JSON-RPC correlation;
- initialization;
- operation routing;
- LLM stream routing;
- deadlines and cancellation delivery;
- provider state;
- shutdown and failure reporting.

It must not own Tool retry semantics.

## 9. `harness-runtime`

Application-level composition crate.

Responsibilities:

- registry wiring;
- scope hierarchy;
- provider binding;
- Agent Registry;
- startup/shutdown orchestration;
- application-facing facade.

A future CLI or server binary should depend primarily on this crate.

## 10. `harness-storage-local`

Reference local backends:

- MemorySessionStore for deterministic tests;
- SQLiteSessionStore or equivalent embedded durable store;
- FilesystemBlobStore.

Production remote storage adapters can live in separate crates later.

## 11. Dependency direction

Recommended conceptual direction:

```text
                  harness-types
                /      |       \
               v       v        v
          harness-session  harness-tools  harness-llm
                 \         |        /
                  \        |       /
                   v       v      v
                    harness-agent
                         |
                         v
                   harness-runtime

harness-provider-protocol
            |
            v
harness-provider-host ---------> harness-runtime

harness-storage-local ---------> session/blob abstractions
```

Exact Cargo edges may differ to avoid cycles. The invariant is that low-level domain crates do not depend on high-level runtime composition.

## 12. Async-runtime boundary

Domain/value crates SHOULD avoid binding themselves to a specific async executor unless necessary.

Async/process concerns belong primarily in:

- `harness-agent`;
- `harness-provider-host`;
- `harness-runtime`;
- concrete storage implementations.

This keeps protocol and durable-domain types portable and testable.

The Rust reference implementation selects Tokio for the live `harness-agent` execution layer beginning in Batch 05. Tokio types are not introduced into `harness-types` or `harness-session`. The production policy for globally unique EventId generation remains a `harness-runtime` composition concern and is injected into `harness-agent` through `AgentEventSource`.

Beginning in Batch 06, deterministic driver planning itself remains synchronous and provider-free even though the actor that commits the resulting plans is asynchronous. Future LLM/Tool waits should be represented as operations outside the deterministic planning function so mailbox progress can continue while capability I/O is pending.

## 13. First implementation vertical slice

The first executable slice SHOULD contain only:

```text
harness-types
MemorySessionStore
SessionProjector
Agent actor
Agent Loop
Fake in-process LLM
ToolRegistry
Fake read_file Tool
Memory BlobStore
```

The slice is complete when it can deterministically execute:

```text
user -> LLM -> tool -> LLM -> final answer
```

and produce the expected durable event sequence before any out-of-process provider implementation is added.

Batch 06 reaches the following intermediate milestone:

```text
Send
 -> inbox/enqueued
 -> wake
 -> turn/started
 -> inbox/claimed
 -> step/started
 -> user/message
 -> ReadyForModel
```

The next implementation batch may therefore focus on `harness-llm` and an in-process fake model operation without changing Turn/Step entry semantics.

## 14. Batch 07 storage abstraction crate amendment

Beginning with Batch 07, the Rust workspace also contains:

```text
crates/
├── harness-storage/        # generic BlobStore abstraction
└── harness-storage-local/  # MemorySessionStore + MemoryBlobStore
```

`harness-storage` depends only on `harness-types` plus async/error support. `harness-storage-local` implements the abstraction and may also implement `harness-session` storage contracts.

The relevant dependency direction is now:

```text
harness-types
    ├──> harness-session
    ├──> harness-storage
    └──> harness-llm

harness-session ----┐
harness-storage ----┼──> harness-agent
harness-llm --------┘

harness-session ----┐
harness-storage ----┼──> harness-storage-local
harness-types ------┘
```

`harness-agent` never depends on the concrete `harness-storage-local` backend outside tests.

## 15. Batch 08 Tool runtime layout

`harness-tools` is now implemented with the following reference modules:

```text
harness-tools/src/
├── definition.rs
├── executor.rs
├── invocation.rs
├── policy.rs
├── registry.rs
├── validation.rs
└── lib.rs
```

`harness-agent` adds:

```text
harness-agent/src/
├── tool_driver.rs
├── tool_operation.rs
├── tool_runtime.rs
├── tool_tests.rs
└── actor/
    └── tool_support.rs
```

The dependency direction is:

```text
harness-types -----> harness-tools
harness-tools -----> harness-agent
harness-llm -------> harness-agent
harness-session ---> harness-agent
```

`harness-tools` does not depend on Agent, SessionStore, Tokio, or Provider Host. The in-process Tool executor returns a generic Send Future; Tokio task spawning remains an Agent execution-layer concern.

The first vertical slice is complete when the integration test demonstrates:

```text
user
 -> model requests read_file
 -> durable tool/call
 -> durable tool/dispatched
 -> Fake read_file executor
 -> durable tool/result
 -> step/ended(tool-continuation)
 -> second model request sees ToolResult
 -> final assistant answer
 -> turn/ended(completed)
```

