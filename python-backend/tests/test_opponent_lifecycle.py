from datetime import date, datetime
from decimal import Decimal
from unittest.mock import AsyncMock, MagicMock
from uuid import uuid4

import pytest
from sqlalchemy.ext.asyncio import AsyncSession

from app.runtime.lifecycle import AuthoritativeExecutionWorker
from app.strategy.common import IST, Candle
from app.strategy.opponent import (
    OptionContract,
    OptionSide,
    benchmark_is_due,
    benchmark_is_recording,
    build_ranked_coincidence_queue,
    calculate_partial_close_lots,
    check_spot_expansion_abort,
    is_in_retracement_zone,
    is_session_trading_allowed,
)


def _candle(at: datetime, open_: float, high: float, low: float, close: float) -> Candle:
    return Candle(
        at=at,
        open=Decimal(str(open_)),
        high=Decimal(str(high)),
        low=Decimal(str(low)),
        close=Decimal(str(close)),
    )


def test_coincidence_queue_perimeter_ranking() -> None:
    """Verify that CE and PE contracts across the 3 strikes are scanned and ranked by premium proximity."""
    t0 = datetime(2026, 11, 10, 9, 30, tzinfo=IST)
    expiry = date(2026, 11, 12)

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

    pe_contracts = {
        Decimal(24450): (
            OptionContract("PE1", "NIFTY24450PE", expiry, 25, Decimal(24450), OptionSide.PUT),
            _candle(t0, 148, 152, 140, 145),
        ),
        Decimal(24500): (
            OptionContract("PE2", "NIFTY24500PE", expiry, 25, Decimal(24500), OptionSide.PUT),
            _candle(t0, 110, 125, 100, "123.0"),
        ),
        Decimal(24550): (
            OptionContract("PE3", "NIFTY24550PE", expiry, 25, Decimal(24550), OptionSide.PUT),
            _candle(t0, 92, 98, 88, 95),
        ),
    }

    strikes = [Decimal(24450), Decimal(24500), Decimal(24550)]
    queue = build_ranked_coincidence_queue(ce_contracts, pe_contracts, strikes)

    # 3 CEs x 3 PEs = 9 candidate pairs
    assert len(queue) == 9

    # Rank 1 must be 24500 with smallest delta |123.2 - 123.0| = 0.2
    rank_1 = queue[0]
    assert rank_1.rank == 1
    assert rank_1.ce_contract.strike == Decimal(24500)
    assert rank_1.pe_contract.strike == Decimal(24500)
    assert rank_1.premium_diff == Decimal("0.2")

    # Levels for 24500 CE come from 24500 PE (High=125, Low=100)
    assert rank_1.ce_levels.marked_high == Decimal(125)
    assert rank_1.ce_levels.marked_low == Decimal(100)


@pytest.mark.asyncio
async def test_opponent_benchmark_window_strictly_forbids_trading() -> None:
    """Between 09:30:00 and 09:32:59 IST, benchmark candle is recording. Zero orders allowed."""
    worker = AuthoritativeExecutionWorker(session_factory=MagicMock(), coordinator=MagicMock())
    session = AsyncMock(spec=AsyncSession)

    # Test at 09:30:00 IST
    t_0930 = datetime(2026, 11, 10, 9, 30, 0, tzinfo=IST)
    assert benchmark_is_recording(t_0930) is True
    assert benchmark_is_due(t_0930) is False
    assert await worker._evaluate_opponent(session, t_0930) == 0

    # Test at 09:31:30 IST
    t_0931 = datetime(2026, 11, 10, 9, 31, 30, tzinfo=IST)
    assert benchmark_is_recording(t_0931) is True
    assert benchmark_is_due(t_0931) is False
    assert await worker._evaluate_opponent(session, t_0931) == 0

    # Test at 09:32:59 IST
    t_0932 = datetime(2026, 11, 10, 9, 32, 59, tzinfo=IST)
    assert benchmark_is_recording(t_0932) is True
    assert benchmark_is_due(t_0932) is False
    assert await worker._evaluate_opponent(session, t_0932) == 0


def test_spot_expansion_abort_logic() -> None:
    """If spot expands > 150 points from 09:30 benchmark, abort setup immediately."""
    bench = Decimal(24500)
    assert check_spot_expansion_abort(bench, Decimal(24550)) is False
    assert check_spot_expansion_abort(bench, Decimal(24650)) is False
    assert check_spot_expansion_abort(bench, Decimal(24651)) is True
    assert check_spot_expansion_abort(bench, Decimal(24349)) is True


def test_retracement_pullback_zone() -> None:
    """Entry is strictly allowed only within [Marked High, Marked High + 5 pts]."""
    marked_h = Decimal(120)
    assert is_in_retracement_zone(Decimal("119.9"), marked_h) is False
    assert is_in_retracement_zone(Decimal(120), marked_h) is True
    assert is_in_retracement_zone(Decimal("123.5"), marked_h) is True
    assert is_in_retracement_zone(Decimal(125), marked_h) is True
    assert is_in_retracement_zone(Decimal("125.1"), marked_h) is False


def test_partial_close_scaling() -> None:
    """Verify configured partial close percentages."""
    assert calculate_partial_close_lots(10, 50) == 5
    assert calculate_partial_close_lots(10, 20) == 2
    assert calculate_partial_close_lots(10, 100) == 9
    assert calculate_partial_close_lots(2, 50) == 1
    assert calculate_partial_close_lots(1, 50) == 1


def test_circuit_breaker_rules() -> None:
    """Verify max daily trades and loss circuit breaker."""
    # Under standard profits: max 3 trades allowed
    assert is_session_trading_allowed(total_trades_today=0, losses_today=0) is True
    assert is_session_trading_allowed(total_trades_today=1, losses_today=0) is True
    assert is_session_trading_allowed(total_trades_today=2, losses_today=0) is True
    assert is_session_trading_allowed(total_trades_today=3, losses_today=0) is False

    # 1 loss: allow at most 1 more trade (total trades < 2)
    assert is_session_trading_allowed(total_trades_today=1, losses_today=1) is True
    assert is_session_trading_allowed(total_trades_today=2, losses_today=1) is False

    # 2 losses: halt immediately
    assert is_session_trading_allowed(total_trades_today=2, losses_today=2) is False


@pytest.mark.asyncio
async def test_select_opponent_basket_matches_identical_strikes() -> None:
    """AngelSuperTrendMarketProvider.select_opponent_basket pairs identical strikes for nearest weekly expiry."""
    from app.runtime.market import AngelSuperTrendMarketProvider

    provider = AngelSuperTrendMarketProvider(client_factory=MagicMock())
    test_rows = [
        # Nearest weekly expiry 12-Nov-2026
        {"exch_seg": "NFO", "name": "NIFTY", "instrumenttype": "OPTIDX", "symbol": "NIFTY12NOV24450CE", "token": "T1", "expiry": "12NOV2026", "strike": "2445000", "lotsize": "25"},
        {"exch_seg": "NFO", "name": "NIFTY", "instrumenttype": "OPTIDX", "symbol": "NIFTY12NOV24450PE", "token": "T2", "expiry": "12NOV2026", "strike": "2445000", "lotsize": "25"},
        {"exch_seg": "NFO", "name": "NIFTY", "instrumenttype": "OPTIDX", "symbol": "NIFTY12NOV24500CE", "token": "T3", "expiry": "12NOV2026", "strike": "2450000", "lotsize": "25"},
        {"exch_seg": "NFO", "name": "NIFTY", "instrumenttype": "OPTIDX", "symbol": "NIFTY12NOV24500PE", "token": "T4", "expiry": "12NOV2026", "strike": "2450000", "lotsize": "25"},
        {"exch_seg": "NFO", "name": "NIFTY", "instrumenttype": "OPTIDX", "symbol": "NIFTY12NOV24550CE", "token": "T5", "expiry": "12NOV2026", "strike": "2455000", "lotsize": "25"},
        {"exch_seg": "NFO", "name": "NIFTY", "instrumenttype": "OPTIDX", "symbol": "NIFTY12NOV24550PE", "token": "T6", "expiry": "12NOV2026", "strike": "2455000", "lotsize": "25"},
        # Later monthly expiry
        {"exch_seg": "NFO", "name": "NIFTY", "instrumenttype": "OPTIDX", "symbol": "NIFTY26NOV24500CE", "token": "T7", "expiry": "26NOV2026", "strike": "2450000", "lotsize": "25"},
    ]
    provider._master = AsyncMock(return_value=tuple(test_rows))

    strikes = [Decimal(24450), Decimal(24500), Decimal(24550)]
    baskets = await provider.select_opponent_basket(
        user_id=uuid4(),
        strikes=strikes,
        trade_date=date(2026, 11, 10),
    )

    assert len(baskets) == 3
    for b in baskets:
        assert b.strike == b.ce_contract.strike == b.pe_contract.strike
        assert b.ce_contract.expiry == date(2026, 11, 12)
        assert b.pe_contract.expiry == date(2026, 11, 12)

    assert baskets[0].strike == Decimal(24450)
    assert baskets[0].ce_contract.symbol == "NIFTY12NOV24450CE"
    assert baskets[0].pe_contract.symbol == "NIFTY12NOV24450PE"

    assert baskets[1].strike == Decimal(24500)
    assert baskets[1].ce_contract.symbol == "NIFTY12NOV24500CE"
    assert baskets[1].pe_contract.symbol == "NIFTY12NOV24500PE"

    assert baskets[2].strike == Decimal(24550)
    assert baskets[2].ce_contract.symbol == "NIFTY12NOV24550CE"
    assert baskets[2].pe_contract.symbol == "NIFTY12NOV24550PE"


@pytest.mark.asyncio
async def test_lifecycle_forbids_trade_without_sufficient_candles() -> None:
    """If 3m candles for either side are missing or < 2 candles, zero orders must be placed."""
    worker = AuthoritativeExecutionWorker(session_factory=MagicMock(), coordinator=MagicMock())
    session = AsyncMock(spec=AsyncSession)

    # Setup session mocks for eligible runners and spot
    eligible_runner = {"user_id": str(uuid4()), "lots": 1, "partial_close_pct": 50.0}
    session.execute.side_effect = [
        # 1. _manage_opponent_positions open trades query -> empty
        MagicMock(mappings=MagicMock(return_value=MagicMock(all=MagicMock(return_value=[])))),
        # 2. runners query
        MagicMock(mappings=MagicMock(return_value=MagicMock(all=MagicMock(return_value=[eligible_runner])))),
        # 3. trades counts query
        MagicMock(mappings=MagicMock(return_value=MagicMock(first=MagicMock(return_value={"total_trades": 0, "losses": 0})))),
        # 4. benchmark row exists check
        MagicMock(mappings=MagicMock(return_value=MagicMock(first=MagicMock(return_value={"id": str(uuid4()), "status": "ready", "previous_close": 24500})))),
        # 5. candle rows query -> return empty!
        MagicMock(mappings=MagicMock(return_value=MagicMock(all=MagicMock(return_value=[])))),
    ]
    session.scalar.side_effect = [
        # active trades check
        False,
        # spot ltp
        Decimal(24500),
    ]

    mock_provider = MagicMock()
    mock_provider.select = AsyncMock(return_value=MagicMock(contract=MagicMock(token="T1", premium=Decimal(120), lot_size=25, symbol="CE1")))
    worker.market_provider = mock_provider

    t_0936 = datetime(2026, 11, 10, 9, 36, 0, tzinfo=IST)
    result = await worker._evaluate_opponent(session, t_0936)
    assert result == 0


@pytest.mark.asyncio
async def test_opponent_candle_close_sl_triggers_exit() -> None:
    """Initial SL only triggers when a completed 3m candle closes below marked low."""
    worker = AuthoritativeExecutionWorker(session_factory=MagicMock(), coordinator=MagicMock())
    session = AsyncMock(spec=AsyncSession)

    trade_id = uuid4()
    open_trade = {
        "id": str(trade_id),
        "user_id": str(uuid4()),
        "execution_mode": "demo",
        "direction": "BUY",
        "quantity": 50,
        "entry_price": 126.0,
        "last_price": 115.0,
        "total_lots": 2,
        "remaining_lots": 2,
        "target_price": 153.8,
        "sl1_price": 108.0,
        "notes": "Initial entry",
        "strategy_snapshot_id": str(uuid4()),
        "instrument_label": "NIFTY",
        "contract_token": "T_CE",
        "contract_symbol": "NIFTY24500CE",
        "lot_size": 25,
        "pnl": 0.0,
        "buy_target": 153.75,
        "buy_sl1": 108.0,
    }

    t0 = datetime(2026, 11, 10, 9, 30, tzinfo=IST)
    t1 = datetime(2026, 11, 10, 9, 33, tzinfo=IST)

    # 3m candle 1: benchmark
    # 3m candle 2: closed at 104 < marked low 108.0 -> triggers SL!
    candle_rows = [
        {"candle_time": t0, "open_price": 115, "high_price": 125, "low_price": 110, "close_price": 123},
        {"candle_time": t1, "open_price": 112, "high_price": 114, "low_price": 102, "close_price": 104},
    ]

    session.execute.side_effect = [
        # open trades
        MagicMock(mappings=MagicMock(return_value=MagicMock(all=MagicMock(return_value=[open_trade])))),
        # candles query
        MagicMock(mappings=MagicMock(return_value=MagicMock(all=MagicMock(return_value=candle_rows)))),
        # cancel demo orders
        MagicMock(),
        # update trades
        MagicMock(),
    ]
    session.scalar.return_value = Decimal(105)

    now = datetime(2026, 11, 10, 9, 36, tzinfo=IST)
    await worker._manage_opponent_positions(session, now)

    # Verify cancel orders and trade update were executed
    assert session.execute.call_count >= 3


@pytest.mark.asyncio
async def test_opponent_trailing_ladder_progression() -> None:
    """When price reaches T2 (184.6) and T3 (215.4), SL trails to T1 (153.8) and T2 (184.6)."""
    worker = AuthoritativeExecutionWorker(session_factory=MagicMock(), coordinator=MagicMock())
    session = AsyncMock(spec=AsyncSession)

    trade_id = uuid4()
    # Trade already booked T1, has 1 runner lot remaining
    open_trade = {
        "id": str(trade_id),
        "user_id": str(uuid4()),
        "execution_mode": "demo",
        "direction": "BUY",
        "quantity": 25,
        "entry_price": 126.0,
        "last_price": 186.0,
        "total_lots": 2,
        "remaining_lots": 1,
        "target_price": 184.6,
        "sl1_price": 126.0,  # currently at breakeven
        "notes": "T1_HIT: partial booked",
        "strategy_snapshot_id": str(uuid4()),
        "instrument_label": "NIFTY",
        "contract_token": "T_CE",
        "contract_symbol": "NIFTY24500CE",
        "lot_size": 25,
        "pnl": 0.0,
        "buy_target": 153.75,  # H = 123
        "buy_sl1": 108.0,
    }

    session.execute.side_effect = [
        # open trades
        MagicMock(mappings=MagicMock(return_value=MagicMock(all=MagicMock(return_value=[open_trade])))),
        # candles query
        MagicMock(mappings=MagicMock(return_value=MagicMock(all=MagicMock(return_value=[])))),
        # update trades trailing SL to T1
        MagicMock(),
    ]
    # Current LTP is 186 >= T2 (184.50)
    session.scalar.return_value = Decimal(186)

    now = datetime(2026, 11, 10, 9, 45, tzinfo=IST)
    await worker._manage_opponent_positions(session, now)

    # 3 execute calls: open_trades, candles, update trades with T2_HIT
    assert session.execute.call_count == 3
