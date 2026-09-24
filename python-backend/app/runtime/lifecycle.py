"""Authoritative trading lifecycle workers.

The workers in this module only operate on durable PostgreSQL state.  Every
LIVE write is represented by a ``strategy_orders`` row before it reaches the
existing authority-fenced :class:`LiveMutationCoordinator`.  Ambiguous writes
are deliberately left for reconciliation and are never blindly replayed.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime, time, timedelta
from decimal import Decimal
from uuid import NAMESPACE_URL, UUID, uuid5
from zoneinfo import ZoneInfo

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from app.broker.mutations import (
    LiveMutationCoordinator,
    MutationPendingError,
    MutationState,
)
from app.risk import ActionKind
from app.strategy.common import Candle, supertrend_entry_allowed
from app.strategy.execution import ExecutionOrchestrator
from app.strategy.persistence import PreparedIntent, SignalRepository, prepare_square_off_intent
from app.strategy.supertrend import CONFIGS, current_signal, signal_is_fresh, supertrend_points
from app.trading.domain import futures_pnl_units, sl2_reversal, trade_pnl

IST = ZoneInfo("Asia/Kolkata")
FUTURES = "futures_breakout_v3"
SUPERTREND = "supertrend_index_options_v1"
ACTIVE_ORDERS = (
    "pending",
    "submitting",
    "ambiguous",
    "submitted",
    "partially_filled",
    "processing",
    "cancelling",
)


@dataclass(frozen=True)
class LifecycleProgress:
    claimed: int = 0
    submitted: int = 0
    completed: int = 0
    waiting: int = 0
    ambiguous: int = 0

    def json(self) -> dict[str, object]:
        return {
            "claimed": self.claimed,
            "submitted": self.submitted,
            "completed": self.completed,
            "waiting": self.waiting,
            "ambiguous": self.ambiguous,
        }


def _order_uuid(namespace: str, reference: UUID | str, watermark: int = 0) -> UUID:
    return uuid5(NAMESPACE_URL, f"rulenix:{namespace}:{reference}:{watermark}")


def _session(value: str) -> str:
    return value[:32]


def _exit_side(direction: str) -> str:
    return "SELL" if direction.upper() == "BUY" else "BUY"


def _is_market_open(now: datetime) -> bool:
    local = now.astimezone(IST)
    if local.weekday() >= 5:
        return False
    minute = local.hour * 60 + local.minute
    return 9 * 60 <= minute <= 15 * 60 + 20 or 17 * 60 <= minute <= 23 * 60 + 25


class DurableOrderFactory:
    """Create deterministic local orders without performing broker I/O."""

    def __init__(self, session: AsyncSession):
        self.session = session

    async def create(
        self,
        *,
        order_id: UUID,
        user_id: UUID,
        snapshot_id: UUID,
        trade_id: UUID | None,
        session_key: str,
        role: str,
        side: str,
        lots: int,
        quantity: int,
        price: Decimal,
        trigger_price: Decimal | None,
        order_type: str,
        idempotency_key: str,
    ) -> UUID:
        if quantity <= 0 or lots <= 0:
            raise ValueError("Durable broker order quantity and lots must be positive.")
        snapshot = (
            (
                await self.session.execute(
                    text("""
                SELECT exchange_segment,product_type FROM strategy_market_snapshots
                 WHERE id=:snapshot AND status='ready'
                """),
                    {"snapshot": snapshot_id},
                )
            )
            .mappings()
            .first()
        )
        if snapshot is None:
            raise ValueError("A ready strategy snapshot is required for broker execution.")
        await self.session.execute(
            text("""
            INSERT INTO strategy_orders(
                id,user_id,snapshot_id,trade_id,session_key,role,side,execution_mode,
                lots,quantity,price,trigger_price,status,idempotency_key,client_order_id,
                order_type,exchange_segment,product_type)
            VALUES(:id,:user,:snapshot,:trade,:session,:role,:side,'live',:lots,:quantity,
                   :price,:trigger,'pending',:key,:client,:order_type,:exchange,:product)
            ON CONFLICT(idempotency_key) DO NOTHING
            """),
            {
                "id": order_id,
                "user": user_id,
                "snapshot": snapshot_id,
                "trade": trade_id,
                "session": _session(session_key),
                "role": role,
                "side": side,
                "lots": lots,
                "quantity": quantity,
                "price": price,
                "trigger": trigger_price,
                "key": idempotency_key,
                "client": f"RX{order_id.hex[:18].upper()}",
                "order_type": order_type,
                "exchange": snapshot["exchange_segment"],
                "product": snapshot["product_type"],
            },
        )
        existing = await self.session.scalar(
            text("SELECT id FROM strategy_orders WHERE idempotency_key=:key"),
            {"key": idempotency_key},
        )
        if existing is None:
            raise RuntimeError("Durable order reservation did not persist.")
        return UUID(str(existing))


class AuthoritativeExecutionWorker:
    """Dispatch durable ENTRY intents through the existing orchestrator."""

    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        coordinator: LiveMutationCoordinator,
    ) -> None:
        self.session_factory = session_factory
        self.coordinator = coordinator

    async def run_once(self) -> dict[str, object]:
        async with self.session_factory() as session:
            generated = await self._evaluate(session, datetime.now(UTC))
            await session.commit()
            recovered = (
                await __import__("app.trading.repository", fromlist=["TradingRepository"])
                .TradingRepository(session)
                .recover_stale_claims()
            )
            await session.commit()
            results = await ExecutionOrchestrator(session, self.coordinator).process_due()
        return {
            "signals_generated": generated,
            "recovered": recovered,
            "processed": len(results),
            "submitted": sum(item.outcome.value == "LIVE_SUBMITTED" for item in results),
            "ambiguous": sum(item.outcome.value == "LIVE_AMBIGUOUS" for item in results),
        }

    async def _evaluate(self, session: AsyncSession, now: datetime) -> int:
        local = now.astimezone(IST)
        if local.weekday() >= 5:
            return 0
        generated = await self._evaluate_futures(session, local)
        generated += await self._evaluate_supertrend(session, local)
        return generated

    async def _evaluate_futures(self, session: AsyncSession, now: datetime) -> int:
        minute = now.hour * 60 + now.minute
        schedules: list[tuple[str, int]] = []
        if 9 * 60 + 10 <= minute <= 9 * 60 + 25:
            schedules.append(("day", 9 * 60 + 10))
        if 9 * 60 + 16 <= minute <= 9 * 60 + 31:
            schedules.append(("day", 9 * 60 + 16))
        if 17 * 60 + 10 <= minute <= 17 * 60 + 25:
            schedules.append(("evening", 17 * 60 + 10))
        if not schedules:
            return 0
        calendar = (
            (
                await session.execute(
                    text(
                        "SELECT morning_open,evening_open FROM market_calendar WHERE trade_date=:date"
                    ),
                    {"date": now.date()},
                )
            )
            .mappings()
            .first()
        )
        generated = 0
        snapshots = (
            (
                await session.execute(
                    text("""
                SELECT * FROM strategy_market_snapshots
                 WHERE strategy_key=:strategy AND trade_date=:date AND status='ready'
                   AND gap_plan_status='READY' AND entry_direction IN ('BUY','SELL','BOTH')
                 ORDER BY instrument,execution_key
                """),
                    {"strategy": FUTURES, "date": now.date()},
                )
            )
            .mappings()
            .all()
        )
        for session_name, due in schedules:
            if calendar is not None and not bool(
                calendar["morning_open" if session_name == "day" else "evening_open"]
            ):
                continue
            for snapshot in snapshots:
                runners = (
                    (
                        await session.execute(
                            text("""
                        SELECT c.user_id,c.lots FROM user_strategy_configs c
                        JOIN user_strategy_activations a ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key
                        JOIN users u ON u.id=c.user_id JOIN user_profiles p ON p.user_id=c.user_id
                         WHERE c.strategy_key=:strategy AND c.instrument=:instrument
                           AND c.enabled=TRUE AND a.is_active=TRUE AND u.is_active=TRUE
                           AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))
                           AND CASE WHEN :session='day' THEN c.run_day_session ELSE c.run_evening_session END
                        """),
                            {
                                "strategy": FUTURES,
                                "instrument": snapshot["instrument"],
                                "session": session_name,
                            },
                        )
                    )
                    .mappings()
                    .all()
                )
                if not runners:
                    continue
                directions = (
                    ("BUY", "SELL")
                    if snapshot["entry_direction"] == "BOTH"
                    else (str(snapshot["entry_direction"]),)
                )
                expires = datetime.combine(
                    now.date(),
                    time(15, 19, 45) if session_name == "day" else time(23, 25),
                    tzinfo=IST,
                )
                intents: list[PreparedIntent] = []
                for runner in runners:
                    for direction in directions:
                        price_value = snapshot["planned_entry"]
                        if snapshot["entry_direction"] == "BOTH":
                            price_value = snapshot[
                                "buy_entry" if direction == "BUY" else "sell_entry"
                            ]
                        if price_value is None:
                            continue
                        intents.append(
                            PreparedIntent(
                                UUID(str(runner["user_id"])),
                                UUID(str(snapshot["id"])),
                                None,
                                FUTURES,
                                str(snapshot["instrument"]),
                                session_name,
                                "ENTRY",
                                "BUY_ENTRY" if direction == "BUY" else "SELL_ENTRY",
                                direction,
                                "STOPLOSS_LIMIT",
                                int(runner["lots"]),
                                int(runner["lots"]) * max(int(snapshot["lot_size"] or 1), 1),
                                Decimal(str(price_value)),
                                Decimal(str(price_value)),
                                expires,
                            )
                        )
                if not intents:
                    continue
                key = f"fb-{now:%Y%m%d}-{session_name}-{snapshot['entry_source'] or 'standard'}"
                _, inserted = await SignalRepository(session).materialize(
                    strategy_key=FUTURES,
                    instrument=str(snapshot["instrument"]),
                    session_key=key,
                    signal_at=now,
                    signal_type="ENTRY",
                    snapshot_id=UUID(str(snapshot["id"])),
                    payload={
                        "entry_direction": snapshot["entry_direction"],
                        "entry_source": snapshot["entry_source"],
                        "planned_entry": snapshot["planned_entry"],
                        "scheduled_minute": due,
                    },
                    intents=intents,
                )
                generated += int(inserted > 0)
        return generated

    async def _evaluate_supertrend(self, session: AsyncSession, now: datetime) -> int:
        if not supertrend_entry_allowed(now) or now.minute % 5:
            return 0
        generated = 0
        for underlying, config in CONFIGS.items():
            rows = (
                (
                    await session.execute(
                        text("""
                    SELECT candle_time,open_price,high_price,low_price,close_price
                      FROM backtest_market_candles
                     WHERE exchange=:exchange AND symbol_token=:token
                       AND interval_key='FIVE_MINUTE' AND candle_time>=:start AND candle_time<=:end
                     ORDER BY candle_time
                    """),
                        {
                            "exchange": config.index_exchange,
                            "token": config.index_token,
                            "start": now - timedelta(days=14),
                            "end": now,
                        },
                    )
                )
                .mappings()
                .all()
            )
            candles = [
                Candle(
                    item["candle_time"].astimezone(IST),
                    Decimal(str(item["open_price"])),
                    Decimal(str(item["high_price"])),
                    Decimal(str(item["low_price"])),
                    Decimal(str(item["close_price"])),
                )
                for item in rows
            ]
            signal = current_signal(supertrend_points(candles), now)
            if (
                signal is None
                or signal.signal_at.date() != now.date()
                or not signal_is_fresh(signal, now)
            ):
                continue
            runners = (
                (
                    await session.execute(
                        text("""
                    SELECT c.user_id,c.lots FROM user_strategy_configs c
                    JOIN user_strategy_activations a ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key
                    JOIN users u ON u.id=c.user_id JOIN user_profiles p ON p.user_id=c.user_id
                     WHERE c.strategy_key=:strategy AND c.instrument=:instrument
                       AND c.enabled=TRUE AND a.is_active=TRUE AND u.is_active=TRUE
                       AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))
                    """),
                        {"strategy": SUPERTREND, "instrument": underlying},
                    )
                )
                .mappings()
                .all()
            )
            intents: list[PreparedIntent] = []
            option_instrument = f"{underlying}_{signal.side.value}"
            for runner in runners:
                snapshot = (
                    (
                        await session.execute(
                            text("""
                        SELECT s.*,p.price FROM strategy_market_snapshots s
                        JOIN LATERAL (
                          SELECT price FROM market_price_ticks
                           WHERE exchange_segment=s.exchange_segment AND contract_token=s.contract_token
                             AND received_at>=NOW()-INTERVAL '5 seconds' AND price>0
                           ORDER BY received_at DESC LIMIT 1) p ON TRUE
                         WHERE s.strategy_key=:strategy AND s.instrument=:instrument
                           AND s.trade_date=:date AND s.status='ready'
                           AND s.execution_key LIKE :owner
                         ORDER BY s.fetched_at DESC LIMIT 1
                        """),
                            {
                                "strategy": SUPERTREND,
                                "instrument": option_instrument,
                                "date": now.date(),
                                "owner": f"%{UUID(str(runner['user_id'])).hex}",
                            },
                        )
                    )
                    .mappings()
                    .first()
                )
                if snapshot is None:
                    continue
                price = Decimal(str(snapshot["price"]))
                intents.append(
                    PreparedIntent(
                        UUID(str(runner["user_id"])),
                        UUID(str(snapshot["id"])),
                        None,
                        SUPERTREND,
                        underlying,
                        f"st-{underlying}-{signal.signal_at:%Y%m%d-%H%M}-{signal.side.value}",
                        "ENTRY",
                        "BUY_ENTRY",
                        "BUY",
                        "MARKET",
                        int(runner["lots"]),
                        int(runner["lots"]) * max(int(snapshot["lot_size"] or 1), 1),
                        price,
                        None,
                        signal.signal_at + timedelta(minutes=6, seconds=30),
                    )
                )
            if not intents:
                continue
            _, inserted = await SignalRepository(session).materialize(
                strategy_key=SUPERTREND,
                instrument=underlying,
                session_key=f"st-{underlying}-{signal.signal_at:%Y%m%d-%H%M}-{signal.side.value}",
                signal_at=signal.signal_at,
                signal_type="ENTRY",
                snapshot_id=None,
                payload={
                    "side": signal.side.value,
                    "index_close": str(signal.index_close),
                    "supertrend": str(signal.supertrend),
                },
                intents=intents,
            )
            generated += int(inserted > 0)
        return generated


class FillLifecycleWorker:
    """Apply reconciled cumulative fills exactly once to local trade state."""

    def __init__(self, session_factory: async_sessionmaker[AsyncSession]) -> None:
        self.session_factory = session_factory

    async def run_once(self) -> dict[str, object]:
        processed = 0
        while True:
            async with self.session_factory() as session, session.begin():
                row = (
                    (
                        await session.execute(
                            text("""
                        SELECT o.id,o.user_id,o.snapshot_id,o.trade_id,o.session_key,o.role,o.side,
                               o.execution_mode,o.lots,o.quantity,o.filled_quantity,
                               o.processed_quantity,o.average_fill_price,
                               s.strategy_key,s.instrument,s.contract_symbol,s.lot_size,
                               s.buy_target,s.buy_sl1,s.buy_sl2,s.sell_target,s.sell_sl1,s.sell_sl2
                          FROM strategy_orders o
                          JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
                         WHERE o.execution_mode='live' AND o.filled_quantity>o.processed_quantity
                           AND o.status IN ('submitted','partially_filled','processing','filled')
                         ORDER BY o.updated_at,o.id FOR UPDATE OF o SKIP LOCKED LIMIT 1
                        """)
                        )
                    )
                    .mappings()
                    .first()
                )
                if row is None:
                    break
                await self._apply(session, row)
                processed += 1
        return {"processed_fills": processed}

    async def _apply(self, session: AsyncSession, order) -> None:
        cumulative = int(order["filled_quantity"])
        already = int(order["processed_quantity"])
        delta = cumulative - already
        fill = Decimal(str(order["average_fill_price"] or 0))
        if delta <= 0 or fill <= 0:
            return
        order_id = UUID(str(order["id"]))
        user_id = UUID(str(order["user_id"]))
        role = str(order["role"])
        terminal = cumulative >= int(order["quantity"])
        if role in {"BUY_ENTRY", "SELL_ENTRY"}:
            await self._entry_fill(session, order, delta, fill)
        elif order["trade_id"] is not None:
            await self._exit_fill(session, order, delta, fill)
        await session.execute(
            text("""
            UPDATE strategy_orders SET processed_quantity=:processed,
                   status=CASE WHEN :terminal THEN 'filled' ELSE 'partially_filled' END,
                   filled_at=CASE WHEN :terminal THEN COALESCE(filled_at,NOW()) ELSE filled_at END,
                   state_version=state_version+1,updated_at=NOW()
             WHERE id=:order AND user_id=:user
            """),
            {"processed": cumulative, "terminal": terminal, "order": order_id, "user": user_id},
        )

    async def _entry_fill(self, session: AsyncSession, order, delta: int, fill: Decimal) -> None:
        order_id = UUID(str(order["id"]))
        trade = None
        if order["trade_id"] is not None:
            trade = (
                (
                    await session.execute(
                        text("SELECT * FROM trades WHERE id=:trade FOR UPDATE"),
                        {"trade": order["trade_id"]},
                    )
                )
                .mappings()
                .first()
            )
        direction = "BUY" if order["role"] == "BUY_ENTRY" else "SELL"
        lot_size = max(int(order["lot_size"] or 1), 1)
        delta_lots = max(1, (delta + lot_size - 1) // lot_size)
        if trade is not None and str(trade["direction"]) == direction and trade["status"] == "open":
            old_quantity = int(trade["quantity"])
            new_quantity = old_quantity + delta
            entry = (
                Decimal(str(trade["entry_price"])) * old_quantity + fill * delta
            ) / new_quantity
            await session.execute(
                text("""
                UPDATE trades SET quantity=:quantity,entry_price=:entry,last_price=:fill,
                       total_lots=total_lots+:lots,remaining_lots=remaining_lots+:lots,
                       safety_status='PROTECTION_REQUIRED',protection_deadline_at=NOW()+INTERVAL '30 seconds',
                       updated_at=NOW() WHERE id=:trade
                """),
                {
                    "quantity": new_quantity,
                    "entry": entry,
                    "fill": fill,
                    "lots": delta_lots,
                    "trade": trade["id"],
                },
            )
            return
        trade_id = _order_uuid("trade", order_id)
        strategy = str(order["strategy_key"])
        if strategy == SUPERTREND:
            if direction == "BUY":
                target = fill + Decimal(str(order["buy_target"] or 0))
                sl1 = max(Decimal("0.05"), fill - Decimal(str(order["buy_sl1"] or 0)))
            else:
                target = fill - Decimal(str(order["sell_target"] or 0))
                sl1 = fill + Decimal(str(order["sell_sl1"] or 0))
            sl2 = None
        else:
            target = Decimal(
                str(order["buy_target"] if direction == "BUY" else order["sell_target"])
            )
            sl1 = Decimal(str(order["buy_sl1"] if direction == "BUY" else order["sell_sl1"]))
            sl2_value = order["buy_sl2"] if direction == "BUY" else order["sell_sl2"]
            sl2 = Decimal(str(sl2_value)) if sl2_value is not None else None
        reversal_source = await session.scalar(
            text("""
            SELECT source_trade_id FROM strategy_reversal_intents
             WHERE user_id=:user AND order_session_key=:session LIMIT 1
            """),
            {"user": order["user_id"], "session": order["session_key"]},
        )
        await session.execute(
            text("""
            INSERT INTO trades(
                id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,
                entry_datetime,instrument_label,contract_symbol,external_entry_id,notes,
                strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,
                sl2_price,reversal_of_trade_id,safety_status,protection_deadline_at)
            VALUES(:id,:user,'live','open',:direction,:quantity,:entry,:entry,0,NOW(),
                   :instrument,:symbol,:external,:notes,:strategy,:snapshot,:lots,:lots,
                   :target,:sl1,:sl2,:reversal,'PROTECTION_REQUIRED',NOW()+INTERVAL '30 seconds')
            ON CONFLICT(id) DO NOTHING
            """),
            {
                "id": trade_id,
                "user": order["user_id"],
                "direction": direction,
                "quantity": delta,
                "entry": fill,
                "instrument": order["instrument"],
                "symbol": order["contract_symbol"] or "",
                "external": "",
                "notes": "Python authoritative reconciled entry fill",
                "strategy": strategy,
                "snapshot": order["snapshot_id"],
                "lots": delta_lots,
                "target": target,
                "sl1": sl1,
                "sl2": sl2,
                "reversal": reversal_source,
            },
        )
        await session.execute(
            text("UPDATE strategy_orders SET trade_id=:trade WHERE id=:order"),
            {"trade": trade_id, "order": order_id},
        )
        if reversal_source is not None:
            await session.execute(
                text("""
                UPDATE strategy_reversal_intents SET status='completed',last_error='',updated_at=NOW()
                 WHERE source_trade_id=:source
                """),
                {"source": reversal_source},
            )

    async def _exit_fill(self, session: AsyncSession, order, delta: int, fill: Decimal) -> None:
        trade = (
            (
                await session.execute(
                    text("SELECT * FROM trades WHERE id=:trade FOR UPDATE"),
                    {"trade": order["trade_id"]},
                )
            )
            .mappings()
            .first()
        )
        if trade is None or trade["status"] != "open":
            return
        old_quantity = int(trade["quantity"])
        closed = min(delta, old_quantity)
        remaining = old_quantity - closed
        lot_size = max(int(order["lot_size"] or 1), 1)
        realized = trade_pnl(
            str(trade["direction"]),
            trade["entry_price"],
            fill,
            futures_pnl_units(str(trade["instrument_label"]), closed, lot_size),
        )
        pnl = Decimal(str(trade["pnl"] or 0)) + realized
        remaining_lots = 0 if remaining == 0 else max(1, (remaining + lot_size - 1) // lot_size)
        role = str(order["role"])
        reason = (
            "MARKET_CLOSED"
            if str(order["session_key"]).startswith("stsq-")
            else "MANUAL_RULENIX_CLOSE"
            if str(order["session_key"]).startswith("mc-")
            else "TP1"
            if role == "TARGET" and trade["strategy_key"] == FUTURES
            else "TP"
            if role == "TARGET"
            else role
        )
        await session.execute(
            text("""
            UPDATE trades SET status=CASE WHEN :remaining=0 THEN 'closed' ELSE 'open' END,
                   safety_status=CASE WHEN :remaining=0 THEN 'CLOSED' ELSE 'PROTECTION_REQUIRED' END,
                   quantity=:remaining,remaining_lots=:lots,last_price=:fill,pnl=:pnl,
                   exit_price=CASE WHEN :remaining=0 THEN :fill ELSE exit_price END,
                   exit_datetime=CASE WHEN :remaining=0 THEN NOW() ELSE exit_datetime END,
                   exit_reason=CASE WHEN :remaining=0 THEN :reason ELSE exit_reason END,
                   protection_deadline_at=CASE WHEN :remaining>0 THEN NOW()+INTERVAL '30 seconds' ELSE NULL END,
                   updated_at=NOW() WHERE id=:trade
            """),
            {
                "remaining": remaining,
                "lots": remaining_lots,
                "fill": fill,
                "pnl": pnl,
                "reason": reason,
                "trade": trade["id"],
            },
        )
        if str(order["session_key"]).startswith("mc-"):
            await session.execute(
                text("""
                UPDATE manual_trade_close_intents
                   SET status=CASE WHEN :remaining=0 THEN 'completed' ELSE 'partially_filled' END,
                       strategy_order_id=:order,last_error='',
                       completed_at=CASE WHEN :remaining=0 THEN NOW() ELSE completed_at END,
                       updated_at=NOW() WHERE trade_id=:trade
                """),
                {"remaining": remaining, "order": order["id"], "trade": trade["id"]},
            )
        if str(order["session_key"]).startswith("stsq-"):
            await session.execute(
                text("""
                UPDATE strategy_execution_intents
                   SET status=CASE WHEN :remaining=0 THEN 'completed' ELSE 'submitted' END,
                       strategy_order_id=:order,last_error='',
                       completed_at=CASE WHEN :remaining=0 THEN NOW() ELSE completed_at END,
                       updated_at=NOW()
                 WHERE trade_id=:trade AND action='SQUARE_OFF'
                """),
                {"remaining": remaining, "order": order["id"], "trade": trade["id"]},
            )
        if role == "SL2" and remaining == 0 and trade["strategy_key"] == FUTURES:
            plan = sl2_reversal(str(trade["direction"]), int(trade["total_lots"]))
            if plan is not None:
                direction, _, lots = plan
                await session.execute(
                    text("""
                    INSERT INTO strategy_reversal_intents(
                        source_trade_id,user_id,snapshot_id,instrument,source_direction,
                        reversal_direction,lots,entry_price,order_session_key)
                    VALUES(:trade,:user,:snapshot,:instrument,:source,:reversal,:lots,:price,:session)
                    ON CONFLICT(source_trade_id) DO NOTHING
                    """),
                    {
                        "trade": trade["id"],
                        "user": trade["user_id"],
                        "snapshot": trade["strategy_snapshot_id"],
                        "instrument": trade["instrument_label"],
                        "source": trade["direction"],
                        "reversal": direction.value,
                        "lots": lots,
                        "price": fill,
                        "session": _session(f"r-{UUID(str(trade['id'])).hex[:30]}"),
                    },
                )


class ProtectionLifecycleWorker:
    """Recover missing LIVE protection and targets from reconciled durable truth."""

    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        coordinator: LiveMutationCoordinator,
    ) -> None:
        self.session_factory = session_factory
        self.coordinator = coordinator

    async def run_once(self) -> dict[str, object]:
        cleaned = await self._cleanup_terminal_siblings()
        async with self.session_factory() as session:
            rows = (
                (
                    await session.execute(
                        text("""
                    SELECT t.id FROM trades t
                     WHERE t.execution_mode='live' AND t.status='open'
                       AND t.safety_status IN ('PROTECTION_REQUIRED','PROTECTION_SUBMITTING',
                           'PROTECTION_UNCERTAIN','PROTECTION_FAILED','RECONCILIATION_REQUIRED')
                     ORDER BY t.entry_datetime LIMIT 100
                    """)
                    )
                )
                .scalars()
                .all()
            )
        progress = LifecycleProgress()
        for value in rows:
            outcome = await self._protect(UUID(str(value)))
            progress = LifecycleProgress(
                progress.claimed + 1,
                progress.submitted + (outcome == "submitted"),
                progress.completed + (outcome == "completed"),
                progress.waiting + (outcome == "waiting"),
                progress.ambiguous + (outcome == "ambiguous"),
            )
        target_results = await self._submit_pending_targets()
        value = progress.json()
        value["targets_submitted"] = target_results[0]
        value["targets_ambiguous"] = target_results[1]
        value["terminal_siblings_cleaned"] = cleaned
        return value

    async def _cleanup_terminal_siblings(self) -> int:
        async with self.session_factory() as session:
            rows = (
                (
                    await session.execute(
                        text("""
                    SELECT o.id,o.order_type FROM strategy_orders o JOIN trades t ON t.id=o.trade_id
                     WHERE t.status='closed' AND o.execution_mode='live'
                       AND o.role IN ('TARGET','SL1','SL2')
                       AND o.status IN ('ambiguous','submitted','partially_filled','processing')
                       AND o.broker_order_id<>'' ORDER BY o.created_at LIMIT 100
                    """)
                    )
                )
                .mappings()
                .all()
            )
        cleaned = 0
        for row in rows:
            try:
                outcome = await self.coordinator.cancel_order(
                    UUID(str(row["id"])),
                    variety="STOPLOSS"
                    if str(row["order_type"]).startswith("STOPLOSS")
                    else "NORMAL",
                )
            except MutationPendingError:
                continue
            cleaned += outcome.state is MutationState.ACKNOWLEDGED
        return cleaned

    async def _cancel_obsolete_stop(self, trade_id: UUID, desired_role: str) -> bool:
        async with self.session_factory() as session:
            row = (
                (
                    await session.execute(
                        text("""
                    SELECT id,order_type,status,broker_order_id FROM strategy_orders
                     WHERE trade_id=:trade AND role IN ('SL1','SL2') AND role<>:role
                       AND status IN ('ambiguous','submitted','partially_filled','processing','cancelling')
                     ORDER BY created_at LIMIT 1
                    """),
                        {"trade": trade_id, "role": desired_role},
                    )
                )
                .mappings()
                .first()
            )
        if row is None:
            return False
        if row["status"] == "cancelling" or not row["broker_order_id"]:
            return True
        try:
            await self.coordinator.cancel_order(
                UUID(str(row["id"])),
                variety="STOPLOSS" if str(row["order_type"]).startswith("STOPLOSS") else "NORMAL",
            )
        except MutationPendingError:
            pass
        return True

    async def _submit_pending_targets(self) -> tuple[int, int]:
        async with self.session_factory() as session:
            ids = (
                (
                    await session.execute(
                        text("""
                    SELECT id FROM strategy_orders WHERE execution_mode='live' AND role='TARGET'
                     AND status='pending' AND idempotency_key LIKE 'python:target:%'
                     ORDER BY created_at LIMIT 100
                    """)
                    )
                )
                .scalars()
                .all()
            )
        submitted = ambiguous = 0
        for value in ids:
            try:
                outcome = await self.coordinator.place_order(
                    UUID(str(value)), action=ActionKind.TARGET
                )
            except MutationPendingError:
                ambiguous += 1
                continue
            submitted += outcome.state is MutationState.ACKNOWLEDGED
            ambiguous += outcome.state is MutationState.AMBIGUOUS
        return submitted, ambiguous

    async def _protect(self, trade_id: UUID) -> str:
        async with self.session_factory() as session, session.begin():
            trade = (
                (
                    await session.execute(
                        text("""
                    SELECT t.*,s.lot_size FROM trades t
                    JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
                    WHERE t.id=:trade FOR UPDATE OF t
                    """),
                        {"trade": trade_id},
                    )
                )
                .mappings()
                .first()
            )
            if trade is None or trade["status"] != "open" or int(trade["quantity"]) <= 0:
                return "completed"
            exact = bool(
                trade["last_position_reconciled_at"]
                and int(trade["broker_net_quantity"] or 0)
                == (
                    int(trade["quantity"])
                    if trade["direction"] == "BUY"
                    else -int(trade["quantity"])
                )
            )
            health = bool(
                await session.scalar(
                    text("""
                    SELECT healthy AND checked_at>=NOW()-INTERVAL '5 minutes'
                      FROM broker_reconciliation_health WHERE user_id=:user
                    """),
                    {"user": trade["user_id"]},
                )
            )
            collision = bool(
                await session.scalar(
                    text("""
                    SELECT EXISTS(
                      SELECT 1 FROM broker_exposure_observations o
                      JOIN strategy_market_snapshots s ON s.id=:snapshot
                       WHERE o.user_id=:user AND UPPER(o.exchange_segment)=UPPER(s.exchange_segment)
                         AND o.contract_token=s.contract_token
                         AND o.ownership_status IN ('manual_external','ambiguous')
                         AND o.observed_at>=NOW()-INTERVAL '5 minutes')
                    """),
                    {"snapshot": trade["strategy_snapshot_id"], "user": trade["user_id"]},
                )
            )
            if not exact or not health or collision:
                await session.execute(
                    text("""
                    UPDATE trades SET safety_status='RECONCILIATION_REQUIRED',
                      last_protection_error='Fresh exact owned broker exposure is required before protection mutation.',
                      updated_at=NOW() WHERE id=:trade
                    """),
                    {"trade": trade_id},
                )
                return "waiting"
            target_done = bool(
                await session.scalar(
                    text(
                        "SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=:trade AND role='TARGET' AND processed_quantity>0)"
                    ),
                    {"trade": trade_id},
                )
            )
            stop_role = "SL2" if target_done and trade["sl2_price"] is not None else "SL1"
            if await self._cancel_obsolete_stop(trade_id, stop_role):
                return "waiting"
            protected = int(
                await session.scalar(
                    text("""
                    SELECT COALESCE(SUM(GREATEST(quantity-processed_quantity,0)),0)
                      FROM strategy_orders WHERE trade_id=:trade AND role=:role
                       AND status IN ('submitted','partially_filled') AND broker_order_id<>''
                       AND last_reconciled_at IS NOT NULL
                    """),
                    {"trade": trade_id, "role": stop_role},
                )
                or 0
            )
            if protected >= int(trade["quantity"]):
                await session.execute(
                    text(
                        "UPDATE trades SET safety_status='PROTECTED',last_protection_error='',updated_at=NOW() WHERE id=:trade"
                    ),
                    {"trade": trade_id},
                )
                if not target_done:
                    await self._reserve_target(session, trade)
                return "completed"
            active = bool(
                await session.scalar(
                    text("""
                    SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=:trade
                     AND role IN ('SL1','SL2') AND status=ANY(:states))
                    """),
                    {"trade": trade_id, "states": list(ACTIVE_ORDERS)},
                )
            )
            if active:
                await session.execute(
                    text(
                        "UPDATE trades SET safety_status='PROTECTION_UNCERTAIN',updated_at=NOW() WHERE id=:trade"
                    ),
                    {"trade": trade_id},
                )
                return "waiting"
            quantity = int(trade["quantity"]) - protected
            lot_size = max(int(trade["lot_size"] or 1), 1)
            lots = max(1, (quantity + lot_size - 1) // lot_size)
            watermark = int(trade["quantity"]) - quantity
            order_id = _order_uuid(f"protection-{stop_role}", trade_id, watermark)
            price = Decimal(str(trade["sl2_price"] if stop_role == "SL2" else trade["sl1_price"]))
            await DurableOrderFactory(session).create(
                order_id=order_id,
                user_id=UUID(str(trade["user_id"])),
                snapshot_id=UUID(str(trade["strategy_snapshot_id"])),
                trade_id=trade_id,
                session_key=f"px-{trade_id.hex[:16]}-{watermark}",
                role=stop_role,
                side=_exit_side(str(trade["direction"])),
                lots=lots,
                quantity=quantity,
                price=price,
                trigger_price=price,
                order_type="STOPLOSS_MARKET",
                idempotency_key=f"python:protection:{trade_id}:{stop_role}:{watermark}",
            )
            await session.execute(
                text("""
                UPDATE trades SET safety_status='PROTECTION_SUBMITTING',
                  protection_attempts=protection_attempts+1,last_protection_error='',updated_at=NOW()
                 WHERE id=:trade
                """),
                {"trade": trade_id},
            )
        return await self._submit(order_id, ActionKind.PROTECTION_RECOVERY, trade_id)

    async def _reserve_target(self, session: AsyncSession, trade) -> None:
        if trade["target_price"] is None:
            return
        quantity = int(trade["quantity"])
        if trade["strategy_key"] == FUTURES:
            lots = max(1, int(trade["total_lots"]) // 2)
            quantity = min(quantity, lots * max(int(trade["lot_size"] or 1), 1))
        else:
            lots = max(1, int(trade["remaining_lots"]))
        order_id = _order_uuid("target", trade["id"])
        await DurableOrderFactory(session).create(
            order_id=order_id,
            user_id=UUID(str(trade["user_id"])),
            snapshot_id=UUID(str(trade["strategy_snapshot_id"])),
            trade_id=UUID(str(trade["id"])),
            session_key=f"pt-{UUID(str(trade['id'])).hex[:16]}",
            role="TARGET",
            side=_exit_side(str(trade["direction"])),
            lots=lots,
            quantity=quantity,
            price=Decimal(str(trade["target_price"])),
            trigger_price=None,
            order_type="LIMIT",
            idempotency_key=f"python:target:{trade['id']}",
        )

    async def _submit(self, order_id: UUID, action: ActionKind, trade_id: UUID) -> str:
        try:
            outcome = await self.coordinator.place_order(order_id, action=action)
        except MutationPendingError:
            return "ambiguous"
        if outcome.state is MutationState.ACKNOWLEDGED:
            return "submitted"
        if outcome.state is MutationState.AMBIGUOUS:
            async with self.session_factory() as session, session.begin():
                await session.execute(
                    text("""
                    UPDATE trades SET safety_status='PROTECTION_UNCERTAIN',
                     last_protection_error='Protection broker acknowledgement is ambiguous.',updated_at=NOW()
                     WHERE id=:trade
                    """),
                    {"trade": trade_id},
                )
            return "ambiguous"
        async with self.session_factory() as session, session.begin():
            await session.execute(
                text("""
                UPDATE trades SET safety_status='PROTECTION_FAILED',
                 last_protection_error='Protection submission failed closed.',updated_at=NOW()
                 WHERE id=:trade
                """),
                {"trade": trade_id},
            )
        return "waiting"


class ReversalLifecycleWorker:
    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        coordinator: LiveMutationCoordinator,
    ) -> None:
        self.session_factory = session_factory
        self.coordinator = coordinator

    async def run_once(self) -> dict[str, object]:
        now = datetime.now(UTC)
        async with self.session_factory() as session, session.begin():
            await session.execute(
                text("""
                UPDATE strategy_reversal_intents SET status='pending',next_attempt_at=NOW(),
                  last_error='Recovered stale Python reversal claim.',updated_at=NOW()
                 WHERE status='processing' AND updated_at<NOW()-INTERVAL '30 seconds'
            """)
            )
            if not _is_market_open(now):
                return LifecycleProgress(waiting=1).json()
            row = (
                (
                    await session.execute(
                        text("""
                UPDATE strategy_reversal_intents SET status='processing',attempts=attempts+1,updated_at=NOW()
                 WHERE source_trade_id IN (
                   SELECT source_trade_id FROM strategy_reversal_intents
                    WHERE status IN ('pending','waiting','failed') AND next_attempt_at<=NOW()
                    ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1)
                 RETURNING *
            """)
                    )
                )
                .mappings()
                .first()
            )
            if row is None:
                return LifecycleProgress().json()
            source = (
                (
                    await session.execute(
                        text("""
                SELECT t.status,t.exit_reason,t.broker_net_quantity,t.exit_datetime,
                       t.last_position_reconciled_at,t.strategy_key,s.lot_size
                  FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
                 WHERE t.id=:trade
            """),
                        {"trade": row["source_trade_id"]},
                    )
                )
                .mappings()
                .first()
            )
            today = now.astimezone(IST).date()
            if row["created_at"].astimezone(IST).date() < today:
                await self._state(
                    session, row["source_trade_id"], "cancelled", "Stale next-day reversal blocked."
                )
                return LifecycleProgress(claimed=1).json()
            if source is None or source["status"] != "closed" or source["exit_reason"] != "SL2":
                await self._state(
                    session,
                    row["source_trade_id"],
                    "cancelled",
                    "Source is not a confirmed SL2 close.",
                )
                return LifecycleProgress(claimed=1).json()
            flat = (
                int(source["broker_net_quantity"] or 0) == 0
                and source["last_position_reconciled_at"] is not None
                and source["last_position_reconciled_at"] >= source["exit_datetime"]
            )
            if not flat:
                await self._state(
                    session,
                    row["source_trade_id"],
                    "waiting",
                    "Waiting for broker-flat source reconciliation.",
                )
                return LifecycleProgress(claimed=1, waiting=1).json()
            active = bool(
                await session.scalar(
                    text("""
                    SELECT EXISTS(
                      SELECT 1 FROM user_strategy_configs c
                      JOIN user_strategy_activations a
                        ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key
                      JOIN users u ON u.id=c.user_id
                       WHERE c.user_id=:user AND c.strategy_key=:strategy
                         AND c.instrument=:instrument AND c.enabled=TRUE
                         AND a.is_active=TRUE AND u.is_active=TRUE)
                    """),
                    {"user": row["user_id"], "strategy": FUTURES, "instrument": row["instrument"]},
                )
            )
            if not active:
                await self._state(
                    session,
                    row["source_trade_id"],
                    "cancelled",
                    "Strategy or instrument was deactivated before reversal submission.",
                )
                return LifecycleProgress(claimed=1).json()
            collision = bool(
                await session.scalar(
                    text("""
                SELECT EXISTS(SELECT 1 FROM trades WHERE user_id=:user AND strategy_key=:strategy
                  AND instrument_label=:instrument AND status='open')
            """),
                    {"user": row["user_id"], "strategy": FUTURES, "instrument": row["instrument"]},
                )
            )
            if collision:
                await self._state(
                    session,
                    row["source_trade_id"],
                    "waiting",
                    "Existing strategy exposure blocks reversal.",
                )
                return LifecycleProgress(claimed=1, waiting=1).json()
            quantity = int(row["lots"]) * max(int(source["lot_size"] or 1), 1)
            role = "BUY_ENTRY" if row["reversal_direction"] == "BUY" else "SELL_ENTRY"
            order_id = _order_uuid("sl2-reversal", row["source_trade_id"])
            await DurableOrderFactory(session).create(
                order_id=order_id,
                user_id=UUID(str(row["user_id"])),
                snapshot_id=UUID(str(row["snapshot_id"])),
                trade_id=UUID(str(row["source_trade_id"])),
                session_key=str(row["order_session_key"]),
                role=role,
                side=str(row["reversal_direction"]),
                lots=int(row["lots"]),
                quantity=quantity,
                price=Decimal(str(row["entry_price"])),
                trigger_price=None,
                order_type="MARKET",
                idempotency_key=f"python:sl2-reversal:{row['source_trade_id']}",
            )
        try:
            outcome = await self.coordinator.place_order(order_id, action=ActionKind.SL2_REVERSAL)
        except MutationPendingError:
            outcome = None
        async with self.session_factory() as session, session.begin():
            if outcome is not None and outcome.state is MutationState.ACKNOWLEDGED:
                await self._state(session, row["source_trade_id"], "submitted", "")
                return LifecycleProgress(claimed=1, submitted=1).json()
            if outcome is None or outcome.state is MutationState.AMBIGUOUS:
                await self._state(
                    session,
                    row["source_trade_id"],
                    "submitted",
                    "Reversal submission requires reconciliation.",
                )
                return LifecycleProgress(claimed=1, ambiguous=1).json()
            await self._state(
                session, row["source_trade_id"], "failed", "Reversal submission failed closed."
            )
            return LifecycleProgress(claimed=1, waiting=1).json()

    @staticmethod
    async def _state(session: AsyncSession, source, status: str, error: str) -> None:
        await session.execute(
            text("""
            UPDATE strategy_reversal_intents SET status=CAST(:status AS varchar(24)),last_error=:error,
             next_attempt_at=CASE WHEN CAST(:status AS varchar(24)) IN ('waiting','failed') THEN NOW()+INTERVAL '5 seconds' ELSE next_attempt_at END,
             updated_at=NOW() WHERE source_trade_id=:source
        """),
            {"status": status, "error": error, "source": source},
        )


class RiskReducingCloseWorker:
    """Shared cancellation-then-close lifecycle for manual and 15:10 exits."""

    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        coordinator: LiveMutationCoordinator,
    ) -> None:
        self.session_factory = session_factory
        self.coordinator = coordinator

    async def run_manual_once(self) -> dict[str, object]:
        async with self.session_factory() as session:
            ids = (
                (
                    await session.execute(
                        text("""
                SELECT trade_id FROM manual_trade_close_intents
                 WHERE status IN ('requested','cancelling_protection','partially_filled','reconciliation_required')
                 ORDER BY requested_at LIMIT 100
            """)
                    )
                )
                .scalars()
                .all()
            )
        progress = LifecycleProgress()
        for value in ids:
            outcome = await self._close(UUID(str(value)), manual=True)
            progress = LifecycleProgress(
                progress.claimed + 1,
                progress.submitted + (outcome == "submitted"),
                progress.completed + (outcome == "completed"),
                progress.waiting + (outcome == "waiting"),
                progress.ambiguous + (outcome == "ambiguous"),
            )
        return progress.json()

    async def _close(self, trade_id: UUID, *, manual: bool) -> str:
        pending_cancel: UUID | None = None
        close_status = "pending"
        async with self.session_factory() as session, session.begin():
            trade = (
                (
                    await session.execute(
                        text("""
                SELECT t.*,s.lot_size FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
                 WHERE t.id=:trade FOR UPDATE OF t
            """),
                        {"trade": trade_id},
                    )
                )
                .mappings()
                .first()
            )
            if trade is None or trade["execution_mode"] != "live":
                return "completed"
            if trade["status"] == "closed":
                if manual:
                    await session.execute(
                        text("""
                        UPDATE manual_trade_close_intents SET status='completed',completed_at=COALESCE(completed_at,NOW()),
                         last_error='',updated_at=NOW() WHERE trade_id=:trade
                    """),
                        {"trade": trade_id},
                    )
                return "completed"
            expected = int(trade["quantity"]) * (1 if trade["direction"] == "BUY" else -1)
            healthy = await session.scalar(
                text("""
                SELECT COALESCE(healthy,FALSE) AND checked_at>=NOW()-INTERVAL '5 minutes'
                  FROM broker_reconciliation_health WHERE user_id=:user
                """),
                {"user": trade["user_id"]},
            )
            collision = bool(
                await session.scalar(
                    text("""
                    SELECT EXISTS(
                      SELECT 1 FROM broker_exposure_observations o
                      JOIN strategy_market_snapshots s ON s.id=:snapshot
                       WHERE o.user_id=:user AND UPPER(o.exchange_segment)=UPPER(s.exchange_segment)
                         AND o.contract_token=s.contract_token
                         AND o.ownership_status IN ('manual_external','ambiguous')
                         AND o.observed_at>=NOW()-INTERVAL '5 minutes')
                    """),
                    {"snapshot": trade["strategy_snapshot_id"], "user": trade["user_id"]},
                )
            )
            fresh = bool(
                healthy
                and trade["last_position_reconciled_at"]
                and trade["last_position_reconciled_at"] >= datetime.now(UTC) - timedelta(minutes=5)
                and int(trade["broker_net_quantity"] or 0) == expected
                and not collision
            )
            if not fresh:
                if manual:
                    await session.execute(
                        text("""
                        UPDATE manual_trade_close_intents SET status='reconciliation_required',
                         last_error='Fresh attributable broker evidence is required before LIVE close.',
                         updated_at=NOW() WHERE trade_id=:trade
                        """),
                        {"trade": trade_id},
                    )
                return "waiting"
            active = (
                (
                    await session.execute(
                        text("""
                SELECT id,broker_order_id,order_type,status FROM strategy_orders
                 WHERE trade_id=:trade AND role IN ('TARGET','SL1','SL2')
                   AND status IN ('ambiguous','submitted','partially_filled','processing','cancelling')
                 ORDER BY created_at FOR UPDATE
            """),
                        {"trade": trade_id},
                    )
                )
                .mappings()
                .all()
            )
            if active:
                candidate = next(
                    (
                        item
                        for item in active
                        if item["status"] != "cancelling" and item["broker_order_id"]
                    ),
                    None,
                )
                if candidate is None:
                    if manual:
                        await session.execute(
                            text("""
                            UPDATE manual_trade_close_intents SET status='reconciliation_required',
                             last_error='Protective-order cancellation awaits authoritative broker evidence.',updated_at=NOW()
                             WHERE trade_id=:trade
                        """),
                            {"trade": trade_id},
                        )
                    return "waiting"
                pending_cancel = UUID(str(candidate["id"]))
                variety = (
                    "STOPLOSS" if str(candidate["order_type"]).startswith("STOPLOSS") else "NORMAL"
                )
                if manual:
                    await session.execute(
                        text("""
                        UPDATE manual_trade_close_intents SET status='cancelling_protection',last_error='',updated_at=NOW()
                         WHERE trade_id=:trade
                    """),
                        {"trade": trade_id},
                    )
            else:
                order_id = _order_uuid("manual-close" if manual else "eod", trade_id)
                role = "EMERGENCY_CLOSE"
                session_key = f"mc-{trade_id.hex[:16]}" if manual else f"stsq-{trade_id.hex[:16]}"
                lot_size = max(int(trade["lot_size"] or 1), 1)
                quantity = int(trade["quantity"])
                await DurableOrderFactory(session).create(
                    order_id=order_id,
                    user_id=UUID(str(trade["user_id"])),
                    snapshot_id=UUID(str(trade["strategy_snapshot_id"])),
                    trade_id=trade_id,
                    session_key=session_key,
                    role=role,
                    side=_exit_side(str(trade["direction"])),
                    lots=max(1, (quantity + lot_size - 1) // lot_size),
                    quantity=quantity,
                    price=Decimal(str(trade["last_price"] or trade["entry_price"])),
                    trigger_price=None,
                    order_type="MARKET",
                    idempotency_key=f"python:{'manual-close' if manual else 'eod'}:{trade_id}",
                )
                close_status = str(
                    await session.scalar(
                        text("SELECT status FROM strategy_orders WHERE id=:order"),
                        {"order": order_id},
                    )
                )
                await session.execute(
                    text("""
                    UPDATE trades SET safety_status='CLOSING',updated_at=NOW() WHERE id=:trade
                """),
                    {"trade": trade_id},
                )
                if manual:
                    await session.execute(
                        text("""
                        UPDATE manual_trade_close_intents SET status='submitted',strategy_order_id=:order,
                         last_error='',updated_at=NOW() WHERE trade_id=:trade
                    """),
                        {"trade": trade_id, "order": order_id},
                    )
                else:
                    await session.execute(
                        text("""
                        UPDATE strategy_execution_intents
                           SET strategy_order_id=:order,status='submitted',updated_at=NOW()
                         WHERE trade_id=:trade AND action='SQUARE_OFF'
                           AND status IN ('pending','retry_wait','claimed','submitted')
                    """),
                        {"trade": trade_id, "order": order_id},
                    )
        if pending_cancel is not None:
            try:
                outcome = await self.coordinator.cancel_order(pending_cancel, variety=variety)
            except MutationPendingError:
                return "ambiguous"
            return "waiting" if outcome.state is MutationState.ACKNOWLEDGED else "ambiguous"
        if close_status != "pending":
            return "ambiguous" if close_status == "ambiguous" else "submitted"
        action = ActionKind.MANUAL_CLOSE if manual else ActionKind.EOD_SQUARE_OFF
        try:
            outcome = await self.coordinator.place_order(order_id, action=action)
        except MutationPendingError:
            return "ambiguous"
        if outcome.state is MutationState.ACKNOWLEDGED:
            return "submitted"
        async with self.session_factory() as session, session.begin():
            if manual:
                await session.execute(
                    text("""
                    UPDATE manual_trade_close_intents SET status=:status,
                     last_error='Close submission requires authoritative reconciliation.',updated_at=NOW()
                     WHERE trade_id=:trade
                """),
                    {
                        "status": "ambiguous"
                        if outcome.state is MutationState.AMBIGUOUS
                        else "failed",
                        "trade": trade_id,
                    },
                )
        return "ambiguous" if outcome.state is MutationState.AMBIGUOUS else "waiting"


class EodLifecycleWorker:
    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        close_worker: RiskReducingCloseWorker,
    ) -> None:
        self.session_factory = session_factory
        self.close_worker = close_worker

    async def run_once(self, now: datetime | None = None) -> dict[str, object]:
        observed = (now or datetime.now(UTC)).astimezone(IST)
        if observed.time() < time(15, 10):
            return {"due": False, **LifecycleProgress().json()}
        async with self.session_factory() as session, session.begin():
            trades = (
                (
                    await session.execute(
                        text("""
                SELECT t.id,t.user_id,t.strategy_snapshot_id,t.strategy_key,t.instrument_label,
                       t.direction,t.quantity,t.last_price,t.entry_price
                  FROM trades t WHERE t.execution_mode='live' AND t.status='open'
                   AND t.strategy_key=:strategy ORDER BY t.user_id,t.id
            """),
                        {"strategy": SUPERTREND},
                    )
                )
                .mappings()
                .all()
            )
            intents: list[PreparedIntent] = []
            for trade in trades:
                intents.append(
                    prepare_square_off_intent(
                        user_id=UUID(str(trade["user_id"])),
                        trade_id=UUID(str(trade["id"])),
                        snapshot_id=UUID(str(trade["strategy_snapshot_id"])),
                        strategy_key=SUPERTREND,
                        instrument=str(trade["instrument_label"]),
                        session_key=f"stsq-{observed:%Y%m%d}",
                        side=_exit_side(str(trade["direction"])),
                        quantity=int(trade["quantity"]),
                        price=Decimal(str(trade["last_price"] or trade["entry_price"])),
                    )
                )
            if intents:
                await SignalRepository(session).materialize(
                    strategy_key=SUPERTREND,
                    instrument="ALL",
                    session_key=f"squareoff-{observed:%Y%m%d}-1510",
                    signal_at=observed,
                    signal_type="SQUARE_OFF",
                    snapshot_id=None,
                    payload={"scheduled_for": "15:10 IST"},
                    intents=intents,
                )
        progress = LifecycleProgress()
        for trade in trades:
            outcome = await self.close_worker._close(UUID(str(trade["id"])), manual=False)
            progress = LifecycleProgress(
                progress.claimed + 1,
                progress.submitted + (outcome == "submitted"),
                progress.completed + (outcome == "completed"),
                progress.waiting + (outcome == "waiting"),
                progress.ambiguous + (outcome == "ambiguous"),
            )
        return {"due": True, **progress.json()}


__all__ = [
    "AuthoritativeExecutionWorker",
    "DurableOrderFactory",
    "EodLifecycleWorker",
    "FillLifecycleWorker",
    "LifecycleProgress",
    "ProtectionLifecycleWorker",
    "ReversalLifecycleWorker",
    "RiskReducingCloseWorker",
]
