# Rulenix

Rulenix is an algorithmic trading platform featuring an asynchronous **Python / FastAPI** backend, a modern **React 19 / Vite** frontend, and **PostgreSQL 16**. It provides Angel One SmartAPI session management, automated strategy execution (Futures Breakout & SuperTrend Index Options), risk controls, demo/live trading lifecycle management, broker reconciliation, and real-time WebSocket market data streaming.

---

## Tech Stack

- **Backend (`python-backend/`)**:
  - Python 3.12+
  - FastAPI & Pydantic v2
  - SQLAlchemy 2.0 (asyncio) + `asyncpg`
  - Alembic for database schema management
  - Angel One SmartAPI REST + SmartStream WebSockets
  - Argon2 / Cryptography for credential and token security
  - Pytest & pytest-asyncio for unit and parity testing
- **Frontend (`frontend/`)**:
  - React 19 + Vite 8
  - Redux Toolkit (`@reduxjs/toolkit`) + React Redux
  - Tailwind CSS + PostCSS
  - Axios for API communication
  - Vitest + Testing Library for unit/component testing
- **Database**: PostgreSQL 16

---

## Repository Layout

```text
Rulenix/
├── frontend/             # React 19 / Vite / Redux Toolkit client application
│   ├── src/              # Application source code (components, pages, store, features)
│   ├── Dockerfile        # Production multi-stage Nginx build
│   ├── package.json      # Dependencies and scripts
│   └── vite.config.js    # Vite configuration & /api reverse proxy
├── python-backend/       # FastAPI trading engine runtime
│   ├── app/              # FastAPI application modules
│   │   ├── api/          # Route handlers (auth, account, risk, strategies, etc.)
│   │   ├── broker/       # Angel One SmartAPI integration, egress handling, WebSockets
│   │   ├── reconciliation/ # Broker position & order reconciliation workers
│   │   ├── risk/         # Circuit breakers, daily loss limits, order sizing
│   │   ├── runtime/      # Production runtime, scheduler leadership, lease observation
│   │   ├── strategy/     # Strategy execution engines & signals
│   │   └── trading/      # Trade domain models, order routing, state machine
│   ├── tests/            # Test suite (parity, lifecycle, safety, DB state)
│   ├── Dockerfile.production # Production container build
│   ├── pyproject.toml    # Python project configuration & dependencies
│   └── .env.production.example # Example environment configuration
└── README.md             # Project documentation (this file)
```

---

## Getting Started

### Prerequisites

- **Python**: `>= 3.12`
- **Node.js**: `>= 20.x` (or `v22+` / `v26+`) and `npm`
- **PostgreSQL**: `16` running locally or accessible via network

---

### 1. Backend Setup (`python-backend`)

1. Change directory to `python-backend`:
   ```powershell
   cd python-backend
   ```

2. Create and activate a Python virtual environment:
   ```powershell
   python -m venv .venv
   .venv\Scripts\Activate.ps1
   ```

3. Install dependencies in editable mode:
   ```powershell
   pip install -e .[dev]
   ```

4. Configure environment variables:
   Copy `.env.production.example` to `.env` or set your local environment:
   ```powershell
   Copy-Item .env.production.example .env
   ```
   Set at minimum:
   - `DATABASE_URL`: `postgresql+asyncpg://user:password@localhost:5432/rulenix`
   - `JWT_SECRET`: Random secure secret key
   - `APP_ENV`: `development`
   - `PYTHON_RUNTIME_MODE`: `shadow` (or `off` for pure API testing without scheduler)

5. Start the backend development server:
   ```powershell
   uvicorn app.main:app --host 127.0.0.1 --port 8080 --reload
   ```
   The backend API will be available at `http://127.0.0.1:8080`.
   Interactive OpenAPI documentation is available at `http://127.0.0.1:8080/docs`.

---

### 2. Frontend Setup (`frontend`)

1. Change directory to `frontend`:
   ```powershell
   cd frontend
   ```

2. Install npm dependencies:
   ```powershell
   npm install
   ```

3. Start the Vite development server:
   ```powershell
   npm run dev
   ```
   The frontend runs on `http://localhost:5173` and automatically proxies `/api` and WebSocket endpoints to `http://127.0.0.1:8080`.

---

## Running Tests

### Backend Tests
From the `python-backend` directory with the virtual environment activated:
```powershell
cd python-backend
pytest
```

### Frontend Tests
From the `frontend` directory:
```powershell
cd frontend
npm test
```

---

## Docker Desktop Deployment

Rulenix includes a multi-container Docker Compose configuration (`postgres`, `python-backend`, and `frontend` with an Nginx reverse proxy):

### Start the Stack
```powershell
docker compose up -d
```

### Accessing the Services
- **Frontend Dashboard**: [http://localhost:8088](http://localhost:8088)
- **Backend Health Check**: [http://localhost:8088/api/health/ready](http://localhost:8088/api/health/ready)
- **PostgreSQL Database**: `localhost:5434` (User: `rulenix`, DB: `rulenix`)

### View Logs and Status
```powershell
docker compose ps
docker compose logs -f backend
```

### Stop the Stack
```powershell
docker compose down
```

---

## Architecture & Safety Principles

1. **Shadow Runtime & Execution Authority**:
   - By default, the Python runtime operates under `PYTHON_RUNTIME_MODE=shadow` and `PYTHON_LIVE_TRADING_ENABLED=false` to validate signals and parity without executing live market mutations until explicitly verified.
2. **Broker Reconciliation**:
   - Periodic reconciliation workers ensure internal database state strictly aligns with real-time broker positions and order books.
3. **Risk Management & Circuit Breakers**:
   - Strict daily loss limits, account exposure checks, and instantaneous admin kill-switches prevent runaway automated execution.

