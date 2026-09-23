# Rulenix Python migration runtime

FastAPI/Pydantic v2 + SQLAlchemy async/asyncpg runtime. It adopts the existing PostgreSQL schema
without creating duplicate tables. Phase 13 includes fenced, durable broker mutation primitives,
but the production scheduler and LIVE lifecycle workers are not wired into this service. Public
LIVE operations therefore remain fail-closed and Rust remains the production authority.

Run locally with `python -m uvicorn app.main:app --reload` after setting `DATABASE_URL`. Alembic is
adoption-only in this phase; do not run migrations against production.
