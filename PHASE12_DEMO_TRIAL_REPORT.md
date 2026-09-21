# Phase 12 Python DEMO trial and cutover preflight

## Outcome

Phase 12 implementation and DEMO lifecycle parity passed, but the final production gate is
blocked. A fresh post-trial authoritative broker read found two broker order records whose states
remain `AMBIGUOUS`. They are not counted as exposure-capable orders, but `UNKNOWN != SAFE`; no
further Phase 12 deployment was performed and no broker state was mutated.

Rust remains the only LIVE and public authority. Phase 13 was not started.

## Releases and reproducibility

```text
RUST PRODUCTION SHA: ab14c3ca05ce91ad959b29ace66f623936cd4f76
PYTHON STARTING SHA: d2066618e7f37fc2fcc20ef176d085ef3a0dd5c2
PHASE 11 SHADOW SHA: 743a99887b9f147241fcd1430306954d935865c2
PHASE 12 DEPLOYED DEMO SHA: 0398e66ac9e82b6198661092218691000946065d
PHASE 12 FINAL EXECUTABLE SOURCE SHA: 4d114907ba9fc81094b5199187262cc2ebe42975
```

The Phase 11 deployed SHA differs from the starting source SHA only by the two committed Markdown
reports `PHASE11_SHADOW_DEPLOYMENT_REPORT.md` and `PYTHON_MIGRATION_MASTER.md`. There is no Python,
dependency, migration, container, or runtime difference, so that initial difference is expected.

The Phase 12 deployed SHA is reproducible from its Git archive. The final executable source adds
only a distinct cumulative `poll_errors` telemetry field and its unit test. It was not redeployed
after the final gate blocked on ambiguous broker evidence.

## Ownership and duplicate prevention

The trial uses only the synthetic accounts `phase12-demo-a` and `phase12-demo-b` in the isolated
`rulenix_demo_trial` schema. Rust never reads that schema, so it cannot schedule or execute these
cycles. Python is denied every write to `public`, reads only explicitly granted production columns,
and can write only the trial schema. A PostgreSQL advisory lock provides one leader. Release-scoped
unique cycle keys, stable intent/order idempotency keys, unique cycle signals, and unique
cycle/lineage trades prevent duplicates across retries and restarts while forcing fresh evidence
for each code release.

The Docker image contains only `app.demo_trial` and its dependencies. It contains no API, broker,
credentials, egress, reconciliation, or trading package; receives no Angel credentials; has no
published port; and is attached only to an internal network with no external gateway.

## Production DEMO evidence

Release `0398e66` recorded 22 completed comparisons, 22 matches, zero mismatches, zero unresolved,
and zero lifecycle errors:

- REAL PRODUCTION DEMO OBSERVATION: two closed-session observations from production clock,
  calendar, and signal state. Futures correctly recorded no signal. SuperTrend correctly recorded
  the observed `SQUARE_OFF` signal with `expected_users=0` as `EOD_NO_POSITION`, with one observed
  signal and no intent, order, trade, position, exit price, or P&L mutation.
- DETERMINISTIC PRODUCTION-DERIVED REPLAY: 20 comparisons across two isolated accounts. Futures
  matched 12/12; SuperTrend matched 8/8.
- Persisted children for the release: 14 signals, 32 intents, 58 orders, 16 trades, and 22 terminal
  events. Six account/strategy assignments remained isolated.
- Futures target, SL1, SL2, opposite-side SL2 reversal, quantity, target/protection state, terminal
  reason, exit semantics, and P&L matched the Rust oracle exactly.
- SuperTrend target, stop, and 15:10 EOD semantics matched exactly.
- Manual DEMO Close, global Kill Switch denial, no-signal, and two-account isolation matched.

The first immutable production observation under `b7fdf84` incorrectly labeled the SuperTrend
`SQUARE_OFF` record as no-signal. It is retained as diagnostic history and excluded from the final
release result. The corrected release used a distinct observation key and preserved exact evidence.

## Restart and failure evidence

Before and after a deliberate restart, release counts were identical:
`22 cycles / 14 signals / 32 intents / 58 orders / 16 trades / 22 events`. The service reacquired
the advisory lock, returned leader/ready, and produced no duplicate state.

A controlled 22-second pause of the trial-only database proxy caused health to fail closed with
HTTP 503, `TimeoutError`, and `stale=true`. After the proxy resumed, the service recovered to HTTP
200, leader/ready, and `stale=false`; every lifecycle count remained identical. Local PostgreSQL
tests also prove stale-claim recovery, exclusive leadership/reacquisition, source read-only
enforcement, sink isolation, idempotent completion, reversal lineage, and account separation.

## Validation

```text
FOCUSED PHASE 12 TESTS: PASS - 10 passed
POSTGRESQL PHASE 12 TEST: PASS - 1 passed against disposable local PostgreSQL
PHASE 10 / COMPLETE PYTHON AUDIT: PASS - 199 passed, 3 skipped, 2 dependency warnings
HTTP DIFFERENTIALS: PASS - 39
WEBSOCKET DIFFERENTIALS: PASS - 2
FUTURES DIFFERENTIALS: PASS - 22
SUPERTREND DIFFERENTIALS: PASS - 6
OCO DIFFERENTIALS: PASS - 12
DATABASE-STATE DIFFERENTIALS: PASS - 8
EOD DIFFERENTIALS: PASS - 4
CRASH/RESTART DIFFERENTIALS: PASS - 5
RUFF: PASS
MYPY: PASS - 87 source files
FRONTEND: NOT CHANGED; deployed root and readiness both HTTP 200
```

One unconfigured full-suite attempt produced an environment-only WebSocket fixture failure because
the Rust adapter requires `TEST_DATABASE_URL`; the authoritative audit script then reran the suite
with its guarded disposable database and passed. Ruff's repo-wide pre-existing Alembic import-order
finding is outside the changed scope; the authoritative `app tests` run passed.

## Production health and safety

```text
RUST PRODUCTION HEALTH: ready; container healthy; restart count 0
RUST SCHEDULER LEADER: true
RUST SCHEDULER ADVANCING: yes; dispatch_count advanced 272557 -> 471114
RUST LAST ADVANCEMENT / DISPATCH: 2026-09-21T08:19:01Z at final sample
RUST WORKER ERRORS: 41 cumulative
PYTHON DEMO HEALTH: ready; leader=true; stale=false; 22/22 matches
POSTGRESQL HEALTH: healthy; restart count 0
FRONTEND HEALTH: running; restart count 0
PUBLIC READINESS: HTTPS 200
GLOBAL KILL SWITCH: disabled
```

The 41 Rust errors are recovered PostgreSQL deadlocks in SuperTrend mandatory square-off and
execution-intent recovery. Rust continues advancing, but a clean observation window after a
separate remediation is required before Phase 13.

The pre-trial deployment gate passed under the established deployment-vs-LIVE-readiness policy:
four accounts were locally flat, no durable blockers existed, and no broker failure was treated as
flat. All four broker reads were unavailable due to missing current token/API material, and every
account remained LIVE-ready false.

The fresh post-trial gate checked four accounts. It found zero broker positions, zero
exposure-capable orders, zero active conditional rules, zero Rulenix-owned exposure, and zero
manual-external exposure. Two accounts were readable; two remained offline and LIVE-ready false.
One readable account contained two order records classified `AMBIGUOUS` because their broker states
were unknown. The gate therefore returned `BLOCK`. No attempt was made to cancel, modify, close, or
otherwise resolve them.

```text
PYTHON ANGEL MUTATION TRANSPORT PATHS: 0
PYTHON ANGEL MUTATION HTTP REQUESTS: 0
PYTHON LIVE ORDERS PLACED: 0
PYTHON LIVE ORDERS MODIFIED: 0
PYTHON LIVE ORDERS CANCELLED: 0
PYTHON LIVE POSITIONS CLOSED: 0
MANUAL BROKER ACTIVITY MODIFIED: NO
```

## Backup and permission boundary

The fresh encrypted backup is
`/var/backups/rulenix/rulenix-phase12-ade9a2d-20260920T150353Z.dump.enc`, size 12,209,856 bytes,
SHA-256 `d1cae7d9bf06fedc2f7e71e16b6bd9919677d187388f3d658f8103dd289e3848`.
Its disposable restore passed with four users and 51 migrations.

The exact deployed image proved that the reader cannot insert into production and that the writer
cannot insert/update/delete production tables but can write the isolated trial schema. Production
role passwords were rotated without disclosure and remain file-backed mode 0400 for UID/GID 10003.

## Phase 13 preflight

Python is not LIVE-cutover-ready. `AngelRestClient` implements reads only. `AngelClient` blocks
place, cancel, and manual close before transport and has no modify or GTT mutator.
`ExecutionOrchestrator` builds typed requests but returns `LIVE_MUTATION_DISABLED_DURING_MIGRATION`
and never sends them. Reconciliation remains read-only.

Phase 13 must implement and certify typed place/cancel/modify/GTT operations; risk-reducing manual
close and protection management; cumulative and partial fills; OCO siblings; SL2 reversal; EOD;
stable tags and idempotency; ambiguous-write reconciliation; restart-safe retries; per-account
source-IP binding; and a final database/broker risk and collision check immediately before every
mutation. It also requires a fenced authority lease/epoch, broker sandbox and fault testing, and a
reviewed mutation allowlist.

The future transfer sequence is: resolve blockers, backup/restore proof, freeze control-plane
changes, stop new entries while preserving exits, drain and fence Rust workers, reconcile fresh
broker truth, prove schema compatibility, start Python read/reconcile-only, transfer the fenced
authority epoch, enable one reviewed canary, verify, then route public traffic. It must never run
two broker mutators.

Rollback keeps Rust SHA `ab14c3c` and compatible schema available. Python entries and mutation must
first be disabled, its workers drained and fenced, all in-flight broker actions reconciled, and its
authority lease relinquished before Rust resumes. Any parity mismatch, stale scheduler/feed,
worker error, ambiguous submission, protection failure, reconciliation disagreement, cross-account
leak, or readiness failure triggers rollback review.

Phase 13 blockers are: two ambiguous broker order states; two offline/not-LIVE-ready accounts; no
Python LIVE mutation implementation; no fenced authority transfer; no broker sandbox certification;
41 recovered Rust worker deadlocks without a clean post-remediation window; Rust
`RUSTSEC-2026-0285` (`rustls 0.23.43`, fixed in 0.23.45+); unfixed `RUSTSEC-2023-0071` in `rsa`;
yanked `chacha20 0.10.1`; and seven frontend development-tool advisories (production dependency
audit remains zero).

## Scope

Executable Phase 12 scope before this report: 16 new files, 1,596 added lines, zero deleted lines.
There was no line-ending normalization, generated-file churn, dependency change, frontend change,
Rust behavior change, or unrelated semantic change. The user's pre-existing edits to the production
broker-safety scripts and the untracked diagnostic script were preserved and not committed.

```text
PHASE 13: NOT STARTED
PHASE 12: BLOCKED
```
