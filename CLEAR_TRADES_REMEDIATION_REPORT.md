# Clear Trades Remediation Report

## Outcome

The corrected Admin Clear Trades release was deployed successfully and the authorized production demo reset completed transactionally.

- Running release: `96fe121f515e4bae695fec0dad75cd85f8429669`
- Previous release: `dfa5316ba210510fc3c176f2ba63cffa252e097c`
- Cutover UTC: `2026-08-25T02:03:06Z`
- `FORCE_DEMO_TRADING=false`: verified in the running backend container
- Global kill switch: enabled throughout
- Real Angel One orders/cancellations/position mutations: none
- Production demo state after cleanup: zero
- Production live trade/order rows before and after cleanup: zero

The originally authorized `8dadcf0e5569990c95c6dfbebb810d7115f6aaa9` candidate still coupled the entire maintenance request to an authenticated Angel One order/position read. With all three daily sessions expired, that would incorrectly block clearing purely simulated demo state. The authorization permitted a minimal correction. Commit `96fe121f515e4bae695fec0dad75cd85f8429669` implements that correction without changing strategy logic.

## Root Cause and Final Semantics

The first remediation correctly removed all demo trade/order/runtime rows, but its endpoint called Angel One before starting any cleanup. An expired or unavailable broker session therefore rejected even a purely demo reset.

The deployed behavior is now:

- Admin authorization remains mandatory.
- The global kill switch remains mandatory and is protected by the existing shared advisory lock.
- The affected user remains protected by the existing exclusive advisory lock.
- All demo state is cleared transactionally even when broker verification is unavailable.
- Closed live history is deleted only after an authoritative broker response reports both zero positions and zero nonterminal orders.
- If broker verification is unavailable, malformed, non-flat, or order-active, every live record is preserved while the demo reset proceeds.
- `FORCE_DEMO_TRADING` is not a Clear Trades prerequisite and remains `false`.
- Normal live-order execution safety is unchanged; the enabled global kill switch still blocks strategy execution.

The API response and append-only audit metadata now state whether broker state was verified and whether closed live history cleanup was permitted.

## Implementation

The final release includes:

- `backend/src/auth.rs`: separates the mandatory global-kill gate from optional broker verification for demo cleanup, preserves live history unless the broker is authoritatively flat/order-free, reports broker verification state, and retains transactional per-user cleanup.
- `backend/src/strategy.rs`: adds stateful coverage proving that unavailable broker verification clears demo state but preserves closed live history.
- `backend/migrations/20260824000000_admin_clear_demo_reset_fence.sql`: adds `user_profiles.demo_state_reset_at`.
- `backend/src/risk.rs`: rejects stale demo execution originating at or before the reset fence under the same user advisory lock.
- Frontend Clear Trades confirmation/refetch behavior from the prior verified remediation is unchanged.

No futures, SuperTrend, signal, pricing, sizing, margin, broker submission, protection, reconciliation, or live-order logic was modified in the final correction.

## Validation

Validation performed after the minimal correction:

| Check | Result |
|---|---|
| Clear Trades unit-policy tests | PASS — 3 passed |
| Clear Trades PostgreSQL tests | PASS — 3/3, serial |
| Expired/unavailable broker regression | PASS — demo removed, closed live preserved |
| Transaction rollback regression | PASS |
| Stale execution/reset-fence regression | PASS |
| `cargo fmt -- --check` | PASS |
| `cargo check --tests` | PASS |
| `cargo test` | PASS — 123 passed, 0 failed, 19 intentionally ignored stateful tests |
| `cargo clippy --tests -- -D warnings` | PASS |

The previously verified `8dadcf0` baseline remained valid for unchanged code: 18/18 stateful PostgreSQL/fake-broker tests, 19 frontend tests, frontend build, and lint all passed. The final change affected backend Clear Trades policy only, so the focused PostgreSQL regression plus complete Rust gates were rerun rather than repeating unrelated frontend and broker-race suites.

## Release Evidence

- Release commit: `96fe121f515e4bae695fec0dad75cd85f8429669`
- Release tree: `ec0d348858e3d404eff11e731d7a39d7cdef26b4`
- Archive: `.runlogs/rulenix-release-96fe121.tar.gz`
- Archive size: 933,664 bytes
- Archive SHA-256: `6d0a034f2c8bb99d1394277d4cd14b0770927c14b90a9e0e5813f786c03a1b7a`
- Running backend image: `sha256:d741ec2a582a350453d7884497095eecad5e7c4f4d9e86ac1699bf70634232f1`
- Running frontend image: `sha256:506103f229f49ed2eef27dddad7fcefe32dadc1fde2ed28481ac2b8e6f73b162`
- Rollback backend image: `sha256:a7fdc7c931c1671647dd73187b664cf88cae466d54194fffa543eb50a6d5c4c2`
- Rollback frontend image: `sha256:65105d6431d4af65ad92b016f04c6976cdcf5d656e3e5b54a8412cd7eb016ab6`
- Preserved previous directory: `/opt/rulenix.previous-20260825T020306Z`

The candidate images and rollback images were parked under explicit tags before cutover. The candidate was promoted only after the old backend stopped and final database safety gates passed.

## Backup, Migration, and TLS Evidence

- Fresh encrypted backup: `/var/backups/rulenix/rulenix-predeploy-20260825T015825Z.dump.enc`
- Backup size: 10,043,744 bytes
- Backup SHA-256: `4dcf60ae578d52ce4633ec8d97225fe063170fe5d38c95708a7dbd2decb7f5f0`
- Full disposable restore: PASS
- Restored users: 4
- Restored trades: 79
- Restored latest migration: `20260823020000`
- Migration preflight against the restored backup: PASS
- Preflight aggregate preservation: PASS — `4|3|23|79|939|288|13876|939`
- Final migration state: 45 successful, 0 failed, latest `20260824000000`
- Host CA path: regular file and matches the authoritative CA
- Backend container CA path: regular file
- PostgreSQL TLS: verify-full, TLS 1.3

The archive-list probe emitted the previously observed pipe warning. It was not used as the recovery gate; the separate full restore with `--exit-on-error` passed.

## Production Classification and Cleanup

Immediately before cleanup:

| State | Count |
|---|---:|
| Demo closed trades | 70 |
| Demo open/running trades | 9 |
| Demo orders | 939 |
| Demo risk decisions | 1,873 |
| Demo-associated events | 1,137 |
| Demo-linked execution intents | 0 |
| Candidate demo snapshots | 188 |
| Demo reversal intents removed | 2 |
| Live trades, all statuses | 0 |
| Live orders, all statuses | 0 |
| Unresolved live broker-position incidents | 0 |
| Closed live history eligible/intended for cleanup | 0 |

The backend was stopped for the reset. The transaction acquired the global shared risk lock plus every affected user's exclusive advisory lock, advanced all three trading profiles' reset fences, deleted the complete demo graph in dependency-safe order, asserted zero residual demo state, and appended `production_demo_trading_state_reset` to the immutable audit log before commit.

After cleanup:

| State | Count |
|---|---:|
| Demo trades | 0 |
| Demo orders | 0 |
| Demo risk decisions | 0 |
| Demo-mode events | 0 |
| Backtest runs/trades | 0 / 0 |
| Live trades | 0 |
| Live orders | 0 |

There was no closed live history to delete. No open/running live record existed or was deleted.

## Preservation Evidence

Counts immediately after deployment and cleanup:

- Users: 4
- User profiles / broker accounts: 3
- Encrypted broker credential records: 3
- Strategy configurations: 23
- Strategy activations: 9
- Risk configurations: 1
- Kill-switch records: 1

Canonical hashes of users/permissions, profile configuration, encrypted broker secrets, strategy configuration, strategy activation, and risk configuration matched the pre-cleanup baseline exactly. `updated_at`, login timestamps, and the new reset fence were excluded only where those maintenance fields were expected to change.

## Runtime Verification

After final restart and 291 seconds of scheduler observation:

- Demo trades reappeared: no
- Demo orders reappeared: no
- Scheduler leadership: acquired
- Live orders/trades/broker-order events created or updated since cutover: 0 / 0 / 0
- Backend panic/fatal log records: 0
- Backend restart count: 0
- PostgreSQL health: pass
- Backend health: pass
- Frontend health: pass
- Internal readiness: pass
- Public readiness and frontend root fetch: pass
- Global kill switch: enabled
- `FORCE_DEMO_TRADING`: false inside the running backend

All three Angel One sessions were expired during classification, cleanup, and the 291-second preservation audit. The task explicitly authorized clearing purely simulated demo state without those sessions. No current broker order/position request was claimed as authoritative, no live record was guessed closed, and no Angel One mutation was attempted. The prior authoritative broker audit had reported three accounts checked, zero exposure, and zero nonterminal broker orders; current durable production state contains no live trade/order/incident rows and shows no live activity since cutover.

A later read-only recheck at `2026-08-25T02:11:56Z` observed normal external/account activity: one of the three broker sessions had reconnected, increasing encrypted broker-secret rows from 3 API-key-only records to 6 records, while two sessions remained invalid. The new credential/session state was preserved and was not rolled back. At that late recheck, demo trades/orders/risk/events remained zero, live trades/orders/incidents remained zero, the global kill switch remained enabled, and `FORCE_DEMO_TRADING` remained false. The immediate post-cleanup credential hash had already matched the pre-cleanup baseline before this later session change.

## Final Status

CODE DEPLOYED: YES

RUNNING COMMIT: `96fe121f515e4bae695fec0dad75cd85f8429669`

DEMO CLOSED TRADES CLEARED: PASS

DEMO OPEN TRADES CLEARED: PASS

DEMO RUNNING TRADES CLEARED: PASS

DEMO POSITIONS CLEARED: PASS

DEMO PENDING ORDERS CLEARED: PASS

DEMO SL/TP STATE CLEARED: PASS

DEMO RUNTIME STATE CLEARED: PASS

DEMO TRADES REAPPEARED: NO

CLOSED LIVE HISTORY CLEARED: PASS — no closed live records existed

OPEN/RUNNING LIVE RECORDS DELETED: NO

EXPECTED: NO

REAL BROKER ORDERS PLACED: NO

EXPECTED: NO

REAL BROKER POSITIONS MODIFIED: NO

EXPECTED: NO

USERS PRESERVED: PASS

BROKER CREDENTIALS PRESERVED: PASS

STRATEGY CONFIGS PRESERVED: PASS

RISK CONFIGS PRESERVED: PASS

FORCE_DEMO_TRADING: FALSE

GLOBAL KILL SWITCH: ENABLED

POSTGRES TLS VERIFY-FULL: PASS

POSTGRES HEALTH: PASS

BACKEND HEALTH: PASS

FRONTEND HEALTH: PASS

PUBLIC READINESS: PASS

READY TO USE CLEAR TRADES: YES
