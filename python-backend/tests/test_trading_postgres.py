import asyncio
import os
from uuid import uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.exc import SQLAlchemyError
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine

from app.trading.repository import OwnershipError, TradingRepository


def _database_url() -> str | None:
    url = os.environ.get("TEST_DATABASE_URL")
    if not url:
        return None
    if url.startswith("postgresql://"):
        return url.replace("postgresql://", "postgresql+asyncpg://", 1)
    if url.startswith("postgres://"):
        return url.replace("postgres://", "postgresql+asyncpg://", 1)
    return url


@pytest.mark.asyncio
async def test_postgres_claims_are_skip_locked_and_intent_is_idempotent():
    url = _database_url()
    if not url:
        pytest.skip("TEST_DATABASE_URL is required for Phase 5 stateful tests")
    engine = create_async_engine(url, pool_size=5, max_overflow=5)
    signal_id, intent_id, trade_id, user_id, snapshot_id = uuid4(), uuid4(), uuid4(), None, None
    suffix = signal_id.hex[:12]
    try:
        async with engine.begin() as connection:
            user_id = (await connection.execute(text("SELECT id FROM users ORDER BY created_at LIMIT 1"))).scalar_one()
            snapshot_id = (await connection.execute(text("SELECT id FROM strategy_market_snapshots WHERE status='ready' ORDER BY fetched_at DESC LIMIT 1"))).scalar_one()
            await connection.execute(text("INSERT INTO strategy_signals(id,strategy_key,instrument,session_key,signal_at,signal_type) VALUES(:id,:strategy,'GOLDTEN',:session,NOW(),'ENTRY')"), {"id": signal_id, "strategy": f"phase5_{suffix}", "session": f"phase5_{suffix}"})
            await connection.execute(text("""
                INSERT INTO strategy_execution_intents(id,signal_id,user_id,snapshot_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,status)
                VALUES(:id,:signal,:user,:snapshot,:strategy,'GOLDTEN',:session,'ENTRY','BUY_ENTRY','BUY','MARKET',1,1,100,'pending')
            """), {"id": intent_id, "signal": signal_id, "user": user_id, "snapshot": snapshot_id, "strategy": f"phase5_{suffix}", "session": f"phase5_{suffix}"})
            await connection.execute(text("""
                INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,pnl,entry_datetime,instrument_label,contract_symbol,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status,exit_reason)
                VALUES(:id,:user,'demo','closed','BUY',1,100,0,NOW(),'GOLDTEN','GOLDTEN-P5',:strategy,:snapshot,1,0,'CLOSED','SL2')
            """), {"id": trade_id, "user": user_id, "strategy": f"phase5_{suffix}", "snapshot": snapshot_id})

        async def worker() -> list:
            async with AsyncSession(engine) as session, session.begin():
                return await TradingRepository(session).claim_execution_intents(limit=1)

        first_result, second_result = await asyncio.gather(worker(), worker())
        assert len(first_result) + len(second_result) == 1

        async with engine.begin() as connection:
            await connection.execute(text("UPDATE strategy_execution_intents SET status='pending',claimed_at=NULL WHERE id=:id"), {"id": intent_id})
            async with AsyncSession(bind=connection) as session:
                repo = TradingRepository(session)
                assert await repo.create_sl2_reversal_intent(source_trade_id=trade_id, user_id=user_id, snapshot_id=snapshot_id, instrument="GOLDTEN", source_direction="BUY", lots=1, entry_price="100") is True
                assert await repo.create_sl2_reversal_intent(source_trade_id=trade_id, user_id=user_id, snapshot_id=snapshot_id, instrument="GOLDTEN", source_direction="BUY", lots=1, entry_price="100") is False
    finally:
        async with engine.begin() as connection:
            await connection.execute(text("DELETE FROM strategy_execution_intents WHERE id=:id"), {"id": intent_id})
            await connection.execute(text("DELETE FROM strategy_signals WHERE id=:id"), {"id": signal_id})
            await connection.execute(text("DELETE FROM strategy_reversal_intents WHERE source_trade_id=:id"), {"id": trade_id})
            await connection.execute(text("DELETE FROM trades WHERE id=:id"), {"id": trade_id})
        await engine.dispose()


@pytest.mark.asyncio
async def test_postgres_trigger_rejects_invalid_order_transition():
    url = _database_url()
    if not url:
        pytest.skip("TEST_DATABASE_URL is required for Phase 5 stateful tests")
    engine = create_async_engine(url)
    order_id, user_id, snapshot_id = uuid4(), None, None
    try:
        async with engine.connect() as connection:
            transaction = await connection.begin()
            user_id = (await connection.execute(text("SELECT id FROM users ORDER BY created_at LIMIT 1"))).scalar_one()
            snapshot_id = (await connection.execute(text("SELECT id FROM strategy_market_snapshots WHERE status='ready' ORDER BY fetched_at DESC LIMIT 1"))).scalar_one()
            try:
                await connection.execute(text("""
                    INSERT INTO strategy_orders(id,user_id,snapshot_id,session_key,role,side,execution_mode,lots,quantity,price,status,idempotency_key,client_order_id)
                    VALUES(:id,:user,:snapshot,'phase5-order','BUY_ENTRY','BUY','demo',1,1,100,'pending',:key,:client)
                """), {"id": order_id, "user": user_id, "snapshot": snapshot_id, "key": f"phase5-{order_id}", "client": f"P5{order_id.hex[:18]}"})
                with pytest.raises(SQLAlchemyError):
                    await connection.execute(text("UPDATE strategy_orders SET status='filled' WHERE id=:id"), {"id": order_id})
            finally:
                await transaction.rollback()
    finally:
        await engine.dispose()


@pytest.mark.asyncio
async def test_postgres_fill_deduplication_and_manual_close_uniqueness():
    url = _database_url()
    if not url:
        pytest.skip("TEST_DATABASE_URL is required for Phase 5 stateful tests")
    engine = create_async_engine(url)
    trade_id, order_id = uuid4(), uuid4()
    try:
        async with engine.connect() as connection:
            transaction = await connection.begin()
            try:
                user_id = (await connection.execute(text("SELECT id FROM users ORDER BY created_at LIMIT 1"))).scalar_one()
                snapshot_id = (await connection.execute(text("SELECT id FROM strategy_market_snapshots WHERE status='ready' ORDER BY fetched_at DESC LIMIT 1"))).scalar_one()
                await connection.execute(text("""
                    INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,pnl,entry_datetime,instrument_label,contract_symbol,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status)
                    VALUES(:id,:user,'demo','open','BUY',10,100,0,NOW(),'GOLDTEN','GOLDTEN-P5','phase5_fill',:snapshot,1,1,'DEMO')
                """), {"id": trade_id, "user": user_id, "snapshot": snapshot_id})
                await connection.execute(text("""
                    INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,execution_mode,lots,quantity,price,status,idempotency_key,client_order_id,filled_quantity,processed_quantity)
                    VALUES(:id,:user,:snapshot,:trade,'phase5-fill','BUY_ENTRY','BUY','demo',1,10,100,'submitted',:key,:client,0,0)
                """), {"id": order_id, "user": user_id, "snapshot": snapshot_id, "trade": trade_id, "key": f"phase5-fill-{order_id}", "client": f"P5F{order_id.hex[:17]}"})
                async with AsyncSession(bind=connection) as session:
                    repo = TradingRepository(session)
                    first = await repo.apply_order_fill(order_id=order_id, user_id=user_id, observed_cumulative=4, fill_price="101")
                    duplicate = await repo.apply_order_fill(order_id=order_id, user_id=user_id, observed_cumulative=4, fill_price="999")
                    assert first is not None and first.delta_quantity == 4
                    assert duplicate is not None and duplicate.delta_quantity == 0
                    with pytest.raises(OwnershipError):
                        await repo.apply_order_fill(order_id=order_id, user_id=uuid4(), observed_cumulative=5, fill_price="102")
                    assert await repo.request_manual_close(trade_id=trade_id, user_id=user_id, requested_quantity=10) is True
                    assert await repo.request_manual_close(trade_id=trade_id, user_id=user_id, requested_quantity=10) is False
            finally:
                await transaction.rollback()
    finally:
        await engine.dispose()
