import math
from typing import Any

import httpx

from .auth import _authenticated_headers, _decode_envelope
from .errors import BrokerError, BrokerErrorCategory
from .models import (
    BrokerReadFailure,
    BrokerReadSuccess,
    ConditionalRule,
    OrderRecord,
    PositionRecord,
    TradeRecord,
)
from .retry import CooldownRegistry, RequestPacer


class AngelRestClient:
    def __init__(self, base_url: str, account, transport: httpx.AsyncClient, pacer: RequestPacer | None = None, cooldowns: CooldownRegistry | None = None):
        self.base_url = base_url.rstrip("/")
        self.account = account
        self.transport = transport
        self.pacer = pacer or RequestPacer()
        self.cooldowns = cooldowns or CooldownRegistry()

    async def _request(self, operation: str, method: str, path: str, body: Any = None, pace: tuple[tuple[int, float], ...] = ((1, 1.05),), retry_candles: bool = False) -> Any:
        cooldown_key = f"{self.account.api_key.get_secret_value()}:{path}"
        if await self.cooldowns.remaining(cooldown_key):
            seconds = await self.cooldowns.remaining(cooldown_key)
            raise BrokerError(BrokerErrorCategory.RATE_LIMITED, "Angel One rate limit is active.", operation, retry_after_seconds=seconds, retryable=False)
        await self.pacer.acquire(self.account.api_key.get_secret_value(), operation, pace)
        attempts = 2 if retry_candles else 1
        for attempt in range(attempts):
            try:
                response = await self.transport.request(method, f"{self.base_url}{path}", headers=_authenticated_headers(self.account), json=body, timeout=15.0)
                try:
                    data = _decode_envelope(response, operation)
                except BrokerError as error:
                    if error.category == BrokerErrorCategory.RATE_LIMITED:
                        await self.cooldowns.activate(cooldown_key, error.retry_after_seconds or 90)
                    raise
                return data
            except BrokerError as broker_error:
                if attempt == 0 and retry_candles and broker_error.category in {BrokerErrorCategory.TIMEOUT, BrokerErrorCategory.TRANSPORT_FAILURE}:
                    continue
                raise
            except httpx.TimeoutException as exc:
                if attempt == 0 and retry_candles:
                    continue
                raise BrokerError(BrokerErrorCategory.TIMEOUT, f"Angel One {operation} timed out.", operation, retryable=retry_candles, diagnostic=type(exc).__name__) from exc
            except httpx.HTTPError as exc:
                if attempt == 0 and retry_candles:
                    continue
                raise BrokerError(BrokerErrorCategory.TRANSPORT_FAILURE, f"Angel One {operation} transport failed.", operation, retryable=retry_candles, diagnostic=type(exc).__name__) from exc
        raise RuntimeError("unreachable")

    async def order_book(self) -> list[OrderRecord]:
        return [OrderRecord.from_payload(item) for item in _list_data(await self._request("order-book", "GET", "/rest/secure/angelbroking/order/v1/getOrderBook"))]

    async def trade_book(self) -> list[TradeRecord]:
        return [TradeRecord.from_payload(item) for item in _list_data(await self._request("trade-book", "GET", "/rest/secure/angelbroking/order/v1/getTradeBook"))]

    async def positions(self) -> list[PositionRecord]:
        return [PositionRecord.from_payload(item) for item in _list_data(await self._request("positions", "GET", "/rest/secure/angelbroking/order/v1/getPosition"))]

    async def conditional_rules(self) -> list[ConditionalRule]:
        result: list[ConditionalRule] = []
        for page in range(1, 101):
            data = _list_data(await self._request("conditional-rules", "POST", "/rest/secure/angelbroking/gtt/v1/ruleList", {"status": ["NEW", "CANCELLED", "ACTIVE", "SENTTOEXCHANGE", "FORALL"], "page": page, "count": 100}))
            result.extend(ConditionalRule.from_payload(item) for item in data)
            if len(data) < 100:
                return result
        raise BrokerError(BrokerErrorCategory.MALFORMED_RESPONSE, "Conditional-order pagination exceeded the safety limit.", "conditional-rules")

    async def candles(self, exchange: str, token: str, interval: str, from_date: str, to_date: str) -> Any:
        return await self._request("candles", "POST", "/rest/secure/angelbroking/historical/v1/getCandleData", {"exchange": exchange, "symboltoken": token, "interval": interval, "fromdate": from_date, "todate": to_date}, ((2, 1.0), (150, 60.0), (4500, 3600.0)), True)

    async def quote(self, mode: str, exchange_tokens: dict[str, list[str]]) -> Any:
        return await self._request("market-quote", "POST", "/rest/secure/angelbroking/market/v1/quote", {"mode": mode, "exchangeTokens": exchange_tokens}, ((1, 1.05), (450, 60.0), (4500, 3600.0)))

    async def rms_limits(self) -> Any:
        return await self._request("rms-limits", "GET", "/rest/secure/angelbroking/user/v1/getRMS", pace=((2, 1.05),))

    async def margin_required(self, position: dict[str, Any]) -> float:
        data = await self._request("margin-calculator", "POST", "/rest/secure/angelbroking/margin/v1/batch", {"positions": [position]}, ((8, 1.0),))
        value = data.get("totalMarginRequired") if isinstance(data, dict) else None
        if value is None:
            raise BrokerError(BrokerErrorCategory.MALFORMED_RESPONSE, "Angel One returned invalid required margin.", "margin-calculator", raw=data)
        try:
            parsed = float(value)
        except (TypeError, ValueError) as exc:
            raise BrokerError(BrokerErrorCategory.MALFORMED_RESPONSE, "Angel One returned invalid required margin.", "margin-calculator", raw=data) from exc
        if parsed < 0 or not math.isfinite(parsed):
            raise BrokerError(BrokerErrorCategory.MALFORMED_RESPONSE, "Angel One returned invalid required margin.", "margin-calculator", raw=data)
        return parsed

    async def safe_order_book(self):
        return await _safe_read(self.order_book)

    async def safe_trade_book(self):
        return await _safe_read(self.trade_book)

    async def safe_positions(self):
        return await _safe_read(self.positions)

    async def safe_conditional_rules(self):
        return await _safe_read(self.conditional_rules)


def _list_data(data: Any) -> list[dict[str, Any]]:
    if data is None:
        return []
    if not isinstance(data, list) or any(not isinstance(item, dict) for item in data):
        raise BrokerError(BrokerErrorCategory.MALFORMED_RESPONSE, "Angel One returned a malformed list response.", "read", raw=data)
    return data


async def _safe_read(call):
    try:
        return BrokerReadSuccess(data=await call())
    except BrokerError as error:
        return BrokerReadFailure(error=error)
