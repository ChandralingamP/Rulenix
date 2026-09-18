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

Stops (the executable max/min formulas are authoritative):

- Buy SL1: `MAX(entry * 0.985, LL2 * 0.9988)`
- Buy SL2: `MAX(entry * 0.985, LL4 * 0.9988)`
- Sell SL1: `MIN(entry * 1.015, HH2 * 1.0012)`
- Sell SL2: `MIN(entry * 1.015, HH4 * 1.0012)`

## Gap-entry behavior

BUY and SELL are evaluated independently. Previous-day close is retained as market context only; it never selects trade direction.

- `buy_missed = session_open >= BUY_ENTRY`
- `sell_missed = session_open <= SELL_ENTRY`

If a side already opened beyond its standard entry level, that side's trigger is considered jumped. The strategy waits for the completed 09:00-09:15 IST range for the missed side:

- Missed BUY opening-range entry: `opening_range_high * 1.0012`
- Missed SELL opening-range entry: `opening_range_low * 0.9988`

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

Each scheduled action has a 15-minute catch-up window after restart. Transient failures retry every 30 seconds.

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

Before order creation, the risk engine checks permissions, kill switches, position/order limits, fresh ticks, and broker/session health.

Normal entry quantity must equal configured lots multiplied by the current Angel One contract-master lot size. Limit and trigger prices are normalized to tick size and checked against the token's current Angel One FULL-quote lower/upper circuit limits. A missing authoritative price band blocks a new live entry.

One user failing risk or broker validation does not make successful users retry or roll back their already submitted orders.

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

## SL2 reversal execution

- A reversal is created only from a fully processed `SL2` execution. A price touch, trigger-pending order, submitted stop, or partial fill is not sufficient.
- The source trade is closed first and a unique durable `strategy_reversal_intents.source_trade_id` row is committed in the same transaction as the SL2 fill accounting.
- The reversal entry keeps the source trade ID as lineage, but is validated as a new `BUY_ENTRY`/`SELL_ENTRY`, not as an exit against the already-closed source trade.
- LIVE reversals wait for a successful broker position reconciliation proving the source contract flat. DEMO reversals use the deterministic simulated-fill path and never call Angel One.
- The normal entry pipeline remains authoritative for kill switches, LIVE permission, session health, egress selection, risk limits, quantity validation, idempotency, and ambiguous-write handling.
- Restarts reclaim durable intents and reuse the stable reversal session key, so repeated fill/reconciliation observations cannot create a second reversal order.

This prevents rows like repeated SELL entries at the same level while keeping legitimate opposite-side/reversal behavior.

## Demo vs live execution

Demo:

- Strategy orders are stored locally.
- Shared live market feed ticks simulate fills.
- Demo P&L is updated locally.

Live:

- Orders are submitted to Angel One.
- Stable client order IDs/tags are used.
- Ambiguous submissions are reconciled instead of blindly retried.
- Partial fills update cumulative filled/processed quantities.
- Every partial fill delta receives an exact stop slice; database coverage checks prevent total active stop coverage from exceeding local exposure.
- Broker events are stored for audit.

Both modes persist into `strategy_orders` and `trades`.

## Manual close and broker reconciliation

- An operator can close an open DEMO trade from Profit/Loss. The backend locks the trade, requires a fresh positive database market tick, terminalizes active demo protection locally, and calculates the final P&L without calling Angel One. Repeated close requests are idempotent and ownership is enforced from the authenticated session.
- Closing a LIVE trade from Profit/Loss keeps the existing broker-close workflow: it submits or reconciles the tagged broker exit and closes the local trade only after authoritative broker evidence proves the contract flat.
- If a LIVE position is closed directly at Angel One, reconciliation accepts only an exact, opposite-side set of external fills for the same exchange, token, and symbol after the last exact broker/local exposure match. Partial, excess, stale, conflicting, malformed, or ambiguous evidence fails closed.
- Exact external-close evidence is stored before protective-order cleanup. This lets a later reconciliation pass finish the local close after protection becomes terminal even if Angel's intraday trade book has expired or is temporarily unavailable. The stored evidence is scoped to the user, account, trade, contract, side, and exact quantity and is consumed once.
- A broker-flat state without attributable fill evidence remains `RECONCILIATION_REQUIRED`; it is never converted into a guessed local close.

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
- If a live user is skipped while others execute, inspect risk decisions, broker token health, and broker order events for that user.
- If backtest trade counts change with lots, first check target split/runner behavior and same-side duplicate guards.
