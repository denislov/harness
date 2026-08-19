# Harness API Batch 01

Scope:

- `harness-types`: durable/cross-crate value objects.
- `harness-session`: SessionEvent envelope, payloads, validation, SessionStore contract, projector contract.

Important design adjustment from Spec v0.1:

`InboxTarget`, `SideEffectClass`, `CancelCause`, `ToolOutcome`, and `TokenUsage` live in
`harness-types`, because they are persisted inside SessionEvents and therefore cross the
`harness-session` / `harness-tools` / `harness-llm` boundaries. Execution behavior remains
owned by the later tool/LLM crates.

## Apply

1. Merge `Cargo.workspace-snippet.toml` into the root `Cargo.toml`.
2. Replace the two crate Cargo.toml files and source trees with the files in this bundle.
3. Run:

```bash
cargo fmt --all -- --check
cargo check -p harness-types -p harness-session
cargo test -p harness-types -p harness-session
```

## Deliberately deferred

A concrete `SessionProjector` implementation is not included yet. The current spec does not
fully freeze how rich `ToolOutcome` becomes the exact model-visible `tool-result` Message.
The trait and projection output are frozen enough to let the next batch resolve that rule
without breaking storage or event APIs.

# Harness API Batch 02

Batch 02 implements the first concrete `SessionStore` backend: `MemorySessionStore` in `harness-storage-local`.

## Scope

This batch intentionally changes only:

```text
crates/harness-storage-local/
```

It relies on the `harness-types` and `harness-session` APIs from Batch 01 and does not modify them.

## Apply

Copy the crate contents over your existing empty crate:

```bash
cp -R crates/harness-storage-local/* <your-repo>/crates/harness-storage-local/
```

No root workspace dependency is required for the production crate. The crate has a direct dev-dependency on `futures = "0.3"` solely so its async trait implementation can be exercised by unit tests without selecting Tokio as a workspace runtime.

## Verify

```bash
cargo fmt --all
cargo check -p harness-storage-local
cargo test -p harness-storage-local
```

A useful additional workspace check is:

```bash
cargo check --workspace
cargo test --workspace
```

## What is frozen by this batch

- `MemorySessionStore` is the deterministic reference backend for the `SessionStore` contract.
- `create` commits `session/created` at `EventSeq::FIRST`.
- `append` is conditional on `expected_seq` and atomically exposes an entire batch.
- `append([])` is a checked no-op: the expected head is still validated.
- `read` is inclusive (`seq >= from_seq`), ascending, and limited.
- `fork` logically copies the source prefix through an existing committed boundary.
- Mutating operations are linearizable in the reference backend through one write lock.
- Corruption is surfaced as `SessionStoreError::Corrupt`; the backend never silently repairs malformed history.

## Deliberately deferred

- durable SQLite storage;
- filesystem `BlobStore`;
- lineage metadata / `session/forked` event;
- compaction and retention;
- cross-process storage;
- Agent Actor and Agent Loop.

# Harness Rust Core API — Batch 03

Batch 03 freezes the first concrete durable-state projector: `V1SessionProjector`.

## Scope

Only `crates/harness-session` changes in this batch.

The batch adds:

- `V1SessionProjector` and projection version `1`;
- strict SessionEvent stream validation;
- FIFO Inbox projection;
- lifecycle projection for open/last turn and step boundaries;
- pending model request projection;
- pending ToolCall projection;
- deterministic model-visible ToolResult rendering;
- recovery gate projection;
- late reconciled `tool/result` support after a blocked turn has already closed.

No Agent Actor, ProviderHost, LLM implementation, SQLite storage, or runtime task model is introduced yet.

## Apply

Replace the current `crates/harness-session/src/projector.rs` and `lib.rs` with this batch. `event.rs`, `store.rs`, and the crate `Cargo.toml` are included for completeness and are unchanged from Batch 01.

Then run:

```bash
cargo fmt --all
cargo check -p harness-session
cargo test -p harness-session
cargo check --workspace
cargo test --workspace
```

## Expected dependency impact

None. Batch 03 uses only dependencies already present in `harness-session`.

## Why this batch precedes Agent Actor

The Agent Actor should be an executor over durable projections, not a second source of truth. After this batch, resume can determine:

- what Inbox work is still pending;
- whether a turn or step was interrupted;
- which model request is still pending;
- which ToolCalls have no authoritative result;
- whether ExecutionGate is blocked;
- the exact provider-neutral message history to use for the next model request.

The next batch can therefore build recovery analysis and the Agent Actor on top of one authoritative projection model.

# Harness API Batch 04

Batch 04 introduces the first recovery-aware Agent bootstrap layer and closes a durable crash-recovery gap in Tool execution.

## Scope

Changed crates:

- `harness-types`
- `harness-session`
- `harness-agent`

Updated normative specs are included under `spec/`.

## Why `tool/dispatched` was added

`tool/call` records the model's logical ToolCall. It cannot safely mean both "logical call exists" and "provider execution may have begun".

Batch 04 therefore adds a second durable boundary:

```text
tool/call
    |
    | policy / validation / approval
    v
tool/dispatched   <-- commit before crossing provider boundary
    |
    v
provider execution
    |
    v
tool/result
```

Recovery now has an unambiguous rule:

- `tool/call` without `tool/dispatched`: no durable evidence of external dispatch; restart before dispatch.
- `tool/dispatched` without `tool/result`:
  - read-only: retry candidate;
  - idempotent-write: retry candidate only after provider idempotency guarantee is verified;
  - non-idempotent-write: persist `recovery/blocked`, never auto-retry.

## Agent bootstrap

`AgentBootstrapper` loads a point-in-time Session head, pages events through that head, projects them with `SessionProjector`, then runs `RecoveryAnalyzer`.

The result is `AgentBootstrap { head, projection, resume }`.

`AgentState::from_bootstrap` creates process-local Agent state. A restarted Agent begins process-locally `Idle`; unfinished durable work is represented by `ResumeDecision` rather than pretending a live driver already exists.

A new turn is legal only when:

```text
AgentPhase == Idle
ExecutionGate == Open
ResumeDecision == Clean
```

## Apply

Copy the included files over the corresponding project paths.

Important changed files:

```text
crates/harness-types/src/ids.rs
crates/harness-types/src/lib.rs

crates/harness-session/src/event.rs
crates/harness-session/src/projector.rs
crates/harness-session/src/lib.rs

crates/harness-agent/Cargo.toml
crates/harness-agent/src/lib.rs
crates/harness-agent/src/command.rs
crates/harness-agent/src/recovery.rs
crates/harness-agent/src/bootstrap.rs
crates/harness-agent/src/state.rs
crates/harness-agent/src/actor.rs
```

The files under `spec/` replace the same spec files in the project.

## Verify

```bash
cargo fmt --all
cargo check -p harness-types -p harness-session -p harness-agent
cargo test -p harness-types -p harness-session -p harness-agent
cargo check --workspace
cargo test --workspace
```

Batch 04 also removes the Batch 03 test warning caused by unused `EventId`, `InvocationId`, and `RequestId` imports in `projector.rs`.

## Deferred to Batch 05

Batch 04 deliberately does not choose an async channel/executor for the live actor. `AgentActor` is the process-local owner skeleton only.

Batch 05 should introduce:

- concrete async actor task and `AgentHandle`;
- command acknowledgement semantics;
- durable `Send` implementation (`inbox/enqueued` before acknowledgement);
- driver wake ownership;
- resume-plan convergence before normal turn execution.

# Harness API Batch 05

Batch 05 turns the Batch 04 Agent recovery skeleton into the first live Tokio actor runtime.
It still does **not** implement the Turn/Step/LLM/Tool driver.

## Scope

Changed files:

```text
Cargo.workspace-snippet.toml
crates/harness-agent/
  Cargo.toml
  src/
    actor.rs
    bootstrap.rs
    command.rs
    error.rs
    event_source.rs
    handle.rs
    lib.rs
    recovery.rs        # unchanged from Batch 04, included for drop-in replacement
    runtime.rs
    state.rs
crates/harness-session/src/projector.rs
crates/harness-storage-local/src/memory_session.rs
spec/
  agent-lifecycle.md
  invariants.md
  rust-core-layout.md
```

`harness-session/projector.rs` and the reference `MemorySessionStore` both enforce the
new EventId uniqueness invariant: duplicate `SessionEvent.eventId` values within one Session
are rejected before a new append becomes visible.

## Apply

1. Merge the Tokio workspace dependency:

```toml
[workspace.dependencies]
tokio = { version = "1", features = ["sync", "rt"] }
```

2. Replace the included `harness-agent` crate files.
3. Replace `crates/harness-session/src/projector.rs`.
4. Replace `crates/harness-storage-local/src/memory_session.rs`.
5. Replace the three included Spec files.

## Verify

```bash
cargo fmt --all

cargo check \
  -p harness-session \
  -p harness-storage-local \
  -p harness-agent

cargo test \
  -p harness-session \
  -p harness-storage-local \
  -p harness-agent

cargo check --workspace
cargo test --workspace
```

## What Batch 05 freezes

- Tokio is the Rust reference execution runtime for the live Agent actor layer.
- `AgentActor` is the singular mutable owner and is no longer cloneable.
- `AgentHandle` is cloneable and communicates through a bounded Tokio `mpsc` mailbox.
- each state-changing command receives a Tokio `oneshot` acknowledgement;
- `SendReceipt` is emitted only after durable `inbox/enqueued` commit and local projection update;
- `wakeup=true` sets a coalescing process-local wake latch only after durable commit;
- the wake latch is reconstructed from pending durable Inbox items on startup;
- `SessionStore::Conflict` is terminal for the current live Agent instance;
- the actor validates the proposed projected history before appending it;
- after append, the actor verifies that the Store returned exactly the prevalidated committed batch;
- bootstrap now retains the exact event prefix used to derive the projection;
- startup automatically durably fails interrupted model requests;
- startup automatically persists an unknown non-idempotent Tool recovery gate and closes its step/turn as blocked;
- duplicate EventIds in one Session are invalid.

## Deliberately deferred

- consuming `wake_requested` into a real driver run;
- turn creation and Inbox claiming;
- step creation and user/message entry;
- LLM request execution;
- Tool execution/retry;
- cancellation tokens and `Cancel` convergence;
- production EventId generation policy;
- Agent Registry / application composition in `harness-runtime`.

`AgentEventSource` is intentionally injected. Batch 05 tests use deterministic sources;
production UUID/ULID policy should be supplied later by `harness-runtime` rather than hard-coded
into the Agent domain crate.
