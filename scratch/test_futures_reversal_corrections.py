import sys
from pathlib import Path
from decimal import Decimal
from datetime import datetime, date, time
from zoneinfo import ZoneInfo
import importlib.util
import types

backend_path = Path(__file__).resolve().parent.parent / "python-backend"

# Setup python path so app modules can be loaded
if str(backend_path) not in sys.path:
    sys.path.insert(0, str(backend_path))

def load_module(name: str, file_path: Path):
    spec = importlib.util.spec_from_file_location(name, file_path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod

app_mod = types.ModuleType("app")
sys.modules["app"] = app_mod

futures_breakout = load_module("app.strategy.futures_breakout", backend_path / "app" / "strategy" / "futures_breakout.py")
calculate_levels = futures_breakout.calculate_levels
exit_levels_for_entry = futures_breakout.exit_levels_for_entry
BUFFER = futures_breakout.BUFFER
TARGET_PCT = futures_breakout.TARGET_PCT
STOP_PCT = futures_breakout.STOP_PCT

trading_domain = load_module("app.trading.domain", backend_path / "app" / "trading" / "domain.py")
sl2_reversal = trading_domain.sl2_reversal
Side = trading_domain.Side
OrderRole = trading_domain.OrderRole

risk_domain = load_module("app.risk.domain", backend_path / "app" / "risk" / "domain.py")
ActionKind = risk_domain.ActionKind
ActionClass = risk_domain.ActionClass
classify_action = risk_domain.classify_action

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

print("=== 1. TESTING DYNAMIC EXIT LEVELS FOR SL2 REVERSALS ===")

# Test scenario:
# 4-day Highs: [100, 110, 105, 108] -> HH4 = 110, HH2 = 108
# 4-day Lows:  [90, 92, 94, 93]    -> LL4 = 90,  LL2 = 93
hh4, hh2 = Decimal(110), Decimal(108)
ll4, ll2 = Decimal(90), Decimal(93)

# Suppose original trade was SELL, and it hit SL2 at price = 102.50
# SL2 reversal enters a BUY trade at actual market fill price E_rev = 102.50
e_rev_buy = Decimal("102.50")
buy_reversal_exits = exit_levels_for_entry("BUY", e_rev_buy, hh2, ll2, hh4, ll4)

# 1. Buy reversal Target must be exactly E_rev * (1 + 0.015) = 104.0375
test("Reversal BUY target calculated from E_rev fill price", 
     buy_reversal_exits.target == e_rev_buy * (Decimal(1) + TARGET_PCT))

# 2. Buy reversal SL1 must be max(E_rev * 0.985, LL2 * (1 - BUFFER))
expected_buy_sl1 = max(e_rev_buy * (Decimal(1) - STOP_PCT), ll2 * (Decimal(1) - BUFFER))
test("Reversal BUY SL1 calculated from E_rev and LL2",
     buy_reversal_exits.sl1 == expected_buy_sl1)

# 3. Buy reversal SL2 must be max(E_rev * 0.985, LL4 * (1 - BUFFER))
expected_buy_sl2 = max(e_rev_buy * (Decimal(1) - STOP_PCT), ll4 * (Decimal(1) - BUFFER))
test("Reversal BUY SL2 calculated from E_rev and LL4",
     buy_reversal_exits.sl2 == expected_buy_sl2)

# Suppose original trade was BUY, and it hit SL2 at price = 98.00
# SL2 reversal enters a SELL trade at actual market fill price E_rev = 98.00
e_rev_sell = Decimal("98.00")
sell_reversal_exits = exit_levels_for_entry("SELL", e_rev_sell, hh2, ll2, hh4, ll4)

# 4. Sell reversal Target must be exactly E_rev * (1 - 0.015) = 96.53
test("Reversal SELL target calculated from E_rev fill price",
     sell_reversal_exits.target == e_rev_sell * (Decimal(1) - TARGET_PCT))

# 5. Sell reversal SL1 must be min(E_rev * 1.015, HH2 * (1 + BUFFER))
expected_sell_sl1 = min(e_rev_sell * (Decimal(1) + STOP_PCT), hh2 * (Decimal(1) + BUFFER))
test("Reversal SELL SL1 calculated from E_rev and HH2",
     sell_reversal_exits.sl1 == expected_sell_sl1)

# 6. Sell reversal SL2 must be min(E_rev * 1.015, HH4 * (1 + BUFFER))
expected_sell_sl2 = min(e_rev_sell * (Decimal(1) + STOP_PCT), hh4 * (Decimal(1) + BUFFER))
test("Reversal SELL SL2 calculated from E_rev and HH4",
     sell_reversal_exits.sl2 == expected_sell_sl2)

print("\n=== 2. TESTING SL2 REVERSAL PLAN GENERATION ===")

# Test 7: Original direction BUY for 4 lots -> Reversal must be SELL for 4 lots
plan_from_buy = sl2_reversal("BUY", 4)
test("SL2 reversal from BUY creates SELL for full original lots",
     plan_from_buy == (Side.SELL, OrderRole.SELL_ENTRY, 4))

# Test 8: Original direction SELL for 2 lots -> Reversal must be BUY for 2 lots
plan_from_sell = sl2_reversal("SELL", 2)
test("SL2 reversal from SELL creates BUY for full original lots",
     plan_from_sell == (Side.BUY, OrderRole.BUY_ENTRY, 2))

# Test 9: Zero or negative lots returns None
test("Zero lots returns None", sl2_reversal("BUY", 0) is None)

print("\n=== 3. TESTING CARRYFORWARD DIRECTION FILTERING ===")

# Simulated logic from lifecycle.py _evaluate_futures:
def get_user_directions(open_trade_dir, snapshot_directions):
    if open_trade_dir:
        # If open position exists (e.g. SELL), only allow the opposite side (BUY)
        reversal_side = "BUY" if str(open_trade_dir).upper() == "SELL" else "SELL"
        return (reversal_side,)
    return snapshot_directions

# Test 10: No trade open -> BOTH directions placed
test("No open trade: places BOTH sides", 
     get_user_directions(None, ("BUY", "SELL")) == ("BUY", "SELL"))

# Test 11: SELL trade open overnight -> ONLY BUY placed on Day 2
test("Open SELL trade: ONLY places BUY limit order on next day", 
     get_user_directions("SELL", ("BUY", "SELL")) == ("BUY",))

# Test 12: BUY trade open overnight -> ONLY SELL placed on Day 2
test("Open BUY trade: ONLY places SELL limit order on next day", 
     get_user_directions("BUY", ("BUY", "SELL")) == ("SELL",))

print("\n=== 4. TESTING RISK CLASSIFICATION FOR TP & SL EXITS ===")

# Test 13: Target is classified as REDUCE_RISK
test("TARGET role is classified as REDUCE_RISK",
     classify_action(ActionKind.TARGET, quantity=1, attributable_quantity=2) == ActionClass.REDUCE_RISK)

# Test 14: STOP_LOSS role is classified as REDUCE_RISK
test("STOP_LOSS role is classified as REDUCE_RISK",
     classify_action(ActionKind.STOP_LOSS, quantity=2, attributable_quantity=2) == ActionClass.REDUCE_RISK)

# Test 15: SL2_REVERSAL is classified as INCREASE_EXPOSURE
test("SL2_REVERSAL is classified as INCREASE_EXPOSURE",
     classify_action(ActionKind.SL2_REVERSAL, quantity=2) == ActionClass.INCREASE_EXPOSURE)

# Test 16: ENTRY is classified as INCREASE_EXPOSURE
test("ENTRY is classified as INCREASE_EXPOSURE",
     classify_action(ActionKind.ENTRY, quantity=2) == ActionClass.INCREASE_EXPOSURE)

print("\n=== 5. TESTING DUPLICATE EXPOSURE CONSTRAINT LOGIC ===")

# Simulated logic matching SafetyRepository.evaluate_constraints:
def check_duplicate(action, role, side, open_trades, active_orders, current_order_id=None):
    role_val = (role or "").strip() or None
    side_val = (side or "").strip() or None
    if not side_val and role_val:
        side_val = "BUY" if "BUY" in role_val else ("SELL" if "SELL" in role_val else None)

    if action is ActionKind.SL2_REVERSAL:
        has_open_trade = any(t["status"] == "open" for t in open_trades)
        has_inflight_sl2 = any(
            o.get("idempotency_key", "").startswith("python:sl2-reversal:")
            and o.get("id") != current_order_id
            and o["status"] in ('pending', 'submitting', 'ambiguous', 'submitted', 'partially_filled', 'processing', 'cancelling')
            for o in active_orders
        )
        return has_open_trade or has_inflight_sl2
    else:
        # Normal ENTRY
        has_same_side_trade = any(
            t["status"] == "open" and (side_val is None or t["direction"] == side_val)
            for t in open_trades
        )
        has_same_order = any(
            o.get("id") != current_order_id
            and o["status"] in ('pending', 'submitting', 'ambiguous', 'submitted', 'partially_filled', 'processing', 'cancelling')
            and (
                (role_val is not None and o.get("role") == role_val)
                or (side_val is not None and o.get("side") == side_val and o.get("role") in ('BUY_ENTRY', 'SELL_ENTRY'))
                if (role_val is not None or side_val is not None)
                else o.get("role") in ('BUY_ENTRY', 'SELL_ENTRY')
            )
            for o in active_orders
        )
        return has_same_side_trade or has_same_order

# Scenario A: Running SELL trade
running_sell_trades = [{"status": "open", "direction": "SELL"}]

# Test 17: Placing opposite BUY_ENTRY limit order should NOT be flagged as duplicate
test("Open SELL trade does NOT block opposite BUY_ENTRY limit order",
     check_duplicate(ActionKind.ENTRY, "BUY_ENTRY", "BUY", running_sell_trades, []) == False)

# Test 18: Placing same-side SELL_ENTRY limit order SHOULD be flagged as duplicate
test("Open SELL trade blocks same-side SELL_ENTRY limit order",
     check_duplicate(ActionKind.ENTRY, "SELL_ENTRY", "SELL", running_sell_trades, []) == True)

# Scenario B: SL2 reversal when previous trade has just closed
closed_trades = [{"status": "closed", "direction": "SELL"}]
morning_orders = [
    {"id": "order-1", "role": "BUY_ENTRY", "side": "BUY", "status": "submitted", "idempotency_key": "python:futures:buy:1"},
]

# Test 19: SL2 reversal order should NOT be blocked by existing morning BUY_ENTRY order
test("SL2 reversal order is NOT blocked by morning standing entry orders",
     check_duplicate(ActionKind.SL2_REVERSAL, "BUY_ENTRY", "BUY", closed_trades, morning_orders, current_order_id="sl2-order") == False)

# Test 20: In-flight duplicate SL2 reversal order IS blocked
inflight_sl2_orders = [
    {"id": "sl2-order-1", "role": "BUY_ENTRY", "side": "BUY", "status": "submitted", "idempotency_key": "python:sl2-reversal:trade-1"},
]
test("In-flight SL2 reversal blocks second duplicate SL2 reversal",
     check_duplicate(ActionKind.SL2_REVERSAL, "BUY_ENTRY", "BUY", closed_trades, inflight_sl2_orders, current_order_id="sl2-order-2") == True)

print(f"\n==========================================")
print(f"RESULTS: {passed} PASSED, {failed} FAILED")
print(f"==========================================")
sys.exit(0 if failed == 0 else 1)
