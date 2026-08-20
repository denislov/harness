# Batch 14 — Harness Runtime Composition Root

Status: normative supplement to Spec v0.1.

## 1. Purpose

Batch 14 introduces `harness-runtime` as the process-level composition root. It owns the lifecycle and binding of providers, profiles, and live Agent actors. It does not own Agent Turn/Step decisions and does not create a second durable state machine above SessionEvent.

## 2. Runtime ownership

One `HarnessRuntime` owns:

- one immutable `ProviderRegistry`;
- one immutable `LlmRegistry` derived from provider manifests;
- one immutable `ProfileRegistry` compiled against those capabilities;
- one dynamic process-local `AgentRegistry`;
- one injected `SessionStore`;
- one injected `BlobStore`;
- one injected `AgentEventSource`;
- one injected `RuntimeIdSource`.

The Runtime is a composition/lifecycle layer. Durable Session truth remains exclusively in the Session event log.

## 3. Static composition in Batch 14

Provider processes and Agent profiles are registered on `HarnessRuntimeBuilder` and frozen when `build()` succeeds. Dynamic provider/profile mutation is out of scope for Batch 14.

Build MUST preflight duplicate configured provider IDs and duplicate/empty profile names before spawning provider processes.

## 4. Provider identity binding

Every provider process configuration carries an expected `ProviderId`. After `provider.initialize`, the manifest `providerId` MUST parse as a Harness `ProviderId` and MUST equal the configured expected ID.

A mismatch is a build failure. The just-started provider and all previously-started providers MUST be shut down best-effort before build returns failure.

## 5. Build rollback

If provider startup, LLM adapter creation, profile compilation, Tool binding, or runtime construction fails after any provider has started, `HarnessRuntimeBuilder::build()` MUST attempt to shut down all started providers before returning the original build error.

No partially-built Runtime may be published.

## 6. LLM registry

`LlmRegistry` is derived from provider manifests. A profile model binding is valid only when:

1. the provider exists in `ProviderRegistry`; and
2. the provider manifest declares the selected model.

`AgentLlmRuntime` remains the Agent-facing execution seam. `LlmRegistry` only resolves composition.

## 7. Tool binding authority

`ToolDefinition` remains Core-authoritative. A Runtime Tool binding combines:

- Core `ToolDefinition`;
- provider ID;
- Core-side `ToolArgumentValidator`.

The provider manifest is execution-capability attestation, not the source of model-visible Tool schemas. Runtime compilation MUST call the ProviderHost Tool adapter compatibility check before constructing `ToolRegistration`.

## 8. Agent profiles

An `AgentProfile` contains one model binding, zero or more Runtime Tool bindings, one Tool policy, automatic retry limits, and Agent actor configuration.

Profiles are compiled once when the Runtime is built. Opening an Agent clones the compiled provider-neutral `AgentLlmRuntime` and `AgentToolRuntime`; it does not rebuild capability bindings per turn.

## 9. Agent registry and single-driver ownership

`AgentRegistry` enforces at most one live or transitioning Agent for a given `SessionId` inside one Runtime.

`open_agent(session, profile)` MUST reserve the Session before asynchronous spawn begins. A concurrent or repeated open for the same Session MUST fail with `AgentAlreadyActive` rather than creating a second driver.

The reservation MUST be removed if Agent spawn fails.

`close_agent` is a process-local driver lifecycle operation and uses Agent shutdown semantics. It is not an implicit semantic `Cancel`. If a caller needs a durable cancellation outcome before closing one Agent while the Provider remains running, it MUST call `AgentHandle::cancel(...)` explicitly first. Any externally-dispatched operation abandoned by shutdown remains governed by the existing durable recovery rules.

## 10. Runtime lifecycle gate

`HarnessRuntime` has process-local states:

- `Running`;
- `ShuttingDown`;
- `Stopped`.

Open/create/close lifecycle operations require `Running`. An operation that has already entered the Runtime lifecycle gate is allowed to finish before shutdown transitions the Runtime to `ShuttingDown`.

No Runtime lifecycle state is a SessionEvent.

## 11. Shutdown order

`HarnessRuntime::shutdown()` MUST:

1. stop accepting new lifecycle operations;
2. shut down all live Agents and join their actor tasks;
3. shut down provider processes in reverse startup order;
4. mark the Runtime `Stopped`.

Provider shutdown MUST NOT precede Agent shutdown during normal Runtime termination.

Shutdown is idempotent after the Runtime reaches `Stopped`. Failures are collected so later shutdown phases still execute.

## 12. Storage and identities

Batch 14 does not introduce durable local storage. `HarnessRuntimeBuilder::in_memory(...)` is a development/test convenience using `MemorySessionStore` and `MemoryBlobStore`.

Production composition MUST inject durable stores when required.

`RuntimeIdSource` generates `SessionId` and `AgentInstanceId`. `AgentEventSource` continues to generate durable `EventId` and UTC time. Production implementations MUST provide collision-resistant identities across process restarts.

## 13. Acceptance boundary

The Batch 14 acceptance test MUST construct the existing out-of-process Python LLM/Tool vertical slice through `HarnessRuntimeBuilder`, create a Session through `HarnessRuntime`, open the Agent by profile name, reject a duplicate open, reach a final answer, and verify Runtime shutdown leaves no live Agents and stops the provider.

## 14. Deferred work

Batch 14 deliberately defers:

- SQLite SessionStore;
- filesystem BlobStore;
- dynamic provider/profile mutation;
- provider crash restart/backoff supervision;
- TOML application configuration;
- CLI/server shell;
- CredentialResolver;
- RuntimeEventBus and telemetry;
- Scope/Prompt registries;
- cross-process Session leases/driver coordination.

These are later skeleton batches and do not change the Batch 14 ownership boundary.
