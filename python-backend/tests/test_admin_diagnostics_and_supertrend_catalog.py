"""Tests for SkipReporter persistence, app INFO logging, Admin diagnostics API, and SuperTrend line catalog."""

import json
import logging
from datetime import UTC, datetime, timedelta
from decimal import Decimal
from types import SimpleNamespace
from unittest.mock import AsyncMock
from uuid import uuid4

import pytest

from app.api.admin import diagnostics
from app.api.strategy import _catalog, fetch_supertrend_statuses
from app.dependencies import Principal
from app.main import _configure_app_logging
from app.runtime.lifecycle import SkipReporter
from app.strategy.common import IST, Candle
from app.strategy.supertrend import supertrend_points


class _FakeResult:
    def __init__(self, rows=None, scalar_value=None):
        self._rows = list(rows or [])
        self._scalar = scalar_value

    def mappings(self):
        return self

    def scalars(self):
        return self

    def all(self):
        return self._rows

    def first(self):
        return self._rows[0] if self._rows else None


class _FakeDb:
    def __init__(self, *, execute_results=None, scalar_values=None):
        self._execute_results = list(execute_results or [])
        self._scalar_values = list(scalar_values or [])
        self.executed: list[tuple[str, dict]] = []

    async def execute(self, stmt, params=None):
        self.executed.append((str(stmt), dict(params or {})))
        if self._execute_results:
            return self._execute_results.pop(0)
        return _FakeResult([])

    async def scalar(self, stmt, params=None):
        self.executed.append((str(stmt), dict(params or {})))
        if self._scalar_values:
            return self._scalar_values.pop(0)
        return 0


def test_configure_app_logging_enables_info_level() -> None:
    _configure_app_logging()
    assert logging.getLogger("app.runtime.lifecycle").isEnabledFor(logging.INFO)


@pytest.mark.asyncio
async def test_skip_reporter_flushes_info_and_warning_events_to_strategy_events() -> None:
    reporter = SkipReporter(repeat_seconds=300.0)
    assert reporter.report("supertrend_index_options_v1", "NIFTY", "no_flip", level=logging.DEBUG)
    assert reporter.report(
        "supertrend_index_options_v1",
        "NIFTY",
        "stale_candles",
        level=logging.WARNING,
        expected_bar="2026-11-26T09:45:00+05:30",
    )
    assert reporter.report(
        "supertrend_index_options_v1",
        "SENSEX",
        "all_runners_skipped",
        level=logging.INFO,
        alternation=2,
    )

    db = _FakeDb()
    flushed = await reporter.flush(db)
    assert flushed == 2
    assert await reporter.flush(db) == 0

    assert len(db.executed) == 2
    sql_0, params_0 = db.executed[0]
    assert "INSERT INTO strategy_events" in sql_0
    assert "'strategy_skip'" in sql_0
    payload_0 = json.loads(params_0["payload"])
    assert payload_0["severity"] == "WARNING"
    assert payload_0["reason"] == "stale_candles"
    assert payload_0["context"] == {"expected_bar": "2026-11-26T09:45:00+05:30"}

    _, params_1 = db.executed[1]
    payload_1 = json.loads(params_1["payload"])
    assert payload_1["severity"] == "INFO"
    assert payload_1["reason"] == "all_runners_skipped"
    assert payload_1["context"] == {"alternation": "2"}


@pytest.mark.asyncio
async def test_fetch_supertrend_statuses_returns_latest_completed_line() -> None:
    closes = [100, 99, 98, 97, 96, 95, 94, 93, 92, 91, 90, 115]
    base = datetime(2026, 11, 26, 9, 15, tzinfo=IST)
    candle_rows = [
        {
            "exchange": "NSE",
            "symbol_token": "99926000",
            "candle_time": base - timedelta(minutes=5),  # pre-open, must be excluded
            "open_price": 100,
            "high_price": 101,
            "low_price": 99,
            "close_price": 100,
        }
    ] + [
        {
            "exchange": "NSE",
            "symbol_token": "99926000",
            "candle_time": base + timedelta(minutes=5 * idx),
            "open_price": close,
            "high_price": close + 2,
            "low_price": close - 2,
            "close_price": close,
        }
        for idx, close in enumerate(closes)
    ]
    expected_points = supertrend_points(
        [
            Candle(
                row["candle_time"],
                Decimal(str(row["open_price"])),
                Decimal(str(row["high_price"])),
                Decimal(str(row["low_price"])),
                Decimal(str(row["close_price"])),
            )
            for row in candle_rows[1:]
        ]
    )

    db = _FakeDb(execute_results=[_FakeResult(candle_rows)])
    now_ist = base + timedelta(minutes=5 * len(closes) + 1)
    statuses = await fetch_supertrend_statuses(db, now=now_ist.astimezone(UTC))

    assert "NIFTY" in statuses
    assert "SENSEX" not in statuses
    nifty = statuses["NIFTY"]
    assert nifty["supertrend_price"] == round(float(expected_points[-1].value), 2)
    assert nifty["supertrend_direction"] == expected_points[-1].direction
    assert nifty["index_close"] == 115.0
    assert nifty["supertrend_candle_at"] == expected_points[-1].candle.ist.isoformat()


@pytest.mark.asyncio
async def test_catalog_exposes_supertrend_line_fields(monkeypatch) -> None:
    monkeypatch.setattr(
        "app.api.strategy.fetch_supertrend_statuses",
        AsyncMock(
            return_value={
                "NIFTY": {
                    "supertrend_price": 24185.45,
                    "supertrend_direction": "UP",
                    "supertrend_candle_at": "2026-11-26T10:10:00+05:30",
                    "index_close": 24240.1,
                }
            }
        ),
    )
    db = _FakeDb(
        execute_results=[
            _FakeResult([]),  # activations
            _FakeResult([]),  # configs
            _FakeResult([]),  # snapshots
            _FakeResult([]),  # breakout_alerts
            _FakeResult([]),  # runs
            _FakeResult([]),  # supertrend_alerts
            _FakeResult([]),  # opponent_alerts
        ],
        scalar_values=[1],  # connected_sessions
    )
    catalog = await _catalog(db, str(uuid4()))
    st = next(item for item in catalog if item["key"] == "supertrend_index_options_v1")
    nifty_snap = next(
        item["snapshot"] for item in st["instruments"] if item["instrument"] == "NIFTY"
    )
    assert nifty_snap["lot_size"] == 65
    assert nifty_snap["supertrend_price"] == 24185.45
    assert nifty_snap["supertrend_direction"] == "UP"
    assert nifty_snap["supertrend_candle_at"] == "2026-11-26T10:10:00+05:30"
    assert nifty_snap["index_close"] == 24240.1


@pytest.mark.asyncio
async def test_admin_diagnostics_endpoint_returns_events_and_levels(monkeypatch) -> None:
    monkeypatch.setattr(
        "app.api.strategy.fetch_supertrend_statuses",
        AsyncMock(
            return_value={
                "SENSEX": {
                    "supertrend_price": 80120.5,
                    "supertrend_direction": "DOWN",
                    "supertrend_candle_at": "2026-11-26T10:10:00+05:30",
                    "index_close": 80010.0,
                }
            }
        ),
    )
    created_at = datetime(2026, 11, 26, 4, 42, tzinfo=UTC)
    db = _FakeDb(
        execute_results=[
            _FakeResult(
                [
                    {
                        "id": 101,
                        "strategy_key": "supertrend_index_options_v1",
                        "instrument": "NIFTY",
                        "event_type": "strategy_skip",
                        "payload": {
                            "severity": "WARNING",
                            "reason": "stale_candles",
                            "message": "stale_candles (latest=09:40)",
                            "context": {"latest": "09:40"},
                        },
                        "created_at": created_at,
                        "username": None,
                    },
                    {
                        "id": 100,
                        "strategy_key": "supertrend_index_options_v1",
                        "instrument": "SENSEX",
                        "event_type": "strategy_skip",
                        "payload": {
                            "severity": "INFO",
                            "reason": "all_runners_skipped",
                            "message": "all_runners_skipped (alternation=1)",
                            "context": {"alternation": "1"},
                        },
                        "created_at": created_at - timedelta(minutes=5),
                        "username": None,
                    },
                ]
            )
        ]
    )
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(production_runtime=None)))
    admin = Principal(
        id=str(uuid4()),
        username="ADMIN",
        can_administer=True,
        can_live_trade=False,
        can_backtest=True,
        can_backtest_on_trading_days=True,
        trading_mode="demo",
        session_id="sess-1",
        csrf_hash=b"csrf",
    )

    result = await diagnostics(
        request=request,
        date=datetime(2026, 11, 26, tzinfo=IST).date(),
        strategy_key=None,
        severity=None,
        limit=200,
        _=admin,
        db=db,
    )

    assert result["summary"] == {
        "total": 2,
        "warnings": 1,
        "errors": 0,
        "info": 1,
        "by_reason": {"stale_candles": 1, "all_runners_skipped": 1},
    }
    assert result["supertrend_levels"]["SENSEX"]["supertrend_price"] == 80120.5
    assert len(result["events"]) == 2
    assert result["events"][0]["reason"] == "stale_candles"
