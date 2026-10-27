# Production Deployment Report

## Successful Retry Deployment

- Deployment date: 2026-08-24
- Controlled cutover started: 2026-08-24 21:51:01 +05:30 (IST)
- Authorized commit: `2b3f846763a6d98a699d59aa09d4f5b319aab53e`
- Authorized archive: `rulenix-release-2b3f846.tar.gz`
- Authorized/server SHA-256: `2c5cf7baf638e2bf377b4bc692c36083ca7d840269308b05b17025090771d286`
- Outcome: **SUCCESS — EXACT VERIFIED RELEASE RUNNING**

No source, strategy logic, migration, or release commit was modified during deployment. No Angel One execution test, live order, cancellation, or live-trading enablement was performed.

### Final preflight

- Local HEAD: PASS — exact authorized commit
- Local working tree release gate: PASS — operational report was recoverably stashed, the release was verified on a clean tree, and the report was restored after deployment
- Archive checksum locally and on production: PASS
- Existing 39 migration checksums against the candidate: PASS
- History-preserving `20260823000000_remove_margin_and_option_entry.sql`: PASS — no destructive data statement
- Five pending migrations on a disposable full restore: PASS
- Migration dry-run aggregate preservation: PASS — `4|3|23|117|939|283|13868|939`
- Candidate backend build: PASS
- Candidate frontend build: PASS
- Final pre-cutover health/readiness: PASS
- Final pre-cutover live exposure/nonterminal orders: `0|0`
- Final pre-cutover force-demo/kill-switch gates: TRUE/ENABLED

Two intermittent SSH connection attempts timed out before remote execution. Each was followed by a fresh health/state check; neither timeout changed production or advanced the deployment phase.

### Fresh recovery point

- Encrypted backup: `/var/backups/rulenix/rulenix-predeploy-20260824T161329Z.dump.enc`
- Encrypted backup size: 10,029,392 bytes
- Encrypted backup SHA-256: `a29b227f875f9b189b833f37bcb324fcae222c38baee631128c3186d3f8f648b`
- Full disposable restore with `--exit-on-error`: PASS
- Restored counts: users 4, broker-linked accounts 3, configurations 23, trades 117, orders 939, snapshots 283, strategy events 13,868, broker order events 939
- Restored migration state: 39 successful, 0 failed, latest `20260816000000`
- Restore procedure: AVAILABLE

The fresh backup supersedes the older recovery point for this deployment. The older encrypted backup and all TLS/CA rollback artifacts remain retained.

### CA and TLS gate

- Production CA host path: `/opt/rulenix/secrets/postgres_ca.crt`
- Host path type after release swap: REGULAR FILE (`0644 deploy:deploy`)
- Container path: `/run/secrets/postgres_ca.crt`
- Container path type: REGULAR FILE
- Bind mount: `/opt/rulenix/secrets/postgres_ca.crt:/run/secrets/postgres_ca.crt`, type `bind`, read-only
- Candidate CA matched `/etc/rulenix/postgres-tls/ca.crt` before and after swap: YES
- Certificate chain and `postgres` hostname verification: PASS
- Backend configuration requires `sslmode=verify-full` and the mounted CA: PASS
- Backend-equivalent production-network verification: PASS — TLS 1.3
- PostgreSQL TLS enabled/health: YES/PASS
- PostgreSQL restart during retry: NO

### Controlled cutover and exact release evidence

- Prepared candidate directory: `/opt/rulenix.next-2b3f846`
- Preserved previous release: `/opt/rulenix.previous-20260824T162101Z`
- Release marker: `2b3f846763a6d98a699d59aa09d4f5b319aab53e`
- Running backend image: `sha256:83d9b40ff4c89e88ffd555b345351f4c6bf80b66357e3abdbbad870955db5fe6`
- Running frontend image: `sha256:425913c8cd0bec571e79abcd73a3605489991776fa00702eaf16969ffa58a1f7`
- Running images match the IDs recorded immediately after candidate build: YES
- Backend restart count after cutover: 0
- Candidate migrations applied by SQLx: 5
- Final migration state: 44 successful, 0 failed, latest `20260823020000`

Applied migrations:

1. `20260724025000_strategy_snapshot_execution_metadata.sql`
2. `20260819000000_durable_signal_fanout.sql`
3. `20260823000000_remove_margin_and_option_entry.sql`
4. `20260823010000_execution_safety_lifecycle.sql`
5. `20260823020000_p0_execution_safety.sql`

### Data preservation comparison

| Aggregate | Fresh backup baseline | After deployment | Result |
|---|---:|---:|---|
| Users | 4 | 4 | PASS |
| Broker-linked accounts | 3 | 3 | PASS |
| User profiles | 3 | 3 | PASS |
| Strategy configurations | 23 | 23 | PASS |
| Risk limits | 1 | 1 | PASS |
| Kill-switch records | 1 | 1 | PASS |
| Trades | 117 | 117 | PASS |
| Orders | 939 | 939 | PASS |
| Filled orders | 316 | 316 | PASS |
| Cumulative filled quantity | 23,548 | 23,548 | PASS |
| Strategy snapshots | 283 | 283 | PASS |
| Strategy events | 13,868 | 13,868 | PASS |
| Broker order events | 939 | 939 | PASS |
| Audit events | 599 | 599 | PASS |
| Risk decisions | 1,873 | 1,873 | PASS |
| Reconciliation-health records | 0 | 0 | PASS |
| Alert attempts | 0 | 1 | PASS — expected service-start alert |

No preserved aggregate decreased. No production record was manually deleted, truncated, reset, or reconstructed.

### Runtime and trading-safety verification

- PostgreSQL: healthy
- Backend: healthy
- Frontend: running/healthy through readiness routing
- Internal readiness: PASS — `http://127.0.0.1:8080/api/health/ready`
- Public readiness: PASS — `https://rulenix.in/api/health/ready`
- Scheduler leadership acquired: 1
- Service-start alert recorded: 1
- Protection-recovery errors after cutover: 0
- Reconciliation errors after cutover: 0
- Panic/fatal log records after cutover: 0
- `FORCE_DEMO_TRADING`: TRUE
- Global kill switch: ENABLED
- Open live trades: 0
- Nonterminal live orders: 0
- Live order rows created/updated since cutover: 0
- Live broker order-event rows since cutover: 0
- Live trade rows created/updated since cutover: 0
- Unexpected live entry/SL/TP/cancellation/emergency-close activity: NO

The exact verified code is deployed and ready for a separately authorized Angel One sandbox/capability verification phase. This deployment does not approve or enable live trading.

## Previous Failed Deployment Attempt

- Timestamp: 2026-08-24 20:36:28 +05:30 (IST)
- Candidate release commit: `2b3f846763a6d98a699d59aa09d4f5b319aab53e`
- Archive: `rulenix-release-2b3f846.tar.gz`
- Expected SHA-256: `2c5cf7baf638e2bf377b4bc692c36083ca7d840269308b05b17025090771d286`
- Outcome: **STOPPED DURING BACKEND CUTOVER; PREVIOUS RELEASE RESTORED**

No Angel One connection, live broker test, order placement, cancellation, or live-trading enablement was authorized or performed.

## Release Verification

- Local HEAD matched the exact candidate commit: PASS
- Local Git status before transfer: CLEAN
- Local archive SHA-256: PASS
- Production archive SHA-256: PASS
- Archive extracted into a separate release directory: PASS
- Production release marker in the prepared directory: PASS
- Applied migration checksums checked: 39/39 PASS
- History-preserving `20260823000000_remove_margin_and_option_entry.sql`: CONFIRMED
- Backend production image build: PASS
- Frontend production image build: PASS

The running `/opt/rulenix` tree was not overwritten in place. The previous release was moved aside for rollback, the candidate was swapped in, and the previous release was restored after the stop condition.

## Backup Result

- Backup created: YES
- Backup path: `/var/backups/rulenix/rulenix-predeploy-20260824T144644Z.dump.enc`
- Backup bytes: 10,029,008
- Backup non-empty: YES
- Full restore into disposable verification database: PASS
- Restored users: 4
- Restored trades: 117
- Restored orders: 938
- Restored latest migration: `20260816000000`
- Restore procedure available: YES

The initial helper's archive-list probe emitted a pipe/trailing-CR warning. It was not accepted as the backup gate. A separate clean full restore was run and passed before any TLS or database-state change.

## PostgreSQL TLS

Server-side TLS provisioning succeeded:

- PostgreSQL `ssl=on`: PASS
- CA/server certificate/server key created: PASS
- Server certificate chain: PASS
- Server certificate SAN contains `DNS:postgres`: PASS
- Server certificate and private key match: PASS
- Data-volume server key: `0600 postgres:postgres`
- Data-volume server certificate: `0644 postgres:postgres`
- Verify-full from the `rulenix_default` Compose network: PASS — TLS 1.3
- Pre-TLS PostgreSQL configuration rollback copy: `/var/backups/rulenix/postgres-tls-config-20260824T144819Z`

End-to-end backend TLS failed at cutover. The production bind source `/opt/rulenix/secrets/postgres_ca.crt` is a **directory**, not a certificate file. The TLS provisioning step placed the CA certificate inside that directory; Compose therefore mounted a directory at `/run/secrets/postgres_ca.crt`. The candidate backend failed closed with:

```text
Error: could not connect to PostgreSQL
Caused by: Is a directory (os error 21)
```

The candidate never connected to PostgreSQL. TLS was not weakened and the deployment was not retried.

PostgreSQL remains SSL-enabled and accepts a verified TLS 1.3 connection using the correctly staged CA at `/etc/rulenix/postgres-tls/ca.crt`. The restored previous backend retains its previous database URL and therefore does not satisfy the release's end-to-end verify-full requirement.

## Migration Result

The candidate backend failed before obtaining a database connection, so SQLx did not apply any pending migration.

- Applied migrations before cutover: 39
- Applied migrations after rollback: 39
- Latest migration before/after: `20260816000000`
- Failed migration records: 0
- Candidate pending migrations applied: NO
- Manual migration-history edits: NONE
- Production schema/data restore required: NO

## Data Preservation

| Category | Before | After rollback | Requirement | Result |
|---|---:|---:|---|---|
| Users | 4 | 4 | Equal | PASS |
| Broker-linked accounts | 3 | 3 | Equal | PASS |
| User strategy configurations | 23 | 23 | Equal | PASS |
| Trades | 117 | 117 | No decrease | PASS |
| Orders | 938 | 939 | No decrease | PASS |
| Strategy snapshots | 283 | 283 | No decrease | PASS |
| Strategy events | 13,866 | 13,868 | No decrease | PASS |

No user, profile, permission, broker mapping, encrypted credential, strategy configuration, risk configuration, trade, historical order, snapshot, or strategy event was deleted or reset.

The one additional order/event row was investigated. It is a synthetic `demo` `SL2` protection order with a `DEMO-*` reference, zero fill, and no new trade. There were zero recent `live` rows with broker references. It was not an Angel One broker mutation.

## Trading Safety

- `FORCE_DEMO_TRADING`: TRUE
- Global kill switch/new-entry gate: ENABLED
- Open live trades before: 0
- Open live trades after rollback: 0
- Nonterminal live orders before: 0
- Nonterminal live orders after rollback: 0
- Recent actual live broker rows: 0
- Live trading enabled: NO

The global kill switch was enabled after the verified backup and remains enabled. Individual user strategy configurations were not disabled or deleted. Protection/reconciliation behavior was not manually altered.

## Service Health After Rollback

- PostgreSQL container: healthy
- Backend container: healthy
- Frontend container: running
- Local readiness: PASS — `http://127.0.0.1:8080/api/health/ready`
- Public readiness: PASS — `https://rulenix.in/api/health/ready`
- Backend crash loop: NO
- PostgreSQL SSL active: YES
- Exact candidate commit running: NO
- Current release marker: absent, as before this attempt

These health results are for the restored previous application release.

## Broker Activity

- Angel One authentication/test: NOT PERFORMED
- Live strategy entry submitted: NO
- Live SL/TP submitted: NO
- Live emergency close submitted: NO
- Live cancellation submitted: NO
- Unexpected live broker activity: NO
- Synthetic demo protection activity: one SL2 row, not a broker mutation

## Rollback

- Stop condition detected: YES
- Candidate backend stopped: YES
- Database migration count verified unchanged before rollback: YES
- Candidate retained for diagnosis: `/opt/rulenix.failed-20260824T150343Z`
- Previous `/opt/rulenix` release restored: YES
- Previous backend/frontend images rebuilt: PASS
- Previous backend/frontend restarted: PASS
- Local/public readiness after rollback: PASS
- Backup database restore needed: NO — no migration ran and production counts remained intact
- Rollback result: **PASS**

## PostgreSQL CA Mount Remediation

- Remediation timestamp: 2026-08-24 21:31:42 +05:30 (IST)
- Original CA source type: DIRECTORY (`0755 deploy:deploy`), containing exactly one regular file named `ca.crt`
- Actual authoritative CA source: `/etc/rulenix/postgres-tls/ca.crt`
- Original nested CA matched the authoritative CA: YES
- Final CA host path: `/opt/rulenix/secrets/postgres_ca.crt`
- Final CA source type: REGULAR FILE (`0644 deploy:deploy`)
- Container CA path: `/run/secrets/postgres_ca.crt`
- Container CA type: REGULAR FILE
- Compose mount: `./secrets/postgres_ca.crt:/run/secrets/postgres_ca.crt:ro`
- Resolved bind: `/opt/rulenix/secrets/postgres_ca.crt:/run/secrets/postgres_ca.crt`, type `bind`, read-only
- Certificate chain verification: PASS
- Certificate hostname verification: PASS — server certificate CN and SAN are valid for `postgres`
- Verify-full result: PASS — TLS 1.3 from a disposable backend-equivalent container on `rulenix_default` using the production bind
- Negative TLS verification: PASS — wrong hostname, wrong CA, and missing CA each failed closed
- PostgreSQL TLS enabled: YES
- PostgreSQL restart required/performed: NO
- PostgreSQL health: PASS — healthy
- Backend health: PASS — healthy
- Frontend health: PASS — running
- Internal readiness: PASS — `http://127.0.0.1:8080/api/health/ready`
- Public health: PASS — `https://rulenix.in/api/health/ready`
- User data preservation: PASS — users 4, broker-linked accounts 3, user strategy configurations 23
- Historical data preservation: PASS — trades 117, orders 939, snapshots 283, strategy events 13,868, broker order events 939
- Migration preservation: PASS — 39 successful, 0 failed, latest `20260816000000`; no candidate migration ran
- Trading safety state: `FORCE_DEMO_TRADING=true`; global kill switch ENABLED; open live trades 0; nonterminal live orders 0; recent live broker rows 0
- Unexpected live broker activity: NO
- Candidate release deployed: NO
- Candidate release marker running: NO
- Current backend container recreation: PASS — restored-release backend image ID unchanged; recreation was required only to rebind the corrected file
- CA/mount rollback artifact: `/var/backups/rulenix/postgres-ca-mount-20260824T160142Z`

The original directory, its sole CA file, and mount metadata were preserved in the new rollback artifact before replacement. No certificate contents, private keys, passwords, tokens, user records, historical trading records, broker state, or migration history were changed. The candidate release was not deployed.

## Previous Retry Requirement (Completed)

The CA source blocker was remediated, its backend-equivalent `sslmode=verify-full` gate passed, and the separately authorized retry completed successfully without weakening TLS.

## Final Status

CODE DEPLOYED: YES

EXPECTED COMMIT RUNNING: YES

RELEASE CHECKSUM: PASS

PREDEPLOYMENT BACKUP: PASS

CA DIRECTORY PROBLEM FIXED: YES

CA HOST PATH IS REGULAR FILE: YES

CA CONTAINER PATH IS REGULAR FILE: YES

POSTGRESQL TLS VERIFY-FULL: PASS

DATABASE MIGRATIONS: PASS

USER DATA PRESERVED: PASS

HISTORICAL DATA PRESERVED: PASS

POSTGRES HEALTH: PASS

BACKEND HEALTH: PASS

FRONTEND HEALTH: PASS

PUBLIC READINESS: PASS

FORCE_DEMO_TRADING: TRUE

GLOBAL KILL SWITCH: ENABLED

OPEN LIVE EXPOSURE: ZERO

UNEXPECTED LIVE BROKER ACTIVITY: NO

READY FOR ANGEL ONE VERIFICATION: YES

## Production Trading History Reset

### Outcome

- Attempt timestamp: 2026-08-24 22:54:53 +05:30 (IST)
- Requested operation: remove production application trading/execution history for all users while preserving every user, account, credential, and configuration record
- Outcome: **STOPPED BEFORE THE PRODUCTION DELETE TRANSACTION**
- Stop condition: `/opt/rulenix/backend/.env.production` contained `FORCE_DEMO_TRADING=false`
- Production trading rows deleted: 0
- Source, schema, migrations, images, and application configuration modified: NO

The request required both `FORCE_DEMO_TRADING=true` and the global kill switch to remain enabled. The global kill switch was enabled, but the force-demo configuration had changed from the previously verified value to `false`. Changing application configuration was outside this database-cleanup authorization, so the production reset was not executed.

### Backup result

- New encrypted backup: `/var/backups/rulenix/rulenix-predeploy-20260824T171212Z.dump.enc`
- Backup bytes: 10,059,888
- Backup SHA-256: `0c7c317e35d80b03cf1f2f9414ffc0f22a6a80796c24689bcd2c8e85a110f144`
- Backup non-empty: YES
- Full disposable restore with `--exit-on-error`: PASS
- Restored preserved/trading aggregates: exact match to production baseline
- Restored migration state: 44 successful, 0 failed, latest `20260823020000`
- Restore procedure: AVAILABLE

The backup is retained. The archive-list probe emitted the previously observed pipe warning, but it was not used as the verification gate; a separate complete restore and aggregate comparison passed.

### Broker exposure check

All three linked broker accounts were checked twice using read-only Angel One order-book and position-book GET requests. Credentials were decrypted only in memory inside a disposable container; no credential or broker payload was printed or persisted.

- Broker accounts checked: 3/3
- Broker read failures: 0
- Nonzero broker net positions: 0
- Nonterminal broker orders: 0
- Orders placed/cancelled/modified: 0
- Broker state mutation: NONE

### Schema and foreign-key analysis

The live PostgreSQL catalog, columns, foreign keys, delete rules, and relevant triggers were inspected before any reset decision.

Category A — preserved user/account data:

- `users`
- `user_profiles`
- `user_sessions`
- `email_otps`
- `broker_secrets`

Category B — preserved user/risk configuration:

- `user_strategy_configs`
- `user_strategy_activations`
- `risk_limits`
- `risk_kill_switches`
- `market_calendar`

Category C — disposable trading/execution/runtime data identified for the reset:

- `trades`
- `strategy_orders` — includes fill watermarks and SL/TP/protection/emergency/square-off order state
- `strategy_market_snapshots`
- `strategy_events`
- `broker_order_events`
- `broker_position_incidents`
- `broker_reconciliation_health`
- `risk_decisions`
- `strategy_signals`
- `strategy_execution_intents`
- `strategy_reversal_intents`
- `strategy_scheduler_runs`
- `market_price_ticks`
- `ichimoku_signal_evaluations`
- `backtest_runs`
- `backtest_trades`

Category D — mixed/operational data preserved:

- `audit_events` — append-only security, administration, configuration, and trade-log audit evidence; deletion is prohibited by the schema
- `alert_delivery_attempts` — only operational `service_started` delivery attempts were present
- `data_repair_backup` — immutable repair/rollback evidence, not active execution state

Category E — preserved system/reference data:

- `_sqlx_migrations`
- `job_runs`
- `backtest_market_candles`
- `backtest_option_contracts`
- `broker_margin_estimates`

Foreign-key order was verified explicitly. `broker_order_events` depends on `strategy_orders`; orders and reversal intents depend on snapshots/trades; execution intents reference signals/orders/trades; backtest trades cascade from backtest runs. No constraint disabling or schema change was planned.

### Baseline and exact dry run

Preserved baseline:

| Category | Count |
|---|---:|
| Users | 4 |
| User profiles/settings | 3 |
| User sessions | 3 |
| Email OTP rows | 0 |
| Broker-linked accounts | 3 |
| Encrypted broker credential records | 12 |
| Strategy configurations | 23 |
| Strategy activations | 9 |
| Risk-limit records | 1 |
| Kill-switch records | 1 |

Trading/runtime baseline:

| Table/category | Before | Production after stopped attempt |
|---|---:|---:|
| Trades | 117 | 117 |
| Orders | 939 | 939 |
| Filled-order records | 316 | 316 |
| Cumulative filled quantity | 23,548 | 23,548 |
| Strategy snapshots | 283 | 283 |
| Strategy events | 13,868 | 13,868 |
| Broker order events | 939 | 939 |
| Strategy signals | 1 | 1 |
| Execution intents | 0 | 0 |
| Reversal intents | 5 | 5 |
| Scheduler runs | 1,064 | 1,064 |
| Position incidents | 0 | 0 |
| Reconciliation-health rows | 0 | 0 |
| Risk decisions | 1,873 | 1,873 |
| Market-price ticks | 46 | 46 |
| Ichimoku signal evaluations | 63 | 63 |
| Backtest runs | 2 | 2 |
| Backtest trades | 14 | 14 |

The exact dependency-ordered SQL transaction was tested against a disposable full restore. It deleted the expected rows, reduced every Category C table to zero, preserved every critical user/configuration/credential fingerprint, preserved all reference/system counts, and committed successfully. That dry-run database was then dropped. The same SQL was **not** executed against production because the force-demo gate failed.

### Application consistency and service health

The backend was briefly stopped after the second successful broker-flat check to prevent concurrent runtime writes. The force-demo mismatch was detected before opening the production reset transaction. The unchanged backend was immediately restarted.

- Exact release remains `2b3f846763a6d98a699d59aa09d4f5b319aab53e`
- PostgreSQL: healthy
- Backend: healthy
- Frontend: healthy through readiness routing
- Internal readiness: PASS
- Public readiness: PASS
- Global kill switch: ENABLED
- Open live application trades: 0
- Nonterminal live application orders: 0
- Open live broker positions: 0
- Nonterminal broker orders: 0
- `FORCE_DEMO_TRADING`: FALSE — blocking condition remains

TRADING HISTORY RESET: FAIL

USERS PRESERVED: PASS

BROKER ACCOUNTS PRESERVED: PASS

BROKER CREDENTIAL RECORDS PRESERVED: PASS

USER SETTINGS PRESERVED: PASS

STRATEGY CONFIGS PRESERVED: PASS

RISK CONFIGS PRESERVED: PASS

TRADES BEFORE: 117

TRADES AFTER: 117

ORDERS BEFORE: 939

ORDERS AFTER: 939

FILLS BEFORE: 316

FILLS AFTER: 316

SNAPSHOTS BEFORE: 283

SNAPSHOTS AFTER: 283

STRATEGY EVENTS BEFORE: 13868

STRATEGY EVENTS AFTER: 13868

OTHER TRADING TABLES CLEANED: NONE — disposable-restore dry run only; production transaction not executed

OPEN LIVE BROKER EXPOSURE: ZERO

NONTERMINAL LIVE BROKER ORDERS: ZERO

FORCE_DEMO_TRADING: FALSE

GLOBAL KILL SWITCH: ENABLED

POSTGRES HEALTH: PASS

BACKEND HEALTH: PASS

FRONTEND HEALTH: PASS

PUBLIC READINESS: PASS

READY FOR CLEAN STRATEGY TESTING: NO

## Admin Clear Trades Safety Deployment

### Outcome

- Completed: 2026-08-24 23:40:16 +05:30 (IST)
- Release commit: `dfa5316ba210510fc3c176f2ba63cffa252e097c`
- Previous release: `2b3f846763a6d98a699d59aa09d4f5b319aab53e`
- Release archive: `rulenix-release-dfa5316.tar.gz`
- Archive SHA-256: `b04d7eab79dcea61ec165dcb4bd2a8698012f299ed43af0db2b732befa646070`
- Deployment result: **PASS**
- Preserved rollback release: `/opt/rulenix.previous-20260824T180453Z`

The earlier trading-history reset stop remains an accurate record of the safety requirements in force for that attempt. Production ownership subsequently clarified that `FORCE_DEMO_TRADING=false` is intentional so authorized users can select live mode. The value was not changed during this remediation or deployment.

### Implementation verification

The existing authenticated Admin Clear Trade Logs endpoint did not inspect `FORCE_DEMO_TRADING`, so no force-demo restriction was removed. It was missing the required maintenance safety gates and was hardened as follows:

- administration permission is still required before any operation;
- the endpoint acquires the same shared global-risk lock and exclusive per-user lock used by live execution safety;
- the global kill switch must be enabled and cannot be disabled until the transaction completes;
- a linked user must have usable encrypted Angel One session credentials;
- Angel One order-book and position-book reads run inside the locked transaction;
- any broker read error, malformed position quantity, nonzero net position, unknown/nonterminal order status, or disabled global kill switch fails closed and rolls back;
- only after all gates pass are the existing closed P&L and saved-backtest deletions executed and committed;
- open application trade/order records remain preserved, consistent with the existing feature contract;
- broker-flat counts and kill-switch state are included in the administrative audit/result metadata;
- `FORCE_DEMO_TRADING` is deliberately not an input to this maintenance gate.

Normal strategy and live-order execution code was not changed. The global kill switch continues to reject new live submissions while allowing necessary protective/reconciliation behavior.

### Local verification

| Gate | Result |
|---|---|
| `cargo fmt -- --check` | PASS |
| `cargo check --tests` | PASS |
| `cargo test` | PASS — 123 passed, 0 failed, 16 intentionally ignored isolated-PostgreSQL tests |
| `cargo clippy --tests -- -D warnings` | PASS |
| `npm test -- --run` | PASS — 6 files, 18 tests |
| `npm run build` | PASS |
| `npm run lint` | PASS |

New unit coverage verifies authoritative flat broker responses, nonzero positions, nonterminal and unknown orders, malformed position fail-closed behavior, and that the maintenance gate depends on the global kill switch rather than force-demo mode.

### Backup and release preparation

- Fresh encrypted backup: `/var/backups/rulenix/rulenix-predeploy-20260824T175159Z.dump.enc`
- Backup bytes: 10,059,840
- Backup SHA-256: `f9509281ebf94bbf7c7cf6b281fa4472bbfeff3be4b15c18e013c697191fc01a`
- Full disposable restore with `--exit-on-error`: PASS
- Restored users/trades/latest migration: `4 / 117 / 20260823020000`
- Candidate environment copy: byte-identical to production
- Applied migration checksums: PASS — 44/44
- Pending migrations: 0
- Candidate backend image: `sha256:f6be963fa00a5c328dcafd76b6cf9f7cf69e35277464d1299bb148d6efb45585`
- Candidate frontend image: `sha256:004e6cb768b42e87c849bdb519c9596e13daa8d7cb0d61e912a77672b8c951cb`

### Production safety and post-deployment verification

All three linked broker accounts were checked repeatedly before and after cutover using read-only Angel One order-book and position-book requests. Credentials were decrypted only in memory and were not printed or persisted.

- Broker accounts checked per audit: 3/3
- Broker read failures: 0
- Open live broker positions: 0
- Nonterminal broker orders: 0
- Broker mutations: 0
- Global kill switch: ENABLED throughout
- `FORCE_DEMO_TRADING`: FALSE before, during, and after deployment
- Open live application trades: 0
- Nonterminal live application orders: 0
- New/updated live order rows since cutover: 0
- Live broker-order events since cutover: 0
- New/updated live trade rows since cutover: 0
- Unexpected live broker activity: NO
- Production Admin Clear Trades call: NOT EXECUTED — no user's history was destructively tested during deployment

Production aggregates remained unchanged across cutover:

```text
USERS=4
BROKER_ACCOUNTS=3
STRATEGY_CONFIGS=23
TRADES=117
ORDERS=939
SNAPSHOTS=283
STRATEGY_EVENTS=13868
BROKER_ORDER_EVENTS=939
MIGRATIONS=44 successful / 0 failed / latest 20260823020000
```

The running backend binary contains the new global-kill Clear Trades gate, and the running frontend bundle contains the updated broker-flat/global-kill confirmation warning.

```text
ADMIN CLEAR TRADES SAFETY DEPLOYMENT: PASS
RUNNING RELEASE: dfa5316ba210510fc3c176f2ba63cffa252e097c
FORCE_DEMO_TRADING: FALSE
GLOBAL KILL SWITCH: ENABLED
OPEN LIVE BROKER EXPOSURE: ZERO
NONTERMINAL LIVE BROKER ORDERS: ZERO
UNEXPECTED LIVE BROKER ACTIVITY: NO
POSTGRES TLS VERIFY-FULL: PASS — TLSv1.3
POSTGRES HEALTH: PASS
BACKEND HEALTH: PASS
FRONTEND HEALTH: PASS
PUBLIC READINESS: PASS
BACKEND RESTART COUNT: 0
PANIC/FATAL COUNT: 0
```
