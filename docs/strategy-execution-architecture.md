# Shared Strategy Execution Architecture

Rulenix evaluates market data once per strategy and instrument, persists the
confirmed signal, and then distributes user-specific orders through durable
execution intents.

```text
shared WebSocket / historical cache
                 |
                 v
      strategy + instrument calculation
                 |
                 v
          confirmed strategy_signals row
                 |
                 v
  fresh database eligibility read at signal time
                 |
                 v
 one strategy_execution_intents row per user/order
                 |
                 v
 bounded Tokio worker pool (8 concurrent tasks)
                 |
                 v
 per-user risk controls, credentials and Angel order
                 |
                 v
 strategy_orders + broker reconciliation
```

## Delivery guarantees

- Signal and audience insertion is one database transaction.
- Signal, user, action and role uniqueness prevents duplicate entry delivery.
- Workers claim due intents with `FOR UPDATE SKIP LOCKED`.
- Interrupted claims return to `retry_wait` after restart.
- Transient broker, market-data, authentication, database and contract errors
  retry within the signal's safe window.
- SuperTrend entry intents expire 90 seconds after the completed signal candle.
  Futures Breakout intents may recover only inside their configured session
  catch-up/cutoff rules.
- Eligibility is checked again immediately before submission. Deactivation,
  risk controls and existing exposure produce a visible skipped/failed reason.

## Broker limits

Each Angel client has independent sliding-window gates. Rulenix keeps headroom
by limiting cumulative place/cancel traffic to 8 per second, 450 per minute and
900 per hour. Order-book calls are consolidated and limited to approximately
one per second per client. Historical and quote fallback calls have separate,
conservative gates. WebSockets remain the primary live-price source.

## Broker execution capabilities and validation

Angel One's current SmartAPI documentation exposes NORMAL, STOPLOSS, AMO, and ROBO varieties. ROBO is documented as a bracket-order variety using the BO product. The strategies in this repository use CARRYFORWARD futures and long INTRADAY options with independent NORMAL/STOPLOSS orders; the integration contains no documented reduce-only field, native linked-exit/OCO request, or atomic sibling-cancel guarantee for those exact products. Application-level sibling cancellation plus broker position reconciliation therefore remains mandatory. ROBO/OCO suitability must not be assumed without Angel One sandbox confirmation.

For priced orders, Rulenix requests FULL market data and validates the actionable limit/trigger against `lowerCircuit` and `upperCircuit`. New live entries fail closed if authoritative circuit data is unavailable. Urgent protective orders may still be submitted for broker-side validation when the circuit lookup itself is unavailable; any broker rejection enters protection recovery/emergency policy.

The RMS parser requires the documented `availablecash` field and does not fall back to `net` or `availablelimitmargin`. Estimated order margin plus the configured safety buffer must fit inside that value. Exact live semantics and rejection behavior remain an Angel One sandbox release gate.

## 15:10 square-off

SuperTrend creates durable `SQUARE_OFF` intents for every open trade at 15:10
IST. Protective orders are cancelled first, then a MARKET exit
is submitted. The watchdog continues every minute after 15:10 and across
restarts until reconciliation confirms closure. Demo expiry checkpoints may
close the local trade directly; live trades are never marked closed without a
confirmed broker outcome.

## Administration

The Daily trades page shows expected recipients, waiting, submitted, completed,
skipped and failed counts per signal. A failed entry may be retried only while
its original safe execution window remains open. The existing strategy reload
job also releases feed leases, refreshes contracts and wakes safe waiting
intents.
