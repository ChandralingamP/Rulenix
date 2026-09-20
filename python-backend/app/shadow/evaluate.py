"""Pure parity evaluators fed only by Rust-persisted observable state."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from decimal import Decimal
from typing import Any

from app.strategy.common import IST, Candle
from app.strategy.futures_breakout import calculate_levels
from app.strategy.supertrend import current_signal, supertrend_points


def _decimal(value: Any) -> Decimal | None:
    if value is None:
        return None
    return Decimal(str(value))


def _render(value: Any) -> Any:
    if isinstance(value, Decimal):
        return format(value, "f")
    if isinstance(value, datetime):
        return value.astimezone(UTC).isoformat()
    if isinstance(value, list):
        return [_render(item) for item in value]
    if isinstance(value, dict):
        return {key: _render(item) for key, item in value.items()}
    return value


def _compare(rust: dict[str, Any], python: dict[str, Any]) -> tuple[str, str, str]:
    mismatches = [key for key in sorted(rust) if _render(rust[key]) != _render(python.get(key))]
    if not mismatches:
        return "MATCH", "", "NONE"
    critical = any(key in {"side", "quantity", "instrument"} for key in mismatches)
    if "intents" in mismatches:
        rust_intents = rust.get("intents") or []
        python_intents = python.get("intents") or []
        critical = len(rust_intents) != len(python_intents) or any(
            actual.get(field) != expected.get(field)
            for actual, expected in zip(rust_intents, python_intents, strict=False)
            for field in ("account_ref", "role", "side", "quantity")
        )
    severity = "CRITICAL" if critical else "HIGH"
    return "MISMATCH", "fields: " + ",".join(mismatches), severity


def evaluate_futures_signal(row: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any], str, str, str]:
    levels = calculate_levels(list(row["highs"]), list(row["lows"]))
    fields = (
        "hh2",
        "ll2",
        "hh4",
        "ll4",
        "buy_entry",
        "buy_target",
        "buy_sl1",
        "buy_sl2",
        "sell_entry",
        "sell_target",
        "sell_sl1",
        "sell_sl2",
    )
    rust: dict[str, Any] = {name: _decimal(row.get(name)) for name in fields}
    python: dict[str, Any] = {name: getattr(levels, name) for name in fields}
    rust.update(
        {
            "instrument": row["instrument"],
            "contract_symbol": row.get("contract_symbol"),
            "entry_direction": row.get("entry_direction"),
        }
    )
    python.update(
        {
            "instrument": row["instrument"],
            "contract_symbol": row.get("contract_symbol"),
            "entry_direction": row.get("entry_direction"),
        }
    )
    rust_intents: list[dict[str, Any]] = []
    python_intents: list[dict[str, Any]] = []
    planned_direction = str(row.get("entry_direction") or "")
    planned_entry = _decimal(row.get("planned_entry"))
    for intent in row.get("intents", []):
        role = str(intent["role"])
        expected_side = "BUY" if role == "BUY_ENTRY" else "SELL"
        expected_price = (
            planned_entry
            if planned_entry is not None and planned_direction == expected_side
            else getattr(levels, "buy_entry" if expected_side == "BUY" else "sell_entry")
        )
        rust_intents.append(
            {
                "account_ref": intent.get("account_ref"),
                "role": role,
                "side": intent["side"],
                "quantity": intent["quantity"],
                "price": _decimal(intent["price"]),
            }
        )
        python_intents.append(
            {
                "account_ref": intent.get("account_ref"),
                "role": role,
                "side": expected_side,
                "quantity": int(intent["lots"]) * int(row["lot_size"]),
                "price": expected_price,
            }
        )
    rust["intents"] = rust_intents
    python["intents"] = python_intents
    classification, reason, severity = _compare(rust, python)
    return _render(rust), _render(python), classification, reason, severity


def evaluate_supertrend_signal(row: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any], str, str, str]:
    signal_at: datetime = row["signal_at"]
    candles = [
        Candle(
            item["at"].astimezone(IST),
            _decimal(item["open"]) or Decimal(0),
            _decimal(item["high"]) or Decimal(0),
            _decimal(item["low"]) or Decimal(0),
            _decimal(item["close"]) or Decimal(0),
        )
        for item in row["candles"]
    ]
    signal = current_signal(supertrend_points(candles), signal_at + timedelta(minutes=5))
    payload = row["payload"]
    event = row.get("event_payload") or {}
    rust = {
        "side": payload.get("side"),
        "index_close": _decimal(payload.get("index_close")),
        "supertrend": _decimal(payload.get("supertrend")),
        "previous_direction": event.get("previous_direction"),
        "direction": event.get("direction"),
        "instrument": row["instrument"],
        "contract_symbol": payload.get("contract_symbol"),
    }
    python = {
        "side": signal.side.value if signal else None,
        "index_close": signal.index_close if signal else None,
        "supertrend": signal.supertrend if signal else None,
        "previous_direction": signal.previous_direction if signal else None,
        "direction": signal.direction if signal else None,
        "instrument": row["instrument"],
        "contract_symbol": payload.get("contract_symbol"),
    }
    classification, reason, severity = _compare(rust, python)
    return _render(rust), _render(python), classification, reason, severity


def evaluate_readiness(row: dict[str, Any], observed_at: datetime) -> tuple[dict[str, Any], dict[str, Any], str, str, str]:
    blockers = int(row.get("blockers", 0))
    owned = int(row.get("rulenix_owned_exposure", 0))
    ambiguous = int(row.get("ambiguous_exposure", 0))
    manual = int(row.get("manual_external_exposure", 0))
    broker_readable = bool(row["healthy"])
    broker_exposure_observed = owned + ambiguous + manual > 0
    deployment_safe = (
        blockers == 0 and owned == 0 and ambiguous == 0
        if broker_readable
        else blockers == 0 and not broker_exposure_observed
    )
    deployment_classification = (
        "readable_safe" if broker_readable and deployment_safe
        else "readable_rulenix_owned_exposure" if broker_readable and owned
        else "readable_ambiguous_exposure" if broker_readable and ambiguous
        else "readable_local_unresolved_live_state" if broker_readable
        else "offline_locally_flat" if deployment_safe
        else "unreadable_broker_exposure_observed" if broker_exposure_observed
        else "offline_with_unresolved_live_state"
    )
    checked_at: datetime = row["checked_at"]
    live_ready = (
        broker_readable
        and blockers == 0
        and owned == 0
        and ambiguous == 0
        and row.get("broker_credential_revision") == row.get("current_credential_revision")
        and checked_at >= observed_at - timedelta(minutes=5)
    )
    rust = {
        "ready": live_ready,
        "deployment_safe": deployment_safe,
        "deployment_classification": deployment_classification,
        "credential_revision": row.get("current_credential_revision"),
        "reconciliation_revision": row.get("broker_credential_revision"),
        "checked_at": row["checked_at"],
        "blockers": blockers,
        "rulenix_owned_exposure": owned,
        "ambiguous_exposure": ambiguous,
        "manual_external_exposure": manual,
    }
    python = {
        "ready": live_ready,
        "deployment_safe": deployment_safe,
        "deployment_classification": deployment_classification,
        "credential_revision": row.get("current_credential_revision"),
        "reconciliation_revision": row.get("broker_credential_revision"),
        "checked_at": checked_at,
        "blockers": blockers,
        "rulenix_owned_exposure": owned,
        "ambiguous_exposure": ambiguous,
        "manual_external_exposure": manual,
    }
    classification, reason, severity = _compare(rust, python)
    return _render(rust), _render(python), classification, reason, severity


__all__ = ["evaluate_futures_signal", "evaluate_readiness", "evaluate_supertrend_signal"]
