import asyncio
import os
from uuid import UUID, uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import async_sessionmaker, create_async_engine

from app.broker.authority import AuthorityError, AuthorityRuntime, LiveMutationAuthority
from app.runtime.authority import AuthorityLeaseLifecycle
from app.runtime.supervisor import DatabaseLeaderScheduler, RuntimeMode
from app.strategy.runtime import SchedulerHealth

RUST_OWNER = UUID("267961f9-6037-580b-906f-152939952a73")


def database_url() -> str:
    value = os.environ.get("TEST_DATABASE_URL", "")
    if not value:
        pytest.skip("TEST_DATABASE_URL is not configured")
    return value.replace("postgresql://", "postgresql+asyncpg://", 1).replace(
        "postgres://", "postgresql+asyncpg://", 1
    )


@pytest.fixture
async def runtime_db():
    engine = create_async_engine(database_url(), pool_size=8)
    factory = async_sessionmaker(engine, expire_on_commit=False)
    async with factory() as session, session.begin():
        await session.execute(
            text("""
            UPDATE live_mutation_authority
               SET holder='rust',epoch=300,lease_owner=:owner,
                   lease_expires_at='infinity'::timestamptz,updated_by='phase13b reset'
             WHERE singleton=TRUE
            """),
            {"owner": RUST_OWNER},
        )
    try:
        yield engine, factory
    finally:
        async with factory() as session, session.begin():
            await session.execute(
                text("""
                UPDATE live_mutation_authority
                   SET holder='rust',epoch=300,lease_owner=:owner,
                       lease_expires_at='infinity'::timestamptz,updated_by='phase13b cleanup'
                 WHERE singleton=TRUE
                """),
                {"owner": RUST_OWNER},
            )
        await engine.dispose()


async def _wait_for(predicate, timeout: float = 3) -> None:
    async with asyncio.timeout(timeout):
        while not predicate():
            await asyncio.sleep(0.02)


@pytest.mark.asyncio
async def test_authoritative_scheduler_has_one_leader_and_fails_over(runtime_db) -> None:
    engine, _ = runtime_db
    first_health, second_health = SchedulerHealth(), SchedulerHealth()
    first_ticks = second_ticks = 0

    async def first_tick():
        nonlocal first_ticks
        first_ticks += 1
        return {"ticks": first_ticks}

    async def second_tick():
        nonlocal second_ticks
        second_ticks += 1
        return {"ticks": second_ticks}

    first = DatabaseLeaderScheduler(
        engine, first_health, RuntimeMode.AUTHORITATIVE, first_tick, interval_seconds=0.02
    )
    second = DatabaseLeaderScheduler(
        engine, second_health, RuntimeMode.AUTHORITATIVE, second_tick, interval_seconds=0.02
    )
    first.start()
    second.start()
    try:
        await _wait_for(lambda: first_health.leader or second_health.leader)
        assert first_health.leader != second_health.leader
        leader, standby = (
            (first, second_health) if first_health.leader else (second, first_health)
        )
        await leader.stop()
        # The standby retries leadership on a bounded five-second cadence.
        await _wait_for(lambda: standby.leader, timeout=7)
        assert standby.snapshot().advancing
    finally:
        await first.stop()
        await second.stop()


@pytest.mark.asyncio
async def test_shadow_scheduler_lock_cannot_contend_with_rust_lock(runtime_db) -> None:
    engine, _ = runtime_db
    rust_connection = await engine.connect()
    await rust_connection.execute(
        text("SELECT pg_advisory_lock(hashtext('rulenix:strategy_scheduler'))")
    )
    health = SchedulerHealth()

    async def tick():
        return {"shadow": True}

    shadow = DatabaseLeaderScheduler(
        engine, health, RuntimeMode.SHADOW, tick, interval_seconds=0.02
    )
    shadow.start()
    try:
        await _wait_for(lambda: health.leader)
        assert shadow.lock_name == "rulenix:python-shadow-scheduler"
        assert health.snapshot().advancing
    finally:
        await shadow.stop()
        await rust_connection.execute(
            text("SELECT pg_advisory_unlock(hashtext('rulenix:strategy_scheduler'))")
        )
        await rust_connection.close()


@pytest.mark.asyncio
async def test_shadow_observes_rust_without_mutation_capability(runtime_db) -> None:
    _, factory = runtime_db
    lifecycle = AuthorityLeaseLifecycle(
        LiveMutationAuthority(factory), RuntimeMode.SHADOW, None, observe_seconds=0.02
    )
    await lifecycle.start()
    try:
        state = lifecycle.snapshot()
        assert state.observed_holder is AuthorityRuntime.RUST
        assert state.observed_epoch == 300
        assert not state.mutation_allowed
    finally:
        await lifecycle.stop()


@pytest.mark.asyncio
async def test_python_lease_renews_then_loss_permanently_fails_closed(runtime_db) -> None:
    _, factory = runtime_db
    authority = LiveMutationAuthority(factory)
    owner = uuid4()
    transferred = await authority.transfer(
        expected_holder=AuthorityRuntime.RUST,
        expected_epoch=300,
        expected_lease_owner=RUST_OWNER,
        new_holder=AuthorityRuntime.PYTHON,
        new_lease_owner=owner,
        lease_seconds=5,
        reason="phase13b lifecycle test",
    )
    lifecycle = AuthorityLeaseLifecycle(
        authority,
        RuntimeMode.AUTHORITATIVE,
        owner,
        lease_seconds=5,
        observe_seconds=0.05,
    )
    await lifecycle.start()
    try:
        assert lifecycle.mutation_allowed()
        await _wait_for(
            lambda: lifecycle.snapshot().lease_expires_at
            != transferred.lease_expires_at
        )
        async with factory() as session, session.begin():
            await session.execute(
                text("""
                UPDATE live_mutation_authority
                   SET holder='rust',epoch=epoch+1,lease_owner=:rust,
                       lease_expires_at='infinity'::timestamptz,updated_by='forced loss'
                 WHERE singleton=TRUE
                """),
                {"rust": RUST_OWNER},
            )
        await _wait_for(lambda: not lifecycle.mutation_allowed())
        # A stale lifecycle exits instead of adopting a new epoch or reacquiring.
        async with factory() as session, session.begin():
            await session.execute(
                text("""
                UPDATE live_mutation_authority
                   SET holder='python',epoch=epoch+1,lease_owner=:owner,
                       lease_expires_at=NOW()+INTERVAL '1 minute',updated_by='new process'
                 WHERE singleton=TRUE
                """),
                {"owner": owner},
            )
        await asyncio.sleep(0.15)
        assert not lifecycle.mutation_allowed()
    finally:
        await lifecycle.stop()


@pytest.mark.asyncio
async def test_competing_python_owner_cannot_adopt_transferred_lease(runtime_db) -> None:
    _, factory = runtime_db
    authority = LiveMutationAuthority(factory)
    owner = uuid4()
    await authority.transfer(
        expected_holder=AuthorityRuntime.RUST,
        expected_epoch=300,
        expected_lease_owner=RUST_OWNER,
        new_holder=AuthorityRuntime.PYTHON,
        new_lease_owner=owner,
        lease_seconds=30,
        reason="phase13b competing process test",
    )
    competitor = AuthorityLeaseLifecycle(
        authority, RuntimeMode.AUTHORITATIVE, uuid4(), lease_seconds=30
    )
    with pytest.raises(RuntimeError, match="not explicitly transferred"):
        await competitor.start()
    assert not competitor.mutation_allowed()


@pytest.mark.asyncio
async def test_cutover_and_rollback_rehearsal_never_overlaps(runtime_db) -> None:
    _, factory = runtime_db
    authority = LiveMutationAuthority(factory)
    python_owner = uuid4()
    user = uuid4()

    python_state = await authority.transfer(
        expected_holder=AuthorityRuntime.RUST,
        expected_epoch=300,
        expected_lease_owner=RUST_OWNER,
        new_holder=AuthorityRuntime.PYTHON,
        new_lease_owner=python_owner,
        lease_seconds=30,
        reason="phase13b isolated cutover rehearsal",
    )
    assert python_state.epoch == 301
    with pytest.raises(AuthorityError):
        async with authority.mutation_permit(
            runtime=AuthorityRuntime.RUST,
            lease_owner=RUST_OWNER,
            user_id=user,
        ):
            pass
    async with authority.mutation_permit(
        runtime=AuthorityRuntime.PYTHON,
        lease_owner=python_owner,
        user_id=user,
    ) as proof:
        assert proof.epoch == 301

    rust_state = await authority.transfer(
        expected_holder=AuthorityRuntime.PYTHON,
        expected_epoch=301,
        expected_lease_owner=python_owner,
        new_holder=AuthorityRuntime.RUST,
        new_lease_owner=RUST_OWNER,
        lease_seconds=None,
        reason="phase13b isolated rollback rehearsal",
    )
    assert rust_state.epoch == 302
    with pytest.raises(AuthorityError):
        async with authority.mutation_permit(
            runtime=AuthorityRuntime.PYTHON,
            lease_owner=python_owner,
            user_id=user,
        ):
            pass
    async with authority.mutation_permit(
        runtime=AuthorityRuntime.RUST,
        lease_owner=RUST_OWNER,
        user_id=user,
    ) as proof:
        assert proof.epoch == 302
