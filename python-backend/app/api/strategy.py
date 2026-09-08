from __future__ import annotations

from uuid import UUID

from fastapi import APIRouter, Depends, Query
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, admin_only, current_user, get_db
from ..errors import DomainError

router = APIRouter(tags=["strategy"])
_STRATEGIES = {
    "futures_breakout_v3": ("Futures Breakout v3", "Four-day MCX futures breakout with stop-and-reverse trade management.", ("GOLDTEN", "GOLDM", "SILVERM", "SILVERMIC", "NATGASMINI")),
    "supertrend_index_options_v1": ("SuperTrend Index Options v1", "Intraday 5-minute SuperTrend flips on SENSEX/NIFTY closed candles.", ("SENSEX", "NIFTY")),
}


async def _catalog(db: AsyncSession, user_id: str) -> list[dict]:
    activations = {str(row["strategy_key"]): bool(row["is_active"]) for row in (await db.execute(text("SELECT strategy_key,is_active FROM user_strategy_activations WHERE user_id=:user"), {"user": user_id})).mappings().all()}
    configs = {(str(row["strategy_key"]), str(row["instrument"])): row for row in (await db.execute(text("SELECT * FROM user_strategy_configs WHERE user_id=:user"), {"user": user_id})).mappings().all()}
    result = []
    for key, (name, description, instruments) in _STRATEGIES.items():
        values = []
        for instrument in instruments:
            row = configs.get((key, instrument))
            values.append({"instrument": instrument, "label": instrument, "enabled": bool(row["enabled"]) if row else False, "lots": int(row["lots"]) if row else 1, "run_day_session": bool(row["run_day_session"]) if row else True, "run_evening_session": bool(row["run_evening_session"]) if row else key != "supertrend_index_options_v1", "target_points": float(row["target_points"]) if row and row["target_points"] is not None else (40 if instrument == "SENSEX" else 25 if key.startswith("supertrend") else None), "stop_loss_points": float(row["stop_loss_points"]) if row and row["stop_loss_points"] is not None else (25 if instrument == "SENSEX" else 15 if key.startswith("supertrend") else None)})
        result.append({"key": key, "name": name, "description": description, "active": activations.get(key, False), "operational_alerts": [], "scheduler_runs": [], "instruments": values})
    return result


@router.get("/strategies")
async def catalog(user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    return {"strategies": await _catalog(db, user.id)}


@router.put("/strategies/{strategy_key}/activation")
async def activation(strategy_key: str, payload: dict, user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    if strategy_key not in _STRATEGIES or not isinstance(payload.get("active"), bool):
        raise DomainError(400, "A valid strategy and boolean active value are required.")
    await db.execute(text("INSERT INTO user_strategy_activations(user_id,strategy_key,is_active,activated_at,deactivated_at) VALUES(:user,:key,:active,CASE WHEN :active THEN NOW() END,CASE WHEN :active THEN NULL ELSE NOW() END) ON CONFLICT(user_id,strategy_key) DO UPDATE SET is_active=EXCLUDED.is_active,updated_at=NOW(),deactivated_at=CASE WHEN EXCLUDED.is_active THEN NULL ELSE NOW() END"), {"user": user.id, "key": strategy_key, "active": payload["active"]})
    await db.commit()
    return {"strategies": await _catalog(db, user.id)}


@router.get("/strategy/futures-breakout")
async def status(instrument: str = Query("GOLDTEN"), user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    if instrument not in _STRATEGIES["futures_breakout_v3"][2]:
        raise DomainError(400, "Futures Breakout supports GOLDTEN, GOLDM, SILVERM, SILVERMIC, NATGASMINI.")
    config = (await db.execute(text("SELECT enabled,lots,run_day_session,run_evening_session FROM user_strategy_configs WHERE user_id=:user AND strategy_key='futures_breakout_v3' AND instrument=:instrument"), {"user": user.id, "instrument": instrument})).mappings().first()
    orders = [dict(row) for row in (await db.execute(text("SELECT id,role,side,status,lots,quantity,price,trigger_price,broker_order_id,filled_quantity,average_fill_price,created_at FROM strategy_orders WHERE user_id=:user ORDER BY created_at DESC LIMIT 100"), {"user": user.id})).mappings().all()]
    trades = [dict(row) for row in (await db.execute(text("SELECT id,status,direction,total_lots,remaining_lots,quantity,entry_price,exit_price,pnl,entry_datetime AS trigger_time,exit_datetime AS exit_time,contract_symbol,target_price AS target,sl1_price AS sl1,sl2_price AS sl2 FROM trades WHERE user_id=:user AND strategy_key='futures_breakout_v3' AND instrument_label=:instrument ORDER BY created_at DESC LIMIT 100"), {"user": user.id, "instrument": instrument})).mappings().all()]
    active = bool(await db.scalar(text("SELECT COALESCE(is_active,FALSE) FROM user_strategy_activations WHERE user_id=:user AND strategy_key='futures_breakout_v3'"), {"user": user.id}))
    return {"strategy_key": "futures_breakout_v3", "strategy_active": active, "instrument": instrument, "configuration": dict(config) if config else None, "snapshot": None, "orders": orders, "trades": trades, "operational_alerts": []}


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
async def admin_executions(date: str | None = Query(None), _: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    rows = (await db.execute(text("SELECT i.id,i.user_id,i.status,i.action,i.role,i.attempts,i.last_error,i.created_at,i.updated_at,u.username FROM strategy_execution_intents i JOIN users u ON u.id=i.user_id WHERE (:date IS NULL OR i.created_at::date=CAST(:date AS date)) ORDER BY i.created_at DESC LIMIT 500"), {"date": date})).mappings().all()
    return {"executions": [dict(row) for row in rows], "date": date}


@router.post("/strategies/admin/executions/retry")
async def retry_execution(payload: dict, _: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    try:
        intent_id = UUID(str(payload.get("intent_id") or payload.get("id")))
    except (TypeError, ValueError) as exc:
        raise DomainError(422, "intent_id must be a UUID.") from exc
    result = await db.execute(text("UPDATE strategy_execution_intents SET status='retry_wait',next_attempt_at=NOW(),last_error='',updated_at=NOW() WHERE id=:id AND status IN ('failed','retry_wait')"), {"id": intent_id})
    await db.commit()
    if not getattr(result, "rowcount", 0):
        raise DomainError(404, "Execution intent was not found or is not retryable.")
    return {"detail": "Execution intent queued for safe retry.", "intent_id": str(intent_id)}
