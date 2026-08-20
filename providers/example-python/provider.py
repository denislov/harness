#!/usr/bin/env python3
"""SDK-free Provider Protocol v1 reference provider.

stdout is protocol-only NDJSON. Diagnostics go to stderr.
"""

from __future__ import annotations

import json
import sys
from typing import Any

PROTOCOL_VERSION = "1.0"


def emit(message: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(message, separators=(",", ":"), ensure_ascii=False) + "\n")
    sys.stdout.flush()


def success(rpc_id: str, result: Any) -> None:
    emit({"jsonrpc": "2.0", "id": rpc_id, "result": result})


def failure(rpc_id: str, code: int, message: str, data: Any | None = None) -> None:
    error: dict[str, Any] = {"code": code, "message": message}
    if data is not None:
        error["data"] = data
    emit({"jsonrpc": "2.0", "id": rpc_id, "error": error})


def manifest() -> dict[str, Any]:
    return {
        "providerId": "example-python",
        "providerVersion": "1.0.0",
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": [
            {
                "kind": "tool",
                "name": "echo",
                "version": "1",
                "parallelSafe": True,
                "sideEffect": "read-only",
                "supportsIdempotencyKey": False,
            },
            {"kind": "llm", "models": ["echo-model"]},
        ],
    }


def last_text(request: dict[str, Any]) -> str:
    for message in reversed(request.get("messages", [])):
        for block in reversed(message.get("content", [])):
            if block.get("type") == "text":
                return str(block.get("text", ""))
    return ""


def emit_llm_stream(stream_id: str, text: str) -> None:
    events = [
        {
            "type": "block-start",
            "index": 0,
            "blockType": "text",
        },
        {
            "type": "text-delta",
            "index": 0,
            "text": text,
        },
        {
            "type": "block-end",
            "index": 0,
            "block": {"type": "text", "text": text},
        },
        {"type": "finish", "reason": "completed"},
    ]
    for seq, event in enumerate(events, start=1):
        emit(
            {
                "jsonrpc": "2.0",
                "method": "llm.event",
                "params": {"streamId": stream_id, "seq": seq, "event": event},
            }
        )


def handle_request(message: dict[str, Any]) -> bool:
    rpc_id = message.get("id")
    method = message.get("method")
    params = message.get("params") or {}
    if not isinstance(rpc_id, str) or not rpc_id:
        return True

    if method == "provider.initialize":
        if params.get("protocolVersion") != PROTOCOL_VERSION:
            failure(rpc_id, -32602, "unsupported protocolVersion")
        else:
            success(rpc_id, manifest())
        return True

    if method == "provider.ping":
        success(rpc_id, {"ok": True})
        return True

    if method == "provider.shutdown":
        success(rpc_id, {"accepted": True})
        return False

    if method == "tool.invoke":
        if params.get("tool") != "echo":
            success(
                rpc_id,
                {
                    "outcome": {
                        "kind": "error",
                        "code": "NOT_FOUND",
                        "message": f"unknown tool: {params.get('tool')}",
                        "content": [],
                    }
                },
            )
            return True
        try:
            arguments = json.loads(params["argumentsJson"])
        except (KeyError, TypeError, json.JSONDecodeError) as exc:
            failure(rpc_id, -32602, f"invalid tool arguments: {exc}")
            return True
        success(
            rpc_id,
            {
                "outcome": {
                    "kind": "success",
                    "content": [
                        {
                            "type": "text",
                            "text": json.dumps(arguments, sort_keys=True, ensure_ascii=False),
                        }
                    ],
                }
            },
        )
        return True

    if method == "llm.start":
        stream_id = params.get("streamId")
        request = params.get("request") or {}
        if not isinstance(stream_id, str) or not stream_id:
            failure(rpc_id, -32602, "streamId must be a non-empty string")
            return True
        if params.get("operationId") != request.get("requestId"):
            failure(rpc_id, -32602, "operationId must equal request.requestId")
            return True
        if request.get("model") != "echo-model":
            success(
                rpc_id,
                {
                    "accepted": False,
                    "streamId": stream_id,
                    "reason": f"unknown model: {request.get('model')}",
                },
            )
            return True

        # The response is flushed before the first llm.event. The streamId was
        # allocated by Core, so Host may install routing before sending llm.start.
        success(rpc_id, {"accepted": True, "streamId": stream_id})
        emit_llm_stream(stream_id, f"echo: {last_text(request)}")
        return True

    failure(rpc_id, -32601, f"method not found: {method}")
    return True


def handle_notification(message: dict[str, Any]) -> None:
    method = message.get("method")
    if method == "capability.cancel":
        # This reference provider performs only immediate local work. A real
        # provider would use operationId to cancel its in-flight task/future.
        return
    print(f"unsupported notification: {method}", file=sys.stderr, flush=True)


def main() -> int:
    print("example-python provider started", file=sys.stderr, flush=True)
    running = True
    while running:
        line = sys.stdin.readline()
        if line == "":
            break
        try:
            message = json.loads(line)
        except json.JSONDecodeError as exc:
            print(f"invalid stdin JSON: {exc}", file=sys.stderr, flush=True)
            continue

        if message.get("jsonrpc") != "2.0":
            print("invalid jsonrpc version", file=sys.stderr, flush=True)
            continue
        if "id" in message:
            running = handle_request(message)
        else:
            handle_notification(message)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
