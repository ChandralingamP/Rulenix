"""Regression tests for SuperTrend candle ingestion, intent expiry, and market selection timing."""

from datetime import UTC, datetime, timedelta
from decimal import Decimal
from unittest.mock import AsyncMock, MagicMock
from uuid import uuid4

import httpx
import pytest

from app.config import Settings
from app.runtime.lifecycle import AuthoritativeExecutionWorker
from app.runtime.market import AngelSuperTrendMarketProvider, SuperTrendMarketSelection
from app.runtime.market_data import MarketDataIngestionService
from app.strategy.common import IST
from app.strategy.supertrend import (
    CONFIGS,
    MAX_ENTRY_DELAY,
    OptionContract,
    OptionSide,
)


class _FakeMappingsResult:
    def __init__(self, rows):
        self._rows = rows

    def mappings(self):
        return self

    def scalars(self):
        return self

    def all(self):
        return self._rows

    def first(self):
        return self._rows[0] if self._rows else None


class _FakeSession:
    def __init__(self, *, scalar_values=None, execute_results=None):
        self._scalar_values = list(scalar_values or [])
        self._execute_results = list(execute_results or [])
        self.executed: list[tuple[str, dict]] = []
        self.committed = False

    async def __aenter__(self):
        return self

    async def __aexit__(self, exc_type, exc, tb):
        return False

    async def scalar(self, stmt, params=None):
        self.executed.append((str(stmt), dict(params or {})))
        if self._scalar_values:
            return self._scalar_values.pop(0)
        return None

    async def execute(self, stmt, params=None):
        self.executed.append((str(stmt), dict(params or {})))
        if self._execute_results:
            return self._execute_results.pop(0)
        return _FakeMappingsResult([])

    async def commit(self):
        self.committed = True


@pytest.mark.asyncio
async def test_sync_supertrend_candles_excludes_forming_candle_and_heals_stale_cache() -> None:
    sessions: list[_FakeSession] = []

    def session_factory():
        # 1st instrument (NIFTY): cache miss (None), then insert session
        # 2nd instrument (SENSEX): cache miss (None), then insert session
        if len(sessions) in (0, 2):
            sess = _FakeSession(scalar_values=[None])
        else:
            sess = _FakeSession()
        sessions.append(sess)
        return sess

    mock_client = MagicMock()
    mock_client.close = AsyncMock()
    mock_client.rest = MagicMock()
    # At 09:50:10 IST, latest completed 5m candle is 09:45:00; 09:50:00 is still forming.
    mock_client.rest.candles = AsyncMock(
        return_value=[
            ["2026-11-25T09:10:00+05:30", 24000.0, 24010.0, 23990.0, 24005.0, 100],
            ["2026-11-25T09:40:00+05:30", 24010.0, 24020.0, 24000.0, 24015.0, 200],
            ["2026-11-25T09:45:00+05:30", 24015.0, 24040.0, 24010.0, 24035.0, 300],
            ["2026-11-25T09:50:00+05:30", 24035.0, 24036.0, 24034.0, 24035.5, 10],
        ]
    )

    service = MarketDataIngestionService(
        session_factory,
        AsyncMock(return_value=mock_client),
        Settings(database_url="postgresql://user:pass@localhost:5432/db"),
    )
    service.get_active_broker_client = AsyncMock(return_value=(uuid4(), mock_client))

    now_ist = datetime(2026, 11, 25, 9, 50, 10, tzinfo=IST)
    inserted = await service.sync_supertrend_candles(until=now_ist.astimezone(UTC), days=1)

    # 2 valid market-hours completed candles (09:40, 09:45) * 2 index instruments = 4.
    # 09:10 (pre-open) and 09:50 (forming) are excluded.
    assert inserted == 4
    mock_client.close.assert_awaited_once()

    # Cache-check SQL must verify exact latest_expected candle AND fetched_at >= settled_at (09:50:02)
    cache_sql, cache_params = sessions[0].executed[0]
    assert "candle_time = :latest_expected" in cache_sql
    assert "fetched_at >= :settled_at" in cache_sql
    assert cache_params["latest_expected"] == datetime(2026, 11, 25, 9, 45, tzinfo=IST)
    assert cache_params["settled_at"] == datetime(2026, 11, 25, 9, 50, 2, tzinfo=IST)

    inserted_times = [
        params["candle_time"].astimezone(IST)
        for sql, params in sessions[1].executed
        if "INSERT INTO backtest_market_candles" in sql
    ]
    assert inserted_times == [
        datetime(2026, 11, 25, 9, 40, tzinfo=IST),
        datetime(2026, 11, 25, 9, 45, tzinfo=IST),
    ]


@pytest.mark.asyncio
async def test_sync_supertrend_candles_skips_only_when_completed_candle_is_fresh() -> None:
    session = _FakeSession(scalar_values=[1, 1])
    mock_client = MagicMock()
    mock_client.close = AsyncMock()
    mock_client.rest = MagicMock()
    mock_client.rest.candles = AsyncMock(return_value=[])

    service = MarketDataIngestionService(
        lambda: session,
        AsyncMock(return_value=mock_client),
        Settings(database_url="postgresql://user:pass@localhost:5432/db"),
    )
    service.get_active_broker_client = AsyncMock(return_value=(uuid4(), mock_client))

    now_ist = datetime(2026, 11, 25, 9, 50, 25, tzinfo=IST)
    inserted = await service.sync_supertrend_candles(until=now_ist.astimezone(UTC), days=1)

    assert inserted == 0
    mock_client.rest.candles.assert_not_awaited()
    mock_client.close.assert_awaited_once()


@pytest.mark.asyncio
async def test_sync_futures_snapshots_returns_early_when_all_instruments_cached() -> None:
    from app.runtime.market_data import FUTURES_INSTRUMENTS

    session = _FakeSession(execute_results=[_FakeMappingsResult(list(FUTURES_INSTRUMENTS))])
    service = MarketDataIngestionService(
        lambda: session,
        AsyncMock(),
        Settings(database_url="postgresql://user:pass@localhost:5432/db"),
    )
    service.get_active_broker_client = AsyncMock()
    service.get_master_contracts = AsyncMock()

    synced = await service.sync_futures_snapshots(datetime(2026, 11, 25, 9, 50, tzinfo=IST).date())

    assert synced == 0
    service.get_active_broker_client.assert_not_awaited()
    service.get_master_contracts.assert_not_awaited()


@pytest.mark.asyncio
async def test_supertrend_intent_expires_at_matches_max_entry_delay(monkeypatch) -> None:
    # Build 12 completed 5-min candles where 09:45:00 flips SuperTrend from DOWN to UP.
    closes = [100, 99, 98, 97, 96, 95, 94, 93, 92, 91, 90, 115]
    candle_rows = (
        [
            {
                "candle_time": datetime(2026, 11, 24, 15, 0, tzinfo=IST)
                + timedelta(minutes=5 * idx),
                "open_price": close,
                "high_price": close + 2,
                "low_price": close - 2,
                "close_price": close,
            }
            for idx, close in enumerate(closes[:5])
        ]
        + [
            {
                "candle_time": datetime(2026, 11, 25, 9, 15, tzinfo=IST)
                + timedelta(minutes=5 * idx),
                "open_price": close,
                "high_price": close + 2,
                "low_price": close - 2,
                "close_price": close,
            }
            for idx, close in enumerate(closes[5:])
        ]
        + [
            {
                "candle_time": datetime(2026, 11, 25, 9, 50, tzinfo=IST),
                "open_price": 115,
                "high_price": 117,
                "low_price": 113,
                "close_price": 115,
            }
        ]
    )
    eval_now = datetime(2026, 11, 25, 9, 52, 0, tzinfo=IST)  # 120s after 09:50 close (>90s, <=180s)

    user_id = uuid4()
    session = _FakeSession(
        execute_results=[
            _FakeMappingsResult(candle_rows),  # SENSEX candles
            _FakeMappingsResult(
                [{"user_id": user_id, "lots": 1, "target_points": 40, "stop_loss_points": 25}]
            ),  # runners
            _FakeMappingsResult([]),  # existing_trade
            _FakeMappingsResult([]),  # last_trade
            _FakeMappingsResult([]),  # market_price_ticks upsert
            _FakeMappingsResult([]),  # empty for NIFTY candles
        ],
        scalar_values=[False, False],  # processed=False, active_intent=False
    )

    class _Provider:
        async def select(self, *, user_id, config, side, trade_date):
            return SuperTrendMarketSelection(
                contract=OptionContract(
                    "tok1", "SENSEXTESTCE", trade_date, 10, Decimal(80000), side, Decimal(150)
                ),
                underlying_ltp=Decimal(80000),
            )

    captured_intents = []

    class _FakeSignalRepo:
        def __init__(self, _sess):
            pass

        async def materialize(self, *, intents, **kwargs):
            captured_intents.extend(intents)
            return uuid4(), len(intents)

    class _FrozenDatetime(datetime):
        @classmethod
        def now(cls, tz=None):
            return eval_now if tz is None else eval_now.astimezone(tz)

    monkeypatch.setattr("app.runtime.lifecycle.datetime", _FrozenDatetime)
    monkeypatch.setattr("app.runtime.lifecycle.SignalRepository", _FakeSignalRepo)
    monkeypatch.setattr(
        AuthoritativeExecutionWorker,
        "_supertrend_snapshot",
        AsyncMock(return_value=uuid4()),
    )

    worker = AuthoritativeExecutionWorker(
        session_factory=MagicMock(),
        coordinator=MagicMock(),
        market_provider=_Provider(),
    )
    generated = await worker._evaluate_supertrend(session, eval_now)

    assert generated == 1
    assert session.executed[0][1]["end"] == datetime(2026, 11, 25, 9, 45, tzinfo=IST)
    assert len(captured_intents) == 1
    intent = captured_intents[0]
    expected_expiry = (
        datetime(2026, 11, 25, 9, 45, tzinfo=IST) + timedelta(minutes=5) + MAX_ENTRY_DELAY
    )
    assert intent.expires_at == expected_expiry
    assert intent.expires_at > eval_now


@pytest.mark.asyncio
async def test_supertrend_market_provider_reuses_client_and_batches_quotes() -> None:
    rows = [
        {
            "token": f"tok-{strike}",
            "symbol": f"NIFTY20AUG26{strike}CE",
            "name": "NIFTY",
            "expiry": "20AUG2026",
            "strike": str(strike * 100),
            "lotsize": "50",
            "instrumenttype": "OPTIDX",
            "exch_seg": "NFO",
        }
        for strike in (24900, 24950, 25000, 25050)
    ]

    async def master(_: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json=rows)

    quote_calls = []

    class BatchRest:
        async def quote(self, _mode, exchange_tokens):
            quote_calls.append(exchange_tokens)
            if "NSE" in exchange_tokens:
                return {
                    "data": {
                        "fetched": [{"symbolToken": CONFIGS["NIFTY"].index_token, "ltp": "25010"}]
                    }
                }
            return {
                "data": {
                    "fetched": [
                        {"symbolToken": "tok-24950", "ltp": "140"},
                        {"symbolToken": "tok-25000", "ltp": "105.5"},
                    ]
                }
            }

    clients_created = []

    async def client_factory(_user):
        client = MagicMock()
        client.rest = BatchRest()
        client.close = AsyncMock()
        clients_created.append(client)
        return client

    async with httpx.AsyncClient(transport=httpx.MockTransport(master)) as http:
        selected = await AngelSuperTrendMarketProvider(client_factory, http_client=http).select(
            user_id=uuid4(),
            config=CONFIGS["NIFTY"],
            side=OptionSide.CALL,
            trade_date=datetime(2026, 8, 19, tzinfo=IST).date(),
        )

    assert selected.contract.token == "tok-25000"
    assert selected.contract.premium == Decimal("105.5")
    assert len(clients_created) == 1
    clients_created[0].close.assert_awaited_once()
    assert len(quote_calls) == 2
