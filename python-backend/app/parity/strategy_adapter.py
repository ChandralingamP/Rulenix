from __future__ import annotations

from datetime import datetime, timedelta, timezone
from decimal import Decimal
from typing import Any

from app.strategy import (
    Candle,
    calculate_levels,
    exit_levels_for_entry,
    missed_entry_plan,
    normalize_to_tick,
    supertrend_entry_allowed,
    supertrend_eod_due,
    supertrend_points,
)


def _number(value: Decimal | float | None) -> float | None:
    return None if value is None else float(value)


def _future(request: dict[str, Any]) -> dict[str, Any]:
    levels = calculate_levels(request["highs"], request["lows"])
    body: dict[str, Any] = {
        "hh2": _number(levels.hh2),
        "ll2": _number(levels.ll2),
        "hh4": _number(levels.hh4),
        "ll4": _number(levels.ll4),
        "buy_entry": _number(levels.buy_entry),
        "buy_target": _number(levels.buy_target),
        "buy_sl1": _number(levels.buy_sl1),
        "buy_sl2": _number(levels.buy_sl2),
        "sell_entry": _number(levels.sell_entry),
        "sell_target": _number(levels.sell_target),
        "sell_sl1": _number(levels.sell_sl1),
        "sell_sl2": _number(levels.sell_sl2),
    }
    if "market_open" in request:
        plan = missed_entry_plan(request["market_open"], levels.buy_entry, levels.sell_entry)
        body.update({"missed_entry": plan.label, "buy_missed": plan.buy_missed, "sell_missed": plan.sell_missed})
    if "direction" in request:
        exits = exit_levels_for_entry(request["direction"], request["entry"], levels.hh2, levels.ll2, levels.hh4, levels.ll4)
        body["exit"] = {"target": _number(exits.target), "sl1": _number(exits.sl1), "sl2": _number(exits.sl2)}
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
