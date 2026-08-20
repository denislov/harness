# Batch 13 — Provider SDK Conformance Contract v1

**Status:** Normative supplement for Provider Protocol v1 SDK implementations.

## 1. Purpose

Batch 13 freezes a language-neutral acceptance contract for Provider SDKs. The contract verifies that an SDK implementation produces the same externally observable Provider Protocol v1 behavior regardless of the implementation language.

This supplement does **not** change Provider Protocol v1 wire syntax or Harness Core domain APIs.

## 2. Separation of concerns

Three different test layers remain distinct:

1. **Protocol conformance** checks JSON-RPC/NDJSON framing and Provider Protocol v1 wire semantics.
2. **SDK conformance** checks that an SDK runtime correctly implements the provider-side lifecycle and normalizes provider-author code into the wire protocol.
3. **Agent acceptance** checks that Harness Core can use a foreign-language provider through `ProviderHost` and the domain adapters.

A Provider SDK MAY have additional implementation-specific unit tests, but passing those tests does not substitute for the shared SDK conformance suite.

## 3. Suite identity

The first suite is identified by:

```text
suiteVersion    = 1.0
fixtureSchema   = 1
protocolVersion = 1.0
```

The suite definition lives under:

```text
conformance/provider-sdk-v1/
```

`contract.json` is authoritative for the fixed conformance-provider manifest. `fixtures/*.json` are authoritative for scenario transcripts.

## 4. Canonical conformance provider

Every SDK under test MUST provide a small executable conformance provider with this identity:

```text
providerId      = sdk-conformance
providerVersion = 1.0.0
```

The provider MUST declare exactly these capabilities, in the manifest order specified by `contract.json`:

```text
Tool: conformance.echo
Tool: conformance.fail
Tool: conformance.wait
LLM:  conformance-text
      conformance-tool-call
      conformance-error
      conformance-wait
```

The conformance provider is test code. Its provider version is intentionally independent from the SDK package version so all language implementations emit the same manifest.

## 5. Canonical Tool behavior

### 5.1 `conformance.echo`

- side effect: `read-only`;
- parallel safe: `true`;
- idempotency-key support: `false`;
- returns `success`;
- returns exactly one text block;
- the text is the decoded JSON arguments re-encoded as compact JSON with lexicographically sorted object keys.

### 5.2 `conformance.fail`

Returns the authoritative Tool outcome:

```text
kind    = error
code    = CONFORMANCE_ERROR
message = requested failure
content = []
```

### 5.3 `conformance.wait`

Remains pending until `capability.cancel` targets its active `operationId`. The original `tool.invoke` RPC then terminates with a `cancelled` Tool outcome carrying the same cancellation cause.

## 6. Canonical LLM behavior

### 6.1 `conformance-text`

Emits:

```text
block-start(text)
text-delta("golden text")
block-end(text("golden text"))
usage(input=7, output=3, cacheReadTokens=2)
finish(completed)
```

The model handler itself does not explicitly emit `finish`; the SDK runtime MUST append the completed finish when the handler returns normally.

### 6.2 `conformance-tool-call`

Emits one ToolCall:

```text
callId        = call_conformance
name          = conformance.echo
argumentsJson = {"value":42}
```

The SDK assigns contiguous stream sequence numbers and appends `finish(completed)` after normal handler return.

### 6.3 `conformance-error`

The handler raises the deterministic failure `conformance model failure`. The SDK MUST normalize it to one terminal error finish with:

```text
code    = INTERNAL
message = model handler failed: conformance model failure
```

### 6.4 `conformance-wait`

Remains pending until cancellation. Cancellation MUST produce exactly one terminal `finish(cancelled)` event with `CANCELLED` failure code and the received cause in failure details.

## 7. Golden fixture model

A fixture contains:

```json
{
  "schemaVersion": 1,
  "name": "...",
  "description": "...",
  "steps": [
    {"send": {}},
    {"expect": {}}
  ]
}
```

`send` writes the exact object as one Provider Protocol NDJSON frame. `expect` reads one frame and compares the parsed JSON object for exact structural equality.

Suite v1 deliberately defines no wildcard, regex, unordered-array, or partial-object matcher. Observable conformance outputs MUST be deterministic.

## 8. Process isolation

Each fixture runs in a fresh provider process.

Before fixture steps, the runner MUST:

1. start the provider process;
2. send `provider.initialize` with the runtime object from `contract.json`;
3. require the exact manifest from `contract.json`.

After fixture steps, the runner MUST:

1. send `provider.shutdown`;
2. require `{ "accepted": true }`;
3. require normal process exit;
4. reject additional protocol frames emitted after the shutdown acknowledgement.

Provider stderr is diagnostic-only and is surfaced when a fixture fails.

## 9. Required v1 scenarios

Suite v1 contains these required scenarios:

```text
ping
Tool success
Tool error
Tool cancellation
LLM text + usage + automatic finish
LLM ToolCall + automatic finish
LLM handler error normalization
LLM cancellation
active operationId collision across Tool/LLM
```

All scenarios are mandatory for an SDK claiming Provider SDK Conformance v1.

## 10. Operation identity

`operationId` ownership is process-wide while an operation is active. It is not scoped separately by Tool and LLM capability type.

If an LLM request owns an active operation ID and a Tool invocation attempts to reuse that ID, the second operation MUST be rejected without replacing or cancelling the first operation.

This requirement prevents cancellation routing from becoming capability-type ambiguous.

## 11. Cancellation semantics

SDK cancellation is cooperative runtime control. It does not prove rollback of external side effects.

The conformance suite verifies only observable provider-side protocol behavior:

- cancellation is routed by `operationId`;
- the original Tool RPC receives `cancelled`;
- the LLM stream receives one terminal cancelled finish;
- the cancellation cause is preserved.

Harness Core remains responsible for side-effect interpretation at the durable `tool/dispatched` boundary.

## 12. Cross-language rule

A future TypeScript, Go, Rust, or other Provider SDK MUST run the same `contract.json` and fixture files. It may supply its own conformance-provider executable, but it MUST NOT fork the golden outputs to accommodate implementation differences.

If a cross-language implementation exposes a legitimate incompatibility in the contract, the shared suite itself must be versioned and updated.

## 13. Versioning

`Provider Protocol` and `Provider SDK Conformance` are versioned independently.

A change to the canonical manifest, required scenario semantics, or existing golden outputs requires a new conformance-suite version. Adding implementation-specific tests does not change the suite version.

Batch 13 freezes only suite `1.0`.

## 14. Invariants

- **C13-01** — SDK conformance fixtures are language-neutral protocol transcripts.
- **C13-02** — Every fixture runs in a fresh provider process.
- **C13-03** — Initialization manifest equality is exact.
- **C13-04** — Golden `expect` objects use exact structural equality; no wildcard matching exists in v1.
- **C13-05** — LLM stream sequence numbers are contiguous and begin at one for every stream.
- **C13-06** — A normally returning model handler is completed by exactly one `finish(completed)` when it did not already finish itself.
- **C13-07** — Model-handler failure becomes exactly one terminal error finish.
- **C13-08** — Cancellation preserves the cancellation cause at the protocol boundary.
- **C13-09** — Active `operationId` values are unique across Tool and LLM operations within one provider process.
- **C13-10** — Passing SDK conformance does not weaken Core crash-recovery or Tool side-effect semantics.

## 15. Non-goals

Batch 13 does not add:

- TypeScript or Go SDK implementations;
- network Provider transports;
- performance benchmarks;
- fuzzing;
- JSON Schema runtime dependencies;
- new Provider Protocol methods;
- new Harness Core events or public Rust APIs.
