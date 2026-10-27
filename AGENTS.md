# Rulenix contributor guide

## Overview

Rulenix is a trading operations platform with a Rust/Axum API and a React 19/Vite frontend. PostgreSQL is the persistence layer; SQLx applies migrations from `backend/migrations`.

## Repository layout

- `backend/`: Rust service, Angel One integrations, trading/risk logic, and SQL migrations.
- `frontend/`: React/Redux client, pages, components, and Vitest tests.
- `infra/`: deployment configuration for systemd, Nginx, and Caddy.
- `scripts/`: PowerShell operational and database helpers.
- `tests/load/`: k6 smoke/load checks.
- `docs/`: operational and strategy documentation.

Important entry points:

- `backend/src/main.rs`: startup, background tasks, middleware, and API route composition.
- `backend/src/config.rs`: environment parsing and production fail-closed validation.
- `backend/src/state.rs`: process-wide shared state.
- `backend/src/auth.rs`, `security.rs`, and `credentials.rs`: sessions, CSRF/rate limits, and encrypted broker secrets.
- `backend/src/strategy.rs`, `risk.rs`, `angel.rs`, and `backtesting.rs`: strategy lifecycle, risk gates, broker integration, and simulations.
- `frontend/src/App.jsx`: page selection and access routing.
- `frontend/src/utils/axiosConfig.js`: credentialed API requests, CSRF headers, and global unauthorized handling.
- `frontend/src/features/`: Redux Toolkit state and async API operations.
- `docs/project-workflow.md`: the best high-level runtime and data-flow map.

## Development commands

Run backend checks from the repository root:

```powershell
cargo fmt --manifest-path backend/Cargo.toml -- --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path backend/Cargo.toml --locked
```

Run the backend itself from `backend/`; migrations are loaded from `./migrations`:

```powershell
cd backend
cargo run
```

Run frontend checks from `frontend/`:

```powershell
npm ci
npm run lint -- --max-warnings=0
npm test -- --run
npm run build
```

Use `setup-database.ps1` for local PostgreSQL provisioning. Start the client with `npm run dev` in `frontend/`; Vite serves port 5173 and proxies `/api` and WebSockets to the backend on port 8080. `start-rulenix.ps1` and `stop-rulenix.ps1` manage both local processes and write ignored runtime output under `.runlogs/`.

Use the environment-specific templates under `backend/.env.*.example`; never create a committed real environment file. Production packaging is defined by `docker-compose.prod.yml` and the two Dockerfiles.

## Architecture and conventions

- Keep public endpoints in `public_api` and session-protected endpoints in `protected_api`. New protected routes must remain behind `auth::authenticated`; state-changing requests must keep CSRF enforcement.
- Derive user identity and permissions from the server-side session. Do not reintroduce request-body `username`, `admin_username`, role, or trading-mode authority.
- Return handler failures through `AppError` so status codes and JSON error shapes remain consistent. Avoid exposing raw database, broker, credential, token, or OTP details.
- Pass cross-cutting resources through `AppState`. Preserve bounded concurrency, feed ownership/generation checks, and per-user isolation in background work.
- Use SQLx with PostgreSQL. Add a new forward migration for schema changes; do not rewrite a migration that may already have run. Startup applies migrations under a PostgreSQL advisory lock.
- Keep migration names in `YYYYMMDDHHMMSS_lowercase_description.sql` format and make migration SQL safe for rolling/retried deployment where practical.
- Use `chrono` explicitly for time handling. Trading schedules and session dates are defined in IST and by `market_calendar`; do not substitute host-local time or assume every weekday is open.
- The frontend intentionally aliases `react-router-dom` to `frontend/src/router.jsx`. Extend that local router when routing behavior changes instead of adding assumptions from the upstream package.
- Use the configured Axios client for application API calls so cookies, CSRF headers, and 401 handling remain uniform. Keep Redux async state in the existing feature slices where one exists.
- Follow the existing colocated `*.test.jsx`/`*.test.js` convention for frontend behavior and inline `#[cfg(test)]` modules for Rust unit tests.

## Trading and security invariants

- Treat `can_administer`, `can_live_trade`, `can_backtest`, and the selected `demo`/`live` trading mode as separate controls.
- Every new entry path must pass the database-backed risk engine and preserve global/per-user kill-switch behavior. Kill switches may stop entries and cancel pending entries, but must not leave open positions without protective exits.
- Live order submission must remain durable and idempotent. Preserve stable order tags, ambiguous-submission reconciliation, cumulative fill handling, restart recovery, and isolation between users.
- Do not silently weaken stale-tick checks, broker/session health checks, position and daily-loss limits, or the distinction between demo simulation and live broker execution.
- Broker credentials, JWTs, refresh/feed tokens, API keys, MPINs, TOTPs, OTPs, encryption keys, SMTP credentials, and production connection strings are secrets. Keep encryption and redaction intact and never print them in logs, tests, fixtures, reports, or tool output.
- Keep production database TLS verification, HTTPS origin validation, credentialed CORS restrictions, request body limits, security headers, abuse prevention, and secure cookie settings fail-closed.

## Change guidelines

- Preserve unrelated worktree changes; inspect `git diff` before editing overlapping files.
- Add frontend tests for changed user-visible behavior and backend tests for changed API or trading logic where practical.
- Keep Rust formatting and Clippy warning-free. Follow existing React/Redux and Tailwind patterns.
- Update relevant documentation and environment templates when configuration or operational behavior changes.
- For strategy changes, update the matching strategy document and verify scheduler timing, restart/catch-up behavior, order persistence, risk decisions, demo execution, and live reconciliation as applicable.
- Avoid broad rewrites of `backend/src/strategy.rs`; make focused changes around the relevant lifecycle and add regression coverage near its existing test module.

## Validation and deployment

The CI workflow in `.github/workflows/ci.yml` is authoritative: it runs Rust format, Clippy, tests, migration filename validation, audit checks, frontend lint/tests/build, npm audit, and secret scanning. Do not commit local `.env` files, runtime logs, build output, or credentials.

During iteration, run the narrowest relevant test first. Before handing off a change, run the checks proportional to its scope; run the complete backend and frontend command sets above for cross-cutting, security-sensitive, schema, dependency, or trading-execution changes. If a check cannot run because PostgreSQL, broker access, credentials, or another external dependency is unavailable, report exactly what was and was not verified.

Use `docs/deployment.md`, `docs/production-security.md`, `docs/observability.md`, `docs/disaster-recovery.md`, and `docs/go-live-checklist.md` for production-affecting work. Do not deploy, rotate credentials, operate kill switches, trigger live jobs, or mutate production data unless the user explicitly requests that operational action.
