# Final Strategy Verification

## 1. Executive Summary

This was a read-only verification of the current working tree, except for creating this report. No source code, tests, configuration, migrations, services, database, credentials, or broker state were modified. No broker order was placed or cancelled.

The Futures mathematical correction is verified from executable code and passing deterministic tests. Previous-day close no longer selects BUY or SELL. The code also contains substantial execution-safety improvements: durable protection states, live SL-before-TP, recovery decisions, emergency-close intents, position-book reconciliation, late-fill accounting, over-close reconstruction, tick/lot/margin gates, shutdown handling, and market-tick ordering.

The remediation is **not verified safe for live trading**. All five original CRITICAL findings are only partially verified because stateful PostgreSQL/broker fault-injection tests were not provided or runnable, and two important source-level gaps remain:

1. Truly concurrent Futures BUY/SELL fill handlers are not serialized around the initial open-trade lookup and insert (`complete_claimed_order`, approximately lines 8528-8672). Both handlers can observe no existing trade and create separate open rows. The unit test named `simultaneous_opposite_fills_are_fully_accounted` tests only the arithmetic helper, not this database race.
2. Periodic broker position reconciliation selects users from nonterminal `strategy_orders` (`reconcile_live`, approximately lines 5931-5949). A connected live user with a broker-only position but no nonterminal strategy order is not necessarily polled, so universal orphan discovery is not established.

Additional blockers include no disposable-database migration test, no stateful Angel sandbox/failure-injection test, no verified broker OCO/reduce-only behavior, incomplete price-band validation, and stale changed documentation that contradicts executable behavior.

Classification of the 20 explicitly identified audit issues:

| Result | Count |
|---|---:|
| VERIFIED FIXED | 2 |
| PARTIALLY FIXED | 13 |
| NOT FIXED | 5 |
| UNABLE TO VERIFY | 0 |

`UNABLE TO VERIFY` is used separately for external production/broker/database claims. A source implementation without a required stateful integration test is classified `PARTIALLY FIXED`, not `VERIFIED FIXED`.

### Material reviewed

- `STRATEGY_IMPLEMENTATION_AUDIT.md` and `STRATEGY_FIX_REPORT.md`.
- Complete tracked Git diff and all untracked files relevant to the remediation.
- Executable changes in `backend/src/{strategy,backtesting,angel,contract_master,risk,market_ws,state,config,main}.rs`.
- Related account/home/margin-removal, notifications, operations, P&L, frontend, README, and documentation changes for collision or contradiction.
- All Rust and frontend tests, including modified test files.
- New migrations `20260819000000_durable_signal_fanout.sql`, `20260823000000_remove_margin_and_option_entry.sql`, and `20260823010000_execution_safety_lifecycle.sql`, plus earlier order/idempotency migrations.

The working tree was already dirty before this verification. This report evaluates that working tree, not a committed or deployed revision.

## 2. Futures Logic Verification

### Exact executable formulas — VERIFIED FIXED

Executable source: `backend/src/strategy.rs:346-412` (`futures_exit_levels_for_entry`, `calculate`). Backtest source imports the same helpers at `backend/src/backtesting.rs:12-14` and calls them around `715-718`.

BUY is exactly:

```text
Entry  = HH4 * (1 + 0.0012) = HH4 * 1.0012
Target = Entry * (1 + 0.015) = Entry * 1.015
SL1    = max(Entry * (1 - 0.015), LL2 * (1 - 0.0012))
       = MAX(Entry * 0.985, LL2 * 0.9988)
SL2    = max(Entry * (1 - 0.015), LL4 * (1 - 0.0012))
       = MAX(Entry * 0.985, LL4 * 0.9988)
```

SELL is exactly:

```text
Entry  = LL4 * (1 - 0.0012) = LL4 * 0.9988
Target = Entry * (1 - 0.015) = Entry * 0.985
SL1    = min(Entry * (1 + 0.015), HH2 * (1 + 0.0012))
       = MIN(Entry * 1.015, HH2 * 1.0012)
SL2    = min(Entry * (1 + 0.015), HH4 * (1 + 0.0012))
       = MIN(Entry * 1.015, HH4 * 1.0012)
```

Tests: `strategy::tests::formulas_match_v3`, `futures_stops_always_apply_the_authoritative_max_min_cap`, `backtesting::tests::backtest_formulas_match_live_strategy`, and opening-range simulator tests. All passed.

Fill-price behavior: `snapshot_order_exit_levels` recomputes percentage portions from actual entry fill while retaining HH/LL MAX/MIN caps. `initial_futures_target_is_anchored_to_actual_fill_price` and `reversal_exit_levels_are_anchored_to_the_new_entry` passed. This behavior is explicit and internally consistent, but differs from interpreting every exit as permanently anchored to the pre-fill trigger.

### Previous-close direction — VERIFIED FIXED

Executable source: `futures_missed_entry_plan` at `backend/src/strategy.rs:308-324` accepts only market open, BUY entry, and SELL entry. It has no previous-close parameter.

```text
buy_missed  = market_open >= buy_entry
sell_missed = market_open <= sell_entry
```

`ensure_futures_gap_plans` (`1554-1672`) persists `entry_direction='BOTH'`; previous close appears only in event/audit payloads. `resolve_futures_opening_range_plan` (`1700-1803`) replaces only a missed side through `COALESCE`; the non-missed side retains its normal entry. The opening interval filter is 09:00 inclusive to 09:15 exclusive.

Tests: `missed_entry_logic_evaluates_buy_and_sell_independently`, `missed_entry_boundaries_are_inclusive_and_ignore_previous_close`, and both backtest opening-range replacement tests. All passed.

Remaining risk: `docs/strategy-futures-breakout-v3.md` is stale and still says previous close determines gap direction and describes stops as fallback logic. That documentation contradicts verified executable code and must not be used as production strategy authority.

## 3. SuperTrend Logic Verification

Signal mathematics remain SuperTrend ATR(7), factor 2.0, using completed five-minute underlying-index candles. DOWN→UP selects a long ATM call; UP→DOWN selects a long ATM put. Stale signals are rejected and entries stop before 15:20 IST.

Tests passed for Wilder RMA, current just-closed flip, call/put direction, stale replay rejection, previous-session continuity, weekend lookback, completed-candle coverage, nearest-expiry ATM selection, and long-options-only behavior.

Protection sequencing in executable code is changed correctly:

- `complete_supertrend_entry_order` atomically creates the trade as `PROTECTION_REQUIRED` (`8344-8369`).
- It submits live `SL1` as `STOPLOSS_MARKET` first (`8417-8442`).
- It returns without creating a live target (`8443-8445`).
- `recover_unprotected_trades` marks `PROTECTED` only from broker-reconciled stop coverage, then calls `ensure_target_after_protection` (`7216-7234`).
- Demo mode still creates stop then target immediately.

Verification result: **PARTIALLY FIXED** for live execution, because there is no direct database/broker test proving SL acknowledgement precedes target under rejection, timeout, response loss, and restart.

Documentation risk: `docs/strategy-supertrend-index-options-v1.md` still says target then stop and `STOPLOSS_LIMIT`; this contradicts executable live behavior (`STOPLOSS_MARKET`, SL first).

## 4. Critical Issues Verification

| Original issue ID | Original problem | Original / fix location | Test covering it | Verification result | Remaining risk |
|---|---|---|---|---|---|
| `COMMON-CRITICAL-001` | Trade committed as ordinary open before broker protection; SuperTrend TP submitted before SL. | Original: old `complete_supertrend_entry_order` / `complete_claimed_order`. Fix: `strategy.rs:8319-8445`, `8484-8731`; migration safety fields/states. | Protection decision tests, STOPLOSS_MARKET payload test, formula/fill tests. No stateful SL-before-TP failure test. | **PARTIALLY FIXED** | Trade is durably unsafe rather than silently safe, and stop is submitted first. A real position still exists during acknowledgement latency. Ambiguous stop state can block safe flattening; DB/broker crash points are untested. |
| `COMMON-CRITICAL-002` | Failed/rejected/ambiguous/missing stops and restart-open unprotected trades were not comprehensively recovered. | Original: old `retry_failed_protective_orders`. Fix: `recover_unprotected_trades` `strategy.rs:7174-7309`, `begin_emergency_close` `7107-7172`, scheduler `5672-5679`. | `protection_recovery_never_treats_submission_as_confirmation`; `protection_timeout_or_retry_exhaustion_requires_emergency_close`. | **PARTIALLY FIXED** | Missing/rejected stops retry deterministically and deadline to emergency close. An ambiguous active stop is intentionally not blindly cancelled/replaced; no restart/fault-injection integration test proves convergence. |
| `STRAT1-CRITICAL-001` | A late/second real Futures fill was marked filled and ignored locally. | Original old fill handler. Fix: `complete_claimed_order` `strategy.rs:8528-8698`; fill reconciliation `6715-6942`. | `simultaneous_opposite_fills_are_fully_accounted`; fill watermark/cancelling tests. | **PARTIALLY FIXED** | Sequential same/opposite late fills are represented and re-protected/closed. The unit test covers arithmetic only. Concurrent handlers can both miss the existing row because no common lock surrounds `8528` lookup through `8662` insert. |
| `COMMON-CRITICAL-003` | Only order book was reconciled; broker/local net-position disagreement and orphans could persist. | Fix: Angel `positions` `angel.rs:738-755`; `reconcile_broker_positions` `strategy.rs:6305-6570`; `reconcile_live_user` ends with position reconciliation `6942`; incident migration. | Position parser and mismatch-policy unit tests only. | **PARTIALLY FIXED** | Quantity/direction/average mismatch and related-token orphan incidents exist. Users are selected for reconciliation from nonterminal orders, so broker-only exposure with no active order can evade polling. No database/broker integration test. Orphans are stored as `open`, not actually changed to `operator_required`, despite report wording. |
| `COMMON-CRITICAL-004` | TP and SL can both fill before sibling cancellation, over-closing into a reverse position. | Fix: exit delta processing `8786-8988`; over-close reconstruction `strategy.rs:6446-6525`; active-exit unique index in migration. | Fill delta/cancellation tests and mismatch helper. No concurrent TP/SL integration test. | **PARTIALLY FIXED** | Race is detected/reconstructed after broker position refresh when attributable. There is no verified broker OCO/reduce-only mechanism, so the race is not prevented. Synthetic residuals not divisible by lot size may fail the close validation. |

No CRITICAL/P0 issue is classified `VERIFIED FIXED` end to end.

## 5. High Issues Verification

### Original HIGH issues

| Original issue ID | Original problem | Original / fix location | Test covering it | Verification result | Remaining risk |
|---|---|---|---|---|---|
| `COMMON-HIGH-001` | No tick-size, price-band, or lot-step normalization. | `strategy.rs:670-759`, `3248-3307`; `contract_master.rs:22-34`; Angel order payload. | `angel_master_tick_size_and_directional_rounding_are_deterministic`; `contract_quantities_must_be_positive_whole_lots`. | **PARTIALLY FIXED** | Tick and quantity-multiple checks exist. No price-band feed/check exists. `valid_contract_quantity` does not generally enforce `quantity == lots * lot_size`; it only enforces positive/multiple, with exact equality added only for fresh Futures entries. Tick-size unit and two-decimal broker payload need sandbox confirmation. |
| `COMMON-HIGH-002` | No broker margin/available-funds/leverage validation. | `angel.rs:757-803`; `strategy.rs:3149-3204`, `3337-3358`; config/env buffer. | `available_funds_parser_is_numeric_and_conservative`. | **PARTIALLY FIXED** | Batch margin and RMS gates fail closed for live entries. Response shape is not sandbox-verified. Parser chooses the first valid field (usually `availablecash`), not the minimum; the test name “conservative” overstates this. No risk-per-stop sizing or correlated leverage control was added. |
| `STRAT2-HIGH-001` | 15:20 square-off cancelled protection before obtaining a quote; quote failure could leave exposure unprotected. | `process_supertrend_square_off` `strategy.rs:4705-4847`; reversal close `4117-4255`; durable intent `4418-4491`. | No direct square-off ordering/restart integration test. Exit-reason unit test only. | **PARTIALLY FIXED** | Source obtains quote and live runner before cancellation, so the original quote-failure sequence is corrected. Cancellation-to-market-submit remains a non-atomic window; recovery exists but is not fault-tested. |
| `COMMON-HIGH-003` | Ambiguous submission absent from order book had no bounded escalation. | `uncertain_since_at` migration; `reconcile_live_user` `6786-6800`; `escalate_ambiguous_order` `6205-6279`. | Ambiguous broker-classification and no-blind-retry tests; no deadline DB test. | **PARTIALLY FIXED** | Deadline, durable incident, blocking, and critical alert are implemented. Order ambiguity is not made terminal automatically. Protective ambiguity cannot be blindly flattened safely without authoritative broker state. |
| `STRAT1-HIGH-001` | Simultaneous BUY/SELL breakout fills were not mutually exclusive. | Sibling cancellation and opposite-fill accounting `strategy.rs:7662+`, `8473-8698`. | `simultaneous_opposite_fills_are_fully_accounted` tests only arithmetic. | **PARTIALLY FIXED** | Sequential second fill is handled. Truly concurrent initial fill processing is not serialized and has no concurrent DB test or open-trade uniqueness constraint. |
| `COMMON-HIGH-004` | Protective STOPLOSS_LIMIT may trigger without filling in a gap. | Live protection creation/recovery uses `STOPLOSS_MARKET` at `6071`, `7285`, `8428`, `8605`, `8717`; Angel payload `613-626`. | `stoploss_market_payload_has_zero_limit_and_keeps_trigger`. | **PARTIALLY FIXED** | Executable live protective stops are stop-market; remaining stop-limit uses are Futures entries or demo exits. Angel acceptance/gap semantics and dynamic price-band behavior are unverified. |

### Original MEDIUM and LOW issues

| Original issue ID | Original problem | Fix location / evidence | Test covering it | Verification result | Remaining risk |
|---|---|---|---|---|---|
| `COMMON-MEDIUM-001` | Trade state could not express protection/closing/recovery. | Safety-state migration; state writes throughout entry/protection/close/reconciliation paths. | Protection policy and exit-reason tests. | **PARTIALLY FIXED** | Model exists in code/schema but migration was not applied to a disposable database and direct SQL can still make invalid logical transitions. |
| `STRAT1-MEDIUM-001` | Risk exposure used total lots after partial target instead of residual exposure. | `risk.rs:305-347` uses `remaining_lots`; exit handlers reduce `quantity` and `remaining_lots`. | Full risk boundary suite plus partial-target simulator tests. | **VERIFIED FIXED** | SQL is statically clear and tests pass. No production data migration verification was performed. |
| `COMMON-MEDIUM-002` | No sequence deduplication or strict out-of-order tick rejection. | `market_ws.rs:191-227`, `496-515`; `state.rs:21-48`; `strategy.rs:9091-9119`; REST BTreeMap identity `982-1020`. | `duplicate_and_out_of_order_tick_sequences_are_rejected`; invalid/duplicate candle test; exchange timestamp test. | **VERIFIED FIXED** | Sequence watermark is in memory and resets after process restart. Corrupt timestamps outside ±24 hours are rejected, but restart replay behavior is not integration-tested. |
| `COMMON-MEDIUM-003` | Session/holiday lookup failure could become silent closed/skipped behavior. | `strategy.rs:5563-5570` still uses `unwrap_or((false, String::new()))`. | None specific. | **NOT FIXED** | Some callers emit session alerts, but the cited scheduler path still suppresses the error and reason. |
| `COMMON-MEDIUM-004` | Direct SQL transitions bypass central transition validation. | `valid_order_transition` exists, but many direct `UPDATE strategy_orders/trades` statements remain. | State-machine helper test only. | **NOT FIXED** | The tested helper does not govern every transition. Schema checks cover value domains, not the full transition graph. |
| `COMMON-LOW-001` | Monolithic strategy module creates review/regression risk. | `backend/src/strategy.rs` is now approximately 10,664 lines. | N/A. | **NOT FIXED** | Module size increased and mixes signals, execution, reconciliation, recovery, APIs, and tests. |
| `COMMON-LOW-002` | Duplicated queries and separate live/backtest formulas can drift. | Backtesting now imports live formula/gap helpers; duplicated runner/query/execution logic remains. | `backtest_formulas_match_live_strategy`. | **PARTIALLY FIXED** | Formula drift is addressed; broader duplication remains. |
| `COMMON-LOW-003` | Some logs lack complete signal→intent→order→trade correlation. | Durable signal/intent IDs and many structured events were added; several alerts still pass empty instrument/context or prose-only diagnostic. | No correlation completeness test. | **NOT FIXED** | End-to-end mandatory correlation fields are not schema-/test-enforced for every alert/log path. |
| `COMMON-LOW-004` | Placeholder Nginx domain conflicts with concrete Caddy domain. | `infra/nginx/rulenix.conf` still uses `app.rulenix.example.com`; Caddy uses `rulenix.in`. | N/A. | **NOT FIXED** | Deployment documentation/configuration remains ambiguous. No production inspection was performed. |

Additional unnumbered audit findings remain: fixed-lot sizing is not risk-per-stop sizing; no explicit correlated-underlying leverage limit, consecutive-loss cooldown, minimum/maximum SL distance, or risk/reward gate was added.

## 6. Crash/Restart Verification

| Original issue ID | Original problem | Code/fix location | Test covering it | Verification result | Remaining risk |
|---|---|---|---|---|---|
| `Audit §22-A` | Crash before broker submission. | `reconcile_live` changes stale `pending` to retryable `failed` (`5919-5926`); durable intents recover claimed work (`5449-5452`). | Durable intent/order transition tests. | **PARTIALLY FIXED** | No process-kill integration test proves no request was sent before the persisted boundary. |
| `Audit §22-B` | Crash during/after submission with unknown outcome. | Stale live `submitting` becomes `ambiguous` with `uncertain_since_at`; reconciliation uses broker ID/client tag and never blind-retries (`5927`, `6758+`). | Angel timeout/disconnect and ambiguous-classification tests. | **PARTIALLY FIXED** | Broker order-book retention/tag behavior and response-loss timing are unverified. |
| `Audit §22-C` | Entry committed, crash before SL. | Atomic `PROTECTION_REQUIRED` trade state/deadline; five-second recovery loop `5675`; `recover_unprotected_trades`. | Pure protection policy tests. | **PARTIALLY FIXED** | No real DB crash after fill commit/before stop-row creation was injected. |
| `Audit §22-D` | Crash during fill processing. | `processing` recovery (`5929`) and `complete_order` rollback/requeue logic (`8251-8316`). | Fill watermark/cancelling/state-machine tests. | **PARTIALLY FIXED** | No transactional failpoint test validates each commit boundary. |
| `Audit §22-E` | Crash during square-off/emergency close. | Durable `SQUARE_OFF` intent, `CLOSING`/`EMERGENCY_CLOSING`, deterministic emergency session, recovery loop. | No direct restart integration test. | **PARTIALLY FIXED** | Source path is durable, but cancellation/submission/acknowledgement restart cases are unverified. |

Startup recovery is leader-gated. If no replica can acquire the database scheduler lock or database access is lost, recovery cannot proceed; alerts exist but market exposure remains external.

## 7. Broker Reconciliation Verification

Position, RMS, and margin endpoints exist in `backend/src/angel.rs`. Order and position books are fetched in `reconcile_live_user`; order fills are processed before `reconcile_broker_positions` runs.

Verified from source:

- Local positions are aggregated by `(exchange, token)` before signed-quantity comparison.
- Broker-flat/local-open and quantity/direction mismatch create durable incidents and set affected trades `RECONCILIATION_REQUIRED`.
- Average-price mismatch is detected above 0.01.
- Recently closed trades receive broker net/average/reconciliation timestamps.
- Known TP/SL over-close broker residuals are reconstructed as `EMERGENCY_CLOSING` trades.
- Related broker-only positions generate `ORPHAN_POSITION` incidents and block new matching/underlying exposure through `risk.rs:252-267`.

Verification result: **PARTIALLY FIXED** (`COMMON-CRITICAL-003`).

Remaining issues:

- Reconciliation audience is derived from nonterminal orders, not all connected/live-enabled users. Universal orphan detection is not proved.
- A broker-only position with no matching historical snapshot is skipped at `strategy.rs:6527-6531`.
- Orphan incidents remain status `open`; the code does not set `operator_required`, though the detail says operator intervention is required.
- A local closing trade is finalized from broker flatness using last known local price; final P&L may require trade-book reconciliation.
- No stateful position-book test covers multiple local rows, DB-only position, broker-only position, quantity/average mismatch, or incident resolution.

## 8. Protection Verification

The implemented live lifecycle is:

```text
entry fill transaction
  -> PROTECTION_REQUIRED + deadline
  -> PROTECTION_SUBMITTING + deterministic STOPLOSS_MARKET
  -> broker order-book reconciliation timestamp/ID
  -> PROTECTED
  -> target submission for uncovered target quantity
```

SL-before-TP is verified statically in both entry handlers. `protected_quantity` counts only submitted/partially-filled SL rows with a nonempty broker ID and `last_reconciled_at`, so local submission success alone is not confirmation.

Rejected/missing stops retry until deadline/attempt limit. Thereafter `begin_emergency_close` sets `EMERGENCY_CLOSING`, cancels known exits, waits for terminal cancellation, and submits one deterministic MARKET `EMERGENCY_CLOSE`. The migration adds one active target, one active stop family, and one active emergency close per trade.

Overall result: **PARTIALLY FIXED**.

Remaining risk:

- An ambiguous stop remains active for safety; `begin_emergency_close` will not submit a close while that stop is nonterminal. This is the correct no-blind-retry posture, but it means the system cannot guarantee an actual close order during broker uncertainty.
- A fresh quote is required for emergency-close construction even though the broker order is MARKET; a quote outage delays submission.
- Migration and uniqueness index were not tested on legacy data. Existing duplicate active exits intentionally make the index creation fail until manually reconciled.
- No broker sandbox confirms Angel accepts `STOPLOSS_MARKET` for every configured exchange/product.

## 9. Duplicate/Race Verification

| Original issue ID | Original problem | Code/fix location | Test covering it | Verification result | Remaining risk |
|---|---|---|---|---|---|
| `Audit §15` | Duplicate signal/order submission. | Signal unique key, signal/user/role intent unique index, order idempotency key, unique client tag, atomic claims, advisory locks, scheduler leader. | Durable intent/state tests and serialized risk reservation test. | **PARTIALLY FIXED** | No multi-replica end-to-end test; safeguards are nevertheless materially preserved. |
| `STRAT1-HIGH-001` | Concurrent sibling BUY/SELL fills. | Sequential opposite-fill accounting and sibling cancellation. | Arithmetic helper only. | **PARTIALLY FIXED** | Initial trade lookup/insert is not under a shared lock; two concurrent handlers can both insert. This is a live-trading blocker. |
| `COMMON-CRITICAL-004` | TP/SL double fill creates reverse exposure. | Monotonic deltas, sibling cancellation, position over-close detection/reconstruction. | Delta/cancellation helpers only. | **PARTIALLY FIXED** | Detection is after the race, dependent on position polling and source attribution; no OCO/reduce-only guarantee. |
| `Audit §20` | Partial fill, later delta, fill-after-cancel may be lost. | `reconciliation_plan`, cumulative watermarks, incremental price, `complete_order`, residual recovery. | Four fill/cancellation/watermark tests. | **PARTIALLY FIXED** | Helpers pass, but no database transaction failure or broker late-fill integration test exists. |
| `COMMON-HIGH-003` | Ambiguous entry blindly retried or left silent. | Ambiguous classification, deadline incident, no retry. | Angel ambiguity tests. | **PARTIALLY FIXED** | Alerting is implemented; definitive broker resolution remains external. |
| `Audit §22 kill/deactivation` | New entries or unsafe cancellations after kill/deactivation. | `enforce_entry_shutdowns` `9171+`; `risk::cancel_pending_entries` `448-460`; live submission recheck `3471-3533`. | `live_submission_guard_rechecks_kills_account_mode_and_session`. | **PARTIALLY FIXED** | No stateful test covers every pending/submitting/ambiguous/partial/cancelling state. Protective orders are correctly excluded from entry shutdown cancellation. |
| `Audit §23 market data` | Duplicate/out-of-order/corrupt ticks and duplicate candles. | Timestamp/sequence validation and candle identity paths described in §5. | Tick/candle/timestamp/completeness tests. | **VERIFIED FIXED** for local runtime behavior | In-memory watermark resets on restart; disconnect/reconnect and delayed cross-session packets lack integration coverage. |
| `Audit contract-roll risk` | Retry on new contract could duplicate an accepted original. | `confirm_original_terminal_nonfill` `3206-3246`; guarded roll `3597-3735`. | No direct contract-roll ambiguity test. | **PARTIALLY FIXED** | Replacement requires direct rejection, tag/order-book terminal zero fill or absent-after-rejection, and position flat. Broker tag retention/pagination and response semantics require sandbox verification. |

## 10. Tests Run

Executed during this read-only verification:

| Command | Result |
|---|---|
| `cargo test` | PASS — 117 passed, 0 failed, 0 ignored |
| `cargo clippy --tests -- -D warnings` | PASS |
| `cargo fmt -- --check` | PASS |
| `cargo check --tests` | PASS |
| `npm test -- --run` | PASS — 6 files, 18 tests |
| `npm run lint` | PASS |
| `npm run build` | PASS — 105 modules transformed |

Tests are predominantly pure/unit tests. They do not constitute the prompt's requested PostgreSQL + stateful broker fault-injection suite.

Not run:

- Migration application against PostgreSQL: neither `TEST_DATABASE_URL` nor `DATABASE_URL` was configured, and using an unknown database was unsafe.
- Angel sandbox calls: no isolated sandbox credentials/environment were supplied.
- Live broker calls, deployments, service restarts, or production health checks: prohibited and not performed.
- Multi-process/multi-replica concurrency and crash-kill tests: no test harness exists in the repository.

Test coverage gaps that matter financially:

- SL rejection/timeout/accepted-response-lost with persisted position.
- Crash at every fill/protection transaction boundary.
- Concurrent sibling BUY/SELL fill handlers using real PostgreSQL transactions.
- Concurrent TP/SL fill over-close and subsequent position reconstruction.
- Broker-only/local-only/multi-row quantity mismatch reconciliation.
- 15:20 quote failure, cancellation acknowledgement, close rejection, and restart.
- Contract-roll response-loss and tag/order-book retention behavior.
- Kill/deactivation across every nonterminal state.

## 11. Remaining Risks

1. **P0 concurrent Futures fill race:** sequential accounting exists, but no shared lock/constraint serializes initial trade creation for simultaneous siblings.
2. **P0 TP/SL race:** software detects/reconstructs some over-close outcomes but cannot prevent exchange-side double fill without verified OCO/reduce-only support.
3. **P0 reconciliation scope:** users without nonterminal strategy orders may not receive position polling; unknown tokens without a matching snapshot are skipped.
4. **P0 ambiguous protection:** safe blind flattening is impossible while an unobserved stop may later trigger. Exposure can remain pending operator/broker authority.
5. **No stateful verification:** green unit tests do not prove DB transaction, crash, network, broker, or multi-replica behavior.
6. **Migration unverified:** schema/index creation and legacy-row compatibility were not exercised on a disposable database.
7. **Validation gaps:** price bands are absent; lot count is not universally checked for equality with quantity/lot size; RMS/margin/tick units need sandbox confirmation.
8. **Operational documentation is inconsistent:** changed Futures and SuperTrend documents describe obsolete logic and order types.
9. **Recovery depends on scheduler leadership, database availability, valid encrypted credentials, current broker books, and usable quotes.** Alerts do not remove market exposure.
10. **Original medium/low debt remains:** swallowed session errors, bypassed transition validator, monolithic module, incomplete correlation enforcement, and conflicting proxy domain templates.

External areas remain **UNABLE TO VERIFY**: production host/deployment revision, production migration state, production database contents, actual Angel order/position/tag behavior, native OCO/reduce-only availability, exchange price bands, alert delivery, and real broker session/rate-limit behavior.

## 12. Safe for Paper Trading?

**CONDITIONALLY YES — only in an isolated paper/demo environment.**

Before paper testing:

1. Apply all migrations to a disposable PostgreSQL database and reconcile any duplicate active-exit rows that block the unique index.
2. Correct the stale strategy documentation or explicitly designate executable code plus this report as the temporary authority.
3. Run the missing stateful failure matrix with a deterministic broker simulator or Angel sandbox.
4. Monitor `PROTECTION_REQUIRED`, `PROTECTION_FAILED`, `EMERGENCY_CLOSING`, `RECONCILIATION_REQUIRED`, ambiguous orders, and open incidents.
5. Confirm no production credentials, endpoints, or databases are present.

Paper trading is appropriate specifically to validate the unresolved lifecycle, concurrency, and broker-schema assumptions. Passing paper tests must not be interpreted as proof of exchange-level race safety.

## 13. Safe for Live Trading?

**NO.**

No CRITICAL/P0 issue is verified fixed end to end. The concurrent sibling-fill race remains in executable source, broker reconciliation is not universal, TP/SL over-close is repaired only after detection, ambiguous protective submissions cannot always be safely flattened, and required database/broker crash/fault tests were not run.

The current code is materially safer than the audited implementation, but “safer” is not the same as verified safe for real capital.

## 14. Final Recommendation

Keep live trading disabled. Proceed with a dedicated fix/verification pass only after explicit authorization, prioritized as follows:

1. Serialize Futures fill accounting per user/strategy/instrument before the existing-trade read and through trade/order commit; add a concurrent PostgreSQL test.
2. Reconcile positions for every connected/live-enabled strategy user, including users with no active orders, and persist/quarantine unmatched tokens even without a snapshot.
3. Build a disposable PostgreSQL + stateful broker simulator test harness covering every P0 failure and restart boundary.
4. Validate or implement an explicit broker-native OCO/reduce-only policy; if unavailable, prove the residual detection/flattening latency and operator escalation path under double fills.
5. Strengthen universal lot/quantity equality semantics, authoritative price-band handling, and RMS/margin parsing with Angel sandbox fixtures.
6. Correct stale strategy/architecture documentation before any operational handoff.
7. Re-run this final verification. Live consideration should require all CRITICAL/P0 rows to be `VERIFIED FIXED`, all migrations tested, and all broker-specific assumptions demonstrated in an isolated sandbox.
