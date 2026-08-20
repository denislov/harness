#!/usr/bin/env python3
from __future__ import annotations

import json
from pathlib import Path

MATRIX = Path(__file__).with_name("crash-matrix-v1.json")
REQUIRED_POST_COMMIT_CUTS = {
    "user/message",
    "model/requested",
    "assistant/message",
    "tool/call",
    "tool/dispatched",
    "tool/result",
    "step/ended",
    "turn/ended",
}
REQUIRED_KINDS = {
    "agent-post-commit",
    "approval-post-commit",
    "provider-fault",
    "storage-reopen",
    "composition-drift",
}


def main() -> int:
    data = json.loads(MATRIX.read_text(encoding="utf-8"))
    if data.get("schemaVersion") != 1:
        raise SystemExit("crash matrix schemaVersion must equal 1")
    if data.get("suite") != "harness-crash-fault-v1":
        raise SystemExit("unexpected crash matrix suite id")

    cases = data.get("cases")
    if not isinstance(cases, list) or not cases:
        raise SystemExit("crash matrix cases must be a non-empty list")

    ids: set[str] = set()
    kinds: set[str] = set()
    post_commit_cuts: set[str] = set()
    for index, case in enumerate(cases):
        if not isinstance(case, dict):
            raise SystemExit(f"case {index} must be an object")
        missing = {
            key
            for key in ("id", "kind", "cut", "atomicBatch", "expect", "test")
            if key not in case
        }
        if missing:
            raise SystemExit(f"case {index} missing fields: {sorted(missing)}")
        case_id = case["id"]
        if not isinstance(case_id, str) or not case_id:
            raise SystemExit(f"case {index} has invalid id")
        if case_id in ids:
            raise SystemExit(f"duplicate crash matrix case id: {case_id}")
        ids.add(case_id)
        kinds.add(case["kind"])
        if case["kind"] == "agent-post-commit":
            post_commit_cuts.add(case["cut"])
        if not isinstance(case["atomicBatch"], list) or not case["atomicBatch"]:
            raise SystemExit(f"case {case_id} atomicBatch must be non-empty")
        if not isinstance(case["test"], str) or not case["test"].startswith("cargo test "):
            raise SystemExit(f"case {case_id} must name its cargo test command")

    missing_kinds = REQUIRED_KINDS - kinds
    if missing_kinds:
        raise SystemExit(f"crash matrix missing kinds: {sorted(missing_kinds)}")
    missing_cuts = REQUIRED_POST_COMMIT_CUTS - post_commit_cuts
    if missing_cuts:
        raise SystemExit(
            f"crash matrix missing required post-commit cuts: {sorted(missing_cuts)}"
        )

    print(f"crash matrix OK: {len(cases)} cases")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
