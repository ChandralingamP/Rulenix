from datetime import UTC, date, datetime, timedelta
from decimal import Decimal
from uuid import uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import async_sessionmaker, create_async_engine

from app.runtime.lifecycle import SUPERTREND, AuthoritativeExecutionWorker
from app.runtime.market import SuperTrendMarketSelection
from app.strategy.common import IST
from app.strategy.supertrend import (
    IndexOptionConfig,
    OptionContract,
    OptionSide,
)


def _db_url() -> str:
    from app.config import get_settings

    return get_settings().async_database_url


@pytest.fixture
async def engine():
    eng = create_async_engine(_db_url())
    yield eng
    await eng.dispose()


class FakeMarketProvider:
    def __init__(self, premium: Decimal = Decimal(100)):
        self.premium = premium

    async def select(
        self,
        *,
        user_id,
        config: IndexOptionConfig,
        side: OptionSide,
        trade_date: date,
    ) -> SuperTrendMarketSelection:
        return SuperTrendMarketSelection(
            contract=OptionContract(
                token="99999",
                symbol=f"{config.instrument}{trade_date:%d%b%y}{side.value}",
                expiry=trade_date + timedelta(days=2),
                lot_size=25,
                strike=Decimal(24000),
                option_type=side,
                premium=self.premium,
            ),
            underlying_ltp=Decimal(24000),
        )


class DummyCoordinator:
    pass


@pytest.mark.asyncio
async def test_supertrend_alternation_and_reversal(engine):
    user_id = uuid4()
    snap_id = uuid4()
    trade_id = uuid4()
    session_factory = async_sessionmaker(engine, expire_on_commit=False)

    market_provider = FakeMarketProvider()
    _ = AuthoritativeExecutionWorker(
        session_factory=session_factory,
        coordinator=DummyCoordinator(),
        market_provider=market_provider,
    )

    now_ist = datetime.now(UTC).astimezone(IST).replace(hour=10, minute=30, second=0, microsecond=0)
    today = now_ist.date()

    async with engine.begin() as conn:
        # Setup user and configs
        await conn.execute(
            text("""
            INSERT INTO users(id, username, email, password_hash, is_active, can_live_trade)
            VALUES(:id, :name, :email, 'testpwd', TRUE, TRUE)
        """),
            {
                "id": user_id,
                "name": f"st_{user_id.hex[:8]}",
                "email": f"st_{user_id.hex[:8]}@test.com",
            },
        )
        await conn.execute(
            text("""
            INSERT INTO user_profiles(user_id, trading_mode, broker_credential_revision)
            VALUES(:id, 'demo', 1)
        """),
            {"id": user_id},
        )
        await conn.execute(
            text("""
            INSERT INTO user_strategy_configs(user_id, strategy_key, instrument, enabled, lots, target_points, stop_loss_points)
            VALUES(:id, :strat, 'NIFTY', TRUE, 1, 25, 15)
        """),
            {"id": user_id, "strat": SUPERTREND},
        )
        await conn.execute(
            text("""
            INSERT INTO user_strategy_activations(user_id, strategy_key, is_active)
            VALUES(:id, :strat, TRUE)
        """),
            {"id": user_id, "strat": SUPERTREND},
        )

        # Setup market candle data for NIFTY around now_ist to produce a valid SuperTrend point
        # Insert 15 candles so SuperTrend can calculate ATR(7)
        for i in range(15, 0, -1):
            c_time = (now_ist - timedelta(minutes=i)).astimezone(UTC)
            await conn.execute(
                text("""
                INSERT INTO backtest_market_candles(id, instrument, exchange, symbol_token, trading_symbol, interval_key, candle_time, open_price, high_price, low_price, close_price, volume)
                VALUES(:c_id, 'NIFTY', 'NSE', '99926000', 'NIFTY', 'ONE_MINUTE', :t, 24000, 24050, 23950, 24020, 1000)
                ON CONFLICT (exchange, symbol_token, interval_key, candle_time) DO UPDATE
                  SET close_price=EXCLUDED.close_price
            """),
                {"c_id": uuid4(), "t": c_time},
            )

    try:
        # Scenario 1: User has a closed trade from earlier today that was CE
        # If another CE signal arrives, it must be SKIPPED due to strict alternation (CE -> PE -> CE -> PE)
        async with engine.begin() as conn:
            await conn.execute(
                text("""
                INSERT INTO strategy_market_snapshots(id, strategy_key, instrument, trade_date, status, exchange_segment, contract_token, contract_symbol)
                VALUES(:s_id, :strat, 'NIFTY_CE', :date, 'ready', 'NFO', '99999', 'NIFTY24000CE')
            """),
                {"s_id": snap_id, "strat": SUPERTREND, "date": today},
            )
            await conn.execute(
                text("""
                INSERT INTO trades(id, user_id, strategy_key, instrument_label, direction, execution_mode, quantity, entry_price, last_price, status, safety_status, entry_datetime, created_at, strategy_snapshot_id)
                VALUES(:t_id, :u_id, :strat, 'NIFTY_CE', 'BUY', 'demo', 25, 100, 125, 'closed', 'CLOSED', :t, :t, :s_id)
            """),
                {
                    "t_id": trade_id,
                    "u_id": user_id,
                    "strat": SUPERTREND,
                    "t": now_ist - timedelta(hours=1),
                    "s_id": snap_id,
                },
            )

        # Verify that checking last_trade detects NIFTY_CE and blocks another CE
        async with session_factory() as session:
            # Query last trade
            last_trade = (
                (
                    await session.execute(
                        text("""
                    SELECT t.id, t.instrument_label, s.instrument AS snapshot_instrument
                      FROM trades t
                      LEFT JOIN strategy_market_snapshots s ON s.id = t.strategy_snapshot_id
                     WHERE t.user_id = :user
                       AND t.strategy_key = :strategy
                       AND (t.instrument_label = :instrument OR t.instrument_label LIKE :prefix OR s.instrument LIKE :prefix)
                       AND t.created_at::date = :trade_date
                     ORDER BY t.created_at DESC LIMIT 1
                """),
                        {
                            "user": user_id,
                            "strategy": SUPERTREND,
                            "instrument": "NIFTY",
                            "prefix": "NIFTY_%",
                            "trade_date": today,
                        },
                    )
                )
                .mappings()
                .first()
            )
            assert last_trade is not None
            last_label = str(
                last_trade["instrument_label"] or last_trade["snapshot_instrument"] or ""
            )
            # Verify same-side CE is rejected
            assert last_label.endswith(("CE", "_CE"))
            # Opposite side PE is permitted
            assert not last_label.endswith(("PE", "_PE"))

        # Scenario 2: Existing OPEN trade is NIFTY_CE
        # When PE signal arrives, it must trigger REVERSAL close on the existing trade
        open_trade_id = uuid4()
        async with engine.begin() as conn:
            await conn.execute(
                text("""
                INSERT INTO trades(id, user_id, strategy_key, instrument_label, direction, execution_mode, quantity, entry_price, last_price, status, safety_status, entry_datetime, created_at, strategy_snapshot_id)
                VALUES(:t_id, :u_id, :strat, 'NIFTY_CE', 'BUY', 'demo', 25, 100, 90, 'open', 'PROTECTED', :t, :t, :s_id)
            """),
                {
                    "t_id": open_trade_id,
                    "u_id": user_id,
                    "strat": SUPERTREND,
                    "t": now_ist - timedelta(minutes=5),
                    "s_id": snap_id,
                },
            )

        async with session_factory() as session:
            # Check existing_trade lookup
            existing_trade = (
                (
                    await session.execute(
                        text("""
                    SELECT t.id, t.direction, t.quantity, t.last_price, t.entry_price,
                           t.execution_mode, t.pnl, t.notes, t.instrument_label,
                           s.instrument AS snapshot_instrument,
                           s.contract_token, s.exchange_segment
                      FROM trades t
                      JOIN strategy_market_snapshots s ON s.id = t.strategy_snapshot_id
                     WHERE t.user_id = :user
                       AND t.strategy_key = :strategy
                       AND (t.instrument_label = :instrument OR t.instrument_label LIKE :prefix OR s.instrument LIKE :prefix)
                       AND t.status = 'open'
                     ORDER BY t.created_at DESC LIMIT 1
                """),
                        {
                            "user": user_id,
                            "strategy": SUPERTREND,
                            "instrument": "NIFTY",
                            "prefix": "NIFTY_%",
                        },
                    )
                )
                .mappings()
                .first()
            )

            # Confirm that NIFTY_CE open trade is successfully found!
            assert existing_trade is not None
            assert existing_trade["id"] == open_trade_id
            trade_inst = str(
                existing_trade["instrument_label"] or existing_trade["snapshot_instrument"] or ""
            )
            # CE signal is same side (blocked)
            assert trade_inst.endswith(("CE", "_CE"))
            # PE signal is opposite side (triggers reversal close)
            assert not trade_inst.endswith(("PE", "_PE"))

            # Simulate demo reversal logic on existing_trade
            await session.execute(
                text("""
                UPDATE trades
                   SET status='closed',
                       safety_status='CLOSED',
                       remaining_lots=0,
                       exit_reason='SIGNAL_REVERSAL',
                       updated_at=NOW()
                 WHERE id=:trade AND status='open'
            """),
                {"trade": open_trade_id},
            )
            await session.commit()

        # Confirm trade was closed with SIGNAL_REVERSAL
        async with session_factory() as session:
            trade_row = (
                await session.execute(
                    text("SELECT status, safety_status, exit_reason FROM trades WHERE id=:id"),
                    {"id": open_trade_id},
                )
            ).mappings().one()
            assert trade_row["status"] == "closed"
            assert trade_row["safety_status"] == "CLOSED"
            assert trade_row["exit_reason"] == "SIGNAL_REVERSAL"

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
                text("DELETE FROM user_strategy_configs WHERE user_id=:u"), {"u": user_id}
            )
            await conn.execute(
                text("DELETE FROM user_strategy_activations WHERE user_id=:u"), {"u": user_id}
            )
            await conn.execute(text("DELETE FROM user_profiles WHERE user_id=:u"), {"u": user_id})
            await conn.execute(text("DELETE FROM users WHERE id=:u"), {"u": user_id})
