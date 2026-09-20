"""Pure DEMO lifecycle oracle comparison with no broker dependency."""

from __future__ import annotations

from decimal import ROUND_HALF_UP, Decimal
from typing import Any

from .models import DemoDecision, DemoScenario, ExitKind

MONEY = Decimal("0.01")


def _money(value: Decimal) -> Decimal:
    return value.quantize(MONEY, rounding=ROUND_HALF_UP)


def _pnl(side: str, entry: Decimal, exit_price: Decimal, quantity: int) -> Decimal:
    movement = exit_price - entry if side == "BUY" else entry - exit_price
    return _money(movement * quantity)


def evaluate_scenario(scenario: DemoScenario) -> DemoDecision:
    """Evaluate one server-owned DEMO scenario and its complete terminal state."""
    if scenario.execution_mode != "demo":
        raise ValueError("Phase 12 trial accepts server-owned DEMO mode only")
    if scenario.side not in {"BUY", "SELL"}:
        raise ValueError("side must be BUY or SELL")
    if scenario.quantity <= 0:
        raise ValueError("quantity must be positive")
    if scenario.global_kill or scenario.user_kill or scenario.exit_kind is ExitKind.KILL_SWITCH:
        return DemoDecision(
            False,
            "global_kill_switch" if scenario.global_kill else "user_kill_switch",
            0,
            0,
            0,
            0,
            None,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            Decimal("0.00"),
        )
    if scenario.exit_kind is ExitKind.NO_SIGNAL:
        return DemoDecision(
            True,
            "no_signal",
            0,
            0,
            0,
            0,
            None,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            Decimal("0.00"),
        )
    if scenario.exit_kind is ExitKind.EOD_NO_POSITION:
        return DemoDecision(
            True,
            "eod_no_open_demo_position",
            1,
            0,
            0,
            0,
            None,
            0,
            None,
            None,
            None,
            None,
            "EOD",
            None,
            Decimal("0.00"),
        )
    if min(scenario.entry, scenario.target, scenario.sl1, scenario.sl2) <= 0:
        raise ValueError("price levels must be positive")
    expected_exit = {
        ExitKind.TARGET: scenario.target,
        ExitKind.SL1: scenario.sl1,
        ExitKind.SL2: scenario.sl2,
        ExitKind.EOD: scenario.exit_price,
        ExitKind.MANUAL_CLOSE: scenario.exit_price,
    }[scenario.exit_kind]
    if expected_exit is None or expected_exit <= 0:
        raise ValueError("terminal exit price is required")
    reversal_side: str | None = None
    reversal_entry: Decimal | None = None
    reversal_exit: Decimal | None = None
    reversal_pnl = Decimal("0.00")
    trade_count = 1
    intent_count = 2
    order_count = 4 if scenario.strategy_key == "futures_breakout_v3" else 3
    if scenario.reversal:
        if scenario.exit_kind is not ExitKind.SL2 or scenario.reversal_exit_price is None:
            raise ValueError("reversal requires a filled SL2 and terminal reversal price")
        reversal_side = "SELL" if scenario.side == "BUY" else "BUY"
        reversal_entry = expected_exit
        reversal_exit = scenario.reversal_exit_price
        reversal_pnl = _pnl(reversal_side, reversal_entry, reversal_exit, scenario.quantity)
        trade_count += 1
        intent_count += 2
        order_count += 4 if scenario.strategy_key == "futures_breakout_v3" else 3
    return DemoDecision(
        True,
        "allowed",
        1,
        intent_count,
        order_count,
        trade_count,
        scenario.side,
        scenario.quantity,
        scenario.entry,
        scenario.target,
        scenario.sl1,
        scenario.sl2,
        scenario.exit_kind.value,
        expected_exit,
        _pnl(scenario.side, scenario.entry, expected_exit, scenario.quantity),
        reversal_side,
        reversal_entry,
        reversal_exit,
        reversal_pnl,
    )


def compare_decisions(
    oracle: dict[str, Any], actual: dict[str, Any]
) -> tuple[str, str]:
    """Compare all material lifecycle fields without financial normalization."""
    keys = sorted(set(oracle) | set(actual))
    different = [key for key in keys if oracle.get(key) != actual.get(key)]
    return ("MATCH", "") if not different else ("MISMATCH", "fields: " + ",".join(different))


__all__ = ["compare_decisions", "evaluate_scenario"]
