"""Provider Protocol v1 Python SDK runtime.

The SDK owns JSON-RPC/NDJSON framing, capability discovery, operation tracking,
stream sequencing, cancellation dispatch, and graceful shutdown. Provider
authors register Tool and LLM model handlers and never write protocol envelopes.
"""

from __future__ import annotations

import asyncio
import inspect
import json
import sys
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Awaitable, Callable, Iterable, Mapping

PROTOCOL_VERSION = "1.0"

JsonObject = dict[str, Any]
ToolHandler = Callable[["ToolContext"], "ToolResult | Awaitable[ToolResult]"]
ModelHandler = Callable[["ModelContext"], "None | Awaitable[None]"]


class ProviderSdkError(RuntimeError):
    """Base error raised for provider authoring/configuration mistakes."""


class RegistrationError(ProviderSdkError):
    """Raised when capability registration violates protocol invariants."""


class StreamStateError(ProviderSdkError):
    """Raised when a model handler emits an invalid local stream sequence."""


class SideEffect(str, Enum):
    READ_ONLY = "read-only"
    IDEMPOTENT_WRITE = "idempotent-write"
    NON_IDEMPOTENT_WRITE = "non-idempotent-write"


class CancelCause(str, Enum):
    USER = "user"
    PARENT = "parent"
    TIMEOUT = "timeout"
    POLICY = "policy"
    SHUTDOWN = "shutdown"
    DISPOSED = "disposed"


@dataclass(slots=True)
class CancellationToken:
    """Cooperative cancellation state visible to provider handlers."""

    _event: asyncio.Event = field(default_factory=asyncio.Event)
    _cause: CancelCause | None = None

    @property
    def cancelled(self) -> bool:
        return self._event.is_set()

    @property
    def cause(self) -> CancelCause | None:
        return self._cause

    async def wait(self) -> CancelCause:
        await self._event.wait()
        return self._cause or CancelCause.USER

    def _cancel(self, cause: CancelCause) -> None:
        if self._event.is_set():
            return
        self._cause = cause
        self._event.set()


@dataclass(frozen=True, slots=True)
class ToolResult:
    """One authoritative provider-level Tool outcome."""

    kind: str
    content: tuple[JsonObject, ...] = ()
    code: str | None = None
    message: str | None = None
    cause: CancelCause | None = None

    @classmethod
    def success(cls, content: Iterable[Mapping[str, Any]] = ()) -> "ToolResult":
        return cls("success", tuple(dict(block) for block in content))

    @classmethod
    def success_text(cls, text: str) -> "ToolResult":
        return cls.success(({"type": "text", "text": text},))

    @classmethod
    def error(
        cls,
        code: str,
        message: str,
        content: Iterable[Mapping[str, Any]] = (),
    ) -> "ToolResult":
        if not code:
            raise ProviderSdkError("ToolResult.error code must not be empty")
        if not message:
            raise ProviderSdkError("ToolResult.error message must not be empty")
        return cls(
            "error",
            tuple(dict(block) for block in content),
            code=code,
            message=message,
        )

    @classmethod
    def cancelled(cls, cause: CancelCause) -> "ToolResult":
        return cls("cancelled", cause=cause)

    def to_wire(self) -> JsonObject:
        if self.kind == "success":
            return {"kind": "success", "content": list(self.content)}
        if self.kind == "error":
            return {
                "kind": "error",
                "code": self.code,
                "message": self.message,
                "content": list(self.content),
            }
        if self.kind == "cancelled":
            if self.cause is None:
                raise ProviderSdkError("cancelled ToolResult must carry a cause")
            return {"kind": "cancelled", "cause": self.cause.value}
        raise ProviderSdkError(f"unsupported ToolResult kind: {self.kind}")


@dataclass(frozen=True, slots=True)
class ToolContext:
    operation_id: str
    invocation_id: str
    call_id: str
    session_id: str
    tool: str
    arguments_json: str
    arguments: Any
    attempt: int
    idempotency_key: str
    deadline: str | None
    cancellation: CancellationToken


@dataclass(frozen=True, slots=True)
class ModelContext:
    operation_id: str
    stream_id: str
    request: JsonObject
    deadline: str | None
    cancellation: CancellationToken
    stream: "LlmStreamWriter"


@dataclass(frozen=True, slots=True)
class _ToolRegistration:
    name: str
    version: str
    parallel_safe: bool
    side_effect: SideEffect
    supports_idempotency_key: bool
    handler: ToolHandler

    def descriptor(self) -> JsonObject:
        return {
            "kind": "tool",
            "name": self.name,
            "version": self.version,
            "parallelSafe": self.parallel_safe,
            "sideEffect": self.side_effect.value,
            "supportsIdempotencyKey": self.supports_idempotency_key,
        }


@dataclass(slots=True)
class _Operation:
    token: CancellationToken
    task: asyncio.Task[None] | None = None


class LlmStreamWriter:
    """Assigns protocol stream sequence numbers and emits `llm.event` frames."""

    def __init__(self, app: "ProviderApp", stream_id: str) -> None:
        self._app = app
        self._stream_id = stream_id
        self._next_seq = 1
        self._terminal = False

    @property
    def terminal(self) -> bool:
        return self._terminal

    async def emit(self, event: Mapping[str, Any]) -> None:
        if self._terminal:
            raise StreamStateError("cannot emit an LLM event after finish")
        event_value = dict(event)
        if not isinstance(event_value.get("type"), str):
            raise StreamStateError("LLM event must contain a string type")
        seq = self._next_seq
        self._next_seq += 1
        if event_value["type"] == "finish":
            self._terminal = True
        await self._app._emit(
            {
                "jsonrpc": "2.0",
                "method": "llm.event",
                "params": {
                    "streamId": self._stream_id,
                    "seq": seq,
                    "event": event_value,
                },
            }
        )

    async def text(self, text: str, *, index: int = 0) -> None:
        await self.emit({"type": "block-start", "index": index, "blockType": "text"})
        await self.emit({"type": "text-delta", "index": index, "text": text})
        await self.emit(
            {
                "type": "block-end",
                "index": index,
                "block": {"type": "text", "text": text},
            }
        )

    async def reasoning(self, text: str, *, index: int = 0) -> None:
        await self.emit(
            {"type": "block-start", "index": index, "blockType": "reasoning"}
        )
        await self.emit({"type": "reasoning-delta", "index": index, "text": text})
        await self.emit(
            {
                "type": "block-end",
                "index": index,
                "block": {"type": "reasoning", "text": text},
            }
        )

    async def tool_call(
        self,
        call_id: str,
        name: str,
        arguments: Any,
        *,
        index: int = 0,
    ) -> None:
        if not call_id or not name:
            raise StreamStateError("tool_call requires non-empty call_id and name")
        arguments_json = (
            arguments
            if isinstance(arguments, str)
            else json.dumps(arguments, separators=(",", ":"), ensure_ascii=False)
        )
        try:
            json.loads(arguments_json)
        except json.JSONDecodeError as exc:
            raise StreamStateError(f"tool_call arguments are not valid JSON: {exc}") from exc

        await self.emit(
            {"type": "block-start", "index": index, "blockType": "tool-call"}
        )
        await self.emit(
            {
                "type": "tool-call-delta",
                "index": index,
                "callId": call_id,
                "name": name,
                "argumentsDelta": arguments_json,
            }
        )
        await self.emit(
            {
                "type": "block-end",
                "index": index,
                "block": {
                    "type": "tool-call",
                    "id": call_id,
                    "name": name,
                    "argumentsJson": arguments_json,
                },
            }
        )

    async def usage(
        self,
        input_tokens: int,
        output_tokens: int,
        **extensions: Any,
    ) -> None:
        usage: JsonObject = {
            "inputTokens": input_tokens,
            "outputTokens": output_tokens,
            **extensions,
        }
        await self.emit({"type": "usage", "usage": usage})

    async def finish(self, reason: str = "completed", failure: JsonObject | None = None) -> None:
        event: JsonObject = {"type": "finish", "reason": reason}
        if failure is not None:
            event["failure"] = failure
        await self.emit(event)

    async def finish_error(
        self,
        code: str,
        message: str,
        *,
        details: Mapping[str, Any] | None = None,
    ) -> None:
        failure: JsonObject = {"code": code, "message": message}
        if details:
            failure["details"] = dict(details)
        await self.finish("error", failure)

    async def finish_cancelled(self, cause: CancelCause) -> None:
        await self.finish(
            "cancelled",
            {
                "code": "CANCELLED",
                "message": f"provider operation cancelled: {cause.value}",
                "details": {"cause": cause.value},
            },
        )


class ProviderApp:
    """Provider Protocol v1 application/runtime."""

    def __init__(self, provider_id: str, provider_version: str) -> None:
        if not provider_id.strip():
            raise RegistrationError("provider_id must not be empty")
        if not provider_version.strip():
            raise RegistrationError("provider_version must not be empty")
        self.provider_id = provider_id
        self.provider_version = provider_version
        self._tools: dict[str, _ToolRegistration] = {}
        self._models: dict[str, ModelHandler] = {}
        self._operations: dict[str, _Operation] = {}
        self._write_lock: asyncio.Lock | None = None
        self._running = False

    def tool(
        self,
        *,
        name: str,
        version: str,
        parallel_safe: bool,
        side_effect: SideEffect,
        supports_idempotency_key: bool = False,
    ) -> Callable[[ToolHandler], ToolHandler]:
        if not name.strip():
            raise RegistrationError("tool name must not be empty")
        if not version.strip():
            raise RegistrationError(f"tool {name} version must not be empty")
        if (
            side_effect is SideEffect.IDEMPOTENT_WRITE
            and not supports_idempotency_key
        ):
            raise RegistrationError(
                f"idempotent-write tool {name} must support idempotency keys"
            )

        def decorator(handler: ToolHandler) -> ToolHandler:
            if name in self._tools:
                raise RegistrationError(f"tool {name} is already registered")
            self._tools[name] = _ToolRegistration(
                name=name,
                version=version,
                parallel_safe=parallel_safe,
                side_effect=side_effect,
                supports_idempotency_key=supports_idempotency_key,
                handler=handler,
            )
            return handler

        return decorator

    def model(self, name: str) -> Callable[[ModelHandler], ModelHandler]:
        if not name.strip():
            raise RegistrationError("model name must not be empty")

        def decorator(handler: ModelHandler) -> ModelHandler:
            if name in self._models:
                raise RegistrationError(f"model {name} is already registered")
            if not inspect.iscoroutinefunction(handler):
                raise RegistrationError(
                    f"model {name} handler must be declared with async def"
                )
            self._models[name] = handler
            return handler

        return decorator

    def manifest(self) -> JsonObject:
        capabilities = [tool.descriptor() for tool in self._tools.values()]
        if self._models:
            capabilities.append({"kind": "llm", "models": list(self._models)})
        return {
            "providerId": self.provider_id,
            "providerVersion": self.provider_version,
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": capabilities,
        }

    def run(self) -> None:
        asyncio.run(self.serve())

    async def serve(self) -> None:
        if self._running:
            raise ProviderSdkError("ProviderApp is already running")
        self._running = True
        self._write_lock = asyncio.Lock()
        self.log(
            f"provider {self.provider_id} {self.provider_version} started with SDK protocol {PROTOCOL_VERSION}"
        )
        try:
            while self._running:
                line = await asyncio.to_thread(sys.stdin.readline)
                if line == "":
                    break
                await self._dispatch_line(line)
        finally:
            await self._cancel_all(CancelCause.SHUTDOWN)
            self._running = False

    def log(self, message: str) -> None:
        print(message, file=sys.stderr, flush=True)

    async def _dispatch_line(self, line: str) -> None:
        try:
            message = json.loads(line)
        except json.JSONDecodeError as exc:
            self.log(f"invalid stdin JSON: {exc}")
            return
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0":
            self.log("invalid JSON-RPC envelope")
            return
        if "id" in message:
            await self._handle_request(message)
        else:
            await self._handle_notification(message)

    async def _handle_request(self, message: JsonObject) -> None:
        rpc_id = message.get("id")
        method = message.get("method")
        params = message.get("params") or {}
        if not isinstance(rpc_id, str) or not rpc_id:
            self.log("request id must be a non-empty string")
            return
        if not isinstance(params, dict):
            await self._failure(rpc_id, -32602, "params must be an object")
            return

        if method == "provider.initialize":
            if params.get("protocolVersion") != PROTOCOL_VERSION:
                await self._failure(rpc_id, -32602, "unsupported protocolVersion")
            else:
                await self._success(rpc_id, self.manifest())
            return
        if method == "provider.ping":
            await self._success(rpc_id, {"ok": True})
            return
        if method == "provider.shutdown":
            await self._success(rpc_id, {"accepted": True})
            self._running = False
            return
        if method == "tool.invoke":
            await self._start_tool(rpc_id, params)
            return
        if method == "llm.start":
            await self._start_model(rpc_id, params)
            return
        await self._failure(rpc_id, -32601, f"method not found: {method}")

    async def _handle_notification(self, message: JsonObject) -> None:
        method = message.get("method")
        params = message.get("params") or {}
        if method != "capability.cancel" or not isinstance(params, dict):
            self.log(f"unsupported notification: {method}")
            return
        operation_id = params.get("operationId")
        cause_value = (params.get("cause") or {}).get("kind")
        if not isinstance(operation_id, str) or not operation_id:
            self.log("capability.cancel missing operationId")
            return
        try:
            cause = CancelCause(cause_value)
        except (TypeError, ValueError):
            self.log(f"capability.cancel has invalid cause: {cause_value!r}")
            return
        operation = self._operations.get(operation_id)
        if operation is None:
            return
        operation.token._cancel(cause)
        if operation.task is not None:
            operation.task.cancel()

    async def _start_tool(self, rpc_id: str, params: JsonObject) -> None:
        try:
            operation_id = _required_string(params, "operationId")
            invocation_id = _required_string(params, "invocationId")
            call_id = _required_string(params, "callId")
            session_id = _required_string(params, "sessionId")
            tool = _required_string(params, "tool")
            arguments_json = _required_string(params, "argumentsJson")
            idempotency_key = _required_string(params, "idempotencyKey")
            attempt = params.get("attempt")
            if operation_id != invocation_id:
                raise ValueError("operationId must equal invocationId")
            if not isinstance(attempt, int) or isinstance(attempt, bool) or attempt <= 0:
                raise ValueError("attempt must be a positive integer")
            arguments = json.loads(arguments_json)
            deadline = _optional_string(params, "deadline")
        except (ValueError, json.JSONDecodeError) as exc:
            await self._failure(rpc_id, -32602, str(exc))
            return

        registration = self._tools.get(tool)
        if registration is None:
            await self._success(
                rpc_id,
                {
                    "outcome": ToolResult.error(
                        "NOT_FOUND", f"unknown tool: {tool}"
                    ).to_wire()
                },
            )
            return
        if operation_id in self._operations:
            await self._failure(rpc_id, -32602, "operationId is already active")
            return

        token = CancellationToken()
        operation = _Operation(token=token)
        self._operations[operation_id] = operation
        context = ToolContext(
            operation_id=operation_id,
            invocation_id=invocation_id,
            call_id=call_id,
            session_id=session_id,
            tool=tool,
            arguments_json=arguments_json,
            arguments=arguments,
            attempt=attempt,
            idempotency_key=idempotency_key,
            deadline=deadline,
            cancellation=token,
        )
        task = asyncio.create_task(
            self._run_tool_request(rpc_id, registration.handler, context)
        )
        operation.task = task

    async def _run_tool_request(
        self,
        rpc_id: str,
        handler: ToolHandler,
        context: ToolContext,
    ) -> None:
        try:
            if inspect.iscoroutinefunction(handler):
                result = await handler(context)
            else:
                result = await asyncio.to_thread(handler, context)
            if not isinstance(result, ToolResult):
                raise ProviderSdkError(
                    "Tool handler must return harness_provider_sdk.ToolResult"
                )
            await self._success(rpc_id, {"outcome": result.to_wire()})
        except asyncio.CancelledError:
            cause = context.cancellation.cause or CancelCause.USER
            await asyncio.shield(
                self._success(
                    rpc_id,
                    {"outcome": ToolResult.cancelled(cause).to_wire()},
                )
            )
        except Exception as exc:  # noqa: BLE001 - provider boundary normalization
            self.log(f"tool {context.tool} failed: {exc}")
            await self._success(
                rpc_id,
                {
                    "outcome": ToolResult.error(
                        "INTERNAL", f"tool handler failed: {exc}"
                    ).to_wire()
                },
            )
        finally:
            self._operations.pop(context.operation_id, None)

    async def _start_model(self, rpc_id: str, params: JsonObject) -> None:
        try:
            operation_id = _required_string(params, "operationId")
            stream_id = _required_string(params, "streamId")
            request = params.get("request")
            if not isinstance(request, dict):
                raise ValueError("request must be an object")
            request_id = _required_string(request, "requestId")
            model = _required_string(request, "model")
            provider = _required_string(request, "provider")
            if operation_id != request_id:
                raise ValueError("operationId must equal request.requestId")
            if provider != self.provider_id:
                raise ValueError(
                    f"request provider {provider!r} does not match {self.provider_id!r}"
                )
            deadline = _optional_string(params, "deadline")
        except ValueError as exc:
            await self._failure(rpc_id, -32602, str(exc))
            return

        handler = self._models.get(model)
        if handler is None:
            await self._success(
                rpc_id,
                {
                    "accepted": False,
                    "streamId": stream_id,
                    "reason": f"unknown model: {model}",
                },
            )
            return
        if operation_id in self._operations:
            await self._failure(rpc_id, -32602, "operationId is already active")
            return

        token = CancellationToken()
        writer = LlmStreamWriter(self, stream_id)
        operation = _Operation(token=token)
        self._operations[operation_id] = operation
        context = ModelContext(
            operation_id=operation_id,
            stream_id=stream_id,
            request=dict(request),
            deadline=deadline,
            cancellation=token,
            stream=writer,
        )

        # Host installs stream routing before the request and the SDK flushes this
        # response before the model task is scheduled, preserving the protocol's
        # start-response-before-first-event rule.
        await self._success(rpc_id, {"accepted": True, "streamId": stream_id})
        task = asyncio.create_task(self._run_model(handler, context))
        operation.task = task

    async def _run_model(self, handler: ModelHandler, context: ModelContext) -> None:
        try:
            await handler(context)
            if not context.stream.terminal:
                await context.stream.finish("completed")
        except asyncio.CancelledError:
            cause = context.cancellation.cause or CancelCause.USER
            if not context.stream.terminal:
                await asyncio.shield(context.stream.finish_cancelled(cause))
        except Exception as exc:  # noqa: BLE001 - provider boundary normalization
            self.log(f"model handler failed: {exc}")
            if not context.stream.terminal:
                await context.stream.finish_error(
                    "INTERNAL", f"model handler failed: {exc}"
                )
        finally:
            self._operations.pop(context.operation_id, None)

    async def _cancel_all(self, cause: CancelCause) -> None:
        operations = list(self._operations.values())
        for operation in operations:
            operation.token._cancel(cause)
            if operation.task is not None:
                operation.task.cancel()
        if operations:
            await asyncio.gather(
                *(op.task for op in operations if op.task is not None),
                return_exceptions=True,
            )

    async def _success(self, rpc_id: str, result: Any) -> None:
        await self._emit({"jsonrpc": "2.0", "id": rpc_id, "result": result})

    async def _failure(
        self,
        rpc_id: str,
        code: int,
        message: str,
        data: Any | None = None,
    ) -> None:
        error: JsonObject = {"code": code, "message": message}
        if data is not None:
            error["data"] = data
        await self._emit({"jsonrpc": "2.0", "id": rpc_id, "error": error})

    async def _emit(self, message: Mapping[str, Any]) -> None:
        if self._write_lock is None:
            raise ProviderSdkError("ProviderApp output is unavailable before serve()")
        frame = json.dumps(
            dict(message), separators=(",", ":"), ensure_ascii=False
        ) + "\n"
        async with self._write_lock:
            sys.stdout.write(frame)
            sys.stdout.flush()


def last_text(request: Mapping[str, Any]) -> str:
    """Return the latest text block from a provider-neutral ModelRequest."""

    messages = request.get("messages", [])
    if not isinstance(messages, list):
        return ""
    for message in reversed(messages):
        if not isinstance(message, dict):
            continue
        content = message.get("content", [])
        if not isinstance(content, list):
            continue
        for block in reversed(content):
            if isinstance(block, dict) and block.get("type") == "text":
                return str(block.get("text", ""))
    return ""


def trailing_tool_result_text(request: Mapping[str, Any]) -> str | None:
    """Return text from the final message's latest tool-result block, if any."""

    messages = request.get("messages", [])
    if not isinstance(messages, list) or not messages:
        return None
    last_message = messages[-1]
    if not isinstance(last_message, dict):
        return None
    content = last_message.get("content", [])
    if not isinstance(content, list):
        return None
    for block in reversed(content):
        if not isinstance(block, dict) or block.get("type") != "tool-result":
            continue
        nested = block.get("content", [])
        if isinstance(nested, list):
            for item in nested:
                if isinstance(item, dict) and item.get("type") == "text":
                    return str(item.get("text", ""))
        return ""
    return None


def _required_string(value: Mapping[str, Any], field: str) -> str:
    candidate = value.get(field)
    if not isinstance(candidate, str) or not candidate:
        raise ValueError(f"{field} must be a non-empty string")
    return candidate


def _optional_string(value: Mapping[str, Any], field: str) -> str | None:
    candidate = value.get(field)
    if candidate is None:
        return None
    if not isinstance(candidate, str) or not candidate:
        raise ValueError(f"{field} must be a non-empty string when present")
    return candidate
