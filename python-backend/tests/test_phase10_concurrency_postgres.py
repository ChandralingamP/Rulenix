from __future__ import annotations

import asyncio
import os
from uuid import uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import create_async_engine


def _url() -> str | None:
    value = os.environ.get("TEST_DATABASE_URL")
    if not value:
        return None
    return value.replace("postgresql://", "postgresql+asyncpg://", 1).replace("postgres://", "postgresql+asyncpg://", 1)


def _require_url() -> str:
    value = _url()
    if not value:
        pytest.skip("TEST_DATABASE_URL must point to an isolated PostgreSQL database")
    if "/rulenix_test" not in value:
        pytest.fail("Refusing to run Phase 10 concurrency tests against a non-test database")
    return value


@pytest.mark.asyncio
async def test_multi_instance_advisory_leadership_and_failover():
    engine = create_async_engine(_require_url(), pool_size=4, max_overflow=0)
    first = await engine.connect()
    second = await engine.connect()
    role = f"rulenix:phase10:{uuid4()}"
    try:
        assert await first.scalar(text("SELECT pg_try_advisory_lock(hashtext(:role))"), {"role": role})
        assert not await second.scalar(text("SELECT pg_try_advisory_lock(hashtext(:role))"), {"role": role})
        await first.invalidate()
        assert await second.scalar(text("SELECT pg_try_advisory_lock(hashtext(:role))"), {"role": role})
        await second.execute(text("SELECT pg_advisory_unlock(hashtext(:role))"), {"role": role})
    finally:
        await first.close()
        await second.close()
        await engine.dispose()


@pytest.mark.asyncio
async def test_real_postgres_skip_locked_claims_each_item_once():
    engine = create_async_engine(_require_url(), pool_size=8, max_overflow=0)
    table = f"phase10_claims_{uuid4().hex[:12]}"
    total = 32
    try:
        async with engine.begin() as connection:
            await connection.execute(text(f"CREATE TABLE {table} (id INTEGER PRIMARY KEY, status TEXT NOT NULL DEFAULT 'pending', claimed_by TEXT)"))
            for item in range(total):
                await connection.execute(text(f"INSERT INTO {table}(id) VALUES(:id)"), {"id": item})

        async def worker(name: str) -> list[int]:
            claimed: list[int] = []
            while True:
                async with engine.connect() as connection, connection.begin():
                        row = (await connection.execute(text(f"SELECT id FROM {table} WHERE status='pending' ORDER BY id FOR UPDATE SKIP LOCKED LIMIT 1"))).first()
                        if row is None:
                            return claimed
                        item = int(row[0])
                        await connection.execute(text(f"UPDATE {table} SET status='done',claimed_by=:worker WHERE id=:id"), {"worker": name, "id": item})
                        claimed.append(item)

        results = await asyncio.gather(*(worker(f"worker-{index}") for index in range(4)))
        flattened = [item for result in results for item in result]
        assert len(flattened) == total and len(set(flattened)) == total
        async with engine.connect() as connection:
            assert await connection.scalar(text(f"SELECT COUNT(*) FROM {table} WHERE status='done'")) == total
    finally:
        async with engine.begin() as connection:
            await connection.execute(text(f"DROP TABLE IF EXISTS {table}"))
        await engine.dispose()


@pytest.mark.asyncio
async def test_claim_then_crash_style_stale_recovery_is_reclaimable():
    engine = create_async_engine(_require_url(), pool_size=4, max_overflow=0)
    table = f"phase10_recovery_{uuid4().hex[:12]}"
    try:
        async with engine.begin() as connection:
            await connection.execute(text(f"CREATE TABLE {table} (id INTEGER PRIMARY KEY, status TEXT NOT NULL, claimed_at TIMESTAMPTZ, attempts INTEGER NOT NULL DEFAULT 0)"))
            await connection.execute(text(f"INSERT INTO {table}(id,status,claimed_at,attempts) VALUES(1,'claimed',NOW()-INTERVAL '10 minutes',1)"))
        async with engine.begin() as connection:
            recovered = await connection.execute(text(f"UPDATE {table} SET status='pending',claimed_at=NULL WHERE status='claimed' AND claimed_at<NOW()-INTERVAL '120 seconds'"))
            assert recovered.rowcount == 1
        async with engine.begin() as connection:
            claimed = await connection.execute(text(f"UPDATE {table} SET status='claimed',claimed_at=NOW(),attempts=attempts+1 WHERE id=1 AND status='pending'"))
            assert claimed.rowcount == 1
            row = (await connection.execute(text(f"SELECT status,attempts FROM {table} WHERE id=1"))).one()
            assert row.status == "claimed" and row.attempts == 2
    finally:
        async with engine.begin() as connection:
            await connection.execute(text(f"DROP TABLE IF EXISTS {table}"))
        await engine.dispose()
