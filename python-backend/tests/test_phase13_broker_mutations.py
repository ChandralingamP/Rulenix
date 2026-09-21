import json

import httpx
import pytest
from pydantic import SecretStr

from app.broker.angel.errors import BrokerError, BrokerErrorCategory
from app.broker.angel.models import (
    AccountContext,
    CancelOrderRequest,
    ConditionalMutationRequest,
    ModifyOrderRequest,
    OrderMutationRequest,
)
from app.broker.angel.rest import AngelRestClient


def account() -> AccountContext:
    return AccountContext(
        "user-a",
        "client-a",
        SecretStr("api-a"),
        SecretStr("jwt-a"),
        SecretStr("refresh-a"),
        SecretStr("feed-a"),
        4,
        "10.0.0.1",
        "203.0.113.10",
        "00:11:22:33:44:55",
    )


def order() -> OrderMutationRequest:
    return OrderMutationRequest(
        variety="STOPLOSS",
        trading_symbol="GOLDTEN26OCTFUT",
        symbol_token="12345",
        transaction_type="BUY",
        exchange="MCX",
        order_type="STOPLOSS_LIMIT",
        product_type="CARRYFORWARD",
        price="100.00",
        trigger_price="99.00",
        quantity=10,
        client_reference="RX0123456789",
    )


@pytest.mark.asyncio
async def test_place_uses_exact_headers_payload_and_single_request():
    seen: list[httpx.Request] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        return httpx.Response(
            200,
            json={
                "status": True,
                "data": {"orderid": "broker-1", "uniqueorderid": "unique-1"},
            },
        )

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
        result = await AngelRestClient("http://angel.test", account(), http).place_order(order())
    assert result.broker_order_id == "broker-1"
    assert result.unique_order_id == "unique-1"
    assert len(seen) == 1
    assert seen[0].url.path.endswith("/placeOrder")
    assert seen[0].headers["authorization"] == "Bearer jwt-a"
    assert json.loads(seen[0].content) == order().angel_payload()


@pytest.mark.asyncio
async def test_timeout_and_lost_response_are_ambiguous_and_never_retried():
    calls = 0

    async def timeout(_: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        raise httpx.ReadTimeout("accepted but response lost")

    async with httpx.AsyncClient(transport=httpx.MockTransport(timeout)) as http:
        with pytest.raises(BrokerError) as caught:
            await AngelRestClient("http://angel.test", account(), http).place_order(order())
    assert caught.value.category is BrokerErrorCategory.AMBIGUOUS
    assert caught.value.retryable is False
    assert calls == 1


@pytest.mark.asyncio
async def test_rejection_cancel_modify_and_gtt_normalization():
    paths: list[str] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        paths.append(request.url.path)
        if request.url.path.endswith("placeOrder"):
            return httpx.Response(
                200,
                json={"status": False, "errorcode": "AB2001", "message": "rejected"},
            )
        body = json.loads(request.content)
        identifier = body.get("orderid") or body.get("id") or "new-rule"
        return httpx.Response(200, json={"status": True, "data": {"orderid": identifier}})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
        client = AngelRestClient("http://angel.test", account(), http)
        with pytest.raises(BrokerError) as caught:
            await client.place_order(order())
        assert caught.value.category is BrokerErrorCategory.BROKER_REJECTED
        assert (
            await client.cancel_order(CancelOrderRequest(variety="NORMAL", order_id="broker-1"))
        ).broker_order_id == "broker-1"
        modified = ModifyOrderRequest(**order().model_dump(), order_id="broker-1")
        assert (await client.modify_order(modified)).broker_order_id == "broker-1"
        gtt = ConditionalMutationRequest(
            trading_symbol="GOLDTEN26OCTFUT",
            symbol_token="12345",
            exchange="MCX",
            product_type="CARRYFORWARD",
            transaction_type="BUY",
            price="100",
            quantity=10,
            trigger_price="99",
        )
        assert (await client.create_conditional(gtt)).broker_order_id == "new-rule"
    assert len(paths) == 4
