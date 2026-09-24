from datetime import date
from decimal import Decimal
from uuid import uuid4

import httpx
import pytest

from app.runtime.market import AngelSuperTrendMarketProvider, quote_ltps
from app.strategy.supertrend import CONFIGS, OptionSide


class FakeRest:
    def __init__(self, quotes):
        self.quotes = quotes

    async def quote(self, _mode, exchange_tokens):
        exchange, tokens = next(iter(exchange_tokens.items()))
        token = tokens[0]
        price = self.quotes.get((exchange, token))
        return {"data": {"fetched": [{"symbolToken": token, "ltp": price}]}}


class FakeClient:
    def __init__(self, quotes):
        self.rest = FakeRest(quotes)
        self.closed = False

    async def close(self):
        self.closed = True


def test_nested_quote_ltp_parser_is_token_specific_and_positive() -> None:
    assert quote_ltps(
        {
            "data": [
                {"symbolToken": "a", "ltp": "101.25"},
                {"token": "b", "ltp": 0},
            ]
        }
    ) == {"a": Decimal("101.25")}


@pytest.mark.asyncio
async def test_supertrend_market_selects_nearest_strike_in_nearest_expiry() -> None:
    contracts = [
        {
            "token": "far-expiry-atm",
            "symbol": "NIFTY27AUG2625000CE",
            "name": "NIFTY",
            "expiry": "27AUG2026",
            "strike": "2500000",
            "lotsize": "50",
            "instrumenttype": "OPTIDX",
            "exch_seg": "NFO",
        },
        {
            "token": "near-low",
            "symbol": "NIFTY20AUG2624950CE",
            "name": "NIFTY",
            "expiry": "20AUG2026",
            "strike": "2495000",
            "lotsize": "50",
            "instrumenttype": "OPTIDX",
            "exch_seg": "NFO",
        },
        {
            "token": "near-atm",
            "symbol": "NIFTY20AUG2625000CE",
            "name": "NIFTY",
            "expiry": "20AUG2026",
            "strike": "2500000",
            "lotsize": "50",
            "instrumenttype": "OPTIDX",
            "exch_seg": "NFO",
        },
    ]

    async def master(_: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json=contracts)

    quotes = {
        ("NSE", CONFIGS["NIFTY"].index_token): "25010",
        ("NFO", "near-atm"): "101.5",
    }
    clients = []

    async def client_factory(_user):
        client = FakeClient(quotes)
        clients.append(client)
        return client

    async with httpx.AsyncClient(transport=httpx.MockTransport(master)) as http:
        provider = AngelSuperTrendMarketProvider(client_factory, http_client=http)
        selected = await provider.select(
            user_id=uuid4(),
            config=CONFIGS["NIFTY"],
            side=OptionSide.CALL,
            trade_date=date(2026, 8, 19),
        )
    assert selected.underlying_ltp == Decimal(25010)
    assert selected.contract.token == "near-atm"
    assert selected.contract.expiry == date(2026, 8, 20)
    assert selected.contract.premium == Decimal("101.5")
    assert all(client.closed for client in clients)


@pytest.mark.asyncio
async def test_supertrend_market_skips_unquoteable_nearest_candidate() -> None:
    rows = [
        {
            "token": token,
            "symbol": f"SENSEX20AUG26{strike}PE",
            "name": "SENSEX",
            "expiry": "20AUG2026",
            "strike": str(strike * 100),
            "lotsize": "20",
            "instrumenttype": "OPTIDX",
            "exch_seg": "BFO",
        }
        for token, strike in (("nearest", 80000), ("fallback", 80100))
    ]

    async def master(_: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json=rows)

    quotes = {
        ("BSE", CONFIGS["SENSEX"].index_token): "80010",
        ("BFO", "fallback"): "88",
    }

    async def client_factory(_user):
        return FakeClient(quotes)

    async with httpx.AsyncClient(transport=httpx.MockTransport(master)) as http:
        selected = await AngelSuperTrendMarketProvider(client_factory, http_client=http).select(
            user_id=uuid4(),
            config=CONFIGS["SENSEX"],
            side=OptionSide.PUT,
            trade_date=date(2026, 8, 19),
        )
    assert selected.contract.token == "fallback"
