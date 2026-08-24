# Clear Trades Remediation Report

## Outcome

The Admin Clear Trades defect is fixed, committed, fully tested, packaged, and prepared on the production host. Production cutover was not performed because the mandatory authoritative Angel One position/order preflight could not run after all three daily broker sessions became invalid at the midnight IST rollover.

The safety gate failed closed. No Clear Trades request was issued against any production user, no broker order was submitted, no production service was stopped or restarted, and the running release remains `dfa5316ba210510fc3c176f2ba63cffa252e097c`.

## Previous Root Cause

The production implementation in `backend/src/auth.rs::clear_user_trade_logs` used:

```sql
DELETE FROM trades WHERE user_id=$1 AND status='closed'
```

It intentionally counted and preserved open trades and active orders. Consequently, running demo BUY/SELL trades were never deleted. Their simulated entry, SL, TP, fill, and protection rows also remained in `strategy_orders`, and their durable execution intents remained eligible. This was root cause **A/G**: the records survived because of the explicit status filter and incomplete deletion graph.

Trace results:

- PostgreSQL `trades` and `strategy_orders` are the authoritative demo position/order store.
- There is no independent simulated broker position store.
- Demo fills and SL/TP/protection state are represented by `strategy_orders` and trade columns.
- `strategy_execution_intents` can retain an old entry cycle after a reset.
- Broker position reconciliation is explicitly restricted to `execution_mode='live'`; it does not reconstruct demo trades.
- Strategy market/tick caches are shared market-data caches, not per-user demo position stores.
- P&L is fetched from PostgreSQL. The old frontend message accurately reflected the old backend behavior by saying open application records were preserved.

## Fix

`backend/src/auth.rs` now performs a complete, per-user, transactional demo reset after the existing admin, global-kill, advisory-lock, and authoritative broker-flat gates pass.

It removes:

- open, running, and closed demo trades;
- pending, submitted, partially filled, filled, SL, TP, and other demo orders;
- broker-order events through the order foreign-key cascade;
- demo risk decisions;
- demo-linked and pre-reset unbound entry intents;
- demo trade reversal intents;
- demo-related strategy events identified by execution mode, trade ID, or order ID;
- orphan signals and snapshots only when no other user/runtime record references them;
- saved backtest runs/trades through the established endpoint behavior.

The endpoint continues its previous cleanup of closed live trade history, but it never removes an open live trade. Account identity, profile, permissions, trading mode, broker credentials, broker mappings, strategy configuration/activation, risk configuration, instrument configuration, and settings are preserved.

The frontend confirmation copy now describes the complete demo reset and refetches the admin user data after success.

## Stale Execution Fence

Migration `20260824000000_admin_clear_demo_reset_fence.sql` adds `user_profiles.demo_state_reset_at`.

Clear Trades advances this boundary inside the same locked transaction as deletion. Durable entry execution carries the immutable originating signal time into the existing risk reservation transaction. A demo signal at or before the latest reset boundary is rejected while holding the same user advisory lock used by Clear Trades.

This provides both race orderings:

1. If execution reserves first, Clear Trades waits and then deletes the resulting demo state.
2. If Clear Trades holds/commits first, the waiting stale execution observes the reset boundary and fails closed.

A genuinely new post-reset signal remains eligible under normal strategy/risk rules after the kill switch is later released. Strategy configuration is not disabled or changed.

## Demo State Sources

| Source | Disposition |
|---|---|
| `trades` demo rows | Deleted for selected user, all statuses/directions |
| `strategy_orders` demo rows | Deleted for selected user, all roles/statuses |
| `broker_order_events` for demo orders | Deleted by FK cascade |
| `risk_decisions` in demo mode | Deleted for selected user |
| `strategy_execution_intents` | Demo-linked and stale pre-reset unbound entry intents deleted |
| `strategy_reversal_intents` | Demo-trade-linked rows deleted |
| `strategy_events` | Demo-mode/trade/order-related rows deleted |
| `strategy_signals` | Deleted only when orphaned after selected-user cleanup |
| `strategy_market_snapshots` | Deleted only when orphaned after selected-user cleanup |
| Backtest runs/trades | Deleted by the endpoint's established cascade |
| Reconciliation | Live-only; verified not to recreate demo state |
| In-memory market caches | Shared market data only; no per-user trade/position state to clear |
| Frontend | Admin data refetched; user P&L refresh reads the now-empty DB state |

## Tests Added

- `admin_clear_trades_removes_running_demo_graph_and_fences_stale_execution`
  - closed demo trade;
  - open demo BUY;
  - open demo SELL;
  - pending demo entry;
  - partially filled demo entry;
  - active demo SL and TP;
  - multiple and mixed demo trades;
  - USER A cleanup with USER B unchanged;
  - active-strategy advisory-lock race;
  - stale signal rejected during the race and after later kill-switch release;
  - fresh post-reset signal remains eligible;
  - live open record preserved;
  - live closed-history behavior preserved;
  - strategy/profile/activation preserved;
  - live reconciliation does not reconstruct demo state.
- `admin_clear_trades_rolls_back_atomically`
  - verifies both deleted rows and the reset boundary return after rollback.
- `AdminUsersPage.test.jsx`
  - verifies successful Clear Trades invokes the endpoint, displays complete-reset counts, and refetches admin state.

Existing live safety tests continue to verify admin authorization, global-kill enforcement independent of `FORCE_DEMO_TRADING`, authoritative flat/order-free broker interpretation, and fail-closed malformed broker responses.

## Test Results

| Validation | Result |
|---|---|
| Focused Clear Trades PostgreSQL tests | PASS — 2/2 |
| All stateful PostgreSQL/fake-broker tests | PASS — 18/18 |
| `cargo fmt -- --check` | PASS |
| `cargo check --tests` | PASS |
| `cargo test` | PASS — 123 passed, 0 failed, 18 intentionally ignored stateful tests |
| `cargo clippy --tests -- -D warnings` | PASS |
| `npm test -- --run` | PASS — 7 files, 19 tests |
| `npm run build` | PASS |
| `npm run lint` | PASS |

## Release

- Release commit: `8dadcf0e5569990c95c6dfbebb810d7115f6aaa9`
- Archive: `.runlogs/rulenix-release-8dadcf0.tar.gz`
- Archive size: 929,411 bytes
- Archive SHA-256: `c0a0b238fe7fd887ae4bc979365da1a56fa1d609e2190310fd58a96fa5fc91bf`
- Clean-tree release packaging: PASS
- Existing 44 production migration checksums: PASS
- Expected pending migration: `20260824000000`
- Candidate backend image: `sha256:c484e2a40b0b0a5260c53899c700f38c23d2ec15b3e8a015500baf4cc0d33b0d`
- Candidate frontend image: `sha256:e1edef5e9f0fe538d8f13a32bc6f09e5db11fd5b253686780ac6d621e231fbd1`
- Production environment copy: byte-identical
- Candidate `FORCE_DEMO_TRADING=false`: verified
- Candidate PostgreSQL `sslmode=verify-full` and CA mount: verified

## Backup and Migration Preflight

- Fresh encrypted backup: `/var/backups/rulenix/rulenix-predeploy-20260824T185439Z.dump.enc`
- Backup size: 10,044,352 bytes
- Backup SHA-256: `79f5c96ceecd0cfda50748f666d5083697ce721ba890675ac765623c639e380a`
- Full restore verification: PASS
- Restored users: 4
- Restored trades: 79
- Restored latest migration: `20260823020000`
- Candidate migration preflight: PASS
- Aggregate preservation across migration: PASS (`4|3|23|79|939|288|13876|939`)

The archive-list probe emitted the previously observed pipe warning and was not accepted as the backup gate. A separate full restore completed successfully.

## Production Cutover Gate

Preflight verified:

- current production release: `dfa5316ba210510fc3c176f2ba63cffa252e097c`;
- `FORCE_DEMO_TRADING=false`;
- global kill switch enabled;
- local open live trades: 0;
- local nonterminal live orders: 0;
- PostgreSQL, backend, frontend, internal readiness, public readiness, and TLS verify-full healthy;
- three broker accounts configured.

At `2026-08-24 18:30:02 UTC` (midnight IST), all three broker sessions were marked `invalid` and JWT/refresh/feed tokens were cleared; only encrypted API keys remain. Therefore Angel One order-book and position-book reads cannot be authenticated. Local application state is not an acceptable substitute for the required authoritative broker state.

Cutover stopped before any service stop, directory swap, migration, or deployment. The prepared candidate directory and verified backup are retained for a controlled retry after all relevant Angel One accounts reconnect and the broker-flat audit passes.

## Final Status

CLEAR CLOSED DEMO TRADES: PASS

CLEAR RUNNING DEMO BUY: PASS

CLEAR RUNNING DEMO SELL: PASS

CLEAR PENDING DEMO ORDERS: PASS

CLEAR DEMO POSITIONS: PASS

CLEAR DEMO SL/TP: PASS

CLEAR DEMO RUNTIME STATE: PASS

DEMO TRADE REAPPEARS AFTER RECONCILIATION: NO

DEMO TRADE REAPPEARS AFTER REFRESH: NO

STALE EXECUTION CAN RESURRECT CLEARED TRADE: NO

USER ISOLATION: PASS

USER DATA PRESERVATION: PASS

LIVE BROKER SAFETY PRESERVED: PASS

CLEAR TRADES PLACES BROKER ORDERS: NO

FORCE_DEMO_TRADING: FALSE

GLOBAL KILL SWITCH: ENABLED

POSTGRES HEALTH: PASS

BACKEND HEALTH: PASS

FRONTEND HEALTH: PASS

PUBLIC READINESS: PASS

NEW_RELEASE_COMMIT:

`8dadcf0e5569990c95c6dfbebb810d7115f6aaa9`

PRODUCTION DEPLOYED: NO

READY TO CLEAR RUNNING DEMO TRADES:

NO — the code and release are ready, but production still runs the previous release until authoritative Angel One position/order reads can confirm all three accounts are flat and order-free.
