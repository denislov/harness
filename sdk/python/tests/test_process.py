from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path
from typing import Any, TextIO


class ProviderProcessTests(unittest.TestCase):
    def setUp(self) -> None:
        fixture = Path(__file__).with_name("slow_provider.py")
        self.process = subprocess.Popen(
            [sys.executable, str(fixture)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        assert self.process.stdin is not None
        assert self.process.stdout is not None
        self.stdin: TextIO = self.process.stdin
        self.stdout: TextIO = self.process.stdout
        self.emit(
            {
                "jsonrpc": "2.0",
                "id": "init",
                "method": "provider.initialize",
                "params": {
                    "protocolVersion": "1.0",
                    "runtime": {"name": "sdk-test", "version": "0.1.0"},
                },
            }
        )
        self.response("init")

    def tearDown(self) -> None:
        if self.process.poll() is None:
            try:
                self.emit(
                    {
                        "jsonrpc": "2.0",
                        "id": "shutdown",
                        "method": "provider.shutdown",
                        "params": {},
                    }
                )
                self.response("shutdown")
            except (BrokenPipeError, RuntimeError):
                pass
        try:
            self.stdin.close()
        except OSError:
            pass
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=3)
        if self.process.stdout is not None:
            self.process.stdout.close()
        if self.process.stderr is not None:
            self.process.stderr.close()

    def emit(self, message: dict[str, Any]) -> None:
        self.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self.stdin.flush()

    def read(self) -> dict[str, Any]:
        line = self.stdout.readline()
        if line == "":
            stderr = self.process.stderr.read() if self.process.stderr else ""
            raise RuntimeError(f"provider EOF; stderr={stderr}")
        value = json.loads(line)
        self.assertEqual(value.get("jsonrpc"), "2.0")
        return value

    def response(self, rpc_id: str) -> dict[str, Any]:
        message = self.read()
        self.assertEqual(message.get("id"), rpc_id)
        self.assertNotIn("error", message)
        return message["result"]

    def test_llm_cancel_emits_terminal_cancelled_finish(self) -> None:
        self.emit(
            {
                "jsonrpc": "2.0",
                "id": "llm",
                "method": "llm.start",
                "params": {
                    "operationId": "req_1",
                    "streamId": "str_1",
                    "request": {
                        "requestId": "req_1",
                        "sessionId": "ses_1",
                        "provider": "sdk-cancel-test",
                        "model": "slow-model",
                        "messages": [],
                        "options": {},
                    },
                },
            }
        )
        self.assertEqual(
            self.response("llm"), {"accepted": True, "streamId": "str_1"}
        )
        self.emit(
            {
                "jsonrpc": "2.0",
                "method": "capability.cancel",
                "params": {"operationId": "req_1", "cause": {"kind": "timeout"}},
            }
        )
        event = self.read()
        self.assertEqual(event.get("method"), "llm.event")
        self.assertEqual(event["params"]["seq"], 1)
        self.assertEqual(event["params"]["event"]["type"], "finish")
        self.assertEqual(event["params"]["event"]["reason"], "cancelled")
        self.assertEqual(
            event["params"]["event"]["failure"]["code"], "CANCELLED"
        )

    def test_tool_cancel_completes_original_rpc_as_cancelled(self) -> None:
        self.emit(
            {
                "jsonrpc": "2.0",
                "id": "tool",
                "method": "tool.invoke",
                "params": {
                    "operationId": "inv_1",
                    "invocationId": "inv_1",
                    "callId": "call_1",
                    "sessionId": "ses_1",
                    "tool": "slow-tool",
                    "argumentsJson": "{}",
                    "attempt": 1,
                    "idempotencyKey": "idem_1",
                },
            }
        )
        self.emit(
            {
                "jsonrpc": "2.0",
                "method": "capability.cancel",
                "params": {"operationId": "inv_1", "cause": {"kind": "user"}},
            }
        )
        result = self.response("tool")
        self.assertEqual(result["outcome"], {"kind": "cancelled", "cause": "user"})


if __name__ == "__main__":
    unittest.main()
