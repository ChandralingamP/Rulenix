import ssl
from collections.abc import AsyncIterator
from pathlib import Path

from sqlalchemy import text
from sqlalchemy.engine.url import make_url
from sqlalchemy.ext.asyncio import (
    AsyncEngine,
    AsyncSession,
    async_sessionmaker,
    create_async_engine,
)


def make_engine(url: str) -> AsyncEngine | None:
    if not url:
        return None
    u = make_url(url)
    connect_args: dict[str, object] = {}
    query = dict(u.query)
    raw_sslmode = query.pop("sslmode", None)
    raw_sslrootcert = query.pop("sslrootcert", None)
    sslmode = raw_sslmode if isinstance(raw_sslmode, str) else None
    sslrootcert = raw_sslrootcert if isinstance(raw_sslrootcert, str) else None
    if sslmode or sslrootcert:
        if sslmode == "disable":
            connect_args["ssl"] = False
        else:
            ctx = ssl.create_default_context()
            if sslrootcert and Path(sslrootcert).exists():
                ctx.load_verify_locations(sslrootcert)
            if sslmode == "verify-full":
                ctx.check_hostname = True
                ctx.verify_mode = ssl.CERT_REQUIRED
            elif sslmode in ("verify-ca", "require"):
                ctx.check_hostname = False
                ctx.verify_mode = ssl.CERT_REQUIRED
            strict_flag = getattr(ssl, "VERIFY_X509_STRICT", 0)
            if strict_flag:
                ctx.verify_flags &= ~strict_flag
            connect_args["ssl"] = ctx
        u = u.set(query=query)
    return create_async_engine(
        u, connect_args=connect_args, pool_pre_ping=True, pool_size=5, max_overflow=5
    )


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

