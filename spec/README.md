# Language-Agnostic Harness Specification v0.1

**Status:** Draft specification  
**Core implementation language:** Rust  
**Protocol version:** 1.0 draft  
**Date:** 2026-08-19

## 1. Purpose

This specification defines the stable architectural and protocol contracts for a language-agnostic agent harness whose control plane is implemented in Rust and whose capability providers may be implemented in any language.

The design is inspired by the strongest architectural properties of modern agent harnesses: an event-sourced session log, an explicit agent state machine, a policy-aware tool pipeline, provider-neutral LLM streaming, and replaceable capability providers. It intentionally does not depend on JavaScript module semantics, Rust ABI stability, or any one plugin runtime.

The v0.1 goal is to make the core semantics precise enough that the following can be implemented independently without semantic drift:

- the Rust Harness Core;
- a local durable SessionStore;
- an in-process Tool and LLM test implementation;
- the out-of-process Provider Host;
- Python, TypeScript, Go, and Rust provider SDKs;
- a cross-language conformance test suite.

## 2. Normative language

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHALL**, **SHALL NOT**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY**, and **OPTIONAL** are to be interpreted as normative requirements.

## 3. Stable v0.1 decisions

The following decisions are considered part of the v0.1 architecture:

1. Harness Core is implemented in Rust.
2. Harness Core is the single control plane and owns authoritative state and ordering.
3. Each live Agent is an actor with a single active driver.
4. Durable session state is event-sourced and append-only.
5. Model-visible history is derived from durable session state.
6. Every model request is snapshotted before dispatch for exact audit and replay.
7. Agent input is persisted through a two-boundary Inbox: `next-turn` and `next-step`.
8. Tool calls and tool invocations are distinct concepts.
9. Tool side effects are classified as `read-only`, `idempotent-write`, or `non-idempotent-write`.
10. Unknown outcomes of non-idempotent writes block automatic continuation.
11. Capability providers are out-of-process and language-neutral.
12. Provider Protocol v1 uses JSON-RPC 2.0, NDJSON framing, and stdin/stdout transport.
13. Provider stdout is protocol-only; provider logs use stderr.
14. Provider Protocol v1 exposes only two first-class capability families: Tool and LLM.
15. Control-plane middleware remains in-process with the Rust Core in v0.1.
16. LLM streaming chunks are runtime events in v0.1; the final assistant message is durable.
17. Session events and runtime events are distinct event domains.
18. A provider crash is detected by Provider Host; retry and recovery semantics are decided by the owning Core subsystem.

## 4. Specification index

| Document | Scope |
|---|---|
| [architecture.md](architecture.md) | System boundaries, responsibilities, module decomposition, extension model |
| [domain-model.md](domain-model.md) | Core identifiers, messages, content blocks, blobs, scopes and common types |
| [invariants.md](invariants.md) | Cross-cutting safety, ordering and durability invariants |
| [session-events.md](session-events.md) | Durable event envelope, event taxonomy, ordering and projection rules |
| [agent-lifecycle.md](agent-lifecycle.md) | Agent actor, Inbox, turn/step lifecycle, cancellation and execution gate |
| [tool-execution.md](tool-execution.md) | Tool definitions, policy pipeline, invocation identity, parallelism and recovery |
| [llm-protocol.md](llm-protocol.md) | Provider-neutral model request, stream events, request snapshots and failures |
| [provider-protocol.md](provider-protocol.md) | JSON-RPC framing, handshake, Tool/LLM RPC, cancellation and provider lifecycle |
| [storage.md](storage.md) | SessionStore, BlobStore, optimistic concurrency, fork and local backend expectations |
| [error-model.md](error-model.md) | Stable machine-readable error taxonomy and ownership rules |
| [security.md](security.md) | Trust boundaries, policy ownership, credentials, provider permissions and sandboxing |
| [conformance.md](conformance.md) | Required provider and storage conformance tests |
| [rust-core-layout.md](rust-core-layout.md) | Recommended Rust workspace boundaries and dependency direction |

## 5. Explicitly deferred from v0.1

The following are intentionally not specified as stable v0.1 contracts:

- arbitrary dynamic-library plugins (`.so`, `.dll`, `.dylib`);
- out-of-process control middleware;
- distributed Agent Loop ownership;
- cross-provider ACID transactions;
- self-modifying runtime plugins;
- first-class Browser, Filesystem, Shell, Embedding, or Reranker capability protocols; they are represented as Tools in v0.1;
- durable per-token LLM stream replay;
- Subagent protocol and orchestration semantics;
- compaction algorithm and summarization policy;
- UI, Web, IDE, ACP, or MCP presentation contracts;
- remote network transport for providers;
- WASM component transport;
- final production configuration schema.

These may be added in later protocol or architecture revisions without weakening the invariants defined here.
