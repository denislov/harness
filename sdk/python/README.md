# Harness Provider SDK for Python

Batch 12 introduces the first author-facing SDK for Provider Protocol v1. It has no runtime third-party dependencies and targets Python 3.11+.

## Minimal provider

```python
from harness_provider_sdk import ProviderApp, SideEffect, ToolResult

app = ProviderApp("my-provider", "1.0.0")

@app.tool(
    name="echo",
    version="1",
    parallel_safe=True,
    side_effect=SideEffect.READ_ONLY,
)
def echo(ctx):
    return ToolResult.success_text(str(ctx.arguments))

@app.model("model-a")
async def model_a(ctx):
    await ctx.stream.text("hello")

if __name__ == "__main__":
    app.run()
```

The SDK owns JSON-RPC envelopes, UTF-8 NDJSON framing, `provider.initialize`, manifest generation, ping/shutdown, `tool.invoke`, `llm.start`, LLM stream sequence numbers, operation tracking, and `capability.cancel` dispatch.

## Cancellation

Every handler receives a `CancellationToken`. The SDK also cancels the owning asyncio task when `capability.cancel` arrives. Async handlers should treat cancellation as cooperative and release their external resources. Synchronous Tool handlers execute through `asyncio.to_thread`; Python cannot forcibly stop the underlying worker thread, so such handlers should check the token around irreversible side effects when cancellation matters.

Core recovery semantics remain authoritative. Receiving a cancellation signal never proves that an already-dispatched external side effect was rolled back.

## Development checkout

From the repository root:

```bash
PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests -v
```

The example provider under `providers/example-python` includes a repository-local import shim so it can be launched without installing the package.
