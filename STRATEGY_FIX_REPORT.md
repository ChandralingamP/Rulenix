# Strategy Fix Report

## 1. Summary

This remediation corrects `futures_breakout_v3` strategy calculations and hardens live execution for both `futures_breakout_v3` and `supertrend_index_options_v1`. The implementation preserves the existing durable signal/intent fan-out, deterministic client tags, idempotency keys, PostgreSQL advisory locks, atomic order claims, scheduler leadership, fill watermarks, and no-blind-retry policy for ambiguous submissions.

No production deployment, service restart, production database change, credential change, live order, cancellation, or broker-side test was performed.

### Work state at the rate-limit interruption

Phases A-F were implemented in the working tree before the interruption:

- Phase A: authoritative Futures formulas, independent missed-entry decisions, opening-range recovery, and matching backtest behavior.
- Phase B: durable protection lifecycle, stop-first submission, recovery, and idempotent emergency-close intent.
- Phase C: broker positions/RMS/margin wrappers, fill and net-position reconciliation, incident persistence, and late/opposing-fill handling.
- Phase D: residual/over-close recovery, sibling-entry race handling, kill/deactivation cancellation, Futures/SuperTrend close safety, and expiry recovery.
- Phase E: contract-master tick metadata, directional price normalization, lot/quantity validation, available-funds/margin validation, and guarded contract rolling.
- Phase F: duplicate/out-of-order tick rejection and deterministic candle identity/validation.

### Work completed after resuming

- Completed and verified aggregate broker/local comparison when multiple local trades share a broker token.
- Prevented a new SuperTrend entry while an expired but still-open local exposure is awaiting reconciliation/flattening.
- Completed formatting and compilation fixes in the interrupted SuperTrend reversal-close change.
- Hardened the migration's legacy role-constraint removal for databases with more than one matching check constraint.
- Cleared strict Clippy findings with named state/SQL row types.
- Ran the complete safe local backend and frontend test/build checks and performed the final issue-by-issue audit.

## 2. Futures Gap Logic — Before vs After

Before: the implementation could classify direction from session open versus previous close, causing one breakout side to be disabled and conflating a market gap with an already-crossed breakout level.

After: `futures_missed_entry_plan` evaluates both sides independently:

- BUY is missed exactly when `SESSION_OPEN >= BUY_ENTRY`.
- SELL is missed exactly when `SESSION_OPEN <= SELL_ENTRY`.
- A non-missed side retains its normal breakout entry.
- Only a missed side is replaced by its 09:00-09:15 opening-range recovery entry.
- Previous close remains available for audit/display, but has no role in BUY/SELL selection.

The equality boundaries are deterministic and treated as crossed. Live strategy and backtest use the same helpers.

## 3. Futures Formula — Before vs After

The authoritative formulas are now centralized in `futures_levels` / `futures_exit_levels_for_entry` and shared with backtesting:

```text
BUY_ENTRY  = HH4 * 1.0012
BUY_TARGET = BUY_ENTRY * 1.015
BUY_SL1    = max(BUY_ENTRY * 0.985, LL2 * 0.9988)
BUY_SL2    = max(BUY_ENTRY * 0.985, LL4 * 0.9988)

SELL_ENTRY  = LL4 * 0.9988
SELL_TARGET = SELL_ENTRY * 0.985
SELL_SL1    = min(SELL_ENTRY * 1.015, HH2 * 1.0012)
SELL_SL2    = min(SELL_ENTRY * 1.015, HH4 * 1.0012)
```

The MAX/MIN behavior is unconditional; it is not fallback logic. After an actual entry fill, percentage-based target/stop portions are anchored to that real fill while the authoritative HH/LL cap remains in force. Tests explicitly document this fill-price behavior.

## 4. Critical Execution Fixes

| File / function | Previous problem | New behavior and reason | Verification |
|---|---|---|---|
| `backend/src/strategy.rs` — `complete_supertrend_entry_order`, `complete_claimed_order` | A broker fill could become an ordinary open trade before stop acknowledgement. | Entry fill and trade creation are committed atomically as `PROTECTION_REQUIRED`, with a deadline. Every positive cumulative-fill delta is processed. | Fill-watermark, partial-fill, simultaneous-opposite-fill, and recovery-policy unit tests pass. |
| `backend/src/strategy.rs` — protection recovery loop, `begin_emergency_close` | Missing/rejected stop recovery was incomplete. | Deterministic stop attempts are recovered after restart; deadline/retry exhaustion transitions to `EMERGENCY_CLOSING` and creates one deterministic `EMERGENCY_CLOSE`. | Protection decision and session-key tests pass; restart path compiles and is scheduler-wired. |
| `backend/src/strategy.rs` — `place_strategy_order` | Live submissions did not uniformly re-check shutdown, account mode, contract metadata, quantity, price, or funds. | The final submission gate re-checks all live eligibility, current contract/lot/tick metadata, directional tick normalization, whole-lot quantity, and funds/margin. | Submission-guard, tick, lot, and funds-parser tests pass. |
| `backend/src/strategy.rs` — `enforce_entry_shutdowns`; `backend/src/risk.rs` — `cancel_pending_entries` | Kill/deactivation could leave entry states unattended. | Cancellable entry orders are cancelled; submitting/ambiguous/processing orders are retained for reconciliation. Protective exits are never cancelled merely because strategy entry is disabled. | State-machine and submission-guard tests pass. |
| `backend/src/strategy.rs` — `process_sl2_reversal_intent` | A reversal could be opened before authoritative closure of the source exposure. | Reversal requires a closed source row, broker net zero reconciled after exit, and no open position incident. | Reversal direction/session tests pass; broker integration remains a sandbox item. |

## 5. Broker Reconciliation Fixes

`backend/src/angel.rs` now exposes authenticated position-book, RMS-limits, and batch-margin calls. `reconcile_live_user` consumes the broker order book and position book on each live reconciliation cycle.

`reconcile_broker_positions` now:

- aggregates all open local rows by exchange/token before comparing signed quantity with broker net quantity;
- refreshes broker net/average/reconciliation timestamps on open and recently closed trades;
- persists one durable incident per user/exchange/token/incident type;
- marks all affected local rows `RECONCILIATION_REQUIRED` and blocks new exposure for that strategy/underlying;
- detects broker-flat/local-open, quantity/direction mismatch, and average-entry mismatch;
- detects broker-only positions and escalates them as operator-required instead of assuming ownership;
- reconstructs a known TP/SL over-close residual as a synthetic `EMERGENCY_CLOSING` trade so it can be flattened idempotently;
- closes a local `CLOSING`/`EMERGENCY_CLOSING` row only after the broker is flat and protective orders are terminal.

An actual late Futures fill is never discarded. Same-side additional fills re-average the local trade and re-protect the residual; opposing fills offset the existing local exposure and any excess is represented as an emergency-closing residual.

## 6. Protection Lifecycle

Migration `20260823010000_execution_safety_lifecycle.sql` adds these durable states:

```text
DEMO
PROTECTION_REQUIRED
PROTECTION_SUBMITTING
PROTECTED
PROTECTION_FAILED
CLOSING
EMERGENCY_CLOSING
RECONCILIATION_REQUIRED
CLOSED
```

For live fills the lifecycle is:

```text
fill persisted atomically
  -> PROTECTION_REQUIRED
  -> deterministic STOPLOSS_MARKET submission
  -> PROTECTION_SUBMITTING
  -> broker order-book acknowledgement
  -> PROTECTED
  -> target submission for uncovered target quantity
```

A locally submitted stop is never treated as broker-confirmed. Failed/rejected/missing attempts remain durable and retry within the configured attempt/deadline policy. Recovery is not dependent on the user's account mode still being set to live after a fill. `CLOSING` and `EMERGENCY_CLOSING` rows are also restart-recovered.

## 7. Partial Fill Handling

- Reconciliation uses monotonic cumulative fill watermarks and processes only a positive delta.
- Fill-after-cancel and each new partial delta are processed before terminal state handling.
- Entry remainder cancellation is idempotent.
- Trade `quantity` and `remaining_lots` are reduced after partial exits, so risk and broker reconciliation compare residual exposure.
- A live residual returns to `PROTECTION_REQUIRED` and is re-protected for its exact remaining quantity.
- Target quantity is added only after acknowledged stop coverage and only up to uncovered residual quantity.

## 8. TP/SL Coordination

- Live protection uses `STOPLOSS_MARKET`; remaining `STOPLOSS_LIMIT` orders are Futures stop-entry orders or demo-only simulation orders.
- Stop acknowledgement precedes target creation for both strategies.
- When one exit fills, siblings are cancelled and every subsequent positive fill delta is still processed.
- If cumulative protective/emergency exit fills exceed the source quantity and the resulting broker residual can be tied to that trade/token, the residual is reconstructed and emergency-closed.
- Carry-stop replacement validates quote/contract metadata before cancelling the existing stop and leaves a durable protection-failed state plus a critical alert on failure.

## 9. SuperTrend Square-Off Fix

For the 15:20 checkpoint and signal reversal:

- a usable quote, contract metadata, and live-forced runner are obtained before an acknowledged protective order is cancelled;
- the trade is set to `CLOSING` before cancellation/submission;
- an existing market close is treated as the active close instead of duplicated;
- the close uses a deterministic `EMERGENCY_CLOSE` order/session key;
- submission failure leaves `EMERGENCY_CLOSING`, allowing restart recovery;
- replacement entry waits until opposing local exposure is gone;
- an expired but still-open trade continues to block new same-instrument exposure.

Demo expiry is closed locally. A live expired row is not falsely marked closed; it becomes `EMERGENCY_CLOSING` and the recovery loop attempts the market close, with a critical/manual warning because a broker may reject a post-expiry order.

## 10. Tick/Lot/Margin Validation

- `MasterContract.tick_size` is parsed from Angel's instrument master and converted from paise to rupees.
- Integer micro-unit arithmetic normalizes non-market prices deterministically: BUY rounds up and SELL rounds down to a valid tick.
- Live entry metadata must come from the current daily instrument master and match token, exchange, symbol, expiry, lot, and tick.
- Protection/closing may use the last locally validated cached master when the daily download is unavailable, preventing a metadata outage from blocking risk reduction.
- Every order requires positive lots/quantity and `quantity % current_lot_size == 0`.
- Live entry uses Angel batch margin plus RMS available funds and a configurable safety buffer; unavailable or malformed data fails closed.
- STOPLOSS_MARKET sends zero limit price and a normalized trigger.

Price-band enforcement beyond broker rejection is not claimed because no authoritative band feed exists in this repository.

## 11. Database Migrations

Added by this remediation:

- `backend/migrations/20260823010000_execution_safety_lifecycle.sql`
  - trade safety/protection and broker-reconciliation columns;
  - ambiguous-order deadline timestamp;
  - safety-state and attempt constraints/index;
  - `EMERGENCY_CLOSE` strategy-order role;
  - database-enforced uniqueness for an active target, stop family, or emergency close per trade;
  - durable, deduplicated `broker_position_incidents` table/index.

Pre-existing untracked migrations `20260819000000_durable_signal_fanout.sql` and `20260823000000_remove_margin_and_option_entry.sql` were present when work resumed and are not claimed as remediation-created files.

The new migration was reviewed but not applied: neither `TEST_DATABASE_URL` nor `DATABASE_URL` is configured in this workspace, and applying it to an unknown database would violate the no-production constraint.
The active-exit unique index intentionally fails migration if legacy duplicate active exits exist; inspect and reconcile those rows against the broker before retrying rather than deleting them automatically.

## 12. Files Changed

Remediation-scoped files (the worktree also contains unrelated pre-existing user changes):

- `backend/src/strategy.rs` — formulas, gap logic, execution/protection lifecycle, close/reversal safety, reconciliation, validation, fill handling, tests.
- `backend/src/backtesting.rs` — shared authoritative formula/gap behavior and backtest regression tests.
- `backend/src/angel.rs` — position/RMS/margin endpoints and stop-market payload behavior/tests.
- `backend/src/contract_master.rs` — tick-size field and stale-cache exit support.
- `backend/src/risk.rs` — remaining-exposure accounting, reconciliation incident blocking, expanded entry cancellation.
- `backend/src/market_ws.rs` — broker tick timestamp/sequence propagation and corrupt timestamp rejection.
- `backend/src/state.rs` — per-token accepted timestamp/sequence state.
- `backend/src/config.rs` — validated protection, ambiguity, and margin-buffer settings.
- `backend/src/main.rs` — state initialization and safety/recovery loop wiring.
- `backend/.env.development.example`, `.env.test.example`, `.env.staging.example`, `.env.production.example` — safe setting examples.
- `backend/migrations/20260823010000_execution_safety_lifecycle.sql` — durable schema.
- `STRATEGY_FIX_REPORT.md` — this report.

## 13. Tests Added

The remediation includes deterministic unit/regression coverage for:

- exact BUY/SELL formulas and MAX/MIN caps;
- all missed-entry boundaries, independent side evaluation, and previous-close regression;
- live/backtest formula consistency and opening-range replacement behavior;
- fill-price anchoring and reversal levels;
- monotonic fill deltas, fill while cancelling, and partial-fill cancellation policy;
- simultaneous opposing Futures fills;
- durable order transitions, ambiguous no-blind-retry classification, and reconciliation status mapping;
- protection acknowledgement vs submission and deadline/retry decisions;
- broker position parsing and quantity/direction mismatch policy;
- tick conversion/directional normalization and whole-lot validation;
- available-funds parsing and STOPLOSS_MARKET payload shape;
- duplicate/out-of-order tick sequences and duplicate/invalid candle filtering;
- SuperTrend flip/candle freshness, long-option direction, expiry/ATM selection, and stable protection keys;
- risk limits using serialized reservations and remaining exposure.

Database/broker fault-injection scenarios from the prompt were not fabricated as passing unit tests. They require a disposable PostgreSQL instance plus a stateful broker simulator or Angel sandbox.

## 14. Tests Passed

Executed after final formatting:

- `cargo check --tests` — PASS.
- `cargo test` — PASS: 117 passed, 0 failed, 0 ignored.
- `cargo clippy --tests -- -D warnings` — PASS.
- `npm test -- --run` — PASS: 6 files, 18 tests.
- `npm run build` — PASS: Vite production build, 105 modules transformed.
- `npm run lint` — PASS.
- `git diff --check` — PASS for whitespace errors; Git emitted only line-ending conversion warnings on existing working-tree files.

Focused strategy/backtest/protection/reconciliation/tick tests were also run during phases A-F and passed before the final full suite.

Tests failed in the final verification: none.

Not executed:

- SQL migration application/integration test: no disposable test database URL was configured.
- Angel sandbox fault injection: no sandbox credentials/environment was provided, and production broker interaction was prohibited.

## 15. Remaining Known Risks

### UNABLE TO IMPLEMENT SAFELY — ambiguous protective-order flattening

Angel's API behavior available to this application does not provide a verified atomic OCO/reduce-only primitive. If a stop submission times out and remains absent from the visible order book, blindly sending another stop or market close can later create a reverse position if the first stop was actually accepted and subsequently fills. The safest implemented fallback is:

- never blind-retry the ambiguous order;
- persist the ambiguity and deadline;
- mark the trade `EMERGENCY_CLOSING` / reconciliation-required;
- block new exposure and raise a critical, deduplicated operator alert;
- resume only after broker order/position state becomes authoritative.

Remaining risk: during that broker-uncertainty window, the database can express an active emergency-close obligation, but cannot guarantee that a close order is safely executable. Therefore the absolute protected-or-flattening invariant is not claimed for this broker-limitation edge.

### UNABLE TO IMPLEMENT SAFELY — unidentified broker-only position ownership

A broker-only position with no matching local order/trade is detected, persisted, alerted, and blocks new related exposure. It is not automatically closed because it may be a manual position or belong to another strategy. Automatic mutation without provable ownership could liquidate legitimate exposure. Operator attribution/closure is required.

Other known risks:

- Broker-native price bands are not locally available; tick-valid orders may still be rejected by dynamic bands.
- A broker-flat reconciled close uses the last known price locally; final realized P&L may require broker trade-book reconciliation.
- Post-expiry market closes can be rejected and require operator action.
- Multi-replica crash/failover, network-response-loss, DB commit failure after a real fill, and true TP/SL exchange races need stateful integration/chaos tests even though their state-machine paths and idempotency guards are implemented.

## 16. Items Requiring Broker Sandbox Validation

Use only an Angel One sandbox or explicitly isolated paper account/database:

1. Verify position-book field names/sign conventions for NFO and MCX, including multiple fills and broker-average pricing.
2. Verify RMS available-funds selection and batch-margin response schema for Futures and index options.
3. Confirm instrument-master tick-size units (`tick_size / 100`) and live order rounding for each supported contract.
4. Confirm STOPLOSS_MARKET acceptance, trigger semantics, gap behavior, product type, and post-market/expiry rejection behavior.
5. Inject entry response loss, partial fill, late fill after cancel, SL rejection/timeout/response loss, TP rejection, and TP/SL double fill.
6. Kill/restart the process after each fill/protection/close transition and verify deterministic recovery with no duplicate broker order.
7. Exercise contract-roll rejection plus order-book/tag/position verification before replacement.
8. Apply the migration to a disposable clone, then validate constraints, rollback/forward deployment order, and mixed-version replica behavior.
9. Exercise global/user kill and strategy deactivation in every nonterminal entry state while confirming existing protection is retained.
10. Validate critical incident/ambiguity/square-off alert delivery and operational runbooks.

The implementation is ready for those controlled validations, not for an unreviewed production rollout.
