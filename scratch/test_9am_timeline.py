import sys
from pathlib import Path
from decimal import Decimal
from datetime import datetime, date, time, timedelta
from zoneinfo import ZoneInfo
import importlib.util
import types

backend_path = Path(r"C:\GitHub\TradingSVCV\Rulenix\python-backend")

def load_module(name: str, file_path: Path):
    spec = importlib.util.spec_from_file_location(name, file_path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod

app_mod = types.ModuleType("app")
app_strategy_mod = types.ModuleType("app.strategy")
sys.modules["app"] = app_mod
sys.modules["app.strategy"] = app_strategy_mod

common = load_module("app.strategy.common", backend_path / "app" / "strategy" / "common.py")
futures_breakout = load_module("app.strategy.futures_breakout", backend_path / "app" / "strategy" / "futures_breakout.py")
supertrend = load_module("app.strategy.supertrend", backend_path / "app" / "strategy" / "supertrend.py")
scheduler = load_module("app.strategy.scheduler", backend_path / "app" / "strategy" / "scheduler.py")

IST = ZoneInfo("Asia/Kolkata")
today = date(2026, 10, 28)

def dt(h, m):
    return datetime.combine(today, time(h, m), tzinfo=IST)

print("=" * 60)
print("TIMELINE ANALYSIS: STRATEGY BEHAVIOR AFTER 9:00 AM")
print("=" * 60)

test_times = [
    (8, 59, "Pre-market before 9am"),
    (9, 0,  "9:00 AM exact (MCX open)"),
    (9, 5,  "9:05 AM (MCX active, Equity pre-open)"),
    (9, 10, "9:10 AM (MCX Day schedule evaluation due)"),
    (9, 14, "9:14 AM (1 min before Equity market open)"),
    (9, 15, "9:15 AM (Equity market open, SuperTrend entry starts)"),
    (9, 16, "9:16 AM (MCX Opening Range breakout recovery due)"),
    (9, 20, "9:20 AM (First 5-min candle closed for SuperTrend)"),
    (9, 30, "9:30 AM (Both strategies active)"),
    (10, 0, "10:00 AM (Mid-morning active)"),
    (12, 0, "12:00 PM (Mid-day active)"),
    (15, 0, "3:00 PM (Afternoon active)"),
    (15, 9, "3:09 PM (Last minute for SuperTrend entry)"),
    (15, 10, "3:10 PM (SuperTrend EOD square-off cutoff)"),
    (15, 20, "3:20 PM (MCX Day session close)"),
    (15, 25, "3:25 PM (Between MCX sessions)"),
    (17, 0,  "5:00 PM (MCX Evening session open)"),
    (17, 10, "5:10 PM (MCX Evening schedule evaluation due)"),
    (23, 25, "11:25 PM (MCX Evening session close)"),
    (23, 30, "11:30 PM (All markets closed)"),
]

for h, m, label in test_times:
    t = dt(h, m)
    fb_open = common.futures_session_open(t)
    st_entry = common.supertrend_entry_allowed(t)
    st_eod = common.supertrend_eod_due(t)
    sch_fb = scheduler.scheduler_action_allowed("futures_breakout_v3", t)
    sch_st = scheduler.scheduler_action_allowed("supertrend_index_options_v1", t)
    
    print(f"[{h:02d}:{m:02d} IST] {label}")
    print(f"   -> Futures Breakout: session_open={fb_open}, action_allowed={sch_fb}")
    print(f"   -> SuperTrend:       entry_allowed={st_entry}, eod_square_off={st_eod}, action_allowed={sch_st}")

