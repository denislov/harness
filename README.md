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

# Harness API Batch 06

Batch 06 implements the first deterministic Agent driver slice:

```text
Send
 -> durable inbox/enqueued acknowledgement
 -> wake
 -> turn/started
 -> inbox/claimed
 -> step/started
 -> user/message
 -> ReadyForModel
```

It is based on GitHub repository `denislov/harness`, baseline commit:

```text
228aa80798d0c0c8b26c64ea674073124df7aef9
```

The user's Batch 05 unused-import cleanup is already reflected in the source used to prepare this package.

## Files in this batch

```text
crates/harness-agent/src/
├── actor.rs              replacement
├── driver_tests.rs       new
├── lib.rs                replacement
├── loop_driver.rs        new
└── state.rs              replacement

projector.patch           patch for harness-session/src/projector.rs
runtime.patch             patch for harness-agent/src/runtime.rs

spec/
├── agent-lifecycle.md
├── invariants.md
└── rust-core-layout.md

API-SURFACE.md
SPEC-DELTA.md
apply.sh
```

No Cargo dependency change is required.

## Recommended application

From any directory:

```bash
./apply.sh /path/to/your/harness
```

Or, if the Batch 06 directory is inside the repository root:

```bash
./harness-api-batch-06/apply.sh .
```

The script first runs `git apply --check` for both narrow patches. If either patch does not match your current tree, it exits before modifying files.

## Manual application

If you prefer to apply manually:

```bash
git apply projector.patch
git apply runtime.patch

cp crates/harness-agent/src/actor.rs      <repo>/crates/harness-agent/src/actor.rs
cp crates/harness-agent/src/state.rs      <repo>/crates/harness-agent/src/state.rs
cp crates/harness-agent/src/lib.rs        <repo>/crates/harness-agent/src/lib.rs
cp crates/harness-agent/src/loop_driver.rs <repo>/crates/harness-agent/src/loop_driver.rs
cp crates/harness-agent/src/driver_tests.rs <repo>/crates/harness-agent/src/driver_tests.rs
```

The paths above assume commands are run from the extracted Batch 06 directory; replace `<repo>` with your repository root.

Also replace the three spec files in `spec/`.

## Validation

Run:

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

For a warning-clean build, additionally run:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

if Clippy is installed in your local toolchain.

## Expected behavioral changes

After:

```rust
handle.followup(message).await?;
```

`SendReceipt` still acknowledges the durable `inbox/enqueued` event itself. The actor then advances deterministically before processing the next mailbox item. A subsequent `snapshot()` should therefore observe the input already claimed into an open step and the actor parked at `ReadyForModel`.

For the first message in a new Session, the expected durable sequence is:

```text
1 session/created
2 inbox/enqueued
3 turn/started
4 inbox/claimed
5 step/started
6 user/message
```

A `next-step` `inject` on an idle Agent remains pending because it has `wakeup=false`. When a future waking input starts a turn, that primary `next-turn` input is entered first, followed by queued `next-step` input.

A second `next-turn` input arriving while the current step is parked at `ReadyForModel` remains queued for a future turn.

## Safety correction included in Batch 06

`ResumeDecision::ContinueOpenStep` alone is not sufficient evidence that a new model request should start. The step may already contain an authoritative assistant message and merely be missing its final `step/ended` convergence.

Therefore `SessionProjection` now exposes replay-derived:

```rust
open_step_assistant_message: Option<MessageId>
```

and `ReadyForModel` is returned only when this field is `None`.

Post-assistant step finalization is intentionally deferred to the next batch rather than risking duplicate model execution after restart.

## Not implemented yet

Batch 06 does not execute an LLM. It does not create a live `model/requested` event, run tools, finalize a post-assistant step, or implement active-operation cancellation.

The intended next boundary is an in-process fake/model operation launched from `ReadyForModel` without blocking actor mailbox progress.

# Harness API Batch 07

Batch 07 is the first executable LLM vertical slice built on top of the Batch 06 deterministic Agent driver.

## Scope

This batch adds:

- `harness-storage` with the generic `BlobStore` abstraction;
- `MemoryBlobStore` in `harness-storage-local`;
- provider-neutral `harness-llm` request and normalized stream APIs;
- strict `LlmStreamAssembler` validation;
- `StreamSeq` as a cross-language-safe runtime stream counter;
- `AgentLlmRuntime` binding one model config, one LLM provider, and one BlobStore;
- process-local `ActiveAgentOperation::Model` state;
- request snapshot persistence before `model/requested`;
- an external LLM task that reports completion through the Agent mailbox;
- durable `assistant/message` / `model/failed` convergence;
- no-tool assistant Step/Turn completion;
- tool-call assistant parking for Batch 08;
- an integration Fake LLM test slice;
- removal of the Batch 06 unused `id<T>()` helper that triggered `cargo clippy -D warnings`.

Tool execution, cancellation propagation, provider process protocol, prompt registry, and real provider adapters are deliberately out of scope.

## Baseline

`apply.py` targets the exact GitHub `main` state immediately after Batch 06. It verifies Git blob SHAs for all files it modifies before writing anything significant.

The expected Batch 06 `loop_driver.rs` still contains the unused test-only `id<T>()` helper. The Batch 07 application removes it instead of suppressing the warning.

## Apply

From the extracted Batch 07 directory:

```bash
./apply.sh /path/to/harness
```

The script adds the new crate/files, updates Cargo manifests, patches the Batch 06 runtime/loop driver, and appends the Batch 07 normative spec amendments.

Then run:

```bash
cd /path/to/harness
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

`Cargo.lock` is intentionally not shipped. Cargo should update it from your workspace dependency graph.

## New runtime flow

```text
AgentHandle.followup
    -> durable inbox/enqueued
    -> ack
    -> deterministic Turn/Step entry
    -> ReadyForModel
    -> build ModelRequest
    -> BlobStore.put(exact JSON snapshot)
    -> durable model/requested
    -> ActiveAgentOperation::Model
    -> spawn LLM provider stream task

actor mailbox remains available
    <- followup / steer / snapshot / shutdown

LLM task
    -> validate SequencedStreamEvent stream
    -> one LlmCompletion through actor mailbox

AgentActor
    -> assistant/message OR model/failed
    -> step/ended when no ToolCall remains
    -> deterministic next-step / turn-end convergence
```

## Important semantics

A live `model/requested` event projects as `RecoverInterruptedModelRequest`, because that is the correct durable interpretation after process loss. While the process is healthy, `ActiveAgentOperation::Model` overlays that projection and tells the live actor that the request is genuinely in flight. This prevents the deterministic driver from applying restart recovery to a normal live request.

The external provider future has no SessionStore access. Only the Agent actor translates completion into authoritative SessionEvents.

## Tool-call boundary

Batch 07 understands normalized ToolCall blocks at the LLM stream layer, but does not yet own `ToolDefinition` metadata or the Tool pipeline. Therefore an assistant containing one or more ToolCall blocks is persisted as the authoritative `assistant/message` and the step remains open. Existing Session projection state prevents a second model request. Batch 08 starts from exactly this state.

# Harness API Batch 08

Batch 08 completes the first in-process vertical slice:

```text
user -> LLM -> Tool -> LLM -> final answer
```

It is based on GitHub `denislov/harness` main commit:

```text
3743501c59baa8be891c7f22f3482ab5b07a92c3
```

## Scope

This batch implements `harness-tools` and connects Tool execution to the existing Batch 07 Agent/LLM runtime.

New domain/runtime pieces:

```text
harness-tools
├── ToolDefinition
├── ToolRegistration
├── ToolRegistry
├── ToolArgumentValidator
├── ToolPolicy / PolicyDecision
├── ToolInvocation
└── ToolExecutor

harness-agent
├── AgentToolRuntime
├── ToolDriverPlan
├── ActiveAgentOperation::Tool
├── ReadyForTools
├── external Tool task -> mailbox completion
└── spawn_agent_with_capabilities
```

Session projection gains replay-derived open-step Tool scheduling state and `StepEndReason::ToolContinuation`.

## Deliberate Batch 08 limits

- ToolCalls execute sequentially in assistant-message order.
- `parallelSafe` is modeled but parallel scheduling is deferred.
- `PolicyDecision::Ask` fails closed as a denied pre-dispatch Tool result until an approval surface exists.
- No Provider Protocol / subprocess Tool provider is introduced yet; `ToolExecutor` is the in-process domain seam.
- Tool cancellation and timeout enforcement remain deferred.
- A JSON-schema engine is not selected. Every Tool registration must supply a Core-side `ToolArgumentValidator`.

## Apply

From the extracted Batch 08 directory:

```bash
./apply.sh /path/to/harness
```

The script verifies Git blob SHA values for all modified Batch 07 files before writing anything. If your working tree no longer matches the referenced Batch 07 baseline, it stops rather than applying a fuzzy patch.

## Verify

Run:

```bash
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The reference integration test is:

```text
harness-agent::tool_tests::user_llm_tool_llm_final_answer_vertical_slice
```

It verifies the durable sequence:

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
 tool/dispatched
 tool/result
step/ended(tool-continuation)
step/started
model/requested
assistant/message
step/ended(completed)
turn/ended(completed)
```

It also verifies that the first ModelRequest receives the ToolRegistry catalog and that the second ModelRequest contains the projected ToolResult.

## Files changed

The apply script modifies or creates files under:

```text
crates/harness-tools/
crates/harness-agent/
crates/harness-session/
spec/
```

See `API-SURFACE.md` and `SPEC-DELTA.md` for the stable API and normative semantic changes.

# Batch 08.1

> Maintenance rebuild of Batch 08. Rust/spec payload is unchanged. `apply.py` is corrected to uniquely target the step-start projector reset; see `FIX-NOTE.md`.

# Harness API Batch 08

Batch 08 completes the first in-process vertical slice:

```text
user -> LLM -> Tool -> LLM -> final answer
```

It is based on GitHub `denislov/harness` main commit:

```text
3743501c59baa8be891c7f22f3482ab5b07a92c3
```

## Scope

This batch implements `harness-tools` and connects Tool execution to the existing Batch 07 Agent/LLM runtime.

New domain/runtime pieces:

```text
harness-tools
├── ToolDefinition
├── ToolRegistration
├── ToolRegistry
├── ToolArgumentValidator
├── ToolPolicy / PolicyDecision
├── ToolInvocation
└── ToolExecutor

harness-agent
├── AgentToolRuntime
├── ToolDriverPlan
├── ActiveAgentOperation::Tool
├── ReadyForTools
├── external Tool task -> mailbox completion
└── spawn_agent_with_capabilities
```

Session projection gains replay-derived open-step Tool scheduling state and `StepEndReason::ToolContinuation`.

## Deliberate Batch 08 limits

- ToolCalls execute sequentially in assistant-message order.
- `parallelSafe` is modeled but parallel scheduling is deferred.
- `PolicyDecision::Ask` fails closed as a denied pre-dispatch Tool result until an approval surface exists.
- No Provider Protocol / subprocess Tool provider is introduced yet; `ToolExecutor` is the in-process domain seam.
- Tool cancellation and timeout enforcement remain deferred.
- A JSON-schema engine is not selected. Every Tool registration must supply a Core-side `ToolArgumentValidator`.

## Apply

From the extracted Batch 08 directory:

```bash
./apply.sh /path/to/harness
```

The script verifies Git blob SHA values for all modified Batch 07 files before writing anything. If your working tree no longer matches the referenced Batch 07 baseline, it stops rather than applying a fuzzy patch.

## Verify

Run:

```bash
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The reference integration test is:

```text
harness-agent::tool_tests::user_llm_tool_llm_final_answer_vertical_slice
```

It verifies the durable sequence:

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
 tool/dispatched
 tool/result
step/ended(tool-continuation)
step/started
model/requested
assistant/message
step/ended(completed)
turn/ended(completed)
```

It also verifies that the first ModelRequest receives the ToolRegistry catalog and that the second ModelRequest contains the projected ToolResult.

## Files changed

The apply script modifies or creates files under:

```text
crates/harness-tools/
crates/harness-agent/
crates/harness-session/
spec/
```

See `API-SURFACE.md` and `SPEC-DELTA.md` for the stable API and normative semantic changes.

# Harness Batch 08.2 Hotfix

Apply this only after Batch 08/08.1 has been applied and your workspace matches the synced GitHub Batch 08 baseline.

```bash
./apply.sh /path/to/harness
```

The script guards the exact current Git blob SHA of:

- `crates/harness-agent/src/actor.rs`
- `crates/harness-agent/src/loop_driver.rs`

It stages all text transformations in memory before writing either file.

After applying, run:

```bash
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

This hotfix intentionally contains no Batch 09 work.

# Harness Batch 08.3 Hotfix

This package applies two narrowly-scoped fixes to the already-applied Batch 08 code on GitHub `main` commit `424bba4f78bbcb051e7686e93b0d23e9ff2c2e63`.

## Apply

From this extracted directory:

```bash
./apply.sh /path/to/harness
```

The script verifies Git blob SHAs before modifying either file and stages all text changes in memory before writing anything.

Expected baseline blobs:

```text
crates/harness-session/src/projector.rs
41f4e640ef79ab5ac5d05bf399dd7374b8f4608e

crates/harness-agent/src/tool_driver.rs
be96a5c9fde7e420175f22b6a2ea31776531ed28
```

## Acceptance

```bash
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

See `FIX-NOTE.md` for the rationale.