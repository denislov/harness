#!/usr/bin/env python3
"""Provider Protocol v1 reference provider implemented with the Python SDK."""

from __future__ import annotations

import json
import sys
from pathlib import Path

# Development checkout shim. Installed providers import harness_provider_sdk
# normally; the repository example remains directly executable without pip.
_REPO_ROOT = Path(__file__).resolve().parents[2]
_SDK_ROOT = _REPO_ROOT / "sdk" / "python"
if str(_SDK_ROOT) not in sys.path:
    sys.path.insert(0, str(_SDK_ROOT))

from harness_provider_sdk import (  # noqa: E402
    ProviderApp,
    SideEffect,
    ToolResult,
    last_text,
    trailing_tool_result_text,
)

app = ProviderApp("example-python", "1.2.0")


@app.tool(
    name="echo",
    version="1",
    parallel_safe=True,
    side_effect=SideEffect.READ_ONLY,
)
def echo(ctx):
    return ToolResult.success_text(
        json.dumps(ctx.arguments, sort_keys=True, ensure_ascii=False)
    )


@app.model("echo-model")
async def echo_model(ctx):
    await ctx.stream.text(f"echo: {last_text(ctx.request)}")


@app.model("agent-model")
async def agent_model(ctx):
    tool_result = trailing_tool_result_text(ctx.request)
    if tool_result is not None:
        await ctx.stream.text(f"final: {tool_result}")
        return

    await ctx.stream.tool_call(
        f"call_echo_{ctx.request.get('requestId', 'request')}",
        "echo",
        {"text": last_text(ctx.request)},
    )


if __name__ == "__main__":
    app.run()
