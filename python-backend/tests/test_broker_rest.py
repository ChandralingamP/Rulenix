import httpx
import pytest
from pydantic import SecretStr

from app.broker.angel.auth import AngelAuthenticator
from app.broker.angel.errors import BrokerError, BrokerErrorCategory
from app.broker.angel.models import AccountContext
from app.broker.angel.rest import AngelRestClient


def account(user: str, api: str = "api-a") -> AccountContext:
    return AccountContext(user, f"client-{user}", SecretStr(api), SecretStr("jwt-" + user), SecretStr("refresh"), SecretStr("feed-" + user), 4, "10.0.0.1", "203.0.113.10", "00:11:22:33:44:55")


@pytest.mark.asyncio
async def test_read_headers_and_empty_success_are_typed():
    seen: list[httpx.Request] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        return httpx.Response(200, json={"status": True, "message": "", "data": []})

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport) as http:
        result = await AngelRestClient("http://angel.test", account("a"), http).order_book()
    assert result == []
    assert seen[0].headers["x-privatekey"] == "api-a"
    assert seen[0].headers["authorization"] == "Bearer jwt-a"
    assert seen[0].url.path.endswith("/getOrderBook")


@pytest.mark.asyncio
async def test_read_failure_is_not_flat_and_preserves_order_not_found():
    async def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(404, json={"status": False, "message": "Order not found", "errorcode": "AB1007", "data": None})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
        client = AngelRestClient("http://angel.test", account("a"), http)
        with pytest.raises(BrokerError) as caught:
            await client.order_book()
    assert caught.value.category is BrokerErrorCategory.ORDER_NOT_FOUND
    assert caught.value.code == "AB1007"
    assert caught.value.category is not BrokerErrorCategory.UNKNOWN

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
        outcome = await AngelRestClient("http://angel.test", account("a"), http).safe_order_book()
    assert outcome.ok is False
    assert outcome.error.category is BrokerErrorCategory.ORDER_NOT_FOUND


@pytest.mark.asyncio
async def test_candle_timeout_has_one_bounded_retry_but_order_read_does_not():
    candle_calls = 0
    order_calls = 0

    async def candles(request: httpx.Request) -> httpx.Response:
        nonlocal candle_calls
        candle_calls += 1
        if candle_calls == 1:
            raise httpx.ReadTimeout("read timeout")
        return httpx.Response(200, json={"status": True, "data": []})

    async with httpx.AsyncClient(transport=httpx.MockTransport(candles)) as http:
        result = await AngelRestClient("http://angel.test", account("a"), http).candles("NSE", "1", "ONE_MINUTE", "2026-01-01 09:15", "2026-01-01 09:16")
    assert result == []
    assert candle_calls == 2

    async def order_timeout(_: httpx.Request) -> httpx.Response:
        nonlocal order_calls
        order_calls += 1
        raise httpx.ReadTimeout("read timeout")

    async with httpx.AsyncClient(transport=httpx.MockTransport(order_timeout)) as http:
        with pytest.raises(BrokerError) as caught:
            await AngelRestClient("http://angel.test", account("a"), http).order_book()
    assert caught.value.category is BrokerErrorCategory.TIMEOUT
    assert order_calls == 1


@pytest.mark.asyncio
async def test_login_uses_rust_contract_and_returns_scoped_tokens():
    async def handler(request: httpx.Request) -> httpx.Response:
        body = request.read().decode()
        assert '"clientcode":"client-a"' in body
        assert request.headers["x-usertype"] == "USER"
        return httpx.Response(200, json={"status": True, "data": {"jwtToken": "new-jwt", "refreshToken": "new-refresh", "feedToken": "new-feed"}})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
        session = await AngelAuthenticator("http://angel.test", http).login(account("a"), "1234", "123456")
    assert session.user_id == "a"
    assert session.jwt_token.get_secret_value() == "new-jwt"
    assert session.credential_revision == 5
