from __future__ import annotations

import csv
import io
from datetime import datetime
from uuid import UUID
from zoneinfo import ZoneInfo

from fastapi import APIRouter, Depends, Path
from fastapi.responses import StreamingResponse
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, current_user, get_db
from ..errors import DomainError

router = APIRouter(prefix="/backtesting", tags=["backtesting"])
TRADING_DAY_BLOCK_MESSAGE = "Backtesting is disabled for the entire Indian trading day to reserve Angel One API capacity for live market data and order execution. Try again on a weekend or full market holiday."
TRADING_DAY_OVERRIDE_MESSAGE = "Trading-day backtesting is enabled by an administrator for this account."


@router.get("/runs")
async def history(user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    if not user.can_backtest:
        raise DomainError(403, "Backtesting access required.")
    today = datetime.now(ZoneInfo("Asia/Kolkata")).date()
    calendar = (await db.execute(text("SELECT morning_open,evening_open,reason FROM market_calendar WHERE trade_date=:date"), {"date": today})).mappings().first()
    market_open = bool(calendar["morning_open"] or calendar["evening_open"]) if calendar else today.weekday() < 5
    override = market_open and user.can_backtest_on_trading_days
    allowed = not market_open or override
    reason = TRADING_DAY_OVERRIDE_MESSAGE if override else ((calendar["reason"] if calendar and calendar["reason"] else "Non-trading day") if allowed else TRADING_DAY_BLOCK_MESSAGE)
    rows = (await db.execute(text("SELECT id,strategy_key,instrument,trading_symbol,symbol_token,interval_key AS interval,lookback_months,from_time,to_time,lots,lot_size,status,summary,error,data_points,reused_points,fetched_points,created_at FROM backtest_runs WHERE user_id=:user ORDER BY created_at DESC LIMIT 20"), {"user": user.id})).mappings().all()
    return {"runs": [dict(row) for row in rows], "availability": {"allowed": allowed, "trading_day_override": override, "trade_date": today.isoformat(), "reason": reason}}


@router.get("/runs/{run_id}/export")
async def export(run_id: UUID = Path(...), user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    run = (await db.execute(text("SELECT id,strategy_key,instrument,status FROM backtest_runs WHERE id=:id AND user_id=:user"), {"id": run_id, "user": user.id})).mappings().first()
    if not run:
        raise DomainError(404, "Backtest run not found.")
    rows = (await db.execute(text("SELECT trade_date,direction,entry_time,entry_price,exit_time,exit_price,lots,quantity,realized_pnl,exit_reason FROM backtest_trades WHERE run_id=:run ORDER BY entry_time"), {"run": run_id})).mappings().all()
    buffer = io.StringIO(); writer = csv.writer(buffer); writer.writerow(["Trade Date", "Direction", "Entry Time", "Entry Price", "Exit Time", "Exit Price", "Lots", "Quantity", "Realized P/L", "Exit Reason"])
    for row in rows:
        writer.writerow(list(row.values()))
    return StreamingResponse(iter([buffer.getvalue()]), media_type="text/csv", headers={"Content-Disposition": f'attachment; filename="backtest-{run_id}.csv"'})


@router.post("/run")
async def run(payload: dict, user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    if not user.can_backtest:
        raise DomainError(403, "Backtesting permission required.")
    raise DomainError(503, "Backtest execution is not enabled in the Python migration shadow.", code="python_backtesting_deferred")
