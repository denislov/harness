# Batch 18 — Scope, Prompt, and Capability Configuration

## Status

Batch 18 defines the first hierarchical application-configuration layer above Harness Core.
It is additive to `harness.toml` schema version `1`; no Provider Protocol or SessionEvent schema changes are introduced.

## 1. Boundary

Scope resolution is composition work. It MUST complete before `AgentProfile` is handed to `HarnessRuntime`.
The Agent state machine MUST NOT query global/workspace/session configuration while driving a Turn or Step.

The resolution chain is deterministic and ordered:

```text
global -> workspace -> profile -> session
```

A selected workspace is optional. A configured session scope is optional. A profile is required for execution.

## 2. Profile remains the capability contract

A profile owns the concrete model baseline, ToolDefinitions, validators, and policy selection. Scope overlays MAY alter:

- system-prompt composition;
- model provider/model/options according to precedence;
- Tool visibility for ToolDefinitions already declared by the profile;
- policy selection;
- automatic Tool-attempt limits.

An overlay MUST NOT synthesize a ToolDefinition from a Tool name. Provider manifests remain execution-capability attestations and MUST NOT become the source of Core Tool definitions.

## 3. Configuration model

Batch 18 adds these optional schema-v1 fields:

```toml
[runtime]
default_workspace = "repo"

[global]
system = "global prompt"
system_mode = "append"

[global.model]
timeout_ms = 30000

[global.capabilities]
disable = ["shell.exec"]

[workspaces.repo]
system = "workspace prompt"

[workspaces.repo.capabilities]
enable = ["repo.search"]

[profiles.coding]
policy = "allow-all"

[profiles.coding.model]
provider = "example-python"
model = "agent-model"
system = "profile prompt"

[[profiles.coding.tools]]
name = "repo.search"
provider = "example-python"
version = "1"
description = "Search repository"
side_effect = "read-only"
enabled = true

[sessions."ses_example"]
profile = "coding"
workspace = "repo"
system = "session prompt"

[sessions."ses_example".model]
max_output_tokens = 512

[sessions."ses_example".capabilities]
disable = ["shell.exec"]
```

## 4. Prompt assembly

Every non-empty system fragment is applied in scope order.

`system_mode = "append"` appends a fragment. `system_mode = "replace"` clears all earlier fragments before adding the current fragment.
The profile's existing `model.system` field is an append fragment at the profile layer.

The final system prompt is the remaining fragments joined with exactly two newline characters (`\n\n`).
No prompt fragment is written to SessionEvent merely because it exists in configuration. The exact provider-neutral ModelRequest snapshot remains the durable model-visible record.

## 5. Model resolution

Model fields use last-writer-wins precedence across the scope chain. The profile MUST still declare its baseline `provider` and `model`, preserving a concrete independently runnable profile. Session scope MAY override them.

Optional `timeout_ms` and `max_output_tokens` inherit through the chain. When no timeout is resolved, the existing Harness default of 120000 ms applies.

A resolved provider reference MUST refer to a configured Provider.

## 6. Tool capability visibility

The profile ToolDefinitions define the complete Tool universe for a resolved profile.

Resolution starts each declared Tool as enabled, then applies:

1. global `capabilities.enable/disable`;
2. workspace `capabilities.enable/disable`;
3. explicit per-profile-tool `enabled = true|false` when present;
4. session `capabilities.enable/disable`.

A capability directive naming a Tool not declared by the selected profile is ignored and recorded in `ignoredCapabilityDirectives` in the resolution trace. It never creates a Tool or executes a Provider capability.
A single scope MUST NOT place the same Tool name in both `enable` and `disable`.

Only enabled ToolDefinitions are placed into the final `AgentProfile` and therefore into the model Tool surface and ToolRegistry.

## 7. Policy and automatic attempts

Policy and `max_automatic_tool_attempts` use the same last-writer-wins scope order.
A final Tool policy MUST resolve before the profile is executable. Batch 18 still exposes only the existing `allow-all` file-configured policy.
The default automatic Tool-attempt count remains `2` when no layer supplies one.

## 8. Session scope binding

A `[sessions."<SessionId>"]` entry MAY declare `profile` and/or `workspace`.
If present, those fields bind that configured session scope. A request to resolve the same SessionId with a different bound profile or workspace MUST fail instead of silently changing the meaning of the session-specific overlay.

Session scopes are configuration, not durable Session state. Batch 18 does not implement hot reload or a durable scope-snapshot event. Existing per-model-request snapshots remain authoritative for reconstruction of model-visible requests.

## 9. RuntimePlan API

`RuntimePlan` retains the pre-Batch-18 base profiles and `runtime_builder()` for compatibility.
It additionally exposes:

- workspace/session-scope counts and lookup helpers;
- `default_workspace()`;
- `resolve_scope(ScopeSelection)`;
- `runtime_builder_for_scope(&ResolvedScope, ...)`.

The scoped builder installs exactly the resolved selected profile while retaining the same configured Providers, credentials, durable storage, and lifecycle behavior.

## 10. Resolution trace

`ScopeResolutionTrace` is diagnostic, non-durable data. It includes:

- selected profile/workspace/session;
- applied layer order;
- prompt fragments and composed system prompt;
- final model provider/name/options;
- enabled and disabled Tool names;
- ignored capability directives;
- final policy and automatic Tool-attempt limit.

The trace MUST NOT contain resolved credential values.

## 11. CLI

Batch 18 adds:

```text
harness config resolve [--profile NAME] [--workspace NAME] [--session SESSION_ID] [--json]
harness run SESSION_ID [--profile NAME] [--workspace NAME]
```

Selection defaults are resolved in this order:

```text
explicit CLI value
-> configured session binding
-> runtime default
```

The resolved scope is computed before Providers are started. `config resolve` remains offline and MUST NOT resolve credentials or launch Provider processes.

## 12. Non-goals

Batch 18 does not add:

- dynamic/hot scope reload for a running Agent;
- a durable scope-binding/snapshot SessionEvent;
- remote configuration services;
- JSON Schema execution for Tool arguments;
- additional Tool policies;
- Provider-derived ToolDefinitions.

## 13. Acceptance expectations

The Batch 18 acceptance path MUST prove:

1. global, workspace, profile, and session prompt fragments resolve in deterministic order;
2. `replace` clears earlier prompt fragments;
3. capability visibility obeys the four-layer precedence above;
4. a session binding can choose profile/workspace for CLI `run` without flags;
5. `harness config resolve --json` reports all four layers without starting a Provider;
6. the existing Python Provider vertical slice still executes through the resolved Runtime profile;
7. existing credential redaction and RuntimeEvent JSONL behavior remain unchanged.
