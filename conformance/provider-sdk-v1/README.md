# Provider SDK Conformance Contract v1

This directory is the language-neutral acceptance contract for Provider SDK implementations that speak Harness Provider Protocol v1.

The contract is intentionally process-oriented. A candidate SDK supplies a small **conformance provider** implementing the fixed capabilities in `contract.json`. The generic runner launches that provider as a subprocess, performs the protocol handshake, and executes every JSON fixture under `fixtures/`.

## Fixed conformance provider

Every SDK conformance provider MUST expose exactly the manifest in `contract.json`:

- `conformance.echo`: read-only Tool returning one compact, key-sorted JSON text block.
- `conformance.fail`: read-only Tool returning `CONFORMANCE_ERROR` / `requested failure`.
- `conformance.wait`: read-only Tool that remains pending until cancellation.
- `conformance-text`: LLM model emitting a text block, usage, then normal completion.
- `conformance-tool-call`: LLM model emitting one deterministic ToolCall, then normal completion.
- `conformance-error`: LLM model whose handler fails so the SDK must normalize it into an error finish.
- `conformance-wait`: LLM model that remains pending until cancellation.

The provider ID and version are fixture values, not the SDK package's own product version. All language implementations use the same values so golden outputs remain byte-structure comparable.

## Fixture semantics

Each fixture is a sequence of two operation types:

- `send`: write the exact JSON object to provider stdin as one NDJSON frame.
- `expect`: read exactly one JSON object from provider stdout and compare it structurally for exact equality.

The runner automatically performs `provider.initialize` before each fixture and verifies the exact manifest from `contract.json`. It automatically sends `provider.shutdown` after each fixture and requires `{ "accepted": true }` before process exit.

No wildcard matcher exists in v1. Golden outputs are deliberately deterministic. If an SDK cannot produce a stable conformance transcript, the difference must be resolved in the SDK contract rather than hidden behind permissive matching.

## Python SDK

Batch 13 includes the first implementation:

```bash
python3 conformance/run_python_sdk_v1.py
```

To run the generic suite against another implementation:

```bash
python3 conformance/provider_sdk_v1_runner.py -- <provider-command> [args...]
```

The command inherits the caller's environment. A future TypeScript SDK, for example, can point the same runner at a Node conformance provider without changing any fixture.
