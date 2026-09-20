"""Typed inputs and outputs for the isolated DEMO trial."""

from __future__ import annotations

from dataclasses import asdict, dataclass
from decimal import Decimal
from enum import StrEnum
from typing import Any


class ExitKind(StrEnum):
    NO_SIGNAL = "NO_SIGNAL"
    TARGET = "TARGET"
    SL1 = "SL1"
    SL2 = "SL2"
    EOD = "EOD"
    EOD_NO_POSITION = "EOD_NO_POSITION"
    MANUAL_CLOSE = "MANUAL_CLOSE"
    KILL_SWITCH = "KILL_SWITCH"


@dataclass(frozen=True)
class DemoScenario:
    scenario_id: str
    strategy_key: str
    instrument: str
    side: str
    quantity: int
    entry: Decimal
    target: Decimal
    sl1: Decimal
    sl2: Decimal
    exit_kind: ExitKind
    exit_price: Decimal | None = None
    reversal: bool = False
    reversal_exit_price: Decimal | None = None
    global_kill: bool = False
    user_kill: bool = False
    execution_mode: str = "demo"


@dataclass(frozen=True)
class DemoDecision:
    allowed: bool
    reason: str
    signal_count: int
    intent_count: int
    order_count: int
    trade_count: int
    side: str | None
    quantity: int
    entry: Decimal | None
    target: Decimal | None
    sl1: Decimal | None
    sl2: Decimal | None
    exit_reason: str | None
    exit_price: Decimal | None
    pnl: Decimal
    reversal_side: str | None = None
    reversal_entry: Decimal | None = None
    reversal_exit: Decimal | None = None
    reversal_pnl: Decimal = Decimal("0.00")

    def json(self) -> dict[str, Any]:
        return {
            key: format(value, "f") if isinstance(value, Decimal) else value
            for key, value in asdict(self).items()
        }


__all__ = ["DemoDecision", "DemoScenario", "ExitKind"]
