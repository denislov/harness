# Batch 16 — Configuration and CLI Application Shell

## Status

Normative Batch 16 supplement for Harness v0.1.

Batch 16 adds an application composition layer and a foreground CLI. It does not change SessionEvent schema, Agent state-machine semantics, Provider Protocol 1.0, Tool recovery semantics, or durable storage semantics.

## 1. Architectural placement

```text
harness.toml
    ↓
harness-config
    ↓
RuntimePlan
    ↓
HarnessRuntimeBuilder
    ↓
HarnessRuntime
    ↓
Agent / ProviderHost / durable local storage

operator terminal
    ↓
harness-cli
```

Configuration and CLI state are process-level application concerns. They MUST NOT become a second source of durable Agent truth.

## 2. Configuration file

The default file name is `harness.toml`. The CLI MAY select another file with `--config`. Every file MUST declare `schema_version = 1`; other schema versions are rejected before composition.

A configuration consists of:

- one top-level `schema_version = 1`;
- one `[runtime]` table;
- zero or more `[[providers]]` entries;
- zero or more `[profiles.<name>]` entries.

### 2.1 Runtime

```toml
schema_version = 1

[runtime]
name = "harness-cli"
data_dir = ".harness"
default_profile = "default"
```

`name` and `data_dir` MUST be non-empty. A configured `default_profile` MUST name an existing profile.

A relative `data_dir` is resolved relative to the canonical directory containing the selected configuration file.

### 2.2 Provider process

```toml
[[providers]]
id = "example-python"
program = "python3"
args = ["providers/example-python/provider.py"]
cwd = "."
request_timeout_ms = 5000
shutdown_timeout_ms = 2000
env = { EXAMPLE_MODE = "reference" }
```

Provider ids MUST be unique and representable as Harness `ProviderId` values.

A path-like relative `program` is resolved against the configuration directory. A bare program name remains suitable for operating-system PATH lookup.

If `cwd` is omitted, the Provider process working directory is the configuration directory. A relative explicit `cwd` is also resolved against the configuration directory.

`env` contains literal process environment entries. Batch 16 preserves the existing `ProviderHost` behavior in which subprocesses inherit the parent process environment. The `env` table adds or overrides individual values. Batch 16 does not define environment isolation, a credential object, or a secret store. Config authors SHOULD NOT commit secret literals; controlled secret resolution/injection is deferred to `CredentialResolver`.

### 2.3 Agent profile

```toml
[profiles.default]
policy = "allow-all"
max_automatic_tool_attempts = 2

[profiles.default.model]
provider = "example-python"
model = "agent-model"
system = "Use the echo tool."
timeout_ms = 5000
max_output_tokens = 256
```

The model provider MUST reference a configured Provider. Model name and timeout MUST be non-empty/non-zero. `max_output_tokens`, when specified, MUST be greater than zero.

Batch 16 supports `policy = "allow-all"` only, and every profile MUST select it explicitly. Omission is a configuration error. This limitation does not remove the durable approval machinery introduced earlier.

### 2.4 Tool binding

```toml
[[profiles.default.tools]]
name = "echo"
provider = "example-python"
version = "1"
description = "Echo an object"
parallel_safe = true
side_effect = "read-only"
timeout_ms = 5000
input_schema = { type = "object" }
```

The Tool provider MUST reference a configured Provider. The generated Core `ToolDefinition` remains authoritative for name/version/description/schema/parallelism/side-effect/timeout semantics. Provider manifest compatibility is still verified dynamically by `HarnessRuntimeBuilder::build()`.

`input_schema` and optional `output_schema` may be a TOML table or a top-level TOML string containing a complete JSON schema object. JSON-text form permits JSON values such as `null` that TOML cannot represent natively. TOML datetime values are not valid schema values in Batch 16. The resulting schemas MUST satisfy existing `ToolDefinition` validation.

Batch 16 still does not choose a JSON Schema execution engine. CLI-composed registrations use a Core-side validator that requires the Tool arguments to be one JSON object. The declared schema remains model-visible metadata.

## 3. Static compilation

`LoadedHarnessConfig::compile()` MUST perform static checks without starting Providers:

- runtime field validity;
- ProviderId validity and uniqueness;
- timeout/line/history limits;
- Provider process limits and literal environment-key validity;
- profile Provider references;
- model limits;
- ToolDefinition validity;
- default-profile validity.

It returns an immutable `RuntimePlan` whose paths are resolved relative to the config file.

`config check` MUST stop at this boundary. It MUST NOT initialize Provider processes merely to validate a file.

## 4. Runtime composition

`RuntimePlan::runtime_builder(event_source, id_source)` wires the plan into Batch 15 durable local storage and the existing Batch 14 composition root.

Dynamic checks remain authoritative at Runtime build:

- Provider initialization and identity;
- manifest model declarations;
- Tool manifest compatibility;
- LLM/Tool adapter construction;
- compiled AgentProfile validation.

Static configuration validation MUST NOT duplicate these Provider-runtime responsibilities.

## 5. CLI identity source

The reference CLI uses UUID v4 to generate collision-resistant opaque ids:

```text
ses_<uuid-simple>
agt_<uuid-simple>
evt_<uuid-simple>
msg_<uuid-simple>
```

UUID structure is not semantically meaningful. The resulting Harness identifiers remain opaque strings.

Event timestamps use UTC `Timestamp::now_utc()`.

## 6. Commands

### 6.1 `config check`

```text
harness [--config FILE] config check
```

Loads and statically compiles configuration. Provider processes are not started.

### 6.2 `session create`

```text
harness [--config FILE] session create
```

Opens the configured Batch 15 durable local storage, creates a new Session with one `session/created` event, prints the SessionId, and exits. Provider processes are not required.

### 6.3 `inspect`

```text
harness [--config FILE] inspect SESSION_ID [--pretty]
```

Reads the complete durable Session log in pages and emits canonical serialized SessionEvent objects. Default output is one compact JSON event per line. `--pretty` changes presentation only.

### 6.4 `run`

```text
harness [--config FILE] run SESSION_ID [--profile NAME]
```

The profile is selected from `--profile` or `[runtime].default_profile`.

The command builds one HarnessRuntime, opens exactly one Agent for the existing Session, waits for startup/replay work to converge to an idle or explicit blocked/approval boundary, and then enters a foreground line-oriented loop. Each non-empty input line is submitted as a next-turn user message. `/quit` ends the loop.

For each submitted turn, the CLI waits until:

- no live external Agent operation exists;
- no pending Inbox input remains;
- no Turn/Step is open.

A recovery block is surfaced as an error. An approval wait is surfaced as unsupported by the Batch 16 CLI rather than auto-approved.

New Assistant text emitted during the turn is printed to the operator. Model failures are surfaced to stderr.

On normal interaction completion the CLI closes the Agent and then shuts down the Runtime, preserving the Batch 14 Agent-before-Provider shutdown order.

## 7. Persistence and restart

The CLI is intentionally not a daemon. Each invocation is a new process.

A Session created or used by one CLI invocation MUST be reopenable by a later invocation through Batch 15 durable local storage. The selected profile is process composition input and is not persisted as a Session binding in Batch 16.

## 8. Non-goals

Batch 16 does not introduce:

- background daemon/server mode;
- HTTP, gRPC, WebSocket, or local control sockets;
- first-class credentials or secret stores;
- interactive durable approval UI;
- recovery-resolution UI;
- JSON Schema execution;
- RuntimeEvent telemetry/observability;
- dynamic Provider registration/restart policy;
- TypeScript/Go Provider SDKs.

These remain later skeleton/runtime batches.
