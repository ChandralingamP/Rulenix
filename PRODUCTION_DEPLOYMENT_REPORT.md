# Production Deployment Report

## Deployment

- Timestamp: 2026-08-24 19:21:25 +05:30 (IST)
- Release commit: `8030a42682b471c02dad47ab162050b6d15a0c7e`
- Archive name: `rulenix-release-8030a42.tar.gz`
- SHA-256: `69dd91804427bdc09450209c577b1434adddffca5624e33c136d00b8ee759e64`
- Previous production version: UNKNOWN — the current release directory has no `RELEASE_COMMIT` marker or Git metadata.
- Deployment outcome: STOPPED BEFORE CUTOVER

The release archive was produced directly from the authorized Git commit. It contains 196 tracked entries and no `.env`, production environment, secret directory, private key, certificate, or credential file.

## Preflight

- Local HEAD: PASS — exact authorized commit.
- Local working tree before packaging: PASS — clean, with no untracked release files.
- SSH verification: PASS — strict established known-host verification and key authentication succeeded as `deploy`.
- Server identity: PASS — expected production host and `/opt/rulenix` deployment layout.
- Environment verification: PASS — `/opt/rulenix/backend/.env.production` exists.
- Secret-reference verification: PASS — the PostgreSQL password and CA reference files exist; contents were not read.
- CA file presence: PASS — the configured CA reference is non-empty.
- Database TLS verification: FAIL — the running backend's database URL has neither the required `sslmode=verify-full` nor the configured CA reference, and PostgreSQL reports SSL disabled.
- Migration metadata verification: PASS for the 39 applied migrations — every applied checksum matches the exact release and no applied migration file is missing.
- Pending migrations: five.

### Pending migration safety review

| Migration | Operations | Data-safety result |
|---|---|---|
| `20260724025000_strategy_snapshot_execution_metadata.sql` | Adds snapshot metadata columns and normalizes empty execution keys | Preserves rows |
| `20260819000000_durable_signal_fanout.sql` | Creates durable signal and execution-intent tables/indexes | Preserves rows |
| `20260823000000_remove_margin_and_option_entry.sql` | Deletes Option Entry orders, trades, snapshots, signals, events, configurations, activations, and associated risk/reversal/backtest records; drops tables and columns | **MANDATORY STOP — destructive to existing historical and user configuration data** |
| `20260823010000_execution_safety_lifecycle.sql` | Adds safety columns, backfills safety state, adds constraints, preflight, triggers, and incident table | Preserves rows; intentionally fails on unsafe active exit coverage |
| `20260823020000_p0_execution_safety.sql` | Adds execution metadata and transition constraints/triggers | Preserves rows |

Production contains records that the destructive pending migration would remove:

- Option Entry trades: 4
- Option Entry orders: 52
- Option Entry snapshots: 28
- Option Entry strategy events: 1,042
- Option Entry user configurations: 3
- Option Entry activations: 3
- Margin-estimate records: 32

Under the deployment authorization, these findings require stopping before migration and cutover. The migration was not applied or altered.

## Data Preservation

| Aggregate | Before | After stopped preflight |
|---|---:|---:|
| Users | 4 | 4 |
| Trades | 117 | 117 |
| Orders | 938 | 938 |
| Filled-order records | 315 | 315 |
| Broker order events | 938 | 938 |
| Audit records | 596 | 596 |
| Risk decisions | 1,872 | 1,872 |
| Strategy events | 13,866 | 13,866 |
| Backtest trades | 14 | 14 |

No production database mutation was performed. User and historical trading counts did not decrease.

## Safety

- Open live trades before cutover: 0
- Nonterminal live orders before cutover: 0
- Open live trades after stopped preflight: 0
- Nonterminal live orders after stopped preflight: 0
- `FORCE_DEMO_TRADING`: TRUE
- Global kill switch: DISABLED
- Active strategy activations: 6
- New-entry shutdown requirement: NOT ESTABLISHED because the global kill switch is disabled.

The global kill switch was not changed because a mandatory migration/TLS stop condition had already been reached. `FORCE_DEMO_TRADING=true` continues to block live entries, but automatic demo strategy execution remains possible.

## Backup

- Backup result: NOT RUN
- Backup verification: NOT RUN
- Restore procedure availability: PASS — established encrypted backup/restore scripts exist.

Backup was intentionally not started because deployment had already stopped during read-only preflight, before any production mutation.

## Build

- Local verified Rust/frontend validation: PASS as recorded by the remediation release.
- Production backend build: NOT RUN
- Production frontend build: NOT RUN

The release archive was not uploaded and no server-side release directory was created.

## Database

- Production database connection: PASS through the existing local container path.
- Applied migration checksum comparison: PASS — 39 matched, zero mismatches.
- Pending migration count: 5
- Migration application: NOT RUN
- Migration result: FAIL — blocked by destructive historical-data operations and failed TLS preflight.

No `_sqlx_migrations` record, schema object, user record, trading record, or configuration record was changed.

## Runtime

These results describe the unchanged pre-existing production release:

- Backend health: PASS — healthy
- Frontend health: PASS — running
- PostgreSQL container health: PASS — healthy
- Internal readiness: PASS
- Public readiness: PASS
- New release running: NO
- Service stop/restart: NOT PERFORMED

## Broker Activity

- Angel One authentication/test: NOT PERFORMED
- Unexpected live orders: NO
- Unexpected cancellations: NO
- Unexpected broker mutations: NO
- Live orders created during the preflight window: 0
- Live orders updated during the preflight window: 0

## Rollback

- Rollback required: NO
- Rollback result: NOT APPLICABLE

No archive was transferred, no production file was replaced, no service was stopped, and no migration ran.

## Final Status

CODE DEPLOYED: NO

EXPECTED COMMIT RUNNING: NO

RELEASE CHECKSUM: PASS

PREDEPLOYMENT BACKUP: FAIL

DATABASE MIGRATIONS: FAIL

USER DATA PRESERVED: PASS

HISTORICAL TRADING DATA PRESERVED: PASS

SERVICE HEALTH: PASS

FORCE_DEMO_TRADING: TRUE

NEW ENTRY GATE: DISABLED

LIVE TRADING ENABLED: NO

UNEXPECTED LIVE ORDERS GENERATED: NO

READY FOR ANGEL ONE VERIFICATION: NO
