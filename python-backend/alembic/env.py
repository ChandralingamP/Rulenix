"""Adoption-only Alembic environment: the Rust migrations remain authoritative in Phase 2."""
from alembic import context
from sqlalchemy import create_engine
from app.config import get_settings

config = context.config

def run_migrations_offline() -> None:
    url = get_settings().async_database_url.replace("+asyncpg", "")
    context.configure(url=url, literal_binds=True, dialect_opts={"paramstyle": "named"})
    with context.begin_transaction():
        context.run_migrations()

def run_migrations_online() -> None:
    url = get_settings().async_database_url.replace("+asyncpg", "")
    if not url:
        raise RuntimeError("DATABASE_URL is required for Alembic adoption checks")
    connectable = create_engine(url, pool_pre_ping=True)
    with connectable.connect() as connection:
        context.configure(connection=connection)
        with context.begin_transaction():
            context.run_migrations()

if context.is_offline_mode():
    run_migrations_offline()
else:
    run_migrations_online()

