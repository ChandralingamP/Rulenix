"""``supertrend_index_options_v1`` calculations and contract selection."""

from dataclasses import dataclass
from datetime import UTC, date, datetime, time, timedelta
from decimal import Decimal
from enum import StrEnum
from itertools import pairwise

from .common import (
    IST,
    Candle,
    as_ist,
    latest_completed_five_minute,
    supertrend_entry_allowed,
    supertrend_eod_due,
)

ATR_PERIOD = 7
SUPERTREND_FACTOR = Decimal(2)
MAX_ENTRY_DELAY = timedelta(seconds=90)
STRATEGY_KEY = "supertrend_index_options_v1"


class OptionSide(StrEnum):
    CALL = "CE"
    PUT = "PE"

    @property
    def opposite(self) -> "OptionSide":
        return OptionSide.PUT if self is OptionSide.CALL else OptionSide.CALL


@dataclass(frozen=True)
class SuperTrendPoint:
    candle: Candle
    value: Decimal
    direction: str


@dataclass(frozen=True)
class SuperTrendSignal:
    side: OptionSide
    signal_at: datetime
    index_close: Decimal
    supertrend: Decimal
    previous_direction: str
    direction: str


@dataclass(frozen=True)
class IndexOptionConfig:
    instrument: str
    index_exchange: str
    index_token: str
    option_exchange: str
    option_name: str
    default_target_points: Decimal
    default_stop_loss_points: Decimal


CONFIGS = {
    "SENSEX": IndexOptionConfig(
        "SENSEX", "BSE", "99919000", "BFO", "SENSEX", Decimal(40), Decimal(25)
    ),
    "NIFTY": IndexOptionConfig(
        "NIFTY", "NSE", "99926000", "NFO", "NIFTY", Decimal(25), Decimal(15)
    ),
}


@dataclass(frozen=True)
class OptionContract:
    token: str
    symbol: str
    expiry: date
    lot_size: int
    strike: Decimal
    option_type: OptionSide
    premium: Decimal = Decimal(0)


def true_ranges(candles: list[Candle]) -> list[Decimal]:
    result: list[Decimal] = []
    for index, candle in enumerate(candles):
        if index == 0:
            result.append(candle.high - candle.low)
        else:
            previous_close = candles[index - 1].close
            result.append(
                max(
                    candle.high - candle.low,
                    abs(candle.high - previous_close),
                    abs(candle.low - previous_close),
                )
            )
    return result


def wilder_rma(values: list[Decimal], period: int = ATR_PERIOD) -> list[Decimal | None]:
    result: list[Decimal | None] = [None] * len(values)
    if period <= 0 or len(values) < period:
        return result
    previous = sum(values[:period], Decimal(0)) / period
    result[period - 1] = previous
    for index, value in enumerate(values[period:], period):
        previous = (previous * (period - 1) + value) / period
        result[index] = previous
    return result


def supertrend_points(
    candles: list[Candle], atr_period: int = ATR_PERIOD, factor: Decimal | str = SUPERTREND_FACTOR
) -> list[SuperTrendPoint]:
    factor = Decimal(str(factor))
    if atr_period <= 0 or factor <= 0:
        return []
    atr = wilder_rma(true_ranges(candles), atr_period)
    points: list[SuperTrendPoint] = []
    previous_upper: Decimal | None = None
    previous_lower: Decimal | None = None
    previous_direction: str | None = None
    for index, candle in enumerate(candles):
        atr_value = atr[index]
        if atr_value is None:
            continue
        hl2 = (candle.high + candle.low) / 2
        basic_upper, basic_lower = hl2 + factor * atr_value, hl2 - factor * atr_value
        if points:
            previous_close = candles[max(index - 1, 0)].close
            previous_upper = previous_upper if previous_upper is not None else basic_upper
            previous_lower = previous_lower if previous_lower is not None else basic_lower
            final_upper = (
                basic_upper
                if basic_upper < previous_upper or previous_close > previous_upper
                else previous_upper
            )
            final_lower = (
                basic_lower
                if basic_lower > previous_lower or previous_close < previous_lower
                else previous_lower
            )
            direction = (
                "UP"
                if (previous_direction or "DOWN") == "DOWN" and candle.close > final_upper
                else "DOWN"
                if (previous_direction or "DOWN") == "DOWN"
                else ("DOWN" if candle.close < final_lower else "UP")
            )
        else:
            final_upper, final_lower, direction = basic_upper, basic_lower, "DOWN"
        value = final_lower if direction == "UP" else final_upper
        previous_upper, previous_lower, previous_direction = final_upper, final_lower, direction
        points.append(SuperTrendPoint(candle, value, direction))
    return points


def signal_from_transition(
    previous: SuperTrendPoint, latest: SuperTrendPoint
) -> SuperTrendSignal | None:
    if previous.direction == latest.direction:
        return None
    side = (
        OptionSide.CALL
        if previous.direction == "DOWN" and latest.direction == "UP"
        else OptionSide.PUT
        if previous.direction == "UP" and latest.direction == "DOWN"
        else None
    )
    if side is None:
        return None
    return SuperTrendSignal(
        side,
        latest.candle.at,
        latest.candle.close,
        latest.value,
        previous.direction,
        latest.direction,
    )


def current_signal(points: list[SuperTrendPoint], now: datetime) -> SuperTrendSignal | None:
    target = latest_completed_five_minute(now).replace(tzinfo=None)
    for previous, latest in pairwise(points):
        if latest.candle.at.replace(tzinfo=None) == target:
            return signal_from_transition(previous, latest)
    return None


def signal_is_fresh(signal: SuperTrendSignal, now: datetime) -> bool:
    closed_at = signal.signal_at + timedelta(minutes=5)
    local_now = as_ist(now).replace(tzinfo=None)
    signal_closed = closed_at.replace(tzinfo=None)
    return signal_closed <= local_now <= signal_closed + MAX_ENTRY_DELAY


def continuous_session_candles(
    candles: list[Candle], today: date, latest_expected: datetime
) -> tuple[list[Candle], date]:
    """Validate Rust's previous-session continuity and current five-minute window."""
    latest = as_ist(latest_expected)
    market_open, market_close = time(9, 15), time(15, 30)
    selected = sorted(
        {
            candle.at: candle
            for candle in candles
            if market_open <= candle.at.astimezone(IST).time() < market_close
            and candle.at.astimezone(IST) <= latest
        }.values(),
        key=lambda candle: candle.at,
    )
    previous_dates = {
        candle.at.astimezone(IST).date()
        for candle in selected
        if candle.at.astimezone(IST).date() < today
    }
    if not previous_dates:
        raise ValueError("previous trading-session candles are missing")
    previous_session = max(previous_dates)
    if (
        sum(candle.at.astimezone(IST).date() == previous_session for candle in selected)
        < ATR_PERIOD + 2
    ):
        raise ValueError(f"previous trading session {previous_session} has insufficient candles")
    if latest.time() >= market_open:
        expected = datetime.combine(today, market_open, tzinfo=IST)
        while expected <= latest:
            if not any(candle.at.astimezone(IST) == expected for candle in selected):
                raise ValueError(f"completed index candle {expected} is not available yet")
            expected += timedelta(minutes=5)
    return selected, previous_session


def parse_expiry(value: str) -> date:
    return datetime.strptime(value.upper(), "%d%b%Y").replace(tzinfo=UTC).date()


def option_candidates(
    contracts: list[OptionContract], config: IndexOptionConfig, trade_date: date, side: OptionSide
) -> list[OptionContract]:
    return sorted(
        (
            contract
            for contract in contracts
            if contract.option_type is side and contract.expiry >= trade_date
        ),
        key=lambda contract: (contract.expiry, contract.strike),
    )


def choose_atm_contract(
    candidates: list[OptionContract], underlying_ltp: Decimal | str
) -> OptionContract:
    if not candidates:
        raise ValueError("No eligible option contracts.")
    ltp = Decimal(str(underlying_ltp))
    return min(
        candidates,
        key=lambda contract: (abs(contract.strike - ltp), contract.expiry, contract.strike),
    )


def config_for(instrument: str) -> IndexOptionConfig:
    try:
        return CONFIGS[instrument]
    except KeyError as exc:
        raise ValueError("Unsupported SuperTrend underlying instrument.") from exc


__all__ = [
    "ATR_PERIOD",
    "CONFIGS",
    "STRATEGY_KEY",
    "IndexOptionConfig",
    "OptionContract",
    "OptionSide",
    "SuperTrendPoint",
    "SuperTrendSignal",
    "choose_atm_contract",
    "config_for",
    "current_signal",
    "option_candidates",
    "parse_expiry",
    "signal_from_transition",
    "signal_is_fresh",
    "supertrend_entry_allowed",
    "supertrend_eod_due",
    "supertrend_points",
    "true_ranges",
    "wilder_rma",
]
