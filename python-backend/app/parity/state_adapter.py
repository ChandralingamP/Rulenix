"""Test-only PostgreSQL state fixture adapter for Rust/Python snapshots."""

from __future__ import annotations

from datetime import datetime
from typing import Any

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncEngine

from ..strategy.scheduler import eod_square_off_due

USER = "00000000-0000-0000-0000-000000000010"
SIGNAL = "00000000-0000-0000-0000-000000000011"
INTENT = "00000000-0000-0000-0000-000000000012"
SNAPSHOT = "00000000-0000-0000-0000-000000000013"
ORDER = "00000000-0000-0000-0000-000000000014"
TRADE = "00000000-0000-0000-0000-000000000015"


async def _reset(db: Any) -> None:
    for statement, params in (
        ("DELETE FROM strategy_execution_intents WHERE session_key IN ('stsq-phase10-1510','phase10-state')", {}),
        ("DELETE FROM strategy_execution_intents WHERE id=:intent OR signal_id=:signal OR trade_id=:trade", {"intent": INTENT, "signal": SIGNAL, "trade": TRADE}),
        ("DELETE FROM strategy_orders WHERE id=:id", {"id": ORDER}),
        ("DELETE FROM trades WHERE id=:id", {"id": TRADE}),
        ("DELETE FROM strategy_signals WHERE id=:id", {"id": SIGNAL}),
        ("DELETE FROM strategy_signals WHERE session_key='squareoff-phase10-1510'", {}),
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
    await db.execute(text("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,execution_key) VALUES(:id,'futures_breakout_v3','GOLDTEN',CURRENT_DATE,'ready','','phase10-state')"), {"id": SNAPSHOT})


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


async def _seed_replay_graph(db: Any, strategy_key: str) -> None:
    instrument = "SENSEX_CE" if strategy_key == "supertrend_index_options_v1" else "GOLDTEN"
    await db.execute(
        text("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,lot_size,execution_key) VALUES(:id,:strategy,:instrument,CURRENT_DATE,'ready','',1,'phase10-replay')"),
        {"id": SNAPSHOT, "strategy": strategy_key, "instrument": instrument},
    )
    await db.execute(
        text("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status) VALUES(:id,:user,'demo','open','BUY',1,100,101,0,:instrument,'PHASE10','replay fixture',:strategy,:snapshot,1,1,'DEMO')"),
        {"id": TRADE, "user": USER, "instrument": instrument, "strategy": strategy_key, "snapshot": SNAPSHOT},
    )


async def _materialize_eod(db: Any) -> None:
    await db.execute(text("""
      WITH signal AS (
        INSERT INTO strategy_signals(id,strategy_key,instrument,session_key,signal_at,signal_type,expected_users,payload,status)
        VALUES(gen_random_uuid(),'supertrend_index_options_v1','ALL','squareoff-phase10-1510',CURRENT_DATE+TIME '15:10','SQUARE_OFF',1,jsonb_build_object('scheduled_for','15:10 IST'),'dispatching')
        ON CONFLICT(strategy_key,instrument,session_key,signal_type) DO UPDATE SET expected_users=GREATEST(strategy_signals.expected_users,EXCLUDED.expected_users),updated_at=NOW()
        RETURNING id)
      INSERT INTO strategy_execution_intents(id,signal_id,user_id,snapshot_id,trade_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,status)
      SELECT gen_random_uuid(),signal.id,:user,:snapshot,:trade,'supertrend_index_options_v1','SENSEX','stsq-phase10-1510','SQUARE_OFF','EMERGENCY_CLOSE','SELL','MARKET',1,1,101,'pending' FROM signal
      ON CONFLICT DO NOTHING
    """), {"user": USER, "snapshot": SNAPSHOT, "trade": TRADE})


async def _eod_state(db: Any, due: bool) -> dict[str, Any]:
    row = (await db.execute(text("""SELECT
      (SELECT COUNT(*) FROM strategy_signals WHERE strategy_key='supertrend_index_options_v1' AND session_key='squareoff-phase10-1510') AS signals,
      (SELECT COUNT(*) FROM strategy_execution_intents WHERE trade_id=:trade AND action='SQUARE_OFF') AS intents
    """), {"trade": TRADE})).one()
    return {"due": due, "signals": row.signals, "intents": row.intents, "duplicate_actions": max(row.intents - 1, 0)}


async def execute_replay_case(engine: AsyncEngine, case: str, at: str | None = None) -> dict[str, Any]:
    async with engine.begin() as db:
        await _reset(db)
        await _seed_user(db)
        if case in {"eod_boundary", "eod_restart"}:
            if at is None:
                raise ValueError("EOD replay requires an at timestamp")
            await _seed_replay_graph(db, "supertrend_index_options_v1")
            due = eod_square_off_due(datetime.fromisoformat(at))
            if due:
                await _materialize_eod(db)
                await _materialize_eod(db)
                if case == "eod_restart":
                    await _materialize_eod(db)
            value = await _eod_state(db, due)
        elif case == "stale_execution_claim":
            await _seed_signal(db)
            await db.execute(text("INSERT INTO strategy_execution_intents(id,signal_id,user_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,status,attempts,claimed_at) VALUES(:id,:signal,:user,'futures_breakout_v3','GOLDTEN','phase10-state','ENTRY','BUY_ENTRY','BUY','LIMIT',1,1,101,'claimed',1,NOW()-INTERVAL '10 minutes')"), {"id": INTENT, "signal": SIGNAL, "user": USER})
            first = await db.execute(text("UPDATE strategy_execution_intents SET status='retry_wait',next_attempt_at=NOW(),last_error='Backend restarted while this execution intent was claimed.',updated_at=NOW() WHERE status='claimed'"))
            second = await db.execute(text("UPDATE strategy_execution_intents SET status='retry_wait' WHERE status='claimed'"))
            row = (await db.execute(text("SELECT status,attempts FROM strategy_execution_intents WHERE id=:id"), {"id": INTENT})).one()
            value = {"same_intent": True, "intent_count": 1, "status": row.status, "attempts": row.attempts, "recovered_once": first.rowcount == 1 and second.rowcount == 0}
        elif case == "sl2_reversal":
            await _seed_replay_graph(db, "futures_breakout_v3")
            await db.execute(text("INSERT INTO strategy_reversal_intents(source_trade_id,user_id,snapshot_id,instrument,source_direction,reversal_direction,lots,entry_price,order_session_key,status,attempts,updated_at) VALUES(:trade,:user,:snapshot,'GOLDTEN','BUY','SELL',1,98,'phase10-reversal','processing',1,NOW()-INTERVAL '10 minutes')"), {"trade": TRADE, "user": USER, "snapshot": SNAPSHOT})
            first = await db.execute(text("UPDATE strategy_reversal_intents SET status='pending',next_attempt_at=NOW(),last_error='Backend restarted while the reversal was being processed.',updated_at=NOW() WHERE status='processing' AND updated_at<NOW()-INTERVAL '30 seconds'"))
            second = await db.execute(text("UPDATE strategy_reversal_intents SET status='pending' WHERE status='processing'"))
            row = (await db.execute(text("SELECT status,(SELECT COUNT(*) FROM strategy_reversal_intents WHERE source_trade_id=:trade) AS count FROM strategy_reversal_intents WHERE source_trade_id=:trade"), {"trade": TRADE})).one()
            value = {"same_intent": True, "intent_count": row[1], "status": row.status, "recovered_once": first.rowcount == 1 and second.rowcount == 0, "duplicate_reversals": row[1] - 1}
        elif case == "manual_close":
            await _seed_replay_graph(db, "futures_breakout_v3")
            await db.execute(text("INSERT INTO manual_trade_close_intents(trade_id,user_id,status,requested_quantity,close_side,updated_at) VALUES(:trade,:user,'cancelling_protection',1,'SELL',NOW()-INTERVAL '10 minutes')"), {"trade": TRADE, "user": USER})
            first = await db.execute(text("UPDATE manual_trade_close_intents SET status='reconciliation_required',last_error='Recovered uncertain manual-close worker claim.',updated_at=NOW() WHERE status='cancelling_protection' AND updated_at<NOW()-INTERVAL '30 seconds'"))
            second = await db.execute(text("UPDATE manual_trade_close_intents SET status='reconciliation_required' WHERE status='cancelling_protection'"))
            row = (await db.execute(text("SELECT status,(SELECT COUNT(*) FROM manual_trade_close_intents WHERE trade_id=:trade) AS count FROM manual_trade_close_intents WHERE trade_id=:trade"), {"trade": TRADE})).one()
            value = {"same_intent": True, "intent_count": row[1], "status": row.status, "recovered_once": first.rowcount == 1 and second.rowcount == 0, "duplicate_closes": row[1] - 1}
        elif case == "eod_crash":
            await _seed_replay_graph(db, "supertrend_index_options_v1")
            await _materialize_eod(db)
            await db.execute(text("UPDATE strategy_execution_intents SET status='claimed',attempts=1,claimed_at=NOW()-INTERVAL '10 minutes' WHERE trade_id=:trade AND action='SQUARE_OFF'"), {"trade": TRADE})
            first = await db.execute(text("UPDATE strategy_execution_intents SET status='retry_wait',next_attempt_at=NOW(),last_error='Backend restarted while this execution intent was claimed.',updated_at=NOW() WHERE trade_id=:trade AND action='SQUARE_OFF' AND status='claimed'"), {"trade": TRADE})
            await _materialize_eod(db)
            row = (await db.execute(text("SELECT status,(SELECT COUNT(*) FROM strategy_execution_intents WHERE trade_id=:trade AND action='SQUARE_OFF') AS count FROM strategy_execution_intents WHERE trade_id=:trade AND action='SQUARE_OFF'"), {"trade": TRADE})).one()
            value = {"same_intent": True, "intent_count": row[1], "status": row.status, "recovered_once": first.rowcount == 1, "duplicate_actions": row[1] - 1}
        elif case == "protection_recovery":
            await _seed_replay_graph(db, "futures_breakout_v3")
            await db.execute(text("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,execution_mode,lots,quantity,price,status,idempotency_key,updated_at) VALUES(:id,:user,:snapshot,:trade,'phase10-protection','SL1','SELL','demo',1,1,99,'processing','phase10-protection',NOW()-INTERVAL '10 minutes')"), {"id": ORDER, "user": USER, "snapshot": SNAPSHOT, "trade": TRADE})
            first = await db.execute(text("UPDATE strategy_orders SET status='submitted',broker_status='Fill processing was interrupted; queued for reconciliation.',updated_at=NOW() WHERE status='processing' AND updated_at<NOW()-INTERVAL '30 seconds'"))
            second = await db.execute(text("UPDATE strategy_orders SET status='submitted' WHERE status='processing'"))
            row = (await db.execute(text("SELECT status,(SELECT COUNT(*) FROM strategy_orders WHERE trade_id=:trade AND role='SL1') AS count FROM strategy_orders WHERE id=:id"), {"trade": TRADE, "id": ORDER})).one()
            value = {"same_order": True, "protection_count": row[1], "status": row.status, "recovered_once": first.rowcount == 1 and second.rowcount == 0, "duplicate_protection": row[1] - 1}
        else:
            raise ValueError(f"unsupported replay case: {case}")
        await _reset(db)
        return value
