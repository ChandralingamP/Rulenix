# Phase 12 Python DEMO trial and Phase 13 preflight

## Authority boundary

Rust release `ab14c3ca05ce91ad959b29ace66f623936cd4f76` remains the only public API,
scheduler for authoritative application state, LIVE reconciliation process, and Angel mutator.
Phase 12 neither routes traffic to Python nor grants Python a write on `public`.

The Phase 12 service owns only two explicit synthetic DEMO trial accounts inside
`rulenix_demo_trial`. Assignments have a database constraint requiring `execution_mode='demo'`.
Inputs from production are read by `rulenix_demo_trial_reader`, which is transaction-read-only and
has column-level access only to calendar, scheduler, signal, and global Kill Switch state. Outputs
are written by `rulenix_demo_trial_writer`, which has no `public` schema usage or table privilege.

The image contains only `app.demo_trial` and `asyncpg`. It deliberately excludes the API, Angel
client, credentials, egress, reconciliation, trading, and general worker packages. It receives no
broker URL, key, token, session, encryption key, or egress-helper socket. Its Docker network is
internal, has no gateway, and publishes no port. These controls keep Angel mutation technically
unreachable independently of configuration flags.

## Duplicate prevention

Rust cannot execute a Phase 12 cycle because the account assignments, cycle namespace, signals,
intents, orders, and trades exist only in `rulenix_demo_trial`; Rust has no dependency on that
schema. Python has a single PostgreSQL advisory-lock leader. Each cycle has a unique stable key;
signals are unique by cycle, intents/orders have stable idempotency keys, and trades are unique by
cycle and lineage. Cycle keys include the observer release, so every code release produces fresh
evidence; within one release a completed cycle cannot be reclaimed. A stale running claim becomes
failed and is safely reclaimed without duplicating child state.

This isolates the trial without disabling or editing any real user's strategy activation. It also
means trial trades are intentionally not presented as user-facing production trades. Real market
observation and deterministic execution are reported separately.

## Trial coverage

The production service records actual closed-session decisions from the live production clock,
calendar, and Rust scheduler evidence. The observed Futures session had no signal. The observed
SuperTrend session had a Rust `SQUARE_OFF` signal with zero expected users, which is explicitly
classified as EOD with no open DEMO position rather than as no-signal. Full lifecycle events that
do not naturally occur during the observation window use immutable deterministic Rust-oracle cases:

- Futures BUY target, SELL SL1, BUY SL2 and opposite SELL reversal;
- SuperTrend target, stop, and 15:10 EOD semantics;
- manual DEMO Close;
- global Kill Switch entry denial;
- no-signal cycles for both strategies;
- two isolated trial accounts for every replay case.

Each eligible case persists a cycle, signal, entry/terminal intents, simulated entry and
protection orders, terminal trade, P&L, reversal lineage when applicable, and a terminal event.
Material comparison fields include timing key, strategy, instrument, side, quantity, entry,
target, SL1, SL2, exit reason/price, P&L, reversal, state counts, and account fan-out. Financial
values are compared exactly in these fixtures.

## Failure and rollback behavior

Worker exceptions do not release leadership or bypass the durable claim. PostgreSQL errors fail
the health check closed and increment a cumulative poll-error counter distinct from lifecycle
parity errors. Stale claims are recovered on startup and every poll. Restarting the container
reuses the same release/cycle keys and cannot create a second signal, intent, order, or trade. A
second instance cannot acquire the advisory lock.

Phase 12 rollback stops/removes only `python-demo-trial` and `demo-trial-db-proxy`. The disposable
trial schema is retained for evidence. Rust, frontend, PostgreSQL, Caddy, egress, user settings,
and the Phase 11 shadow are not stopped or reconfigured.

## Phase 13 LIVE-mutation gaps

Python is not ready for LIVE authority. Its `AngelRestClient` implements reads only. `AngelClient`
hard-blocks place, cancel, and manual close before transport and has no modify or GTT mutation
method. `ExecutionOrchestrator` constructs typed requests but returns
`LIVE_MUTATION_DISABLED_DURING_MIGRATION`, leaves work retryable, and never submits it.
Reconciliation applies read-only broker truth.

Phase 13 therefore requires, at minimum:

1. typed Angel place/cancel/modify and required GTT endpoints with strict response validation;
2. risk-reducing manual LIVE close and protective target/SL management;
3. cumulative fills, partials, OCO sibling handling, SL2 reversal, EOD square-off, and over-close
   recovery;
4. stable tags/idempotency keys, durable submission records, ambiguous-write reconciliation, and
   restart-safe retry state machines;
5. per-account source-IP binding and bounded rate/cooldown policy;
6. an immediately-before-mutation database risk/readiness/collision recheck;
7. mutation-trap tests, fault injection, broker sandbox/certification, and a reviewed production
   mutation allowlist that cannot be enabled by a client request.

## Phase 13 authority-transfer sequence (not executed)

1. Resolve every Phase 13 blocker and create a restore-verified encrypted backup.
2. Freeze control-plane changes and disable new entries while preserving protective exits.
3. Drain Rust scheduler and workers; prove there are no running claims or ambiguous submissions.
4. Perform fresh authoritative positions, order/trade, individual-order, and GTT reconciliation.
5. Prove all database migrations are backward-compatible with the retained Rust image.
6. Start Python in read/reconcile mode and acquire a dedicated authority lease only after Rust has
   relinquished it. A database epoch/fencing token must reject stale Rust or Python workers.
7. Transfer scheduler, execution, reconciliation, feed, and API authority as one fenced operation;
   never run two broker mutators.
8. Enable mutation for a reviewed canary account only after the final safety recheck, then verify
   idempotency, protection, telemetry, routing, and broker state before expanding.
9. Route frontend/API traffic only after all canary gates pass. Retain the stopped Rust image and
   its compatible configuration for rollback.

## Phase 13 rollback sequence (not executed)

Trigger rollback on any material parity mismatch, stale scheduler/feed, repeated worker error,
ambiguous broker submission, protection failure, reconciliation disagreement, cross-account leak,
or readiness failure. First disable Python entries and mutation, drain/fence Python workers, and
reconcile every in-flight broker action. Only after Python has relinquished the authority lease may
Rust `ab14c3ca05ce91ad959b29ace66f623936cd4f76` resume workers and routing. The fencing epoch and
stable broker tags prevent duplicate mutation during the transition. If schema compatibility is
not proven, rollback is blocked and forward recovery is required.

## Security/dependency preflight

- `RUSTSEC-2026-0285`: production has `rustls 0.23.43`; fixed in `>=0.23.45`. This is a Phase 13
  blocker requiring a narrow reviewed lockfile update and complete Rust regression before cutover.
- `RUSTSEC-2023-0071`: transitive `rsa 0.9.10` remains a medium advisory without a fixed release;
  document exposure/mitigation and re-check before Phase 13.
- `chacha20 0.10.1` is yanked; determine the dependency path and replace it during the reviewed
  security follow-up.
- Frontend production dependencies have zero npm advisories. Development tooling has seven
  advisories (two high, four moderate, one low), including Vitest. They do not affect the deployed
  frontend bundle, but the test/build toolchain must be upgraded and revalidated before Phase 13.

The observed recovered PostgreSQL deadlocks in Rust SuperTrend square-off/execution recovery also
remain a Phase 13 operational blocker. Do not change stable Rust as part of Phase 12; diagnose and
remediate separately, then require a clean worker-error observation window.
