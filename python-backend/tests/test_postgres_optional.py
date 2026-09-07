import os

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import create_async_engine


@pytest.mark.asyncio
async def test_isolated_postgres_ping():
    url = os.environ.get("TEST_DATABASE_URL")
    if not url:
        pytest.skip("TEST_DATABASE_URL is not configured; no shared database is used")
    if url.startswith("postgresql://"):
        url = "postgresql+asyncpg://" + url.removeprefix("postgresql://")
    elif url.startswith("postgres://"):
        url = "postgresql+asyncpg://" + url.removeprefix("postgres://")
    engine = create_async_engine(url, pool_pre_ping=True)
    try:
        async with engine.begin() as connection:
            assert (await connection.execute(text("SELECT 1"))).scalar_one() == 1
            await connection.execute(text("CREATE TEMP TABLE python_phase3_probe(value integer) ON COMMIT DROP"))
            await connection.execute(text("INSERT INTO python_phase3_probe(value) VALUES (42)"))
            assert (await connection.execute(text("SELECT value FROM python_phase3_probe"))).scalar_one() == 42
    finally:
        await engine.dispose()
