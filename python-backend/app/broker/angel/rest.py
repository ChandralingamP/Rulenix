import math
from typing import Any

import httpx

from .auth import _authenticated_headers, _decode_envelope
from .errors import BrokerError, BrokerErrorCategory
from .models import (
    BrokerMutationResponse,
    BrokerReadFailure,
    BrokerReadSuccess,
    CancelOrderRequest,
    ConditionalMutationRequest,
    ConditionalRule,
    ModifyOrderRequest,
    OrderMutationRequest,
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

    async def _mutation_request(
        self, operation: str, path: str, body: dict[str, Any]
    ) -> Any:
        """Execute exactly one broker write; unknown outcomes are never retried."""
        cooldown_key = f"{self.account.api_key.get_secret_value()}:{path}"
        remaining = await self.cooldowns.remaining(cooldown_key)
        if remaining:
            raise BrokerError(
                BrokerErrorCategory.RATE_LIMITED,
                "Angel One rate limit is active.",
                operation,
                retry_after_seconds=remaining,
                retryable=False,
            )
        await self.pacer.acquire(
            self.account.api_key.get_secret_value(), operation, ((1, 1.05),)
        )
        try:
            response = await self.transport.request(
                "POST",
                f"{self.base_url}{path}",
                headers=_authenticated_headers(self.account),
                json=body,
                timeout=15.0,
            )
            try:
                return _decode_envelope(response, operation)
            except BrokerError as error:
                if error.category == BrokerErrorCategory.RATE_LIMITED:
                    await self.cooldowns.activate(cooldown_key, error.retry_after_seconds or 90)
                if error.category in {
                    BrokerErrorCategory.TIMEOUT,
                    BrokerErrorCategory.TRANSPORT_FAILURE,
                }:
                    raise BrokerError(
                        BrokerErrorCategory.AMBIGUOUS,
                        f"Angel One {operation} outcome is unknown; reconciliation is required.",
                        operation,
                        status_code=error.status_code,
                        code=error.code,
                        retryable=False,
                        diagnostic=error.diagnostic or error.category.value,
                    ) from error
                raise
        except httpx.ConnectError as exc:
            # A connect failure is known to occur before an HTTP submission,
            # but remains durable and operator/reconciliation driven.  The
            # coordinator does not automatically replay it.
            raise BrokerError(
                BrokerErrorCategory.TRANSPORT_FAILURE,
                f"Angel One {operation} could not connect before submission.",
                operation,
                retryable=True,
                diagnostic=type(exc).__name__,
            ) from exc
        except httpx.TimeoutException as exc:
            raise BrokerError(
                BrokerErrorCategory.AMBIGUOUS,
                f"Angel One {operation} outcome is unknown; reconciliation is required.",
                operation,
                retryable=False,
                diagnostic=type(exc).__name__,
            ) from exc
        except httpx.HTTPError as exc:
            raise BrokerError(
                BrokerErrorCategory.AMBIGUOUS,
                f"Angel One {operation} outcome is unknown; reconciliation is required.",
                operation,
                retryable=False,
                diagnostic=type(exc).__name__,
            ) from exc

    @staticmethod
    def _mutation_response(operation: str, data: Any, fallback_id: str = "") -> BrokerMutationResponse:
        if isinstance(data, str):
            order_id = data.strip()
            unique_id = ""
        elif isinstance(data, dict):
            order_id = str(data.get("orderid") or data.get("orderId") or fallback_id).strip()
            unique_id = str(data.get("uniqueorderid") or data.get("uniqueOrderId") or "").strip()
        elif data is None and fallback_id:
            order_id = fallback_id
            unique_id = ""
        else:
            order_id = ""
            unique_id = ""
        if not order_id:
            raise BrokerError(
                BrokerErrorCategory.AMBIGUOUS,
                f"Angel One accepted {operation} without an order identifier; reconciliation is required.",
                operation,
                retryable=False,
                diagnostic="missing_order_id",
            )
        return BrokerMutationResponse(
            operation=operation,
            broker_order_id=order_id,
            unique_order_id=unique_id,
            raw=data,
        )

    async def place_order(self, request: OrderMutationRequest) -> BrokerMutationResponse:
        data = await self._mutation_request(
            "place-order",
            "/rest/secure/angelbroking/order/v1/placeOrder",
            request.angel_payload(),
        )
        return self._mutation_response("place-order", data)

    async def cancel_order(self, request: CancelOrderRequest) -> BrokerMutationResponse:
        data = await self._mutation_request(
            "cancel-order",
            "/rest/secure/angelbroking/order/v1/cancelOrder",
            request.angel_payload(),
        )
        return self._mutation_response("cancel-order", data, request.order_id)

    async def modify_order(self, request: ModifyOrderRequest) -> BrokerMutationResponse:
        data = await self._mutation_request(
            "modify-order",
            "/rest/secure/angelbroking/order/v1/modifyOrder",
            request.angel_payload(),
        )
        return self._mutation_response("modify-order", data, request.order_id)

    async def create_conditional(
        self, request: ConditionalMutationRequest
    ) -> BrokerMutationResponse:
        data = await self._mutation_request(
            "gtt-create",
            "/rest/secure/angelbroking/gtt/v1/createRule",
            request.angel_payload(),
        )
        return self._mutation_response("gtt-create", data)

    async def modify_conditional(
        self, request: ConditionalMutationRequest
    ) -> BrokerMutationResponse:
        if not request.rule_id:
            raise ValueError("Conditional modification requires a rule_id.")
        data = await self._mutation_request(
            "gtt-modify",
            "/rest/secure/angelbroking/gtt/v1/modifyRule",
            request.angel_payload(),
        )
        return self._mutation_response("gtt-modify", data, request.rule_id)

    async def cancel_conditional(self, rule_id: str) -> BrokerMutationResponse:
        if not rule_id.strip():
            raise ValueError("Conditional cancellation requires a rule_id.")
        data = await self._mutation_request(
            "gtt-cancel",
            "/rest/secure/angelbroking/gtt/v1/cancelRule",
            {"id": rule_id},
        )
        return self._mutation_response("gtt-cancel", data, rule_id)

    async def order_book(self) -> list[OrderRecord]:
        return [OrderRecord.from_payload(item) for item in _list_data(await self._request("order-book", "GET", "/rest/secure/angelbroking/order/v1/getOrderBook"))]

    async def individual_order(self, order_id: str) -> OrderRecord:
        """Read one order without changing it.

        SmartAPI exposes this as the order-details resource.  AB1007 is left as
        ``BrokerErrorCategory.ORDER_NOT_FOUND`` so OCO classification can
        distinguish a proven absence from a timeout or authentication failure.
        """
        if not order_id.strip():
            raise ValueError("order_id must not be empty")
        payload = await self._request("individual-order", "POST", "/rest/secure/angelbroking/order/v1/details", {"orderid": order_id})
        if not isinstance(payload, dict):
            raise BrokerError(BrokerErrorCategory.MALFORMED_RESPONSE, "Angel One individual-order response is malformed.", "individual-order", raw=payload)
        return OrderRecord.from_payload(payload)

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

    async def safe_individual_order(self, order_id: str):
        return await _safe_read(lambda: self.individual_order(order_id))

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
