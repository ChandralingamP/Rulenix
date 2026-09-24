import os
from datetime import UTC, datetime
from decimal import Decimal
from uuid import uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import async_sessionmaker, create_async_engine

from app.broker.mutations import MutationOutcome, MutationState
from app.reconciliation.domain import (
    BrokerFill,
    BrokerOrder,
    BrokerPosition,
    ReadEvidence,
    ReconciliationSnapshot,
)
from app.reconciliation.service import ReconciliationService
from app.runtime.lifecycle import (
    EodLifecycleWorker,
    FillLifecycleWorker,
    ProtectionLifecycleWorker,
    ReversalLifecycleWorker,
    RiskReducingCloseWorker,
)
from app.trading.repository import TradingRepository


def database_url() -> str:
    value = os.environ.get("TEST_DATABASE_URL", "")
    if not value:
        pytest.skip("TEST_DATABASE_URL is not configured")
    return value.replace("postgresql://", "postgresql+asyncpg://", 1).replace(
        "postgres://", "postgresql+asyncpg://", 1
    )


class FakeCoordinator:
    def __init__(self, factory):
        self.factory = factory
        self.places: list = []
        self.cancels: list = []

    async def place_order(self, order_id, *, action=None):
        self.places.append((order_id, action))
        async with self.factory() as session, session.begin():
            await session.execute(
                text("""
                UPDATE strategy_orders SET status='submitted',broker_order_id=:broker,
                  last_reconciled_at=NOW(),updated_at=NOW() WHERE id=:order
                """),
                {"order": order_id, "broker": f"fake-{order_id}"},
            )
        return MutationOutcome(uuid4(), MutationState.ACKNOWLEDGED, f"fake-{order_id}")

    async def cancel_order(self, order_id, *, variety):
        self.cancels.append((order_id, variety))
        async with self.factory() as session, session.begin():
            await session.execute(
                text(
                    "UPDATE strategy_orders SET status='cancelled',updated_at=NOW() WHERE id=:order"
                ),
                {"order": order_id},
            )
        return MutationOutcome(uuid4(), MutationState.ACKNOWLEDGED)


@pytest.fixture
async def lifecycle_db():
    engine = create_async_engine(database_url(), pool_size=8)
    factory = async_sessionmaker(engine, expire_on_commit=False)
    user, snapshot, order = uuid4(), uuid4(), uuid4()
    async with factory() as session, session.begin():
        await session.execute(
            text("""
            INSERT INTO users(id,username,email,password_hash,is_active,can_live_trade)
            VALUES(:user,:name,:email,'test',TRUE,TRUE)
            """),
            {"user": user, "name": f"p13c-{user.hex}", "email": f"{user.hex}@test.invalid"},
        )
        await session.execute(
            text("""
            INSERT INTO user_profiles(user_id,trading_mode,last_token_status,broker_credential_revision)
            VALUES(:user,'live','success',1)
            """),
            {"user": user},
        )
        await session.execute(
            text("""
            INSERT INTO broker_reconciliation_health(
              user_id,healthy,detail,checked_at,broker_credential_revision)
            VALUES(:user,TRUE,'phase13c fixture',NOW(),1)
            """),
            {"user": user},
        )
        await session.execute(
            text("""
            INSERT INTO strategy_market_snapshots(
              id,strategy_key,instrument,trade_date,status,contract_token,contract_symbol,
              lot_size,exchange_segment,product_type,execution_key,
              buy_target,buy_sl1,buy_sl2,sell_target,sell_sl1,sell_sl2)
            VALUES(:snapshot,'futures_breakout_v3','GOLDTEN',CURRENT_DATE,'ready','p13c-token',
              'GOLDTENP13CFUT',10,'MCX','CARRYFORWARD',:key,105,98,97,95,102,103)
            """),
            {"snapshot": snapshot, "key": f"p13c-{snapshot.hex}"},
        )
        await session.execute(
            text("""
            INSERT INTO strategy_orders(
              id,user_id,snapshot_id,session_key,role,side,execution_mode,lots,quantity,
              price,trigger_price,status,idempotency_key,client_order_id,order_type,
              exchange_segment,product_type)
            VALUES(:order,:user,:snapshot,'p13c-entry','BUY_ENTRY','BUY','live',1,10,
              100,100,'submitting',:key,:client,'STOPLOSS_LIMIT','MCX','CARRYFORWARD')
            """),
            {
                "order": order,
                "user": user,
                "snapshot": snapshot,
                "key": f"p13c:{order}",
                "client": f"RX{order.hex[:18].upper()}",
            },
        )
    try:
        yield factory, user, snapshot, order
    finally:
        async with factory() as session, session.begin():
            await session.execute(text("DELETE FROM users WHERE id=:user"), {"user": user})
        await engine.dispose()


def broker_snapshot(user, order, client, *, position=10, filled=10, price="100"):
    revision = 1
    return ReconciliationSnapshot(
        user_id=user,
        account_id="phase13c",
        credential_revision=revision,
        egress_identity="fake",
        positions=ReadEvidence.success(
            (BrokerPosition("p13c-token", "GOLDTENP13CFUT", "MCX", position, Decimal(price)),),
            credential_revision=revision,
        ),
        orders=ReadEvidence.success(
            (
                BrokerOrder(
                    "broker-entry",
                    "p13c-token",
                    "GOLDTENP13CFUT",
                    "MCX",
                    "BUY",
                    "complete",
                    10,
                    filled,
                    Decimal(price),
                    client_order_id=client,
                ),
            ),
            credential_revision=revision,
        ),
        fills=ReadEvidence.success(
            (
                BrokerFill(
                    "fill-entry",
                    "broker-entry",
                    "p13c-token",
                    "GOLDTENP13CFUT",
                    "MCX",
                    "BUY",
                    filled,
                    Decimal(price),
                    datetime.now(UTC),
                    True,
                    client,
                ),
            ),
            credential_revision=revision,
        ),
        conditional_rules=ReadEvidence.success((), credential_revision=revision),
        account_validation=ReadEvidence.success(True, credential_revision=revision),
        current_credential_revision=revision,
    )


async def open_trade(factory, user, entry_order):
    async with factory() as session:
        client = await session.scalar(
            text("SELECT client_order_id FROM strategy_orders WHERE id=:id"),
            {"id": entry_order},
        )
    async with factory() as session:
        await ReconciliationService(session).apply_snapshot(
            broker_snapshot(user, entry_order, client)
        )
    await FillLifecycleWorker(factory).run_once()
    async with factory() as session, session.begin():
        trade_id = await session.scalar(
            text("SELECT id FROM trades WHERE user_id=:user"), {"user": user}
        )
        await session.execute(
            text("""
            UPDATE trades SET broker_net_quantity=quantity,last_position_reconciled_at=NOW()
             WHERE id=:trade
            """),
            {"trade": trade_id},
        )
    return trade_id


@pytest.mark.asyncio
async def test_reconciled_entry_fill_and_protection_are_restart_idempotent(lifecycle_db):
    factory, user, _, entry_order = lifecycle_db
    async with factory() as session:
        client = await session.scalar(
            text("SELECT client_order_id FROM strategy_orders WHERE id=:order"),
            {"order": entry_order},
        )
    async with factory() as session:
        await ReconciliationService(session).apply_snapshot(
            broker_snapshot(user, entry_order, client)
        )
    async with factory() as session:
        entry = (
            await session.execute(
                text("""
            SELECT status,filled_quantity,processed_quantity FROM strategy_orders WHERE id=:order
        """),
                {"order": entry_order},
            )
        ).one()
        assert tuple(entry) == ("submitted", 10, 0)

    fills = FillLifecycleWorker(factory)
    assert (await fills.run_once())["processed_fills"] == 1
    assert (await fills.run_once())["processed_fills"] == 0
    async with factory() as session, session.begin():
        trade = (
            (
                await session.execute(
                    text("""
            SELECT id,status,quantity,direction,safety_status FROM trades WHERE user_id=:user
        """),
                    {"user": user},
                )
            )
            .mappings()
            .one()
        )
        assert (trade["status"], trade["quantity"], trade["direction"], trade["safety_status"]) == (
            "open",
            10,
            "BUY",
            "PROTECTION_REQUIRED",
        )
        await session.execute(
            text("""
            UPDATE trades SET broker_net_quantity=10,last_position_reconciled_at=NOW() WHERE id=:trade
        """),
            {"trade": trade["id"]},
        )

    coordinator = FakeCoordinator(factory)
    protection = ProtectionLifecycleWorker(factory, coordinator)
    first = await protection.run_once()
    assert first["submitted"] == 1
    second = await protection.run_once()
    assert second["completed"] == 1
    assert second["targets_submitted"] == 1
    await protection.run_once()
    assert len(coordinator.places) == 2
    async with factory() as session:
        roles = (
            await session.execute(
                text("""
            SELECT role,COUNT(*) FROM strategy_orders WHERE trade_id=:trade
             AND role IN ('SL1','SL2','TARGET') GROUP BY role ORDER BY role
        """),
                {"trade": trade["id"]},
            )
        ).all()
        assert roles == [("SL1", 1), ("TARGET", 1)]


@pytest.mark.asyncio
async def test_target_fill_replaces_sl1_with_sl2_without_duplicate(lifecycle_db):
    factory, user, _, entry_order = lifecycle_db
    async with factory() as session:
        client = await session.scalar(
            text("SELECT client_order_id FROM strategy_orders WHERE id=:id"), {"id": entry_order}
        )
    async with factory() as session:
        await ReconciliationService(session).apply_snapshot(
            broker_snapshot(user, entry_order, client)
        )
    fills = FillLifecycleWorker(factory)
    await fills.run_once()
    async with factory() as session, session.begin():
        trade_id = await session.scalar(
            text("SELECT id FROM trades WHERE user_id=:user"), {"user": user}
        )
        await session.execute(
            text(
                "UPDATE trades SET broker_net_quantity=10,last_position_reconciled_at=NOW() WHERE id=:trade"
            ),
            {"trade": trade_id},
        )
    coordinator = FakeCoordinator(factory)
    protection = ProtectionLifecycleWorker(factory, coordinator)
    await protection.run_once()
    await protection.run_once()
    async with factory() as session, session.begin():
        target = await session.scalar(
            text("SELECT id FROM strategy_orders WHERE trade_id=:trade AND role='TARGET'"),
            {"trade": trade_id},
        )
        await session.execute(
            text("""
            UPDATE strategy_orders SET filled_quantity=5,average_fill_price=105,status='submitted'
             WHERE id=:order
        """),
            {"order": target},
        )
    await fills.run_once()
    async with factory() as session, session.begin():
        await session.execute(
            text(
                "UPDATE trades SET broker_net_quantity=5,last_position_reconciled_at=NOW() WHERE id=:trade"
            ),
            {"trade": trade_id},
        )
    waiting = await protection.run_once()
    assert waiting["waiting"] == 1
    assert len(coordinator.cancels) == 1
    submitted = await protection.run_once()
    assert submitted["submitted"] == 1
    await protection.run_once()
    async with factory() as session:
        roles = (
            await session.execute(
                text("""
            SELECT role,status,COUNT(*) FROM strategy_orders WHERE trade_id=:trade
             GROUP BY role,status ORDER BY role,status
        """),
                {"trade": trade_id},
            )
        ).all()
        assert ("SL1", "cancelled", 1) in roles
        assert ("SL2", "submitted", 1) in roles
        assert sum(count for role, _, count in roles if role == "SL2") == 1


@pytest.mark.asyncio
async def test_manual_live_close_cancels_owned_protection_then_closes_once(lifecycle_db):
    factory, user, _, entry_order = lifecycle_db
    trade_id = await open_trade(factory, user, entry_order)
    coordinator = FakeCoordinator(factory)
    protection = ProtectionLifecycleWorker(factory, coordinator)
    await protection.run_once()
    await protection.run_once()
    async with factory() as session, session.begin():
        assert await TradingRepository(session).request_manual_close(
            trade_id=trade_id, user_id=user, requested_quantity=10
        )
        assert not await TradingRepository(session).request_manual_close(
            trade_id=trade_id, user_id=user, requested_quantity=10
        )
    close = RiskReducingCloseWorker(factory, coordinator)
    assert (await close.run_manual_once())["waiting"] == 1
    assert (await close.run_manual_once())["waiting"] == 1
    result = await close.run_manual_once()
    assert result["submitted"] == 1
    placed_after_first = len(coordinator.places)
    duplicate = await close.run_manual_once()
    assert duplicate["submitted"] == 0
    assert len(coordinator.places) == placed_after_first
    async with factory() as session, session.begin():
        close_order = await session.scalar(
            text("SELECT strategy_order_id FROM manual_trade_close_intents WHERE trade_id=:trade"),
            {"trade": trade_id},
        )
        await session.execute(
            text("""
            UPDATE strategy_orders SET filled_quantity=quantity,average_fill_price=101,status='submitted'
             WHERE id=:order
            """),
            {"order": close_order},
        )
    await FillLifecycleWorker(factory).run_once()
    async with factory() as session:
        trade = (
            await session.execute(
                text("SELECT status,safety_status,exit_reason FROM trades WHERE id=:trade"),
                {"trade": trade_id},
            )
        ).one()
        intent = await session.scalar(
            text("SELECT status FROM manual_trade_close_intents WHERE trade_id=:trade"),
            {"trade": trade_id},
        )
        assert tuple(trade) == ("closed", "CLOSED", "MANUAL_RULENIX_CLOSE")
        assert intent == "completed"


@pytest.mark.asyncio
async def test_sl2_reversal_is_exactly_once_and_opposite_side(lifecycle_db):
    factory, user, snapshot, entry_order = lifecycle_db
    trade_id = await open_trade(factory, user, entry_order)
    async with factory() as session, session.begin():
        await session.execute(
            text("""
            INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots)
            VALUES(:user,'futures_breakout_v3','GOLDTEN',TRUE,1)
            """),
            {"user": user},
        )
        await session.execute(
            text("""
            INSERT INTO user_strategy_activations(user_id,strategy_key,is_active,activated_at)
            VALUES(:user,'futures_breakout_v3',TRUE,NOW())
            """),
            {"user": user},
        )
        await session.execute(
            text("""
            UPDATE trades SET status='closed',safety_status='CLOSED',quantity=0,remaining_lots=0,
              exit_reason='SL2',exit_price=97,exit_datetime=NOW()-INTERVAL '1 second',
              broker_net_quantity=0,last_position_reconciled_at=NOW() WHERE id=:trade
            """),
            {"trade": trade_id},
        )
        await session.execute(
            text("""
            INSERT INTO strategy_reversal_intents(
              source_trade_id,user_id,snapshot_id,instrument,source_direction,reversal_direction,
              lots,entry_price,order_session_key)
            VALUES(:trade,:user,:snapshot,'GOLDTEN','BUY','SELL',1,97,:session)
            """),
            {
                "trade": trade_id,
                "user": user,
                "snapshot": snapshot,
                "session": f"r-{trade_id.hex[:30]}",
            },
        )
    coordinator = FakeCoordinator(factory)
    reversal = ReversalLifecycleWorker(factory, coordinator)
    result = await reversal.run_once()
    assert result["submitted"] == 1
    assert (await reversal.run_once())["claimed"] == 0
    assert len(coordinator.places) == 1
    async with factory() as session:
        row = (
            await session.execute(
                text("""
                SELECT o.side,o.role,o.quantity,r.status FROM strategy_reversal_intents r
                JOIN strategy_orders o ON o.user_id=r.user_id AND o.session_key=r.order_session_key
                WHERE r.source_trade_id=:trade
                """),
                {"trade": trade_id},
            )
        ).one()
        assert tuple(row) == ("SELL", "SELL_ENTRY", 10, "submitted")


@pytest.mark.asyncio
async def test_eod_boundary_materializes_one_close_and_repeated_cycle_is_idempotent(lifecycle_db):
    factory, user, _, entry_order = lifecycle_db
    trade_id = await open_trade(factory, user, entry_order)
    async with factory() as session, session.begin():
        await session.execute(
            text("""
            UPDATE trades SET strategy_key='supertrend_index_options_v1',instrument_label='NIFTY_CE'
             WHERE id=:trade
            """),
            {"trade": trade_id},
        )
    coordinator = FakeCoordinator(factory)
    close = RiskReducingCloseWorker(factory, coordinator)
    eod = EodLifecycleWorker(factory, close)
    local_date = datetime.now(UTC).date()
    before = datetime.combine(
        local_date,
        datetime.min.time().replace(hour=15, minute=9),
        tzinfo=__import__("zoneinfo").ZoneInfo("Asia/Kolkata"),
    )
    at = before.replace(minute=10)
    assert (await eod.run_once(before))["due"] is False
    first = await eod.run_once(at)
    assert first["due"] is True and first["submitted"] == 1
    places = len(coordinator.places)
    second = await eod.run_once(at)
    assert second["due"] is True and second["submitted"] == 1
    assert len(coordinator.places) == places
    async with factory() as session:
        counts = (
            await session.execute(
                text("""
                SELECT (SELECT COUNT(*) FROM strategy_execution_intents
                         WHERE trade_id=:trade AND action='SQUARE_OFF'),
                       (SELECT COUNT(*) FROM strategy_orders
                         WHERE trade_id=:trade AND session_key LIKE 'stsq-%')
                """),
                {"trade": trade_id},
            )
        ).one()
        assert tuple(counts) == (1, 1)
