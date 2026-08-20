# Batch 17 — Credentials, Runtime Events, and Local Observability

Status: normative for the Batch 17 surface.

## 1. Scope

Batch 17 adds process-level credential resolution and operational events. It does not change the Session event schema, Agent state machine, Provider Protocol, Tool recovery rules, or Provider SDK protocol conformance contract.

The boundary is:

```text
Harness configuration
        |
        +-- credential references ----> CredentialResolver ----> SecretValue
        |                                      |
        |                                      +----> Provider subprocess env
        |
        +-- observability path --------> RuntimeEventBus ----> JSONL subscriber

SessionEvent remains the durable execution source of truth.
```

## 2. Credential contract

`CredentialKey` is a non-empty process-level lookup key. It is not a Session identifier and is never persisted into SessionEvent data by Core.

`CredentialResolver` resolves one `CredentialKey` into one `SecretValue`. Resolution occurs when a Provider is being prepared for process start. Static configuration compilation MUST NOT require secret material to be present.

`SecretValue` MUST NOT implement serialization and its `Debug` representation MUST redact the underlying value. Runtime errors and RuntimeEvents MAY contain credential keys and target environment variable names, but MUST NOT contain resolved secret values.

A Provider process may declare credential environment bindings:

```text
subprocess env key -> CredentialKey
```

Plain `env` configuration and credential configuration MUST NOT target the same subprocess environment key. Such a composition is invalid before Provider start.
Plain `env` values remain ordinary configuration and are not redacted by this contract; secret-bearing values SHOULD use credential bindings instead.

Credential resolution failure prevents that Provider from starting. Providers already started earlier in the same Runtime build MUST be shut down before the build error is returned.

Batch 17 does not change the inherited parent-process environment behavior of `ProviderHost`. Credential bindings are explicit secret injection, not environment isolation.

## 3. Config-backed environment resolver

The Batch 17 file configuration supports environment-backed credentials:

```toml
[credentials.openai]
source = "env"
variable = "OPENAI_API_KEY"

[[providers]]
id = "provider"
program = "provider"
credentials = { OPENAI_API_KEY = "openai" }
```

The TOML file contains the credential key and source environment variable name only. The value of `OPENAI_API_KEY` is read when the Runtime starts the Provider.

`config check`, `session create`, and `inspect` MUST NOT resolve credentials. `run` resolves only credentials needed by Providers participating in Runtime composition.

## 4. RuntimeEvent contract

A RuntimeEvent is operational observation, not durable Agent state. RuntimeEvents MUST NOT be used to reconstruct Session messages, recovery state, inbox state, Tool dispatch state, approvals, or Agent turn/step state.

Each event has:

```text
schemaVersion = 1
seq           = process-local monotonically increasing JS-safe u64
time          = observation-time UTC timestamp
type          = event kind
data          = kind-specific fields when applicable
```

`seq` restarts for a new RuntimeEventBus/process. It is not comparable to `SessionEvent.seq` and is not globally durable.

Batch 17 defines the following event families:

```text
runtime/build-started
runtime/build-failed
runtime/started
runtime/stopping
runtime/stopped

provider/starting
provider/ready
provider/start-failed
provider/stopping
provider/stopped

credential/resolution-failed

agent/opening
agent/opened
agent/open-failed
agent/closing
agent/closed
```

Build-failure and lifecycle events intentionally omit arbitrary source error strings. This reduces the risk of Provider stderr or third-party error messages copying sensitive material into observability output.

## 5. RuntimeEventBus semantics

`RuntimeEventBus` is a bounded process-local broadcast channel. Publishers do not wait for subscribers and the absence of a subscriber is not an error.

A slow subscriber may lag and lose RuntimeEvents. Subscriber loss MUST NOT alter SessionEvent persistence, Agent behavior, Tool execution, Provider lifecycle decisions, or Runtime shutdown decisions.

Runtime shutdown ordering remains:

```text
runtime/stopping
Agent shutdowns
Provider shutdowns
runtime/stopped
```

Provider and Agent lifecycle events are emitted around the corresponding process-local transitions.

## 6. JSONL observability

The application config may opt into a local JSONL subscriber:

```toml
[observability]
runtime_events_jsonl = ".harness/runtime-events.jsonl"
```

Only the `run` command starts this observer. `config check`, `session create`, and `inspect` do not create the runtime-event log.

The JSONL file is append-only at the application-shell level: each RuntimeEvent is serialized as one JSON object followed by one newline. Multiple CLI `run` processes may append separate process-local event sequences to the same file. Batch 17 defines neither a cross-process total order nor cross-process log-file locking.

The JSONL observer is not a replacement for SessionStore and is not an input to recovery.

## 7. Non-goals

Batch 17 does not implement:

- remote secret managers or cloud KMS integrations;
- credential rotation/watch APIs;
- Provider environment allowlisting/isolation;
- OpenTelemetry exporters, tracing subscribers, metrics backends, or remote log shipping;
- durable RuntimeEvent storage semantics;
- changes to Provider Protocol or SessionEvent schema.

Those capabilities may build on the seams introduced here without changing Agent state ownership.
