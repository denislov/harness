#!/usr/bin/env python3
"""SDK-free Provider Protocol v1 reference provider.

stdout is protocol-only NDJSON. Diagnostics go to stderr.

The original Batch 10 ``echo-model`` remains unchanged for protocol conformance.
Batch 11 adds ``agent-model`` so the Rust Agent acceptance test can exercise a
real out-of-process LLM -> Tool -> LLM loop through this Python process.
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
        "providerVersion": "1.1.0",
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
            {"kind": "llm", "models": ["echo-model", "agent-model"]},
        ],
    }


def last_text(request: dict[str, Any]) -> str:
    for message in reversed(request.get("messages", [])):
        for block in reversed(message.get("content", [])):
            if block.get("type") == "text":
                return str(block.get("text", ""))
    return ""


def trailing_tool_result_text(request: dict[str, Any]) -> str | None:
    messages = request.get("messages", [])
    if not messages:
        return None
    for block in reversed(messages[-1].get("content", [])):
        if block.get("type") != "tool-result":
            continue
        for content in block.get("content", []):
            if content.get("type") == "text":
                return str(content.get("text", ""))
        return ""
    return None


def emit_stream_events(stream_id: str, events: list[dict[str, Any]]) -> None:
    for seq, event in enumerate(events, start=1):
        emit(
            {
                "jsonrpc": "2.0",
                "method": "llm.event",
                "params": {"streamId": stream_id, "seq": seq, "event": event},
            }
        )


def emit_text_stream(stream_id: str, text: str) -> None:
    emit_stream_events(
        stream_id,
        [
            {"type": "block-start", "index": 0, "blockType": "text"},
            {"type": "text-delta", "index": 0, "text": text},
            {"type": "block-end", "index": 0, "block": {"type": "text", "text": text}},
            {"type": "finish", "reason": "completed"},
        ],
    )


def emit_tool_call_stream(
    stream_id: str,
    call_id: str,
    tool_name: str,
    arguments_json: str,
) -> None:
    emit_stream_events(
        stream_id,
        [
            {"type": "block-start", "index": 0, "blockType": "tool-call"},
            {
                "type": "tool-call-delta",
                "index": 0,
                "callId": call_id,
                "name": tool_name,
                "argumentsDelta": arguments_json,
            },
            {
                "type": "block-end",
                "index": 0,
                "block": {
                    "type": "tool-call",
                    "id": call_id,
                    "name": tool_name,
                    "argumentsJson": arguments_json,
                },
            },
            {"type": "finish", "reason": "completed"},
        ],
    )


def emit_agent_model_stream(stream_id: str, request: dict[str, Any]) -> None:
    tool_result = trailing_tool_result_text(request)
    if tool_result is not None:
        emit_text_stream(stream_id, f"final: {tool_result}")
        return

    arguments_json = json.dumps(
        {"text": last_text(request)}, separators=(",", ":"), ensure_ascii=False
    )
    request_id = str(request.get("requestId", "request"))
    emit_tool_call_stream(
        stream_id,
        f"call_echo_{request_id}",
        "echo",
        arguments_json,
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
        model = request.get("model")
        if model not in {"echo-model", "agent-model"}:
            success(
                rpc_id,
                {
                    "accepted": False,
                    "streamId": stream_id,
                    "reason": f"unknown model: {model}",
                },
            )
            return True

        # The response is flushed before the first llm.event. The streamId was
        # allocated by Core, so Host installed routing before sending llm.start.
        success(rpc_id, {"accepted": True, "streamId": stream_id})
        if model == "agent-model":
            emit_agent_model_stream(stream_id, request)
        else:
            emit_text_stream(stream_id, f"echo: {last_text(request)}")
        return True

    failure(rpc_id, -32601, f"method not found: {method}")
    return True


def handle_notification(message: dict[str, Any]) -> None:
    method = message.get("method")
    if method == "capability.cancel":
        # This reference provider performs immediate local work. A real provider
        # would use operationId to cancel its in-flight task/future.
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
