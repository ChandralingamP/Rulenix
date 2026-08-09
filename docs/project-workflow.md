# Rulenix project workflow

This document explains how the Rulenix application is organized and how data moves through the system. Strategy-specific trading rules are documented separately:

- [Futures Breakout v3](strategy-futures-breakout-v3.md)
- [Option Entry Strategy V1.0](strategy-option-entry-v1.md)

## Purpose

Rulenix is a trading operations platform for authenticated users who connect an Angel One account, configure strategies, run demo or live execution, view P&L/logs, and run controlled backtests.

The system has three main layers:

- Frontend: React/Vite/Tailwind UI in `frontend/`.
- Backend: Rust/Axum/SQLx API in `backend/`.
- Database: PostgreSQL schema managed by Rust startup migrations in `backend/migrations/`.

Production packaging is Docker-based through `docker-compose.prod.yml`; local development can run backend and frontend directly with the helper PowerShell scripts in the repository root.

## High-level runtime flow

```text
Browser
  -> React pages call /api with session cookie + CSRF token
  -> Rust Axum backend validates session, role, request limits
  -> PostgreSQL stores users, configs, snapshots, orders, trades, logs, audits
  -> Angel One SmartAPI is used for broker login, market data, quotes, margins, orders
  -> Strategy scheduler and WebSocket feeds run in backend background tasks
  -> Frontend receives current state through REST and live strategy/market WebSockets
```

## Application startup

Backend startup is handled in `backend/src/main.rs`.

1. Load `.env` / process environment.
2. Validate production configuration through `Config::from_env()`.
3. Connect to PostgreSQL. In production, database TLS is forced to `verify-full`.
4. Acquire a PostgreSQL advisory lock and run migrations.
5. Initialize encrypted broker credential storage.
6. Optionally migrate old plaintext broker credentials to encrypted records.
7. Optionally force live accounts back to demo if `FORCE_DEMO_TRADING` is enabled and no execution is in flight.
8. Run one-off CLI actions when requested:
   - `--create-admin`
   - `--create-admin-from-env`
   - `--rotate-credentials`
9. Start background loops:
   - strategy scheduler and reconciliation
   - Angel One session maintenance
   - auth session cleanup
10. Build the Axum router and listen on the configured host/port.

## Frontend pages

Frontend route selection is in `frontend/src/App.jsx`.

Main user pages:

- `/` Home: account/broker connection and profile status.
- `/strategies`: strategy activation, instrument configuration, latest scheduler status, orders, trades, alerts.
- `/backtesting`: backtest setup, latest run summary, latest trades, run history, XLSX export.
- `/pnl`: P&L list/export.
- `/logs`: user-visible logs.
- `/account` functionality is surfaced through the layout/account modal.

Auth pages:

- `/login`
- `/signup`
- `/verify-otp`
- `/forgot-password`
- `/forgot-password/verify`
- `/forgot-password/reset`

Admin pages:

- `/admin/users`
- `/admin/limits`
- `/admin/jobs`

## API route groups

The backend exposes public auth/health routes and session-protected routes under `/api`.

Public:

- `GET /api/health`, `/api/health/live`, `/api/health/ready`
- `POST /api/auth/request-otp/`
- `POST /api/auth/signup/`
- `POST /api/auth/login/`
- `POST /api/auth/password/request-reset/`
- `POST /api/auth/password/verify-otp/`
- `POST /api/auth/password/reset/`

Protected:

- Account/profile/trading mode: `/api/account/*`, `/api/home/*`
- Strategies: `/api/strategies`, `/api/strategies/{strategy_key}/activation`, `/api/strategy/futures-breakout`
- Backtesting: `/api/backtesting/run`, `/api/backtesting/runs`, `/api/backtesting/runs/{run_id}/export`
- P&L: `/api/pnl`, `/api/pnl/export`
- Logs: `/api/logs/*`
- Admin scheduler/risk/user operations: `/api/scheduler/*`, `/api/risk/admin*`, `/api/auth/admin/users/`
- WebSockets: `/api/ws/market`, `/api/ws/strategy`

Protected routes are guarded by server-side session authentication. State-changing requests also require the frontend CSRF token.

## Authentication and authorization workflow

1. User signs up or logs in through OTP/password flows.
2. Backend creates a server-side session and returns an opaque `HttpOnly` session cookie.
3. A readable CSRF cookie is echoed by the frontend in `X-CSRF-Token` for mutating requests.
4. Access checks are derived from the authenticated session, not request payload identity fields.
5. Admin and trading permissions are separate:
   - `can_administer`: user/admin/risk/scheduler administration.
   - `can_live_trade`: permission to choose live mode.
   - `can_backtest`: permission to run/view/export backtests.
6. Trading mode is stored per user profile as `demo` or `live`.

Important operational rule: if a user has open trades or in-flight strategy orders, profile/account deletion or unsafe mode changes are blocked.

## Broker connection workflow

Angel One credentials and tokens are managed through the Home/account flows.

1. User stores Client ID and API key.
2. User connects with MPIN and TOTP.
3. Backend logs in to Angel One SmartAPI and stores JWT/feed/refresh tokens encrypted in `broker_secrets`.
4. A background session maintenance loop refreshes tokens when they approach expiry.
5. Invalid refresh/API token states are marked on the user profile so the UI can ask for reconnection.

Broker secrets must never be committed to the repository. Production encryption keys are provided through environment/secrets infrastructure.

## Market-data workflow

Rulenix uses Angel One for:

- REST historical candles.
- REST market quotes/LTP.
- SmartAPI WebSocket V2 live ticks.
- Margin estimation.
- Live order submission and reconciliation.

Shared market-data helpers rotate through a small pool of connected Angel One sessions. This prevents one user's temporary rate limit from immediately blocking all shared strategy data. If a broker returns a rate-limit error, the system can try another usable session; invalid credentials are marked invalid.

Market ticks are stored in `market_price_ticks` and are used by the risk engine and demo execution simulator.

## Strategy runtime workflow

The scheduler starts in `backend/src/strategy.rs`.

1. One backend replica acquires scheduler leadership with a PostgreSQL advisory lock.
2. Interrupted scheduler runs are marked failed and retried.
3. Daily futures contract metadata is selected and cached.
4. Market snapshots are refreshed once enough data is available.
5. Due scheduler actions are claimed through `strategy_scheduler_runs`.
6. Entries/protective exits are placed per enabled user/instrument.
7. Demo orders are simulated from live feed ticks.
8. Live orders are submitted to Angel One and reconciled.
9. Strategy events and operational alerts are persisted and broadcast to the UI.

The strategy engine currently exposes two strategy keys:

- `futures_breakout_v3`
- `option_entry_v1`

Futures gap-entry behavior is part of `futures_breakout_v3`; it is not a separate strategy key.

## Order lifecycle

Orders are stored in `strategy_orders`.

Common roles:

- `BUY_ENTRY`
- `SELL_ENTRY`
- `TARGET`
- `SL1`
- `SL2`

Order states include:

- pending/submission states: `pending`, `submitting`, `ambiguous`, `submitted`, `partially_filled`, `processing`, `cancelling`
- terminal states such as filled/cancelled/rejected/skipped

When an entry fill is processed, the backend creates or updates a `trades` row. Protective orders then manage exits. Live execution uses stable client order tags and reconciliation; demo execution simulates fills from ticks and updates the same tables.

## Risk workflow

Before new entries are accepted, the risk engine checks:

- user trading mode and permissions
- global/user kill switches
- configured lots/quantity
- margin/notional limits
- open positions and pending orders
- daily trade/loss limits
- fresh market ticks
- broker health and live account margin when applicable

Each allow/reject decision is written to `risk_decisions`. Staff can manage limits and kill switches from the Admin Risk page.

Kill switches stop new entries and cancel pending entries. Protective exits are not blindly cancelled because that could leave open risk unmanaged.

## Backtesting workflow

Backtesting is implemented in `backend/src/backtesting.rs` and surfaced by `frontend/src/pages/BacktestingPage.jsx`.

Users need `can_backtest`. Backtesting can be blocked during the Indian trading day to reserve Angel One capacity for live data/orders. Admin trading-day override support exists for accounts that are explicitly allowed.

Backtesting stores:

- historical candles in `backtest_market_candles`
- run metadata and summaries in `backtest_runs`
- trade rows in `backtest_trades`
- legacy/optional option contract snapshots in `backtest_option_contracts`

Backtesting data fetch is cache-first:

1. Determine the latest completed market candle; do not use wall-clock time directly inside active sessions.
2. Resolve contract/token.
3. Count cached candles in the requested range.
4. Fetch only missing ranges or missing edges.
5. Insert/update candles by `(exchange, token, interval, candle_time)`.
6. Load candles from DB and simulate.
7. Store run and trades.

Fetch chunk sizes are interval-dependent:

- `ONE_MINUTE`: 20 days
- `FIVE_MINUTE`: 90 days
- `FIFTEEN_MINUTE` / `THIRTY_MINUTE`: 180 days
- `ONE_HOUR`: 365 days
- `ONE_DAY`: 1900 days

Option Entry Strategy V1.0 backtesting is removed. `/api/backtesting/run` rejects `option_entry_v1`; the Backtesting UI exposes Futures Breakout v3 only. Option Entry validation should use live/demo runtime events, selected contract snapshots, and trade/order history.

## Database areas

Key table groups:

- Identity and auth: `users`, `user_profiles`, `email_otps`, `user_sessions`, `broker_secrets`
- Strategies: `user_strategy_activations`, `user_strategy_configs`, `strategy_market_snapshots`, `strategy_orders`, `strategy_events`, `strategy_scheduler_runs`
- Execution and P&L: `trades`, `broker_order_events`, `strategy_reversal_intents`
- Risk/ops: `risk_limits`, `risk_kill_switches`, `risk_decisions`, `market_price_ticks`, `broker_reconciliation_health`, `market_calendar`
- Backtesting: `backtest_market_candles`, `backtest_runs`, `backtest_trades`, `backtest_option_contracts`
- Audit/alerts: `audit_events`, `alert_delivery_attempts`

## Deployment workflow

See [deployment.md](deployment.md) for the operational deployment checklist.

Production deployment should:

1. Preserve production env and secrets outside source control.
2. Build backend and frontend images.
3. Let backend startup run migrations under advisory lock.
4. Restart backend/frontend containers.
5. Verify `/api/health/ready`.
6. Review compose status, logs, and operational alerts.

## Verification commands

Backend:

```powershell
cd backend
cargo fmt --all
cargo test
```

Frontend:

```powershell
cd frontend
npm test -- --run
npm run build
```

Production health:

```powershell
curl http://127.0.0.1:8080/api/health/ready
```

## Safe extension points

When adding a new strategy:

1. Add a strategy key constant.
2. Add migration(s) for any new persistent state.
3. Add configuration/catalog support.
4. Add scheduler or manual trigger flow.
5. Add risk checks before entries.
6. Persist snapshots/orders/trades/events with enough audit metadata.
7. Add backtesting only if the data source and broker API limits are understood.
8. Add frontend UI and tests.
9. Document the strategy separately under `docs/`.

Avoid reusing live strategy tables for experimental data unless the new records are clearly keyed, auditable, and harmless to reconciliation.
