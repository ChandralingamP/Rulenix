import os
from datetime import UTC, datetime
from uuid import UUID, uuid4

import httpx
import pytest
from pydantic import SecretStr
from sqlalchemy import text
from sqlalchemy.ext.asyncio import async_sessionmaker, create_async_engine

from app.broker.angel.client import AngelClient
from app.broker.angel.egress import DefaultEgressBinding
from app.broker.angel.models import AccountContext
from app.broker.angel.mutation_guard import MutationGuard
from app.broker.authority import LiveMutationAuthority
from app.broker.mutations import (
    LiveMutationCoordinator,
    MutationPendingError,
    MutationState,
)
from app.reconciliation.workers import RecoveryWorker


def database_url() -> str:
    value = os.environ.get("TEST_DATABASE_URL", "")
    if not value:
        pytest.skip("TEST_DATABASE_URL is not configured")
    return value.replace("postgresql://", "postgresql+asyncpg://", 1).replace(
        "postgres://", "postgresql+asyncpg://", 1
    )


@pytest.fixture
async def mutation_db():
    engine = create_async_engine(database_url(), pool_size=8)
    factory = async_sessionmaker(engine, expire_on_commit=False)
    user, snapshot, order, owner = uuid4(), uuid4(), uuid4(), uuid4()
    async with factory() as session, session.begin():
        await session.execute(
            text("""
            INSERT INTO users(id,username,email,password_hash,is_active,can_live_trade)
            VALUES(:user,:username,:email,'test',TRUE,TRUE)
            """),
            {"user": user, "username": f"p13-{user.hex}", "email": f"{user.hex}@test.invalid"},
        )
        await session.execute(
            text("""
            INSERT INTO user_profiles(
                user_id,brokerage_user_id,trading_mode,last_token_status,
                broker_credential_revision)
            VALUES(:user,'fixture-client','live','success',1)
            """),
            {"user": user},
        )
        await session.execute(
            text("""
            INSERT INTO broker_reconciliation_health(
                user_id,healthy,detail,checked_at,broker_credential_revision)
            VALUES(:user,TRUE,'phase13 isolated fixture',NOW(),1)
            """),
            {"user": user},
        )
        await session.execute(
            text("""
            INSERT INTO strategy_market_snapshots(
                id,strategy_key,instrument,trade_date,status,contract_token,
                contract_symbol,lot_size,exchange_segment,product_type,execution_key)
            VALUES(:snapshot,'futures_breakout_v3','GOLDTEN',:today,'ready','12345',
                   'GOLDTEN26OCTFUT',10,'MCX','CARRYFORWARD',:key)
            """),
            {
                "snapshot": snapshot,
                "today": datetime.now(UTC).date(),
                "key": f"phase13-{order.hex}",
            },
        )
        await session.execute(
            text("""
            INSERT INTO strategy_orders(
                id,user_id,snapshot_id,session_key,role,side,execution_mode,lots,
                quantity,price,trigger_price,status,idempotency_key,client_order_id,
                order_type,exchange_segment,product_type)
            VALUES(:order,:user,:snapshot,:session,'BUY_ENTRY','BUY','live',1,10,
                   100,99,'pending',:key,:client,'STOPLOSS_LIMIT','MCX','CARRYFORWARD')
            """),
            {
                "order": order,
                "user": user,
                "snapshot": snapshot,
                "session": f"p13-{order.hex[:20]}",
                "key": f"phase13:{order}",
                "client": f"RX{order.hex[:18].upper()}",
            },
        )
        await session.execute(
            text("""
            UPDATE live_mutation_authority
               SET holder='python',epoch=200,lease_owner=:owner,
                   lease_expires_at=NOW()+INTERVAL '5 minutes',updated_by='phase13 mutation test'
             WHERE singleton=TRUE
            """),
            {"owner": owner},
        )
    try:
        yield factory, user, order, owner
    finally:
        async with factory() as session, session.begin():
            await session.execute(text("DELETE FROM users WHERE id=:user"), {"user": user})
            await session.execute(
                text("""
                UPDATE live_mutation_authority
                   SET holder='rust',epoch=100,
                       lease_owner='267961f9-6037-580b-906f-152939952a73'::uuid,
                       lease_expires_at='infinity'::timestamptz,updated_by='phase13 test cleanup'
                 WHERE singleton=TRUE
                """)
            )
        await engine.dispose()


def client_factory(handler, user_id: UUID):
    async def create(requested_user: UUID) -> AngelClient:
        assert requested_user == user_id
        account = AccountContext(
            str(user_id),
            "fixture-client",
            SecretStr("fixture-key"),
            SecretStr("fixture-jwt"),
        )
        return AngelClient(
            "https://angel.fixture",
            "wss://angel.fixture/ws",
            account,
            httpx.AsyncClient(transport=httpx.MockTransport(handler)),
            DefaultEgressBinding(),
            MutationGuard(),
        )

    return create


@pytest.mark.asyncio
async def test_durable_place_ack_and_duplicate_worker_do_not_duplicate(mutation_db):
    factory, user, order, owner = mutation_db
    calls = 0

    async def accepted(_: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        return httpx.Response(200, json={"status": True, "data": {"orderid": "broker-1"}})

    coordinator = LiveMutationCoordinator(
        factory,
        LiveMutationAuthority(factory),
        client_factory(accepted, user),
        lease_owner=owner,
        enabled=True,
    )
    outcome = await coordinator.place_order(order)
    assert outcome.state is MutationState.ACKNOWLEDGED
    assert outcome.broker_order_id == "broker-1"
    with pytest.raises(MutationPendingError):
        await coordinator.place_order(order)
    assert calls == 1
    async with factory() as session:
        row = (
            await session.execute(
                text("""
                SELECT o.status,o.broker_order_id,a.state,a.authority_epoch
                  FROM strategy_orders o JOIN broker_mutation_attempts a
                    ON a.strategy_order_id=o.id WHERE o.id=:order
                """),
                {"order": order},
            )
        ).one()
    assert tuple(row) == ("submitted", "broker-1", "acknowledged", 200)


@pytest.mark.asyncio
async def test_lost_response_is_ambiguous_and_not_replayed_after_restart(mutation_db):
    factory, user, order, owner = mutation_db
    calls = 0

    async def lost(_: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        raise httpx.ReadTimeout("response lost after acceptance")

    coordinator = LiveMutationCoordinator(
        factory,
        LiveMutationAuthority(factory),
        client_factory(lost, user),
        lease_owner=owner,
        enabled=True,
    )
    outcome = await coordinator.place_order(order)
    assert outcome.state is MutationState.AMBIGUOUS
    with pytest.raises(MutationPendingError):
        await coordinator.place_order(order)
    assert calls == 1


@pytest.mark.asyncio
async def test_pre_network_crash_recovers_to_safe_retry(mutation_db):
    factory, user, order, owner = mutation_db
    calls = 0

    async def accepted(_: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        return httpx.Response(200, json={"status": True, "data": {"orderid": "broker-retry"}})

    class CrashBeforeNetwork(LiveMutationCoordinator):
        async def _mark_network(self, attempt_id, proof) -> None:
            raise RuntimeError("injected crash before network")

    crashing = CrashBeforeNetwork(
        factory,
        LiveMutationAuthority(factory),
        client_factory(accepted, user),
        lease_owner=owner,
        enabled=True,
    )
    with pytest.raises(RuntimeError, match="before network"):
        await crashing.place_order(order)
    assert calls == 0
    async with factory() as session, session.begin():
        await session.execute(
            text("UPDATE broker_mutation_attempts SET updated_at=NOW()-INTERVAL '5 minutes' WHERE strategy_order_id=:order"),
            {"order": order},
        )
        await session.execute(
            text("UPDATE strategy_orders SET updated_at=NOW()-INTERVAL '5 minutes' WHERE id=:order"),
            {"order": order},
        )
    await RecoveryWorker(factory, role=f"phase13-{order}", age_seconds=1).run_once()
    retrying = LiveMutationCoordinator(
        factory,
        LiveMutationAuthority(factory),
        client_factory(accepted, user),
        lease_owner=owner,
        enabled=True,
    )
    outcome = await retrying.place_order(order)
    assert outcome.state is MutationState.ACKNOWLEDGED
    assert calls == 1


@pytest.mark.asyncio
async def test_post_accept_database_failure_becomes_ambiguous_without_replay(mutation_db):
    factory, user, order, owner = mutation_db
    calls = 0

    async def accepted(_: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        return httpx.Response(200, json={"status": True, "data": {"orderid": "broker-lost"}})

    class AckDatabaseFailure(LiveMutationCoordinator):
        async def _finish(self, **kwargs) -> None:
            if kwargs["state"] is MutationState.ACKNOWLEDGED:
                raise RuntimeError("injected database failure after acceptance")
            await super()._finish(**kwargs)

    coordinator = AckDatabaseFailure(
        factory,
        LiveMutationAuthority(factory),
        client_factory(accepted, user),
        lease_owner=owner,
        enabled=True,
    )
    with pytest.raises(RuntimeError, match="after acceptance"):
        await coordinator.place_order(order)
    assert calls == 1
    async with factory() as session, session.begin():
        await session.execute(
            text("UPDATE broker_mutation_attempts SET updated_at=NOW()-INTERVAL '5 minutes' WHERE strategy_order_id=:order"),
            {"order": order},
        )
        await session.execute(
            text("UPDATE strategy_orders SET updated_at=NOW()-INTERVAL '5 minutes' WHERE id=:order"),
            {"order": order},
        )
    await RecoveryWorker(factory, role=f"phase13-{order}", age_seconds=1).run_once()
    async with factory() as session:
        state = await session.scalar(
            text("SELECT state FROM broker_mutation_attempts WHERE strategy_order_id=:order"),
            {"order": order},
        )
    assert state == "ambiguous"
    with pytest.raises(MutationPendingError):
        await coordinator.place_order(order)
    assert calls == 1


@pytest.mark.asyncio
async def test_cancel_is_durable_and_order_not_found_remains_ambiguous(mutation_db):
    factory, user, order, owner = mutation_db
    calls: list[str] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        calls.append(request.url.path)
        if request.url.path.endswith("placeOrder"):
            return httpx.Response(200, json={"status": True, "data": {"orderid": "broker-cancel"}})
        return httpx.Response(
            404,
            json={"status": False, "errorcode": "AB1007", "message": "Order not found"},
        )

    coordinator = LiveMutationCoordinator(
        factory,
        LiveMutationAuthority(factory),
        client_factory(handler, user),
        lease_owner=owner,
        enabled=True,
    )
    assert (await coordinator.place_order(order)).state is MutationState.ACKNOWLEDGED
    result = await coordinator.cancel_order(order, variety="STOPLOSS")
    assert result.state is MutationState.AMBIGUOUS
    assert calls == [
        "/rest/secure/angelbroking/order/v1/placeOrder",
        "/rest/secure/angelbroking/order/v1/cancelOrder",
    ]
    with pytest.raises(MutationPendingError):
        await coordinator.cancel_order(order, variety="STOPLOSS")


@pytest.mark.asyncio
async def test_fenced_python_worker_makes_zero_network_requests(mutation_db):
    factory, user, order, owner = mutation_db
    calls = 0

    async def should_not_run(_: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        raise AssertionError("fenced worker reached broker transport")

    async with factory() as session, session.begin():
        await session.execute(
            text("""
            UPDATE live_mutation_authority
               SET holder='rust',epoch=201,
                   lease_owner='267961f9-6037-580b-906f-152939952a73'::uuid,
                   lease_expires_at='infinity'::timestamptz
             WHERE singleton=TRUE
            """)
        )
    coordinator = LiveMutationCoordinator(
        factory,
        LiveMutationAuthority(factory),
        client_factory(should_not_run, user),
        lease_owner=owner,
        enabled=True,
    )
    assert (await coordinator.place_order(order)).state is MutationState.BLOCKED
    assert calls == 0
