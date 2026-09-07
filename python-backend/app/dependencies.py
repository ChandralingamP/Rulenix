from collections.abc import AsyncGenerator
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from typing import Annotated

from fastapi import Depends, Request
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from .errors import DomainError
from .security import digest


@dataclass
class Principal:
    id: str
    username: str
    can_administer: bool
    can_live_trade: bool
    can_backtest: bool
    can_backtest_on_trading_days: bool
    trading_mode: str
    session_id: str
    csrf_hash: bytes


async def get_db(request: Request) -> AsyncGenerator[AsyncSession, None]:
    factory = request.app.state.session_factory
    if factory is None:
        raise DomainError(503, "Database unavailable.", code="database_unavailable")
    async with factory() as session:
        yield session


async def current_user(request: Request, db: Annotated[AsyncSession, Depends(get_db)]) -> Principal:
    token = request.cookies.get("rulenix_session")
    if not token:
        raise DomainError(401, "Authentication required.")
    row = (await db.execute(text("""
      SELECT s.id AS session_id,u.id,u.username,u.can_administer,u.can_live_trade,
             u.can_backtest,u.can_backtest_on_trading_days,COALESCE(p.trading_mode,'demo') trading_mode,
             s.csrf_hash,s.absolute_expires_at,s.idle_expires_at
      FROM user_sessions s JOIN users u ON u.id=s.user_id
      LEFT JOIN user_profiles p ON p.user_id=u.id
      WHERE s.token_hash=:token_hash AND s.revoked_at IS NULL AND s.idle_expires_at>NOW()
        AND s.absolute_expires_at>NOW() AND u.is_active=TRUE AND s.created_at>=u.password_changed_at
    """), {"token_hash": digest(token)})).mappings().first()
    if not row:
        raise DomainError(401, "Authentication required.")
    if request.method not in {"GET", "HEAD", "OPTIONS"}:
        csrf = request.headers.get("X-CSRF-Token") or request.cookies.get("rulenix_csrf")
        if not csrf or not hmac_compare(digest(csrf), bytes(row["csrf_hash"])):
            raise DomainError(403, "CSRF validation failed.")
    now = datetime.now(UTC)
    idle = min(row["absolute_expires_at"], now + timedelta(minutes=request.app.state.settings.session_idle_minutes))
    await db.execute(text("UPDATE user_sessions SET last_seen_at=NOW(),idle_expires_at=:idle WHERE id=:id"), {"idle": idle, "id": row["session_id"]})
    await db.commit()
    return Principal(**{key: row[key] for key in ("id","username","can_administer","can_live_trade","can_backtest","can_backtest_on_trading_days","trading_mode","session_id","csrf_hash")})


def hmac_compare(a: bytes, b: bytes) -> bool:
    import hmac
    return hmac.compare_digest(a, b)


async def admin_only(user: Annotated[Principal, Depends(current_user)]) -> Principal:
    if not user.can_administer:
        raise DomainError(403, "Administrator permission required.")
    return user
