"""Unit and integration tests for MarketDataIngestionService."""

from datetime import UTC, date, datetime
from unittest.mock import AsyncMock, MagicMock
from uuid import uuid4

import httpx
import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from app.config import get_settings
from app.main import app, lifespan
from app.runtime.market_data import (
    MarketDataIngestionService,
    select_futures_contract,
    weekdays_until,
)


@pytest.fixture
async def app_client():
    async with lifespan(app):
        transport = httpx.ASGITransport(app=app)
        async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
            yield client


def test_weekdays_until():
    assert weekdays_until(date(2026, 10, 5), date(2026, 10, 5)) == 0  # Monday to Monday
    assert weekdays_until(date(2026, 10, 5), date(2026, 10, 6)) == 1  # Monday to Tuesday
    assert weekdays_until(date(2026, 10, 2), date(2026, 10, 5)) == 1  # Friday to Monday (skips Sat, Sun)
    assert weekdays_until(date(2026, 10, 5), date(2026, 10, 19)) == 10 # 2 weeks


def test_select_futures_contract():
    contracts = [
        # Near expiry (< 10 weekdays)
        {"exch_seg": "MCX", "name": "GOLDTEN", "instrumenttype": "FUTCOM", "expiry": "08OCT2026", "token": "tok1", "symbol": "GOLDTEN08OCT26FUT", "lotsize": "10"},
        # Target expiry (>= 10 weekdays)
        {"exch_seg": "MCX", "name": "GOLDTEN", "instrumenttype": "FUTCOM", "expiry": "30OCT2026", "token": "tok2", "symbol": "GOLDTEN30OCT26FUT", "lotsize": "10"},
        # Far expiry
        {"exch_seg": "MCX", "name": "GOLDTEN", "instrumenttype": "FUTCOM", "expiry": "27NOV2026", "token": "tok3", "symbol": "GOLDTEN27NOV26FUT", "lotsize": "10"},
        # Different instrument
        {"exch_seg": "MCX", "name": "GOLDM", "instrumenttype": "FUTCOM", "expiry": "30OCT2026", "token": "tok4", "symbol": "GOLDM30OCT26FUT", "lotsize": "100"},
    ]
    selected = select_futures_contract(contracts, "GOLDTEN", date(2026, 10, 5))
    assert selected is not None
    assert selected["token"] == "tok2"
    assert selected["symbol"] == "GOLDTEN30OCT26FUT"


@pytest.mark.asyncio
async def test_sync_futures_snapshots_and_supertrend_candles(app_client):
    session_factory: async_sessionmaker[AsyncSession] = app.state.session_factory
    settings = get_settings()

    mock_client = MagicMock()
    mock_client.rest = MagicMock()

    # Daily candles for Gold/Silver
    mock_client.rest.candles = AsyncMock(side_effect=lambda exch, token, interval, from_date, to_date: [
        ["2026-09-30T00:00:00+05:30", 150000.0, 151000.0, 149000.0, 150500.0, 100],
        ["2026-10-01T00:00:00+05:30", 150500.0, 152000.0, 150000.0, 151500.0, 120],
        ["2026-10-02T00:00:00+05:30", 151500.0, 151800.0, 149500.0, 150200.0, 110],
        ["2026-10-05T00:00:00+05:30", 150200.0, 153000.0, 150100.0, 152800.0, 130],
    ] if interval == "ONE_DAY" else [
        ["2026-10-06T09:15:00+05:30", 22600.0, 22650.0, 22580.0, 22620.0, 1000],
        ["2026-10-06T09:20:00+05:30", 22620.0, 22680.0, 22610.0, 22670.0, 1200],
    ])

    mock_client.rest.quote = AsyncMock(return_value={"status": True, "data": {"open": 152500.0}})

    mock_factory = AsyncMock(return_value=mock_client)

    service = MarketDataIngestionService(session_factory, mock_factory, settings)

    # Mock get_active_broker_client directly to avoid DB dependency on real active tokens
    dummy_user_id = uuid4()
    service.get_active_broker_client = AsyncMock(return_value=(dummy_user_id, mock_client))

    # Mock get_master_contracts
    service.get_master_contracts = AsyncMock(return_value=[
        {"exch_seg": "MCX", "name": "GOLDTEN", "instrumenttype": "FUTCOM", "expiry": "30OCT2026", "token": "571307", "symbol": "GOLDTEN30OCT26FUT", "lotsize": "10"},
        {"exch_seg": "MCX", "name": "GOLDM", "instrumenttype": "FUTCOM", "expiry": "05NOV2026", "token": "571445", "symbol": "GOLDM05NOV26FUT", "lotsize": "100"},
        {"exch_seg": "MCX", "name": "SILVERM", "instrumenttype": "FUTCOM", "expiry": "30NOV2026", "token": "571555", "symbol": "SILVERM30NOV26FUT", "lotsize": "5"},
        {"exch_seg": "MCX", "name": "SILVERMIC", "instrumenttype": "FUTCOM", "expiry": "30NOV2026", "token": "571666", "symbol": "SILVERMIC30NOV26FUT", "lotsize": "1"},
        {"exch_seg": "MCX", "name": "NATGASMINI", "instrumenttype": "FUTCOM", "expiry": "28OCT2026", "token": "571777", "symbol": "NATGASMINI28OCT26FUT", "lotsize": "250"},
    ])

    target_date = date(2026, 10, 6)
    synced_futures = await service.sync_futures_snapshots(target_date)
    assert synced_futures == 5

    # Verify snapshots in DB
    async with session_factory() as session:
        rows = (await session.execute(
            text("SELECT instrument, status, lot_size, buy_entry, sell_entry, gap_plan_status FROM strategy_market_snapshots WHERE trade_date=:date"),
            {"date": target_date},
        )).mappings().all()
        assert len(rows) == 5
        instruments = {row["instrument"] for row in rows}
        assert instruments == {"GOLDTEN", "GOLDM", "SILVERM", "SILVERMIC", "NATGASMINI"}
        for r in rows:
            assert r["status"] == "ready"
            assert r["gap_plan_status"] in ("READY", "WAITING_RANGE")
            assert float(r["buy_entry"]) > 0
            assert float(r["sell_entry"]) > 0

    # Sync SuperTrend candles
    candles_count = await service.sync_supertrend_candles(days=1)
    assert candles_count > 0

    # Verify candles in DB
    async with session_factory() as session:
        candle_rows = (await session.execute(
            text("SELECT COUNT(*) FROM backtest_market_candles WHERE interval_key='FIVE_MINUTE' AND candle_time >= :start"),
            {"start": datetime(2026, 10, 6, 0, 0, tzinfo=UTC)},
        )).scalar()
        assert candle_rows > 0
