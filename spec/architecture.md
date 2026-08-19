# Architecture

**Status:** Draft v0.1

## 1. Architectural objective

The system separates a single authoritative Rust control plane from a language-neutral capability plane.

```text
Clients: CLI / Web / IDE / API
              |
              v
+----------------------------------------------+
|                HARNESS CORE                  |
|                  Rust                        |
|                                              |
| Agent Registry       Session Event Store     |
| Agent Actors         Session Projector       |
| Agent Loop           Prompt Registry         |
| Inbox                Tool Registry           |
| Policy Engine        LLM Registry            |
| Provider Supervisor  Capability Router       |
| Blob Store           Runtime Event Bus       |
+----------------------+-----------------------+
                       |
              Provider Protocol v1
           JSON-RPC 2.0 / NDJSON / stdio
                       |
        +--------------+--------------+
        |              |              |
        v              v              v
      Rust           Python          Node/Go/...
    Provider         Provider         Provider
```

The architectural boundary is semantic rather than language-specific:

```text
Harness Core                 Capability Provider
------------                 -------------------
owns state                   executes capabilities
owns ordering                streams results
owns policy                  reports failures
owns persistence             obeys cancellation
owns recovery decisions      declares capabilities
```

## 2. Control-plane ownership

Harness Core MUST be the only component that can authoritatively:

- create, resume, and dispose live Agent actors;
- transition Agent phase;
- open and close turns and steps;
- claim Inbox work;
- append authoritative SessionEvents;
- decide which messages enter model-visible history;
- assemble model requests;
- decide whether a Tool invocation is allowed;
- classify retries and crash recovery;
- bind capability calls to providers;
- decide when execution is blocked.

Capability Providers MUST NOT directly mutate any of the above state.

## 3. Single-writer model

Each live Agent is represented by one actor. The actor is the sole in-process owner of mutable Agent state for that session while it is active.

Different Agents MAY run concurrently. A single Agent MUST NOT have more than one active turn driver.

The intended consistency model is:

```text
Agent Actor single writer
          +
SessionStore expected-sequence check
          =
serialized domain state with storage-level conflict detection
```

The system MUST NOT expose shared mutable Agent state as a general-purpose `Arc<Mutex<...>>` API.

## 4. Event-sourced durable state

The SessionEvent log is the durable source of truth for a session.

Model history MUST be projected from SessionEvents. The implementation MUST NOT treat a separate mutable `messages[]` collection as the authoritative durable conversation.

This supports:

- process restart and resume;
- deterministic inspection;
- fork at an event boundary;
- auditing;
- crash analysis;
- future compaction;
- future telemetry projection.

## 5. Exact model-request snapshots

Normal requests are assembled from projections and registries, but the exact provider-neutral `ModelRequest` MUST be snapshotted to BlobStore before provider dispatch.

The corresponding durable `model/requested` event records the snapshot reference. This separates two concerns:

- **Projection** explains how the request was generated from current durable state.
- **Snapshot** records exactly what was dispatched at that point in time.

This makes request auditing independent of future projector or prompt-registry changes.

## 6. Extension model

v0.1 distinguishes two extension categories.

### 6.1 Control extensions

Control extensions can participate in policy, request interception, prompt contribution, or other ordering-sensitive behavior. In v0.1 they remain in-process with the Rust Core.

Examples:

- prompt-section contributors;
- pre-step policy;
- tool pre/post middleware;
- approval policy;
- request configuration policy.

### 6.2 Capability providers

Capability providers execute work on behalf of Core through Provider Protocol v1. They MAY be implemented in any language.

v0.1 defines only:

- Tool capability;
- LLM capability.

Filesystem, shell, web search, embeddings and similar functions are represented as Tools until a later specification creates a first-class capability family.

## 7. Hierarchical scopes

Core supports a hierarchical registration concept:

```text
Global Scope
    |
Workspace Scope
    |
Agent Scope
    |
Invocation Scope
```

Registrations belong to a scope and are automatically invalidated when the scope is disposed.

A scope MAY own:

- tool registrations;
- prompt sections;
- policy registrations;
- provider bindings;
- child scopes;
- in-flight operations.

Scope disposal MUST cancel owned in-flight operations before releasing owned registrations and resources.

The exact public Rust Scope API is not frozen by v0.1, but the lifecycle semantics above are normative.

## 8. Runtime events versus durable events

The system has two event domains.

### Durable SessionEvents

Used for facts that must survive restart and affect reconstruction or recovery.

Examples:

- user/message;
- tool/call;
- tool/result;
- turn/ended.

### RuntimeEvents

Used for transient observation and UI/runtime behavior.

Examples:

- provider/restarted;
- agent/status;
- tool/progress;
- LLM text delta;
- transport latency.

A fact that is required to reconstruct model-visible history or determine crash recovery MUST be represented durably.

## 9. Capability routing

CapabilityRouter maps a resolved capability to a ready provider instance. ProviderHost/Supervisor owns provider process lifecycle and transport.

ProviderHost MUST NOT decide domain retry semantics. For example, process failure during a Tool call is reported upward; Tool Runtime determines whether the operation can safely retry based on `SideEffectClass` and idempotency support.

## 10. Non-goals

v0.1 does not attempt to make every Core component independently distributed. The initial design is a single control-plane process with out-of-process capability workers.

The architecture favors deterministic state ownership over maximal runtime dynamism.
