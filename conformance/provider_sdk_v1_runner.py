#!/usr/bin/env python3
"""Language-neutral process runner for Provider SDK Conformance Contract v1.

The runner itself is Python for portability, but it treats the provider as an
opaque subprocess. Any SDK/runtime that can launch a process speaking Provider
Protocol v1 can be tested against the same JSON golden fixtures.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import subprocess
import sys
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping, Sequence

JsonObject = dict[str, Any]
_EOF = object()


class ConformanceFailure(RuntimeError):
    pass


@dataclass(frozen=True)
class Fixture:
    name: str
    description: str
    steps: tuple[JsonObject, ...]
    path: Path


class ProviderProcess:
    def __init__(
        self,
        command: Sequence[str],
        *,
        timeout: float,
        env: Mapping[str, str] | None = None,
    ) -> None:
        if not command:
            raise ConformanceFailure("provider command must not be empty")
        self.timeout = timeout
        self.process = subprocess.Popen(
            list(command),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            env=dict(env) if env is not None else None,
        )
        if self.process.stdin is None or self.process.stdout is None or self.process.stderr is None:
            self.process.kill()
            raise ConformanceFailure("provider process did not expose stdio pipes")
        self.stdin = self.process.stdin
        self.stdout = self.process.stdout
        self.stderr = self.process.stderr
        self.stdout_queue: queue.Queue[object] = queue.Queue()
        self.stderr_lines: list[str] = []
        self.stdout_thread = threading.Thread(target=self._read_stdout, daemon=True)
        self.stderr_thread = threading.Thread(target=self._read_stderr, daemon=True)
        self.stdout_thread.start()
        self.stderr_thread.start()

    def _read_stdout(self) -> None:
        try:
            for line in self.stdout:
                self.stdout_queue.put(line)
        finally:
            self.stdout_queue.put(_EOF)

    def _read_stderr(self) -> None:
        for line in self.stderr:
            self.stderr_lines.append(line.rstrip("\n"))

    def send(self, message: Mapping[str, Any]) -> None:
        if self.process.poll() is not None:
            raise self.failure("provider exited before send")
        frame = json.dumps(dict(message), separators=(",", ":"), ensure_ascii=False)
        try:
            self.stdin.write(frame + "\n")
            self.stdin.flush()
        except (BrokenPipeError, OSError) as exc:
            raise self.failure(f"failed to write provider stdin: {exc}") from exc

    def receive(self) -> JsonObject:
        try:
            item = self.stdout_queue.get(timeout=self.timeout)
        except queue.Empty as exc:
            raise self.failure(f"timed out after {self.timeout:g}s waiting for provider stdout") from exc
        if item is _EOF:
            raise self.failure("provider stdout reached EOF")
        assert isinstance(item, str)
        try:
            value = json.loads(item)
        except json.JSONDecodeError as exc:
            raise self.failure(f"provider emitted invalid JSON: {item!r}") from exc
        if not isinstance(value, dict) or value.get("jsonrpc") != "2.0":
            raise self.failure(f"provider emitted invalid JSON-RPC envelope: {value!r}")
        return value

    def expect(self, expected: Mapping[str, Any], *, label: str) -> None:
        actual = self.receive()
        expected_value = dict(expected)
        if actual != expected_value:
            expected_json = json.dumps(expected_value, indent=2, sort_keys=True, ensure_ascii=False)
            actual_json = json.dumps(actual, indent=2, sort_keys=True, ensure_ascii=False)
            raise self.failure(
                f"{label}: golden output mismatch\nEXPECTED:\n{expected_json}\nACTUAL:\n{actual_json}"
            )

    def shutdown_and_verify(self, shutdown_id: str) -> None:
        self.send(
            {
                "jsonrpc": "2.0",
                "id": shutdown_id,
                "method": "provider.shutdown",
                "params": {},
            }
        )
        self.expect(
            {
                "jsonrpc": "2.0",
                "id": shutdown_id,
                "result": {"accepted": True},
            },
            label="provider.shutdown",
        )
        try:
            self.stdin.close()
        except OSError:
            pass
        try:
            return_code = self.process.wait(timeout=self.timeout)
        except subprocess.TimeoutExpired as exc:
            self.process.kill()
            self.process.wait(timeout=self.timeout)
            raise self.failure("provider did not exit after provider.shutdown") from exc
        self.stdout_thread.join(timeout=self.timeout)
        self.stderr_thread.join(timeout=self.timeout)
        if return_code != 0:
            raise self.failure(f"provider exited with status {return_code}")

        trailing: list[str] = []
        while True:
            try:
                item = self.stdout_queue.get_nowait()
            except queue.Empty:
                break
            if item is _EOF:
                continue
            assert isinstance(item, str)
            trailing.append(item.rstrip("\n"))
        if trailing:
            raise self.failure(
                "provider emitted protocol frames after shutdown acknowledgement: "
                + " | ".join(trailing)
            )

    def force_close(self) -> None:
        if self.process.poll() is None:
            self.process.kill()
            try:
                self.process.wait(timeout=self.timeout)
            except subprocess.TimeoutExpired:
                pass
        for stream in (self.stdin, self.stdout, self.stderr):
            try:
                stream.close()
            except OSError:
                pass

    def failure(self, message: str) -> ConformanceFailure:
        stderr = "\n".join(self.stderr_lines[-20:])
        suffix = f"\nprovider stderr:\n{stderr}" if stderr else ""
        return ConformanceFailure(message + suffix)


def load_contract(path: Path) -> JsonObject:
    value = load_json_object(path)
    if value.get("suiteVersion") != "1.0":
        raise ConformanceFailure(f"{path}: unsupported suiteVersion")
    if value.get("protocolVersion") != "1.0":
        raise ConformanceFailure(f"{path}: protocolVersion must be 1.0")
    runtime = value.get("runtime")
    manifest = value.get("manifest")
    if not isinstance(runtime, dict) or not isinstance(manifest, dict):
        raise ConformanceFailure(f"{path}: runtime and manifest must be objects")
    return value


def load_fixture(path: Path) -> Fixture:
    value = load_json_object(path)
    if value.get("schemaVersion") != 1:
        raise ConformanceFailure(f"{path}: schemaVersion must be 1")
    name = value.get("name")
    description = value.get("description", "")
    steps = value.get("steps")
    if not isinstance(name, str) or not name:
        raise ConformanceFailure(f"{path}: name must be a non-empty string")
    if not isinstance(description, str):
        raise ConformanceFailure(f"{path}: description must be a string")
    if not isinstance(steps, list) or not steps:
        raise ConformanceFailure(f"{path}: steps must be a non-empty array")

    normalized: list[JsonObject] = []
    for index, step in enumerate(steps, start=1):
        if not isinstance(step, dict):
            raise ConformanceFailure(f"{path}: step {index} must be an object")
        keys = set(step)
        if keys not in ({"send"}, {"expect"}):
            raise ConformanceFailure(
                f"{path}: step {index} must contain exactly one of send/expect"
            )
        payload = step.get("send", step.get("expect"))
        if not isinstance(payload, dict):
            raise ConformanceFailure(f"{path}: step {index} payload must be an object")
        normalized.append(dict(step))
    return Fixture(name, description, tuple(normalized), path)


def load_json_object(path: Path) -> JsonObject:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ConformanceFailure(f"cannot load {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise ConformanceFailure(f"{path}: root must be a JSON object")
    return value


def run_fixture(
    fixture: Fixture,
    contract: Mapping[str, Any],
    command: Sequence[str],
    *,
    timeout: float,
    env: Mapping[str, str] | None,
) -> None:
    provider = ProviderProcess(command, timeout=timeout, env=env)
    init_id = f"rpc_conformance_init_{fixture.name}"
    shutdown_id = f"rpc_conformance_shutdown_{fixture.name}"
    try:
        provider.send(
            {
                "jsonrpc": "2.0",
                "id": init_id,
                "method": "provider.initialize",
                "params": {
                    "protocolVersion": contract["protocolVersion"],
                    "runtime": contract["runtime"],
                },
            }
        )
        provider.expect(
            {
                "jsonrpc": "2.0",
                "id": init_id,
                "result": contract["manifest"],
            },
            label=f"{fixture.name}: provider.initialize",
        )

        for index, step in enumerate(fixture.steps, start=1):
            if "send" in step:
                provider.send(step["send"])
            else:
                provider.expect(step["expect"], label=f"{fixture.name}: step {index}")

        provider.shutdown_and_verify(shutdown_id)
    finally:
        provider.force_close()


def run_suite(
    command: Sequence[str],
    *,
    contract_path: Path,
    fixtures_dir: Path,
    timeout: float = 5.0,
    scenarios: set[str] | None = None,
    env: Mapping[str, str] | None = None,
) -> int:
    if timeout <= 0:
        raise ConformanceFailure("timeout must be greater than zero")
    contract = load_contract(contract_path)
    fixture_paths = sorted(fixtures_dir.glob("*.json"))
    fixtures = [load_fixture(path) for path in fixture_paths]
    if scenarios:
        fixtures = [fixture for fixture in fixtures if fixture.name in scenarios]
        missing = scenarios - {fixture.name for fixture in fixtures}
        if missing:
            raise ConformanceFailure(
                "unknown scenario(s): " + ", ".join(sorted(missing))
            )
    if not fixtures:
        raise ConformanceFailure(f"no conformance fixtures found in {fixtures_dir}")

    for fixture in fixtures:
        run_fixture(
            fixture,
            contract,
            command,
            timeout=timeout,
            env=env,
        )
        print(f"PASS {fixture.name}")
    print(f"Provider SDK Conformance v1 passed: {len(fixtures)} scenario(s)")
    return 0


def main() -> int:
    base = Path(__file__).resolve().parent / "provider-sdk-v1"
    parser = argparse.ArgumentParser()
    parser.add_argument("--contract", type=Path, default=base / "contract.json")
    parser.add_argument("--fixtures", type=Path, default=base / "fixtures")
    parser.add_argument("--timeout", type=float, default=5.0)
    parser.add_argument("--scenario", action="append", default=[])
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = list(args.command)
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        parser.error("provider command is required after --")

    try:
        return run_suite(
            command,
            contract_path=args.contract,
            fixtures_dir=args.fixtures,
            timeout=args.timeout,
            scenarios=set(args.scenario) or None,
            env=os.environ,
        )
    except (ConformanceFailure, OSError) as exc:
        print(f"Provider SDK Conformance v1 failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
