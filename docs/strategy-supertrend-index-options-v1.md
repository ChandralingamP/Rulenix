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
- SuperTrend ATR period: `10`
- SuperTrend multiplier/factor: `3.0`

The runtime only acts after a completed 5-minute candle. It does not use
intrabar flips.

Entry rules:

- SuperTrend flips from downtrend to uptrend: buy ATM CE.
- SuperTrend flips from uptrend to downtrend: buy ATM PE.

## Contract selection

At signal time the backend:

1. Gets current underlying index LTP.
2. Loads the Angel One contract master.
3. Selects nearest-expiry `OPTIDX` contracts:
   - SENSEX from BFO
   - NIFTY from NFO
4. Selects the strike nearest to the underlying LTP.
5. Fetches selected option LTP and places a MARKET buy entry.

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

Entries run from 09:20 IST until before 15:20 IST.

At/after 15:20 IST, the strategy attempts intraday square-off for open
positions. Active protective orders are cancelled before square-off when
possible.

## Backtesting

This strategy is live/demo runtime only. User-facing backtesting remains
available only for Futures Breakout v3.
