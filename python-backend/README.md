# Rulenix Python foundation

FastAPI/Pydantic v2 + SQLAlchemy async/asyncpg foundation. It adopts the existing PostgreSQL schema
without creating duplicate tables. `PYTHON_LIVE_TRADING_ENABLED` is rejected if true. Broker and
trading behavior is intentionally unavailable; deferred endpoints return a structured 503.

Run locally with `python -m uvicorn app.main:app --reload` after setting `DATABASE_URL`. Alembic is
adoption-only in this phase; do not run migrations against production.

