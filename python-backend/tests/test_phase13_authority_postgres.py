import asyncio
import os
from uuid import UUID, uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import async_sessionmaker, create_async_engine

from app.broker.authority import (
    AuthorityError,
    AuthorityRuntime,
    LiveMutationAuthority,
)

RUST_OWNER = UUID("267961f9-6037-580b-906f-152939952a73")


def database_url() -> str:
    value = os.environ.get("TEST_DATABASE_URL", "")
    if not value:
        pytest.skip("TEST_DATABASE_URL is not configured")
    return value.replace("postgresql://", "postgresql+asyncpg://", 1).replace(
        "postgres://", "postgresql+asyncpg://", 1
    )


@pytest.fixture
async def authority():
    engine = create_async_engine(database_url(), pool_size=6)
    factory = async_sessionmaker(engine, expire_on_commit=False)
    async with factory() as session, session.begin():
        await session.execute(
            text("""
            UPDATE live_mutation_authority
               SET holder='rust',epoch=100,lease_owner=:owner,
                   lease_expires_at='infinity'::timestamptz,updated_by='phase13 test reset'
             WHERE singleton=TRUE
            """),
            {"owner": RUST_OWNER},
        )
    try:
        yield LiveMutationAuthority(factory), factory
    finally:
        async with factory() as session, session.begin():
            await session.execute(
                text("""
                UPDATE live_mutation_authority
                   SET holder='rust',epoch=100,lease_owner=:owner,
                       lease_expires_at='infinity'::timestamptz,updated_by='phase13 test cleanup'
                 WHERE singleton=TRUE
                """),
                {"owner": RUST_OWNER},
            )
        await engine.dispose()


@pytest.mark.asyncio
async def test_rust_python_rollback_transfer_fences_stale_workers(authority):
    gate, _ = authority
    python_owner = uuid4()
    python_state = await gate.transfer(
        expected_holder=AuthorityRuntime.RUST,
        expected_epoch=100,
        expected_lease_owner=RUST_OWNER,
        new_holder=AuthorityRuntime.PYTHON,
        new_lease_owner=python_owner,
        lease_seconds=30,
        reason="controlled test transfer",
    )
    assert python_state.epoch == 101

    with pytest.raises(AuthorityError):
        async with gate.mutation_permit(
            runtime=AuthorityRuntime.RUST,
            lease_owner=RUST_OWNER,
            user_id=uuid4(),
        ):
            pass

    async with gate.mutation_permit(
        runtime=AuthorityRuntime.PYTHON,
        lease_owner=python_owner,
        user_id=uuid4(),
    ) as proof:
        assert proof.epoch == 101

    rust_state = await gate.transfer(
        expected_holder=AuthorityRuntime.PYTHON,
        expected_epoch=101,
        expected_lease_owner=python_owner,
        new_holder=AuthorityRuntime.RUST,
        new_lease_owner=RUST_OWNER,
        lease_seconds=None,
        reason="controlled rollback test",
    )
    assert rust_state.epoch == 102
    with pytest.raises(AuthorityError):
        async with gate.mutation_permit(
            runtime=AuthorityRuntime.PYTHON,
            lease_owner=python_owner,
            user_id=uuid4(),
        ):
            pass


@pytest.mark.asyncio
async def test_transfer_waits_for_inflight_mutation_lock(authority):
    gate, _ = authority
    rollback = None
    async with gate.mutation_permit(
        runtime=AuthorityRuntime.RUST,
        lease_owner=RUST_OWNER,
        user_id=uuid4(),
    ):
        rollback = asyncio.create_task(
            gate.transfer(
                expected_holder=AuthorityRuntime.RUST,
                expected_epoch=100,
                expected_lease_owner=RUST_OWNER,
                new_holder=AuthorityRuntime.NONE,
                new_lease_owner=None,
                lease_seconds=None,
                reason="pause/resume fencing test",
            )
        )
        await asyncio.sleep(0.1)
        assert not rollback.done()
    assert rollback is not None
    result = await asyncio.wait_for(rollback, 2)
    assert result.holder is AuthorityRuntime.NONE
    assert result.epoch == 101


@pytest.mark.asyncio
async def test_expired_lease_and_concurrent_transfer_fail_closed(authority):
    gate, factory = authority
    first_owner, second_owner = uuid4(), uuid4()

    async def acquire(owner):
        return await gate.transfer(
            expected_holder=AuthorityRuntime.RUST,
            expected_epoch=100,
            expected_lease_owner=RUST_OWNER,
            new_holder=AuthorityRuntime.PYTHON,
            new_lease_owner=owner,
            lease_seconds=30,
            reason="concurrent acquisition test",
        )

    results = await asyncio.gather(
        acquire(first_owner), acquire(second_owner), return_exceptions=True
    )
    assert sum(not isinstance(item, Exception) for item in results) == 1
    assert sum(isinstance(item, AuthorityError) for item in results) == 1
    state = await gate.current()
    assert state.lease_owner is not None

    async with factory() as session, session.begin():
        await session.execute(
            text("""
            UPDATE live_mutation_authority
               SET lease_expires_at=clock_timestamp()-INTERVAL '1 second'
             WHERE singleton=TRUE
            """)
        )
    with pytest.raises(AuthorityError):
        async with gate.mutation_permit(
            runtime=AuthorityRuntime.PYTHON,
            lease_owner=state.lease_owner,
            user_id=uuid4(),
        ):
            pass
    with pytest.raises(AuthorityError):
        await gate.renew_python(
            epoch=state.epoch, lease_owner=state.lease_owner, lease_seconds=30
        )
