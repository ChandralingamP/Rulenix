from __future__ import annotations

from datetime import date as Date
from datetime import datetime
from uuid import UUID
from zoneinfo import ZoneInfo

from fastapi import APIRouter, Depends, Query
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, admin_only, current_user, get_db
from ..errors import DomainError

router = APIRouter(tags=["strategy"])
_STRATEGIES = {
    "futures_breakout_v3": ("Futures Breakout v3", "Four-day MCX futures breakout with stop-and-reverse trade management.", ("GOLDTEN", "GOLDM", "SILVERM", "SILVERMIC", "NATGASMINI")),
    "supertrend_index_options_v1": ("SuperTrend Index Options v1", "Intraday 5-minute SuperTrend flips on SENSEX/NIFTY closed candles, buying ATM CE/PE with user-defined TP and SL points.", ("SENSEX", "NIFTY")),
}
_FUTURES_LABELS = {"GOLDTEN": "Gold Ten", "GOLDM": "Gold Mini", "SILVERM": "Silver Mini", "SILVERMIC": "Silver Micro", "NATGASMINI": "Natural Gas Mini"}
_INDEX_OPTIONS = {
    "SENSEX": {"label": "SENSEX ATM Options", "exchange": "BFO", "token": "99919000", "target": 40.0, "stop": 25.0},
    "NIFTY": {"label": "NIFTY ATM Options", "exchange": "NFO", "token": "99926000", "target": 25.0, "stop": 15.0},
}


async def _catalog(db: AsyncSession, user_id: str) -> list[dict]:
    activations = {str(row["strategy_key"]): bool(row["is_active"]) for row in (await db.execute(text("SELECT strategy_key,is_active FROM user_strategy_activations WHERE user_id=:user"), {"user": user_id})).mappings().all()}
    configs = {(str(row["strategy_key"]), str(row["instrument"])): row for row in (await db.execute(text("SELECT * FROM user_strategy_configs WHERE user_id=:user"), {"user": user_id})).mappings().all()}
    today = datetime.now(ZoneInfo("Asia/Kolkata")).date()
    snapshots = {
        row["instrument"]: row["value"]
        for row in (await db.execute(text("SELECT instrument,to_jsonb(s) AS value FROM strategy_market_snapshots s WHERE strategy_key='futures_breakout_v3' AND trade_date=:date"), {"date": today})).mappings().all()
    }
    connected_sessions = int(await db.scalar(text("SELECT COUNT(*) FROM user_profiles p JOIN users u ON u.id=p.user_id WHERE u.is_active=TRUE AND p.last_token_status IN ('success','refreshed') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='api_key') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='jwt_token')")) or 0)
    market_data = {
        "status": "connected" if connected_sessions else "disconnected",
        "connected_sessions": connected_sessions,
        "message": "Angel One market-data session is connected." if connected_sessions else "Angel One market-data session is disconnected. Reconnect Angel One before SuperTrend can fetch index candles/options LTP or place live/demo entries.",
    }
    breakout_alerts = list((await db.execute(text("SELECT jsonb_build_object('id',id,'instrument',instrument,'severity',payload->>'severity','code',payload->>'code','message',payload->>'message','created_at',created_at) FROM strategy_events WHERE strategy_key='futures_breakout_v3' AND event_type='operational_alert' AND (user_id=:user OR user_id IS NULL) AND created_at>NOW()-INTERVAL '10 minutes' AND (instrument='' OR instrument=ANY(:instruments)) ORDER BY created_at DESC LIMIT 1"), {"user": user_id, "instruments": list(_STRATEGIES["futures_breakout_v3"][2])})).scalars().all())
    runs = list((await db.execute(text("SELECT jsonb_build_object('instrument',instrument,'session',session_key,'action',action,'status',status,'attempts',attempts,'scheduled_for',scheduled_for,'last_error',last_error,'updated_at',updated_at) FROM strategy_scheduler_runs WHERE strategy_key='futures_breakout_v3' AND trade_date=:date AND instrument=ANY(:instruments) ORDER BY scheduled_for,action"), {"date": today, "instruments": list(_STRATEGIES["futures_breakout_v3"][2])})).scalars().all())
    breakout_instruments = []
    for instrument in _STRATEGIES["futures_breakout_v3"][2]:
        row = configs.get(("futures_breakout_v3", instrument))
        breakout_instruments.append({"instrument": instrument, "label": _FUTURES_LABELS[instrument], "enabled": bool(row["enabled"]) if row else False, "lots": int(row["lots"]) if row else 1, "run_day_session": bool(row["run_day_session"]) if row else True, "run_evening_session": bool(row["run_evening_session"]) if row else True, "snapshot": snapshots.get(instrument)})
    supertrend_alerts = list((await db.execute(text("SELECT jsonb_build_object('id',id,'instrument',instrument,'severity',payload->>'severity','code',payload->>'code','message',payload->>'message','created_at',created_at) FROM strategy_events WHERE strategy_key='supertrend_index_options_v1' AND event_type='operational_alert' AND (user_id=:user OR user_id IS NULL) AND created_at>NOW()-INTERVAL '10 minutes' ORDER BY created_at DESC LIMIT 1"), {"user": user_id})).scalars().all())
    supertrend_instruments = []
    for instrument, defaults in _INDEX_OPTIONS.items():
        row = configs.get(("supertrend_index_options_v1", instrument))
        target = float(row["target_points"]) if row and row["target_points"] and row["target_points"] > 0 else defaults["target"]
        stop = float(row["stop_loss_points"]) if row and row["stop_loss_points"] and row["stop_loss_points"] > 0 else defaults["stop"]
        supertrend_instruments.append({
            "instrument": instrument,
            "label": defaults["label"],
            "enabled": bool(row["enabled"]) if row else False,
            "lots": int(row["lots"]) if row else 1,
            "run_day_session": bool(row["run_day_session"]) if row else True,
            "run_evening_session": bool(row["run_evening_session"]) if row else False,
            "target_points": target,
            "stop_loss_points": stop,
            "parameters": {"target_points": target, "stop_loss_points": stop, "atr_period": 7, "factor": 2.0, "interval": "FIVE_MINUTE", "contract_selection": "ATM"},
            "snapshot": {"strategy_key": "supertrend_index_options_v1", "instrument": instrument, "status": "ready", "execution_key": "catalog-preview", "exchange_segment": defaults["exchange"], "product_type": "INTRADAY", "underlying_token": defaults["token"], "contract_expiry": None, "lot_size": None, "market_data": market_data},
        })
    return [
        {"key": "futures_breakout_v3", "name": "Futures Breakout v3", "description": _STRATEGIES["futures_breakout_v3"][1], "active": activations.get("futures_breakout_v3", False), "operational_alerts": breakout_alerts, "scheduler_runs": runs, "instruments": breakout_instruments},
        {"key": "supertrend_index_options_v1", "name": "SuperTrend Index Options v1", "description": _STRATEGIES["supertrend_index_options_v1"][1], "active": activations.get("supertrend_index_options_v1", False), "operational_alerts": supertrend_alerts, "scheduler_runs": [], "instruments": supertrend_instruments},
    ]


@router.get("/strategies")
async def catalog(user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    return {"strategies": await _catalog(db, user.id)}


@router.put("/strategies/{strategy_key}/activation")
async def activation(strategy_key: str, payload: dict, user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    if strategy_key not in _STRATEGIES:
        raise DomainError(404, "Strategy not found.")
    if not isinstance(payload.get("active"), bool):
        raise DomainError(400, "A valid strategy and boolean active value are required.")
    await db.execute(text("INSERT INTO user_strategy_activations(user_id,strategy_key,is_active,activated_at,deactivated_at) VALUES(:user,:key,:active,CASE WHEN :active THEN NOW() END,CASE WHEN :active THEN NULL ELSE NOW() END) ON CONFLICT(user_id,strategy_key) DO UPDATE SET is_active=EXCLUDED.is_active,updated_at=NOW(),deactivated_at=CASE WHEN EXCLUDED.is_active THEN NULL ELSE NOW() END"), {"user": user.id, "key": strategy_key, "active": payload["active"]})
    await db.commit()
    return {"strategies": await _catalog(db, user.id)}


@router.get("/strategy/futures-breakout")
async def status(instrument: str = Query("GOLDTEN"), user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    instrument = instrument.upper()
    if instrument not in _STRATEGIES["futures_breakout_v3"][2]:
        raise DomainError(400, "Futures Breakout supports GOLDTEN, GOLDM, SILVERM, SILVERMIC, NATGASMINI.")
    config = (await db.execute(text("SELECT enabled,lots,run_day_session,run_evening_session FROM user_strategy_configs WHERE user_id=:user AND strategy_key='futures_breakout_v3' AND instrument=:instrument"), {"user": user.id, "instrument": instrument})).mappings().first()
    snapshot = await db.scalar(text("SELECT to_jsonb(s) FROM strategy_market_snapshots s WHERE strategy_key='futures_breakout_v3' AND instrument=:instrument AND trade_date=(NOW() AT TIME ZONE 'Asia/Kolkata')::date"), {"instrument": instrument})
    orders = list((await db.execute(text("SELECT jsonb_build_object('id',id,'role',role,'side',side,'status',status,'lots',lots,'quantity',quantity,'price',price,'trigger_price',trigger_price,'client_order_id',client_order_id,'broker_order_id',broker_order_id,'filled_quantity',filled_quantity,'average_fill_price',average_fill_price,'broker_error_class',broker_error_class,'broker_error_code',broker_error_code,'broker_http_status',broker_http_status,'last_reconciled_at',last_reconciled_at,'created_at',created_at) FROM strategy_orders WHERE user_id=:user ORDER BY created_at DESC LIMIT 100"), {"user": user.id})).scalars().all())
    trades = list((await db.execute(text("SELECT jsonb_build_object('id',id,'status',status,'direction',direction,'lots',total_lots,'remaining_lots',remaining_lots,'quantity',quantity,'entry_price',entry_price,'exit_price',exit_price,'pnl',pnl,'trigger_time',entry_datetime,'exit_time',exit_datetime,'contract_symbol',contract_symbol,'target',target_price,'sl1',sl1_price,'sl2',sl2_price,'reversal_of_trade_id',reversal_of_trade_id) FROM trades WHERE user_id=:user AND strategy_key='futures_breakout_v3' AND instrument_label=:instrument ORDER BY created_at DESC LIMIT 100"), {"user": user.id, "instrument": instrument})).scalars().all())
    alerts = list((await db.execute(text("SELECT jsonb_build_object('id',id,'instrument',instrument,'severity',payload->>'severity','code',payload->>'code','message',payload->>'message','created_at',created_at) FROM strategy_events WHERE strategy_key='futures_breakout_v3' AND event_type='operational_alert' AND (user_id=:user OR user_id IS NULL) AND created_at>NOW()-INTERVAL '24 hours' ORDER BY created_at DESC LIMIT 20"), {"user": user.id})).scalars().all())
    active = bool(await db.scalar(text("SELECT COALESCE(is_active,FALSE) FROM user_strategy_activations WHERE user_id=:user AND strategy_key='futures_breakout_v3'"), {"user": user.id}))
    return {"strategy_key": "futures_breakout_v3", "strategy_active": active, "instrument": instrument, "configuration": dict(config) if config else None, "snapshot": snapshot, "orders": orders, "trades": trades, "operational_alerts": alerts}


@router.put("/strategy/futures-breakout")
async def update(payload: dict, user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    key = str(payload.get("strategy_key") or "futures_breakout_v3").strip()
    instrument = str(payload.get("instrument") or ("SENSEX" if key == "supertrend_index_options_v1" else "GOLDTEN")).upper()
    if key not in _STRATEGIES or instrument not in _STRATEGIES[key][2]:
        raise DomainError(400, "Unsupported strategy or instrument.")
    lots = payload.get("lots", 1)
    if not isinstance(lots, int) or lots <= 0:
        raise DomainError(400, "Lots must be a positive integer.")
    if payload.get("enabled") and not await db.scalar(text("SELECT COALESCE(is_active,FALSE) FROM user_strategy_activations WHERE user_id=:user AND strategy_key=:key"), {"user": user.id, "key": key}):
        raise DomainError(400, "Activate the strategy before enabling an instrument.")
    await db.execute(text("INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session,target_points,stop_loss_points) VALUES(:user,:key,:instrument,:enabled,:lots,:day,:evening,:target,:stop) ON CONFLICT(user_id,strategy_key,instrument) DO UPDATE SET enabled=EXCLUDED.enabled,lots=EXCLUDED.lots,run_day_session=EXCLUDED.run_day_session,run_evening_session=EXCLUDED.run_evening_session,target_points=EXCLUDED.target_points,stop_loss_points=EXCLUDED.stop_loss_points,updated_at=NOW()"), {"user": user.id, "key": key, "instrument": instrument, "enabled": bool(payload.get("enabled", False)), "lots": lots, "day": bool(payload.get("run_day_session", True)), "evening": bool(payload.get("run_evening_session", key != "supertrend_index_options_v1")), "target": payload.get("target_points"), "stop": payload.get("stop_loss_points")})
    await db.commit()
    return {"strategies": await _catalog(db, user.id)}


@router.get("/strategies/admin/executions")
async def admin_executions(date: Date | None = Query(None), _: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    if date is None:
        date = await db.scalar(text("SELECT (NOW() AT TIME ZONE 'Asia/Kolkata')::date"))
    signals = list((await db.execute(text("""
      SELECT jsonb_build_object(
        'signal_id',s.id,'strategy_key',s.strategy_key,'instrument',s.instrument,'session_key',s.session_key,
        'signal_type',s.signal_type,'signal_at',s.signal_at,'status',s.status,'expected_users',s.expected_users,
        'pending',COUNT(i.id) FILTER (WHERE i.status IN ('pending','claimed','retry_wait')),
        'submitted',COUNT(i.id) FILTER (WHERE i.status='submitted'),
        'completed',COUNT(i.id) FILTER (WHERE i.status='completed'),
        'skipped',COUNT(i.id) FILTER (WHERE i.status IN ('skipped','expired')),
        'failed',COUNT(i.id) FILTER (WHERE i.status='failed'),
        'intents',COALESCE(jsonb_agg(jsonb_build_object('intent_id',i.id,'user_id',i.user_id,'username',u.username,'instrument',i.instrument,'action',i.action,'role',i.role,'status',i.status,'attempts',i.attempts,'last_error',i.last_error,'updated_at',i.updated_at) ORDER BY u.username,i.role) FILTER (WHERE i.id IS NOT NULL),'[]'::jsonb))
      FROM strategy_signals s LEFT JOIN strategy_execution_intents i ON i.signal_id=s.id
      LEFT JOIN users u ON u.id=i.user_id
      WHERE (s.signal_at AT TIME ZONE 'Asia/Kolkata')::date=:date
      GROUP BY s.id ORDER BY s.signal_at DESC
    """), {"date": date})).scalars().all())
    totals = await db.scalar(text("""
      SELECT jsonb_build_object('expected_users',COALESCE(SUM(s.expected_users),0),
        'pending',COUNT(i.id) FILTER (WHERE i.status IN ('pending','claimed','retry_wait')),
        'submitted',COUNT(i.id) FILTER (WHERE i.status='submitted'),
        'completed',COUNT(i.id) FILTER (WHERE i.status='completed'),
        'skipped',COUNT(i.id) FILTER (WHERE i.status IN ('skipped','expired')),
        'failed',COUNT(i.id) FILTER (WHERE i.status='failed'))
      FROM strategy_signals s LEFT JOIN strategy_execution_intents i ON i.signal_id=s.id
      WHERE (s.signal_at AT TIME ZONE 'Asia/Kolkata')::date=:date
    """), {"date": date})
    return {"date": date.isoformat(), "timezone": "Asia/Kolkata", "totals": totals, "signals": signals}


@router.post("/strategies/admin/executions/retry")
async def retry_execution(payload: dict, _: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    try:
        intent_id = UUID(str(payload.get("intent_id") or payload.get("id")))
    except (TypeError, ValueError) as exc:
        raise DomainError(422, "intent_id must be a UUID.") from exc
    result = await db.execute(text("UPDATE strategy_execution_intents SET status='retry_wait',next_attempt_at=NOW(),last_error='',updated_at=NOW() WHERE id=:id AND status IN ('failed','retry_wait')"), {"id": intent_id})
    await db.commit()
    if not getattr(result, "rowcount", 0):
        raise DomainError(400, "Only failed or waiting entry intents inside their safe execution window can be retried.")
    return {"detail": "Execution intent queued for safe retry.", "intent_id": str(intent_id)}
