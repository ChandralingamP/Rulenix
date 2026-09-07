import asyncio
import os
from datetime import UTC, datetime
from uuid import uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine

from app.strategy.persistence import PreparedIntent, SignalRepository


def _url() -> str | None:
    value = os.environ.get("TEST_DATABASE_URL")
    return (
        value.replace("postgresql://", "postgresql+asyncpg://", 1).replace(
            "postgres://", "postgresql+asyncpg://", 1
        )
        if value
        else None
    )


@pytest.mark.asyncio
async def test_postgres_signal_fanout_is_idempotent_under_concurrent_workers():
    url = _url()
    if not url:
        pytest.skip("TEST_DATABASE_URL is not configured; no shared database is used")
    engine = create_async_engine(url)
    user = uuid4()
    try:
        async with engine.begin() as conn:
            await conn.execute(
                text(
                    "INSERT INTO users(id,username,email,password_hash) VALUES(:id,:name,:email,'test')"
                ),
                {
                    "id": user,
                    "name": f"phase7-{user.hex[:10]}",
                    "email": f"phase7-{user.hex[:10]}@test.invalid",
                },
            )
        intent = PreparedIntent(
            user,
            None,
            None,
            "futures_breakout_v3",
            "GOLDTEN",
            "day",
            "ENTRY",
            "BUY_ENTRY",
            "BUY",
            "STOPLOSS",
            1,
            10,
            100,
            99,
        )

        async def materialize():
            async with AsyncSession(engine, expire_on_commit=False) as session:
                result = await SignalRepository(session).materialize(
                    strategy_key="futures_breakout_v3",
                    instrument="GOLDTEN",
                    session_key="day",
                    signal_at=datetime.now(UTC),
                    signal_type="entry",
                    snapshot_id=None,
                    payload={"fixture": "phase7"},
                    intents=[intent],
                )
                await session.commit()
                return result

        results = await asyncio.gather(materialize(), materialize())
        assert results[0][0] == results[1][0]
        async with engine.connect() as conn:
            assert (
                await conn.execute(
                    text("SELECT COUNT(*) FROM strategy_signals WHERE id=:id"),
                    {"id": results[0][0]},
                )
            ).scalar_one() == 1
            assert (
                await conn.execute(
                    text("SELECT COUNT(*) FROM strategy_execution_intents WHERE signal_id=:id"),
                    {"id": results[0][0]},
                )
            ).scalar_one() == 1
    finally:
        async with engine.begin() as conn:
            await conn.execute(text("DELETE FROM users WHERE id=:id"), {"id": user})
            await conn.execute(
                text("DELETE FROM strategy_signals WHERE payload->>'fixture'='phase7'")
            )
        await engine.dispose()
