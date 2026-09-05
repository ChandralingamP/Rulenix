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

The SuperTrend series is continuous across trading sessions. Rulenix loads the
most recent prior trading session (skipping weekends and holidays), preserves
the rolling ATR/bands/direction from the historical lookback, and then appends
today's candles. This allows the first 09:15 candle to flip directly from the
previous session's downtrend to an uptrend, or vice versa. The 09:15 candle is
acted on after it closes at approximately 09:20; its opening tick alone is not
a confirmed, non-repainting crossover.

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
6. Persists one confirmed signal and a durable execution intent for every
   eligible user. A bounded worker pool fans out the same selected contract;
   each user keeps their own lot, TP, SL, risk controls, and broker-session
   checks.

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

After an entry fill, the backend uses this safety sequence:

1. Persist the real fill and open trade as `PROTECTION_REQUIRED`.
2. Persist and submit the stop as `STOPLOSS_MARKET SELL`.
3. Reconcile the broker order book; local submission alone is not protection confirmation.
4. Mark the trade `PROTECTED` only when the stop is broker-acknowledged for the required quantity.
5. Submit the `LIMIT SELL` target only after confirmed protection.

An ambiguous stop blocks the target and all compounding entries until bounded order/position reconciliation confirms the broker outcome or escalates to operator-required state. A rejected stop enters deterministic protection retry/emergency-close policy.

When one protective exit fills, the existing shared fill handler cancels the
remaining active exit order.

## Trading window

The scheduler evaluates SuperTrend on exact 5-minute boundaries from 09:15 IST
until 15:10 IST. Entries and reversals are allowed from 09:15 IST until before
15:10 IST. Only completed 5-minute candles are used. A shared Angel websocket
continuously builds the SENSEX and NIFTY candles once for all users. Historical
REST candles warm the previous-session state and recover websocket gaps.

The just-closed candle must be present and continuous before evaluation. Rulenix
retries a briefly delayed candle inside the same boundary, but it will not replay
an old crossover at the next 5- or 10-minute cycle. Entry intents expire 90
seconds after the signal candle closes, preventing a slow quote/contract lookup
from creating a stale trade.

At/after 15:10 IST, the strategy creates durable intraday square-off intents for
all open positions. Active protective orders are cancelled before square-off.
The watchdog keeps retrying after 15:30 and across backend restarts until broker
reconciliation confirms closure. No new SuperTrend entries are submitted at or
after 15:10 IST.

The existing stop remains active if the square-off quote cannot be obtained. A MARKET close is attempted only after protective cancellation is broker-terminal. Rejected attempts remain in history and retries use a new deterministic attempt key; an active or ambiguous MARKET close is never blindly duplicated.

## Runtime concurrency

The backend uses Tokio asynchronous tasks; it does not reserve an operating-
system thread for every user or instrument. The production container currently
has two CPU cores, so async tasks are multiplexed over two Tokio worker threads.

- One leader scheduler task dispatches the SuperTrend cycle.
- SENSEX and NIFTY candle/signal evaluations run independently, so a slow or
  failed SENSEX request cannot delay NIFTY (or vice versa). Shared endpoint
  gates still keep the fallback REST calls within Angel One's limits.
- A confirmed instrument signal performs one shared index/ATM-contract lookup.
- A fresh database audience is frozen at signal time and processed through
  durable, bounded concurrent tasks. One user's slow broker response or failure
  does not hold or cancel another user's task.
- One physical shared market WebSocket carries all required NSE/BSE index and
  NFO/BFO/MCX contract groups, rather than opening sockets per user, token, or
  exchange. This remains below Angel One's per-client connection cap.
- A per-user signal/session guard prevents the same closed-candle signal from
  being entered twice during the recovery window.

See [the SuperTrend runtime flowchart](supertrend-runtime-flow.svg).

## Backtesting

This strategy is live/demo runtime only. User-facing backtesting remains
available only for Futures Breakout v3.
