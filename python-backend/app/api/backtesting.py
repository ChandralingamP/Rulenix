from __future__ import annotations

import csv
import io
from uuid import UUID

from fastapi import APIRouter, Depends, Path
from fastapi.responses import StreamingResponse
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, current_user, get_db
from ..errors import DomainError

router = APIRouter(prefix="/backtesting", tags=["backtesting"])


@router.get("/runs")
async def history(user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    rows = (await db.execute(text("SELECT id,strategy_key,instrument,trading_symbol,symbol_token,interval_key,lookback_months,from_time,to_time,lots,lot_size,status,summary,error,data_points,reused_points,fetched_points,created_at FROM backtest_runs WHERE user_id=:user ORDER BY created_at DESC LIMIT 100"), {"user": user.id})).mappings().all()
    return {"runs": [dict(row) for row in rows]}


@router.get("/runs/{run_id}/export")
async def export(run_id: UUID = Path(...), user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    run = (await db.execute(text("SELECT id,strategy_key,instrument,status FROM backtest_runs WHERE id=:id AND user_id=:user"), {"id": run_id, "user": user.id})).mappings().first()
    if not run:
        raise DomainError(404, "Backtest run was not found.")
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
