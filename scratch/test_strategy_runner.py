import sys
import importlib.util
from pathlib import Path
from decimal import Decimal
from datetime import datetime, date, timedelta, timezone
from zoneinfo import ZoneInfo

backend_path = Path(r"C:\GitHub\TradingSVCV\Rulenix\python-backend")

def load_module(name: str, file_path: Path):
    spec = importlib.util.spec_from_file_location(name, file_path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod

# Create package namespaces
import types
app_mod = types.ModuleType("app")
app_strategy_mod = types.ModuleType("app.strategy")
sys.modules["app"] = app_mod
sys.modules["app.strategy"] = app_strategy_mod

common = load_module("app.strategy.common", backend_path / "app" / "strategy" / "common.py")
futures_breakout = load_module("app.strategy.futures_breakout", backend_path / "app" / "strategy" / "futures_breakout.py")
supertrend = load_module("app.strategy.supertrend", backend_path / "app" / "strategy" / "supertrend.py")
scheduler = load_module("app.strategy.scheduler", backend_path / "app" / "strategy" / "scheduler.py")

passed = 0
failed = 0

def test(name, condition, msg=""):
    global passed, failed
    if condition:
        passed += 1
        print(f"  [PASS] {name}")
    else:
        failed += 1
        print(f"  [FAIL] {name}: {msg}")

def ts(hour: int, minute: int) -> datetime:
    return datetime(2026, 8, 17, hour, minute, tzinfo=ZoneInfo("Asia/Kolkata"))

def candle(at: datetime, close: str) -> common.Candle:
    val = Decimal(close)
    return common.Candle(at, val, val + Decimal("2"), val - Decimal("2"), val)

print("=== 1. TESTING FUTURES BREAKOUT V3 STRATEGY ===")

# Test 1: Levels calculation
try:
    levels = futures_breakout.calculate_levels([100, 110, 105, 108], [90, 92, 94, 93])
    test("HH4 and HH2", levels.hh4 == Decimal(110) and levels.hh2 == Decimal(108))
    test("LL4 and LL2", levels.ll4 == Decimal(90) and levels.ll2 == Decimal(93))
    test("Buy entry level formula (hh4 * (1 + BUFFER))", levels.buy_entry == Decimal("110.132"))
    test("Sell entry level formula (ll4 * (1 - BUFFER))", levels.sell_entry == Decimal("89.892"))
    test("Buy target formula (entry * 1.015)", levels.buy_target == Decimal("111.78398"))
    test("Buy SL1 (max percentage_stop, ll2 * (1 - BUFFER))", levels.buy_sl1 == max(levels.buy_entry * Decimal("0.985"), Decimal(93) * (1 - futures_breakout.BUFFER)))
except Exception as e:
    test("Futures calculation", False, str(e))

# Test 2: Missed entry plan
try:
    plan_none = futures_breakout.missed_entry_plan("100", "110", "90")
    test("Missed entry plan: NONE_MISSED", plan_none.label == "NONE_MISSED" and not plan_none.buy_missed and not plan_none.sell_missed)
    plan_buy = futures_breakout.missed_entry_plan("110", "110", "90")
    test("Missed entry plan: BUY_MISSED", plan_buy.label == "BUY_MISSED" and plan_buy.buy_missed and not plan_buy.sell_missed)
    plan_sell = futures_breakout.missed_entry_plan("90", "110", "90")
    test("Missed entry plan: SELL_MISSED", plan_sell.label == "SELL_MISSED" and not plan_sell.buy_missed and plan_sell.sell_missed)
except Exception as e:
    test("Missed entry plan", False, str(e))

# Test 3: Opening range recovery entries
try:
    plan = futures_breakout.missed_entry_plan("110.01", "110", "90")
    buy, sell = futures_breakout.opening_range_entries(plan, "112", "108")
    test("Opening range buy recovery (OR high + buffer)", buy == Decimal("112.1344") and sell is None)
except Exception as e:
    test("Opening range recovery", False, str(e))

# Test 4: Gap classification
try:
    test("Gap neutral at boundary", futures_breakout.classify_gap("110", "110", "90") == "NEUTRAL")
    test("Gap UP above HH4", futures_breakout.classify_gap("110.01", "110", "90") == "UP")
    test("Gap DOWN below LL4", futures_breakout.classify_gap("89.99", "110", "90") == "DOWN")
except Exception as e:
    test("Gap classification", False, str(e))

# Test 5: Exit levels for SELL
try:
    sell = futures_breakout.exit_levels_for_entry("SELL", 100, 110, 90, 120, 80)
    test("Sell target formula (100 * (1 - 0.015))", sell.target == Decimal("98.500"))
    test("Sell SL1 min percentage stop vs hh2 buffer", sell.sl1 == min(Decimal("101.500"), Decimal(110) * (1 + futures_breakout.BUFFER)))
    test("Sell SL2 min percentage stop vs hh4 buffer", sell.sl2 == min(Decimal("101.500"), Decimal(120) * (1 + futures_breakout.BUFFER)))
except Exception as e:
    test("Sell exit levels", False, str(e))

# Test 6: Directional tick rounding
try:
    test("Directional tick: BUY ceiled to 0.05", common.normalize_to_tick("100.021", "0.05", "BUY") == Decimal("100.05"))
    test("Directional tick: SELL floored to 0.05", common.normalize_to_tick("100.021", "0.05", "SELL") == Decimal(100))
except Exception as e:
    test("Tick normalization", False, str(e))

print("\n=== 2. TESTING SUPERTREND INDEX OPTIONS V1 STRATEGY ===")

# Test 7: Wilder RMA & True Range
try:
    rma = supertrend.wilder_rma([Decimal(1), Decimal(2), Decimal(3), Decimal(4)], 3)
    test("Wilder RMA seeds on period 3", rma[:2] == [None, None] and rma[2] == Decimal(2))
    # Next value: (2 * (3 - 1) + 4) / 3 = 8 / 3
    test("Wilder RMA recurrence step", rma[3] == Decimal("8") / Decimal("3"))
except Exception as e:
    test("Wilder RMA", False, str(e))

# Test 8: Supertrend points & signal transition
try:
    closes = [100, 99, 98, 97, 96, 95, 94, 93, 92, 91, 90, 91, 92, 93, 108]
    points = supertrend.supertrend_points(
        [
            candle(ts(9, 15) + timedelta(minutes=i * 5), str(close))
            for i, close in enumerate(closes)
        ],
        atr_period=3,
        factor=1,
    )
    test("SuperTrend points count", len(points) == len(closes) - 2)
    test("SuperTrend flip from DOWN to UP", points[-2].direction == "DOWN" and points[-1].direction == "UP")
    sig = supertrend.current_signal(points, ts(10, 30))
    test("Current signal extracted correctly as CALL", sig is not None and sig.side == supertrend.OptionSide.CALL)
    test("Signal index close matches latest candle", sig is not None and sig.index_close == Decimal("108"))
except Exception as e:
    test("SuperTrend flip & signal", False, str(e))

# Test 9: Signal freshness (max 90s delay)
try:
    sig = supertrend.current_signal(points, ts(10, 30))
    # closed_at = 10:25 + 5m = 10:30
    test("Signal is fresh within 90s window (at 10:31:00)", supertrend.signal_is_fresh(sig, ts(10, 31)))
    test("Signal is stale beyond 90s (at 10:31:31)", not supertrend.signal_is_fresh(sig, ts(10, 31) + timedelta(seconds=31)))
    test("Signal is stale prior to candle close (at 10:29:59)", not supertrend.signal_is_fresh(sig, ts(10, 29) + timedelta(seconds=59)))
except Exception as e:
    test("Signal freshness", False, str(e))

# Test 10: ATM contract selection & expiry tie break
try:
    contracts = [
        supertrend.OptionContract("far", "NIFTY27AUG", date(2026, 8, 27), 75, Decimal(25000), supertrend.OptionSide.CALL),
        supertrend.OptionContract("low", "NIFTY20AUG", date(2026, 8, 20), 75, Decimal(24950), supertrend.OptionSide.CALL),
        supertrend.OptionContract("atm", "NIFTY20AUG", date(2026, 8, 20), 75, Decimal(25050), supertrend.OptionSide.CALL),
    ]
    chosen = supertrend.choose_atm_contract(contracts, "25060")
    test("Choose ATM contract nearest to strike", chosen.token == "atm" and chosen.strike == Decimal(25050))
except Exception as e:
    test("ATM contract selection", False, str(e))

# Test 11: Continuous session validation
try:
    friday = datetime(2026, 8, 21, 14, 40, tzinfo=ZoneInfo("Asia/Kolkata"))
    monday = datetime(2026, 8, 24, 9, 15, tzinfo=ZoneInfo("Asia/Kolkata"))
    previous = [candle(friday + timedelta(minutes=i * 5), "100") for i in range(9)]
    selected, prev_date = supertrend.continuous_session_candles(
        previous + [candle(monday, "110")], monday.date(), monday
    )
    test("Continuous session validates previous date and candles", prev_date.isoformat() == "2026-08-21" and len(selected) == 10)
except Exception as e:
    test("Continuous session validation", False, str(e))

# Test 12: Missing previous session or missing intermediate candles raises ValueError
try:
    raised_empty = False
    try:
        supertrend.continuous_session_candles([candle(monday, "110")], monday.date(), monday)
    except ValueError as e:
        raised_empty = "previous trading-session candles are missing" in str(e)
    test("Missing previous session raises error", raised_empty)

    raised_gap = False
    try:
        # Expected monday 9:15 and 9:20, but 9:15 is missing
        supertrend.continuous_session_candles(previous + [candle(monday + timedelta(minutes=5), "110")], monday.date(), monday + timedelta(minutes=5))
    except ValueError as e:
        raised_gap = "not available yet" in str(e)
    test("Missing intermediate candle raises error", raised_gap)
except Exception as e:
    test("Continuous session errors", False, str(e))

print("\n=== 3. TESTING TIMING AND SESSION GUARDS ===")

# Test 13: Timing cutoffs
try:
    test("SuperTrend entry disallowed before 09:15", not common.supertrend_entry_allowed(ts(9, 14)))
    test("SuperTrend entry allowed at 09:15", common.supertrend_entry_allowed(ts(9, 15)))
    test("SuperTrend entry allowed at 15:09", common.supertrend_entry_allowed(ts(15, 9)))
    test("SuperTrend entry disallowed at 15:10 cutoff", not common.supertrend_entry_allowed(ts(15, 10)))
    test("SuperTrend EOD square off due at 15:10", common.supertrend_eod_due(ts(15, 10)))
    test("SuperTrend EOD square off due after 15:10 (e.g., 15:20)", common.supertrend_eod_due(ts(15, 20)))
    test("SuperTrend EOD square off NOT due before 15:10", not common.supertrend_eod_due(ts(15, 9)))
except Exception as e:
    test("Timing cutoffs", False, str(e))

# Test 14: Weekend and session checks
try:
    is_open_weekday, _ = scheduler.session_is_open(ts(10, 0))
    is_open_weekend, reason = scheduler.session_is_open(ts(10, 0) + timedelta(days=5))
    test("Session open on Monday 10:00", is_open_weekday)
    test("Session closed on Saturday", not is_open_weekend and reason == "Weekend")
    test("Futures session open during morning slot (09:00 - 15:20)", common.futures_session_open(ts(9, 0)) and common.futures_session_open(ts(15, 20)))
    test("Futures session closed between sessions (15:21 - 16:59)", not common.futures_session_open(ts(16, 0)))
    test("Futures session open during evening slot (17:00 - 23:25)", common.futures_session_open(ts(17, 0)) and common.futures_session_open(ts(23, 25)))
except Exception as e:
    test("Session intervals", False, str(e))

print(f"\n==========================================")
print(f"RESULTS: {passed} PASSED, {failed} FAILED")
print(f"==========================================")
sys.exit(0 if failed == 0 else 1)
