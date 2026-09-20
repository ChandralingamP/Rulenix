from __future__ import annotations

from datetime import UTC, datetime, timedelta
from decimal import Decimal
from pathlib import Path

import pytest

from app.shadow.config import ShadowSettings
from app.shadow.evaluate import (
    evaluate_futures_signal,
    evaluate_readiness,
    evaluate_supertrend_signal,
)
from app.shadow.service import HealthState, ShadowObserver
from app.strategy.common import IST, Candle
from app.strategy.futures_breakout import calculate_levels
from app.strategy.supertrend import current_signal, supertrend_points

ROOT = Path(__file__).parents[2]


def test_shadow_health_fails_closed_after_poll_error() -> None:
    health = HealthState(last_success_at=datetime.now(UTC))
    assert health.ready(15)
    health.consecutive_failures = 1
    health.last_error = "InsufficientPrivilegeError"
    assert not health.ready(15)


def test_shadow_settings_require_distinct_narrow_identities(tmp_path: Path, monkeypatch) -> None:
    source = tmp_path / "source"
    sink = tmp_path / "sink"
    key = tmp_path / "key"
    source.write_text("postgresql://rulenix_shadow_reader:x@shadow-db-proxy/rulenix")
    sink.write_text("postgresql://rulenix_shadow_writer:y@shadow-db-proxy/rulenix")
    key.write_text("a" * 32)
    monkeypatch.setenv("RULENIX_SHADOW_ONLY", "required")
    monkeypatch.setenv("SHADOW_SOURCE_DATABASE_URL_FILE", str(source))
    monkeypatch.setenv("SHADOW_SINK_DATABASE_URL_FILE", str(sink))
    monkeypatch.setenv("SHADOW_PSEUDONYM_KEY_FILE", str(key))
    monkeypatch.setenv("SHADOW_RELEASE", "phase11-test")
    settings = ShadowSettings.from_environment()
    assert settings.poll_seconds == 15

    source.write_text("postgresql://rulenix:x@shadow-db-proxy/rulenix")
    with pytest.raises(ValueError, match="rulenix_shadow_reader"):
        ShadowSettings.from_environment()


def test_futures_shadow_matches_authoritative_snapshot_and_intent() -> None:
    levels = calculate_levels([100, 110, 105, 108], [90, 92, 94, 93])
    row = {
        "instrument": "GOLDTEN",
        "contract_symbol": "GOLDTEN30SEP26FUT",
        "entry_direction": "BOTH",
        "planned_entry": None,
        "lot_size": 10,
        "highs": [100, 110, 105, 108],
        "lows": [90, 92, 94, 93],
        **{name: getattr(levels, name) for name in levels.__dataclass_fields__},
        "intents": [
            {
                "account_ref": "a" * 64,
                "role": "BUY_ENTRY",
                "side": "BUY",
                "lots": 2,
                "quantity": 20,
                "price": levels.buy_entry,
            }
        ],
    }
    _, _, classification, reason, severity = evaluate_futures_signal(row)
    assert (classification, reason, severity) == ("MATCH", "", "NONE")

    row["intents"][0]["side"] = "SELL"
    _, _, classification, reason, severity = evaluate_futures_signal(row)
    assert classification == "MISMATCH"
    assert "intents" in reason
    assert severity == "CRITICAL"


def test_supertrend_shadow_recomputes_signal_from_rust_cached_candles() -> None:
    start = datetime(2026, 9, 9, 9, 15, tzinfo=IST)
    closes = [100, 99, 98, 97, 96, 95, 94, 93, 92, 91, 90, 91, 92, 93, 118]
    candles = [
        Candle(
            start + timedelta(minutes=index * 5),
            Decimal(close),
            Decimal(close + 2),
            Decimal(close - 2),
            Decimal(close),
        )
        for index, close in enumerate(closes)
    ]
    signal = current_signal(supertrend_points(candles), candles[-1].at + timedelta(minutes=5))
    assert signal is not None
    row = {
        "signal_at": signal.signal_at,
        "instrument": "SENSEX",
        "payload": {
            "side": signal.side.value,
            "index_close": signal.index_close,
            "supertrend": signal.supertrend,
            "contract_symbol": "SENSEX_TEST_CE",
        },
        "event_payload": {
            "previous_direction": signal.previous_direction,
            "direction": signal.direction,
        },
        "candles": [
            {
                "at": candle.at,
                "open": candle.open,
                "high": candle.high,
                "low": candle.low,
                "close": candle.close,
            }
            for candle in candles
        ],
    }
    _, _, classification, reason, severity = evaluate_supertrend_signal(row)
    assert (classification, reason, severity) == ("MATCH", "", "NONE")


def test_stale_reconciliation_is_high_mismatch() -> None:
    observed = datetime.now(UTC)
    row = {
        "healthy": True,
        "checked_at": observed - timedelta(minutes=6),
        "broker_credential_revision": 7,
        "current_credential_revision": 7,
        "blockers": 0,
    }
    _, python, classification, reason, severity = evaluate_readiness(row, observed)
    assert python["ready"] is False
    assert classification == "MISMATCH"
    assert "ready" in reason
    assert severity == "HIGH"


@pytest.mark.asyncio
async def test_observer_deduplicates_readiness_rows_between_polls() -> None:
    class Sink:
        writes = 0

        async def store(self, _observation) -> bool:
            self.writes += 1
            return True

    observer = ShadowObserver(
        ShadowSettings("postgresql://source", "postgresql://sink", b"x" * 32, "phase11-test")
    )
    sink = Sink()
    row = {
        "account_ref": "a" * 64,
        "healthy": True,
        "checked_at": datetime.now(UTC),
        "broker_credential_revision": 7,
        "current_credential_revision": 7,
        "blockers": 0,
    }

    await observer._observe_readiness(sink, row)  # type: ignore[arg-type]
    await observer._observe_readiness(sink, row)  # type: ignore[arg-type]

    assert sink.writes == 1


def test_shadow_image_and_network_exclude_authoritative_and_broker_capabilities() -> None:
    dockerfile = (ROOT / "python-backend" / "Dockerfile.shadow").read_text(encoding="utf-8")
    requirements = (ROOT / "python-backend" / "requirements-shadow.txt").read_text(
        encoding="utf-8"
    )
    compose = (ROOT / "docker-compose.shadow.yml").read_text(encoding="utf-8")
    provisioning = (ROOT / "scripts" / "phase11-provision-shadow.sql").read_text(
        encoding="utf-8"
    )
    gitignore = (ROOT / ".gitignore").read_text(encoding="utf-8")

    assert "COPY ." not in dockerfile
    for forbidden in ("app/api", "app/broker", "app/trading", "app/reconciliation"):
        assert forbidden not in dockerfile
    assert requirements.strip() == "asyncpg==0.31.0"
    assert "shadow_internal:\n    internal: true" in compose
    assert "EGRESS_HELPER_SOCKET" not in compose
    assert "ANGEL" not in compose
    assert "ports:" not in compose
    assert "secrets/" in gitignore
    assert "default_transaction_read_only=on" in provisioning
    assert "ON rulenix_shadow.observations TO rulenix_shadow_writer" in provisioning
    assert "GRANT INSERT ON public" not in provisioning
    assert "GRANT UPDATE ON public" not in provisioning
    assert "GRANT DELETE ON public" not in provisioning
