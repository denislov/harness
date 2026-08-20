# Batch 10 — Provider Protocol v1 amendment

This amendment records the decisions introduced when the previously draft-only Provider Protocol became executable code.

## Normative decisions

1. `harness-provider-protocol` is a wire-only crate and MUST NOT depend on `harness-types`, `harness-llm`, `harness-tools`, `harness-agent`, or storage crates.
2. v1 JSON-RPC request ids are non-empty strings only.
3. stdout is protocol-only NDJSON; stderr is diagnostic-only.
4. Provider-to-Core requests are unsupported in v1.
5. `tool.invoke.operationId == invocationId`.
6. `llm.start.operationId == request.requestId`.
7. Core allocates `streamId` before `llm.start`; Provider echoes it.
8. LLM stream sequence starts at 1 and is gap-free.
9. Provider Tool outcomes are limited to `success`, `error`, and `cancelled`; `denied` and `unknown` remain Core-derived.
10. A protocol/transport failure never authorizes automatic replay of side-effecting Tool work.
11. A timed-out JSON-RPC id is retained temporarily so a late response can be ignored without poisoning the Provider process.
12. Batch 10 stops at the transport boundary. Rust `LlmProvider` / `ToolExecutor` adapters are Batch 11 work.
13. A manifest declaring an `idempotent-write` Tool MUST also declare keyed idempotency support.
14. Host MUST reject Tool/model dispatch that is not declared by the initialized Provider manifest; LLM request provider identity MUST equal the manifest provider id.

## Reference implementation

Batch 10 adds:

- strongly typed wire schemas and validators;
- JSON-RPC/NDJSON codec;
- subprocess lifecycle state;
- request/response correlation;
- LLM stream routing;
- bounded stderr history;
- graceful shutdown with force-kill fallback;
- dependency-free Python reference provider;
- process-level Python conformance smoke test.
