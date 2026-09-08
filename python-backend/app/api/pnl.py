from __future__ import annotations

import csv
import io
from uuid import UUID

from fastapi import APIRouter, Depends, Query
from fastapi.responses import StreamingResponse
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, current_user, get_db
from ..errors import DomainError
from ..trading.repository import TradingRepository

router = APIRouter(tags=["pnl"])
_MODES = {"all", "demo", "live"}


def _validate(page: int, page_size: int, mode: str) -> None:
    if page < 1 or page_size < 1 or page_size > 100:
        raise DomainError(400, "Page must be positive and page_size must be between 1 and 100.")
    if mode not in _MODES:
        raise DomainError(400, "Mode must be one of all, demo, or live.")


async def _rows(db: AsyncSession, user_id: str, mode: str, limit: int, offset: int) -> list[dict]:
    mode_clause = "" if mode == "all" else "AND t.execution_mode=:mode"
    result = await db.execute(text(f"""
        SELECT t.id,t.status,t.execution_mode,t.direction,t.quantity,t.entry_price,t.exit_price,t.last_price,t.pnl,
               t.entry_datetime,t.exit_datetime,t.instrument_label,t.contract_symbol,t.notes,t.exit_reason,
               t.strategy_key,t.total_lots,t.remaining_lots,t.target_price,t.sl1_price,t.sl2_price,
               t.safety_status,m.status AS manual_close_status
          FROM trades t LEFT JOIN manual_trade_close_intents m ON m.trade_id=t.id
         WHERE t.user_id=:user {mode_clause}
         ORDER BY COALESCE(t.entry_datetime,t.created_at) DESC,t.created_at DESC
         LIMIT :limit OFFSET :offset
    """), {"user": user_id, "mode": mode, "limit": limit, "offset": offset})
    return [dict(row) for row in result.mappings().all()]


@router.get("/pnl")
async def list_pnl(page: int = Query(1), page_size: int = Query(20), mode: str = Query("all"), user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    _validate(page, page_size, mode)
    clause = "" if mode == "all" else "AND execution_mode=:mode"
    params = {"user": user.id, "mode": mode}
    summary = (await db.execute(text(f"SELECT COUNT(*) AS total,COALESCE(SUM(pnl),0) AS profit FROM trades WHERE user_id=:user {clause}"), params)).mappings().one()
    total = int(summary["total"] or 0)
    profit = summary["profit"] or 0
    total_pages = max(1, (total + page_size - 1) // page_size)
    return {"results": await _rows(db, user.id, mode, page_size, (page - 1) * page_size), "page": page, "page_size": page_size, "total_pages": total_pages, "total_records": total, "total_profit": profit or 0, "total_brokerage": 0, "total_net_profit": profit or 0, "mode": mode}


@router.get("/pnl/export")
async def export_pnl(page: int = Query(1), page_size: int = Query(20), mode: str = Query("all"), user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    _validate(page, page_size, mode)
    rows = await _rows(db, user.id, mode, 100_000, 0)
    if not rows:
        raise DomainError(404, "No trades available for export.")
    buffer = io.StringIO()
    columns = ["#", "Entry Date", "Exit Date", "Strategy", "Instrument", "Symbol", "Mode", "Direction", "Quantity", "Entry @", "Exit @", "Exit Reason", "P/L"]
    writer = csv.writer(buffer)
    writer.writerow(columns)
    for index, row in enumerate(rows, 1):
        writer.writerow([index, row.get("entry_datetime") or "", row.get("exit_datetime") or "", row.get("strategy_key") or "", row.get("instrument_label") or "", row.get("contract_symbol") or "", row.get("execution_mode") or "", row.get("direction") or "", row.get("quantity") or 0, row.get("entry_price") or "", row.get("exit_price") or "", row.get("exit_reason") or "", row.get("pnl") or 0])
    return StreamingResponse(iter([buffer.getvalue()]), media_type="text/csv", headers={"Content-Disposition": f'attachment; filename="rulenix-pnl-{mode}-page-{page}.csv"'})


@router.post("/pnl/trades/{trade_id}/close")
async def close_trade(trade_id: UUID, user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    row = (await db.execute(text("SELECT status,execution_mode,quantity FROM trades WHERE id=:id AND user_id=:user FOR UPDATE"), {"id": trade_id, "user": user.id})).mappings().first()
    if row is None:
        raise DomainError(404, "Trade was not found.")
    if row["status"] == "closed":
        return {"trade_id": str(trade_id), "status": "completed", "message": "Trade is already closed."}
    if row["execution_mode"] != "live":
        raise DomainError(400, "Close Trade is currently available only for running LIVE trades.")
    # Preserve the durable intent, but stop before any Angel mutation.  The
    # frontend receives an explicit migration-safe error, never fake success.
    await TradingRepository(db).request_manual_close(trade_id=trade_id, user_id=UUID(user.id), requested_quantity=int(row["quantity"]))
    await db.commit()
    raise DomainError(503, "LIVE manual close is queued for broker reconciliation; Python Angel mutation transport is disabled.", code="python_live_mutation_disabled")
