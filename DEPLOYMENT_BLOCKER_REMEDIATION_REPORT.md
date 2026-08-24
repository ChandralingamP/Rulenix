# Deployment Blocker Remediation Report

Scope: development workspace only. No production host, database, configuration, service, or broker was contacted or changed during this remediation. No Angel One request or order action was made.

The previously verified release commit `8030a42682b471c02dad47ab162050b6d15a0c7e` and archive SHA-256 `69dd91804427bdc09450209c577b1434adddffca5624e33c136d00b8ee759e64` predate this remediation. A new reviewed commit and release archive must be created before another deployment attempt.

## 1. Destructive Migration Analysis

The former `backend/migrations/20260823000000_remove_margin_and_option_entry.sql` mixed removal from active execution with destruction of historical records. The executable feature had already been removed, so none of these data mutations were required to deactivate it.

| Former statement | Direct impact | Relationship/cascade impact | Active application need | Historical dependency and disposition |
|---|---|---|---|---|
| Delete `strategy_execution_intents` for `option_entry_v1` | Destroyed durable per-user execution history | Removed links to signals, users, snapshots, trades, and orders | No active Option Entry routing depends on these rows | IDs, attempts, states, and order links are audit history; deletion removed |
| Delete `strategy_signals` for `option_entry_v1` | Destroyed source-signal history | `strategy_execution_intents.signal_id` uses `ON DELETE CASCADE`, so any remaining child intents would also be deleted | No active Option Entry routing depends on these rows | Signal time, payload, and fan-out history retained |
| Set `strategy_orders.risk_decision_id` to `NULL` | Broke the order-to-risk-decision audit link | Worked around the non-cascading risk-decision FK solely to enable later deletion | Active queries do not require the unlink | Original decision relationship retained |
| Delete matching `strategy_orders`, then `risk_decisions` | Destroyed orders, fills, client IDs, broker IDs, and decisions | `broker_order_events.order_id` is `ON DELETE CASCADE`; execution intent order links are `ON DELETE SET NULL`; order-to-trade history was lost | Active Option Entry execution does not need these rows | Orders, fill watermarks, broker events/payloads, and risk JSON retained |
| Delete `strategy_reversal_intents` | Destroyed historical reversal lifecycle | Linked to trade/user/snapshot; source-trade deletion also has cascade behavior | No active Option Entry routing depends on these rows | Reversal identity and state retained |
| Delete `trades` for `option_entry_v1` | Destroyed position and P&L history | Order `trade_id` and execution-intent `trade_id` use `ON DELETE SET NULL`; reversal intents can cascade from source trade | Active execution does not need legacy trades | IDs, timestamps, broker entry/exit references, quantity, prices, and P&L retained |
| Delete `strategy_market_snapshots` | Destroyed the market/contract context for trades and orders | Orders and trades have snapshot FKs; signals/intents have `ON DELETE SET NULL`; prior deletes were arranged to bypass these relationships | No active Option Entry snapshot creation exists | Contract, session, token, price, and level context retained |
| Delete `strategy_events` | Destroyed operational history | User deletion can cascade, but no cascade was needed here | Not used to run the retired strategy | Event type, payload, and timestamps retained |
| Delete `strategy_scheduler_runs` | Destroyed scheduler attempt history | No dependent runtime row required its removal | Not used to schedule the retired strategy | Attempt/status timing retained |
| Delete `user_strategy_configs` and `user_strategy_activations` | Destroyed historical user configuration and activation state | Both are user-owned and may cascade only when a user is deleted | No executable Option Entry catalog/router entry consumes them | Original user choices and active/inactive history retained |
| Delete `backtest_runs` | Destroyed Option Entry backtest summaries | `backtest_trades.run_id` is `ON DELETE CASCADE`, destroying trade results too | No current execution path depends on old backtests | Runs, child trades, summary, and P&L retained |
| Drop `backtest_option_contracts` | Destroyed historical contract snapshots | Table-owned indexes and constraints would be removed with the table | No current executable code reads it | Required for reproducibility and historical context; table retained |
| Drop `broker_margin_estimates` | Destroyed broker response/cache history | Table indexes, uniqueness constraints, user reference, and raw responses would be removed | Current funds validation calls Angel One margin and RMS endpoints directly; it does not read this cache | Historical estimates and raw responses retained |
| Drop `strategy_orders.margin_required` and `trades.margin_required` | Destroyed per-order/per-trade recorded estimates | Removed values from otherwise preserved audit rows | No executable source reads these columns | Financial history retained in place |
| Drop `user_profiles.demo_balance` | Destroyed the historical simulated account balance | Earlier accounting migrations used this value | No current executable source reads it | Historical user/account state retained |
| Drop `risk_limits.margin_requirement_percent` | Destroyed the historical risk input | Historical risk-decision JSON may record the same policy | Current live margin validation uses broker required margin, typed RMS `availablecash`, and a configured safety buffer | Historical policy retained |
| Rewrite `risk_decisions.values` | Removed margin inputs and health evidence from immutable decision history | Did not delete rows but changed their original meaning | Current decisions do not require old JSON to be rewritten | Rewrite removed; original JSON retained |
| Rewrite `backtest_runs.summary` | Removed initial/max/buy/sell/calculator margin fields | Changed historical backtest output | Current execution does not require old summaries to be rewritten | Rewrite removed; original summary retained |

The former migration did not explicitly create or drop triggers. Its table drops would implicitly remove table-owned indexes and constraints. It used no `DROP ... CASCADE`, but existing FK actions caused additional loss or broken links as described above.

Captured production preflight evidence showed the migration would have removed 4 trades, 52 orders, 28 snapshots, 1,042 strategy events, 3 configurations, 3 activations, and 32 margin-estimate rows. That evidence was not re-queried during this local-only task.

## 2. Historical Data Preservation Design

The design separates runtime retirement from data retention:

- `option_entry_v1` remains absent from executable strategy routing and catalog behavior.
- Existing legacy rows stay in their original normalized tables; there is no copy/archive step that could change IDs or omit relationships.
- Existing foreign keys, indexes, constraints, timestamps, client IDs, broker IDs, fill events, P&L, risk JSON, and backtest summaries remain intact.
- Legacy margin tables and columns remain compatibility/history storage. PostgreSQL comments identify that they are not active execution inputs.
- Current live margin validation remains the Angel One margin endpoint plus typed RMS `availablecash` and the configured safety buffer.
- No historical trading value is rewritten, normalized, or marked with a new state.

Keeping the data in place is safer than introducing `*_legacy` copies because it avoids a second identity map, prevents broken foreign keys, and preserves existing application history queries.

## 3. Migration Changes

`20260823000000_remove_margin_and_option_entry.sql` now contains no `DELETE`, destructive `UPDATE`, `DROP TABLE`, or `DROP COLUMN` operation.

It only applies descriptive `COMMENT ON` metadata to:

- `broker_margin_estimates`
- `backtest_option_contracts`
- `strategy_orders.margin_required`
- `trades.margin_required`
- `user_profiles.demo_balance`
- `risk_limits.margin_requirement_percent`

The migration filename and sequence are unchanged because it has not been applied to production. The migration checksum will intentionally differ from commit `8030a426...`; a new release must be built and its migration checksums re-verified before deployment.

No Futures formula, missed-entry rule, opening-range recovery, SuperTrend rule, SL/TP behavior, or P0 execution-safety runtime code was changed. The only Rust change is test infrastructure and the preservation test under `cfg(test)`.

## 4. Before/After Legacy Data Counts

`removed_legacy_features_preserve_active_and_inactive_history` creates both active and inactive users/configurations, an open and a closed legacy trade, filled broker orders, broker fill events, risk decisions, snapshots, strategy events, signals/intents, a reversal intent, a backtest and child trade, an option contract snapshot, a margin estimate, and an audit event.

| Preserved category | Before | After |
|---|---:|---:|
| Fixture users | 2 | 2 |
| User profiles with historical demo balances | 2 | 2 |
| Option Entry configurations | 2 | 2 |
| Option Entry activations | 2 | 2 |
| Historical snapshots | 2 | 2 |
| Historical trades | 2 | 2 |
| Historical orders | 2 | 2 |
| Broker fill/order events | 2 | 2 |
| Strategy events | 2 | 2 |
| Durable signals | 2 | 2 |
| Execution intents | 2 | 2 |
| Reversal intents | 1 | 1 |
| Backtest runs | 1 | 1 |
| Backtest trades | 1 | 1 |
| Option contract snapshots | 1 | 1 |
| Broker margin estimates | 1 | 1 |
| Audit events | 1 | 1 |

The test also verifies two order→trade→snapshot joins, two broker-event→order joins, two intent→signal→order joins, and two order→risk-decision joins. Broker entry/exit IDs, broker order IDs, recorded margin, and realized P&L are asserted unchanged.

## 5. Migration Tests

Three database shapes are covered:

1. Clean database: the full SQLx migration set applies successfully.
2. Representative safety-legacy database: active trade/order states are migrated; unsafe duplicate exit coverage fails closed until explicitly reconciled, after which history is retained and migration succeeds.
3. Active/inactive Option Entry and margin legacy database: the retirement migration preserves every seeded category and relationship, then both P0 migrations apply and historical trade/order/snapshot queries still return both rows.

Results on isolated PostgreSQL 18 over TLS verify-full:

| Test/gate | Result |
|---|---|
| `removed_legacy_features_preserve_active_and_inactive_history` | PASS |
| `migrations_pass_clean_and_require_explicit_legacy_exit_reconciliation` | PASS |
| All stateful PostgreSQL/fake-broker tests, serial | PASS — 16 passed, 0 failed |
| `cargo fmt -- --check` | PASS |
| `cargo check --tests` | PASS |
| `cargo test` | PASS — 120 passed, 0 failed, 16 intentionally ignored stateful tests |
| `cargo clippy --tests -- -D warnings` | PASS |

Frontend files and behavior were not changed, so frontend gates were not rerun for this migration/TLS-only remediation.

## 6. Application Compatibility

Executable-source review found no `option_entry_v1`, `broker_margin_estimates`, `backtest_option_contracts`, `demo_balance`, or persisted `margin_required` reference in `backend/src` outside tests. Therefore, retaining these schema objects does not re-enable the retired strategy.

The remaining `angel::margin_required` call is the current broker-side entry safety check. It must not be confused with the retired persisted margin cache: the runtime obtains required margin from Angel One, obtains typed RMS funds, applies the safety buffer, and fails closed when funds data is invalid or insufficient.

The actual backend executable was started in staging mode against a fresh isolated TLS database. It applied 44 SQLx migrations, bound only to local loopback, and returned:

```text
GET /api/health/ready
{"checks":{"database":"ok"},"status":"ready"}
```

The local test process was then stopped. Test startup contained no users or broker credentials; no Angel One connection or order operation occurred.

## 7. PostgreSQL TLS Target Architecture

The backend connects to the Compose service hostname `postgres`. The server certificate must therefore contain `DNS:postgres` in Subject Alternative Name; CN alone is not sufficient as the long-term design.

Exact target:

```text
private CA
  -> signs PostgreSQL server certificate
       SAN: DNS:postgres
       EKU: serverAuth

postgres container
  /var/lib/postgresql/data/server.crt  owner postgres:postgres, mode 0644
  /var/lib/postgresql/data/server.key  owner postgres:postgres, mode 0600
  ssl = on
  ssl_cert_file = 'server.crt'
  ssl_key_file = 'server.key'

backend container
  /run/secrets/postgres_ca.crt         read-only CA certificate
  DATABASE_URL=postgresql://rulenix:<URL-encoded-password>@postgres:5432/rulenix?sslmode=verify-full&sslrootcert=/run/secrets/postgres_ca.crt
```

CA private-key requirements:

- Generate and retain the CA private key outside the application release and containers.
- Root-owned mode `0600`; never mount the CA private key into backend or PostgreSQL.
- Deploy only the CA certificate to `./secrets/postgres_ca.crt` for the existing backend read-only mount.
- Generate a unique server private key and certificate; server certificate validity and renewal dates must be monitored.

PostgreSQL/Compose requirements:

- The existing named `postgres_data` volume can hold the server certificate and key after a controlled root copy, ownership change, and permission check. The existing captured provisioning draft follows this pattern.
- Alternatively, future Compose changes may mount certificate source files into a root-only staging path, but startup must copy them into the data directory with the exact PostgreSQL ownership/modes; a world-readable Docker secret is not acceptable for `server.key`.
- Add a `pg_hba.conf` `hostssl` rule limited to the application database/user and the actual `rulenix_default` subnet, using `scram-sha-256`.
- Ensure there is no broader non-SSL `host` rule that allows the backend network to bypass TLS. A local socket rule may remain for container-local administration/health checks.
- `ssl_ca_file` is not required unless PostgreSQL is also configured to authenticate client certificates. Server certificate verification by the backend requires its `sslrootcert` CA mount.

Controlled restart and validation order:

1. Reconfirm no open live liabilities and take a verified PostgreSQL backup plus before-count manifest.
2. Stage CA certificate, server certificate, and server key; verify SAN, chain, expiry, ownership, and modes.
3. Install server certificate/key into the PostgreSQL data volume and set `ssl=on`, `ssl_cert_file`, and `ssl_key_file`; install restrictive `hostssl` policy.
4. Restart PostgreSQL only during the approved maintenance window.
5. Require `SHOW ssl = on`, `pg_isready`, a `verify-full` query from the Compose network using hostname `postgres`, and `pg_stat_ssl.ssl = true`.
6. Set the backend URL to the exact verify-full form and verify the CA mount inside the backend container.
7. Start the candidate backend, allow SQLx migrations under its advisory lock, and require `/api/health/ready` plus migration/checksum and after-count validation.
8. Keep `FORCE_DEMO_TRADING=true`; start frontend only after backend readiness. Do not connect the broker or enable live trading as part of TLS/migration cutover.

## 8. TLS Test Results

Docker Desktop was unavailable on the development machine, so no claim is made for a Docker-engine test. Instead, an isolated PostgreSQL 18 cluster was initialized under the ignored local run-log directory, bound to `127.0.0.1:55432`, and configured with a test-only CA and server certificate containing `DNS:localhost,DNS:postgres`. No production certificate was used.

| Verification | Result |
|---|---|
| Correct CA + matching `localhost` hostname + `sslmode=verify-full` | PASS — TLS 1.3 and `pg_stat_ssl.ssl=true` |
| Wrong CA | PASS — connection rejected |
| Wrong hostname (`127.0.0.1` with no IP SAN) | PASS — connection rejected |
| Missing CA file | PASS — connection rejected |
| SQLx migrations over verify-full | PASS — 44 migrations recorded |
| Backend application startup over verify-full | PASS — readiness database check `ok` |
| Stateful test suite over verify-full | PASS — 16/16 |

This proves the application/SQLx trust-chain and hostname behavior locally. It does not replace validation of the actual production Docker network, files, permissions, PostgreSQL 16 image, or operational restart.

## 9. Production Changes That Will Be Required Later

No item in this section has been applied.

1. Review and commit this remediation; run the full release gates again from the clean commit and generate a new release archive/checksum. Do not deploy the old `8030a426...` archive.
2. Schedule a PostgreSQL maintenance window and obtain explicit deployment authorization.
3. Take and verify a production backup and row-count manifest.
4. Generate/stage the production CA certificate and `DNS:postgres` server certificate/key under the approved secret-management process.
5. Copy the server certificate/key into the PostgreSQL data volume with owner `postgres:postgres`, certificate mode `0644`, and key mode `0600`.
6. Enable PostgreSQL SSL and the restrictive `hostssl`/SCRAM rule, restart PostgreSQL, and prove verify-full from `rulenix_default` before touching the backend.
7. Mount the CA certificate at `/run/secrets/postgres_ca.crt:ro` (already declared in Compose) and set the production `DATABASE_URL` to hostname `postgres`, `sslmode=verify-full`, and the mounted root-cert path.
8. Deploy the newly built candidate with demo-only enforcement, run preflight/migrations, and compare every historical count and broker reference before cutover.

## 10. Rollback Plan

Migration rollback:

- The redesigned retirement migration changes comments only. Rolling back the application requires no deletion, reverse data migration, or historical reconstruction.
- If a candidate fails before migration completion, stop the candidate and restore the previous application revision; retain the database and inspect `_sqlx_migrations` and logs.
- If any count/reference mismatch appears, stop immediately and restore into a separate database from the verified backup for investigation. Never “repair” by deleting legacy rows.

TLS rollback:

1. Keep the previous PostgreSQL configuration and backend environment as protected rollback artifacts before change.
2. If PostgreSQL cannot start, restore the prior PostgreSQL SSL configuration and `pg_hba.conf`, restore certificate files/permissions as needed, and restart PostgreSQL before any backend restart.
3. If PostgreSQL TLS succeeds but backend verify-full fails, keep the backend stopped, diagnose SAN/CA/path/permission errors, and do not weaken `sslmode`.
4. Only if the approved maintenance rollback explicitly requires returning to the former non-TLS service may the prior database configuration be restored; this returns the deployment to blocked status and must not be treated as release completion.
5. After either path, verify database counts, referential integrity, old-backend readiness, demo-only mode, and no live liabilities.

## 11. Remaining Risks

1. Production PostgreSQL SSL is still off and verify-full is not active; this was intentionally not changed.
2. The local TLS test used PostgreSQL 18 on Windows rather than the production PostgreSQL 16 Alpine container because Docker Desktop was unavailable. The production Compose-network test remains mandatory.
3. The ignored operational TLS scripts are captured drafts, not a committed release interface. They require review against the actual container name, Compose subnet, certificate policy, and maintenance procedure.
4. Certificate rotation, expiry monitoring, backup restoration, and the final `pg_hba.conf` policy still require operational verification.
5. The working tree contains the earlier intentional `PRODUCTION_DEPLOYMENT_REPORT.md` update plus this remediation. A clean, reviewed commit and new archive/checksum are required.
6. The production-like preservation fixture proves categories and relationships rather than reproducing all 1,131 observed Option Entry rows. Production before/after counts and FK checks remain required during the approved migration window.
7. This work does not change the broker-sandbox gate or authorize live trading.

## 12. Deployment Readiness

The destructive migration blocker is fixed and tested locally. The verify-full architecture and application behavior are tested locally. The workspace is ready to be turned into a new verified release candidate and used for a controlled second deployment attempt, during which production TLS must be enabled and validated before backend migration/cutover.

“Ready” below means ready to begin that controlled attempt after explicit authorization; it does not mean production is deployed, TLS is already active there, or live trading is approved.

DESTRUCTIVE MIGRATION FIXED: YES

HISTORICAL USER DATA PRESERVED IN TESTS: YES

HISTORICAL TRADES PRESERVED IN TESTS: YES

HISTORICAL ORDERS PRESERVED IN TESTS: YES

HISTORICAL SNAPSHOTS PRESERVED IN TESTS: YES

HISTORICAL STRATEGY EVENTS PRESERVED IN TESTS: YES

POSTGRESQL TLS DESIGN COMPLETE: YES

VERIFY-FULL TESTED LOCALLY: YES

PRODUCTION TLS CHANGES APPLIED: NO

PRODUCTION DEPLOYED: NO

READY FOR SECOND DEPLOYMENT ATTEMPT: YES
