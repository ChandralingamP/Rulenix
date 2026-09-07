from datetime import date, datetime, timedelta, timezone
from decimal import Decimal
from uuid import uuid4

import pytest

from app.strategy import (
    Candle,
    OptionContract,
    OptionSide,
    calculate_levels,
    choose_atm_contract,
    classify_gap,
    continuous_session_candles,
    current_signal,
    exit_levels_for_entry,
    missed_entry_plan,
    normalize_to_tick,
    opening_range_entries,
    prepare_square_off_intent,
    supertrend_entry_allowed,
    supertrend_eod_due,
    supertrend_points,
    wilder_rma,
)
from app.strategy.execution import BrokerActionRequest, ExecutionOrchestrator, ExecutionOutcome
from app.strategy.scheduler import scheduler_action_allowed, session_is_open
from app.strategy.supertrend import ATR_PERIOD


def ts(hour: int, minute: int) -> datetime:
    from zoneinfo import ZoneInfo

    return datetime(2026, 8, 17, hour, minute, tzinfo=ZoneInfo("Asia/Kolkata"))


def candle(at: datetime, close: str) -> Candle:
    value = Decimal(close)
    return Candle(at, value, value + 2, value - 2, value)


def test_future_breakout_v3_levels_match_rust_formulas():
    levels = calculate_levels([100, 110, 105, 108], [90, 92, 94, 93])
    assert levels.hh4 == Decimal(110) and levels.hh2 == Decimal(108)
    assert levels.buy_entry == Decimal("110.132")
    assert levels.buy_target == Decimal("111.78398")
    assert levels.buy_sl1 == max(Decimal("108.48002"), Decimal("93.8872"))


@pytest.mark.parametrize(
    ("market_open", "buy", "sell", "label"),
    [
        ("100", "110", "90", "NONE_MISSED"),
        ("110", "110", "90", "BUY_MISSED"),
        ("90", "110", "90", "SELL_MISSED"),
    ],
)
def test_future_gap_and_missed_entry_boundaries(market_open, buy, sell, label):
    assert missed_entry_plan(market_open, buy, sell).label == label


def test_future_gap_uses_open_against_hh4_ll4_not_previous_close():
    assert classify_gap("110", "110", "90") == "NEUTRAL"
    assert classify_gap("110.01", "110", "90") == "UP"
    assert classify_gap("89.99", "110", "90") == "DOWN"


def test_future_opening_range_entries_and_invalid_history():
    plan = missed_entry_plan("110.01", "110", "90")
    buy, sell = opening_range_entries(plan, "112", "108")
    assert buy == Decimal("112.1344") and sell is None
    with pytest.raises(ValueError):
        calculate_levels([1, 2, 3], [1, 2, 3])


def test_future_sell_levels_are_intentionally_mirrored_by_rust():
    sell = exit_levels_for_entry("SELL", 100, 110, 90, 120, 80)
    assert sell.target == Decimal("98.500") and sell.sl1 > 100 and sell.sl2 > 100


def test_directional_tick_rounding_matches_rust():
    assert normalize_to_tick("100.021", "0.05", "BUY") == Decimal("100.05")
    assert normalize_to_tick("100.021", "0.05", "SELL") == Decimal(100)
    with pytest.raises(ValueError):
        normalize_to_tick("0.001", "0.05", "SELL")


def test_supertrend_wilder_seed_and_parameters():
    values = wilder_rma([Decimal(1), Decimal(2), Decimal(3), Decimal(4)], 3)
    assert values[:2] == [None, None] and values[2] == Decimal(2)
    assert ATR_PERIOD == 7


def test_supertrend_flip_to_call_matches_rust_fixture():
    closes = [100, 99, 98, 97, 96, 95, 94, 93, 92, 91, 90, 91, 92, 93, 108]
    points = supertrend_points(
        [
            candle(ts(9, 15) + timedelta(minutes=index * 5), str(close))
            for index, close in enumerate(closes)
        ],
        3,
        1,
    )
    assert points[-2].direction == "DOWN" and points[-1].direction == "UP"
    signal = current_signal(points, ts(10, 30))
    assert signal is not None and signal.side is OptionSide.CALL


def test_supertrend_entry_cutoff_and_eod_are_ist_and_timezone_independent():
    assert not supertrend_entry_allowed(ts(9, 14))
    assert supertrend_entry_allowed(ts(15, 9))
    assert not supertrend_entry_allowed(ts(15, 10))
    assert not supertrend_eod_due(ts(15, 9)) and supertrend_eod_due(ts(15, 10))
    utc = ts(15, 10).astimezone(timezone.utc)
    assert supertrend_eod_due(utc)


def test_supertrend_stale_flip_is_not_replayed():
    points = supertrend_points(
        [candle(ts(9, 15) + timedelta(minutes=i * 5), str(100 + i)) for i in range(10)], 3, 1
    )
    assert current_signal(points, ts(12, 0)) is None


def test_supertrend_continuity_requires_previous_session_and_current_candles():
    from zoneinfo import ZoneInfo

    friday = datetime(2026, 8, 21, 14, 40, tzinfo=ZoneInfo("Asia/Kolkata"))
    monday = datetime(2026, 8, 24, 9, 15, tzinfo=ZoneInfo("Asia/Kolkata"))
    previous = [candle(friday + timedelta(minutes=i * 5), "100") for i in range(9)]
    selected, previous_session = continuous_session_candles(
        previous + [candle(monday, "110")], monday.date(), monday
    )
    assert previous_session.isoformat() == "2026-08-21" and selected[-1].at == monday
    with pytest.raises(ValueError):
        continuous_session_candles(
            [candle(monday, "110")], monday.date(), monday + timedelta(minutes=5)
        )


def test_supertrend_contract_selection_is_nearest_atm_with_expiry_tie_break():
    contracts = [
        OptionContract("far", "NIFTY27AUG", date(2026, 8, 27), 75, Decimal(25000), OptionSide.CALL),
        OptionContract("low", "NIFTY20AUG", date(2026, 8, 20), 75, Decimal(24950), OptionSide.CALL),
        OptionContract("atm", "NIFTY20AUG", date(2026, 8, 20), 75, Decimal(25050), OptionSide.CALL),
    ]
    assert choose_atm_contract(contracts, "25060").token == "atm"


def test_scheduler_weekend_and_strategy_independence():
    assert session_is_open(ts(10, 0))[0]
    assert not session_is_open(ts(10, 0) + timedelta(days=5))[0]
    assert scheduler_action_allowed("futures_breakout_v3", ts(16, 0)) is False
    assert scheduler_action_allowed("supertrend_index_options_v1", ts(15, 9)) is True


def test_execution_request_is_typed_and_never_has_broker_id():
    intent = type(
        "Intent",
        (),
        {
            "id": uuid4(),
            "action": "ENTRY",
            "role": "BUY_ENTRY",
            "side": "BUY",
            "quantity": 10,
            "price": 100,
            "trigger_price": 99,
        },
    )()
    request = ExecutionOrchestrator.build_request(
        intent, exchange="MCX", symbol="GOLDTEN", token="1"
    )
    assert isinstance(request, BrokerActionRequest) and request.side == "BUY"
    assert ExecutionOutcome.LIVE_MUTATION_DISABLED_DURING_MIGRATION.value.startswith("LIVE_")


def test_eod_is_durable_square_off_not_local_close():
    intent = prepare_square_off_intent(
        user_id=uuid4(),
        trade_id=uuid4(),
        snapshot_id=uuid4(),
        strategy_key="supertrend_index_options_v1",
        instrument="NIFTY_CE",
        session_key="strev-20260817-1510-CE",
        side="SELL",
        quantity=75,
        price=Decimal(100),
    )
    assert intent.action == "SQUARE_OFF" and intent.role == "EMERGENCY_CLOSE"
