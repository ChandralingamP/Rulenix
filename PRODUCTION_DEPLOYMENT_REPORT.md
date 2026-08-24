# Production Deployment Report

## 1. Deployment Date/Time

2026-08-24 08:58:40 +05:30 (IST).

## 2. Git Commit Deployed

No commit was deployed. The verified remediation was present as an uncommitted working-tree release candidate; a local release commit was prepared after this report was written, but no production target was contacted.

## 3. Build/Test Results

- `cargo fmt -- --check`: PASS
- `cargo check`: PASS
- `cargo test`: PASS — 120 passed, 0 failed, 15 database-backed tests correctly ignored because no isolated `TEST_DATABASE_URL` was configured.
- `cargo clippy --tests -- -D warnings`: PASS
- `npm test -- --run`: PASS — 6 files, 18 tests.
- `npm run build`: PASS
- `npm run lint`: PASS

## 4. Database Migration Results

NOT RUN against production. No production database connection or migration mechanism could be accessed safely. The pending remediation migrations were reviewed locally and the remediation report records prior disposable-PostgreSQL coverage.

## 5. Production Server/Service

The documented topology is Docker Compose project `rulenix` in `/opt/rulenix`, running `postgres`, `backend`, and `frontend`. No current production host, secure deployment connection, production environment reference, or secret mount was available in this workspace. No credentials are included in this report.

## 6. Services Started

None. No production service was stopped, started, restarted, or reconfigured.

## 7. Production Health Checks

NOT RUN. No production target was contacted.

## 8. Trading Enabled?

NO. No production configuration was changed. Deployment was stopped because the actual production configuration could not be verified to set `FORCE_DEMO_TRADING=true`; the example production template defaults it to `false`.

## 9. Live Orders Generated?

NO. No broker connection, order, cancellation, modification, stop, target, or close action was performed.

## 10. Broker Connection Status

NOT CHECKED. There was no safe, established production connection method available locally, and no broker action was attempted.

## 11. Errors/Warnings

- Deployment blocked: no configured SSH/deployment connection or production-server reference was available.
- Deployment blocked: `backend/.env.production`, the PostgreSQL password secret reference, and the PostgreSQL CA reference were absent locally.
- Deployment blocked: live trading could not be proven disabled because the actual production `FORCE_DEMO_TRADING` value was inaccessible.
- No safety-critical local validation failed.

## 12. Rollback Information

No deployment occurred; rollback is not applicable. The documented production procedure requires forward recovery or a verified database restore when migrations are not backward-compatible.

## 13. Final Deployment Status

```text
CODE DEPLOYED: NO
EXPECTED COMMIT RUNNING: NO
DATABASE MIGRATIONS: NOT RUN
SERVICE HEALTH: NOT RUN
LIVE TRADING ENABLED: NO
LIVE ORDERS GENERATED: NO
READY FOR BROKER VERIFICATION: NO
```

Deployment remains blocked pending an established secure connection to the documented production target and a non-secret confirmation that the deployed configuration sets `FORCE_DEMO_TRADING=true`.
