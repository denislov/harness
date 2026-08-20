# Batch 20 — Crash / Fault-Injection Conformance Matrix

## Status

Batch 20 converts the durability and recovery rules established through Batch 19 into deterministic executable conformance tests. It adds no new Agent, SessionEvent, Provider Protocol, configuration, or Runtime behavior.

## Goal

Harness claims that recovery decisions are derived from committed Session state and that external Tool effects are governed by the durable dispatch boundary plus `SideEffectClass`. Batch 20 makes those claims continuously testable at the append boundaries where process death can create ambiguity.

The batch MUST detect regressions that would:

- repeat an already authoritative model or Tool result;
- dispatch a Tool before `tool/dispatched` is durable;
- lose a committed ToolCall after an Agent process restart;
- retry a non-idempotent write whose external outcome is unknown;
- change provider or idempotency-key identity across an automatic retry;
- duplicate approval requests or lose a committed approval decision;
- silently adopt a different Batch 19 execution composition for unfinished work;
- treat events inside one atomic `SessionStore::append` batch as independently crashable states.

## Crash model

The deterministic crash primitive is a `SessionStore` wrapper that performs the real inner `append` first and then, for one configured event occurrence, deliberately withholds the successful acknowledgement by returning a storage error.

The resulting state is:

1. the append batch is committed in the authoritative SessionStore;
2. the actor has not accepted the returned `AppendResult` and therefore has not advanced its local history/projection;
3. the actor is terminated or explicitly aborted;
4. a new actor bootstraps only from the committed Session state.

This is the relevant logical cut for a process death after storage commit and before Core observes commit success. The test primitive is deterministic and does not require production failpoint branches.

## Atomic append boundaries

A crash point exists between `SessionStore::append` transactions. No crash point is modeled between SessionEvents that Core intentionally commits in the same append transaction.

Current relevant batches are:

| Boundary | Atomic committed events |
| --- | --- |
| step entry | `turn/started` when needed, `inbox/claimed`, `step/started`, `user/message` |
| model dispatch | `model/requested` |
| ToolCall assistant | `assistant/message` |
| terminal assistant | `assistant/message`, `step/ended` |
| ToolCall materialization | one or more `tool/call` events |
| approval request | `approval/requested` |
| approval resolution | `approval/resolved` |
| Tool dispatch | `tool/dispatched` |
| Tool terminal result | `tool/result` |
| Tool continuation | `step/ended` |
| normal turn close | `turn/ended` |
| non-idempotent recovery close | `recovery/blocked`, `step/ended`, `turn/ended` |

The matrix MUST describe the complete append batch associated with a tested crash cut. A test MUST NOT manufacture a partial prefix that could never be produced by the production append transaction.

## Workspace conformance crate

Batch 20 adds the unpublished workspace crate `harness-conformance`.

The crate owns only reusable conformance fixtures:

- `AppendFault`;
- `FaultInjectingSessionStore`;
- `ObservedAppend`;
- deterministic `TestEventSource` and `ScriptedLlm` fixtures;
- small helper functions for constructing Tool runtimes and reading/waiting on Session state.

Production crates do not depend on `harness-conformance`.

## Agent post-commit matrix

The primary matrix executes the same `user -> model ToolCall -> Tool -> model final answer` logical turn while injecting acknowledgement loss after each durable append boundary.

Required cuts:

1. step-entry batch, matched by `user/message`;
2. first `model/requested`;
3. first `assistant/message` containing the ToolCall;
4. second `assistant/message`, which is atomically committed with terminal `step/ended`;
5. `tool/call`;
6. first `tool/dispatched`;
7. `tool/result`;
8. first `step/ended`, the ToolContinuation boundary;
9. `turn/ended`.

After restart every case MUST converge to the same logical final history. The fake Tool MUST cross the external invocation boundary exactly once. The fake LLM MUST receive exactly two provider-visible requests. A crash after `tool/dispatched` is the exception at the durable dispatch layer: the event log contains attempts 1 and 2, but attempt 1 never crossed into the fake Tool executor because commit acknowledgement was lost before the provider task could be spawned.

For the `model/requested` cut, startup MUST durably terminalize the interrupted request as `model/failed` before a new request attempt is started. The provider never saw the interrupted request because the model operation is created only after the request event append returns successfully.

## Approval matrix

Two independent post-commit cuts are required:

- acknowledgement loss after `approval/requested`;
- acknowledgement loss after `approval/resolved` with an Allow decision.

A committed approval request MUST remain the unique pending approval after restart. A committed Allow decision MUST survive acknowledgement loss and permit dispatch without creating a second approval request. The Tool MUST execute exactly once.

## Provider fault matrix

Provider faults are injected through the existing `ToolExecutor` contract. `Err(PortableError)` represents the absence of an authoritative Tool outcome after a durable dispatch.

### Read-only

The first invocation returns `ProviderUnavailable`; the second succeeds. Core MUST append dispatch attempt 2, preserve provider identity and the original idempotency key, and complete the logical ToolCall.

### Idempotent write

The first provider invocation applies the side effect and then returns `ProviderUnavailable`. The fake provider records the keyed result internally. Core MUST retry with the same idempotency key. The provider MUST deduplicate the retry so two invocations produce one external side effect.

### Non-idempotent write

The provider applies the side effect and then returns `ProviderUnavailable`. Core MUST NOT redispatch. Startup/recovery convergence MUST commit a recovery block and close the step/turn as blocked. The unresolved recovery gate remains authoritative until an explicit resolution path produces an authoritative result.

## Durable local reopen acceptance

At least one post-commit crash case MUST run against `DurableLocalStorage`, not only the in-memory reference store.

Batch 20 uses acknowledgement loss after `tool/dispatched` and then drops/reopens the SQLite SessionStore and filesystem BlobStore. Reopen MUST preserve:

- the original durable dispatch attempt;
- the request snapshot BlobRefs and their integrity;
- the retry idempotency key;
- the projected pending ToolCall required for safe attempt 2.

## Batch 19 composition interaction

The existing Batch 19 `composition_epoch` integration test remains part of the Batch 20 matrix. The critical case is an undispatched durable ToolCall followed by execution-composition drift before Runtime reopen. `HarnessRuntime::open_agent` MUST fail closed with `CompositionDrift`; a new composition activation MUST NOT be appended over unfinished work.

Batch 20 does not move composition reconciliation into the Agent or Provider layers.

## Machine-readable matrix

`conformance/crash-matrix-v1.json` is the coverage manifest for this batch. It records each case, crash/fault cut, atomic append batch, expected recovery property, and owning test command.

`conformance/validate_crash_matrix.py` MUST reject duplicate case IDs, missing required case kinds, missing primary post-commit cuts, malformed atomic-batch declarations, and test entries that do not identify a Cargo test command.

The JSON manifest is descriptive conformance metadata. Durable recovery semantics remain defined by the Rust domain contracts and this specification.

## Non-goals

Batch 20 does not add:

- Provider process restart/supervision;
- Provider generation indirection;
- new retry policy owned by ProviderHost;
- Session schema evolution;
- hot reload;
- parallel Tool scheduling;
- a production failpoint API;
- OS-level SIGKILL orchestration for every boundary.

Provider process supervision remains Batch 21. The Batch 20 Provider fault fixtures represent the transport/outcome ambiguity that the future supervisor must preserve rather than reinterpret.

## Acceptance

The batch is accepted when all commands succeed:

```bash
python3 conformance/validate_crash_matrix.py
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p harness-conformance --test crash_matrix -- --nocapture
cargo test -p harness-conformance --test approval_crash_matrix -- --nocapture
cargo test -p harness-conformance --test provider_fault_matrix -- --nocapture
cargo test -p harness-conformance --test durable_local_crash -- --nocapture
cargo test -p harness-runtime --test composition_epoch -- --nocapture
python3 conformance/provider_protocol_v1_smoke.py
python3 conformance/run_python_sdk_v1.py
```

The first Cargo command may update `Cargo.lock` to add the new workspace-only `harness-conformance` package. No new third-party dependency is introduced.
