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

Suggested files:

```text
command.rs
recovery.rs
bootstrap.rs
inbox.rs
state.rs
loop_driver.rs
actor.rs
lib.rs
```

Responsibilities:

- Agent actor;
- AgentCommand;
- Session bootstrap and RecoveryAnalyzer;
- ResumeDecision and durable recovery classification;
- Inbox projection and delivery behavior;
- AgentPhase and ExecutionGate;
- turn/step driver;
- cancellation convergence.

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
- application-facing façade.

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
