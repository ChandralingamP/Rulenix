import httpx
import pytest
from pydantic import SecretStr

from app.broker.angel.client import AngelClient
from app.broker.angel.egress import DefaultEgressBinding
from app.broker.angel.errors import MutationBlockedError
from app.broker.angel.models import AccountContext
from app.broker.angel.mutation_guard import MutationGuard


@pytest.mark.asyncio
async def test_every_defined_mutation_is_blocked_before_http():
    calls = 0

    async def handler(_: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        return httpx.Response(200, json={"status": True, "data": {}})

    events: list[dict[str, str]] = []
    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as transport:
        context = AccountContext("user-a", "client-a", SecretStr("key-a"), SecretStr("jwt-a"), SecretStr("refresh"), SecretStr("feed-a"))
        client = AngelClient("http://angel.test", "ws://angel.test", context, transport, DefaultEgressBinding(), MutationGuard(events.append))
        for operation in ("place_order", "cancel_order", "manual_close"):
            with pytest.raises(MutationBlockedError) as caught:
                getattr(client, operation)({"example": "payload"})
            assert caught.value.operation == operation
    assert calls == 0
    assert [event["operation"] for event in events] == ["place_order", "cancel_order", "manual_close"]
    assert all("key-a" not in str(event) and "jwt-a" not in str(event) for event in events)

