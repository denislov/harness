# Batch 21 — Provider Supervisor and Generation Slots

## Status

Normative Batch 21 contract. This batch adds process availability recovery only. Durable Agent recovery, Tool retry safety, SessionEvent semantics, and execution-composition epochs remain authoritative in their existing layers.

## Problem

Before Batch 21, `ProviderRegistry`, `LlmRegistry`, and compiled Tool registrations retained concrete `ProviderHost` values created during Runtime build. `ProviderHost` correctly detected subprocess EOF, transport loss, and protocol faults by entering `Unhealthy`, but no Runtime component replaced that process. Even if a new process were started, already compiled LLM/Tool adapters would still point at the dead Host.

Batch 21 introduces one stable indirection per configured provider: `ProviderSlot`. Runtime capability adapters bind the slot once and resolve its current process generation at operation time.

## Provider generations

1. A successfully initialized provider at Runtime build is generation 1.
2. `ProviderSlot` owns the stable provider identity and a replaceable current generation.
3. A generation contains a `ProviderHost` and the manifest returned by that process.
4. Slot status is one of `Ready`, `Unavailable`, `Quarantined`, or `Stopped`, each carrying the last generation number.
5. A compatible successful restart increments the generation by exactly one.
6. Existing `AgentProfile`, `AgentLlmRuntime`, and `ToolRegistry` instances are not rebuilt after a restart.
7. Provider generation numbers are process-local Runtime observability. They are not SessionEvents and reset to generation 1 when a new HarnessRuntime process is built.

## Capability routing

`ProviderSlotLlmAdapter` and `ProviderSlotToolAdapter` are process-generation neutral.

- LLM start waits for a Ready slot, snapshots that generation, and routes the complete stream through that Host.
- Tool invocation waits for a Ready slot, snapshots that generation, and routes exactly one invocation attempt through that Host.
- An in-flight operation never migrates from one generation to another.
- A new operation may use a later compatible generation.
- Best-effort cancellation is routed to the exact Host generation that owns an already-started request/invocation. An operation still waiting for a Ready slot has no external target yet, so cancellation does not wait for provider recovery.
- Provider-unavailable operation results mark the current generation unavailable. Protocol faults are also observed by the supervisor through `ProviderHost::state()`.

The Agent remains the owner of model-attempt and Tool-attempt semantics. Slot waiting does not create a durable retry attempt by itself.

## Supervisor

Every configured provider has one supervisor task. The supervisor:

1. polls the current Ready Host health;
2. observes an already-Unavailable slot immediately;
3. publishes `provider/unhealthy` for the failed generation;
4. re-resolves credentials for each restart attempt;
5. starts a new Provider process with the original `ProviderProcessSpec`;
6. uses capped exponential restart backoff;
7. validates identity and manifest compatibility before slot replacement;
8. publishes `provider/restarting`, `provider/restart-failed`, `provider/restarted`, or `provider/quarantined` RuntimeEvents;
9. stops retrying after quarantine or Runtime shutdown.

For RuntimeEvents, `provider/unhealthy`, `provider/restarting`, `provider/restart-failed`, and `provider/quarantined` carry the last installed failed generation. `provider/restarted` carries the newly installed generation. Multiple restart attempts therefore share the same failed-generation number and are distinguished by `attempt`.

Default policy:

- health poll interval: 100 ms;
- initial restart backoff: 100 ms;
- maximum restart backoff: 5 s.

`ProviderSupervisorConfig::new(...)` rejects zero durations and a maximum backoff smaller than the initial backoff.

## Manifest compatibility gate

A restart may replace the slot only when the candidate manifest is semantically equal to the Runtime-build baseline manifest. Equality covers:

- provider ID;
- provider version;
- Provider Protocol version;
- Tool name and Tool version;
- Tool `parallelSafe`;
- Tool side-effect class;
- Tool keyed-idempotency support;
- the set of declared LLM model names.

Capability ordering and LLM model ordering are not semantic and are ignored.

An invalid/mismatched provider identity, a structurally invalid restart manifest, or semantic manifest drift quarantines the failed generation. Invalid manifests include unsupported Provider Protocol versions rejected during initialize. The candidate Host is shut down and never becomes current.

This rule is deliberately stricter than ordinary process restart. Provider supervision is availability recovery, not hot deployment. A changed provider version or capability contract requires a new Runtime composition and, where applicable, a new durable execution-composition epoch.

## Interaction with Batch 19

`ProviderRegistry::manifest()` continues to expose the immutable Runtime-build baseline manifest. Execution-composition snapshots therefore remain stable across compatible process restarts. Supervisor replacement never mutates the active Session composition.

## Interaction with Batch 20

Supervisor recovery does not weaken crash/fault conformance:

- a prior durable `tool/dispatched` remains the retry boundary;
- read-only calls may be retried by Agent policy after provider failure;
- idempotent writes still require the persisted idempotency key and provider guarantee;
- non-idempotent writes with ambiguous dispatched outcomes still fail closed;
- the supervisor does not issue Tool or LLM retries itself.
- `tool/dispatched` is still committed before `ToolExecutor::invoke`; waiting for a Ready slot is part of that already-durable attempt. If availability is discovered only after dispatch, recovery remains conservatively ambiguous for non-idempotent effects. Process-local knowledge that the adapter was waiting does not weaken the durable boundary.

## Shutdown

Runtime shutdown signals all supervisor tasks first, waits for them to exit, then shuts down the current/last Host for each provider in reverse initial startup order. Slots finally enter `Stopped`.

## Acceptance

Batch 21 is accepted when:

1. an existing compiled Agent profile completes a Tool -> LLM turn after generation 1 crashes and a compatible generation 2 is started;
2. the durable log records the Agent-owned Tool redispatch attempts and no supervisor-owned Tool attempt;
3. runtime events contain unhealthy/restarting/restarted transitions with generation numbers;
4. a restarted process whose provider version/capability semantics drift is quarantined and generation remains unchanged;
5. the complete Batch 20 conformance suite still passes;
6. workspace fmt/check/test/clippy gates pass.

## Non-goals

Batch 21 does not add dynamic provider membership, live config reload, remote supervisors, operator-driven quarantine release, health RPCs beyond Host state observation, SessionEvent changes, schema migration, Tool parallelism, or LLM request replay by the supervisor.
