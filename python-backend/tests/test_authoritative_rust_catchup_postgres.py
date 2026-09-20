import os
from datetime import UTC, datetime
from decimal import Decimal
from uuid import uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine

from app.api.pnl import close_trade
from app.dependencies import Principal
from app.errors import DomainError
from app.reconciliation.domain import ManualCloseEvidence
from app.reconciliation.repository import ReconciliationRepository


def _url() -> str | None:
    value = os.environ.get("TEST_DATABASE_URL")
    return (
        value.replace("postgresql://", "postgresql+asyncpg://", 1).replace(
            "postgres://", "postgresql+asyncpg://", 1
        )
        if value
        else None
    )


def _principal(user_id) -> Principal:
    return Principal(
        str(user_id), "catchup", False, False, False, False, "demo", "session", b""
    )


@pytest.mark.asyncio
async def test_demo_close_is_owned_idempotent_and_local_only() -> None:
    url = _url()
    if not url:
        pytest.skip("TEST_DATABASE_URL is not configured; no shared database is used")
    engine = create_async_engine(url)
    user, other, snapshot, trade, order = (uuid4() for _ in range(5))
    token = f"demo-close-{trade.hex[:12]}"
    try:
        async with engine.begin() as connection:
            for identity, label in ((user, "owner"), (other, "other")):
                await connection.execute(text("""
                    INSERT INTO users(id,username,email,password_hash)
                    VALUES(:id,:name,:email,'test')
                """), {"id": identity, "name": f"catchup-{label}-{identity.hex[:8]}",
                         "email": f"catchup-{label}-{identity.hex[:8]}@test.invalid"})
            await connection.execute(text("""
                INSERT INTO strategy_market_snapshots(
                  id,strategy_key,instrument,trade_date,status,contract_token,contract_symbol,
                  lot_size,exchange_segment,product_type,execution_key)
                VALUES(:id,'futures_breakout_v3','GOLDTEN',CURRENT_DATE,'ready',:token,
                  'GOLDTEN-DEMO-FUT',10,'MCX','CARRYFORWARD',:key)
            """), {"id": snapshot, "token": token, "key": f"demo-close-{trade.hex}"})
            await connection.execute(text("""
                INSERT INTO trades(
                  id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,
                  entry_datetime,instrument_label,contract_symbol,strategy_key,strategy_snapshot_id,
                  total_lots,remaining_lots,safety_status)
                VALUES(:id,:user,'demo','open','BUY',20,100,100,0,NOW(),'GOLDTEN',
                  'GOLDTEN-DEMO-FUT','futures_breakout_v3',:snapshot,2,2,'DEMO')
            """), {"id": trade, "user": user, "snapshot": snapshot})
            await connection.execute(text("""
                INSERT INTO strategy_orders(
                  id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,
                  lots,quantity,price,trigger_price,status,broker_order_id,idempotency_key,client_order_id)
                VALUES(:id,:user,:snapshot,:trade,'demo-close','SL1','SELL','STOPLOSS_MARKET',
                  'demo',2,20,99,99,'submitted','DEMO-STOP',:key,'DEMO-STOP-TAG')
            """), {"id": order, "user": user, "snapshot": snapshot, "trade": trade,
                     "key": f"demo-close-order-{order.hex}"})
            await connection.execute(text("""
                INSERT INTO market_price_ticks(exchange_segment,contract_token,price,received_at)
                VALUES('MCX',:token,98,NOW())
                ON CONFLICT(exchange_segment,contract_token) DO UPDATE
                  SET price=EXCLUDED.price,received_at=EXCLUDED.received_at
            """), {"token": token})

        async with AsyncSession(engine, expire_on_commit=False) as session:
            with pytest.raises(DomainError) as denied:
                await close_trade(trade, _principal(other), session)
            assert denied.value.status_code == 404
            await session.rollback()

            result = await close_trade(trade, _principal(user), session)
            assert result["status"] == "completed"
            assert result["execution_mode"] == "demo"
            assert Decimal(result["pnl"]) == Decimal(-4)
            again = await close_trade(trade, _principal(user), session)
            assert again["status"] == "completed"

        async with engine.connect() as connection:
            state = (await connection.execute(text("""
                SELECT t.status,t.quantity,t.remaining_lots,t.exit_reason,t.pnl,o.status AS order_status
                  FROM trades t JOIN strategy_orders o ON o.trade_id=t.id WHERE t.id=:trade
            """), {"trade": trade})).mappings().one()
            assert state == {
                "status": "closed", "quantity": 20, "remaining_lots": 0,
                "exit_reason": "MANUAL_RULENIX_CLOSE", "pnl": Decimal("-4.00"),
                "order_status": "cancelled",
            }
    finally:
        async with engine.begin() as connection:
            await connection.execute(text("DELETE FROM market_price_ticks WHERE exchange_segment='MCX' AND contract_token=:token"), {"token": token})
            await connection.execute(text("DELETE FROM users WHERE id IN (:user,:other)"), {"user": user, "other": other})
            await connection.execute(text("DELETE FROM strategy_market_snapshots WHERE id=:snapshot"), {"snapshot": snapshot})
        await engine.dispose()


@pytest.mark.asyncio
async def test_manual_close_evidence_survives_sessions_and_is_revision_scoped() -> None:
    url = _url()
    if not url:
        pytest.skip("TEST_DATABASE_URL is not configured; no shared database is used")
    engine = create_async_engine(url)
    user, snapshot, trade = uuid4(), uuid4(), uuid4()
    observed = datetime.now(UTC)
    try:
        async with engine.begin() as connection:
            await connection.execute(text("""
                INSERT INTO users(id,username,email,password_hash)
                VALUES(:id,:name,:email,'test')
            """), {"id": user, "name": f"evidence-{user.hex[:8]}",
                     "email": f"evidence-{user.hex[:8]}@test.invalid"})
            await connection.execute(text("""
                INSERT INTO strategy_market_snapshots(
                  id,strategy_key,instrument,trade_date,status,contract_token,contract_symbol,
                  lot_size,exchange_segment,product_type,execution_key)
                VALUES(:id,'futures_breakout_v3','GOLDTEN',CURRENT_DATE,'ready','evidence-token',
                  'GOLDTEN-EVIDENCE',10,'MCX','CARRYFORWARD',:key)
            """), {"id": snapshot, "key": f"evidence-{trade.hex}"})
            await connection.execute(text("""
                INSERT INTO trades(
                  id,user_id,execution_mode,status,direction,quantity,entry_price,pnl,
                  instrument_label,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status)
                VALUES(:id,:user,'live','open','BUY',10,100,0,'GOLDTEN',
                  'futures_breakout_v3',:snapshot,1,1,'PROTECTED')
            """), {"id": trade, "user": user, "snapshot": snapshot})
        evidence = ManualCloseEvidence(
            trade, user, 7, "MCX", "evidence-token", "GOLDTEN-EVIDENCE", "SELL",
            10, Decimal(101), ("external-1",), observed, observed, observed,
        )
        async with AsyncSession(engine) as session:
            await ReconciliationRepository(session).store_manual_close_evidence(evidence)
            await session.commit()
        async with AsyncSession(engine) as restarted:
            repository = ReconciliationRepository(restarted)
            assert await repository.load_manual_close_evidence(
                trade_id=trade, user_id=user, credential_revision=6
            ) is None
            loaded = await repository.load_manual_close_evidence(
                trade_id=trade, user_id=user, credential_revision=7
            )
            assert loaded == evidence
            assert await repository.consume_manual_close_evidence(
                trade_id=trade, user_id=user, credential_revision=7, consumed_at=observed
            )
            await restarted.commit()
        async with AsyncSession(engine) as later:
            assert await ReconciliationRepository(later).load_manual_close_evidence(
                trade_id=trade, user_id=user, credential_revision=7
            ) is None
    finally:
        async with engine.begin() as connection:
            await connection.execute(text("DELETE FROM users WHERE id=:user"), {"user": user})
            await connection.execute(text("DELETE FROM strategy_market_snapshots WHERE id=:snapshot"), {"snapshot": snapshot})
        await engine.dispose()
