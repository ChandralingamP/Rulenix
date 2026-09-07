import asyncio
import json
import struct

import pytest
from pydantic import SecretStr

from app.broker.angel.egress import StaticEgressBinding
from app.broker.angel.errors import BrokerError
from app.broker.angel.models import AccountContext
from app.broker.angel.websocket import AngelWebSocketClient, parse_tick


def context(user: str) -> AccountContext:
    return AccountContext(user, f"client-{user}", SecretStr(f"key-{user}"), SecretStr(f"jwt-{user}"), SecretStr("refresh"), SecretStr(f"feed-{user}"))


def test_rust_binary_tick_parser_and_timestamp_fields():
    data = bytearray(51)
    data[0] = 1
    data[1] = 1
    data[2:7] = b"12345"
    data[27:35] = struct.pack("<q", 11)
    data[35:43] = struct.pack("<q", 1_700_000_000_000)
    data[43:51] = struct.pack("<q", 12345)
    tick = parse_tick(bytes(data))
    assert tick is not None
    assert tick.token == "12345"
    assert tick.last_traded_price == 123.45
    assert parse_tick(b"short") is None


@pytest.mark.asyncio
async def test_websocket_headers_subscription_and_account_egress_are_scoped(monkeypatch):
    sent: list[str] = []

    class FakeSocket:
        async def send(self, message: str):
            sent.append(message)

        async def close(self):
            return None

    captured: dict[str, object] = {}

    async def fake_connect(url: str, **kwargs):
        captured["url"] = url
        captured.update(kwargs)
        return FakeSocket()

    monkeypatch.setattr("app.broker.angel.websocket.websockets.connect", fake_connect)
    egress = StaticEgressBinding({"user-a": "198.51.100.11"})
    client = AngelWebSocketClient("wss://angel.test/feed", context("user-a"), egress)
    await client.connect()
    await client.subscribe(["12345"], exchange_type=1, mode=1)
    headers = captured["additional_headers"]
    assert headers["x-api-key"] == "key-user-a"
    assert headers["x-client-code"] == "client-user-a"
    assert captured["local_address"] == ("198.51.100.11", 0)
    message = json.loads(sent[0])
    assert message["params"]["tokenList"][0]["tokens"] == ["12345"]
    await client.close()


@pytest.mark.asyncio
async def test_reconnect_replays_subscriptions_after_disconnect(monkeypatch):
    connections: list[object] = []

    class FakeSocket:
        async def send(self, message: str):
            return None

        async def recv(self):
            return None

        async def close(self):
            return None

    async def fake_connect(_url: str, **_kwargs):
        socket = FakeSocket()
        connections.append(socket)
        return socket

    monkeypatch.setattr("app.broker.angel.websocket.websockets.connect", fake_connect)
    stop = asyncio.Event()
    client = AngelWebSocketClient("wss://angel.test/feed", context("user-a"), StaticEgressBinding({}))
    await client.connect()
    await client.subscribe(["12345"])
    stream = client.reconnecting_events(stop, max_attempts=1)
    with pytest.raises(BrokerError):
        await stream.__anext__()
    assert len(connections) == 3
