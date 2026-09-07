import asyncio

import httpx
import pytest
from pydantic import SecretStr

from app.broker.angel.egress import StaticEgressBinding
from app.broker.angel.models import AccountContext
from app.broker.angel.rest import AngelRestClient


def ctx(user: str) -> AccountContext:
    return AccountContext(user, f"client-{user}", SecretStr(f"key-{user}"), SecretStr(f"jwt-{user}"), SecretStr("refresh"), SecretStr(f"feed-{user}"), 1)


@pytest.mark.asyncio
async def test_concurrent_account_reads_do_not_cross_credentials():
    seen: list[tuple[str, str]] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        api = request.headers["x-privatekey"]
        jwt = request.headers["authorization"]
        seen.append((api, jwt))
        return httpx.Response(200, json={"status": True, "data": []})

    async def fetch(user: str):
        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
            return await AngelRestClient("http://angel.test", ctx(user), http).positions()

    await asyncio.gather(fetch("a"), fetch("b"))
    assert set(seen) == {("key-a", "Bearer jwt-a"), ("key-b", "Bearer jwt-b")}


@pytest.mark.asyncio
async def test_explicit_egress_never_falls_back_to_default_transport():
    binding = StaticEgressBinding({"a": "198.51.100.10"})
    with pytest.raises(Exception) as caught:
        from app.broker.angel.factory import AngelClientFactory
        await AngelClientFactory("http://angel.test", "ws://angel.test", binding).create(ctx("a"))
    assert "fall back" in str(caught.value).lower()

