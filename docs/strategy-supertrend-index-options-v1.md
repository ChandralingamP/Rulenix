# SuperTrend Index Options v1

Strategy key: `supertrend_index_options_v1`

Implementation:

- Live/demo runtime: `backend/src/strategy.rs`
- Frontend strategy UI: `frontend/src/pages/StrategiesPage.jsx`

## Scope

This is an intraday options-only strategy for:

- SENSEX ATM CE/PE options
- NIFTY ATM CE/PE options

Signals are calculated from the underlying index candles. The strategy does not
calculate SuperTrend on option premium candles.

## Indicator and signal

Defaults:

- Interval: `FIVE_MINUTE`
- SuperTrend ATR period: `7`
- SuperTrend multiplier/factor: `2.0`

The runtime only acts after a completed 5-minute candle. It does not use
intrabar flips.

Entry rules:

- SuperTrend flips from downtrend to uptrend: buy ATM CE.
- SuperTrend flips from uptrend to downtrend: buy ATM PE.
- If the opposite SuperTrend option trade is still open, cancel its active
  protective exits, close it with a MARKET SELL square-off, then place the new
  ATM option BUY entry.
- Entries are long options only. The strategy never opens short option
  positions; SELL orders are used only to close existing long CE/PE trades.

## Contract selection

At signal time the backend:

1. Gets current underlying index LTP.
2. Loads the Angel One contract master.
3. Selects nearest-expiry `OPTIDX` contracts:
   - SENSEX from BFO
   - NIFTY from NFO
4. Selects the strike nearest to the underlying LTP.
5. Fetches the selected option LTP once for that instrument signal.
6. Fans the same selected contract out to isolated concurrent per-user MARKET
   buy entry tasks. Each user keeps their own lot, TP, SL, risk, margin, and
   broker-session checks.

## User configuration

Each user can configure per instrument:

- enabled flag
- lot size
- TP points
- SL points

Defaults:

- SENSEX: TP 40 points, SL 25 points
- NIFTY: TP 25 points, SL 15 points

TP/SL are applied to the option entry fill. Example:

```text
Entry fill: 200
TP points: 40  -> target 240
SL points: 25  -> stop 175
```

After an entry fill, the backend places:

- target: LIMIT SELL
- stop: STOPLOSS_LIMIT SELL

When one protective exit fills, the existing shared fill handler cancels the
remaining active exit order.

## Trading window

The scheduler evaluates SuperTrend on exact 5-minute boundaries from 09:15 IST
until 15:20 IST. Entries and reversals are allowed from 09:15 IST until before
15:20 IST. Only completed 5-minute candles are used.

At/after 15:20 IST, the strategy attempts intraday square-off for open
positions. Active protective orders are cancelled before square-off when
possible. No new SuperTrend entries are submitted at or after 15:20 IST.

## Runtime concurrency

The backend uses Tokio asynchronous tasks; it does not reserve an operating-
system thread for every user or instrument. The production container currently
has two CPU cores, so async tasks are multiplexed over two Tokio worker threads.

- One leader scheduler task dispatches the SuperTrend cycle.
- SENSEX and NIFTY candle/signal evaluation is intentionally sequential to
  avoid bursting Angel One's historical-data API.
- A confirmed instrument signal performs one shared index/ATM-contract lookup.
- Eligible users are then processed concurrently in isolated tasks. One user's
  slow broker response or failure does not hold or cancel another user's task.
- One shared market WebSocket task is used per active exchange (BFO/NFO), not
  per user and not per token.
- A per-user signal/session guard prevents the same closed-candle signal from
  being entered twice during the recovery window.

See [the SuperTrend runtime flowchart](supertrend-runtime-flow.svg).

## Backtesting

This strategy is live/demo runtime only. User-facing backtesting remains
available only for Futures Breakout v3.
