# Future Breakout mismatch table (pre-fix)

Generated against Rust `3f788f2a842ef9b1b66366d439431867850e3753` before changing
Python. All twelve original fixtures return status 200 except `fb-insufficient-history`,
which returns 422 in both runtimes.

| fixture_id | input | Rust output | Python output | fields differing | root cause |
|---|---|---|---|---|---|
| `fb-neutral` | `highs=[100,110,105,108], lows=[90,92,94,93], market_open=100` | `buy_entry=110.132, buy_sl1=108.48002000000001, buy_sl2=108.48002000000001, sell_entry=89.892, sell_sl1=91.24037999999999, sell_sl2=91.24037999999999, sell_target=88.54361999999999, missed=NONE_MISSED` | `buy_entry=110.132, buy_sl1=108.48002, buy_sl2=108.48002, sell_entry=89.892, sell_sl1=91.24038, sell_sl2=91.24038, sell_target=88.54362, missed=NONE_MISSED` | `buy_sl1,buy_sl2,sell_sl1,sell_sl2,sell_target` | Rust performs IEEE-754 `f64` operations; Python calculates exact `Decimal` then converts the final value to float. |
| `fb-gap-up` | same history, `market_open=111` | same numeric values as `fb-neutral`, `missed=BUY_MISSED` | same decimal values, `missed=BUY_MISSED` | `buy_sl1,buy_sl2,sell_sl1,sell_sl2,sell_target` | Numeric representation only; gap/missed-entry boundary agrees. |
| `fb-gap-down` | same history, `market_open=89` | same numeric values as `fb-neutral`, `missed=SELL_MISSED` | same decimal values, `missed=SELL_MISSED` | `buy_sl1,buy_sl2,sell_sl1,sell_sl2,sell_target` | Numeric representation only; gap/missed-entry boundary agrees. |
| `fb-open-equals-hh4` | same history, `market_open=110` | same numeric values, `missed=NONE_MISSED` | same decimal values, `missed=NONE_MISSED` | `buy_sl1,buy_sl2,sell_sl1,sell_sl2,sell_target` | Inclusive/exclusive boundary agrees; only f64 serialization differs. |
| `fb-open-equals-ll4` | same history, `market_open=90` | same numeric values, `missed=NONE_MISSED` | same decimal values, `missed=NONE_MISSED` | `buy_sl1,buy_sl2,sell_sl1,sell_sl2,sell_target` | Inclusive/exclusive boundary agrees; only f64 serialization differs. |
| `fb-buy` | same history, `direction=BUY, entry=110.132` | levels as above plus `exit={target:111.78398,sl1:108.48002000000001,sl2:108.48002000000001}` | levels as above plus `exit={target:111.78398,sl1:108.48002,sl2:108.48002}` | `buy_sl1,buy_sl2,exit.sl1,exit.sl2,sell_sl1,sell_sl2,sell_target` | Same BUY path and operation order; Decimal-to-float differs from Rust intermediate f64. |
| `fb-sell` | same history, `direction=SELL, entry=89.892` | levels as above plus `exit={target:88.54361999999999,sl1:91.24037999999999,sl2:91.24037999999999}` | levels as above plus `exit={target:88.54362,sl1:91.24038,sl2:91.24038}` | `buy_sl1,buy_sl2,exit.sl1,exit.sl2,exit.target,sell_sl1,sell_sl2,sell_target` | Same SELL path and operation order; Decimal-to-float differs from Rust intermediate f64. |
| `fb-buffer-target-sl` | `highs=[123.45,126.78,124.11,125.55], lows=[119.02,120.14,121.33,120.88], direction=BUY, entry=126.93132` | `buy_entry=126.93213600000001, buy_sl1=125.02815396000001, sell_entry=118.87717599999999, sell_sl1=120.66033363999998, sell_target=117.09401835999999` | `buy_entry=126.932136, buy_sl1=125.02815396, sell_entry=118.877176, sell_sl1=120.66033364, sell_target=117.09401836` | `buy_entry,buy_sl1,buy_sl2,sell_entry,sell_sl1,sell_sl2,sell_target` | Rust f64 multiplication order is preserved only when the Python external serializer follows the same float path. |
| `fb-missed-boundary-buy` | same history, `market_open=110.132` | numeric values as above, `missed=BUY_MISSED` | same decimal values, `missed=BUY_MISSED` | `buy_sl1,buy_sl2,sell_sl1,sell_sl2,sell_target` | Exact equality uses `>=` and agrees; numeric representation only. |
| `fb-missed-boundary-sell` | same history, `market_open=89.892` | numeric values as above, `missed=SELL_MISSED` | same decimal values, `missed=SELL_MISSED` | `buy_sl1,buy_sl2,sell_sl1,sell_sl2,sell_target` | Exact equality uses `<=` and agrees; numeric representation only. |
| `fb-insufficient-history` | `highs=[100,110,105], lows=[90,92,94]` | `status=422, {code:fixture_error,message:insufficient breakout history}` | `status=422, {code:fixture_error,message:Future Breakout requires exactly four historical candles.}` | `body.message` | Same result/status/code; externally visible domain error text differs. |
| `fb-duplicate-evaluation` | same history, `market_open=100` | same numeric values as `fb-neutral`, `missed=NONE_MISSED` | same decimal values, `missed=NONE_MISSED` | `buy_sl1,buy_sl2,sell_sl1,sell_sl2,sell_target` | Numeric representation only; duplicate evaluation semantics agree. |

Rust calculation path: `strategy::calculate` → `futures_exit_levels_for_entry` →
`f64` multiplication/max/min → serde JSON `f64`; missed-entry uses
`futures_missed_entry_plan` with `>=`/`<=`.

Python calculation path: `calculate_levels` → `exit_levels_for_entry` → exact
`Decimal` multiplication/max/min → adapter `float(Decimal)` → JSON; missed-entry uses
the same inclusive comparisons. `normalize_to_tick` was independently compared for
BUY and SELL and matched exactly, so tick rounding is not the cause of these failures.
