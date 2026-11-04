# Rulenix Python Foundation Contributor Guide

## Overview

This workspace (`python-foundation` worktree on branch `python-production`) hosts the isolated Python backend (FastAPI/SQLAlchemy) and React 19 frontend stack for Rulenix. PostgreSQL is the backing persistence store; the Python runtime adopts the existing schema without duplicating tables.

---

## Workspace Layout

```
.
├── python-backend/               # Python/FastAPI service and migration runtime
│   ├── alembic/                 # Adoption-only migration configurations
│   ├── app/                     # Application source tree
│   │   ├── api/                 # FastAPI routes (auth, admin, strategy, pnl, egress, ws)
│   │   ├── broker/              # Angel One broker API, auth, egress, mutation guards
│   │   ├── demo_trial/          # Demo execution & simulated trial engine
│   │   ├── parity/              # Differential parity validation framework
│   │   ├── reconciliation/      # Trade, order, and broker state reconciliation
│   │   ├── risk/                # Pre-trade risk gates, daily loss, kill-switch
│   │   ├── runtime/             # Supervisor, worker lifecycles, database scheduler
│   │   ├── shadow/              # Shadow runtime evaluation & verification
│   │   ├── strategy/            # SuperTrend and Futures Breakout strategies
│   │   ├── trading/             # Core trading domain models, repository, state machine
│   │   ├── config.py            # Pydantic Settings & environment validation
│   │   ├── db.py                # AsyncEngine, async_sessionmaker & database health
│   │   ├── dependencies.py      # Session extraction, user auth & database session injection
│   │   ├── errors.py            # Domain and HTTP error hierarchy
│   │   ├── main.py              # Application lifecycle, CORS, route composition
│   │   └── security.py          # Password hashing, tokens, CSRF protection
│   ├── tests/                   # Comprehensive pytest suite (unit, integration, adversarial)
│   ├── pyproject.toml           # Project metadata, dependencies, ruff & pytest configs
│   └── Dockerfile.production    # Production container definition
├── frontend/                    # React 19 / Vite client
│   ├── src/                     # React components, pages, Redux slices, custom router
│   │   ├── features/            # Redux Toolkit state slices
│   │   ├── pages/               # Views (Home, Admin, Strategy, PnL, Login, etc.)
│   │   ├── utils/               # Axios configuration with CSRF & credential handling
│   │   ├── App.jsx              # Main app entry & route dispatch
│   │   └── router.jsx           # Internal router alias for react-router-dom
│   ├── package.json             # NPM package manifest & scripts
│   ├── vite.config.js           # Vite dev server & proxy settings (port 5173 -> 8080)
│   └── tailwind.config.js       # Tailwind CSS styling configuration
├── .env                         # Local runtime environment settings
└── .gitignore                   # Workspace ignore rules
```

---

## Development & Test Commands

### Backend (`python-backend/`)

Run commands with `python-backend` in `PYTHONPATH` or navigate into `python-backend`:

- **Run all unit & non-database tests:**
  ```powershell
  $env:PYTHONPATH="python-backend"; python -m pytest python-backend/tests
  ```
- **Run a specific test file:**
  ```powershell
  $env:PYTHONPATH="python-backend"; python -m pytest python-backend/tests/test_security.py
  ```
- **Linting & Code Formatting:**
  ```powershell
  python -m ruff check python-backend
  python -m ruff format --check python-backend
  ```
- **Static Type Checking:**
  ```powershell
  python -m mypy python-backend/app
  ```
- **Run Development Server (Port 8080):**
  ```powershell
  cd python-backend
  python -m uvicorn app.main:app --reload --port 8080
  ```

### Frontend (`frontend/`)

- **Install dependencies:**
  ```powershell
  cd frontend
  npm ci
  ```
- **Run Dev Server (Port 5173 with API proxy to 8080):**
  ```powershell
  cd frontend
  npm run dev
  ```
- **Run Vitest Tests:**
  ```powershell
  cd frontend
  npm test -- --run
  ```
- **Run ESLint:**
  ```powershell
  cd frontend
  npm run lint
  ```
- **Build Production Bundle:**
  ```powershell
  cd frontend
  npm run build
  ```

---

## Architecture & Code Conventions

1. **Authentication & Session Security:**
   - Public endpoints are declared in `app.api.auth` (`/api/auth/login`, `/api/auth/logout`, `/api/health`).
   - Protected endpoints require `get_current_user` / `get_current_admin` dependencies.
   - User identity and permissions are strictly derived from server-side sessions; never trust client-supplied user IDs or roles in request bodies.
   - All state-mutating requests (`POST`, `PUT`, `DELETE`, `PATCH`) must validate CSRF tokens via `validate_csrf`.

2. **Database & Persistence:**
   - Uses SQLAlchemy Async (`AsyncEngine`, `AsyncSession`) with `asyncpg`.
   - Adopts the existing PostgreSQL schema; do not run destructive migrations against live schemas.
   - Database operations must respect ownership checks (`OwnershipError`) and pass through `TradingRepository`.

3. **Trading Engine & Authority Lease:**
   - Default runtime mode is `shadow` (`PYTHON_RUNTIME_MODE=shadow`). Live trading authority is gated by `PYTHON_LIVE_TRADING_ENABLED=false` until explicit certified cutover.
   - Live order placement and mutations are mediated by `LiveMutationAuthority` and `LiveMutationCoordinator`.
   - All schedules and market session dates use Indian Standard Time (`ZoneInfo("Asia/Kolkata")`) and reference the market calendar. Never assume host-local time.

4. **Frontend Architecture:**
   - React 19 with Redux Toolkit for async state management.
   - Uses local router alias at `frontend/src/router.jsx`.
   - All API calls must use `axiosConfig.js` to ensure cookies and CSRF headers are sent uniformly.

5. **Secrets & Safe Logging:**
   - Broker credentials, TOTPs, MPINs, JWTs, encryption ciphers, and API keys are strictly confidential.
   - Redact all sensitive fields from logs, test assertions, fixtures, and reports.

