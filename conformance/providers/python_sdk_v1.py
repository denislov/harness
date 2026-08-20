#!/usr/bin/env python3
"""Canonical conformance provider implemented with the Python SDK."""

from __future__ import annotations

import json

from harness_provider_sdk import ProviderApp, SideEffect, ToolResult

app = ProviderApp("sdk-conformance", "1.0.0")


@app.tool(
    name="conformance.echo",
    version="1",
    parallel_safe=True,
    side_effect=SideEffect.READ_ONLY,
)
def echo(ctx):
    text = json.dumps(
        ctx.arguments,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    )
    return ToolResult.success_text(text)


@app.tool(
    name="conformance.fail",
    version="1",
    parallel_safe=True,
    side_effect=SideEffect.READ_ONLY,
)
def fail(_ctx):
    return ToolResult.error("CONFORMANCE_ERROR", "requested failure")


@app.tool(
    name="conformance.wait",
    version="1",
    parallel_safe=True,
    side_effect=SideEffect.READ_ONLY,
)
async def wait_tool(ctx):
    cause = await ctx.cancellation.wait()
    return ToolResult.cancelled(cause)


@app.model("conformance-text")
async def text_model(ctx):
    await ctx.stream.text("golden text")
    await ctx.stream.usage(7, 3, cacheReadTokens=2)
    # No explicit finish: the SDK must append finish(completed).


@app.model("conformance-tool-call")
async def tool_call_model(ctx):
    await ctx.stream.tool_call(
        "call_conformance",
        "conformance.echo",
        {"value": 42},
    )
    # No explicit finish: the SDK must append finish(completed).


@app.model("conformance-error")
async def error_model(_ctx):
    raise RuntimeError("conformance model failure")


@app.model("conformance-wait")
async def wait_model(ctx):
    await ctx.cancellation.wait()


if __name__ == "__main__":
    app.run()
