from uuid import uuid4

import httpx
import pytest
from pydantic import SecretStr

from app.broker.angel.client import AngelClient
from app.broker.angel.egress import DefaultEgressBinding
from app.broker.angel.errors import MutationBlockedError
from app.broker.angel.models import AccountContext
from app.broker.angel.mutation_guard import MutationGuard

MUTATION_PATHS = frozenset(
    {
        "/rest/secure/angelbroking/order/v1/placeOrder",
        "/rest/secure/angelbroking/order/v1/modifyOrder",
        "/rest/secure/angelbroking/order/v1/cancelOrder",
        "/rest/secure/angelbroking/gtt/v1/createRule",
        "/rest/secure/angelbroking/gtt/v1/modifyRule",
        "/rest/secure/angelbroking/gtt/v1/cancelRule",
    }
)


class MutationTrapTransport(httpx.AsyncBaseTransport):
    def __init__(self):
        self.requests: list[httpx.Request] = []

    async def handle_async_request(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        if request.url.path in MUTATION_PATHS:
            raise AssertionError(f"Angel mutation reached network trap: {request.method} {request.url.path}")
        return httpx.Response(404, request=request, json={"status": False, "message": "fixture not configured"})


@pytest.mark.asyncio
async def test_network_trap_observes_zero_trading_mutation_requests():
    trap = MutationTrapTransport()
    transport = httpx.AsyncClient(transport=trap)
    account = AccountContext(
        user_id=str(uuid4()),
        client_code="fixture-client",
        api_key=SecretStr("fixture-key"),
        jwt_token=SecretStr("fixture-jwt"),
        refresh_token=SecretStr("fixture-refresh"),
        feed_token=SecretStr("fixture-feed"),
    )
    client = AngelClient(
        "https://fixture.invalid",
        "wss://fixture.invalid/ws",
        account,
        transport,
        DefaultEgressBinding(),
        MutationGuard(),
    )
    with pytest.raises(MutationBlockedError):
        await client.place_order({"symbol": "GOLDTEN"})
    with pytest.raises(MutationBlockedError):
        await client.cancel_order("broker-order")
    with pytest.raises(MutationBlockedError):
        await client.manual_close("trade")
    assert trap.requests == []
    await transport.aclose()
