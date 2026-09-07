import asyncio
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from datetime import UTC, datetime

from .auth import AngelAuthenticator
from .models import AccountContext, BrokerSession


@dataclass
class SessionManager:
    authenticator: AngelAuthenticator
    persist: Callable[[BrokerSession], Awaitable[None]] | None = None

    def __post_init__(self) -> None:
        self._locks: dict[str, asyncio.Lock] = {}
        self._sessions: dict[str, BrokerSession] = {}

    async def login(self, account: AccountContext, mpin: str, totp: str) -> BrokerSession:
        lock = self._locks.setdefault(account.user_id, asyncio.Lock())
        async with lock:
            session = await self.authenticator.login(account, mpin, totp)
            self._sessions[account.user_id] = session
            if self.persist:
                await self.persist(session)
            return session

    def current(self, user_id: str) -> BrokerSession | None:
        return self._sessions.get(user_id)

    async def refresh(self, account: AccountContext) -> BrokerSession:
        lock = self._locks.setdefault(account.user_id, asyncio.Lock())
        async with lock:
            tokens = await self.authenticator.refresh(account)
            session = BrokerSession(account.user_id, account.client_code, tokens.jwt_token, tokens.refresh_token, tokens.feed_token, datetime.now(UTC), account.credential_revision + 1)
            self._sessions[account.user_id] = session
            if self.persist:
                await self.persist(session)
            return session
