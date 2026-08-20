#!/usr/bin/env python3
"""Run Provider SDK Conformance v1 against the in-tree Python SDK."""

from __future__ import annotations

import os
import sys
from pathlib import Path

from provider_sdk_v1_runner import ConformanceFailure, run_suite


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    sdk = root / "sdk" / "python"
    provider = root / "conformance" / "providers" / "python_sdk_v1.py"
    contract = root / "conformance" / "provider-sdk-v1" / "contract.json"
    fixtures = root / "conformance" / "provider-sdk-v1" / "fixtures"

    env = os.environ.copy()
    previous = env.get("PYTHONPATH")
    env["PYTHONPATH"] = str(sdk) if not previous else str(sdk) + os.pathsep + previous
    try:
        return run_suite(
            [sys.executable, str(provider)],
            contract_path=contract,
            fixtures_dir=fixtures,
            timeout=5.0,
            env=env,
        )
    except (ConformanceFailure, OSError) as exc:
        print(f"Python SDK conformance failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
