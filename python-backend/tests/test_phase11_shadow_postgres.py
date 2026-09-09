from __future__ import annotations

import os
from datetime import UTC, datetime

import asyncpg
import pytest

from app.shadow.database import ShadowWriter, SourceReader, _sink_connection, _source_connection
from app.shadow.models import Observation


def _database_url() -> str:
    value = os.environ.get("TEST_DATABASE_URL", "")
    if "/rulenix_test" not in value:
        pytest.skip("TEST_DATABASE_URL must point to an isolated PostgreSQL test database")
    if os.environ.get("PHASE11_SHADOW_ROLES_READY") != "true":
        pytest.skip("Phase 11 shadow roles must be provisioned explicitly for permission proof")
    return value.replace("postgresql+asyncpg://", "postgresql://", 1)


def _role_url(database_url: str, role: str, password: str) -> str:
    _, location = database_url.split("://", 1)
    _, host = location.split("@", 1)
    return f"postgresql://{role}:{password}@{host}"


@pytest.mark.asyncio
async def test_database_enforces_shadow_read_write_split() -> None:
    database_url = _database_url()
    reader_url = _role_url(database_url, "rulenix_shadow_reader", "phase11-local-reader")
    writer_url = _role_url(database_url, "rulenix_shadow_writer", "phase11-local-writer")
    source_pool = await asyncpg.create_pool(reader_url, min_size=1, max_size=1, init=_source_connection)
    sink_pool = await asyncpg.create_pool(writer_url, min_size=1, max_size=1, init=_sink_connection)
    source, sink = SourceReader(source_pool, b"p" * 32), ShadowWriter(sink_pool, "phase11-test")
    source_id = f"permission-proof-{datetime.now(UTC).isoformat()}"
    try:
        await source.prove_boundary()
        await sink.prove_boundary()
        assert isinstance(await source.signals(1, 1), list)
        assert isinstance(await source.readiness(), list)
        async with source_pool.acquire() as connection:
            with pytest.raises((asyncpg.InsufficientPrivilegeError, asyncpg.ReadOnlySQLTransactionError)):
                await connection.execute("INSERT INTO public.trades DEFAULT VALUES")
        async with sink_pool.acquire() as connection:
            for statement in (
                "INSERT INTO public.strategy_signals DEFAULT VALUES",
                "UPDATE public.strategy_orders SET status=status WHERE FALSE",
                "DELETE FROM public.trades WHERE FALSE",
            ):
                with pytest.raises(asyncpg.InsufficientPrivilegeError):
                    await connection.execute(statement)
        inserted = await sink.store(
            Observation(
                "permission_test",
                source_id,
                datetime.now(UTC),
                None,
                "boundary",
                "",
                "a" * 64,
                {},
                {},
                "MATCH",
                "",
                "NONE",
                0.1,
            )
        )
        assert inserted is True
    finally:
        await source_pool.close()
        await sink_pool.close()
