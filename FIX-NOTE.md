# Batch 08.1 Apply Fix

Batch 08.1 contains the same Batch 08 Rust/spec payload with a corrected `apply.py`.

## Root cause

The Batch 08 projector patch searched for this reset fragment:

```rust
self.current_step_assistant_seen = false;
self.projection.open_step_assistant_message = None;
self.current_step_announced_calls.clear();
```

The Batch 07 `V1SessionProjector` contains that fragment twice: once while opening a step and once while ending a step. The original apply script incorrectly required exactly one match.

Changing the operation to replace both matches would also be wrong because the later Batch 08 patch intentionally performs a different, larger rewrite of the step-end block.

## Fix

The first replacement is now anchored by:

```rust
self.projection.lifecycle.last_started_step = Some(position);
```

so it uniquely targets the step-start reset. The step-end reset remains owned by the later dedicated `persist projected step-end reason` replacement.

No Rust API, durable schema, or spec semantics changed between Batch 08 and Batch 08.1.

# Batch 08.2 Hotfix

This hotfix fixes the Batch 08 compile/clippy integration failures reported after Batch 08.1 was applied successfully.

Baseline: GitHub `denislov/harness` commit `f780f46978e284da4031a6fd565f26f322524d02`.

## Fixes

### 1. Remove unused `ToolCompletion` import from `actor.rs`

`ToolCompletion` is used by `actor/tool_support.rs`, which imports it directly. The parent `actor.rs` import is unused and fails `clippy -D warnings`.

### 2. Pass `tool_runtime.as_ref()` in the command-driven `advance_runtime` call

`AgentActor::advance_runtime` now takes five arguments after Batch 08:

```rust
advance_runtime(
    store,
    event_source,
    llm_runtime,
    tool_runtime,
    self_tx,
)
```

The LLM-completion and Tool-completion branches were already updated. The successful command branch was missed.

### 3. Import `AgentDriverBoundary` and `StepEndReason` in `loop_driver.rs` tests

The production module imports both names, but the nested `#[cfg(test)] mod tests` has its own lexical scope. Its new Batch 08 tests refer to these names directly, so they must be imported inside the test module.

## Scope

No runtime semantics, durable schema, Tool policy, or Spec behavior changes are made. This is a compile/clippy-only hotfix.

# Batch 08.3 Hotfix

Baseline: GitHub `main` commit `424bba4f78bbcb051e7686e93b0d23e9ff2c2e63`.

This hotfix addresses the two remaining Batch 08 acceptance failures. It does not change the durable schema, public API, provider protocol, or Tool execution semantics.

## 1. Projector test fixture

`projects_inbox_lifecycle_and_model_visible_tool_result` was still using the pre-Batch-08 terminal shape:

```text
tool/result
step/ended(completed)
turn/ended(completed)
```

Batch 08 deliberately requires every ToolCall-producing step with durable terminal results to end as:

```text
tool/result
step/ended(tool-continuation)
```

The Turn remains open because the next step must let the model observe the ToolResult. The fixture now stops at that legitimate intermediate durable boundary and asserts:

- `open_turn == Some(turn 1)`;
- `open_step == None`;
- `last_ended_turn == None`;
- `last_ended_step_reason == ToolContinuation`.

No production projector rule is weakened.

## 2. Clippy large enum variant

`ToolDriverPlan::Dispatch` owned a full `ToolRegistration`, making that enum variant much larger than the others. The registration is now stored as `Box<ToolRegistration>`.

This is crate-private planner state. Ownership and execution behavior do not change; method calls in `tool_support.rs` continue to work through Rust deref coercion.

## Validation

After applying, run:

```bash
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
