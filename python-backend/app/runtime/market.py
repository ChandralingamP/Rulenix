"""Read-only market selection used by the authoritative SuperTrend dispatcher."""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from datetime import date, datetime
from decimal import Decimal, InvalidOperation
from typing import Any, Protocol
from uuid import UUID
from zoneinfo import ZoneInfo

import httpx

from app.broker.angel.client import AngelClient
from app.strategy.supertrend import IndexOptionConfig, OptionContract, OptionSide, parse_expiry

IST = ZoneInfo("Asia/Kolkata")
MASTER_URL = "https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json"


@dataclass(frozen=True)
class SuperTrendMarketSelection:
    contract: OptionContract
    underlying_ltp: Decimal


class SuperTrendMarketProvider(Protocol):
    async def select(
        self,
        *,
        user_id: UUID,
        config: IndexOptionConfig,
        side: OptionSide,
        trade_date: date,
    ) -> SuperTrendMarketSelection: ...


def _decimal(value: object) -> Decimal | None:
    try:
        parsed = Decimal(str(value))
    except (InvalidOperation, TypeError, ValueError):
        return None
    return parsed if parsed.is_finite() and parsed > 0 else None


def quote_ltps(value: object, prices: dict[str, Decimal] | None = None) -> dict[str, Decimal]:
    """Collect token-specific LTPs from Angel's nested quote response."""
    found = prices if prices is not None else {}
    if isinstance(value, list):
        for item in value:
            quote_ltps(item, found)
    elif isinstance(value, dict):
        token = next(
            (
                str(value[key])
                for key in ("symbolToken", "symboltoken", "symbol_token", "token")
                if value.get(key) not in (None, "")
            ),
            "",
        )
        price = next(
            (
                _decimal(value[key])
                for key in (
                    "ltp",
                    "LTP",
                    "last_traded_price",
                    "lastTradedPrice",
                    "last_price",
                    "close",
                )
                if value.get(key) not in (None, "")
            ),
            None,
        )
        if token and price is not None:
            found[token] = price
        for item in value.values():
            quote_ltps(item, found)
    return found


class AngelSuperTrendMarketProvider:
    """Daily Angel master plus account-egress-bound, read-only quote selection."""

    def __init__(
        self,
        client_factory: Callable[[UUID], Awaitable[AngelClient]],
        *,
        master_url: str = MASTER_URL,
        http_client: httpx.AsyncClient | None = None,
    ) -> None:
        self.client_factory = client_factory
        self.master_url = master_url
        self.http_client = http_client
        self._cache_date: date | None = None
        self._contracts: tuple[dict[str, Any], ...] = ()
        self._lock = asyncio.Lock()

    async def _master(self, *, refresh: bool = False) -> tuple[dict[str, Any], ...]:
        today = datetime.now(IST).date()
        async with self._lock:
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

    @staticmethod
    def _candidates(
        rows: tuple[dict[str, Any], ...],
        config: IndexOptionConfig,
        side: OptionSide,
        trade_date: date,
    ) -> list[OptionContract]:
        candidates: list[OptionContract] = []
        for row in rows:
            symbol = str(row.get("symbol") or "")
            if (
                str(row.get("exch_seg") or "") != config.option_exchange
                or str(row.get("name") or "").upper() != config.option_name
                or str(row.get("instrumenttype") or "") != "OPTIDX"
                or not symbol.endswith(side.value)
            ):
                continue
            try:
                expiry = parse_expiry(str(row.get("expiry") or ""))
                lot_size = int(Decimal(str(row.get("lotsize") or "0")))
                strike = Decimal(str(row.get("strike") or "0")) / 100
            except (InvalidOperation, TypeError, ValueError):
                continue
            token = str(row.get("token") or "")
            if expiry >= trade_date and token and lot_size > 0 and strike > 0:
                candidates.append(OptionContract(token, symbol, expiry, lot_size, strike, side))
        return candidates

    async def _quote(self, user_id: UUID, exchange: str, token: str) -> Decimal | None:
        client = await self.client_factory(user_id)
        try:
            payload = await client.rest.quote("LTP", {exchange: [token]})
            return quote_ltps(payload).get(token)
        finally:
            await client.close()

    async def select(
        self,
        *,
        user_id: UUID,
        config: IndexOptionConfig,
        side: OptionSide,
        trade_date: date,
    ) -> SuperTrendMarketSelection:
        underlying = await self._quote(user_id, config.index_exchange, config.index_token)
        if underlying is None:
            raise RuntimeError(f"Angel {config.instrument} quote did not include LTP.")
        for refresh in (False, True):
            candidates = self._candidates(
                await self._master(refresh=refresh), config, side, trade_date
            )
            expiries = sorted({item.expiry for item in candidates})
            for expiry in expiries:
                bucket = sorted(
                    (item for item in candidates if item.expiry == expiry),
                    key=lambda item: (abs(item.strike - underlying), item.strike),
                )
                for candidate in bucket:
                    premium = await self._quote(user_id, config.option_exchange, candidate.token)
                    if premium is not None:
                        return SuperTrendMarketSelection(
                            OptionContract(
                                candidate.token,
                                candidate.symbol,
                                candidate.expiry,
                                candidate.lot_size,
                                candidate.strike,
                                candidate.option_type,
                                premium,
                            ),
                            underlying,
                        )
        raise RuntimeError(
            f"No quoteable {config.instrument} {side.value} option contract is available."
        )


__all__ = [
    "AngelSuperTrendMarketProvider",
    "SuperTrendMarketProvider",
    "SuperTrendMarketSelection",
    "quote_ltps",
]
