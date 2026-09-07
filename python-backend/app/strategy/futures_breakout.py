"""Pure ``futures_breakout_v3`` calculations ported from Rust strategy.rs."""

from dataclasses import dataclass
from decimal import Decimal

BUFFER = Decimal("0.0012")
TARGET_PCT = Decimal("0.015")
STOP_PCT = Decimal("0.015")
STRATEGY_KEY = "futures_breakout_v3"


@dataclass(frozen=True)
class FuturesExitLevels:
    target: Decimal
    sl1: Decimal
    sl2: Decimal


@dataclass(frozen=True)
class MissedEntryPlan:
    buy_missed: bool
    sell_missed: bool

    @property
    def label(self) -> str:
        return {
            (True, True): "BOTH_MISSED",
            (True, False): "BUY_MISSED",
            (False, True): "SELL_MISSED",
            (False, False): "NONE_MISSED",
        }[(self.buy_missed, self.sell_missed)]


@dataclass(frozen=True)
class FuturesLevels:
    hh2: Decimal
    ll2: Decimal
    hh4: Decimal
    ll4: Decimal
    buy_entry: Decimal
    buy_target: Decimal
    buy_sl1: Decimal
    buy_sl2: Decimal
    sell_entry: Decimal
    sell_target: Decimal
    sell_sl1: Decimal
    sell_sl2: Decimal


def _positive(values: tuple[Decimal, ...]) -> bool:
    return all(value.is_finite() and value > 0 for value in values)


def exit_levels_for_entry(
    direction: str,
    entry: Decimal | str,
    hh2: Decimal | str,
    ll2: Decimal | str,
    hh4: Decimal | str,
    ll4: Decimal | str,
) -> FuturesExitLevels:
    entry, hh2, ll2, hh4, ll4 = tuple(Decimal(str(v)) for v in (entry, hh2, ll2, hh4, ll4))
    if not _positive((entry, hh2, ll2, hh4, ll4)):
        raise ValueError("Futures level inputs must be positive finite values.")
    if direction == "BUY":
        percentage_stop = entry * (Decimal(1) - STOP_PCT)
        return FuturesExitLevels(
            entry * (Decimal(1) + TARGET_PCT),
            max(percentage_stop, ll2 * (Decimal(1) - BUFFER)),
            max(percentage_stop, ll4 * (Decimal(1) - BUFFER)),
        )
    if direction == "SELL":
        percentage_stop = entry * (Decimal(1) + STOP_PCT)
        return FuturesExitLevels(
            entry * (Decimal(1) - TARGET_PCT),
            min(percentage_stop, hh2 * (Decimal(1) + BUFFER)),
            min(percentage_stop, hh4 * (Decimal(1) + BUFFER)),
        )
    raise ValueError("Direction must be BUY or SELL.")


def calculate_levels(highs: list[Decimal | str], lows: list[Decimal | str]) -> FuturesLevels:
    if len(highs) != 4 or len(lows) != 4:
        raise ValueError("Future Breakout requires exactly four historical candles.")
    hs, ls = [Decimal(str(v)) for v in highs], [Decimal(str(v)) for v in lows]
    if not _positive(tuple(hs + ls)):
        raise ValueError("Historical prices must be positive finite values.")
    hh2, ll2, hh4, ll4 = max(hs[2:]), min(ls[2:]), max(hs), min(ls)
    buy_entry, sell_entry = hh4 * (1 + BUFFER), ll4 * (1 - BUFFER)
    buy, sell = (
        exit_levels_for_entry("BUY", buy_entry, hh2, ll2, hh4, ll4),
        exit_levels_for_entry("SELL", sell_entry, hh2, ll2, hh4, ll4),
    )
    return FuturesLevels(
        hh2,
        ll2,
        hh4,
        ll4,
        buy_entry,
        buy.target,
        buy.sl1,
        buy.sl2,
        sell_entry,
        sell.target,
        sell.sl1,
        sell.sl2,
    )


def missed_entry_plan(
    market_open: Decimal | str, buy_entry: Decimal | str, sell_entry: Decimal | str
) -> MissedEntryPlan:
    market_open, buy_entry, sell_entry = tuple(
        Decimal(str(v)) for v in (market_open, buy_entry, sell_entry)
    )
    if not _positive((market_open, buy_entry, sell_entry)) or buy_entry <= sell_entry:
        raise ValueError("Invalid missed-entry levels.")
    return MissedEntryPlan(market_open >= buy_entry, market_open <= sell_entry)


def opening_range_entries(
    plan: MissedEntryPlan, opening_range_high: Decimal | str, opening_range_low: Decimal | str
) -> tuple[Decimal | None, Decimal | None]:
    high, low = Decimal(str(opening_range_high)), Decimal(str(opening_range_low))
    if not _positive((high, low)) or high < low:
        raise ValueError("Invalid opening range.")
    return (
        high * (1 + BUFFER) if plan.buy_missed else None,
        low * (1 - BUFFER) if plan.sell_missed else None,
    )


def classify_gap(market_open: Decimal | str, hh4: Decimal | str, ll4: Decimal | str) -> str:
    open_value, high, low = (Decimal(str(v)) for v in (market_open, hh4, ll4))
    if open_value > high:
        return "UP"
    if open_value < low:
        return "DOWN"
    return "NEUTRAL"


__all__ = [
    "BUFFER",
    "STRATEGY_KEY",
    "FuturesExitLevels",
    "FuturesLevels",
    "MissedEntryPlan",
    "calculate_levels",
    "classify_gap",
    "exit_levels_for_entry",
    "missed_entry_plan",
    "opening_range_entries",
]
