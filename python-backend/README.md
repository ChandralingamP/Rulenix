# Rulenix Python migration runtime

FastAPI/Pydantic v2 + SQLAlchemy async/asyncpg runtime. It adopts the existing PostgreSQL schema
without creating duplicate tables. Phase 13B adds a supervised shadow runtime, independent
database scheduler leadership, authority-lease observation/renewal, and account-scoped broker
reconciliation. Authoritative mode still refuses startup until the protection, reversal,
manual-close, and EOD mutation lifecycles are installed and certified. Rust therefore remains the
production authority.

Run locally with `python -m uvicorn app.main:app --reload` after setting `DATABASE_URL`. Alembic is
adoption-only in this phase; do not run migrations against production.

The production-capable image is built from `Dockerfile.production`. Start it with
`PYTHON_RUNTIME_MODE=shadow` and `PYTHON_LIVE_TRADING_ENABLED=false` for non-authoritative
validation. Do not configure `authoritative` until a separately approved cutover transfers the
database authority lease to that exact process owner.
