"""Continuous market-data ingestion for Futures Breakout snapshots and SuperTrend 5-minute candles."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Callable, Sequence
from datetime import UTC, date, datetime, timedelta
from decimal import Decimal
from typing import Any
from uuid import UUID, uuid4

import httpx
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from app.broker.angel.client import AngelClient
from app.config import Settings
from app.strategy.common import IST
from app.strategy.futures_breakout import calculate_levels
from app.strategy.supertrend import parse_expiry

logger = logging.getLogger(__name__)

MASTER_URL = "https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json"
FUTURES_INSTRUMENTS = ("GOLDTEN", "GOLDM", "SILVERM", "SILVERMIC", "NATGASMINI")
INDEX_INSTRUMENTS = {
    "NIFTY": ("NSE", "99926000", "NIFTY"),
    "SENSEX": ("BSE", "99919000", "SENSEX"),
}


def weekdays_until(start: date, end: date) -> int:
    count = 0
    current = start
    while current < end:
        current += timedelta(days=1)
        if current.weekday() < 5:
            count += 1
    return count


def select_futures_contract(
    contracts: Sequence[dict[str, Any]],
    instrument: str,
    trade_date: date,
) -> dict[str, Any] | None:
    candidates: list[tuple[date, dict[str, Any]]] = []
    for item in contracts:
        if (
            str(item.get("exch_seg") or "") == "MCX"
            and str(item.get("name") or "").upper() == instrument.upper()
            and str(item.get("instrumenttype") or "") == "FUTCOM"
        ):
            try:
                expiry = parse_expiry(str(item.get("expiry") or ""))
            except Exception as exc:
                logger.debug("Failed parsing expiry for contract %s: %s", item.get("symbol"), exc)
                continue
            if expiry >= trade_date and weekdays_until(trade_date, expiry) >= 10:
                candidates.append((expiry, item))

    if not candidates:
        for item in contracts:
            if (
                str(item.get("exch_seg") or "") == "MCX"
                and str(item.get("name") or "").upper() == instrument.upper()
                and str(item.get("instrumenttype") or "") == "FUTCOM"
            ):
                try:
                    expiry = parse_expiry(str(item.get("expiry") or ""))
                except Exception as exc:
                    logger.debug("Failed parsing expiry for fallback contract %s: %s", item.get("symbol"), exc)
                    continue
                if expiry >= trade_date:
                    candidates.append((expiry, item))

    if not candidates:
        return None
    candidates.sort(key=lambda pair: pair[0])
    return candidates[0][1]


class MarketDataIngestionService:
    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        client_factory: Callable[[UUID], Any],
        settings: Settings,
        *,
        master_url: str = MASTER_URL,
        http_client: httpx.AsyncClient | None = None,
    ) -> None:
        self.session_factory = session_factory
        self.client_factory = client_factory
        self.settings = settings
        self.master_url = master_url
        self.http_client = http_client
        self._cache_date: date | None = None
        self._contracts: tuple[dict[str, Any], ...] = ()
        self._master_lock = asyncio.Lock()

    async def get_master_contracts(self, *, refresh: bool = False) -> tuple[dict[str, Any], ...]:
        today = datetime.now(IST).date()
        async with self._master_lock:
            if not refresh and self._cache_date == today and self._contracts:
                return self._contracts
            owns_client = self.http_client is None
            client = self.http_client or httpx.AsyncClient(timeout=120)
            try:
                response = await client.get(self.master_url)
                response.raise_for_status()
                payload = response.json()
            finally:
                if owns_client:
                    await client.aclose()
            if not isinstance(payload, list) or not payload:
                raise RuntimeError("Angel contract master is empty or malformed.")
            contracts = tuple(item for item in payload if isinstance(item, dict))
            if not contracts:
                raise RuntimeError("Angel contract master contains no valid contracts.")
            self._cache_date = today
            self._contracts = contracts
            return contracts

    async def get_active_broker_client(self) -> tuple[UUID, AngelClient] | None:
        async with self.session_factory() as session:
            row = (
                await session.execute(
                    text("""
                    SELECT p.user_id
                      FROM user_profiles p
                      JOIN users u ON u.id = p.user_id
                     WHERE u.is_active = TRUE
                       AND p.token_state = 'connected'
                       AND p.last_token_status IN ('success', 'refreshed')
                       AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id = p.user_id AND s.secret_kind = 'api_key')
                       AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id = p.user_id AND s.secret_kind = 'jwt_token')
                     ORDER BY CASE WHEN EXISTS (SELECT 1 FROM user_strategy_activations a WHERE a.user_id = p.user_id AND a.is_active = TRUE) THEN 0 ELSE 1 END,
                              p.token_received_at DESC NULLS LAST
                     LIMIT 1
                    """)
                )
            ).first()
            if not row:
                return None
            user_id = row[0]
        client: AngelClient = await self.client_factory(user_id)
        return user_id, client

    async def sync_supertrend_candles(
        self,
        until: datetime | None = None,
        days: int = 5,
    ) -> int:
        active = await self.get_active_broker_client()
        if not active:
            logger.warning("No active connected broker session found for SuperTrend candle ingestion.")
            return 0
        _, client = active

        now = until or datetime.now(UTC)
        from_dt = now - timedelta(days=days)
        from_str = from_dt.astimezone(IST).strftime("%Y-%m-%d %H:%M")
        to_str = now.astimezone(IST).strftime("%Y-%m-%d %H:%M")

        total_inserted = 0
        for instrument_name, (exchange, token, trading_symbol) in INDEX_INSTRUMENTS.items():
            try:
                raw_candles = await client.rest.candles(exchange, token, "FIVE_MINUTE", from_str, to_str)
            except Exception as exc:
                logger.warning("Failed fetching 5-min candles for %s: %s", instrument_name, exc)
                continue

            if not isinstance(raw_candles, list):
                continue

            async with self.session_factory() as session:
                for item in raw_candles:
                    if not isinstance(item, list) or len(item) < 5:
                        continue
                    try:
                        time_str = str(item[0])
                        candle_dt = datetime.fromisoformat(time_str).astimezone(UTC)
                        open_p = float(item[1])
                        high_p = float(item[2])
                        low_p = float(item[3])
                        close_p = float(item[4])
                        vol = float(item[5]) if len(item) > 5 else 0.0
                    except (ValueError, TypeError):
                        continue

                    await session.execute(
                        text("""
                        INSERT INTO backtest_market_candles (
                            id, exchange, instrument, symbol_token, trading_symbol,
                            interval_key, candle_time, open_price, high_price, low_price,
                            close_price, volume, source, fetched_at
                        ) VALUES (
                            :id, :exchange, :instrument, :token, :trading_symbol,
                            'FIVE_MINUTE', :candle_time, :open_price, :high_price, :low_price,
                            :close_price, :volume, 'angel_one', NOW()
                        ) ON CONFLICT (exchange, symbol_token, interval_key, candle_time) DO UPDATE SET
                            open_price = EXCLUDED.open_price,
                            high_price = EXCLUDED.high_price,
                            low_price = EXCLUDED.low_price,
                            close_price = EXCLUDED.close_price,
                            volume = EXCLUDED.volume,
                            fetched_at = NOW()
                        """),
                        {
                            "id": uuid4(),
                            "exchange": exchange,
                            "instrument": instrument_name,
                            "token": token,
                            "trading_symbol": trading_symbol,
                            "candle_time": candle_dt,
                            "open_price": open_p,
                            "high_price": high_p,
                            "low_price": low_p,
                            "close_price": close_p,
                            "volume": vol,
                        },
                    )
                    total_inserted += 1
                await session.commit()
        return total_inserted

    async def sync_futures_snapshots(self, target_date: date | None = None) -> int:
        trade_date = target_date or datetime.now(IST).date()
        active = await self.get_active_broker_client()
        if not active:
            logger.warning("No active connected broker session found for Futures snapshot sync.")
            return 0
        _, client = active

        contracts = await self.get_master_contracts()
        from_str = (trade_date - timedelta(days=25)).strftime("%Y-%m-%d 00:00")
        to_str = (trade_date - timedelta(days=1)).strftime("%Y-%m-%d 23:59")

        synced_count = 0
        for instrument in FUTURES_INSTRUMENTS:
            contract = select_futures_contract(contracts, instrument, trade_date)
            if not contract:
                logger.warning("No contract available for %s on %s", instrument, trade_date)
                continue

            token = str(contract.get("token") or "")
            symbol = str(contract.get("symbol") or "")
            try:
                expiry = parse_expiry(str(contract.get("expiry") or ""))
                lot_size = int(float(str(contract.get("lotsize") or "1")))
            except Exception as exc:
                logger.warning("Invalid contract fields for %s: %s", instrument, exc)
                continue

            try:
                raw_candles = await client.rest.candles("MCX", token, "ONE_DAY", from_str, to_str)
            except Exception as exc:
                logger.warning("Failed fetching daily candles for %s: %s", instrument, exc)
                continue

            daily_candles: list[tuple[date, float, float, float]] = []
            if isinstance(raw_candles, list):
                for row in raw_candles:
                    if not isinstance(row, list) or len(row) < 5:
                        continue
                    try:
                        c_date = datetime.fromisoformat(str(row[0])).date()
                        if c_date < trade_date:
                            daily_candles.append(
                                (c_date, float(row[2]), float(row[3]), float(row[4]))
                            )
                    except (ValueError, TypeError):
                        continue

            daily_candles.sort(key=lambda item: item[0])
            deduped: list[tuple[date, float, float, float]] = []
            seen_dates: set[date] = set()
            for item in daily_candles:
                if item[0] not in seen_dates:
                    seen_dates.add(item[0])
                    deduped.append(item)

            if len(deduped) > 4:
                deduped = deduped[-4:]

            async with self.session_factory() as session:
                if len(deduped) == 4:
                    c_dates = [item[0] for item in deduped]
                    highs = [item[1] for item in deduped]
                    lows = [item[2] for item in deduped]
                    prev_close = deduped[-1][3]

                    levels = calculate_levels(
                        [Decimal(str(h)) for h in highs],
                        [Decimal(str(l)) for l in lows],
                    )

                    quote = None
                    try:
                        quote = await client.rest.quote("FULL", {"MCX": [token]})
                    except Exception as exc:
                        logger.debug("Could not fetch MCX quote for token %s: %s", token, exc)

                    market_open: float | None = None
                    if isinstance(quote, dict):
                        data = quote.get("data")
                        if isinstance(data, dict):
                            market_open = float(data.get("open") or data.get("opn") or 0.0) or None
                        elif isinstance(data, list) and data:
                            first_item = data[0]
                            if isinstance(first_item, dict):
                                market_open = float(first_item.get("open") or first_item.get("opn") or 0.0) or None

                    buy_entry = float(levels.buy_entry)
                    sell_entry = float(levels.sell_entry)

                    if market_open is not None and market_open > 0:
                        if market_open > buy_entry:
                            gap_dir = "BUY_MISSED"
                            plan_status = "WAITING_RANGE"
                            src = "OPENING_RANGE"
                        elif market_open < sell_entry:
                            gap_dir = "SELL_MISSED"
                            plan_status = "WAITING_RANGE"
                            src = "OPENING_RANGE"
                        else:
                            gap_dir = "NONE_MISSED"
                            plan_status = "READY"
                            src = "STANDARD"
                    else:
                        market_open = prev_close
                        gap_dir = "NONE_MISSED"
                        plan_status = "READY"
                        src = "STANDARD"

                    await session.execute(
                        text("""
                        INSERT INTO strategy_market_snapshots (
                            id, strategy_key, instrument, trade_date, status, error,
                            contract_token, contract_symbol, contract_expiry, lot_size,
                            exchange_segment, product_type, execution_key, underlying_token,
                            candle_dates, highs, lows, hh2, ll2, hh4, ll4,
                            buy_entry, buy_target, buy_sl1, buy_sl2,
                            sell_entry, sell_target, sell_sl1, sell_sl2,
                            previous_close, market_open, gap_direction, entry_direction,
                            entry_source, gap_plan_status, fetched_at
                        ) VALUES (
                            :id, 'futures_breakout_v3', :instrument, :trade_date, 'ready', '',
                            :contract_token, :contract_symbol, :contract_expiry, :lot_size,
                            'MCX', 'CARRYFORWARD', 'daily', '',
                            :candle_dates, :highs, :lows, :hh2, :ll2, :hh4, :ll4,
                            :buy_entry, :buy_target, :buy_sl1, :buy_sl2,
                            :sell_entry, :sell_target, :sell_sl1, :sell_sl2,
                            :previous_close, :market_open, :gap_direction, 'BOTH',
                            :entry_source, :gap_plan_status, NOW()
                        ) ON CONFLICT (strategy_key, instrument, trade_date, execution_key) DO UPDATE SET
                            status = 'ready',
                            error = '',
                            contract_token = EXCLUDED.contract_token,
                            contract_symbol = EXCLUDED.contract_symbol,
                            contract_expiry = EXCLUDED.contract_expiry,
                            lot_size = EXCLUDED.lot_size,
                            candle_dates = EXCLUDED.candle_dates,
                            highs = EXCLUDED.highs,
                            lows = EXCLUDED.lows,
                            hh2 = EXCLUDED.hh2,
                            ll2 = EXCLUDED.ll2,
                            hh4 = EXCLUDED.hh4,
                            ll4 = EXCLUDED.ll4,
                            buy_entry = EXCLUDED.buy_entry,
                            buy_target = EXCLUDED.buy_target,
                            buy_sl1 = EXCLUDED.buy_sl1,
                            buy_sl2 = EXCLUDED.buy_sl2,
                            sell_entry = EXCLUDED.sell_entry,
                            sell_target = EXCLUDED.sell_target,
                            sell_sl1 = EXCLUDED.sell_sl1,
                            sell_sl2 = EXCLUDED.sell_sl2,
                            previous_close = EXCLUDED.previous_close,
                            market_open = COALESCE(EXCLUDED.market_open, strategy_market_snapshots.market_open),
                            gap_direction = COALESCE(EXCLUDED.gap_direction, strategy_market_snapshots.gap_direction),
                            entry_direction = EXCLUDED.entry_direction,
                            entry_source = COALESCE(EXCLUDED.entry_source, strategy_market_snapshots.entry_source),
                            gap_plan_status = COALESCE(EXCLUDED.gap_plan_status, strategy_market_snapshots.gap_plan_status),
                            fetched_at = NOW()
                        """),
                        {
                            "id": uuid4(),
                            "instrument": instrument,
                            "trade_date": trade_date,
                            "contract_token": token,
                            "contract_symbol": symbol,
                            "contract_expiry": expiry,
                            "lot_size": lot_size,
                            "candle_dates": c_dates,
                            "highs": highs,
                            "lows": lows,
                            "hh2": float(levels.hh2),
                            "ll2": float(levels.ll2),
                            "hh4": float(levels.hh4),
                            "ll4": float(levels.ll4),
                            "buy_entry": buy_entry,
                            "buy_target": float(levels.buy_target),
                            "buy_sl1": float(levels.buy_sl1),
                            "buy_sl2": float(levels.buy_sl2),
                            "sell_entry": sell_entry,
                            "sell_target": float(levels.sell_target),
                            "sell_sl1": float(levels.sell_sl1),
                            "sell_sl2": float(levels.sell_sl2),
                            "previous_close": prev_close,
                            "market_open": market_open,
                            "gap_direction": gap_dir,
                            "entry_source": src,
                            "gap_plan_status": plan_status,
                        },
                    )
                    synced_count += 1
                else:
                    err_msg = f"Expected 4 completed trading days, received {len(deduped)}."
                    await session.execute(
                        text("""
                        INSERT INTO strategy_market_snapshots (
                            id, strategy_key, instrument, trade_date, status, error,
                            contract_token, contract_symbol, contract_expiry, lot_size,
                            exchange_segment, product_type, execution_key, underlying_token,
                            fetched_at
                        ) VALUES (
                            :id, 'futures_breakout_v3', :instrument, :trade_date, 'missing', :error,
                            :contract_token, :contract_symbol, :contract_expiry, :lot_size,
                            'MCX', 'CARRYFORWARD', 'daily', '',
                            NOW()
                        ) ON CONFLICT (strategy_key, instrument, trade_date, execution_key) DO UPDATE SET
                            status = 'missing',
                            error = EXCLUDED.error,
                            contract_token = EXCLUDED.contract_token,
                            contract_symbol = EXCLUDED.contract_symbol,
                            contract_expiry = EXCLUDED.contract_expiry,
                            lot_size = EXCLUDED.lot_size,
                            fetched_at = NOW()
                        """),
                        {
                            "id": uuid4(),
                            "instrument": instrument,
                            "trade_date": trade_date,
                            "error": err_msg,
                            "contract_token": token,
                            "contract_symbol": symbol,
                            "contract_expiry": expiry,
                            "lot_size": lot_size,
                        },
                    )
                await session.commit()

        return synced_count

    async def run_cycle(self) -> dict[str, object]:
        now_ist = datetime.now(UTC).astimezone(IST)
        today = now_ist.date()

        # Sync futures snapshots for today
        futures_synced = await self.sync_futures_snapshots(today)

        # If past 16:00 IST on a weekday, pre-populate next trading day's snapshots
        next_day = today + timedelta(days=1)
        while next_day.weekday() >= 5:
            next_day += timedelta(days=1)
        if now_ist.hour >= 16:
            await self.sync_futures_snapshots(next_day)

        # Sync 5-minute candles
        candles_synced = 0
        minute = now_ist.hour * 60 + now_ist.minute
        if now_ist.weekday() < 5 and (9 * 60 + 15 <= minute <= 15 * 60 + 35):
            candles_synced = await self.sync_supertrend_candles(days=1)
        elif now_ist.weekday() < 5:
            candles_synced = await self.sync_supertrend_candles(days=3)

        return {
            "status": "ok",
            "trade_date": str(today),
            "futures_snapshots_synced": futures_synced,
            "candles_synced": candles_synced,
        }


__all__ = [
    "FUTURES_INSTRUMENTS",
    "INDEX_INSTRUMENTS",
    "MarketDataIngestionService",
    "select_futures_contract",
    "weekdays_until",
]
