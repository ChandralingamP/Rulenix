"""Timezone-aware strategy scheduling helpers."""

from datetime import datetime

from .common import (
    as_ist,
    futures_session_open,
    is_weekend,
    supertrend_entry_allowed,
    supertrend_eod_due,
)


def session_is_open(
    value: datetime, *, morning_open: bool = True, evening_open: bool = True
) -> tuple[bool, str]:
    local = as_ist(value)
    if is_weekend(local.date()):
        return False, "Weekend"
    if futures_session_open(local, day_open=morning_open, evening_open=evening_open):
        return True, ""
    return False, "market session is closed"


def scheduler_action_allowed(strategy_key: str, value: datetime) -> bool:
    if strategy_key == "supertrend_index_options_v1":
        return supertrend_entry_allowed(value)
    if strategy_key == "futures_breakout_v3":
        return futures_session_open(value)
    return False


def eod_square_off_due(value: datetime) -> bool:
    return supertrend_eod_due(value)


__all__ = ["eod_square_off_due", "scheduler_action_allowed", "session_is_open"]
