"""Shared market-time and candle primitives matching Rust's IST semantics."""

from dataclasses import dataclass
from datetime import UTC, date, datetime, timedelta, timezone
from decimal import ROUND_CEILING, ROUND_FLOOR, Decimal, InvalidOperation

IST = timezone(timedelta(hours=5, minutes=30), name="Asia/Kolkata")


@dataclass(frozen=True)
class Candle:
    at: datetime
    open: Decimal
    high: Decimal
    low: Decimal
    close: Decimal

    def __post_init__(self) -> None:
        if self.at.tzinfo is None or self.at.utcoffset() is None:
            raise ValueError("Candle timestamps must be timezone-aware.")
        if any(value <= 0 for value in (self.open, self.high, self.low, self.close)):
            raise ValueError("Candle prices must be positive.")
        if self.high < max(self.open, self.close) or self.low > min(self.open, self.close):
            raise ValueError("Candle high/low does not contain open/close.")

    @property
    def ist(self) -> datetime:
        return self.at.astimezone(IST)


def as_ist(value: datetime) -> datetime:
    if value.tzinfo is None or value.utcoffset() is None:
        raise ValueError("Trading times require timezone-aware datetimes.")
    return value.astimezone(IST)


def ist_now() -> datetime:
    return datetime.now(UTC).astimezone(IST)


def is_weekend(value: date) -> bool:
    return value.weekday() >= 5


def minute_of_day(value: datetime) -> int:
    local = as_ist(value)
    return local.hour * 60 + local.minute


def supertrend_entry_allowed(value: datetime) -> bool:
    minute = minute_of_day(value)
    return 9 * 60 + 15 <= minute < 15 * 60 + 10


def supertrend_eod_due(value: datetime) -> bool:
    return minute_of_day(value) >= 15 * 60 + 10


def futures_session_open(
    value: datetime, *, day_open: bool = True, evening_open: bool = True
) -> bool:
    minute = minute_of_day(value)
    return (day_open and 9 * 60 <= minute <= 15 * 60 + 20) or (
        evening_open and 17 * 60 <= minute <= 23 * 60 + 25
    )


def normalize_to_tick(
    price: Decimal | str | int, tick_size: Decimal | str | int, side: str
) -> Decimal:
    """Rust's directional tick rounding: BUY ceil, SELL floor."""
    try:
        value, tick = Decimal(str(price)), Decimal(str(tick_size))
    except (InvalidOperation, ValueError) as exc:
        raise ValueError("Price and tick must be finite decimals.") from exc
    if value <= 0 or tick <= 0 or side not in {"BUY", "SELL"}:
        raise ValueError("Price, tick, and transaction side are invalid.")
    quotient = value / tick
    rounding = ROUND_CEILING if side == "BUY" else ROUND_FLOOR
    normalized = quotient.to_integral_value(rounding=rounding) * tick
    if normalized <= 0:
        raise ValueError("Directional tick rounding produced a non-positive price.")
    return normalized.normalize()


def latest_completed_five_minute(value: datetime) -> datetime:
    local = as_ist(value).replace(second=0, microsecond=0)
    minute = (local.minute // 5) * 5
    return local.replace(minute=minute) - timedelta(minutes=5)


__all__ = [
    "IST",
    "Candle",
    "as_ist",
    "futures_session_open",
    "is_weekend",
    "ist_now",
    "latest_completed_five_minute",
    "minute_of_day",
    "normalize_to_tick",
    "supertrend_entry_allowed",
    "supertrend_eod_due",
]
