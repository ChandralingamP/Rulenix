# Futures Breakout v3

Strategy key: `futures_breakout_v3`

Implementation:

- Live/demo runtime: `backend/src/strategy.rs`
- Backtesting: `backend/src/backtesting.rs`
- Frontend configuration/status: `frontend/src/pages/StrategiesPage.jsx`
- Backtesting UI: `frontend/src/pages/BacktestingPage.jsx`

## Supported instruments

The strategy supports these MCX futures instruments:

- `GOLDTEN` - Gold Ten
- `GOLDM` - Gold Mini
- `SILVERM` - Silver Mini
- `SILVERMIC` - Silver Micro
- `NATGASMINI` - Natural Gas Mini

Full-size `GOLD` is intentionally not supported.

## Contract selection

For each supported instrument, the backend loads the Angel One contract master and selects an MCX `FUTCOM` contract whose expiry is at least 10 weekdays away from the trade date. Contract token, symbol, expiry, and lot size are cached in `strategy_market_snapshots`.

Daily contract metadata is warmed at startup and retried by the scheduler when missing.

## Daily level calculation

The strategy uses the last four completed daily candles before the trade date.

Definitions:

- `HH2`: highest high of the most recent two completed daily candles.
- `LL2`: lowest low of the most recent two completed daily candles.
- `HH4`: highest high of the most recent four completed daily candles.
- `LL4`: lowest low of the most recent four completed daily candles.

Standard entries:

- Buy entry: `HH4 * 1.0012`
- Sell entry: `LL4 * 0.9988`

Targets:

- Buy target: `entry * 1.015`
- Sell target: `entry * 0.985`

Stops:

- Buy main stop: `entry * 0.985`
- Buy SL1 technical stop: `LL2 * 0.9988`
- Buy SL2 technical stop: `LL4 * 0.9988`
- Sell main stop: `entry * 1.015`
- Sell SL1 technical stop: `HH2 * 1.0012`
- Sell SL2 technical stop: `HH4 * 1.0012`

The final stop level is constrained so it stays on the correct side of the entry. If a technical stop is not useful, the main 1.5% stop is used.

## Gap-entry behavior

At the day session open, the strategy compares market open with the previous close:

- Gap up: entry direction is BUY.
- Gap down: entry direction is SELL.
- Flat: both standard BUY and SELL entries are allowed.

If the market already opened beyond the standard entry level, the standard trigger is considered jumped. In that case, the strategy waits for the completed 09:00-09:15 IST range:

- Gap-up opening-range entry: `opening_range_high * 1.0012`
- Gap-down opening-range entry: `opening_range_low * 0.9988`

This produces an `OPENING_RANGE` entry source. Otherwise the source is `STANDARD`.

## Scheduler timing

The backend scheduler runs under one PostgreSQL advisory-lock leader.

Day session:

- 09:00 IST: carry/refresh target orders for open trades.
- 09:10 IST: carry/refresh stop orders and place normal entries.
- 09:16 IST: place gap opening-range entry if a jumped gap was waiting for the 09:00-09:15 candle.

Evening session:

- 17:00 IST: carry/refresh target orders for open trades.
- 17:10 IST: carry/refresh stop orders and place entries.

Each scheduled action has a 15-minute catch-up window after restart. Transient failures retry every 30 seconds. Terminal margin errors are skipped and recorded.

## User activation/configuration

Strategy activation and instrument configuration are separate.

1. User activates `futures_breakout_v3`.
2. User enables one or more supported instruments.
3. User sets integer lots.
4. User chooses whether day/evening sessions should run.

The backend only selects active users whose instrument is enabled and whose trading mode is valid:

- demo mode is always allowed for active users
- live mode requires `can_live_trade`

## Entry placement

For each configured runner, the backend places STOPLOSS_LIMIT entry orders:

- `BUY_ENTRY` for buy triggers.
- `SELL_ENTRY` for sell triggers.

Before order creation, the risk engine checks permissions, kill switches, position/order limits, margin, fresh ticks, and broker/session health.

One user failing risk/margin/broker validation does not make successful users retry or roll back their already submitted orders.

## Exit management

Each entry becomes a `trades` row after fill.

Target handling:

- If configured lots are `1`, TP1 exits the full lot.
- If configured lots are greater than `1`, TP1 exits `(lots + 1) / 2`, rounded up.
- The remaining lots continue as a runner.

Stop handling:

- Before TP1, stop role is `SL1`.
- After TP1, stop role is `SL2`.
- Target price is fixed from the trade entry.
- SL1/SL2 levels are refreshed daily from the latest levels.

Protective exits are carried across sessions/days rather than cancelling open trades manually.

## SL2 reversal

When SL2 is filled, the strategy can create an opposite-direction reversal intent:

- Source BUY stopped at SL2 creates SELL reversal.
- Source SELL stopped at SL2 creates BUY reversal.
- Reversal uses the original configured lot count.
- Reversal entry price is based on the SL2 exit price.
- Reversal gets fresh target and stop levels.

Reversal intents are persisted in `strategy_reversal_intents`, so restart/reconciliation can recover incomplete reversal placement.

## Same-side duplicate prevention

The current backtest model allows multiple concurrent trades, but not duplicate same-side open trades.

Rules:

- A normal breakout entry is skipped if a same-direction trade is already open.
- An SL2 reversal is skipped if a same-direction trade/reversal is already open or scheduled in that candle.
- Opposite-side trades and valid reversals remain allowed.
- Existing trades are not manually closed just because a new signal appears.

This prevents rows like repeated SELL entries at the same level while keeping legitimate opposite-side/reversal behavior.

## Demo vs live execution

Demo:

- Strategy orders are stored locally.
- Shared live market feed ticks simulate fills.
- Demo balance and P&L are updated locally.

Live:

- Orders are submitted to Angel One.
- Stable client order IDs/tags are used.
- Ambiguous submissions are reconciled instead of blindly retried.
- Partial fills update cumulative filled/processed quantities.
- Broker events are stored for audit.

Both modes persist into `strategy_orders` and `trades`.

## Backtesting behavior

Backtesting supports lookbacks of 1, 3, or 6 months and intervals:

- `ONE_MINUTE`
- `FIVE_MINUTE`
- `FIFTEEN_MINUTE`
- `THIRTY_MINUTE`
- `ONE_HOUR`

Backtesting fetches:

- daily candles from `from_time - 20 days` through `to_time`
- requested interval candles from `from_time` through `to_time`
- extra 15-minute candles when the selected interval cannot describe the 09:00-09:15 opening range precisely

The simulator:

1. Rebuilds daily HH/LL levels.
2. Builds gap/opening-range plans per day.
3. Processes open-position exits first.
4. Opens valid new entries.
5. Opens SL2 reversals when allowed.
6. Leaves surviving positions open until normal exit or `END_OF_TEST`.

`END_OF_TEST` means the test window ended while the trade was still open, so the simulator marks it closed at the last available candle for reporting only. It is not a real strategy exit.

Backtest P&L model:

```text
futures price movement * contract point-value multiplier * lots
```

Per-lot point-value multipliers:

- `GOLDTEN`: 1
- `GOLDM`: 10
- `SILVERM`: 5
- `SILVERMIC`: 1
- `NATGASMINI`: 250

## Main database tables

- `strategy_market_snapshots`: contract metadata, daily candles, HH/LL levels, gap plan.
- `user_strategy_activations`: active/inactive strategy state.
- `user_strategy_configs`: instrument lots and session flags.
- `strategy_scheduler_runs`: idempotent scheduler action tracking.
- `strategy_orders`: entries and protective orders.
- `trades`: open/closed positions.
- `strategy_reversal_intents`: SL2 reversal recovery state.
- `strategy_events`: user-visible strategy events and operational alerts.
- `backtest_market_candles`, `backtest_runs`, `backtest_trades`: backtesting cache/results.

## Operational notes

- Keep MCX holiday/session data in `market_calendar` updated when exchange calendars change.
- Do not manually cancel protective exits unless replacing them with equivalent protection.
- If a live user is skipped while others execute, inspect risk decisions, broker token health, margin estimate, and broker order events for that user.
- If backtest trade counts change with lots, first check target split/runner behavior and same-side duplicate guards.
