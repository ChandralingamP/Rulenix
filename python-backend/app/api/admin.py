from datetime import date as Date

from fastapi import APIRouter, Depends, Query
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, admin_only, get_db
from ..errors import DomainError

router = APIRouter(prefix="/auth/admin", tags=["admin"])

@router.get("/users/")
async def users(_: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    rows = (await db.execute(text("""SELECT u.id,u.username,u.email,u.can_administer,u.can_live_trade,u.can_backtest,u.can_backtest_on_trading_days,COALESCE(p.trading_mode,'demo') trading_mode,u.is_active,u.created_at,p.brokerage_user_id,p.broker_egress_ip_id,host(e.ip_address) AS broker_egress_ip,e.configuration_status AS broker_egress_configuration_status,e.verification_status AS broker_egress_verification_status FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id LEFT JOIN broker_egress_ips e ON e.id=p.broker_egress_ip_id ORDER BY u.username"""))).mappings().all()
    return [dict(row) for row in rows]

@router.patch("/users/")
async def update_user(payload: dict, actor: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    target = payload.get("user_id") or payload.get("id")
    if not target and payload.get("username"):
        target = await db.scalar(text("SELECT id FROM users WHERE LOWER(username)=LOWER(:username)"), {"username": payload["username"]})
    if not target:
        raise DomainError(422, "user_id is required.")
    if str(target) == str(actor.id) and "can_administer" in payload and not payload["can_administer"]:
        raise DomainError(400, "You cannot change your own administration permission.")
    allowed = {k: payload[k] for k in ("can_administer", "can_live_trade", "can_backtest", "can_backtest_on_trading_days", "is_active") if k in payload}
    if not allowed:
        raise DomainError(422, "At least one permission is required.")
    sets = ",".join(f"{key}=:{key}" for key in allowed)
    allowed["id"] = target
    await db.execute(text(f"UPDATE users SET {sets},updated_at=NOW() WHERE id=:id"), allowed)
    await db.commit()
    updated = (await db.execute(text("SELECT u.id,u.username,u.email,u.can_administer,u.can_live_trade,u.can_backtest,u.can_backtest_on_trading_days,u.is_active,COALESCE(p.trading_mode,'demo') trading_mode,p.brokerage_user_id,p.broker_egress_ip_id FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=:id"), {"id": target})).mappings().first()
    return {"detail": "User updated.", "user": dict(updated) if updated else None}

@router.get("/trade-logs/")
async def trade_logs(_: Principal = Depends(admin_only)):
    raise DomainError(503, "Trade log administration is deferred.", code="python_foundation_deferred")


@router.delete("/users/")
async def delete_user(payload: dict, actor: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    username = str(payload.get("username", "")).strip()
    target = (await db.execute(text("SELECT id,username FROM users WHERE LOWER(username)=LOWER(:username)"), {"username": username})).mappings().first()
    if not target:
        raise DomainError(404, "User was not found.")
    if str(target["id"]) == str(actor.id):
        raise DomainError(400, "You cannot delete your own account.")
    await db.execute(text("DELETE FROM users WHERE id=:id"), {"id": target["id"]})
    await db.commit()
    return {"detail": "User deleted."}


@router.delete("/users/trade-logs/")
async def clear_trade_logs(payload: dict, _: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    username = str(payload.get("username", "")).strip()
    scope = str(payload.get("scope", "demo")).lower()
    if scope not in {"demo", "live", "all"}:
        raise DomainError(400, "Scope must be demo, live, or all.")
    target = (await db.execute(text("SELECT id FROM users WHERE LOWER(username)=LOWER(:username)"), {"username": username})).scalar()
    if not target:
        raise DomainError(404, "User was not found.")
    kill = bool(await db.scalar(text("SELECT COALESCE(enabled,FALSE) FROM risk_kill_switches WHERE user_id IS NULL")))
    if not kill:
        raise DomainError(400, "Clear Trades requires the global kill switch to be enabled.")
    if scope in {"live", "all"}:
        # Python has no broker-read capability in this migration phase.  A
        # local readiness view is not proof that Angel is flat, so fail closed
        # instead of allowing LIVE/ALL cleanup to hide unresolved exposure.
        raise DomainError(503, "LIVE trade cleanup is unavailable until broker reconciliation is enabled in Python.", code="python_broker_read_deferred")
    deleted_trades_result = await db.execute(text("DELETE FROM trades WHERE user_id=:user AND execution_mode='demo'"), {"user": target})
    deleted_orders_result = await db.execute(text("DELETE FROM strategy_orders WHERE user_id=:user AND execution_mode='demo'"), {"user": target})
    deleted_runs_result = await db.execute(text("DELETE FROM backtest_runs WHERE user_id=:user"), {"user": target})
    deleted_trades = int(getattr(deleted_trades_result, "rowcount", 0) or 0)
    deleted_orders = int(getattr(deleted_orders_result, "rowcount", 0) or 0)
    deleted_runs = int(getattr(deleted_runs_result, "rowcount", 0) or 0) if deleted_runs_result else 0
    await db.commit()
    return {"detail": "Eligible local trading records cleared successfully. No broker order or position was changed.", "username": username, "scope": "demo", "deleted_trades": deleted_trades, "deleted_demo_trades": deleted_trades, "deleted_live_trades": 0, "deleted_closed_live_trades": 0, "deleted_demo_orders": deleted_orders, "deleted_live_orders": 0, "deleted_demo_intents": 0, "deleted_live_intents": 0, "deleted_demo_events": 0, "deleted_live_events": 0, "deleted_demo_risk_decisions": 0, "deleted_live_risk_decisions": 0, "deleted_orphan_signals": 0, "deleted_orphan_snapshots": 0, "deleted_backtest_runs": deleted_runs, "deleted_backtest_trades": 0, "broker_mutations": {"placed": 0, "modified": 0, "cancelled": 0}, "closed_live_history_cleanup": False, "global_kill_switch": True}


@router.get("/trades/daily/")
async def daily_trades(date: Date | None = Query(None), _: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    if date is None:
        date = await db.scalar(text("SELECT (NOW() AT TIME ZONE 'Asia/Kolkata')::date"))
    rows = (await db.execute(text("""
      WITH pnl_counts AS (
        SELECT user_id,COUNT(*)::bigint AS pnl_trades,
               COUNT(*) FILTER (WHERE execution_mode='demo')::bigint AS demo_trades,
               COUNT(*) FILTER (WHERE execution_mode='live')::bigint AS live_trades,
               COUNT(*) FILTER (WHERE status='open')::bigint AS open_trades,
               COUNT(*) FILTER (WHERE status='closed')::bigint AS closed_trades
          FROM trades
         WHERE (entry_datetime AT TIME ZONE 'Asia/Kolkata')::date=CAST(:date AS date)
         GROUP BY user_id
      ), backtest_counts AS (
        SELECT run.user_id,COUNT(trade.id)::bigint AS backtest_trades
          FROM backtest_runs run JOIN backtest_trades trade ON trade.run_id=run.id
         WHERE (trade.entry_time AT TIME ZONE 'Asia/Kolkata')::date=CAST(:date AS date)
         GROUP BY run.user_id
      )
      SELECT u.id AS user_id,u.username,
             (COALESCE(p.pnl_trades,0)+COALESCE(b.backtest_trades,0))::bigint AS total_trades,
             COALESCE(p.pnl_trades,0)::bigint AS pnl_trades,
             COALESCE(b.backtest_trades,0)::bigint AS backtest_trades,
             COALESCE(p.demo_trades,0)::bigint AS demo_trades,
             COALESCE(p.live_trades,0)::bigint AS live_trades,
             COALESCE(p.open_trades,0)::bigint AS open_trades,
             COALESCE(p.closed_trades,0)::bigint AS closed_trades
        FROM users u LEFT JOIN pnl_counts p ON p.user_id=u.id
        LEFT JOIN backtest_counts b ON b.user_id=u.id
       ORDER BY (COALESCE(p.pnl_trades,0)+COALESCE(b.backtest_trades,0)) DESC,u.username
    """), {"date": date})).mappings().all()
    users = [dict(row) for row in rows]
    return {
        "date": date.isoformat(),
        "timezone": "Asia/Kolkata",
        "total_trades": sum(row["total_trades"] for row in users),
        "pnl_trades": sum(row["pnl_trades"] for row in users),
        "backtest_trades": sum(row["backtest_trades"] for row in users),
        "demo_trades": sum(row["demo_trades"] for row in users),
        "live_trades": sum(row["live_trades"] for row in users),
        "users": users,
    }

