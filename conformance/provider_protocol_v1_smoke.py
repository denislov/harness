#!/usr/bin/env python3
"""Process-level smoke test for Provider Protocol v1.

Uses only the Python standard library. By default it launches the Batch 10
example provider from providers/example-python/provider.py.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any, TextIO


class ProtocolFailure(RuntimeError):
    pass


def emit(stdin: TextIO, message: dict[str, Any]) -> None:
    stdin.write(json.dumps(message, separators=(",", ":"), ensure_ascii=False) + "\n")
    stdin.flush()


def read_message(stdout: TextIO) -> dict[str, Any]:
    line = stdout.readline()
    if line == "":
        raise ProtocolFailure("provider stdout reached EOF")
    try:
        value = json.loads(line)
    except json.JSONDecodeError as exc:
        raise ProtocolFailure(f"provider emitted invalid JSON: {exc}: {line!r}") from exc
    if not isinstance(value, dict) or value.get("jsonrpc") != "2.0":
        raise ProtocolFailure(f"provider emitted invalid JSON-RPC envelope: {value!r}")
    return value


def expect_response(stdout: TextIO, rpc_id: str) -> dict[str, Any]:
    message = read_message(stdout)
    if message.get("id") != rpc_id:
        raise ProtocolFailure(f"expected response {rpc_id}, got {message!r}")
    if "error" in message:
        raise ProtocolFailure(f"request {rpc_id} failed: {message['error']!r}")
    if "result" not in message:
        raise ProtocolFailure(f"response {rpc_id} has no result: {message!r}")
    result = message["result"]
    if not isinstance(result, dict):
        raise ProtocolFailure(f"response {rpc_id} result must be an object")
    return result


def run(provider: Path) -> None:
    process = subprocess.Popen(
        [sys.executable, str(provider)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    assert process.stdin is not None
    assert process.stdout is not None
    assert process.stderr is not None

    stdin = process.stdin
    stdout = process.stdout
    try:
        emit(
            stdin,
            {
                "jsonrpc": "2.0",
                "id": "rpc_1",
                "method": "provider.initialize",
                "params": {
                    "protocolVersion": "1.0",
                    "runtime": {"name": "harness-conformance", "version": "0.1.0"},
                },
            },
        )
        manifest = expect_response(stdout, "rpc_1")
        if manifest.get("protocolVersion") != "1.0":
            raise ProtocolFailure(f"unexpected manifest: {manifest!r}")

        emit(
            stdin,
            {"jsonrpc": "2.0", "id": "rpc_2", "method": "provider.ping", "params": {}},
        )
        if expect_response(stdout, "rpc_2") != {"ok": True}:
            raise ProtocolFailure("provider.ping did not return ok=true")

        emit(
            stdin,
            {
                "jsonrpc": "2.0",
                "id": "rpc_3",
                "method": "tool.invoke",
                "params": {
                    "operationId": "inv_1",
                    "invocationId": "inv_1",
                    "callId": "call_1",
                    "sessionId": "ses_1",
                    "tool": "echo",
                    "argumentsJson": '{"value":42}',
                    "attempt": 1,
                    "idempotencyKey": "idem_1",
                },
            },
        )
        tool_result = expect_response(stdout, "rpc_3")
        if tool_result.get("outcome", {}).get("kind") != "success":
            raise ProtocolFailure(f"unexpected tool outcome: {tool_result!r}")

        emit(
            stdin,
            {
                "jsonrpc": "2.0",
                "id": "rpc_4",
                "method": "llm.start",
                "params": {
                    "operationId": "req_1",
                    "streamId": "str_core_1",
                    "request": {
                        "requestId": "req_1",
                        "sessionId": "ses_1",
                        "provider": "example-python",
                        "model": "echo-model",
                        "messages": [
                            {
                                "id": "msg_1",
                                "role": "user",
                                "source": {"kind": "user"},
                                "content": [{"type": "text", "text": "hello"}],
                            }
                        ],
                        "options": {},
                    },
                },
            },
        )
        start = expect_response(stdout, "rpc_4")
        if start != {"accepted": True, "streamId": "str_core_1"}:
            raise ProtocolFailure(f"unexpected llm.start result: {start!r}")

        events = [read_message(stdout) for _ in range(4)]
        for expected_seq, message in enumerate(events, start=1):
            if message.get("method") != "llm.event":
                raise ProtocolFailure(f"expected llm.event, got {message!r}")
            params = message.get("params", {})
            if params.get("streamId") != "str_core_1" or params.get("seq") != expected_seq:
                raise ProtocolFailure(f"invalid stream routing/sequence: {message!r}")
        if events[-1]["params"]["event"].get("type") != "finish":
            raise ProtocolFailure("LLM stream did not terminate with finish")

        emit(
            stdin,
            {
                "jsonrpc": "2.0",
                "method": "capability.cancel",
                "params": {"operationId": "req_already_done", "cause": {"kind": "user"}},
            },
        )

        emit(
            stdin,
            {
                "jsonrpc": "2.0",
                "id": "rpc_5",
                "method": "provider.shutdown",
                "params": {},
            },
        )
        if expect_response(stdout, "rpc_5") != {"accepted": True}:
            raise ProtocolFailure("provider.shutdown was not accepted")
    finally:
        try:
            stdin.close()
        except OSError:
            pass

    return_code = process.wait(timeout=5)
    stderr = process.stderr.read()
    if return_code != 0:
        raise ProtocolFailure(
            f"provider exited with {return_code}; stderr follows:\n{stderr}"
        )
    print("Provider Protocol v1 smoke test passed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "provider",
        nargs="?",
        type=Path,
        default=(
            Path(__file__).resolve().parents[1]
            / "providers"
            / "example-python"
            / "provider.py"
        ),
    )
    args = parser.parse_args()
    try:
        run(args.provider)
    except (OSError, ProtocolFailure, subprocess.TimeoutExpired) as exc:
        print(f"Provider Protocol v1 smoke test failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
