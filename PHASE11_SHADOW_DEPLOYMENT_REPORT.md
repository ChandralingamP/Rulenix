# Phase 11 production shadow deployment report

## Fresh read-only preflight

Preflight was performed on 2026-09-09 without changing production. The LIVE exposure gate was
rechecked at 17:18 UTC against `broker_deployment_account_safety` after Rust reconciliation.

```text
PRODUCTION RUST RELEASE: 3f788f2a842ef9b1b66366d439431867850e3753
PYTHON SHADOW RELEASE: NOT DEPLOYED
SHADOW START TIME: NOT STARTED
SHADOW OBSERVATION DURATION: 0

RUST HEALTH: PASS - container healthy, restart count 0
FRONTEND HEALTH: PASS - running, restart count 0
POSTGRESQL HEALTH: PASS - PostgreSQL 16.14, container healthy, restart count 0
PUBLIC READINESS: PASS - HTTP 200
PYTHON SHADOW HEALTH: NOT DEPLOYED

PYTHON AUTHORITATIVE DB WRITES: 0 - denied in isolated role proof; production roles not provisioned
PYTHON SHADOW DB WRITES: 0 IN PRODUCTION - isolated local write proof passed
PYTHON ANGEL MUTATION TRANSPORT: ABSENT FROM REVIEWED SHADOW IMAGE; IMAGE NOT DEPLOYED
PYTHON ANGEL MUTATION REQUESTS: 0

FRONTEND ROUTING: Rust backend (`proxy_pass http://backend:8080/api/`)
AUTHORITATIVE TRADING BACKEND: Rust
GLOBAL KILL SWITCH: DISABLED
FORCE_DEMO_TRADING: FALSE
DATABASE MIGRATIONS: 49 successful, 0 failed, latest 20260904010000
```

Production uses Caddy in front of the frontend Nginx container. Nginx serves React and proxies
`/api/` to the Rust `backend` service. PostgreSQL, Rust, and frontend share the existing
`rulenix_default` bridge. The egress helper is active, its socket is `0600` and owned by UID
10001, both configured addresses are `CONFIGURED/VERIFIED`, and one account is assigned to each.

The latest backup file visible in the established backup directory is
`rulenix-predeploy-3f788f2-20260905T071147Z.dump.enc` (10,816,048 bytes). No fresh Phase 11 backup
or disposable restore was created because the LIVE exposure gate failed first.

## LIVE and readiness state

```text
OPEN LIVE APPLICATION TRADES: 0
OPEN BROKER POSITION INCIDENTS: 0
NONTERMINAL LIVE ORDERS: 2
NONTERMINAL LIVE EXECUTION INTENTS: 2
DEPLOYMENT-SAFETY BLOCKERS: 4
OPEN RECONCILIATION BLOCKER ROWS: 0
LIVE ACCOUNT RECONCILIATION: HEALTHY/FRESH
```

Both orders belong to one LIVE Futures Breakout account. They are opposite-side `BUY_ENTRY` and
`SELL_ENTRY` orders, quantity 20 each, and Angel status is freshly reconciled as
`trigger pending`. Their paired execution intents remain `submitted`. These are genuine
exposure-capable LIVE broker orders even though no LIVE trade is open.

No direct duplicate Python broker read was performed. The preflight used Rust's freshly updated,
credential-revision-bound reconciliation evidence and durable broker-order state. This avoided
additional Angel sessions, requests, quota use, and mutable client state.

## Deployment design review gate

1. **Inputs:** read-only polling of Rust-persisted signals, snapshots, intents, cached candles,
   reconciliation health, and deployment-safety state.
2. **Authoritative reads:** exact column grants only; no credential ciphertext or secret values.
3. **Shadow writes:** only `rulenix_shadow.observations` and `rulenix_shadow.observer_health`.
4. **Write-denial proof:** PASS on isolated PostgreSQL; production proof awaits provisioning.
5. **Broker mutation transport:** absent from the image; Python also has no external network route,
   broker credentials, Angel configuration, or helper socket.
6. **Angel sessions/reads:** none. Existing Rust-normalized durable evidence is reused.
7. **Resource limits:** Python 0.25 CPU/256 MiB/64 PIDs; proxy 0.10 CPU/32 MiB/32 PIDs; four total
   bounded DB connections.
8. **Failure isolation:** independent containers/network/restart policy; no Rust lifecycle or
   routing dependency.
9. **Rollback:** stop/remove only `python-shadow` and `shadow-db-proxy`; retain Rust and PostgreSQL.
10. **Required restarts:** none planned for Rust, frontend, PostgreSQL, Caddy, or egress helper.

The normal Python API entrypoint is explicitly rejected for production shadow use: it can
rehydrate egress aliases and exposes authoritative-write handlers/repositories. The dedicated
image excludes those modules and starts only `app.shadow`.

## Gate result and exact blocker

The code and isolated permission model are ready for further validation, but production
provisioning and container startup were not performed. Creating production roles/schema is a
PostgreSQL change. The Phase 11 instructions require a stop when a deployment action could alter
PostgreSQL while LIVE exposure-capable orders exist. Production currently has two freshly
confirmed `trigger pending` LIVE entry orders and two submitted intents.

Local validation completed before the stop:

```text
SHADOW UNIT/STRUCTURAL TESTS: 6 passed
ISOLATED POSTGRESQL PERMISSION PROOF: 1 passed
COMPLETE PYTHON REGRESSION: 177 passed, 2 skipped
RUFF: PASS
MYPY: PASS - 79 source files
COMPILEALL: PASS
COMPOSE CONFIG VALIDATION: PASS
PRODUCTION IMAGE BUILD: NOT RUN - local Docker daemon unavailable; production build prohibited by LIVE gate
```

The skips are the established Windows Unix-helper case and the explicitly provisioned-role
permission proof in the general suite. That permission proof was also run separately with its
isolated PostgreSQL roles enabled and passed. The two existing Starlette/httpx deprecation
warnings remain informational.

Resume only after Rust/Angel's normal lifecycle makes those orders terminal and the durable
intents terminal, followed by a fresh read-only exposure audit and a fresh restore-verified
backup. Do not cancel, modify, clear, or otherwise bypass the active orders for this deployment.

## Observation

```text
SHADOW DECISIONS: 0
MATCHES: 0
MISMATCHES: 0
CRITICAL: 0
HIGH: 0
MEDIUM: 0
LOW: 0
ERRORS: 0

FUTURE BREAKOUT OBSERVATIONS: 0 - shadow not started
SUPER TREND OBSERVATIONS: 0 - shadow not started
READINESS/RECONCILIATION OBSERVATIONS: 0 - shadow not started

CPU IMPACT: NONE
MEMORY IMPACT: NONE
DB IMPACT: READ-ONLY PREFLIGHT ONLY
BROKER READ IMPACT: NONE
CROSS-ACCOUNT LEAKAGE: 0
RUST INTERRUPTION: 0
FRONTEND INTERRUPTION: 0
ROLLBACK/REMOVAL TEST: NOT RUN - no shadow service was started
```

## Safety lines

```text
PYTHON LIVE ORDERS PLACED: 0
PYTHON LIVE ORDERS MODIFIED: 0
PYTHON LIVE ORDERS CANCELLED: 0
PYTHON LIVE POSITIONS CLOSED: 0
PYTHON AUTHORITATIVE TRADING WRITES: 0
REACHABLE PYTHON ANGEL MUTATION TRANSPORT PATHS: 0
PYTHON ANGEL MUTATION HTTP REQUESTS: 0
FAKE LIVE BROKER SUCCESSES: 0
RUST REMAINS AUTHORITATIVE: YES
FRONTEND STILL ROUTED TO RUST: YES
```

PHASE 11: BLOCKED
