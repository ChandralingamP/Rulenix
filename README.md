# Rulenix

Rulenix is an algorithmic trading platform featuring an asynchronous **Python / FastAPI** backend, a modern **React 19 / Vite** frontend, and **PostgreSQL 16**. It provides Angel One SmartAPI session management, automated strategy execution (Futures Breakout & SuperTrend Index Options), risk controls, demo/live trading lifecycle management, broker reconciliation, and real-time WebSocket market data streaming.

## Tech Stack

- **Backend**: Python 3.12+, FastAPI, Pydantic v2, SQLAlchemy (asyncpg), Uvicorn
- **Frontend**: React 19, Redux Toolkit, Vite, Tailwind CSS
- **Database**: PostgreSQL 16
- **Broker Integration**: Angel One SmartAPI REST APIs & SmartStream WebSocket market data feed
- **Orchestration**: Docker Compose, Nginx, Systemd

## Repository Layout

```text
Rulenix/
  frontend/          React 19 / Redux Toolkit client application
  python-backend/    FastAPI backend, Angel One broker client, trading engine, background workers
  infra/             Production Nginx, Caddy, and systemd deployment configurations
  docker-compose.prod.yml  Production multi-container orchestration
  README.md          Project documentation
```

## Backend Architecture

The backend (`python-backend/`) is structured into dedicated domain layers:
- `app/api/`: REST API endpoints (authentication, broker connection, trading, risk controls, admin operations)
- `app/broker/`: Angel One SmartAPI client, TOTP authentication, order execution, position sync, and WebSocket market stream
- `app/trading/`: Strategy execution engines (Futures Breakout, SuperTrend Index Options), trade lifecycle, signal evaluation
- `app/workers/`: Resilient background worker lifecycle (market data ingestion, tick updates, reconciliation, demo lifecycle, protection recovery)
- `app/models/`: SQLAlchemy database models
- `app/risk/`: Risk management engine, daily loss limits, and global kill switch controls

## Local Development Setup

### Prerequisites
- Python 3.12+
- Node.js 18+
- PostgreSQL 16
- Git

### 1. Backend Setup
```powershell
cd python-backend
python -m venv .venv
.venv\Scripts\Activate.ps1
pip install -e .
uvicorn app.main:app --host 127.0.0.1 --port 8080 --reload
```

### 2. Frontend Setup
```powershell
cd frontend
npm install
npm run dev
```
Vite serves on `http://localhost:5173` and proxies `/api` and WebSocket requests to `http://127.0.0.1:8080`.

## Production Deployment

Production runs via Docker Compose:
```bash
docker compose -f docker-compose.prod.yml up -d --build
```
Health status is available at `/api/health/ready`.
