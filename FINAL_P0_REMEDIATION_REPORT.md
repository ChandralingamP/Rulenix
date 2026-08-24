# Final P0 Remediation Report

## 1. Executive Summary

The remaining executable P0/CRITICAL defects identified by `FINAL_STRATEGY_VERIFICATION.md` were remediated and exercised locally with disposable PostgreSQL plus a deterministic fake Angel One broker.

Result:

- All five original CRITICAL issues are classified **VERIFIED FIXED** after code review and stateful simulation.
- The real PostgreSQL concurrent Futures fill test passes repeatedly.
- Universal broker-position polling, unknown-token persistence, TP/SL over-close containment, protection rejection/ambiguity/restart, partial fills, contract-roll ambiguity, and 15:20 square-off recovery all have passing stateful tests.
- Clean and representative-legacy migrations pass on disposable PostgreSQL.
- Rust, frontend, build, and lint gates pass.
- No production database, broker credential, live order, deployment, or production service was touched.

This is not approval for live trading. Broker-specific behavior still needs Angel One sandbox verification.

```text
CODE + SIMULATION VERIFIED
BROKER SANDBOX VERIFICATION STILL REQUIRED
```

## 2. Strategy Freeze Verification

The strategy formulas and signals remain frozen.

Futures executable code at `backend/src/strategy.rs:309-399` implements:

```text
BUY
Entry  = HH4 * 1.0012
Target = Entry * 1.015
SL1    = MAX(Entry * 0.985, LL2 * 0.9988)
SL2    = MAX(Entry * 0.985, LL4 * 0.9988)

SELL
Entry  = LL4 * 0.9988
Target = Entry * 0.985
SL1    = MIN(Entry * 1.015, HH2 * 1.0012)
SL2    = MIN(Entry * 1.015, HH4 * 1.0012)
```

`futures_missed_entry_plan` independently computes:

```text
buy_missed  = session_open >= BUY_ENTRY
sell_missed = session_open <= SELL_ENTRY
```

Previous-day close is not an input to direction selection. Opening-range recovery remains side-specific. Passing tests include `formulas_match_v3`, `futures_stops_always_apply_the_authoritative_max_min_cap`, `missed_entry_logic_evaluates_buy_and_sell_independently`, `missed_entry_boundaries_are_inclusive_and_ignore_previous_close`, and `backtest_formulas_match_live_strategy`.

SuperTrend remains completed 5-minute candles, Wilder RMA ATR 7, factor 2.0, DOWN→UP ATM CALL, UP→DOWN ATM PUT, 90-second freshness, nearest eligible expiry, and long options only. No strategy optimization was performed.

## 3. Concurrent Futures Fill Fix

Futures exposure mutation now acquires a PostgreSQL transaction advisory lock keyed by user, strategy, and instrument before re-reading the existing trade. The current order is locked, the cumulative fill watermark is revalidated, exposure is mutated, and the processed watermark is committed in the same transaction (`backend/src/strategy.rs:9118+`).

The test-only overlap barrier forces two handlers through the former race location. `concurrent_opposite_futures_fill_handlers_serialize_in_postgres` uses independent tasks/connections for concurrent BUY and SELL fills and repeats the race 20 times. It verifies both fill watermarks, deterministic net exposure, no duplicate uncontrolled trade, and no duplicate protection intent.

Final classification: **VERIFIED FIXED**.

## 4. Universal Position Reconciliation

`reconciliation_audience` (`backend/src/strategy.rs:6178+`) includes every relevant user with any of:

- active permitted live mode and connected encrypted broker credentials;
- a live nonterminal order;
- an open live trade; or
- an unresolved broker-position incident.

It does not require an active order. Connected accounts receive both order-book and position-book reconciliation. Disconnected accounts with liabilities generate alerts.

Known broker-only exposure is quarantined without automatically claiming manual positions. Unknown exchange/token exposure is persisted as `UNMAPPED_BROKER_POSITION`, including product, quantity, average price, reconciliation time, symbol/token, ownership classification, and raw broker JSON. Matching new live entries are blocked in `backend/src/risk.rs:254+`.

Stateful coverage:

- `position_reconciliation_includes_zero_order_accounts_and_persists_unmapped_exposure`
- `broker_position_matrix_reconciles_orphans_mismatches_and_aggregates`

The matrix covers broker-only/no-order, local-only/broker-flat, greater/lower quantity, direction mismatch, average mismatch, unknown token, and multiple local rows mapping to one broker position.

Final classification: **VERIFIED FIXED**.

## 5. TP/SL Race Handling

The integration cannot prove native reduce-only or linked OCO for the exact CARRYFORWARD/INTRADAY products used. The application therefore assumes both exits may fill.

Containment now:

1. persists every cumulative exit-fill delta;
2. requests sibling cancellation but does not assume cancellation is terminal;
3. reconciles actual broker net position;
4. detects cumulative exits exceeding cumulative entry fills;
5. reconstructs the exact broker-confirmed opposite residual as `exposure_origin='broker_over_close'`;
6. creates one serialized deterministic emergency-close attempt;
7. uses the exact broker residual, including a non-lot-multiple residual;
8. retains failed attempts and retries with a new deterministic attempt key;
9. closes locally only after the emergency order is terminal and the broker is flat.

`tp_sl_double_fills_create_one_exact_emergency_close_and_converge_flat` covers TP50→SL50, SL50→TP50, TP25+SL50, SL25+TP50, two concurrent reconciliation workers, exact residual close quantity, no duplicate emergency intent, critical incident persistence, and convergence to broker flat.

Final classification: **VERIFIED FIXED**. This classification is supported by code review and stateful simulation. Exchange-side prevention remains unavailable; containment depends on position polling and must be sandbox-tested.

## 6. Protection/Ambiguity Handling

The live lifecycle is:

```text
real fill committed
-> PROTECTION_REQUIRED
-> durable stop intent
-> PROTECTION_SUBMITTING
-> broker order-book acknowledgement
-> PROTECTED
-> target submission
```

Only a submitted/partially-filled stop with broker ID and reconciliation timestamp counts as confirmed protection. Local submission success alone is insufficient.

Rejected/missing stops use deterministic new attempts. Deadline or attempt exhaustion enters `EMERGENCY_CLOSING`. Ambiguous stops enter `PROTECTION_UNCERTAIN`, create an operator-required incident, block target and compounding entry, and repeatedly reconcile order and position books. They are never blindly duplicated or flattened while an unseen stop may still exist.

Passing stateful tests:

- `rejected_stop_retries_deterministically_then_protects`
- `ambiguous_stop_response_is_reconciled_without_blind_retry_before_target`
- `stop_timeout_before_acceptance_escalates_without_conflicting_exit`
- `protection_restart_boundaries_converge_without_duplicate_stop`
- `fill_commit_crash_recovers_protection_on_restart`

Final classification for `COMMON-CRITICAL-001` and `COMMON-CRITICAL-002`: **VERIFIED FIXED**. The verification scope is code review plus stateful simulation; the broker sandbox gate remains open.

## 7. Stateful Test Infrastructure

The harness uses `TEST_DATABASE_URL` and refuses to run unless:

- the host is loopback; and
- the database name begins with `rulenix_test_`.

The local run used `rulenix_test_p0` on loopback PostgreSQL. Tests apply migrations, truncate only the explicitly disposable database, and restore test-only risk seed rows.

The deterministic Axum broker controls acceptance, rejection, timeout before/after acceptance, response loss, authentication failure, rate limit, broker unavailable, order book, position book, cancellation outcomes, quotes/circuit limits, RMS funds, and margin responses. Tests directly control cumulative fills, late fills, partial fills, broker positions, and event ordering.

No production credentials or database were used.

## 8. Crash/Restart Tests

Test-only failpoints exist only under `cfg(test)` around:

- fill committed before protection;
- protective intent persisted before broker submission;
- broker accepted before local acknowledgement;
- square-off intent before MARKET close.

Results:

| Boundary | Test | Result |
|---|---|---|
| Entry accepted, response lost | `entry_response_loss_reconciles_by_client_tag_without_resubmission` | PASS |
| Fill committed before stop | `fill_commit_crash_recovers_protection_on_restart` | PASS |
| Stop intent persisted before request | `protection_restart_boundaries_converge_without_duplicate_stop` | PASS |
| Stop accepted before local acknowledgement | same test | PASS |
| Restart with `PROTECTION_REQUIRED`/`PROTECTION_SUBMITTING` | same tests | PASS |
| Restart with emergency/square-off retry | `square_off_failures_retain_history_and_restart_with_new_attempt` | PASS |
| Partial fill durable boundary | `multiple_entry_partial_fills_receive_exact_nonduplicated_stop_slices` | PASS |

A crash-safe partial-fill correction was also made: inner fill transactions now persist `partially_filled` whenever cumulative fill is below requested quantity, instead of temporarily writing terminal `filled` and relying on a later outer correction.

## 9. Quantity/Tick/Price-Band/Margin Validation

Quantity policies are explicit and separately tested:

- Normal entry: exact `configured_lots * current_contract_lot_size`.
- Normal exit: positive and no greater than intended remaining quantity.
- Broker residual emergency exit: exact latest broker-confirmed absolute residual, even when not divisible by the original lot size.

Tick size comes from the current Angel instrument master and uses side-aware normalization.

Angel One FULL quotes provide `lowerCircuit` and `upperCircuit`. New priced live entries fail closed if authoritative bands are missing or the normalized price/trigger is outside them. An urgent protection request may continue to broker-side validation if the circuit lookup itself is unavailable; a known out-of-band stop is rejected locally into protection recovery.

The RMS parser now requires typed `availablecash`. It retains `net` and `availablelimitmargin` only for diagnostics and never uses them as permissive fallbacks. Broker batch required margin plus the configured safety buffer must fit inside available cash.

Evidence consulted:

- Angel One SmartAPI documentation: `https://smartapi.angelone.in/docs`
- Angel One official Java SDK examples: `https://github.com/angel-one/smartapi-java`
- Angel One official Python SDK: `https://github.com/angel-one/smartapi-python`

Classification:

- Tick/quantity/circuit executable behavior is locally verified; overall classification: **PARTIALLY FIXED** because Angel FULL/MPP semantics still require sandbox verification.
- Exact Angel RMS and STOPLOSS_MARKET behavior classification: **PARTIALLY FIXED** because broker sandbox verification remains pending.

## 10. Session and State Transition Hardening

The scheduler no longer uses `unwrap_or((false, String::new()))`. A calendar database failure remains distinguishable from a normal holiday, emits a critical structured alert, and fail-closes new entries. Protection, reconciliation, emergency close, and square-off loops continue.

Database triggers now enforce the order transition graph for pending, submitting, submitted, ambiguous, partially filled, processing, filled, failed, rejected, cancelling, and cancelled. Terminal order states cannot regress under a stale worker. Trade `CLOSED` and `EMERGENCY_CLOSING` terminal/monotonic invariants are also guarded.

The exit-coverage trigger serializes on the trade row. It permits legitimate partial-fill stop slices up to actual exposure, rejects over-coverage, and forbids emergency-close overlap with another active exit.

Tests:

- `calendar_lookup_failure_is_not_reclassified_as_normal_market_close`
- `state_transition_guard_rejects_stale_worker_regressions`
- `multiple_entry_partial_fills_receive_exact_nonduplicated_stop_slices`

Final classification for `COMMON-MEDIUM-003` and `COMMON-MEDIUM-004`: **VERIFIED FIXED**.

## 11. Documentation Corrections

Updated:

- `docs/strategy-futures-breakout-v3.md`: exact formulas, independent missed-side rules, previous close non-directional, opening-range recovery, exact quantity and circuit checks, partial-fill stop slices.
- `docs/strategy-supertrend-index-options-v1.md`: durable fill, SL first, STOPLOSS_MARKET, broker acknowledgement, target second, ambiguity, and 15:20 retry behavior.
- `docs/strategy-execution-architecture.md`: 90-second SuperTrend expiry, broker capability boundary, circuit validation, and RMS policy.

## 12. Database Migrations

Relevant migrations:

- `20260724025000_strategy_snapshot_execution_metadata.sql`
- `20260819000000_durable_signal_fanout.sql`
- `20260823000000_remove_margin_and_option_entry.sql`
- `20260823010000_execution_safety_lifecycle.sql`
- `20260823020000_p0_execution_safety.sql`

The lifecycle migration adds protection/reconciliation state, incidents, coverage preflight, and serialized exit-coverage enforcement. The P0 migration adds executable order metadata, raw/ownership incident metadata, over-close exposure origin, order transition enforcement, and terminal trade-safety enforcement.

`migrations_pass_clean_and_require_explicit_legacy_exit_reconciliation` verifies:

- clean migration application;
- representative active trade, pending order, failed protection, ambiguous order, and duplicate active exits;
- unsafe legacy coverage stops migration explicitly;
- broker-informed terminal reconciliation retains the historical row;
- migration then succeeds without silently deleting history.

Result: **PASS** on disposable PostgreSQL.

## 13. Files Changed

P0-specific implementation/reporting changes are concentrated in:

- `backend/src/strategy.rs`
- `backend/src/risk.rs`
- `backend/src/config.rs`
- `backend/src/credentials.rs`
- `backend/src/contract_master.rs`
- `backend/src/account.rs`, `auth.rs`, and `home.rs` for correct UUID advisory-lock casts
- the migrations listed in Section 12
- the three strategy/execution documentation files listed in Section 11
- `FINAL_P0_REMEDIATION_REPORT.md`

The complete working diff also contains earlier strategy-remediation changes in backend, frontend, environment examples, README, observability/project docs, and the three prior audit reports. Those were preserved and included in final verification rather than reverted.

## 14. Tests Added

Stateful PostgreSQL/fake-broker tests now include:

1. `concurrent_opposite_futures_fill_handlers_serialize_in_postgres`
2. `migrations_pass_clean_and_require_explicit_legacy_exit_reconciliation`
3. `position_reconciliation_includes_zero_order_accounts_and_persists_unmapped_exposure`
4. `broker_position_matrix_reconciles_orphans_mismatches_and_aggregates`
5. `tp_sl_double_fills_create_one_exact_emergency_close_and_converge_flat`
6. `ambiguous_stop_response_is_reconciled_without_blind_retry_before_target`
7. `rejected_stop_retries_deterministically_then_protects`
8. `protection_restart_boundaries_converge_without_duplicate_stop`
9. `contract_roll_retry_requires_terminal_nonfill_and_flat_position`
10. `stop_timeout_before_acceptance_escalates_without_conflicting_exit`
11. `square_off_failures_retain_history_and_restart_with_new_attempt`
12. `multiple_entry_partial_fills_receive_exact_nonduplicated_stop_slices`
13. `state_transition_guard_rejects_stale_worker_regressions`
14. `entry_response_loss_reconciles_by_client_tag_without_resubmission`
15. `fill_commit_crash_recovers_protection_on_restart`

Additional unit tests cover formula/gap freeze, circuit parsing, typed RMS behavior, quantity policy separation, calendar-error classification, fill watermarks, cancellation races, broker parsing, tick sequencing, and transition policy.

## 15. Complete Test Results

Final run:

| Command | Result |
|---|---|
| `cargo fmt -- --check` | PASS |
| `cargo check --tests` | PASS |
| `cargo test` | PASS — 120 passed, 0 failed, 15 intentionally ignored stateful tests |
| `cargo clippy --tests -- -D warnings` | PASS |
| Stateful tests with isolated `TEST_DATABASE_URL`, serial | PASS — 15 passed, 0 failed |
| `npm test -- --run` | PASS — 6 files, 18 tests |
| `npm run build` | PASS |
| `npm run lint` | PASS |

Required named results:

| Required test | Result |
|---|---|
| PostgreSQL migration test | PASS |
| Concurrent Futures fill test | PASS |
| Broker orphan-position test | PASS |
| TP/SL double-fill test | PASS |
| SL rejection test | PASS |
| SL response-loss test | PASS |
| Restart protection test | PASS |
| 15:20 square-off failure/restart test | PASS |
| Contract-roll ambiguity test | PASS |
| State-transition stale-worker test | PASS |

## 16. Broker Capabilities Requiring Sandbox Verification

The repository and official SDK material do not prove native reduce-only, linked OCO, or atomic sibling cancellation for the exact NORMAL/STOPLOSS CARRYFORWARD and INTRADAY orders used. ROBO/BO exists but is not assumed compatible.

Angel One sandbox verification is still required for:

- STOPLOSS_MARKET acceptance and gap behavior for every configured MCX/NFO/BFO product;
- client order-tag retention, uniqueness, truncation, pagination, and delayed order-book visibility;
- timeout-after-acceptance and cancellation response-loss behavior;
- partial/late fills after cancellation;
- exact residual quantities, including broker-reported nonstandard residuals;
- FULL quote circuit fields and market-protection/MPP behavior;
- RMS `availablecash`, batch margin shape, and live rejection semantics;
- rate limits, authentication expiry, and position-book lag;
- alert delivery in the actual operational channel.

## 17. Remaining Risks

1. Exchange-side TP/SL double fill is contained after broker reconciliation, not prevented by a proven native reduce-only/OCO primitive.
2. During authoritative broker uncertainty, the safe policy may leave exposure in `PROTECTION_UNCERTAIN` and operator-required state rather than risk a duplicate reversing exit.
3. Fake-broker and PostgreSQL tests do not reproduce every Angel One or exchange timing behavior.
4. Multi-replica process-kill/failover testing beyond independent PostgreSQL connections remains advisable.
5. Production migration state, deployed revision, database contents, secrets, logs, and broker positions were not inspected or changed.
6. The monolithic `strategy.rs` remains a LOW maintainability risk and was intentionally not decomposed during this P0 pass.
7. Full correlated-risk/risk-per-stop portfolio optimization remains outside this execution-safety remediation.

## 18. Safe for Paper Trading?

**YES, with controlled rollout and monitoring.**

Paper/demo trading is appropriate after applying the reviewed migrations to an isolated staging database and monitoring operational alerts, reconciliation incidents, protection latency, and square-off intents. This statement does not authorize deployment or production modification.

## 19. Safe for Live Trading?

**NO — not yet approved for live trading.**

The executable P0 defects are code-and-simulation verified, but the broker-specific release gate is incomplete.

```text
CODE + SIMULATION VERIFIED
BROKER SANDBOX VERIFICATION STILL REQUIRED
```

### Original CRITICAL/P0 issue disposition

| Issue | Previous status | Fix | Code location | Test | Test type | Result | Remaining risk | Final classification |
|---|---|---|---|---|---|---|---|---|
| `COMMON-CRITICAL-001` — open trade before protection / TP before SL | PARTIALLY FIXED | Durable protection states; SL intent and broker confirmation before target; emergency policy | `strategy.rs` entry completion and protection recovery (`~8890+`, `~7510+`) | rejection, ambiguity, restart, fill-crash tests | PostgreSQL + fake broker | PASS | Broker stop acceptance/gap semantics; broker sandbox gate remains | **VERIFIED FIXED** |
| `COMMON-CRITICAL-002` — incomplete protection recovery | PARTIALLY FIXED | Missing/rejected retry; uncertainty quarantine; deterministic emergency attempts | `strategy.rs:7525+`, `7695+`, `7716+` | `rejected_stop...`, `stop_timeout...`, `protection_restart...` | PostgreSQL + fake broker | PASS | Authoritative-unknown broker state may require operator; broker sandbox gate remains | **VERIFIED FIXED** |
| `STRAT1-CRITICAL-001` — late/second real fill ignored | PARTIALLY FIXED | Serialized exposure accounting; every delta applied; partial stop slices | `strategy.rs:9118+` | concurrent fill, partial-fill, fill-crash tests | Concurrent PostgreSQL + fake broker | PASS | Real exchange timing | **VERIFIED FIXED** |
| `COMMON-CRITICAL-003` — no universal position reconciliation | PARTIALLY FIXED | Universal connected/live/liability audience; raw unmapped incident persistence; entry block | `strategy.rs:6178+`, `risk.rs:254+` | zero-order orphan + A–H matrix | PostgreSQL + fake broker | PASS | Broker position lag/pagination | **VERIFIED FIXED** |
| `COMMON-CRITICAL-004` — TP/SL over-close reversal | PARTIALLY FIXED | Broker-net reconstruction and one exact serialized residual close | `strategy.rs:6773-7032` | four-ordering/partial race matrix + two workers | PostgreSQL + fake broker | PASS | No proven exchange-side OCO/reduce-only; broker sandbox gate remains | **VERIFIED FIXED** |

### Original HIGH issue disposition

| Issue | Result | Final classification | Remaining risk |
|---|---|---|---|
| `COMMON-HIGH-001` tick/lot/price band | Tick/quantity/circuit tests PASS | **PARTIALLY FIXED** | Local enforcement is verified; Angel FULL/MPP semantics require sandbox |
| `COMMON-HIGH-002` margin/funds | Typed RMS and margin tests PASS | **PARTIALLY FIXED** | Live RMS/batch semantics require sandbox |
| `STRAT2-HIGH-001` 15:20 protection cancellation order | Stateful failure/restart PASS | **VERIFIED FIXED** | Broker cancellation/market timing |
| `COMMON-HIGH-003` bounded ambiguity escalation | Timeout/operator-required test PASS | **VERIFIED FIXED** | Broker authority may remain unknown |
| `STRAT1-HIGH-001` simultaneous BUY/SELL fills | Repeated concurrent PostgreSQL test PASS | **VERIFIED FIXED** | Multi-replica chaos test advisable |
| `COMMON-HIGH-004` stop-limit gap risk | STOPLOSS_MARKET payload/unit and stateful protection tests PASS | **PARTIALLY FIXED** | Exact broker/product acceptance requires sandbox |

Final recommendation: keep live trading disabled; run the named Angel One sandbox matrix, review the resulting broker payload/order/position evidence, repeat migrations and tests in staging, and require an explicit human release decision after every sandbox gate passes.
