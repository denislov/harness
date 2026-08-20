from __future__ import annotations

import asyncio

from harness_provider_sdk import ProviderApp, SideEffect, ToolResult

app = ProviderApp("sdk-cancel-test", "1.0.0")


@app.tool(
    name="slow-tool",
    version="1",
    parallel_safe=False,
    side_effect=SideEffect.READ_ONLY,
)
async def slow_tool(_ctx):
    await asyncio.sleep(60)
    return ToolResult.success_text("late")


@app.model("slow-model")
async def slow_model(_ctx):
    await asyncio.sleep(60)


if __name__ == "__main__":
    app.run()
