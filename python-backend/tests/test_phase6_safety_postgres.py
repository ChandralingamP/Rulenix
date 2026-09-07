import asyncio
import os
from uuid import uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine

from app.risk.domain import ActionKind, ReasonCode, SafetyRequest
from app.risk.repository import SafetyRepository


def _url() -> str | None:
    value = os.environ.get("TEST_DATABASE_URL")
    if not value:
        return None
    return value.replace("postgresql://", "postgresql+asyncpg://", 1).replace("postgres://", "postgresql+asyncpg://", 1)


@pytest.mark.asyncio
async def test_postgres_final_gate_kill_switch_toctou_and_revision():
    url = _url()
    if not url:
        pytest.skip("TEST_DATABASE_URL is not configured; no shared database is used")
    engine = create_async_engine(url)
    user = uuid4()
    try:
        async with engine.begin() as conn:
            await conn.execute(text("""
                INSERT INTO users(id,username,email,password_hash,is_active,can_live_trade)
                VALUES(:id,:username,:email,'test',TRUE,TRUE)
            """), {"id": user, "username": f"phase6-{user.hex[:10]}", "email": f"phase6-{user.hex[:10]}@test.invalid"})
            await conn.execute(text("""
                INSERT INTO user_profiles(user_id,trading_mode,last_token_status,broker_credential_revision)
                VALUES(:id,'live','success',10)
            """), {"id": user})
            await conn.execute(text("""
                INSERT INTO broker_reconciliation_health(user_id,healthy,checked_at,broker_credential_revision)
                VALUES(:id,TRUE,NOW(),10)
            """), {"id": user})
        request = SafetyRequest(user_id=user, action=ActionKind.ENTRY, execution_mode="live", quantity=1, lots=1)
        async with AsyncSession(engine, expire_on_commit=False) as session:
            first = await SafetyRepository(session).final_pre_mutation_check(request)
            assert first.allowed

        async def concurrent_check():
            async with AsyncSession(engine, expire_on_commit=False) as session:
                return await SafetyRepository(session).final_pre_mutation_check(request)

        concurrent = await asyncio.gather(*(concurrent_check() for _ in range(3)))
        assert all(decision.allowed for decision in concurrent)
        async with engine.begin() as conn:
            await conn.execute(text("UPDATE risk_kill_switches SET enabled=TRUE WHERE user_id IS NULL"))
        async with AsyncSession(engine, expire_on_commit=False) as session:
            second = await SafetyRepository(session).final_pre_mutation_check(request)
            assert not second.allowed and second.reason_code is ReasonCode.GLOBAL_KILL_SWITCH
        async with engine.begin() as conn:
            await conn.execute(text("UPDATE risk_kill_switches SET enabled=FALSE WHERE user_id IS NULL"))
            await conn.execute(text("UPDATE user_profiles SET broker_credential_revision=11 WHERE user_id=:id"), {"id": user})
        async with AsyncSession(engine, expire_on_commit=False) as session:
            third = await SafetyRepository(session).final_pre_mutation_check(request)
            assert not third.allowed and third.reason_code is ReasonCode.CREDENTIAL_REVISION_MISMATCH
    finally:
        async with engine.begin() as conn:
            await conn.execute(text("DELETE FROM users WHERE id=:id"), {"id": user})
            await conn.execute(text("UPDATE risk_kill_switches SET enabled=FALSE WHERE user_id IS NULL"))
        await engine.dispose()
