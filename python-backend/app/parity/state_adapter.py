"""Test-only PostgreSQL state fixture adapter for Rust/Python snapshots."""

from __future__ import annotations

from typing import Any

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncEngine

USER = "00000000-0000-0000-0000-000000000010"
SIGNAL = "00000000-0000-0000-0000-000000000011"
INTENT = "00000000-0000-0000-0000-000000000012"
SNAPSHOT = "00000000-0000-0000-0000-000000000013"
ORDER = "00000000-0000-0000-0000-000000000014"
TRADE = "00000000-0000-0000-0000-000000000015"


async def _reset(db: Any) -> None:
    for statement, params in (
        ("DELETE FROM strategy_execution_intents WHERE id=:intent OR signal_id=:signal OR trade_id=:trade", {"intent": INTENT, "signal": SIGNAL, "trade": TRADE}),
        ("DELETE FROM strategy_orders WHERE id=:id", {"id": ORDER}),
        ("DELETE FROM trades WHERE id=:id", {"id": TRADE}),
        ("DELETE FROM strategy_signals WHERE id=:id", {"id": SIGNAL}),
        ("DELETE FROM strategy_market_snapshots WHERE id=:id", {"id": SNAPSHOT}),
        ("DELETE FROM risk_kill_switches WHERE user_id=:id", {"id": USER}),
        ("DELETE FROM broker_reconciliation_health WHERE user_id=:id", {"id": USER}),
        ("DELETE FROM user_profiles WHERE user_id=:id", {"id": USER}),
        ("DELETE FROM users WHERE id=:id", {"id": USER}),
    ):
        await db.execute(text(statement), params)


async def _seed_user(db: Any) -> None:
    await db.execute(text("INSERT INTO users(id,username,email,password_hash) VALUES(:id,'phase10_state','phase10_state@example.test','fixture')"), {"id": USER})
    await db.execute(text("INSERT INTO user_profiles(user_id,trading_mode) VALUES(:id,'demo')"), {"id": USER})


async def _seed_signal(db: Any) -> None:
    await db.execute(text("INSERT INTO strategy_signals(id,strategy_key,instrument,session_key,signal_at,signal_type,status,expected_users,payload) VALUES(:id,'futures_breakout_v3','GOLDTEN','phase10-state',NOW(),'ENTRY','confirmed',1,'{}')"), {"id": SIGNAL})


async def _seed_snapshot(db: Any) -> None:
    await db.execute(text("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error) VALUES(:id,'futures_breakout_v3','GOLDTEN',CURRENT_DATE,'ready','')"), {"id": SNAPSHOT})


async def _seed_trade(db: Any) -> None:
    await db.execute(text("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,instrument_label,contract_symbol,notes) VALUES(:id,:user,'demo','open','BUY',10,100,100,0,'GOLDTEN','PHASE10','state fixture')"), {"id": TRADE, "user": USER})


async def execute_state_case(engine: AsyncEngine, case: str) -> dict[str, Any]:
    async with engine.begin() as connection:
        await _reset(connection)
        await _seed_user(connection)
        if case == "strategy_signal":
            await _seed_signal(connection)
            query = "SELECT jsonb_build_object('id',id,'strategy_key',strategy_key,'instrument',instrument,'status',status,'expected_users',expected_users,'payload',payload) FROM strategy_signals WHERE id=:id"
            params = {"id": SIGNAL}
        elif case == "execution_intent":
            await _seed_signal(connection)
            await connection.execute(text("INSERT INTO strategy_execution_intents(id,signal_id,user_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,status,last_error) VALUES(:id,:signal,:user,'futures_breakout_v3','GOLDTEN','phase10-state','ENTRY','BUY_ENTRY','BUY','LIMIT',1,1,110.132,'submitted','')"), {"id": INTENT, "signal": SIGNAL, "user": USER})
            query = "SELECT jsonb_build_object('id',id,'user_id',user_id,'action',action,'role',role,'side',side,'quantity',quantity,'price',price,'status',status,'attempts',attempts) FROM strategy_execution_intents WHERE id=:id"
            params = {"id": INTENT}
        elif case == "order_transition":
            await _seed_snapshot(connection)
            await connection.execute(text("INSERT INTO strategy_orders(id,user_id,snapshot_id,session_key,role,side,execution_mode,lots,quantity,price,status,idempotency_key) VALUES(:id,:user,:snapshot,'phase10-state','BUY_ENTRY','BUY','demo',1,1,110.132,'pending','phase10-order')"), {"id": ORDER, "user": USER, "snapshot": SNAPSHOT})
            await connection.execute(text("UPDATE strategy_orders SET status='submitted',broker_order_id='rust-state-order',updated_at=NOW() WHERE id=:id"), {"id": ORDER})
            query = "SELECT jsonb_build_object('id',id,'user_id',user_id,'side',side,'quantity',quantity,'price',price,'status',status,'broker_order_id',broker_order_id) FROM strategy_orders WHERE id=:id"
            params = {"id": ORDER}
        elif case == "partial_fill":
            await _seed_trade(connection)
            await connection.execute(text("UPDATE trades SET quantity=4,last_price=101,pnl=4,updated_at=NOW() WHERE id=:id"), {"id": TRADE})
            query = "SELECT jsonb_build_object('id',id,'user_id',user_id,'direction',direction,'quantity',quantity,'entry_price',entry_price,'last_price',last_price,'pnl',pnl,'status',status) FROM trades WHERE id=:id"
            params = {"id": TRADE}
        elif case in {"reversal_intent", "manual_close_intent"}:
            await _seed_signal(connection)
            await _seed_trade(connection)
            action = "REVERSAL" if case == "reversal_intent" else "SQUARE_OFF"
            await connection.execute(text("INSERT INTO strategy_execution_intents(id,signal_id,user_id,trade_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,status,last_error) VALUES(:id,:signal,:user,:trade,'futures_breakout_v3','GOLDTEN','phase10-state',:action,'TARGET','SELL','MARKET',1,1,101,'pending','')"), {"id": INTENT, "signal": SIGNAL, "user": USER, "trade": TRADE, "action": action})
            query = "SELECT jsonb_build_object('id',id,'user_id',user_id,'trade_id',trade_id,'action',action,'role',role,'side',side,'quantity',quantity,'status',status) FROM strategy_execution_intents WHERE id=:id"
            params = {"id": INTENT}
        elif case == "kill_switch":
            await connection.execute(text("INSERT INTO risk_kill_switches(user_id,enabled,reason,updated_by) VALUES(:id,TRUE,'phase10 fixture',:id)"), {"id": USER})
            query = "SELECT jsonb_build_object('user_id',user_id,'enabled',enabled,'reason',reason,'updated_by',updated_by) FROM risk_kill_switches WHERE user_id=:id"
            params = {"id": USER}
        elif case == "readiness":
            await connection.execute(text("INSERT INTO broker_reconciliation_health(user_id,healthy,detail,checked_at) VALUES(:id,FALSE,'phase10 fixture',NOW())"), {"id": USER})
            query = "SELECT jsonb_build_object('user_id',user_id,'healthy',healthy,'detail',detail) FROM broker_reconciliation_health WHERE user_id=:id"
            params = {"id": USER}
        else:
            raise ValueError(f"unsupported state case: {case}")
        row = (await connection.execute(text(query), params)).scalar_one()
        value = dict(row)
        await _reset(connection)
        return value
