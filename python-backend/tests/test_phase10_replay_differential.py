from __future__ import annotations

import asyncio
import json
import os
import subprocess
from pathlib import Path

import pytest
from sqlalchemy.ext.asyncio import create_async_engine

from app.parity.state_adapter import execute_replay_case

ROOT = Path(__file__).parents[1]
RUST_BINARY = ROOT.parent / "backend" / "target" / "debug" / "rulenix-backend.exe"
CASES = (
    ("eod_boundary", "2026-09-09T15:09:59+05:30"),
    ("eod_boundary", "2026-09-09T15:10:00+05:30"),
    ("eod_boundary", "2026-09-09T15:10:01+05:30"),
    ("eod_restart", "2026-09-09T15:10:01+05:30"),
    ("stale_execution_claim", None),
    ("sl2_reversal", None),
    ("manual_close", None),
    ("eod_crash", None),
    ("protection_recovery", None),
)


def _database_url() -> str:
    value = os.environ.get("TEST_DATABASE_URL", "")
    if "/rulenix_test" not in value:
        pytest.skip("TEST_DATABASE_URL must point to an isolated PostgreSQL test database")
    return value


@pytest.mark.asyncio
async def test_eod_and_crash_restart_durable_state_match_across_runtimes() -> None:
    database_url = _database_url()
    if not RUST_BINARY.exists():
        raise AssertionError("build the phase10 adapter before running replay differential tests")
    engine = create_async_engine(database_url, pool_size=4, max_overflow=0)
    try:
        for index, (case, at) in enumerate(CASES):
            request = {"case": case}
            if at is not None:
                request["at"] = at
            envelope = {"fixture_id": f"{case}-{index}", "category": "Replay", "operation": "replay_state", "request": request}
            completed = await asyncio.to_thread(
                subprocess.run,
                [str(RUST_BINARY), "--phase10-fixture-adapter"],
                input=json.dumps(envelope),
                text=True,
                capture_output=True,
                check=True,
                env={**os.environ, "TEST_DATABASE_URL": database_url},
                timeout=20,
            )
            rust = json.loads(completed.stdout)["body"]
            python = await execute_replay_case(engine, case, at)
            assert rust == python, (case, at, rust, python)
    finally:
        await engine.dispose()
