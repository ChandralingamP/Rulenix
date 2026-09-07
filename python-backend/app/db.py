from collections.abc import AsyncIterator

from sqlalchemy import text
from sqlalchemy.ext.asyncio import (
    AsyncEngine,
    AsyncSession,
    async_sessionmaker,
    create_async_engine,
)


def make_engine(url: str) -> AsyncEngine | None:
    if not url:
        return None
    return create_async_engine(url, pool_pre_ping=True, pool_size=5, max_overflow=5)


def make_session_factory(engine: AsyncEngine | None) -> async_sessionmaker[AsyncSession] | None:
    return async_sessionmaker(engine, expire_on_commit=False) if engine else None


async def session_dependency(factory: async_sessionmaker[AsyncSession] | None) -> AsyncIterator[AsyncSession]:
    if factory is None:
        raise RuntimeError("Database unavailable")
    async with factory() as session:
        yield session


async def ping(engine: AsyncEngine | None) -> bool:
    if engine is None:
        return False
    try:
        async with engine.connect() as conn:
            await conn.execute(text("SELECT 1"))
        return True
    except Exception:
        return False

