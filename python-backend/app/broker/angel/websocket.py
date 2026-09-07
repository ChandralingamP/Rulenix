import asyncio
import json
import logging
import random
import struct
from collections.abc import AsyncIterator
from datetime import UTC, datetime
from typing import Any

import websockets
from pydantic import ValidationError
from websockets.asyncio.client import ClientConnection

from .egress import EgressBinding
from .errors import BrokerError, BrokerErrorCategory
from .models import AccountContext, MarketTick

logger = logging.getLogger(__name__)


def parse_tick(data: bytes) -> MarketTick | None:
    if len(data) < 51:
        return None
    try:
        mode = data[0]
        exchange_type = data[1]
        token_bytes = data[2:27]
        token = token_bytes.split(b"\0", 1)[0].decode(errors="replace")
        values = struct.unpack_from("<qqq", data, 27)
        tick = {"subscription_mode": mode, "exchange_type": exchange_type, "token": token, "sequence_number": values[0], "exchange_timestamp": values[1], "last_traded_price": values[2] / 100.0}
        if mode >= 2 and len(data) >= 123:
            offsets = {"last_traded_quantity": 51, "average_traded_price": 59, "volume_trade_for_the_day": 67, "open_price_of_the_day": 91, "high_price_of_the_day": 99, "low_price_of_the_day": 107, "closed_price": 115}
            for key, offset in offsets.items():
                value = struct.unpack_from("<q", data, offset)[0]
                tick[key] = value / 100.0 if key.endswith("price") else value
        return MarketTick.model_validate(tick)
    except (struct.error, ValidationError, UnicodeError):
        return None


def tick_timestamp(tick: MarketTick) -> datetime | None:
    try:
        value = datetime.fromtimestamp(tick.exchange_timestamp / 1000, UTC)
    except (OverflowError, OSError, ValueError):
        return None
    return value if abs((value - datetime.now(UTC)).total_seconds()) <= 24 * 3600 else None


def stale_threshold(exchange: str) -> int:
    return 120 if exchange.upper() == "MCX" else 45


class AngelWebSocketClient:
    def __init__(self, ws_url: str, account: AccountContext, egress: EgressBinding):
        self.ws_url = ws_url
        self.account = account
        self.egress = egress
        self._socket: ClientConnection | None = None
        self._subscriptions: dict[int, set[str]] = {}

    async def connect(self) -> None:
        selection = await self.egress.select(self.account.user_id)
        kwargs: dict[str, Any] = {"additional_headers": self._headers()}
        if selection.explicit:
            kwargs["local_address"] = (selection.source_ip, 0)
        try:
            self._socket = await websockets.connect(self.ws_url, **kwargs)
        except Exception as exc:
            raise BrokerError(BrokerErrorCategory.TRANSPORT_FAILURE, "Angel One WebSocket connection failed without fallback.", "websocket-connect", diagnostic=type(exc).__name__) from exc

    async def close(self) -> None:
        if self._socket:
            await self._socket.close()
            self._socket = None

    async def subscribe(self, tokens: list[str], exchange_type: int = 1, mode: int = 1) -> None:
        if not self._socket:
            raise BrokerError(BrokerErrorCategory.TRANSPORT_FAILURE, "WebSocket is not connected.", "websocket-subscribe")
        clean = [token.strip() for token in tokens if token.strip()]
        if not clean:
            raise ValueError("At least one token is required.")
        self._subscriptions.setdefault(exchange_type, set()).update(clean)
        message = {"correlationID": f"{random.randint(0, 9999999999):010d}", "action": 1, "params": {"mode": mode, "tokenList": [{"exchangeType": exchange_type, "tokens": clean}]}}
        await self._socket.send(json.dumps(message, separators=(",", ":")))

    async def unsubscribe(self, tokens: list[str], exchange_type: int = 1, mode: int = 1) -> None:
        if not self._socket:
            raise BrokerError(BrokerErrorCategory.TRANSPORT_FAILURE, "WebSocket is not connected.", "websocket-unsubscribe")
        clean = [token.strip() for token in tokens if token.strip()]
        self._subscriptions.get(exchange_type, set()).difference_update(clean)
        message = {"correlationID": f"{random.randint(0, 9999999999):010d}", "action": 0, "params": {"mode": mode, "tokenList": [{"exchangeType": exchange_type, "tokens": clean}]}}
        await self._socket.send(json.dumps(message, separators=(",", ":")))

    async def events(self, stop: asyncio.Event | None = None) -> AsyncIterator[MarketTick | dict[str, Any]]:
        if not self._socket:
            raise BrokerError(BrokerErrorCategory.TRANSPORT_FAILURE, "WebSocket is not connected.", "websocket-events")
        while not (stop and stop.is_set()):
            try:
                message = await asyncio.wait_for(self._socket.recv(), timeout=10)
            except TimeoutError:
                await self._socket.send("ping")
                continue
            if message is None:
                return
            if isinstance(message, bytes):
                tick = parse_tick(message)
                if tick:
                    yield tick
            elif message == "pong":
                continue
            else:
                try:
                    yield json.loads(message)
                except (TypeError, json.JSONDecodeError):
                    raise BrokerError(BrokerErrorCategory.MALFORMED_RESPONSE, "Angel One WebSocket event is malformed.", "websocket-event")

    async def reconnecting_events(self, stop: asyncio.Event | None = None, max_attempts: int = 6) -> AsyncIterator[MarketTick | dict[str, Any]]:
        attempt = 0
        while not (stop and stop.is_set()):
            try:
                await self.connect()
                for exchange_type, tokens in self._subscriptions.items():
                    await self.subscribe(list(tokens), exchange_type)
                async for event in self.events(stop):
                    yield event
                if stop and stop.is_set():
                    return
                raise BrokerError(BrokerErrorCategory.TRANSPORT_FAILURE, "Angel One WebSocket disconnected.", "websocket-disconnect", retryable=True)
            except (BrokerError, websockets.WebSocketException) as exc:
                await self.close()
                attempt += 1
                if attempt > max_attempts or (stop and stop.is_set()):
                    raise BrokerError(BrokerErrorCategory.TRANSPORT_FAILURE, "Angel One WebSocket reconnect budget exhausted.", "websocket-reconnect", diagnostic=type(exc).__name__) from exc
                delay = min(90, 2 ** min(attempt - 1, 6))
                await asyncio.sleep(delay + random.random() * 0.25 * delay)

    def _headers(self) -> dict[str, str]:
        return {"Authorization": self.account.jwt_token.get_secret_value(), "x-api-key": self.account.api_key.get_secret_value(), "x-client-code": self.account.client_code, "x-feed-token": self.account.feed_token.get_secret_value()}
