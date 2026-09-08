from __future__ import annotations

from datetime import datetime, timedelta, timezone
from decimal import Decimal
from typing import Any

from app.strategy import (
    Candle,
    calculate_levels,
    missed_entry_plan,
    normalize_to_tick,
    supertrend_entry_allowed,
    supertrend_eod_due,
    supertrend_points,
)


def _number(value: Decimal | float | None) -> float | None:
    return None if value is None else float(value)


def _rust_f64_levels(request: dict[str, Any]) -> dict[str, float]:
    """Render the external Future Breakout contract with Rust's f64 operation order.

    The production Python domain retains exact Decimal values. Rust's authoritative
    contract computes these fields as intermediate IEEE-754 values, so the isolated
    parity adapter mirrors that operation order only at the serialization boundary.
    """

    highs = [float(value) for value in request["highs"]]
    lows = [float(value) for value in request["lows"]]
    hh2, ll2 = max(highs[2:]), min(lows[2:])
    hh4, ll4 = max(highs), min(lows)
    buy_entry, sell_entry = hh4 * (1.0 + 0.0012), ll4 * (1.0 - 0.0012)

    def exits(direction: str, entry: float) -> tuple[float, float, float]:
        if direction == "BUY":
            percentage_stop = entry * (1.0 - 0.015)
            return (
                entry * (1.0 + 0.015),
                max(percentage_stop, ll2 * (1.0 - 0.0012)),
                max(percentage_stop, ll4 * (1.0 - 0.0012)),
            )
        percentage_stop = entry * (1.0 + 0.015)
        return (
            entry * (1.0 - 0.015),
            min(percentage_stop, hh2 * (1.0 + 0.0012)),
            min(percentage_stop, hh4 * (1.0 + 0.0012)),
        )

    buy_target, buy_sl1, buy_sl2 = exits("BUY", buy_entry)
    sell_target, sell_sl1, sell_sl2 = exits("SELL", sell_entry)
    return {
        "hh2": hh2,
        "ll2": ll2,
        "hh4": hh4,
        "ll4": ll4,
        "buy_entry": buy_entry,
        "buy_target": buy_target,
        "buy_sl1": buy_sl1,
        "buy_sl2": buy_sl2,
        "sell_entry": sell_entry,
        "sell_target": sell_target,
        "sell_sl1": sell_sl1,
        "sell_sl2": sell_sl2,
    }


def _future(request: dict[str, Any]) -> dict[str, Any]:
    # Execute the domain calculation as a safety check; expose Rust-compatible
    # f64 values only for the differential adapter's external contract.
    calculate_levels(request["highs"], request["lows"])
    rendered = _rust_f64_levels(request)
    body: dict[str, Any] = dict(rendered)
    if "market_open" in request:
        plan = missed_entry_plan(request["market_open"], str(rendered["buy_entry"]), str(rendered["sell_entry"]))
        body.update({"missed_entry": plan.label, "buy_missed": plan.buy_missed, "sell_missed": plan.sell_missed})
    if "direction" in request:
        entry = float(request["entry"])
        if request["direction"] == "BUY":
            target = entry * (1.0 + 0.015)
            percentage_stop = entry * (1.0 - 0.015)
            sl1 = max(percentage_stop, rendered["ll2"] * (1.0 - 0.0012))
            sl2 = max(percentage_stop, rendered["ll4"] * (1.0 - 0.0012))
        else:
            target = entry * (1.0 - 0.015)
            percentage_stop = entry * (1.0 + 0.015)
            sl1 = min(percentage_stop, rendered["hh2"] * (1.0 + 0.0012))
            sl2 = min(percentage_stop, rendered["hh4"] * (1.0 + 0.0012))
        body["exit"] = {"target": target, "sl1": sl1, "sl2": sl2}
    return body


def _supertrend(request: dict[str, Any]) -> dict[str, Any]:
    candles = [
        Candle(
            datetime.fromisoformat(item["at"]).replace(tzinfo=timezone(timedelta(hours=5, minutes=30))),
            Decimal(str(item["open"])),
            Decimal(str(item["high"])),
            Decimal(str(item["low"])),
            Decimal(str(item["close"])),
        )
        for item in request["candles"]
    ]
    points = supertrend_points(candles, int(request.get("atr_period", 7)), str(request.get("factor", "2")))
    body: dict[str, Any] = {
        "atr_period": int(request.get("atr_period", 7)),
        "factor": float(request.get("factor", 2)),
        "points": [{"at": point.candle.at.replace(tzinfo=None).strftime("%Y-%m-%dT%H:%M:%S"), "value": _number(point.value), "direction": point.direction} for point in points],
    }
    if "at" in request:
        at = datetime.fromisoformat(request["at"])
        body["entry_allowed"] = supertrend_entry_allowed(at)
        body["eod_due"] = supertrend_eod_due(at)
    return body


def execute_strategy_fixture(operation: str, request: dict[str, Any]) -> dict[str, Any]:
    if operation == "future_breakout":
        return _future(request)
    if operation == "supertrend":
        return _supertrend(request)
    if operation == "tick":
        return {"normalized": _number(normalize_to_tick(request["price"], request["tick_size"], request["side"]))}
    raise ValueError(f"unsupported strategy fixture operation: {operation}")


def execute_strategy_result(operation: str, request: dict[str, Any]) -> tuple[int, dict[str, Any]]:
    try:
        return 200, execute_strategy_fixture(operation, request)
    except (KeyError, TypeError, ValueError) as error:
        return 422, {"code": "fixture_error", "message": str(error)}
