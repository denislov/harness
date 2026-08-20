# Batch 19 — Durable Execution Composition Epochs

## Status

Batch 19 closes the configuration-drift recovery gap introduced once Runtime composition became configurable. It adds one durable Session event and one immutable Blob snapshot format; it does not make application configuration itself durable and does not move configuration resolution into the Agent state machine.

## 1. Problem

Before Batch 19, `model/requested` snapshotted the exact provider-neutral request, but an unfinished logical ToolCall was durably identified only by its name, arguments, and side-effect class. Tool version, provider binding, schema/validator semantics, provider version, model configuration, policy identity, and retry budget lived only in the newly compiled Runtime profile.

A process could therefore crash after an assistant ToolCall became durable but before `tool/dispatched`, restart under changed configuration, and attempt to continue the old durable work with a different execution composition.

Batch 19 makes that transition explicit and fail-closed.

## 2. Durable event

Batch 19 adds:

```text
composition/activated
```

with payload:

```text
profile
snapshot: BlobRef
```

The referenced Blob is an immutable `ExecutionCompositionSnapshot` using schema version `1` and media type:

```text
application/vnd.harness.execution-composition+json;version=1
```

The Blob SHA-256 digest is the durable semantic identity of the epoch. Blob ids are not used for equality because the `BlobStore` contract does not require content-addressed identifiers.

## 3. Snapshot contents

The v1 snapshot contains:

- profile name;
- model provider id and provider manifest version;
- model id, system prompt, model options, and Core-owned model timeout;
- every enabled Core `ToolDefinition` in model-visible order;
- each Tool provider id and provider manifest version;
- provider keyed-idempotency support for each Tool;
- Core Tool argument-validator composition identity;
- Tool-policy composition identity;
- maximum automatic Tool attempt count.

Credentials and secret values MUST NOT be written to the snapshot.

Provider authors MUST change `providerVersion` when a provider release changes capability semantics that are relevant to safe recovery while retaining the same provider id/capability identity.

## 4. Validator and policy identities

`ToolArgumentValidator` and `ToolPolicy` gain `composition_identity()`.

The default identity is the concrete Rust type name, which preserves source compatibility for in-process implementations. Implementations whose behavior depends on configuration SHOULD override the method with a stable semantic identity containing their own version/configuration identity. Built-in file-configured validation and `AllowAllToolPolicy` use explicit stable identities.

## 5. Activation rule

`composition/activated` is legal only outside an open Turn/Step and while there is no pending model request, approval, Tool work, Tool dispatch, or unresolved recovery block. Pending Inbox input does not prevent activation because no execution composition has consumed it yet.

The latest activation is the active durable execution epoch.

## 6. Runtime open rule

Before spawning an Agent, `HarnessRuntime::open_agent` verifies the latest activated BlobRef and compares the compiled profile snapshot with the Session's latest durable activation. A missing or corrupt active snapshot fails before Agent spawn.

- Same profile and same snapshot SHA-256/size: open normally, including recovery of unfinished work.
- Different composition and `ResumeDecision::Clean`: durably append a new `composition/activated` event, then open normally.
- Different composition while durable work is unfinished: fail with `HarnessRuntimeError::CompositionDrift`; do not start an Agent and do not mutate the Session.
- No prior activation and `ResumeDecision::Clean`: append the first activation. This admits quiescent pre-Batch-19 Sessions.
- No prior activation while durable work is unfinished: fail with `HarnessRuntimeError::LegacyCompositionUnbound`. The Core cannot prove which composition owns the unfinished legacy work.

The activation append is prevalidated through `V1SessionProjector`, uses the observed Session head as the expected sequence, and verifies the `SessionStore` returned exactly the prevalidated committed batch.

## 7. Configuration changes

Batch 19 intentionally permits configuration changes between completed activities. A new resolved profile may become active at a quiescent boundary, creating a new durable epoch. Historical model requests remain independently auditable through their existing request snapshots.

Changing configuration is not itself a Session mutation. A new epoch is written only when a Runtime actually opens that Session with the changed compiled profile.

## 8. Non-goals

Batch 19 does not add:

- dynamic/hot mutation of a live Agent profile;
- automatic recovery using an old provider binary that is no longer configured;
- provider process restart/supervision;
- secret or credential-value fingerprints;
- a general configuration history service;
- Tool parallel scheduling;
- session-log schema migration.

Provider supervision remains the next runtime-robustness layer. It must preserve the rule that availability recovery never decides Tool retry safety.

## 9. Acceptance expectations

Batch 19 is accepted when:

1. opening a clean Session writes exactly one initial `composition/activated` event;
2. reopening under the same resolved composition does not append another activation;
3. changing composition at a quiescent Session appends a new activation and execution may continue;
4. changing composition while a Turn/Step/recovery is incomplete fails before Agent spawn;
5. a pre-Batch-19 Session with unfinished durable activity fails closed as unbound;
6. the snapshot Blob is verifiable and decodes as `ExecutionCompositionSnapshot` v1;
7. snapshot bytes change when model/tool/provider-version/policy/validator/retry semantics represented by the snapshot change;
8. existing Provider Protocol, Python SDK, durable storage, and Agent recovery tests remain green.
