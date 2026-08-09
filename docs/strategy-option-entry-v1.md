# Option Entry Strategy V1.0

Strategy key: `option_entry_v1`

Implementation:

- Live/demo runtime: `backend/src/strategy.rs`
- Frontend strategy UI: `frontend/src/pages/StrategiesPage.jsx`

## Scope

The strategy is for SENSEX options.

Runtime/live execution:

- Signal is calculated from SENSEX index candles.
- Contract selection uses BFO SENSEX option contracts.
- Entries are long options only:
  - CALL signal buys CE.
  - PUT signal buys PE.

Backtesting has been removed for Option Entry Strategy V1.0. It is a live/demo runtime strategy only. The backend rejects `option_entry_v1` backtest requests, and the Backtesting page only exposes Futures Breakout v3.

## Market data and indicators

Index token:

- SENSEX index token: `99919000`
- Exchange for index candles: `BSE`
- Runtime signal interval: `FIVE_MINUTE`

Indicators:

- Keltner middle: EMA 20 of close.
- ATR input: true range.
- Keltner ATR EMA: 10.
- Keltner multiplier: 2.0.
- TSI long period: 25.
- TSI short period: 13.
- Entry TSI threshold: `0.5` on the TradingView-style decimal TSI scale.

The indicator set is generated only after enough candles exist for EMA/ATR/TSI warmup.

## Trading window

Entries are allowed from 09:20 IST up to, but not including, 15:20 IST.

At or after 15:20 IST:

- the strategy stops looking for new entries
- open option trades are squared off

The scheduler evaluates the option strategy every 5 minutes during the option strategy window and runs until 15:30 IST.

## CALL signal

A CALL setup progresses through these states:

1. Idle:
   - candle high is above the Keltner upper band
   - candle close is above the Keltner upper band
2. Await retrace:
   - candle low touches or crosses the Keltner middle band
3. Await confirmation:
   - bullish candle
   - candle high is above middle
   - candle close is above middle
4. Await confirmation high break:
   - a later candle closes above the latest valid confirmation candle high
   - entry candle TSI is greater than `0.5`
   - reward to the Keltner upper band is at least equal to risk to the confirmation candle low
   - if price closes back below the Keltner middle before the high break, the confirmation is invalidated and a new bullish confirmation candle is required

When all conditions pass, the strategy records a CALL signal on the high-break candle and enters by buying CE. The confirmation candle low becomes the stop-loss reference and the Keltner upper band is the target reference.

## PUT signal

A PUT setup mirrors the CALL logic:

1. Idle:
   - candle low is below the Keltner lower band
   - candle close is below the Keltner lower band
2. Await retrace:
   - candle high touches or crosses the Keltner middle band
3. Await confirmation:
   - bearish candle
   - candle low is below middle
   - candle close is below middle
4. Await confirmation low break:
   - a later candle closes below the latest valid confirmation candle low
   - entry candle TSI is less than `-0.5`
   - reward to the Keltner lower band is at least equal to risk to the confirmation candle high
   - if price closes back above the Keltner middle before the low break, the confirmation is invalidated and a new bearish confirmation candle is required

When all conditions pass, the strategy records a PUT signal on the low-break candle and enters by buying PE. The confirmation candle high becomes the stop-loss reference and the Keltner lower band is the target reference.

## Live/demo option contract selection

When a CALL/PUT signal appears in runtime:

1. Backend gets current SENSEX LTP.
2. Backend loads the Angel One contract master.
3. It filters BFO `OPTIDX` contracts where:
   - `name == SENSEX`
   - symbol ends in `CE` for CALL or `PE` for PUT
   - expiry is today or later
4. It keeps nearest expiry contracts.
5. It requests LTP quotes for candidate option tokens.
6. It keeps contracts with premium from Rs. 220 to Rs. 290.
7. It selects the contract whose premium is closest to Rs. 260.
8. Ties are resolved by distance from underlying LTP, then strike.

Selected contract metadata is stored in `strategy_market_snapshots` with strategy key `option_entry_v1` and instrument label:

- `SENSEX_CE`
- `SENSEX_PE`

The Strategies page displays the nearest SENSEX option expiry series from the local Angel contract master so users can see which expiry the strategy will choose from at signal time. This preview does not consume Angel quote API capacity.

## Runtime entry placement

The runtime uses one configured instrument: `SENSEX`.

For each enabled runner:

- If the user already has exposure for that option side, skip.
- Place a MARKET entry:
  - CALL: role `BUY_ENTRY`, side `BUY`, instrument `SENSEX_CE`
  - PUT: role `SELL_ENTRY`, side `BUY`, instrument `SENSEX_PE`

Even though the PUT role is named `SELL_ENTRY`, the actual option order side is `BUY`; it represents buying PE.

## Runtime exit rules

For open option trades, the backend recalculates index indicators from recent SENSEX candles.

CALL exits:

- SL1 when candle low touches stop and close is below stop.
- TARGET when candle high reaches the current Keltner upper band.

PUT exits:

- SL1 when candle high touches stop and close is above stop.
- TARGET when candle low reaches the current Keltner lower band.

Exit orders are MARKET SELL orders for the selected option contract.

At 15:20 IST, remaining open option trades are squared off by MARKET SELL if there is no active protective exit already in flight.

## Backtesting removal rationale

Historical Option Entry replay is expensive and easy to misread because each backtest range can include different SENSEX expiries, strikes, and CE/PE contracts. Fetching every historical option candidate quickly consumes Angel One API limits, while an index-only proxy does not prove real option premium execution.

For this reason, Option Entry backtesting is removed. Futures Breakout v3 remains available in Backtesting. Option Entry should be validated from live/demo runtime events, selected contract snapshots, and trade/order history.

If exact option premium replay is needed later, implement it as an offline warming job, not inside a user-facing backtest request. It should:

1. Snapshot eligible contracts per date/expiry.
2. Fetch option candles in rate-limited background batches.
3. Store progress in DB.
4. Resume across API cooldowns.
5. Use cached data only for final backtest replay.

## Main database tables

- `user_strategy_activations`: active/inactive state for `option_entry_v1`.
- `user_strategy_configs`: SENSEX enabled flag, lots, session flags.
- `strategy_market_snapshots`: selected option contract metadata and signal levels.
- `strategy_orders`: option entries and exits.
- `trades`: open/closed option positions.
- `strategy_events`: option signals, square-offs, operational alerts.

## Operational notes

- If runtime cannot select an option contract, check SENSEX LTP, contract master freshness, candidate premiums, and Angel One quote rate limits.
- If the strategy card shows the Angel One market-data session as disconnected, reconnect Angel One before expecting entries.
- Runtime CALL signals create long CE trades. Runtime PUT signals create long PE trades.
- A PUT signal is not shorting options; it is buying PE.
