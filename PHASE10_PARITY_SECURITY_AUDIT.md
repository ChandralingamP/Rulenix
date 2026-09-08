# Phase 10 parity and security audit

This audit is frozen to:

```text
RUST BASELINE SHA: 3f788f2a842ef9b1b66366d439431867850e3753
PYTHON PHASE 9 SHA: 89332b5701e80237094b54f9e88d7496bd326009
BRANCH: phase10-parity-security-audit
```

Rust remains the authoritative backend. No deployment, production service, production
database, networking, permission, Kill Switch, or broker mutation operation was used.

## Differential framework

`python-backend/app/parity/` contains reusable fixture loading, callable and JSON
subprocess runtime adapters, narrow path-scoped normalization, and structured comparison.
`tests/parity/phase10_fixtures.json` is sanitized and contains the framework smoke
comparisons. The test-only Rust adapter is compiled with the `phase10-adapter` feature and
consumes `tests/parity/strategy_differential_fixtures.json` through JSON stdin/stdout.
`tests/parity/phase10_differential_results.json` records the exact comparison results and
`tests/parity/phase10_scorecard.json` is the machine-readable scorecard.

Normalization is limited to explicitly named UUID/timestamp fields and explicitly unordered
collections. Financial values, quantities, sides, statuses, reason codes, readiness, safety
decisions, broker requests and P&L are never normalized.

## Results

```text
HTTP CONTRACTS: 50/50 inventoried; 0 executable Rust HTTP comparisons; 44 fixture-only, 5 mutation-disabled, 1 environment-gated
BROWSER WEBSOCKETS DIFFERENTIAL-COVERED: 0/2 executable Rust comparisons
DATABASE STATE PARITY: GAP — Rust before/after adapter unavailable
FUTURE BREAKOUT RUST↔PYTHON PARITY: BLOCKED — 12 exact price/error mismatches in 12 fixtures
SUPER TREND RUST↔PYTHON PARITY: PASS — 6/6 fixtures, including ATR/reversal and 15:10 boundary
STATE MACHINE PARITY: PASS — Python invariant matrix
RISK/SAFETY PARITY: PASS — Python adversarial matrix
```

The Phase 9 migration-safe 503 boundaries remain explicit approved differences: broker
connect, backtest execution, LIVE manual close, and LIVE/ALL Clear Trades.

## Security and safety

- Authentication, cookie attributes, CSRF, session expiry and RBAC: PASS through existing regression and adversarial tests.
- IDOR/ownership and account isolation: PASS through user-scoped SQL and broker isolation tests.
- SQL injection: PASS after manual review of eight fixed-identifier SQL builders; all values remain bound parameters. No user-controlled value is interpolated.
- Helper injection: PASS through typed egress/helper protocol tests.
- Secret leakage: PASS through credential repr/redaction and response tests.
- Mutation boundary: PASS. Static scan found zero reachable mutation transport paths and the network trap observed zero mutation requests.
- Dependency audit: `pip-audit` and `bandit` are not installed in the isolated environment; this is INFORMATIONAL and must be rerun in security CI.

## Concurrency and recovery

Real isolated PostgreSQL evidence now passes for advisory leadership/failover, `FOR UPDATE
SKIP LOCKED` claiming across four workers and 32 items, stale claim recovery, and account-
scoped existing reversal/manual-close idempotency tests. The Phase 10 fault injector
provides deterministic PostgreSQL, Angel-read, WebSocket, egress and lifecycle checkpoints.
Cross-runtime crash replay and full bounded stress across all strategy workers remain outside
the executable adapter.

## Full verification

```text
PYTHON TESTS: 104 passed, 1 Windows-only Unix-helper skip
POSTGRESQL TESTS: included in isolated run; no SQLite substitution
PHASE 10 FRAMEWORK/ADVERSARIAL/CONCURRENCY TESTS: 21 passed, 3 skipped
RUFF: PASS
MYPY: PASS
COMPILEALL: PASS
PIP-AUDIT: unavailable (not installed)
BANDIT: unavailable (not installed)
RUST TESTS: 131 passed, 0 failed, 31 ignored
FRONTEND TESTS: 29 passed
FRONTEND LINT: PASS
FRONTEND BUILD: PASS
```

## Gaps

```text
BLOCKERS:
- Future Breakout exact Rust/Python differential mismatch: Rust f64 serialization differs from Python Decimal price/exit values; insufficient-history error detail also differs.
- No isolated Rust HTTP/WebSocket runtime adapter or PostgreSQL before/after state adapter is connected.

MAJOR GAPS:
- Browser WebSocket packet/reconnect differential evidence is not executable against Rust.
- OCO cross-runtime comparison is not connected.

MINOR GAPS:
- Security tooling must be installed and rerun in the approved CI image.
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

PHASE 10: BLOCKED
