#!/usr/bin/env python3
"""Batch 21 provider fixture: crash generation 1, optionally drift generation 2."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[4]
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

_MARKER = Path(os.environ["HARNESS_SUPERVISOR_MARKER"])
_RESTART_FAILURE_MARKER = _MARKER.with_name(f"{_MARKER.name}.restart-failed")
_DRIFT = os.environ.get("HARNESS_SUPERVISOR_DRIFT") == "1"
_FAIL_FIRST_RESTART = os.environ.get("HARNESS_SUPERVISOR_FAIL_FIRST_RESTART") == "1"

if _FAIL_FIRST_RESTART and _MARKER.exists() and not _RESTART_FAILURE_MARKER.exists():
    _RESTART_FAILURE_MARKER.parent.mkdir(parents=True, exist_ok=True)
    _RESTART_FAILURE_MARKER.write_text("failed-before-initialize\n", encoding="utf-8")
    sys.exit(18)

_VERSION = "2.0.0" if _DRIFT and _MARKER.exists() else "1.2.0"

app = ProviderApp("supervised-python", _VERSION)


@app.tool(
    name="echo",
    version="1",
    parallel_safe=True,
    side_effect=SideEffect.READ_ONLY,
)
def echo(ctx):
    if not _MARKER.exists():
        _MARKER.parent.mkdir(parents=True, exist_ok=True)
        with _MARKER.open("w", encoding="utf-8") as handle:
            handle.write("generation-1-crashed\n")
            handle.flush()
            os.fsync(handle.fileno())
        os._exit(17)

    return ToolResult.success_text(
        json.dumps(ctx.arguments, sort_keys=True, ensure_ascii=False)
    )


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
