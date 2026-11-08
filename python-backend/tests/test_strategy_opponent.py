"""Comprehensive tests for Nifty 50 Equivalent Opponent Strategy."""

from datetime import date, datetime
from decimal import Decimal

from app.strategy.common import IST, Candle
from app.strategy.opponent import (
    CoincidencePair,
    OpponentTargetLadder,
    OptionContract,
    OptionSide,
    build_ranked_coincidence_queue,
    calculate_3_strike_perimeter,
    calculate_atm_strike,
    calculate_partial_close_lots,
    check_confirmation_and_entry,
    check_spot_expansion_abort,
    is_in_retracement_zone,
    is_session_trading_allowed,
    verify_opponent_no_retouch,
)


def _candle(
    at: datetime, open_p: str | float, high: str | float, low: str | float, close: str | float
) -> Candle:
    return Candle(
        at=at,
        open=Decimal(str(open_p)),
        high=Decimal(str(high)),
        low=Decimal(str(low)),
        close=Decimal(str(close)),
    )


def test_atm_and_perimeter_calculation() -> None:
    # Spot at 24,520: nearest strike is 24500
    assert calculate_atm_strike(24520) == Decimal(24500)
    assert calculate_atm_strike("24525") == Decimal(24550)
    assert calculate_atm_strike("24500") == Decimal(24500)

    perimeter = calculate_3_strike_perimeter(24520)
    assert perimeter == [Decimal(24450), Decimal(24500), Decimal(24550)]


def test_ranked_coincidence_queue_walkthrough() -> None:
    t0 = datetime(2026, 11, 8, 9, 30, tzinfo=IST)
    expiry = date(2026, 11, 12)

    # 3 CE strikes
    ce_contracts = {
        Decimal(24450): (
            OptionContract("CE1", "NIFTY24450CE", expiry, 25, Decimal(24450), OptionSide.CALL),
            _candle(t0, 160, 165, 145, 155),
        ),
        Decimal(24500): (
            OptionContract("CE2", "NIFTY24500CE", expiry, 25, Decimal(24500), OptionSide.CALL),
            # PDF Walkthrough: 24500 CE High=125, Low=110, Close=123.2
            _candle(t0, 115, 125, 110, "123.2"),
        ),
        Decimal(24550): (
            OptionContract("CE3", "NIFTY24550CE", expiry, 25, Decimal(24550), OptionSide.CALL),
            _candle(t0, 95, 100, 85, 90),
        ),
    }

    # 3 PE strikes
    pe_contracts = {
        Decimal(24450): (
            OptionContract("PE1", "NIFTY24450PE", expiry, 25, Decimal(24450), OptionSide.PUT),
            _candle(t0, 90, 95, 80, 85),
        ),
        Decimal(24500): (
            OptionContract("PE2", "NIFTY24500PE", expiry, 25, Decimal(24500), OptionSide.PUT),
            _candle(t0, 110, 118, 100, 105),
        ),
        Decimal(24550): (
            OptionContract("PE3", "NIFTY24550PE", expiry, 25, Decimal(24550), OptionSide.PUT),
            # PDF Walkthrough: 24550 PE High=123, Low=108, Close=123.0
            _candle(t0, 112, 123, 108, "123.0"),
        ),
    }

    strikes = [Decimal(24450), Decimal(24500), Decimal(24550)]
    queue = build_ranked_coincidence_queue(ce_contracts, pe_contracts, strikes)

    assert len(queue) == 9
    top_pair = queue[0]
    assert top_pair.rank == 1
    # Best coincidence is 24500 CE (124) and 24550 PE (123.8) -> delta 0.2
    assert top_pair.ce_contract.strike == Decimal(24500)
    assert top_pair.pe_contract.strike == Decimal(24550)
    assert top_pair.premium_diff == Decimal("0.2")

    # Cross-projected levels:
    # CE chart gets PE's High (123) and Low (108)
    ce_levels = top_pair.ce_levels
    assert ce_levels.marked_high == Decimal(123)
    assert ce_levels.marked_low == Decimal(108)

    # PE chart gets CE's High (125) and Low (110)
    pe_levels = top_pair.pe_levels
    assert pe_levels.marked_high == Decimal(125)
    assert pe_levels.marked_low == Decimal(110)


def test_target_ladder_calculations() -> None:
    # High = 123 (from simulation in PDF)
    ladder = OpponentTargetLadder.from_marked_high(Decimal(123))
    assert ladder.marked_high == Decimal(123)
    assert ladder.target_1 == Decimal("153.75")
    assert ladder.target_2 == Decimal("184.50")
    assert ladder.target_3 == Decimal("215.25")
    assert ladder.target_4 == Decimal("246.00")


def test_verify_opponent_no_retouch_filter() -> None:
    t0 = datetime(2026, 11, 8, 9, 36, tzinfo=IST)
    marked_low = Decimal(110)

    # Case 1: Next candle range 98 to 106. High=106 < 110. (VALID per walkthrough)
    candle_safe = _candle(t0, 102, 106, 98, 100)
    assert verify_opponent_no_retouch([candle_safe], marked_low) is True

    # Case 2: Candle tests marked low (High = 110) -> INVALID
    candle_test = _candle(t0, 102, 110, 98, 100)
    assert verify_opponent_no_retouch([candle_test], marked_low) is False

    # Case 3: Candle breaches marked low (High = 112) -> INVALID
    candle_breach = _candle(t0, 102, 112, 98, 100)
    assert verify_opponent_no_retouch([candle_breach], marked_low) is False


def test_confirmation_and_entry_signal() -> None:
    t0 = datetime(2026, 11, 8, 9, 30, tzinfo=IST)
    t1 = datetime(2026, 11, 8, 9, 33, tzinfo=IST)
    t2 = datetime(2026, 11, 8, 9, 36, tzinfo=IST)
    expiry = date(2026, 11, 12)

    pair = CoincidencePair(
        ce_contract=OptionContract("CE", "NIFTY24500CE", expiry, 25, Decimal(24500), OptionSide.CALL),
        pe_contract=OptionContract("PE", "NIFTY24550PE", expiry, 25, Decimal(24550), OptionSide.PUT),
        ce_candle=_candle(t0, 115, 125, 110, 123),  # CE High 125, Low 110 -> projected to PE
        pe_candle=_candle(t0, 112, 123, 108, 123),  # PE High 123, Low 108 -> projected to CE
        premium_diff=Decimal("0.0"),
        rank=1,
    )

    # At 09:33-09:36:
    # CE breaks above marked High (123) and closes at 128
    ce_breakout = _candle(t1, 122, 130, 121, 128)
    # PE breaks below marked Low (110) with a Red candle (open 108, close 102)
    pe_breakdown = _candle(t1, 108, 109, 101, 102)
    # Next PE candle ranges from 98 to 106, closing at 100 (No re-touch: High 106 < 110)
    pe_subsequent = _candle(t2, 102, 106, 98, 100)

    signal = check_confirmation_and_entry(
        pair=pair,
        breakout_side=OptionSide.CALL,
        breakout_candles=[ce_breakout],
        opponent_candles=[pe_breakdown, pe_subsequent],
    )

    assert signal is not None
    assert signal.side is OptionSide.CALL
    assert signal.contract.symbol == "NIFTY24500CE"
    assert signal.marked_high == Decimal(123)
    assert signal.marked_low == Decimal(108)
    assert signal.retracement_min == Decimal(123)
    assert signal.retracement_max == Decimal(128)  # 123 + 5 pts
    assert signal.pair_rank == 1

    # Retracement pullback check
    assert is_in_retracement_zone(Decimal(126), signal.marked_high) is True
    assert is_in_retracement_zone(Decimal(120), signal.marked_high) is False
    assert is_in_retracement_zone(Decimal(129), signal.marked_high) is False


def test_spot_expansion_abort() -> None:
    bench = Decimal(24520)
    # <= 150 points is within tolerance
    assert check_spot_expansion_abort(bench, Decimal(24670)) is False
    assert check_spot_expansion_abort(bench, Decimal(24370)) is False
    # > 150 points triggers abort
    assert check_spot_expansion_abort(bench, Decimal(24671)) is True
    assert check_spot_expansion_abort(bench, Decimal(24369)) is True


def test_calculate_partial_close_lots() -> None:
    # 2 lots with 50% close -> 1 lot
    assert calculate_partial_close_lots(2, 50) == 1
    # 4 lots with 50% close -> 2 lots
    assert calculate_partial_close_lots(4, 50) == 2
    # 4 lots with 25% close -> 1 lot
    assert calculate_partial_close_lots(4, 25) == 1
    # 10 lots with 30% close -> 3 lots
    assert calculate_partial_close_lots(10, 30) == 3
    # Single lot stays 1 lot
    assert calculate_partial_close_lots(1, 50) == 1


def test_session_trading_allowed_guardrails() -> None:
    # Fresh day: 0 trades, 0 losses -> allowed
    assert is_session_trading_allowed(0, 0) is True

    # 1 winning trade -> allowed
    assert is_session_trading_allowed(1, 0) is True

    # 1 trade that logged a loss -> allowed (max 1 more attempt)
    assert is_session_trading_allowed(1, 1) is True

    # 2 trades where 1 was a loss -> halted (loss limit reached)
    assert is_session_trading_allowed(2, 1) is False

    # 2 losses -> halted immediately
    assert is_session_trading_allowed(2, 2) is False

    # 3 profitable trades -> max daily trades reached
    assert is_session_trading_allowed(3, 0) is False


def test_next_best_coincidence_fallback() -> None:
    t0 = datetime(2026, 11, 8, 9, 30, tzinfo=IST)
    expiry = date(2026, 11, 12)

    # 3 CE strikes
    ce_contracts = {
        Decimal(24450): (
            OptionContract("CE1", "NIFTY24450CE", expiry, 25, Decimal(24450), OptionSide.CALL),
            _candle(t0, 160, 165, 145, 155),
        ),
        Decimal(24500): (
            OptionContract("CE2", "NIFTY24500CE", expiry, 25, Decimal(24500), OptionSide.CALL),
            _candle(t0, 115, 125, 110, "123.2"),
        ),
        Decimal(24550): (
            OptionContract("CE3", "NIFTY24550CE", expiry, 25, Decimal(24550), OptionSide.CALL),
            _candle(t0, 95, 100, 85, 90),
        ),
    }

    # 3 PE strikes
    pe_contracts = {
        Decimal(24450): (
            OptionContract("PE1", "NIFTY24450PE", expiry, 25, Decimal(24450), OptionSide.PUT),
            _candle(t0, 90, 95, 80, "89.5"),  # Delta with CE3 is |90 - 89.5| = 0.5 (Second best)
        ),
        Decimal(24500): (
            OptionContract("PE2", "NIFTY24500PE", expiry, 25, Decimal(24500), OptionSide.PUT),
            _candle(t0, 110, 118, 100, 105),
        ),
        Decimal(24550): (
            OptionContract("PE3", "NIFTY24550PE", expiry, 25, Decimal(24550), OptionSide.PUT),
            _candle(t0, 112, 123, 108, "123.0"),  # Delta with CE2 is |123.2 - 123.0| = 0.2 (Best)
        ),
    }

    strikes = [Decimal(24450), Decimal(24500), Decimal(24550)]
    queue = build_ranked_coincidence_queue(ce_contracts, pe_contracts, strikes)

    # Best coincidence is Rank 1
    rank_1 = queue[0]
    assert rank_1.rank == 1
    assert rank_1.ce_contract.strike == Decimal(24500)
    assert rank_1.pe_contract.strike == Decimal(24550)
    assert rank_1.premium_diff == Decimal("0.2")

    # If Rank 1 fails or invalidates, promote Next Best Coincidence (Rank 2)
    rank_2 = queue[1]
    assert rank_2.rank == 2
    assert rank_2.ce_contract.strike == Decimal(24550)
    assert rank_2.pe_contract.strike == Decimal(24450)
    assert rank_2.premium_diff == Decimal("0.5")

    # Levels for Rank 2 are derived cleanly from its own candles
    r2_ce_levels = rank_2.ce_levels
    assert r2_ce_levels.marked_high == Decimal(95)
    assert r2_ce_levels.marked_low == Decimal(80)

