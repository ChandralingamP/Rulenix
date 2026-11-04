from datetime import UTC, datetime, timedelta
from decimal import Decimal
from unittest.mock import MagicMock
from uuid import uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine

from app.api.admin import (
    clear_trade_logs,
)
from app.dependencies import Principal
from app.errors import DomainError
from app.reconciliation.domain import (
    BrokerFill,
    BrokerOrder,
    BrokerPosition,
    ManualCloseClassification,
    ReadEvidence,
    ReconciliationSnapshot,
    classify_manual_broker_close,
    classify_order,
    classify_position,
    weighted_fill_price,
)
from app.reconciliation.service import ReconciliationService


def _db_url() -> str:
    from app.config import get_settings

    return get_settings().async_database_url


@pytest.fixture
async def engine():
    eng = create_async_engine(_db_url())
    yield eng
    await eng.dispose()


@pytest.mark.asyncio
async def test_reconciliation_domain_unit():
    # 1. Position classification
    matched_pos = BrokerPosition(
        token="123", symbol="GOLD", exchange="MCX", quantity=10, average_price=Decimal(50000)
    )
    assert (
        classify_position(
            local_quantity=10,
            local_symbol="GOLD",
            local_token="123",
            broker=matched_pos,
            local_open=True,
        ).value
        == "matched"
    )
    assert (
        classify_position(
            local_quantity=10, local_symbol="GOLD", local_token="123", broker=None, local_open=True
        ).value
        == "broker_flat_local_open"
    )
    assert (
        classify_position(
            local_quantity=5,
            local_symbol="GOLD",
            local_token="123",
            broker=matched_pos,
            local_open=True,
        ).value
        == "quantity_mismatch"
    )
    assert (
        classify_position(
            local_quantity=10,
            local_symbol="SILVER",
            local_token="123",
            broker=matched_pos,
            local_open=True,
        ).value
        == "side_or_symbol_mismatch"
    )

    # 2. Order classification
    broker_ord = BrokerOrder(order_id="b1", status="complete", quantity=10, filled_quantity=10)
    assert (
        classify_order(local_status="open", broker=broker_ord, read_succeeded=True).value
        == "filled"
    )
    assert (
        classify_order(local_status="open", broker=None, read_succeeded=False).value == "ambiguous"
    )
    assert classify_order(local_status="open", broker=None, read_succeeded=True).value == "missing"

    # 3. Weighted fill price
    entry = datetime(2026, 10, 1, 10, 0, tzinfo=UTC)
    fills = [
        BrokerFill(
            trade_id="t1",
            order_id="o1",
            token="123",
            symbol="GOLD",
            exchange="MCX",
            side="SELL",
            quantity=4,
            price=Decimal(100),
            filled_at=entry,
        ),
        BrokerFill(
            trade_id="t2",
            order_id="o2",
            token="123",
            symbol="GOLD",
            exchange="MCX",
            side="SELL",
            quantity=6,
            price=Decimal(110),
            filled_at=entry,
        ),
    ]
    assert weighted_fill_price(fills) == Decimal(106)

    # 4. Manual broker close classification
    result = classify_manual_broker_close(
        local_open=True,
        local_symbol="GOLD",
        local_token="123",
        local_exchange="MCX",
        entry_at=entry - timedelta(hours=1),
        remaining_quantity=10,
        broker_position=None,
        fills=fills,
        order_book_succeeded=True,
        positions_succeeded=True,
        fills_succeeded=True,
    )
    assert result is ManualCloseClassification.MANUAL_BROKER_CLOSE


@pytest.mark.asyncio
async def test_reconciliation_service_in_postgres(engine):
    user_id = uuid4()
    async with engine.begin() as conn:
        await conn.execute(
            text("""
            INSERT INTO users(id, username, email, password_hash, is_active, can_live_trade)
            VALUES(:id, :name, :email, 'testpwd', TRUE, TRUE)
        """),
            {
                "id": user_id,
                "name": f"recon_{user_id.hex[:8]}",
                "email": f"recon_{user_id.hex[:8]}@test.com",
            },
        )
        await conn.execute(
            text("""
            INSERT INTO user_profiles(user_id, trading_mode, broker_credential_revision)
            VALUES(:id, 'live', 1)
        """),
            {"id": user_id},
        )

    # Authoritative empty snapshot
    snapshot = ReconciliationSnapshot(
        user_id=user_id,
        account_id="A123",
        credential_revision=1,
        egress_identity="127.0.0.1",
        positions=ReadEvidence.success([], credential_revision=1),
        orders=ReadEvidence.success([], credential_revision=1),
        fills=ReadEvidence.success([], credential_revision=1),
        conditional_rules=ReadEvidence.success([], credential_revision=1),
        account_validation=ReadEvidence.success(True, credential_revision=1),
    )

    async with AsyncSession(engine, expire_on_commit=False) as session:
        service = ReconciliationService(session)
        res = await service.apply_snapshot(snapshot)
        assert res.authoritative is True
        assert res.healthy is True
        assert res.blockers == 0
        assert res.incidents == 0

    # Verify health record in DB
    async with engine.connect() as conn:
        row = (
            (
                await conn.execute(
                    text(
                        "SELECT healthy, broker_credential_revision FROM broker_reconciliation_health WHERE user_id=:id"
                    ),
                    {"id": user_id},
                )
            )
            .mappings()
            .one()
        )
        assert row["healthy"] is True
        assert row["broker_credential_revision"] == 1

    # Cleanup user
    async with engine.begin() as conn:
        await conn.execute(
            text("DELETE FROM broker_reconciliation_health WHERE user_id=:id"), {"id": user_id}
        )
        await conn.execute(text("DELETE FROM user_profiles WHERE user_id=:id"), {"id": user_id})
        await conn.execute(text("DELETE FROM users WHERE id=:id"), {"id": user_id})


@pytest.mark.asyncio
async def test_clear_trade_logs_kill_switch_guard(engine):
    user_id = uuid4()
    admin_id = uuid4()
    username = f"user_{user_id.hex[:8]}"
    snap_id = uuid4()

    async with engine.begin() as conn:
        await conn.execute(
            text("""
            INSERT INTO users(id, username, email, password_hash, is_active, can_administer)
            VALUES(:id, :name, :email, 'test', TRUE, TRUE)
        """),
            {
                "id": admin_id,
                "name": f"admin_{admin_id.hex[:8]}",
                "email": f"admin_{admin_id.hex[:8]}@test.com",
            },
        )
        await conn.execute(
            text("""
            INSERT INTO users(id, username, email, password_hash, is_active)
            VALUES(:id, :name, :email, 'test', TRUE)
        """),
            {"id": user_id, "name": username, "email": f"{username}@test.com"},
        )
        await conn.execute(
            text("""
            INSERT INTO user_profiles(user_id, trading_mode) VALUES(:id, 'demo')
        """),
            {"id": user_id},
        )
        # Ensure global kill switch is OFF
        await conn.execute(
            text("""
            INSERT INTO risk_kill_switches(user_id, enabled)
            VALUES(NULL, FALSE)
            ON CONFLICT ((TRUE)) WHERE user_id IS NULL DO UPDATE SET enabled=FALSE
        """)
        )
        # Insert market snapshot
        await conn.execute(
            text("""
            INSERT INTO strategy_market_snapshots(id, strategy_key, instrument, trade_date, status, exchange_segment, contract_token, contract_symbol)
            VALUES(:s_id, 'supertrend', 'NIFTY', '2026-10-06', 'ready', 'MCX', '12345', 'NIFTYFUT')
        """),
            {"s_id": snap_id},
        )

    actor = Principal(
        id=str(admin_id),
        username="admin",
        can_administer=True,
        can_live_trade=True,
        can_backtest=True,
        can_backtest_on_trading_days=True,
        trading_mode="demo",
        session_id="test_sess",
        csrf_hash=b"",
    )
    req = MagicMock()

    try:
        # 1. Attempt to clear demo trades with kill switch OFF -> must raise DomainError(400)
        async with AsyncSession(engine, expire_on_commit=False) as session:
            with pytest.raises(DomainError) as exc_info:
                await clear_trade_logs(
                    request=req,
                    payload={"username": username, "scope": "demo"},
                    _=actor,
                    db=session,
                )
            assert "global kill switch" in exc_info.value.detail.lower()

        # 2. Turn kill switch ON
        async with engine.begin() as conn:
            await conn.execute(
                text("""
                INSERT INTO risk_kill_switches(user_id, enabled)
                VALUES(NULL, TRUE)
                ON CONFLICT ((TRUE)) WHERE user_id IS NULL DO UPDATE SET enabled=TRUE
            """)
            )

        # 3. Create demo trades, demo orders, and backtest runs
        async with engine.begin() as conn:
            trade_id = uuid4()
            order_id = uuid4()
            run_id = uuid4()
            await conn.execute(
                text("""
                INSERT INTO trades(id, user_id, strategy_key, instrument_label, direction, execution_mode, quantity, entry_price, status, safety_status, entry_datetime, strategy_snapshot_id)
                VALUES(:t_id, :u_id, 'supertrend', 'NIFTY', 'BUY', 'demo', 50, 22000, 'open', 'DEMO', NOW(), :s_id)
            """),
                {"t_id": trade_id, "u_id": user_id, "s_id": snap_id},
            )
            await conn.execute(
                text("""
                INSERT INTO strategy_orders(id, user_id, trade_id, snapshot_id, side, execution_mode, quantity, price, status, role, session_key, lots, idempotency_key)
                VALUES(:o_id, :u_id, :t_id, :s_id, 'BUY', 'demo', 50, 22000, 'open', 'BUY_ENTRY', 'regular', 1, 'idem-1')
            """),
                {"o_id": order_id, "u_id": user_id, "t_id": trade_id, "s_id": snap_id},
            )
            await conn.execute(
                text("""
                INSERT INTO backtest_runs(id, user_id, strategy_key, instrument, trading_symbol, symbol_token, interval_key, lookback_months, from_time, to_time, lots, lot_size, status)
                VALUES(:r_id, :u_id, 'supertrend', 'NIFTY', 'NIFTYFUT', '12345', 'ONE_MINUTE', 1, NOW(), NOW(), 1, 50, 'completed')
            """),
                {"r_id": run_id, "u_id": user_id},
            )

        # 4. Clear demo trades with kill switch ON
        async with AsyncSession(engine, expire_on_commit=False) as session:
            res = await clear_trade_logs(
                request=req,
                payload={"username": username, "scope": "demo"},
                _=actor,
                db=session,
            )
            assert res["scope"] == "demo"
            assert res["deleted_demo_trades"] == 1
            assert res["deleted_demo_orders"] == 1
            assert res["deleted_backtest_runs"] == 1

        # Verify DB is clean for demo
        async with engine.connect() as conn:
            t_cnt = await conn.scalar(
                text("SELECT COUNT(*) FROM trades WHERE user_id=:u_id"), {"u_id": user_id}
            )
            o_cnt = await conn.scalar(
                text("SELECT COUNT(*) FROM strategy_orders WHERE user_id=:u_id"), {"u_id": user_id}
            )
            b_cnt = await conn.scalar(
                text("SELECT COUNT(*) FROM backtest_runs WHERE user_id=:u_id"), {"u_id": user_id}
            )
            assert t_cnt == 0
            assert o_cnt == 0
            assert b_cnt == 0

    finally:
        # Cleanup
        async with engine.begin() as conn:
            await conn.execute(
                text("""
                INSERT INTO risk_kill_switches(user_id, enabled)
                VALUES(NULL, FALSE)
                ON CONFLICT ((TRUE)) WHERE user_id IS NULL DO UPDATE SET enabled=FALSE
            """)
            )
            await conn.execute(text("DELETE FROM strategy_orders WHERE user_id=:u"), {"u": user_id})
            await conn.execute(text("DELETE FROM trades WHERE user_id=:u"), {"u": user_id})
            await conn.execute(
                text("DELETE FROM strategy_market_snapshots WHERE id=:s"), {"s": snap_id}
            )
            await conn.execute(
                text("DELETE FROM user_profiles WHERE user_id IN (:u, :a)"),
                {"u": user_id, "a": admin_id},
            )
            await conn.execute(
                text("DELETE FROM users WHERE id IN (:u, :a)"), {"u": user_id, "a": admin_id}
            )


@pytest.mark.asyncio
async def test_clear_trade_logs_live_blockers(engine):
    user_id = uuid4()
    admin_id = uuid4()
    username = f"user_{user_id.hex[:8]}"

    async with engine.begin() as conn:
        await conn.execute(
            text("""
            INSERT INTO users(id, username, email, password_hash, is_active, can_administer)
            VALUES(:id, :name, :email, 'test', TRUE, TRUE)
        """),
            {
                "id": admin_id,
                "name": f"admin_{admin_id.hex[:8]}",
                "email": f"admin_{admin_id.hex[:8]}@test.com",
            },
        )
        await conn.execute(
            text("""
            INSERT INTO users(id, username, email, password_hash, is_active, can_live_trade)
            VALUES(:id, :name, :email, 'test', TRUE, TRUE)
        """),
            {"id": user_id, "name": username, "email": f"{username}@test.com"},
        )
        await conn.execute(
            text("""
            INSERT INTO user_profiles(user_id, trading_mode) VALUES(:id, 'live')
        """),
            {"id": user_id},
        )
        # Enable kill switch
        await conn.execute(
            text("""
            INSERT INTO risk_kill_switches(user_id, enabled)
            VALUES(NULL, TRUE)
            ON CONFLICT ((TRUE)) WHERE user_id IS NULL DO UPDATE SET enabled=TRUE
        """)
        )

    actor = Principal(
        id=str(admin_id),
        username="admin",
        can_administer=True,
        can_live_trade=True,
        can_backtest=True,
        can_backtest_on_trading_days=True,
        trading_mode="demo",
        session_id="test_sess",
        csrf_hash=b"",
    )
    req = MagicMock()
    req.app.state.settings = None  # No real broker connection

    try:
        # Insert an OPEN live trade with valid safety_status 'PROTECTED'
        live_trade_id = uuid4()
        async with engine.begin() as conn:
            await conn.execute(
                text("""
                INSERT INTO trades(id, user_id, strategy_key, instrument_label, direction, execution_mode, quantity, entry_price, status, safety_status, entry_datetime)
                VALUES(:t_id, :u_id, 'supertrend', 'NIFTY', 'BUY', 'live', 50, 22000, 'open', 'PROTECTED', NOW())
            """),
                {"t_id": live_trade_id, "u_id": user_id},
            )

        # Attempting clear LIVE must fail because open live trade exists
        async with AsyncSession(engine, expire_on_commit=False) as session:
            with pytest.raises(DomainError) as exc_info:
                await clear_trade_logs(
                    request=req,
                    payload={"username": username, "scope": "live"},
                    _=actor,
                    db=session,
                )
            assert "open_live_trades=1" in exc_info.value.detail

        # Close the trade properly with safety_status='CLOSED'
        async with engine.begin() as conn:
            await conn.execute(
                text("""
                UPDATE trades SET status='closed', safety_status='CLOSED', exit_datetime=NOW(), exit_price=22100, pnl=5000
                WHERE id=:id
            """),
                {"id": live_trade_id},
            )

        # Now clear LIVE should succeed because safety_status is CLOSED and no broker blockers exist
        async with AsyncSession(engine, expire_on_commit=False) as session:
            res = await clear_trade_logs(
                request=req,
                payload={"username": username, "scope": "live"},
                _=actor,
                db=session,
            )
            assert res["scope"] == "live"
            assert res["deleted_live_trades"] == 1

        # Verify DB is clean
        async with engine.connect() as conn:
            cnt = await conn.scalar(
                text("SELECT COUNT(*) FROM trades WHERE user_id=:u_id"), {"u_id": user_id}
            )
            assert cnt == 0

    finally:
        async with engine.begin() as conn:
            await conn.execute(
                text("""
                INSERT INTO risk_kill_switches(user_id, enabled)
                VALUES(NULL, FALSE)
                ON CONFLICT ((TRUE)) WHERE user_id IS NULL DO UPDATE SET enabled=FALSE
            """)
            )
            await conn.execute(text("DELETE FROM trades WHERE user_id=:u"), {"u": user_id})
            await conn.execute(
                text("DELETE FROM user_profiles WHERE user_id IN (:u, :a)"),
                {"u": user_id, "a": admin_id},
            )
            await conn.execute(
                text("DELETE FROM users WHERE id IN (:u, :a)"), {"u": user_id, "a": admin_id}
            )


@pytest.mark.asyncio
async def test_external_manual_close_and_stale_order_reconciliation(engine):
    user_id = uuid4()
    snap_id = uuid4()
    trade_id = uuid4()
    order_id = uuid4()
    intent_id = uuid4()
    sig_id = uuid4()
    sess_key = f"sess_{user_id.hex[:8]}"

    async with engine.begin() as conn:
        await conn.execute(
            text("""
            INSERT INTO users(id, username, email, password_hash, is_active, can_live_trade)
            VALUES(:id, :name, :email, 'testpwd', TRUE, TRUE)
        """),
            {
                "id": user_id,
                "name": f"recon_{user_id.hex[:8]}",
                "email": f"recon_{user_id.hex[:8]}@test.com",
            },
        )
        await conn.execute(
            text("""
            INSERT INTO user_profiles(user_id, trading_mode, broker_credential_revision)
            VALUES(:id, 'live', 1)
        """),
            {"id": user_id},
        )
        await conn.execute(
            text("""
            INSERT INTO strategy_market_snapshots(id, strategy_key, instrument, trade_date, status, exchange_segment, contract_token, contract_symbol)
            VALUES(:s_id, 'supertrend', 'NIFTY', '2026-10-06', 'ready', 'MCX', '55555', 'NIFTYFUT')
        """),
            {"s_id": snap_id},
        )
        # Insert an execution intent linked to the order
        await conn.execute(
            text("""
            INSERT INTO strategy_signals(id, strategy_key, instrument, session_key, signal_at, signal_type)
            VALUES(:sig_id, 'supertrend', 'NIFTY', :sess, NOW() - INTERVAL '2 days', 'ENTRY')
        """),
            {"sig_id": sig_id, "sess": sess_key},
        )
        # Insert a live trade from yesterday
        await conn.execute(
            text("""
            INSERT INTO trades(id, user_id, strategy_key, instrument_label, direction, execution_mode, quantity, entry_price, last_price, status, safety_status, entry_datetime, strategy_snapshot_id)
            VALUES(:t_id, :u_id, 'supertrend', 'NIFTY', 'BUY', 'live', 50, 22000, 22050, 'open', 'PROTECTED', NOW() - INTERVAL '2 days', :s_id)
        """),
            {"t_id": trade_id, "u_id": user_id, "s_id": snap_id},
        )
        # Insert a live child order (e.g. SL1) from yesterday
        await conn.execute(
            text("""
            INSERT INTO strategy_orders(id, user_id, trade_id, snapshot_id, side, execution_mode, quantity, price, status, role, session_key, lots, idempotency_key, created_at)
            VALUES(:o_id, :u_id, :t_id, :s_id, 'SELL', 'live', 50, 21900, 'submitted', 'SL1', :sess, 1, 'idem-recon-1', NOW() - INTERVAL '2 days')
        """),
            {
                "o_id": order_id,
                "u_id": user_id,
                "t_id": trade_id,
                "s_id": snap_id,
                "sess": sess_key,
            },
        )
        await conn.execute(
            text("""
            INSERT INTO strategy_execution_intents(id, signal_id, user_id, trade_id, strategy_order_id, snapshot_id, strategy_key, instrument, session_key, action, role, side, order_type, lots, quantity, price, status, created_at)
            VALUES(:i_id, :sig_id, :u_id, :t_id, :o_id, :s_id, 'supertrend', 'NIFTY', :sess, 'ENTRY', 'SL1', 'SELL', 'LIMIT', 1, 50, 21900, 'submitted', NOW() - INTERVAL '2 days')
        """),
            {
                "i_id": intent_id,
                "sig_id": sig_id,
                "u_id": user_id,
                "t_id": trade_id,
                "o_id": order_id,
                "s_id": snap_id,
                "sess": sess_key,
            },
        )

    # The broker position is FLAT (empty list). Notice: NO manual_trade_close_intents was created!
    snapshot = ReconciliationSnapshot(
        user_id=user_id,
        account_id="A123",
        credential_revision=1,
        egress_identity="127.0.0.1",
        positions=ReadEvidence.success([], credential_revision=1),
        orders=ReadEvidence.success([], credential_revision=1),
        fills=ReadEvidence.success([], credential_revision=1),
        conditional_rules=ReadEvidence.success([], credential_revision=1),
        account_validation=ReadEvidence.success(True, credential_revision=1),
    )

    try:
        async with AsyncSession(engine, expire_on_commit=False) as session:
            service = ReconciliationService(session)
            res = await service.apply_snapshot(snapshot)
            assert res.authoritative is True

        # Verify:
        # 1. Trade is CLOSED with safety_status='CLOSED' and exit_reason='MANUAL_BROKER_CLOSE'
        async with engine.connect() as conn:
            trade_row = (
                (
                    await conn.execute(
                        text("SELECT status, safety_status, exit_reason FROM trades WHERE id=:id"),
                        {"id": trade_id},
                    )
                )
                .mappings()
                .one()
            )
            assert trade_row["status"] == "closed"
            assert trade_row["safety_status"] == "CLOSED"
            assert trade_row["exit_reason"] == "MANUAL_BROKER_CLOSE"

            # 2. Child order is CANCELLED
            order_row = (
                (
                    await conn.execute(
                        text("SELECT status FROM strategy_orders WHERE id=:id"),
                        {"id": order_id},
                    )
                )
                .mappings()
                .one()
            )
            assert order_row["status"] == "cancelled"

            # 3. Execution intent is EXPIRED
            intent_row = (
                (
                    await conn.execute(
                        text("SELECT status FROM strategy_execution_intents WHERE id=:id"),
                        {"id": intent_id},
                    )
                )
                .mappings()
                .one()
            )
            assert intent_row["status"] == "expired"

            # 4. No active orders or intents remain
            active_orders = await conn.scalar(
                text(
                    "SELECT COUNT(*) FROM strategy_orders WHERE user_id=:u AND status IN ('pending','submitted','open','trigger_pending','partially_filled','cancelling')"
                ),
                {"u": user_id},
            )
            assert active_orders == 0
            active_intents = await conn.scalar(
                text(
                    "SELECT COUNT(*) FROM strategy_execution_intents WHERE user_id=:u AND status IN ('pending','claimed','retry_wait','submitted')"
                ),
                {"u": user_id},
            )
            assert active_intents == 0

    finally:
        async with engine.begin() as conn:
            await conn.execute(
                text("DELETE FROM strategy_execution_intents WHERE user_id=:u"), {"u": user_id}
            )
            await conn.execute(text("DELETE FROM strategy_orders WHERE user_id=:u"), {"u": user_id})
            await conn.execute(text("DELETE FROM trades WHERE user_id=:u"), {"u": user_id})
            await conn.execute(
                text("DELETE FROM strategy_market_snapshots WHERE id=:s"), {"s": snap_id}
            )
            await conn.execute(
                text("DELETE FROM broker_reconciliation_health WHERE user_id=:u"), {"u": user_id}
            )
            await conn.execute(text("DELETE FROM user_profiles WHERE user_id=:u"), {"u": user_id})
            await conn.execute(text("DELETE FROM users WHERE id=:u"), {"u": user_id})
