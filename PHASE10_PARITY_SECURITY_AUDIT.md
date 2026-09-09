# Phase 10 parity and security audit

This audit is frozen to:

```text
RUST BASELINE SHA: 3f788f2a842ef9b1b66366d439431867850e3753
PYTHON PHASE 9 SHA: 89332b5701e80237094b54f9e88d7496bd326009
PHASE 10 ORIGINAL SHA: d85c682f8edd371a41152154a441c512afd616ee
REMEDIATION 1 SHA: 06aba7a3b62ecf682cd1556b94a92501bd245d14
REMEDIATION 2 SHA: dcc309eeb16229b770db21dc5d1452061bb17986
BRANCH: phase10-parity-security-audit
```

Rust remains the authoritative backend. The work used only test-feature adapters,
sanitized fixtures, loopback listeners, and disposable `rulenix_test_*` PostgreSQL
state. No deployment, production service, production database, networking, permission,
Kill Switch, or broker mutation operation was used.

## Differential framework

`python-backend/app/parity/` contains reusable fixture loading, callable and JSON
subprocess runtime adapters, narrow path-scoped normalization, HTTP comparison support,
OCO classification, and durable-state replay support. The test-only Rust adapters compile
only with the `phase10-adapter` feature. They require loopback `rulenix_test_*` PostgreSQL,
start no workers, use no production credentials, and point the inert Angel base URL at an
unbound loopback port.

Normalization is limited to explicitly named UUID/timestamp fields and explicitly unordered
collections. Financial values, quantities, sides, statuses, reason codes, readiness, safety
decisions, broker requests, and P&L are never normalized.

## Results

```text
HTTP CONTRACTS: 50/50 classified exactly once
HTTP EXECUTABLE RUST/PYTHON DIFFERENTIAL: PASS - 39/39, 0 mismatches
HTTP CONTRACT FIXTURE PARITY: 0
HTTP ENVIRONMENT-GATED: 8
HTTP MUTATION-GATED: 3
HTTP UNCLASSIFIED: 0
BROWSER WEBSOCKETS: PASS - 2/2 executable isolated comparisons
DATABASE STATE PARITY: PASS - 8/8 isolated PostgreSQL before/after cases
FUTURE BREAKOUT: PASS - 22/22 exact Rust/Python fixtures
SUPER TREND: PASS - 6/6 exact Rust/Python fixtures
TICK ROUNDING: PASS - 2/2 exact Rust/Python fixtures
OCO CLASSIFIER: PASS - 12/12 cross-runtime vectors
EOD CONCURRENCY/REPLAY: PASS - 4/4 cross-runtime scenarios
CRASH/RESTART REPLAY: PASS - 5/5 cross-runtime scenarios
STATE MACHINE: PASS
RISK/SAFETY: PASS
POSTGRESQL CONCURRENCY: PASS
```

The 39 executable HTTP cases exercise the actual Rust Axum handlers and Python ASGI
handlers over isolated loopback servers. Coverage includes public and authenticated paths,
admin and non-admin authorization, CSRF handling, validation/error responses, exports,
and deterministic local-state mutations. Every HTTP contract has exactly one evidence
classification and no executable mismatch remains.

The environment-gated contracts and exact reasons are:

- `POST /home/connect/`: requires broker session and credential exchange.
- `PATCH /home/profile/`: requires credential-encryption and token-invalidation configuration.
- `PATCH /account/profile`: requires the OTP environment.
- `POST /account/profile/request-otp`: requires SMTP/OTP delivery.
- `POST /backtesting/run`: requires broker market-data reads.
- `PUT /strategy/futures-breakout`: returns a catalog requiring the Angel contract master.
- `GET /strategies`: requires the Angel contract master.
- `PUT /strategies/{strategy_key}/activation`: returns a catalog requiring the Angel contract master.

The mutation-gated contracts and exact reasons are:

- `POST /admin/egress-ips`: privileged host-network mutation.
- `POST /admin/egress-ips/{id}/verify`: privileged host-network verification.
- `POST /pnl/trades/{trade_id}/close`: live broker mutation boundary.

Their classification is conservative even where a deterministic safe rejection path is
also covered. The Phase 9 migration-safe 503 boundaries remain approved differences:
broker connect, backtest execution, LIVE manual close, and LIVE/ALL Clear Trades.

## OCO classifier parity

The repository has no Rust implementation of the synthetic Android OCO classifier at the
frozen baseline. The authoritative production classifier is the Node implementation in
`scripts/broker-exposure-classifier.mjs`; the Phase 10 Node adapter invokes that implementation
directly. Python matches it for all 12 positive and fail-closed vectors, including non-flat
positions, individual-order evidence, non-AB1007 errors, timeouts, conditional-order evidence
and failures, exact/sibling trade records, malformed shapes, and missing evidence.

## EOD concurrency and crash/restart replay

Cross-runtime durable-state evidence covers the 15:10 Asia/Kolkata boundary at 15:09:59,
15:10:00, and 15:10:01 plus replay after restart. Repeated materialization produces one
signal and one square-off intent. Existing real-PostgreSQL multi-instance tests cover
advisory leadership/failover and `FOR UPDATE SKIP LOCKED` claims across four workers and
32 items.

Crash/restart parity covers stale execution claims, SL2 reversal recovery, manual-close
recovery, interrupted EOD square-off, and interrupted protection-order processing. Replay
preserves the original intent/order identity and creates no duplicate close, reversal,
protection, or EOD action. This work corrected a Python-only stale-order recovery defect:
only uncertain submissions become `ambiguous`; interrupted processing and cancellation
return to the same reconciliation states and messages as Rust.

## Security and safety

- Authentication, cookie attributes, CSRF, session expiry, and RBAC: PASS.
- IDOR/ownership and account isolation: PASS.
- SQL injection/manual dynamic-SQL review: PASS; values remain bound parameters.
- Helper injection and egress isolation: PASS.
- Secret redaction and response leakage checks: PASS.
- Mutation boundary static scan and network trap: PASS with zero reachable paths and requests.
- `npm audit --audit-level=high`: PASS, zero vulnerabilities after compatible lockfile-only updates.
- `cargo audit`: PASS with no vulnerabilities; one allowed yanked `chacha20 0.10.1` warning remains.
- `pip-audit`: NOT EXECUTED - tool unavailable in the isolated environment.
- `bandit`: NOT EXECUTED - tool unavailable in the isolated environment.

The unavailable optional Python security tools are informational, not functional parity
blockers; the authoritative CI should continue to run its configured audit and secret scans.

## Full verification

```text
PYTHON TESTS: PASS - 171 passed, 1 skipped
POSTGRESQL TESTS: PASS - included in the isolated full suite; no SQLite substitution
RUFF: PASS
MYPY: PASS - 71 source files
COMPILEALL: PASS
RUST FORMAT: PASS
RUST CLIPPY: PASS - all targets, locked, warnings denied
RUST TESTS: PASS - 131 passed, 0 failed, 31 ignored
FRONTEND TESTS: PASS - 29/29
FRONTEND LINT: PASS
FRONTEND BUILD: PASS
NPM AUDIT: PASS - 0 vulnerabilities
CARGO AUDIT: PASS - 0 vulnerabilities; 1 allowed yanked warning
```

The single Python skip is the established Windows-only Unix-helper case. Python emitted
two Starlette/httpx deprecation warnings. Rust's ignored cases are the established tests
that require an explicitly provided loopback `TEST_DATABASE_URL`; the Phase 10 audit ran
its PostgreSQL-backed tests with that isolated URL.

## Findings

```text
BLOCKERS: NONE
MAJOR GAPS: NONE
MINOR/INFORMATIONAL:
- pip-audit and bandit are unavailable in the isolated environment.
- Starlette/httpx emitted two deprecation warnings.
- cargo audit reports one allowed yanked chacha20 0.10.1 package warning.
```

## Mutation safety

```text
REACHABLE PYTHON ANGEL MUTATION TRANSPORT PATHS: 0
PYTHON ANGEL MUTATION HTTP REQUESTS: 0
PYTHON LIVE ORDERS PLACED: 0
PYTHON LIVE ORDERS MODIFIED: 0
PYTHON LIVE ORDERS CANCELLED: 0
FAKE LIVE BROKER SUCCESSES: 0
PRODUCTION MODIFIED: NO
```

PHASE 10: PASS — READY FOR PRODUCTION SHADOW DEPLOYMENT
