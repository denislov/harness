from __future__ import annotations

import unittest

from harness_provider_sdk import (
    ProviderApp,
    RegistrationError,
    SideEffect,
    ToolResult,
    last_text,
    trailing_tool_result_text,
)


class ProviderSdkTests(unittest.TestCase):
    def test_manifest_is_generated_from_registered_capabilities(self) -> None:
        app = ProviderApp("python-test", "1.0.0")

        @app.tool(
            name="echo",
            version="1",
            parallel_safe=True,
            side_effect=SideEffect.READ_ONLY,
        )
        def echo(_ctx):
            return ToolResult.success_text("ok")

        @app.model("model-a")
        async def model_a(_ctx):
            return None

        self.assertEqual(
            app.manifest(),
            {
                "providerId": "python-test",
                "providerVersion": "1.0.0",
                "protocolVersion": "1.0",
                "capabilities": [
                    {
                        "kind": "tool",
                        "name": "echo",
                        "version": "1",
                        "parallelSafe": True,
                        "sideEffect": "read-only",
                        "supportsIdempotencyKey": False,
                    },
                    {"kind": "llm", "models": ["model-a"]},
                ],
            },
        )

    def test_idempotent_write_requires_key_support(self) -> None:
        app = ProviderApp("python-test", "1.0.0")
        with self.assertRaises(RegistrationError):
            app.tool(
                name="write",
                version="1",
                parallel_safe=False,
                side_effect=SideEffect.IDEMPOTENT_WRITE,
                supports_idempotency_key=False,
            )

    def test_duplicate_capabilities_are_rejected(self) -> None:
        app = ProviderApp("python-test", "1.0.0")

        @app.model("model-a")
        async def first(_ctx):
            return None

        with self.assertRaises(RegistrationError):
            app.model("model-a")(first)

    def test_tool_result_wire_shapes(self) -> None:
        self.assertEqual(
            ToolResult.success_text("ok").to_wire(),
            {"kind": "success", "content": [{"type": "text", "text": "ok"}]},
        )
        self.assertEqual(
            ToolResult.error("E", "failed").to_wire(),
            {"kind": "error", "code": "E", "message": "failed", "content": []},
        )

    def test_request_helpers_preserve_agent_vertical_semantics(self) -> None:
        request = {
            "messages": [
                {"content": [{"type": "text", "text": "hello"}]},
                {
                    "content": [
                        {
                            "type": "tool-result",
                            "toolCallId": "call_1",
                            "content": [{"type": "text", "text": "tool-output"}],
                            "isError": False,
                        }
                    ]
                },
            ]
        }
        self.assertEqual(last_text(request), "hello")
        self.assertEqual(trailing_tool_result_text(request), "tool-output")


if __name__ == "__main__":
    unittest.main()
