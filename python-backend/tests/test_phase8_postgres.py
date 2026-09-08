import asyncio
import os
from uuid import uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine

from app.reconciliation.domain import EvidenceStatus, ReadEvidence, ReconciliationSnapshot
from app.reconciliation.repository import ReconciliationRepository
from app.reconciliation.service import ReconciliationService
from app.reconciliation.workers import RecoveryWorker


def _url() -> str | None:
    value = os.environ.get("TEST_DATABASE_URL")
    return value.replace("postgresql://", "postgresql+asyncpg://", 1).replace("postgres://", "postgresql+asyncpg://", 1) if value else None


def _snapshot(user, *, revision=12, failed=False):
    kwargs = {"credential_revision": revision}
    positions = ReadEvidence.failure(EvidenceStatus.TIMED_OUT, "positions timeout", **kwargs) if failed else ReadEvidence.success([], **kwargs)
    return ReconciliationSnapshot(
        user_id=user,
        account_id="phase8-account",
        credential_revision=revision,
        egress_identity="198.51.100.8",
        positions=positions,
        orders=ReadEvidence.success([], **kwargs),
        fills=ReadEvidence.success([], **kwargs),
        conditional_rules=ReadEvidence.success([], **kwargs),
        account_validation=ReadEvidence.success(True, **kwargs),
    )


@pytest.mark.asyncio
async def test_postgres_reconciliation_health_is_revision_bound_and_fail_closed():
    url = _url()
    if not url:
        pytest.skip("TEST_DATABASE_URL is not configured; no shared database is used")
    engine = create_async_engine(url)
    user = uuid4()
    try:
        async with engine.begin() as conn:
            await conn.execute(text("INSERT INTO users(id,username,email,password_hash) VALUES(:id,:name,:email,'test')"), {"id": user, "name": f"phase8-{user.hex[:10]}", "email": f"phase8-{user.hex[:10]}@test.invalid"})
        async with AsyncSession(engine, expire_on_commit=False) as session:
            assert (await ReconciliationService(session).apply_snapshot(_snapshot(user))).authoritative
            await session.commit()
        async with engine.connect() as conn:
            row = (await conn.execute(text("SELECT healthy,broker_credential_revision FROM broker_reconciliation_health WHERE user_id=:user"), {"user": user})).mappings().one()
            assert row["healthy"] is True and row["broker_credential_revision"] == 12
        async with AsyncSession(engine, expire_on_commit=False) as session:
            result = await ReconciliationService(session).apply_snapshot(_snapshot(user, revision=13, failed=True))
            await session.commit()
            assert not result.healthy and not result.authoritative
        async with engine.connect() as conn:
            health = (await conn.execute(text("SELECT healthy,broker_credential_revision FROM broker_reconciliation_health WHERE user_id=:user"), {"user": user})).mappings().one()
            blocker = (await conn.execute(text("SELECT status FROM broker_reconciliation_blockers WHERE user_id=:user"), {"user": user})).scalar_one()
            assert health["healthy"] is False and health["broker_credential_revision"] is None and blocker == "open"
    finally:
        async with engine.begin() as conn:
            await conn.execute(text("DELETE FROM users WHERE id=:id"), {"id": user})
        await engine.dispose()


@pytest.mark.asyncio
async def test_postgres_account_lock_allows_one_reconciliation_owner():
    url = _url()
    if not url:
        pytest.skip("TEST_DATABASE_URL is not configured; no shared database is used")
    engine = create_async_engine(url)
    user = uuid4()
    try:
        async with engine.begin() as conn:
            await conn.execute(text("INSERT INTO users(id,username,email,password_hash) VALUES(:id,:name,:email,'test')"), {"id": user, "name": f"phase8-lock-{user.hex[:10]}", "email": f"phase8-lock-{user.hex[:10]}@test.invalid"})
        async with AsyncSession(engine) as first, AsyncSession(engine) as second, first.begin(), ReconciliationRepository(first).user_lock(user) as acquired:
            assert acquired
            contender = asyncio.create_task(_try_lock(second, user))
            await asyncio.sleep(0.05)
            assert await contender is False
        # The transaction release above makes the same lock available again.
        async with AsyncSession(engine) as session, session.begin(), ReconciliationRepository(session).user_lock(user) as acquired:
            assert acquired
    finally:
        async with engine.begin() as conn:
            await conn.execute(text("DELETE FROM users WHERE id=:id"), {"id": user})
        await engine.dispose()


async def _try_lock(session: AsyncSession, user):
    async with session.begin(), ReconciliationRepository(session).user_lock(user) as acquired:
        return acquired


@pytest.mark.asyncio
async def test_postgres_worker_recovery_is_bounded_and_idempotent():
    url = _url()
    if not url:
        pytest.skip("TEST_DATABASE_URL is not configured; no shared database is used")
    engine = create_async_engine(url)
    worker = RecoveryWorker(lambda: AsyncSession(engine, expire_on_commit=False), role="phase8-test", age_seconds=120)
    result = await worker.run_once()
    assert result.acquired
    second = await worker.run_once()
    assert second.acquired
    await engine.dispose()
