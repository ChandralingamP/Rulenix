from __future__ import annotations

import asyncio
import json
import os
import subprocess
from pathlib import Path

import pytest
from sqlalchemy.ext.asyncio import create_async_engine

from app.parity.state_adapter import execute_state_case

CASES = (
    "strategy_signal",
    "execution_intent",
    "order_transition",
    "partial_fill",
    "reversal_intent",
    "manual_close_intent",
    "kill_switch",
    "readiness",
)
ROOT = Path(__file__).parents[1]
RUST_BINARY = ROOT.parent / "backend" / "target" / "debug" / "rulenix-backend.exe"


def _database_url() -> str:
    value = os.environ.get("TEST_DATABASE_URL", "")
    if "/rulenix_test" not in value:
        pytest.skip("TEST_DATABASE_URL must point to an isolated PostgreSQL test database")
    return value


@pytest.mark.asyncio
async def test_rust_python_postgres_state_before_after_differential() -> None:
    database_url = _database_url()
    if not RUST_BINARY.exists():
        raise AssertionError("build the phase10 adapter before running database differential tests")
    async_url = database_url.replace("postgresql://", "postgresql+asyncpg://", 1).replace(
        "postgres://", "postgresql+asyncpg://", 1
    )
    engine = create_async_engine(async_url, pool_size=2, max_overflow=0)
    try:
        for case in CASES:
            envelope = {"fixture_id": case, "category": "Database state", "operation": "db_state", "request": {"case": case}}
            completed = await asyncio.to_thread(
                subprocess.run,
                [str(RUST_BINARY), "--phase10-fixture-adapter"],
                input=json.dumps(envelope),
                text=True,
                capture_output=True,
                check=True,
                env={**os.environ, "TEST_DATABASE_URL": database_url},
            )
            rust = json.loads(completed.stdout)["body"]
            python = await execute_state_case(engine, case)
            assert rust == python, case
    finally:
        await engine.dispose()
