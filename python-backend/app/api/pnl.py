from __future__ import annotations

import csv
import io
from decimal import Decimal
from uuid import UUID

from fastapi import APIRouter, Depends, Query
from fastapi.responses import StreamingResponse
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, current_user, get_db
from ..errors import DomainError
from ..trading.domain import futures_pnl_units, trade_pnl
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
    await db.execute(text("SELECT pg_advisory_xact_lock_shared(hashtext('rulenix:risk:global'))"))
    await db.execute(
        text("SELECT pg_advisory_xact_lock(hashtextextended(:user,0))"),
        {"user": str(user.id)},
    )
    row = (await db.execute(text("""
        SELECT t.status,t.execution_mode,t.direction,t.quantity,t.total_lots,t.entry_price,t.pnl,
               t.strategy_key,t.instrument_label,UPPER(s.exchange_segment) AS exchange_segment,
               s.contract_token,s.lot_size
          FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
         WHERE t.id=:id AND t.user_id=:user FOR UPDATE OF t
    """), {"id": trade_id, "user": user.id})).mappings().first()
    if row is None:
        raise DomainError(404, "Trade was not found.")
    if row["status"] == "closed":
        return {"trade_id": str(trade_id), "status": "completed", "message": "Trade is already closed."}
    if row["execution_mode"] == "demo":
        if row["status"] != "open" or int(row["quantity"] or 0) <= 0:
            raise DomainError(400, "Only an eligible running DEMO trade can be closed locally.")
        orders = (await db.execute(text("""
            SELECT status FROM strategy_orders
             WHERE trade_id=:trade AND execution_mode='demo'
               AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
             FOR UPDATE
        """), {"trade": trade_id})).scalars().all()
        if "processing" in orders:
            raise DomainError(400, "A DEMO exit fill is already being processed; refresh the trade before closing it.")
        max_age = await db.scalar(text("""
            SELECT COALESCE(u.max_price_age_seconds,g.max_price_age_seconds)::int
              FROM risk_limits g LEFT JOIN risk_limits u ON u.user_id=:user
             WHERE g.user_id IS NULL
        """), {"user": user.id})
        exit_price = await db.scalar(text("""
            SELECT price FROM market_price_ticks
             WHERE exchange_segment=:exchange AND contract_token=:token
               AND received_at>NOW()-(:age * INTERVAL '1 second') AND price>0
             ORDER BY received_at DESC LIMIT 1
        """), {
            "exchange": row["exchange_segment"], "token": row["contract_token"],
            "age": max(int(max_age or 1), 1),
        })
        if exit_price is None or Decimal(str(exit_price)) <= 0:
            raise DomainError(400, "DEMO close stopped because no fresh valid market price is available.")
        quantity = int(row["quantity"])
        realized = Decimal(str(row["pnl"] or 0)) + trade_pnl(
            row["direction"], row["entry_price"], exit_price,
            futures_pnl_units(row["instrument_label"], quantity, row["lot_size"]),
        )
        reporting_quantity = (
            int(row["total_lots"] or 0) * max(int(row["lot_size"] or 1), 1)
            if row["strategy_key"] == "futures_breakout_v3" else quantity
        )
        await db.execute(text("""
            UPDATE strategy_orders
               SET status='cancelled',broker_status='DEMO order terminalized by user close',
                   state_version=state_version+1,updated_at=NOW()
             WHERE trade_id=:trade AND execution_mode='demo'
               AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','cancelling')
        """), {"trade": trade_id})
        changed = await db.execute(text("""
            UPDATE trades SET status='closed',safety_status='CLOSED',quantity=:quantity,
                   remaining_lots=0,exit_price=:price,last_price=:price,pnl=:pnl,
                   exit_datetime=NOW(),exit_reason='MANUAL_RULENIX_CLOSE',
                   notes=CONCAT(notes,'; running DEMO trade closed locally by user'),updated_at=NOW()
             WHERE id=:trade AND user_id=:user AND execution_mode='demo' AND status='open'
        """), {"trade": trade_id, "user": user.id, "quantity": reporting_quantity,
                 "price": exit_price, "pnl": realized})
        if int(getattr(changed, "rowcount", 0) or 0) != 1:
            raise DomainError(400, "DEMO trade state changed while the close was being processed; refresh and try again.")
        await db.commit()
        return {
            "trade_id": str(trade_id), "status": "completed", "execution_mode": "demo",
            "exit_price": exit_price, "pnl": realized,
            "message": "DEMO trade closed locally at the latest authoritative simulated price.",
        }
    if row["execution_mode"] != "live":
        raise DomainError(400, "Close Trade is available only for running DEMO or LIVE trades.")
    inserted = await TradingRepository(db).request_manual_close(
        trade_id=trade_id,
        user_id=UUID(str(user.id)),
        requested_quantity=int(row["quantity"]),
    )
    await db.commit()
    intent = (await db.execute(text("""
        SELECT status,requested_quantity,close_side FROM manual_trade_close_intents
         WHERE trade_id=:trade AND user_id=:user
    """), {"trade": trade_id, "user": user.id})).mappings().one()
    return {
        "trade_id": str(trade_id),
        "status": intent["status"],
        "execution_mode": "live",
        "requested_quantity": int(intent["requested_quantity"]),
        "close_side": intent["close_side"],
        "duplicate": not inserted,
        "message": "LIVE close is durably queued for authority-fenced broker execution.",
    }
