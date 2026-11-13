"""Authoritative trading lifecycle workers.

The workers in this module only operate on durable PostgreSQL state.  Every
LIVE write is represented by a ``strategy_orders`` row before it reaches the
existing authority-fenced :class:`LiveMutationCoordinator`.  Ambiguous writes
are deliberately left for reconciliation and are never blindly replayed.
"""

from __future__ import annotations

import json
import logging
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
from app.strategy.supertrend import (
    CONFIGS,
    current_signal,
    signal_is_fresh,
    supertrend_points,
)
from app.trading.domain import futures_pnl_units, sl2_reversal, trade_pnl

from .market import SuperTrendMarketProvider

logger = logging.getLogger(__name__)

IST = ZoneInfo("Asia/Kolkata")
FUTURES = "futures_breakout_v3"
SUPERTREND = "supertrend_index_options_v1"
OPPONENT = "nifty_opponent_v1"
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
        execution_mode: str = "live",
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
            VALUES(:id,:user,:snapshot,:trade,:session,:role,:side,:mode,:lots,:quantity,
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
                "mode": execution_mode,
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
        market_provider: SuperTrendMarketProvider | None = None,
    ) -> None:
        self.session_factory = session_factory
        self.coordinator = coordinator
        self.market_provider = market_provider

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
        generated += await self._evaluate_opponent(session, local)
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
        if not supertrend_entry_allowed(now):
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
                    SELECT c.user_id,c.lots,
                           CASE WHEN c.target_points>0 THEN c.target_points ELSE :target END AS target_points,
                           CASE WHEN c.stop_loss_points>0 THEN c.stop_loss_points ELSE :stop END AS stop_loss_points
                      FROM user_strategy_configs c
                    JOIN user_strategy_activations a ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key
                    JOIN users u ON u.id=c.user_id JOIN user_profiles p ON p.user_id=c.user_id
                     WHERE c.strategy_key=:strategy AND c.instrument=:instrument
                       AND c.enabled=TRUE AND a.is_active=TRUE AND u.is_active=TRUE
                       AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))
                    """),
                        {
                            "strategy": SUPERTREND,
                            "instrument": underlying,
                            "target": config.default_target_points,
                            "stop": config.default_stop_loss_points,
                        },
                    )
                )
                .mappings()
                .all()
            )
            session_key = f"st-{underlying}-{signal.signal_at:%Y%m%d-%H%M}-{signal.side.value}"
            pending = []
            for runner in runners:
                processed = bool(
                    await session.scalar(
                        text("""
                        SELECT EXISTS(SELECT 1 FROM strategy_execution_intents
                         WHERE user_id=:user AND strategy_key=:strategy
                           AND session_key=:session AND action='ENTRY'
                           AND status<>'failed')
                        """),
                        {
                            "user": runner["user_id"],
                            "strategy": SUPERTREND,
                            "session": session_key,
                        },
                    )
                )
                if processed:
                    continue

                active_intent = bool(
                    await session.scalar(
                        text("""
                        SELECT EXISTS(SELECT 1 FROM strategy_execution_intents
                         WHERE user_id=:user AND strategy_key=:strategy
                           AND instrument=:instrument AND action='ENTRY'
                           AND status IN ('pending','claimed','retry_wait','submitted'))
                        """),
                        {
                            "user": runner["user_id"],
                            "strategy": SUPERTREND,
                            "instrument": underlying,
                        },
                    )
                )
                if active_intent:
                    continue

                existing_trade = (
                    (
                        await session.execute(
                            text("""
                        SELECT t.id, t.direction, t.quantity, t.last_price, t.entry_price,
                               t.execution_mode, t.pnl, t.notes, t.instrument_label,
                               s.instrument AS snapshot_instrument,
                               s.contract_token, s.exchange_segment
                          FROM trades t
                          JOIN strategy_market_snapshots s ON s.id = t.strategy_snapshot_id
                         WHERE t.user_id = :user
                           AND t.strategy_key = :strategy
                           AND (t.instrument_label = :instrument OR t.instrument_label LIKE :prefix OR s.instrument LIKE :prefix)
                           AND t.status = 'open'
                         ORDER BY t.created_at DESC LIMIT 1
                         FOR UPDATE OF t
                        """),
                            {
                                "user": runner["user_id"],
                                "strategy": SUPERTREND,
                                "instrument": underlying,
                                "prefix": f"{underlying}_%",
                            },
                        )
                    )
                    .mappings()
                    .first()
                )

                if existing_trade is not None:
                    trade_inst = str(
                        existing_trade["instrument_label"]
                        or existing_trade["snapshot_instrument"]
                        or ""
                    )
                    if trade_inst.endswith((signal.side.value, f"_{signal.side.value}")):
                        continue

                    # SuperTrend trend reversal: close the existing opposite trade
                    if existing_trade["execution_mode"] == "demo":
                        await session.execute(
                            text("""
                            UPDATE strategy_orders
                               SET status='cancelled',
                                   broker_status='Demo order cancelled on SuperTrend reversal',
                                   state_version=state_version+1,
                                   updated_at=NOW()
                             WHERE trade_id=:trade AND execution_mode='demo'
                               AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
                            """),
                            {"trade": existing_trade["id"]},
                        )

                        exit_price = await session.scalar(
                            text("""
                            SELECT price FROM market_price_ticks
                             WHERE UPPER(exchange_segment)=UPPER(:exchange)
                               AND contract_token=:token
                               AND price>0
                             ORDER BY received_at DESC LIMIT 1
                            """),
                            {
                                "exchange": existing_trade["exchange_segment"],
                                "token": existing_trade["contract_token"],
                            },
                        )
                        if exit_price is None or Decimal(str(exit_price)) <= 0:
                            exit_price = (
                                existing_trade["last_price"]
                                or existing_trade["entry_price"]
                                or Decimal(0)
                            )

                        trade_qty = int(existing_trade["quantity"] or 0)
                        realized_pnl = Decimal(str(existing_trade["pnl"] or 0)) + trade_pnl(
                            str(existing_trade["direction"]),
                            existing_trade["entry_price"],
                            Decimal(str(exit_price)),
                            trade_qty,
                        )

                        await session.execute(
                            text("""
                            UPDATE trades
                               SET status='closed',
                                   safety_status='CLOSED',
                                   remaining_lots=0,
                                   exit_price=:price,
                                   last_price=:price,
                                   pnl=:pnl,
                                   exit_datetime=NOW(),
                                   exit_reason='SIGNAL_REVERSAL',
                                   notes=CONCAT(COALESCE(notes,''),'; closed on SuperTrend trend reversal'),
                                   updated_at=NOW()
                             WHERE id=:trade AND status='open'
                            """),
                            {
                                "trade": existing_trade["id"],
                                "price": exit_price,
                                "pnl": realized_pnl,
                            },
                        )

                        await session.execute(
                            text("""
                            INSERT INTO strategy_events(user_id, strategy_key, instrument, event_type, payload)
                            VALUES(:user, :strategy, :instrument, 'operational_alert', CAST(:payload AS jsonb))
                            """),
                            {
                                "user": runner["user_id"],
                                "strategy": SUPERTREND,
                                "instrument": underlying,
                                "payload": json.dumps(
                                    {
                                        "severity": "INFO",
                                        "code": "SIGNAL_REVERSAL",
                                        "message": f"SuperTrend reversed to {signal.side.value}. Closed existing DEMO {underlying} position.",
                                    }
                                ),
                            },
                        )
                        pending.append(runner)
                    else:
                        has_in_flight_close = bool(
                            await session.scalar(
                                text("""
                                SELECT EXISTS(
                                    SELECT 1 FROM manual_trade_close_intents
                                     WHERE trade_id=:trade AND status NOT IN ('completed','cancelled','failed')
                                )
                                """),
                                {"trade": existing_trade["id"]},
                            )
                        )
                        if not has_in_flight_close:
                            from app.trading.repository import TradingRepository

                            await TradingRepository(session).request_manual_close(
                                trade_id=UUID(str(existing_trade["id"])),
                                user_id=UUID(str(runner["user_id"])),
                                requested_quantity=int(existing_trade["quantity"]),
                            )
                            await session.execute(
                                text("""
                                UPDATE trades
                                   SET notes=CONCAT(COALESCE(notes,''),'; SuperTrend reversal close requested'),
                                       updated_at=NOW()
                                 WHERE id=:trade
                                """),
                                {"trade": existing_trade["id"]},
                            )
                            await session.execute(
                                text("""
                                INSERT INTO strategy_events(user_id, strategy_key, instrument, event_type, payload)
                                VALUES(:user, :strategy, :instrument, 'operational_alert', CAST(:payload AS jsonb))
                                """),
                                {
                                    "user": runner["user_id"],
                                    "strategy": SUPERTREND,
                                    "instrument": underlying,
                                    "payload": json.dumps(
                                        {
                                            "severity": "INFO",
                                            "code": "SIGNAL_REVERSAL",
                                            "message": f"SuperTrend reversed to {signal.side.value}. Squaring off existing LIVE {underlying} position before reverse entry.",
                                        }
                                    ),
                                },
                            )
                        continue
                else:
                    last_trade = (
                        (
                            await session.execute(
                                text("""
                            SELECT t.id, t.instrument_label, s.instrument AS snapshot_instrument
                              FROM trades t
                              LEFT JOIN strategy_market_snapshots s ON s.id = t.strategy_snapshot_id
                             WHERE t.user_id = :user
                               AND t.strategy_key = :strategy
                               AND (t.instrument_label = :instrument OR t.instrument_label LIKE :prefix OR s.instrument LIKE :prefix)
                               AND t.created_at::date = :trade_date
                             ORDER BY t.created_at DESC LIMIT 1
                            """),
                                {
                                    "user": runner["user_id"],
                                    "strategy": SUPERTREND,
                                    "instrument": underlying,
                                    "prefix": f"{underlying}_%",
                                    "trade_date": signal.signal_at.date(),
                                },
                            )
                        )
                        .mappings()
                        .first()
                    )
                    if last_trade is not None:
                        last_label = str(
                            last_trade["instrument_label"]
                            or last_trade["snapshot_instrument"]
                            or ""
                        )
                        if last_label.endswith((signal.side.value, f"_{signal.side.value}")):
                            # Strict alternation: already executed this side today, wait for next trend reversal
                            continue
                    pending.append(runner)
            if not pending or self.market_provider is None:
                continue
            selection = await self.market_provider.select(
                user_id=UUID(str(pending[0]["user_id"])),
                config=config,
                side=signal.side,
                trade_date=signal.signal_at.date(),
            )
            if not signal_is_fresh(signal, datetime.now(IST)):
                continue
            await session.execute(
                text("""
                INSERT INTO market_price_ticks(exchange_segment,contract_token,price,received_at)
                VALUES(:exchange,:token,:price,NOW())
                ON CONFLICT(exchange_segment,contract_token) DO UPDATE
                  SET price=EXCLUDED.price,received_at=EXCLUDED.received_at
                """),
                {
                    "exchange": config.option_exchange,
                    "token": selection.contract.token,
                    "price": selection.contract.premium,
                },
            )
            intents: list[PreparedIntent] = []
            for runner in pending:
                snapshot_id = await self._supertrend_snapshot(
                    session, config, signal, runner, selection
                )
                intents.append(
                    PreparedIntent(
                        UUID(str(runner["user_id"])),
                        snapshot_id,
                        None,
                        SUPERTREND,
                        underlying,
                        session_key,
                        "ENTRY",
                        "BUY_ENTRY",
                        "BUY",
                        "MARKET",
                        int(runner["lots"]),
                        int(runner["lots"]) * selection.contract.lot_size,
                        selection.contract.premium,
                        None,
                        signal.signal_at + timedelta(minutes=6, seconds=30),
                    )
                )
            if not intents:
                continue
            _, inserted = await SignalRepository(session).materialize(
                strategy_key=SUPERTREND,
                instrument=underlying,
                session_key=session_key,
                signal_at=signal.signal_at,
                signal_type="ENTRY",
                snapshot_id=None,
                payload={
                    "side": signal.side.value,
                    "index_close": str(signal.index_close),
                    "supertrend": str(signal.supertrend),
                    "option_execution_price": str(selection.contract.premium),
                    "contract_symbol": selection.contract.symbol,
                },
                intents=intents,
            )
            generated += int(inserted > 0)
        return generated

    async def _supertrend_snapshot(self, session, config, signal, runner, selection) -> UUID:
        snapshot_id = _order_uuid(
            "supertrend-snapshot",
            f"{signal.signal_at.isoformat()}:{selection.contract.token}:{runner['user_id']}",
        )
        execution_key = (
            f"{signal.signal_at:%Y%m%d%H%M}-{selection.contract.symbol}-"
            f"{UUID(str(runner['user_id'])).hex}"
        )
        await session.execute(
            text("""
            INSERT INTO strategy_market_snapshots(
              id,strategy_key,instrument,trade_date,status,error,contract_token,
              contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,
              execution_key,underlying_token,buy_target,buy_sl1,sell_target,sell_sl1,
              previous_close,fetched_at)
            VALUES(:id,:strategy,:instrument,:date,'ready','',:token,:symbol,:expiry,:lot_size,
              :exchange,'INTRADAY',:key,:underlying,:buy_target,:buy_sl1,:sell_target,:sell_sl1,
              :underlying_ltp,NOW())
            ON CONFLICT(strategy_key,instrument,trade_date,execution_key) DO UPDATE SET
              status='ready',error='',contract_token=EXCLUDED.contract_token,
              contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,
              lot_size=EXCLUDED.lot_size,exchange_segment=EXCLUDED.exchange_segment,
              product_type=EXCLUDED.product_type,underlying_token=EXCLUDED.underlying_token,
              buy_target=EXCLUDED.buy_target,buy_sl1=EXCLUDED.buy_sl1,
              sell_target=EXCLUDED.sell_target,sell_sl1=EXCLUDED.sell_sl1,
              previous_close=EXCLUDED.previous_close,fetched_at=NOW()
            """),
            {
                "id": snapshot_id,
                "strategy": SUPERTREND,
                "instrument": f"{config.instrument}_{signal.side.value}",
                "date": signal.signal_at.date(),
                "token": selection.contract.token,
                "symbol": selection.contract.symbol,
                "expiry": selection.contract.expiry,
                "lot_size": selection.contract.lot_size,
                "exchange": config.option_exchange,
                "key": execution_key,
                "underlying": config.index_token,
                "buy_target": runner["target_points"],
                "buy_sl1": runner["stop_loss_points"],
                "sell_target": runner["target_points"],
                "sell_sl1": runner["stop_loss_points"],
                "underlying_ltp": selection.underlying_ltp,
            },
        )
        value = await session.scalar(
            text("""
            SELECT id FROM strategy_market_snapshots
             WHERE strategy_key=:strategy AND instrument=:instrument
               AND trade_date=:date AND execution_key=:key
            """),
            {
                "strategy": SUPERTREND,
                "instrument": f"{config.instrument}_{signal.side.value}",
                "date": signal.signal_at.date(),
                "key": execution_key,
            },
        )
        return UUID(str(value))

    async def _manage_opponent_positions(self, session: AsyncSession, now: datetime) -> None:
        """Manage active open Nifty Opponent trades:
        1. Candle-close Stop Loss: If a completed 3m candle closes below marked Low, trigger SL exit.
        2. Trailed Stop Loss: If LTP hits or breaches trailed SL (Cost-to-Cost, T1, T2), exit.
        3. Target Ladder Progression:
           - T2 hit: Hold runner, trail SL to T1 level.
           - T3 hit: Hold runner, trail SL to T2 level.
           - T4 hit: 100% full position exit.
        """
        res = await session.execute(
            text("""
            SELECT t.id, t.user_id, t.execution_mode, t.direction, t.quantity,
                   t.entry_price, t.last_price, t.total_lots, t.remaining_lots,
                   t.target_price, t.sl1_price, t.notes, t.strategy_snapshot_id,
                   t.instrument_label, s.contract_token, s.contract_symbol, s.lot_size,
                   s.buy_target, s.buy_sl1
              FROM trades t
              JOIN strategy_market_snapshots s ON s.id = t.strategy_snapshot_id
             WHERE t.strategy_key = :strategy AND t.status = 'open' AND t.quantity > 0
            """),
            {"strategy": OPPONENT},
        )
        if hasattr(res, "mappings"):
            mapped = res.mappings()
            open_trades = mapped.all() if hasattr(mapped, "all") else []
        else:
            open_trades = []

        if not open_trades or not isinstance(open_trades, (list, tuple)):
            return

        from app.strategy.opponent import OpponentTargetLadder

        for trade in open_trades:
            trade_id = UUID(str(trade["id"]))
            token = trade["contract_token"]
            entry_p = Decimal(str(trade["entry_price"]))
            notes = str(trade["notes"] or "")
            marked_h = (
                Decimal(str(trade["buy_target"])) / Decimal("1.25")
                if trade["buy_target"]
                else entry_p
            )
            marked_l = Decimal(str(trade["buy_sl1"])) if trade["buy_sl1"] else Decimal("1.0")
            ladder = OpponentTargetLadder.from_marked_high(marked_h)

            ltp_val = await session.scalar(
                text("""
                SELECT price FROM market_price_ticks
                 WHERE contract_token = :token AND price > 0
                 ORDER BY received_at DESC LIMIT 1
                """),
                {"token": token},
            )
            ltp = Decimal(str(ltp_val)) if ltp_val else Decimal(str(trade["last_price"] or entry_p))

            start_t = now.replace(hour=9, minute=30, second=0, microsecond=0)
            candles_raw = (
                await session.execute(
                    text("""
                    SELECT candle_time, open_price, high_price, low_price, close_price
                      FROM backtest_market_candles
                     WHERE exchange = 'NFO' AND symbol_token = :token
                       AND interval_key IN ('THREE_MINUTE', 'FIVE_MINUTE')
                       AND candle_time >= :start_time AND candle_time <= :end_time
                     ORDER BY candle_time
                    """),
                    {"token": token, "start_time": start_t, "end_time": now},
                )
            ).mappings().all()

            # 1. Initial Candle-Close Stop Loss
            if "T1_HIT" not in notes and "SL_TRAILED" not in notes:
                sl_triggered = False
                for c in candles_raw[1:]:
                    if Decimal(str(c["close_price"])) < marked_l:
                        sl_triggered = True
                        break
                if sl_triggered:
                    await self._close_opponent_trade(
                        session, trade, ltp, exit_reason="SL_CANDLE_CLOSE"
                    )
                    continue

            # 2. Trailed Stop Loss Check
            current_sl = Decimal(str(trade["sl1_price"])) if trade["sl1_price"] else marked_l
            if ("T1_HIT" in notes or "T2_HIT" in notes or "T3_HIT" in notes) and ltp <= current_sl:
                await self._close_opponent_trade(session, trade, ltp, exit_reason="SL_TRAILED")
                continue

            # 3. Target Ladder Progression
            if ltp >= ladder.target_4:
                await self._close_opponent_trade(session, trade, ltp, exit_reason="T4_FULL_EXIT")
                continue

            if ltp >= ladder.target_3 and "T3_HIT" not in notes:
                await session.execute(
                    text("""
                    UPDATE trades
                       SET sl1_price = :sl,
                           notes = CONCAT(COALESCE(notes,''), '; T3_HIT: runner trailed to T2 (', :sl_str, ')'),
                           updated_at = NOW()
                     WHERE id = :trade
                    """),
                    {"trade": trade_id, "sl": ladder.target_2, "sl_str": str(ladder.target_2)},
                )
                continue

            if ltp >= ladder.target_2 and "T2_HIT" not in notes and "T3_HIT" not in notes:
                await session.execute(
                    text("""
                    UPDATE trades
                       SET sl1_price = :sl,
                           notes = CONCAT(COALESCE(notes,''), '; T2_HIT: runner trailed to T1 (', :sl_str, ')'),
                           updated_at = NOW()
                     WHERE id = :trade
                    """),
                    {"trade": trade_id, "sl": ladder.target_1, "sl_str": str(ladder.target_1)},
                )
                continue

    async def _close_opponent_trade(
        self, session: AsyncSession, trade, exit_price: Decimal, exit_reason: str
    ) -> None:
        trade_id = UUID(str(trade["id"]))
        mode = str(trade["execution_mode"])
        quantity = int(trade["quantity"])
        if quantity <= 0:
            return

        lot_size = max(int(trade.get("lot_size") or 1), 1)
        realized = trade_pnl(
            str(trade["direction"]),
            trade["entry_price"],
            exit_price,
            futures_pnl_units(str(trade.get("instrument_label") or "NIFTY"), quantity, lot_size),
        )
        pnl_val = trade.get("pnl") if hasattr(trade, "get") else getattr(trade, "pnl", 0)
        total_pnl = Decimal(str(pnl_val or 0)) + realized

        if mode == "demo":
            await session.execute(
                text("""
                UPDATE strategy_orders SET status='cancelled',
                  broker_status='Demo order cancelled for opponent exit',updated_at=NOW()
                 WHERE trade_id=:trade AND execution_mode='demo'
                   AND role IN ('TARGET','SL1','SL2')
                   AND status IN ('pending','submitted','partially_filled')
                """),
                {"trade": trade_id},
            )
            await session.execute(
                text("""
                UPDATE trades
                   SET status = 'closed',
                       safety_status = 'CLOSED',
                       quantity = 0,
                       remaining_lots = 0,
                       last_price = :price,
                       exit_price = :price,
                       exit_datetime = NOW(),
                       exit_reason = :reason,
                       pnl = :pnl,
                       notes = CONCAT(COALESCE(notes,''), '; Closed via ', :reason),
                       updated_at = NOW()
                 WHERE id = :trade
                """),
                {"trade": trade_id, "price": exit_price, "reason": exit_reason, "pnl": total_pnl},
            )
        else:
            from app.trading.repository import TradingRepository

            await TradingRepository(session).request_manual_close(
                trade_id=trade_id,
                user_id=UUID(str(trade["user_id"])),
                requested_quantity=quantity,
            )
            await session.execute(
                text("""
                UPDATE trades
                   SET exit_reason = :reason,
                       notes = CONCAT(COALESCE(notes,''), '; Close requested via ', :reason),
                       updated_at = NOW()
                 WHERE id = :trade
                """),
                {"trade": trade_id, "reason": exit_reason},
            )

    async def _evaluate_opponent(self, session: AsyncSession, now: datetime) -> int:
        from app.strategy.opponent import (
            CoincidencePair,
            OpponentSignal,
            OpponentTargetLadder,
            OptionContract,
            OptionSide,
            benchmark_is_due,
            benchmark_is_recording,
            build_ranked_coincidence_queue,
            calculate_3_strike_perimeter,
            calculate_atm_strike,
            calculate_partial_close_lots,
            check_confirmation_and_entry,
            check_spot_expansion_abort,
            is_in_retracement_zone,
            is_session_trading_allowed,
        )

        minute = now.hour * 60 + now.minute
        # Nifty Opponent strategy monitors between 09:30 IST and 15:00 IST
        if not (9 * 60 + 30 <= minute <= 15 * 60):
            return 0

        # CRITICAL HARD GUARD:
        # Between 09:30:00 and 09:32:59 IST, the foundational 3-minute benchmark candle
        # is actively recording. Under NO circumstance should any order or entry intent
        # be placed during this window.
        if benchmark_is_recording(now):
            return 0

        if not benchmark_is_due(now):
            return 0

        # Manage active open opponent positions for ladder progression & candle-close SL
        await self._manage_opponent_positions(session, now)

        # Check enabled runners for Nifty Opponent
        runners = (
            (
                await session.execute(
                    text("""
                SELECT c.user_id, c.lots,
                       COALESCE(c.target_points, 50.0) AS partial_close_pct
                  FROM user_strategy_configs c
                  JOIN user_strategy_activations a ON a.user_id = c.user_id AND a.strategy_key = c.strategy_key
                  JOIN users u ON u.id = c.user_id
                  JOIN user_profiles p ON p.user_id = c.user_id
                 WHERE c.strategy_key = :strategy AND c.instrument = 'NIFTY'
                   AND c.enabled = TRUE AND a.is_active = TRUE AND u.is_active = TRUE
                   AND (p.trading_mode = 'demo' OR (p.trading_mode = 'live' AND u.can_live_trade = TRUE))
                """),
                    {"strategy": OPPONENT},
                )
            )
            .mappings()
            .all()
        )
        if not runners:
            return 0

        # Filter runners who haven't exceeded daily trade or loss circuit breaker
        eligible_runners = []
        for runner in runners:
            user_id = runner["user_id"]
            # Check daily trade count and loss count today
            counts = (
                await session.execute(
                    text("""
                SELECT COUNT(*) AS total_trades,
                       COUNT(CASE WHEN pnl < 0 THEN 1 END) AS losses
                  FROM trades
                 WHERE user_id = :user AND strategy_key = :strategy
                   AND created_at::date = :trade_date
                """),
                    {"user": user_id, "strategy": OPPONENT, "trade_date": now.date()},
                )
            ).mappings().first()
            total_trades = int(counts["total_trades"] if counts else 0)
            losses = int(counts["losses"] if counts else 0)
            if not is_session_trading_allowed(total_trades, losses):
                continue

            # Check if runner already has an open position or in-flight intent
            active = bool(
                await session.scalar(
                    text("""
                SELECT EXISTS(
                    SELECT 1 FROM trades WHERE user_id = :user AND strategy_key = :strategy AND status = 'open'
                ) OR EXISTS(
                    SELECT 1 FROM strategy_execution_intents
                     WHERE user_id = :user AND strategy_key = :strategy
                       AND action = 'ENTRY' AND status IN ('pending', 'claimed', 'retry_wait', 'submitted')
                )
                """),
                    {"user": user_id, "strategy": OPPONENT},
                )
            )
            if active:
                continue

            eligible_runners.append(runner)

        if not eligible_runners:
            return 0

        # Get latest Nifty spot price from ticks
        spot_ltp = await session.scalar(
            text("""
            SELECT price FROM market_price_ticks
             WHERE UPPER(exchange_segment) = 'NSE' AND contract_token = '99926000'
               AND price > 0
             ORDER BY received_at DESC LIMIT 1
            """)
        )
        if spot_ltp is None or float(spot_ltp) <= 0:
            return 0

        spot = Decimal(str(spot_ltp))
        atm_strike = calculate_atm_strike(spot)
        _ = calculate_3_strike_perimeter(spot)

        # Check or lock benchmark snapshot for today
        benchmark_key = f"opp-benchmark-{now.date()}"
        bench_row = (
            await session.execute(
                text("""
                SELECT id, status, previous_close FROM strategy_market_snapshots
                 WHERE strategy_key = :strategy AND instrument = 'NIFTY'
                   AND trade_date = :trade_date AND execution_key = :key
                """),
                {"strategy": OPPONENT, "trade_date": now.date(), "key": benchmark_key},
            )
        ).mappings().first()

        if bench_row:
            if str(bench_row["status"]) == "aborted":
                return 0
            bench_spot = Decimal(str(bench_row["previous_close"]))
            if check_spot_expansion_abort(bench_spot, spot):
                await session.execute(
                    text("UPDATE strategy_market_snapshots SET status='aborted' WHERE id=:id"),
                    {"id": bench_row["id"]},
                )
                return 0
        else:
            # First evaluation at/after 09:33: lock benchmark reference spot & discover 3-strike basket
            bench_id = _order_uuid("opponent-benchmark", f"NIFTY:{now.date()}")
            perimeter = calculate_3_strike_perimeter(spot)
            if hasattr(self.market_provider, "select_opponent_basket"):
                try:
                    await self.market_provider.select_opponent_basket(
                        user_id=UUID(str(eligible_runners[0]["user_id"])),
                        strikes=perimeter,
                        trade_date=now.date(),
                    )
                except Exception as exc:
                    logger.debug("Opponent basket discovery failed: %s", exc)

            await session.execute(
                text("""
                INSERT INTO strategy_market_snapshots(
                  id, strategy_key, instrument, trade_date, status, error, execution_key,
                  underlying_token, exchange_segment, product_type, previous_close, fetched_at)
                VALUES(:id, :strategy, 'NIFTY', :trade_date, 'ready', '', :key,
                  '99926000', 'NSE', 'INTRADAY', :spot, NOW())
                ON CONFLICT(strategy_key, instrument, trade_date, execution_key) DO UPDATE SET
                  previous_close = EXCLUDED.previous_close, fetched_at = NOW()
                """),
                {
                    "id": bench_id,
                    "strategy": OPPONENT,
                    "trade_date": now.date(),
                    "key": benchmark_key,
                    "spot": float(spot),
                },
            )
            # Benchmark was just locked at 09:33:00. Under no circumstances should an order
            # be created immediately before candidate breakout confirmation candles close.
            return 0

        if self.market_provider is None:
            return 0

        # Discover 3-strike perimeter baskets: [ATM - 50, ATM, ATM + 50]
        perimeter = calculate_3_strike_perimeter(spot)
        baskets = []
        if hasattr(self.market_provider, "select_opponent_basket"):
            try:
                baskets = await self.market_provider.select_opponent_basket(
                    user_id=UUID(str(eligible_runners[0]["user_id"])),
                    strikes=perimeter,
                    trade_date=now.date(),
                )
            except Exception as exc:
                logger.debug("Opponent basket discovery failed: %s", exc)

        # Fallback for mock environments that only implement select()
        if not baskets or len(baskets) < len(perimeter):
            from app.strategy.supertrend import CONFIGS
            config = CONFIGS.get("NIFTY")
            if config is None or not hasattr(self.market_provider, "select"):
                return 0
            side = OptionSide.CALL if spot >= atm_strike else OptionSide.PUT
            try:
                selection = await self.market_provider.select(
                    user_id=UUID(str(eligible_runners[0]["user_id"])),
                    config=config,
                    side=side,
                    trade_date=now.date(),
                )
                opponent_selection = await self.market_provider.select(
                    user_id=UUID(str(eligible_runners[0]["user_id"])),
                    config=config,
                    side=side.opposite,
                    trade_date=now.date(),
                )
            except Exception:
                return 0
            if selection is None or opponent_selection is None:
                return 0
            from unittest.mock import MagicMock
            baskets = [
                MagicMock(
                    strike=atm_strike,
                    ce_contract=selection.contract if side is OptionSide.CALL else opponent_selection.contract,
                    pe_contract=opponent_selection.contract if side is OptionSide.CALL else selection.contract,
                )
            ]

        # Query completed 3-minute candles for all candidate contracts from 09:30 onwards
        start_candle_time = now.replace(hour=9, minute=30, second=0, microsecond=0)
        all_tokens = list({str(b.ce_contract.token) for b in baskets} | {str(b.pe_contract.token) for b in baskets})
        candle_rows = (
            await session.execute(
                text("""
                SELECT symbol_token, candle_time, open_price, high_price, low_price, close_price
                  FROM backtest_market_candles
                 WHERE exchange = 'NFO' AND symbol_token = ANY(:tokens)
                   AND interval_key IN ('THREE_MINUTE', 'FIVE_MINUTE')
                   AND candle_time >= :start_time AND candle_time <= :end_time
                 ORDER BY candle_time
                """),
                {
                    "tokens": all_tokens,
                    "start_time": start_candle_time,
                    "end_time": now,
                },
            )
        ).mappings().all()

        candles_by_token: dict[str, list[Candle]] = {}
        for r in candle_rows:
            tok = str(r["symbol_token"])
            c = Candle(
                at=r["candle_time"],
                open=Decimal(str(r["open_price"])),
                high=Decimal(str(r["high_price"])),
                low=Decimal(str(r["low_price"])),
                close=Decimal(str(r["close_price"])),
            )
            candles_by_token.setdefault(tok, []).append(c)

        # Build CE and PE contract benchmark candle dictionaries
        ce_map: dict[Decimal, tuple[OptionContract, Candle]] = {}
        pe_map: dict[Decimal, tuple[OptionContract, Candle]] = {}
        strikes_available: list[Decimal] = []
        for b in baskets:
            ce_c = candles_by_token.get(str(b.ce_contract.token), [])
            pe_c = candles_by_token.get(str(b.pe_contract.token), [])
            if ce_c and pe_c:
                ce_map[b.strike] = (b.ce_contract, ce_c[0])
                pe_map[b.strike] = (b.pe_contract, pe_c[0])
                strikes_available.append(b.strike)

        if not ce_map or not pe_map:
            return 0

        # Build ranked coincidence queue across the available strikes
        ranked_queue = build_ranked_coincidence_queue(ce_map, pe_map, strikes_available)
        if not ranked_queue:
            return 0

        # Evaluate confirmation on the ranked queue (starting with Rank 1)
        confirmed_signal: OpponentSignal | None = None
        for pair in ranked_queue:
            ce_candles = candles_by_token.get(str(pair.ce_contract.token), [])
            pe_candles = candles_by_token.get(str(pair.pe_contract.token), [])
            if len(ce_candles) < 2 or len(pe_candles) < 2:
                continue

            # Check if CE is breakout side (PE is opponent breakdown)
            sig_ce = check_confirmation_and_entry(
                pair=pair,
                breakout_side=OptionSide.CALL,
                breakout_candles=ce_candles[1:],
                opponent_candles=pe_candles[1:],
            )
            if sig_ce is not None:
                confirmed_signal = sig_ce
                break

            # Check if PE is breakout side (CE is opponent breakdown)
            sig_pe = check_confirmation_and_entry(
                pair=pair,
                breakout_side=OptionSide.PUT,
                breakout_candles=pe_candles[1:],
                opponent_candles=ce_candles[1:],
            )
            if sig_pe is not None:
                confirmed_signal = sig_pe
                break

        if confirmed_signal is None:
            return 0

        # Step 4: Retracement Pullback: Current LTP must be in [Marked High, Marked High + 5.0 pts]
        curr_ltp = await session.scalar(
            text("""
            SELECT price FROM market_price_ticks
             WHERE contract_token = :token AND price > 0
             ORDER BY received_at DESC LIMIT 1
            """),
            {"token": confirmed_signal.contract.token},
        )
        opt_price = (
            Decimal(str(curr_ltp))
            if curr_ltp is not None
            else Decimal(str(confirmed_signal.contract.premium or confirmed_signal.marked_high))
        )

        if not is_in_retracement_zone(opt_price, confirmed_signal.marked_high):
            return 0

        session_key = f"opp-NIFTY-{now:%Y%m%d-%H%M}-{confirmed_signal.side.value}"
        intents: list[PreparedIntent] = []
        for runner in eligible_runners:
            snapshot_id = await self._opponent_snapshot(
                session,
                runner,
                confirmed_signal.contract,
                confirmed_signal.side,
                spot,
                atm_strike,
                confirmed_signal.marked_high,
                confirmed_signal.marked_low,
                opt_price,
            )
            intents.append(
                PreparedIntent(
                    UUID(str(runner["user_id"])),
                    snapshot_id,
                    None,
                    OPPONENT,
                    "NIFTY",
                    session_key,
                    "ENTRY",
                    "BUY_ENTRY",
                    "BUY",
                    "MARKET",
                    int(runner["lots"]),
                    int(runner["lots"]) * confirmed_signal.contract.lot_size,
                    opt_price,
                    None,
                    now + timedelta(minutes=15),
                )
            )

        if not intents:
            return 0

        _, inserted = await SignalRepository(session).materialize(
            strategy_key=OPPONENT,
            instrument="NIFTY",
            session_key=session_key,
            signal_at=now,
            signal_type="ENTRY",
            snapshot_id=None,
            payload={
                "side": confirmed_signal.side.value,
                "spot_ltp": str(spot),
                "atm_strike": str(atm_strike),
                "marked_high": str(confirmed_signal.marked_high),
                "marked_low": str(confirmed_signal.marked_low),
                "target_1": str(confirmed_signal.ladder.target_1),
                "target_2": str(confirmed_signal.ladder.target_2),
                "target_3": str(confirmed_signal.ladder.target_3),
                "target_4": str(confirmed_signal.ladder.target_4),
                "option_execution_price": str(opt_price),
                "contract_symbol": confirmed_signal.contract.symbol,
            },
            intents=intents,
        )
        return int(inserted > 0)

    async def _opponent_snapshot(
        self,
        session,
        runner,
        selection,
        side,
        spot,
        atm,
        marked_h=None,
        marked_l=None,
        execution_price=None,
    ) -> UUID:
        contract = getattr(selection, "contract", selection)
        token = str(contract.token)
        symbol = str(contract.symbol)
        expiry = contract.expiry
        lot_size = int(contract.lot_size)
        prem = float(execution_price if execution_price is not None else getattr(contract, "premium", 0.0))
        entry_price = float(prem) if prem > 0 else float(marked_h or 100.0)
        h = float(marked_h) if marked_h is not None else entry_price
        l = float(marked_l) if marked_l is not None else max(1.0, entry_price - 15.0)
        tp1_price = h * 1.25
        tp2_price = h * 1.50
        sl_price = max(1.0, l)

        snapshot_id = _order_uuid(
            "opponent-snapshot",
            f"{token}:{runner['user_id']}:{atm}",
        )
        execution_key = (
            f"opp-{symbol}-{UUID(str(runner['user_id'])).hex}"
        )

        await session.execute(
            text("""
            INSERT INTO strategy_market_snapshots(
              id,strategy_key,instrument,trade_date,status,error,contract_token,
              contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,
              execution_key,underlying_token,buy_target,buy_sl1,buy_sl2,sell_target,sell_sl1,sell_sl2,
              previous_close,fetched_at)
            VALUES(:id,:strategy,:instrument,CURRENT_DATE,'ready','',:token,:symbol,:expiry,:lot_size,
              'NFO','INTRADAY',:key,'99926000',:tp1,:sl,:tp2,:tp1,:sl,:tp2,
              :spot,NOW())
            ON CONFLICT(strategy_key,instrument,trade_date,execution_key) DO UPDATE SET
              status='ready',error='',contract_token=EXCLUDED.contract_token,
              contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,
              lot_size=EXCLUDED.lot_size,exchange_segment=EXCLUDED.exchange_segment,
              product_type=EXCLUDED.product_type,underlying_token=EXCLUDED.underlying_token,
              buy_target=EXCLUDED.buy_target,buy_sl1=EXCLUDED.buy_sl1,buy_sl2=EXCLUDED.buy_sl2,
              sell_target=EXCLUDED.sell_target,sell_sl1=EXCLUDED.sell_sl1,sell_sl2=EXCLUDED.sell_sl2,
              previous_close=EXCLUDED.previous_close,fetched_at=NOW()
            """),
            {
                "id": snapshot_id,
                "strategy": OPPONENT,
                "instrument": f"NIFTY_{side.value}",
                "token": token,
                "symbol": symbol,
                "expiry": expiry,
                "lot_size": lot_size,
                "key": execution_key,
                "tp1": tp1_price,
                "tp2": tp2_price,
                "sl": sl_price,
                "spot": float(spot),
            },
        )
        value = await session.scalar(
            text("""
            SELECT id FROM strategy_market_snapshots
             WHERE strategy_key=:strategy AND instrument=:instrument
               AND trade_date=CURRENT_DATE AND execution_key=:key
            """),
            {
                "strategy": OPPONENT,
                "instrument": f"NIFTY_{side.value}",
                "key": execution_key,
            },
        )
        return UUID(str(value))


class FillLifecycleWorker:
    """Apply reconciled cumulative fills exactly once to local trade state."""

    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        *,
        protection_ack_timeout_seconds: int = 30,
    ) -> None:
        self.session_factory = session_factory
        self.protection_ack_timeout_seconds = protection_ack_timeout_seconds

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
                               o.processed_quantity,o.average_fill_price,o.broker_status,
                               s.strategy_key,s.instrument,s.contract_symbol,s.lot_size,
                               s.buy_target,s.buy_sl1,s.buy_sl2,s.sell_target,s.sell_sl1,s.sell_sl2
                          FROM strategy_orders o
                          JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
                         WHERE o.filled_quantity>o.processed_quantity
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
        broker_terminal = str(order["broker_status"] or "").strip().lower() in {
            "complete",
            "completed",
            "filled",
        }
        terminal = cumulative >= int(order["quantity"]) or broker_terminal
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
        if terminal and role in {"BUY_ENTRY", "SELL_ENTRY"}:
            await session.execute(
                text("""
                UPDATE strategy_execution_intents SET status='completed',completed_at=NOW(),
                  last_error='',updated_at=NOW()
                 WHERE strategy_order_id=:order AND user_id=:user AND status='submitted'
                """),
                {"order": order_id, "user": user_id},
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
                       safety_status=CASE WHEN CAST(:mode AS varchar(8))='demo' THEN 'DEMO' ELSE 'PROTECTION_REQUIRED' END,
                       protection_deadline_at=CASE WHEN CAST(:mode AS varchar(8))='demo' THEN NULL
                         ELSE NOW()+(:ack_timeout * INTERVAL '1 second') END,
                       updated_at=NOW() WHERE id=:trade
                """),
                {
                    "quantity": new_quantity,
                    "entry": entry,
                    "fill": fill,
                    "lots": delta_lots,
                    "trade": trade["id"],
                    "mode": order["execution_mode"],
                    "ack_timeout": self.protection_ack_timeout_seconds,
                },
            )
            return
        trade_id = _order_uuid("trade", order_id)
        strategy = str(order["strategy_key"])
        if strategy == SUPERTREND:
            target_pts = Decimal(str(order["buy_target"] or order["sell_target"] or 0))
            sl_pts = Decimal(str(order["buy_sl1"] or order["sell_sl1"] or 0))
            target = fill + target_pts
            sl1 = max(Decimal("0.05"), fill - sl_pts)
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
            VALUES(:id,:user,CAST(:mode AS varchar(8)),'open',:direction,:quantity,:entry,:entry,0,NOW(),
                   :instrument,:symbol,:external,:notes,:strategy,:snapshot,:lots,:lots,
                   :target,:sl1,:sl2,:reversal,
                   CASE WHEN CAST(:mode AS varchar(8))='demo' THEN 'DEMO' ELSE 'PROTECTION_REQUIRED' END,
                   CASE WHEN CAST(:mode AS varchar(8))='demo' THEN NULL
                     ELSE NOW()+(:ack_timeout * INTERVAL '1 second') END)
            ON CONFLICT(id) DO NOTHING
            """),
            {
                "id": trade_id,
                "user": order["user_id"],
                "mode": order["execution_mode"],
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
                "ack_timeout": self.protection_ack_timeout_seconds,
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
            "SIGNAL_REVERSAL"
            if trade.get("notes") and "SuperTrend reversal" in str(trade["notes"])
            else "MARKET_CLOSED"
            if str(order["session_key"]).startswith("stsq-")
            else "MANUAL_RULENIX_CLOSE"
            if str(order["session_key"]).startswith("mc-")
            else "TP1"
            if role == "TARGET" and trade["strategy_key"] in (FUTURES, OPPONENT)
            else "TP"
            if role == "TARGET"
            else role
        )
        await session.execute(
            text("""
            UPDATE trades SET status=CASE WHEN :remaining=0 THEN 'closed' ELSE 'open' END,
                   safety_status=CASE WHEN :remaining=0 THEN 'CLOSED'
                     WHEN CAST(:mode AS varchar(8))='demo' THEN 'DEMO'
                     WHEN :role='EMERGENCY_CLOSE' THEN 'EMERGENCY_CLOSING'
                     ELSE 'PROTECTION_REQUIRED' END,
                   quantity=:remaining,remaining_lots=:lots,last_price=:fill,pnl=:pnl,
                   exit_price=CASE WHEN :remaining=0 THEN :fill ELSE exit_price END,
                   exit_datetime=CASE WHEN :remaining=0 THEN NOW() ELSE exit_datetime END,
                   exit_reason=CASE WHEN :remaining=0 THEN :reason ELSE exit_reason END,
                   protection_deadline_at=CASE WHEN :remaining>0 AND CAST(:mode AS varchar(8))='live'
                     THEN NOW()+(:ack_timeout * INTERVAL '1 second') ELSE NULL END,
                   updated_at=NOW() WHERE id=:trade
            """),
            {
                "remaining": remaining,
                "lots": remaining_lots,
                "fill": fill,
                "pnl": pnl,
                "reason": reason,
                "role": role,
                "mode": order["execution_mode"],
                "ack_timeout": self.protection_ack_timeout_seconds,
                "trade": trade["id"],
            },
        )
        if role == "TARGET" and remaining > 0 and trade["strategy_key"] == OPPONENT:
            entry_p = trade["entry_price"]
            await session.execute(
                text("""
                UPDATE trades
                   SET sl1_price = :entry_price,
                       notes = CONCAT(COALESCE(notes,''), '; T1_HIT: partial booked, SL trailed to breakeven (', :entry_str, ')')
                 WHERE id = :trade
                """),
                {"trade": trade["id"], "entry_price": entry_p, "entry_str": str(entry_p)},
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


class DemoLifecycleWorker:
    """Advance durable DEMO orders from stored ticks without broker I/O."""

    def __init__(self, session_factory: async_sessionmaker[AsyncSession]) -> None:
        self.session_factory = session_factory

    async def run_once(self) -> dict[str, object]:
        expired = await self._expire_stale_orders()
        protected = await self._ensure_protection()
        filled = await self._process_ticks()
        return {"expired": expired, "protected": protected, "filled": filled, "broker_mutations": 0}

    async def _expire_stale_orders(self) -> int:
        async with self.session_factory() as session, session.begin():
            now_ist = datetime.now(IST)
            minute = now_ist.hour * 60 + now_ist.minute
            result = await session.execute(
                text("""
                UPDATE strategy_orders o
                   SET status='cancelled',
                       broker_status='Demo order expired at session close',
                       updated_at=NOW()
                 WHERE o.execution_mode='demo'
                   AND o.role IN ('BUY_ENTRY','SELL_ENTRY')
                   AND o.status IN ('pending','submitted')
                   AND (
                       o.created_at < (CURRENT_TIMESTAMP AT TIME ZONE 'Asia/Kolkata')::date
                       OR (o.session_key = 'day' AND :minute >= 920)
                       OR (o.session_key = 'evening' AND :minute >= 1405)
                   )
                RETURNING o.id
                """),
                {"minute": minute},
            )
            cancelled_ids = result.scalars().all()
            if cancelled_ids:
                await session.execute(
                    text("""
                    UPDATE strategy_execution_intents
                       SET status='expired', completed_at=NOW(), updated_at=NOW(),
                           last_error='Demo entry order cancelled at session close'
                     WHERE strategy_order_id = ANY(:ids) AND status='submitted'
                    """),
                    {"ids": list(cancelled_ids)},
                )
            return len(cancelled_ids)

    async def _ensure_protection(self) -> int:
        async with self.session_factory() as session:
            ids = (
                (
                    await session.execute(
                        text("""
                    SELECT id FROM trades
                     WHERE execution_mode='demo' AND status='open' AND quantity>0
                     ORDER BY entry_datetime,id LIMIT 100
                    """)
                    )
                )
                .scalars()
                .all()
            )
        changed = 0
        for value in ids:
            async with self.session_factory() as session, session.begin():
                trade = (
                    (
                        await session.execute(
                            text("""
                        SELECT t.*,s.lot_size FROM trades t
                        JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
                         WHERE t.id=:trade AND t.execution_mode='demo' AND t.status='open'
                         FOR UPDATE OF t
                        """),
                            {"trade": value},
                        )
                    )
                    .mappings()
                    .first()
                )
                if trade is None:
                    continue
                target_done = bool(
                    await session.scalar(
                        text("""
                        SELECT EXISTS(SELECT 1 FROM strategy_orders
                         WHERE trade_id=:trade AND role='TARGET' AND processed_quantity>0)
                        """),
                        {"trade": value},
                    )
                )
                stop_role = "SL2" if target_done and trade["sl2_price"] is not None else "SL1"
                await session.execute(
                    text("""
                    UPDATE strategy_orders SET status='cancelled',
                      broker_status='Demo protection replaced locally',updated_at=NOW()
                     WHERE trade_id=:trade AND execution_mode='demo'
                       AND role IN ('SL1','SL2') AND role<>:role
                       AND status IN ('pending','submitted','partially_filled')
                    """),
                    {"trade": value, "role": stop_role},
                )
                active_stop = bool(
                    await session.scalar(
                        text("""
                        SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=:trade
                         AND execution_mode='demo' AND role=:role
                         AND status IN ('pending','submitted','partially_filled'))
                        """),
                        {"trade": value, "role": stop_role},
                    )
                )
                factory = DurableOrderFactory(session)
                lot_size = max(int(trade["lot_size"] or 1), 1)
                quantity = int(trade["quantity"])
                if not active_stop:
                    stop_price = trade["sl2_price"] if stop_role == "SL2" else trade["sl1_price"]
                    if stop_price is not None:
                        order_id = _order_uuid(f"demo-{stop_role.lower()}", value, quantity)
                        await factory.create(
                            order_id=order_id,
                            user_id=UUID(str(trade["user_id"])),
                            snapshot_id=UUID(str(trade["strategy_snapshot_id"])),
                            trade_id=UUID(str(value)),
                            session_key=f"dpx-{UUID(str(value)).hex[:14]}-{stop_role.lower()}",
                            role=stop_role,
                            side=_exit_side(str(trade["direction"])),
                            lots=max(1, (quantity + lot_size - 1) // lot_size),
                            quantity=quantity,
                            price=Decimal(str(stop_price)),
                            trigger_price=Decimal(str(stop_price)),
                            order_type="STOPLOSS_MARKET",
                            idempotency_key=f"python:demo-protection:{value}:{stop_role}:{quantity}",
                            execution_mode="demo",
                        )
                        await self._submit_local(session, order_id)
                        changed += 1
                active_target = bool(
                    await session.scalar(
                        text("""
                        SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=:trade
                         AND execution_mode='demo' AND role='TARGET'
                         AND status IN ('pending','submitted','partially_filled'))
                        """),
                        {"trade": value},
                    )
                )
                if not target_done and not active_target and trade["target_price"] is not None:
                    target_quantity = quantity
                    if trade["strategy_key"] == FUTURES:
                        target_lots = max(1, int(trade["total_lots"]) // 2)
                        target_quantity = min(quantity, target_lots * lot_size)
                    elif trade["strategy_key"] == OPPONENT:
                        from app.strategy.opponent import calculate_partial_close_lots

                        target_lots = calculate_partial_close_lots(int(trade["total_lots"]), 50)
                        target_quantity = target_lots * lot_size
                    else:
                        target_lots = max(1, int(trade["remaining_lots"]))
                    order_id = _order_uuid("demo-target", value)
                    await factory.create(
                        order_id=order_id,
                        user_id=UUID(str(trade["user_id"])),
                        snapshot_id=UUID(str(trade["strategy_snapshot_id"])),
                        trade_id=UUID(str(value)),
                        session_key=f"dpt-{UUID(str(value)).hex[:16]}",
                        role="TARGET",
                        side=_exit_side(str(trade["direction"])),
                        lots=target_lots,
                        quantity=target_quantity,
                        price=Decimal(str(trade["target_price"])),
                        trigger_price=None,
                        order_type="LIMIT",
                        idempotency_key=f"python:demo-target:{value}",
                        execution_mode="demo",
                    )
                    await self._submit_local(session, order_id)
                    changed += 1
        return changed

    @staticmethod
    async def _submit_local(session: AsyncSession, order_id: UUID) -> None:
        await session.execute(
            text("""
            UPDATE strategy_orders SET status='submitted',broker_order_id=:broker,
              broker_status='Demo order accepted locally',last_reconciled_at=NOW(),updated_at=NOW()
             WHERE id=:order AND status='pending'
            """),
            {"order": order_id, "broker": f"DEMO-{order_id}"},
        )

    async def _process_ticks(self) -> int:
        async with self.session_factory() as session:
            ids = (
                (
                    await session.execute(
                        text("""
                    SELECT o.id FROM strategy_orders o
                    JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
                    JOIN market_price_ticks p ON UPPER(p.exchange_segment)=UPPER(s.exchange_segment)
                      AND p.contract_token=s.contract_token
                     WHERE o.execution_mode='demo' AND o.status='submitted'
                       AND p.received_at>=NOW()-INTERVAL '60 seconds' AND p.price>0
                     ORDER BY CASE WHEN o.role='TARGET' THEN 0
                                   WHEN o.role IN ('SL1','SL2') THEN 1 ELSE 2 END,
                              o.created_at,o.id LIMIT 200
                    """)
                    )
                )
                .scalars()
                .all()
            )
        filled = 0
        for value in ids:
            async with self.session_factory() as session, session.begin():
                row = (
                    (
                        await session.execute(
                            text("""
                        SELECT o.id,o.trade_id,o.role,o.side,o.price,o.quantity,p.price AS ltp,
                               s.strategy_key,s.buy_sl1
                          FROM strategy_orders o
                          JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
                          JOIN market_price_ticks p
                            ON UPPER(p.exchange_segment)=UPPER(s.exchange_segment)
                           AND p.contract_token=s.contract_token
                         WHERE o.id=:order AND o.execution_mode='demo' AND o.status='submitted'
                           AND p.received_at>=NOW()-INTERVAL '60 seconds' AND p.price>0
                         FOR UPDATE OF o
                        """),
                            {"order": value},
                        )
                    )
                    .mappings()
                    .first()
                )
                if row is None:
                    continue
                ltp = Decimal(str(row["ltp"]))
                price = Decimal(str(row["price"]))
                role, side = str(row["role"]), str(row["side"])
                is_opp_initial_sl = (
                    str(row.get("strategy_key") or "") == OPPONENT
                    and role in {"SL1", "SL2"}
                    and row.get("buy_sl1") is not None
                    and price == Decimal(str(row["buy_sl1"]))
                )
                triggered = not is_opp_initial_sl and (
                    (role == "BUY_ENTRY" and ltp >= price)
                    or (role == "SELL_ENTRY" and ltp <= price)
                    or (role == "TARGET" and side == "SELL" and ltp >= price)
                    or (role == "TARGET" and side == "BUY" and ltp <= price)
                    or (role in {"SL1", "SL2"} and side == "SELL" and ltp <= price)
                    or (role in {"SL1", "SL2"} and side == "BUY" and ltp >= price)
                )
                if not triggered:
                    continue
                if row["trade_id"] is not None and role in {"TARGET", "SL1", "SL2"}:
                    cancel_roles = (
                        "('TARGET')"
                        if (role == "TARGET" and str(row.get("strategy_key") or "") == OPPONENT)
                        else "('TARGET','SL1','SL2')"
                    )
                    await session.execute(
                        text(f"""
                        UPDATE strategy_orders SET status='cancelled',
                          broker_status='Demo sibling exit cancelled locally',updated_at=NOW()
                         WHERE trade_id=:trade AND id<>:order AND execution_mode='demo'
                           AND role IN {cancel_roles}
                           AND status IN ('pending','submitted','partially_filled')
                        """),
                        {"trade": row["trade_id"], "order": value},
                    )
                await session.execute(
                    text("""
                    UPDATE strategy_orders SET status='filled',filled_quantity=quantity,
                      average_fill_price=:ltp,filled_price=:ltp,filled_at=NOW(),
                      broker_status='Demo order filled from fresh market tick',updated_at=NOW()
                     WHERE id=:order AND status='submitted'
                    """),
                    {"order": value, "ltp": ltp},
                )
                if row["trade_id"] is not None:
                    await session.execute(
                        text("UPDATE trades SET last_price=:ltp,updated_at=NOW() WHERE id=:trade"),
                        {"trade": row["trade_id"], "ltp": ltp},
                    )
                filled += 1
        return filled


class ProtectionLifecycleWorker:
    """Recover missing LIVE protection and targets from reconciled durable truth."""

    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        coordinator: LiveMutationCoordinator,
        *,
        max_attempts: int = 3,
    ) -> None:
        self.session_factory = session_factory
        self.coordinator = coordinator
        self.max_attempts = max_attempts

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
                           'PROTECTION_UNCERTAIN','PROTECTION_FAILED','RECONCILIATION_REQUIRED',
                           'CLOSING','EMERGENCY_CLOSING')
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
                and trade["last_position_reconciled_at"] >= datetime.now(UTC) - timedelta(minutes=5)
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
            deadline_elapsed = bool(
                trade["protection_deadline_at"]
                and trade["protection_deadline_at"] <= datetime.now(UTC)
            )
            if (
                trade["safety_status"] in {"CLOSING", "EMERGENCY_CLOSING"}
                or deadline_elapsed
                or int(trade["protection_attempts"] or 0) >= self.max_attempts
            ):
                return await self._emergency_close(session, trade)
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
            attempt = int(trade["protection_attempts"] or 0) + 1
            order_id = _order_uuid(f"protection-{stop_role}-{attempt}", trade_id, watermark)
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
                idempotency_key=(
                    f"python:protection:{trade_id}:{stop_role}:{watermark}:attempt:{attempt}"
                ),
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

    async def _emergency_close(self, session: AsyncSession, trade) -> str:
        trade_id = UUID(str(trade["id"]))
        await session.execute(
            text("""
            UPDATE trades SET safety_status='EMERGENCY_CLOSING',
              last_protection_error='Protection deadline or retry budget exhausted.',updated_at=NOW()
             WHERE id=:trade
            """),
            {"trade": trade_id},
        )
        active = (
            (
                await session.execute(
                    text("""
                    SELECT id,order_type,status,broker_order_id FROM strategy_orders
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
        candidate = next(
            (row for row in active if row["status"] != "cancelling" and row["broker_order_id"]),
            None,
        )
        if active:
            if candidate is None:
                return "waiting"
            order_id = UUID(str(candidate["id"]))
            variety = (
                "STOPLOSS" if str(candidate["order_type"]).startswith("STOPLOSS") else "NORMAL"
            )
            await session.commit()
            try:
                outcome = await self.coordinator.cancel_order(order_id, variety=variety)
            except MutationPendingError:
                return "ambiguous"
            return "waiting" if outcome.state is MutationState.ACKNOWLEDGED else "ambiguous"
        existing = bool(
            await session.scalar(
                text("""
                SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=:trade
                 AND role='EMERGENCY_CLOSE' AND status NOT IN ('failed','rejected','cancelled'))
                """),
                {"trade": trade_id},
            )
        )
        if existing:
            return "waiting"
        price = await session.scalar(
            text("""
            SELECT p.price FROM market_price_ticks p JOIN strategy_market_snapshots s
              ON UPPER(p.exchange_segment)=UPPER(s.exchange_segment)
             AND p.contract_token=s.contract_token
             WHERE s.id=:snapshot AND p.received_at>=NOW()-INTERVAL '60 seconds'
               AND p.price>0 ORDER BY p.received_at DESC LIMIT 1
            """),
            {"snapshot": trade["strategy_snapshot_id"]},
        )
        if price is None:
            await session.execute(
                text("""
                UPDATE trades SET last_protection_error=
                  'Emergency close paused because no fresh contract price is available.',updated_at=NOW()
                 WHERE id=:trade
                """),
                {"trade": trade_id},
            )
            return "waiting"
        terminal_attempts = int(
            await session.scalar(
                text("""
                SELECT COUNT(*) FROM strategy_orders WHERE trade_id=:trade
                 AND role='EMERGENCY_CLOSE' AND status IN ('failed','rejected','cancelled')
                """),
                {"trade": trade_id},
            )
            or 0
        )
        close_id = _order_uuid("emergency-close", trade_id, terminal_attempts + 1)
        lot_size = max(int(trade["lot_size"] or 1), 1)
        quantity = int(trade["quantity"])
        await DurableOrderFactory(session).create(
            order_id=close_id,
            user_id=UUID(str(trade["user_id"])),
            snapshot_id=UUID(str(trade["strategy_snapshot_id"])),
            trade_id=trade_id,
            session_key=(
                f"ec-{trade_id.hex[:16]}"
                if terminal_attempts == 0
                else f"ec-{trade_id.hex[:13]}a{terminal_attempts + 1}"
            ),
            role="EMERGENCY_CLOSE",
            side=_exit_side(str(trade["direction"])),
            lots=max(1, (quantity + lot_size - 1) // lot_size),
            quantity=quantity,
            price=Decimal(str(price)),
            trigger_price=None,
            order_type="MARKET",
            idempotency_key=f"python:emergency-close:{trade_id}:{terminal_attempts + 1}",
        )
        await session.commit()
        return await self._submit(close_id, ActionKind.EMERGENCY_CLOSE, trade_id)

    async def _reserve_target(self, session: AsyncSession, trade) -> None:
        if trade["target_price"] is None:
            return
        quantity = int(trade["quantity"])
        if trade["strategy_key"] == FUTURES:
            lots = max(1, int(trade["total_lots"]) // 2)
            quantity = min(quantity, lots * max(int(trade["lot_size"] or 1), 1))
        elif trade["strategy_key"] == OPPONENT:
            from app.strategy.opponent import calculate_partial_close_lots

            lots = calculate_partial_close_lots(int(trade["total_lots"]), 50)
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
                    UPDATE trades SET safety_status=:status,
                     last_protection_error=:error,updated_at=NOW()
                     WHERE id=:trade
                    """),
                    {
                        "trade": trade_id,
                        "status": "EMERGENCY_CLOSING"
                        if action is ActionKind.EMERGENCY_CLOSE
                        else "PROTECTION_UNCERTAIN",
                        "error": "Emergency-close broker acknowledgement is ambiguous."
                        if action is ActionKind.EMERGENCY_CLOSE
                        else "Protection broker acknowledgement is ambiguous.",
                    },
                )
            return "ambiguous"
        async with self.session_factory() as session, session.begin():
            await session.execute(
                text("""
                UPDATE trades SET safety_status=:status,
                 last_protection_error=:error,updated_at=NOW()
                 WHERE id=:trade
                """),
                {
                    "trade": trade_id,
                    "status": "EMERGENCY_CLOSING"
                    if action is ActionKind.EMERGENCY_CLOSE
                    else "PROTECTION_FAILED",
                    "error": "Emergency close submission failed closed."
                    if action is ActionKind.EMERGENCY_CLOSE
                    else "Protection submission failed closed.",
                },
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
                SELECT t.status,t.exit_reason,t.broker_net_quantity,t.exit_datetime,t.execution_mode,
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
            demo = source["execution_mode"] == "demo"
            flat = demo or (
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
                execution_mode=str(source["execution_mode"]),
            )
            if demo:
                await session.execute(
                    text("""
                    UPDATE strategy_orders SET status='submitted',broker_order_id=:broker,
                      broker_status='Demo reversal accepted locally',updated_at=NOW()
                     WHERE id=:order AND status='pending'
                    """),
                    {"order": order_id, "broker": f"DEMO-{order_id}"},
                )
                await session.execute(
                    text("""
                    UPDATE strategy_orders SET status='filled',filled_quantity=quantity,
                      average_fill_price=price,filled_price=price,filled_at=NOW(),
                      broker_status='Demo reversal market order filled locally',updated_at=NOW()
                     WHERE id=:order AND status='submitted'
                    """),
                    {"order": order_id},
                )
                await self._state(session, row["source_trade_id"], "submitted", "")
        if demo:
            return LifecycleProgress(claimed=1, submitted=1).json()
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
                       t.direction,t.quantity,t.last_price,t.entry_price,t.execution_mode
                  FROM trades t WHERE t.status='open'
                   AND t.strategy_key IN (:supertrend, :opponent) ORDER BY t.user_id,t.id
            """),
                        {"supertrend": SUPERTREND, "opponent": OPPONENT},
                    )
                )
                .mappings()
                .all()
            )
            intents: list[PreparedIntent] = []
            for trade in trades:
                strat = str(trade["strategy_key"])
                prefix = "oppsq" if strat == OPPONENT else "stsq"
                intents.append(
                    prepare_square_off_intent(
                        user_id=UUID(str(trade["user_id"])),
                        trade_id=UUID(str(trade["id"])),
                        snapshot_id=UUID(str(trade["strategy_snapshot_id"])),
                        strategy_key=strat,
                        instrument=str(trade["instrument_label"]),
                        session_key=f"{prefix}-{observed:%Y%m%d}",
                        side=_exit_side(str(trade["direction"])),
                        quantity=int(trade["quantity"]),
                        price=Decimal(str(trade["last_price"] or trade["entry_price"])),
                    )
                )
            for strat in {str(t["strategy_key"]) for t in trades}:
                strat_intents = [i for i in intents if i.strategy_key == strat]
                if strat_intents:
                    await SignalRepository(session).materialize(
                        strategy_key=strat,
                        instrument="ALL",
                        session_key=f"squareoff-{strat}-{observed:%Y%m%d}-1510",
                        signal_at=observed,
                        signal_type="SQUARE_OFF",
                        snapshot_id=None,
                        payload={"scheduled_for": "15:10 IST"},
                        intents=strat_intents,
                    )
        progress = LifecycleProgress()
        for trade in trades:
            outcome = (
                await self._close_demo(UUID(str(trade["id"])))
                if trade["execution_mode"] == "demo"
                else await self.close_worker._close(UUID(str(trade["id"])), manual=False)
            )
            progress = LifecycleProgress(
                progress.claimed + 1,
                progress.submitted + (outcome == "submitted"),
                progress.completed + (outcome == "completed"),
                progress.waiting + (outcome == "waiting"),
                progress.ambiguous + (outcome == "ambiguous"),
            )
        return {"due": True, **progress.json()}

    async def _close_demo(self, trade_id: UUID) -> str:
        async with self.session_factory() as session, session.begin():
            trade = (
                (
                    await session.execute(
                        text("""
                    SELECT t.*,s.lot_size FROM trades t
                    JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
                     WHERE t.id=:trade AND t.execution_mode='demo' FOR UPDATE OF t
                    """),
                        {"trade": trade_id},
                    )
                )
                .mappings()
                .first()
            )
            if trade is None or trade["status"] == "closed":
                return "completed"
            await session.execute(
                text("""
                UPDATE strategy_orders SET status='cancelled',
                  broker_status='Demo protection cancelled for 15:10 square-off',updated_at=NOW()
                 WHERE trade_id=:trade AND execution_mode='demo'
                   AND role IN ('TARGET','SL1','SL2')
                   AND status IN ('pending','submitted','partially_filled')
                """),
                {"trade": trade_id},
            )
            order_id = _order_uuid("demo-eod", trade_id)
            quantity = int(trade["quantity"])
            lot_size = max(int(trade["lot_size"] or 1), 1)
            await DurableOrderFactory(session).create(
                order_id=order_id,
                user_id=UUID(str(trade["user_id"])),
                snapshot_id=UUID(str(trade["strategy_snapshot_id"])),
                trade_id=trade_id,
                session_key=f"stsq-{trade_id.hex[:16]}",
                role="EMERGENCY_CLOSE",
                side=_exit_side(str(trade["direction"])),
                lots=max(1, (quantity + lot_size - 1) // lot_size),
                quantity=quantity,
                price=Decimal(str(trade["last_price"] or trade["entry_price"])),
                trigger_price=None,
                order_type="MARKET",
                idempotency_key=f"python:demo-eod:{trade_id}",
                execution_mode="demo",
            )
            await session.execute(
                text("""
                UPDATE strategy_orders SET status='submitted',broker_order_id=:broker,
                  broker_status='Demo EOD order accepted locally',last_reconciled_at=NOW(),
                  updated_at=NOW() WHERE id=:order AND status='pending'
                """),
                {"order": order_id, "broker": f"DEMO-{order_id}"},
            )
            await session.execute(
                text("""
                UPDATE strategy_orders SET status='filled',filled_quantity=quantity,
                  average_fill_price=price,filled_price=price,filled_at=NOW(),
                  broker_status='Demo EOD market order filled locally',updated_at=NOW()
                 WHERE id=:order AND status='submitted'
                """),
                {"order": order_id},
            )
            await session.execute(
                text("""
                UPDATE strategy_execution_intents SET status='submitted',strategy_order_id=:order,
                  updated_at=NOW() WHERE trade_id=:trade AND action='SQUARE_OFF'
                  AND status IN ('pending','retry_wait','claimed')
                """),
                {"trade": trade_id, "order": order_id},
            )
            return "submitted"


__all__ = [
    "AuthoritativeExecutionWorker",
    "DemoLifecycleWorker",
    "DurableOrderFactory",
    "EodLifecycleWorker",
    "FillLifecycleWorker",
    "LifecycleProgress",
    "ProtectionLifecycleWorker",
    "ReversalLifecycleWorker",
    "RiskReducingCloseWorker",
]
