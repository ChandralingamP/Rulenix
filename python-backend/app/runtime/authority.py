"""Process lifecycle for observing or renewing the Python mutation lease."""

from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass
from datetime import UTC, datetime
from uuid import UUID

from app.broker.authority import AuthorityRuntime, LiveMutationAuthority

from .supervisor import RuntimeMode

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class AuthorityLifecycleSnapshot:
    mode: RuntimeMode
    mutation_allowed: bool
    observed_holder: AuthorityRuntime | None
    observed_epoch: int | None
    lease_owner: UUID | None
    lease_expires_at: datetime | None
    last_observed_at: datetime | None
    lost_reason: str


class AuthorityLeaseLifecycle:
    """Adopts only an already-transferred lease; it never transfers authority."""

    def __init__(
        self,
        authority: LiveMutationAuthority,
        mode: RuntimeMode,
        lease_owner: UUID | None,
        *,
        lease_seconds: int = 30,
        observe_seconds: float = 5,
    ) -> None:
        if not 5 <= lease_seconds <= 300:
            raise ValueError("Authority lease must be between 5 and 300 seconds.")
        if mode is RuntimeMode.AUTHORITATIVE and lease_owner is None:
            raise ValueError("Authoritative runtime requires an explicit lease owner.")
        self.authority = authority
        self.mode = mode
        self.lease_owner = lease_owner
        self.lease_seconds = lease_seconds
        self.observe_seconds = observe_seconds
        self._allowed = False
        self._epoch: int | None = None
        self._observed_holder: AuthorityRuntime | None = None
        self._expires_at: datetime | None = None
        self._last_observed_at: datetime | None = None
        self._lost_reason = ""
        self._stop = asyncio.Event()
        self._task: asyncio.Task[None] | None = None

    def mutation_allowed(self) -> bool:
        return self._allowed

    def snapshot(self) -> AuthorityLifecycleSnapshot:
        return AuthorityLifecycleSnapshot(
            self.mode,
            self._allowed,
            self._observed_holder,
            self._epoch,
            self.lease_owner,
            self._expires_at,
            self._last_observed_at,
            self._lost_reason,
        )

    async def start(self) -> None:
        if self.mode is RuntimeMode.OFF:
            return
        state = await self.authority.current()
        self._observe(state)
        if self.mode is RuntimeMode.AUTHORITATIVE:
            if state.holder is not AuthorityRuntime.PYTHON or state.lease_owner != self.lease_owner:
                raise RuntimeError(
                    "Python authority was not explicitly transferred to this process; startup refused."
                )
            if state.lease_expires_at <= datetime.now(UTC):
                raise RuntimeError("The transferred Python authority lease is already expired.")
            self._epoch = state.epoch
            self._allowed = True
        self._stop.clear()
        self._task = asyncio.create_task(self._run(), name="rulenix:authority-lease")

    async def stop(self) -> None:
        self._allowed = False
        self._stop.set()
        if self._task is not None:
            self._task.cancel()
            await asyncio.gather(self._task, return_exceptions=True)
        self._task = None

    async def _run(self) -> None:
        interval = min(self.observe_seconds, max(1.0, self.lease_seconds / 3))
        while not self._stop.is_set():
            try:
                if self.mode is RuntimeMode.AUTHORITATIVE:
                    if not self._allowed or self._epoch is None or self.lease_owner is None:
                        return
                    state = await self.authority.renew_python(
                        epoch=self._epoch,
                        lease_owner=self.lease_owner,
                        lease_seconds=self.lease_seconds,
                    )
                else:
                    state = await self.authority.current()
                self._observe(state)
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                self._allowed = False
                self._lost_reason = f"{type(exc).__name__}: {exc}"[:1000]
                logger.exception("authority lease observation/renewal failed closed")
                if self.mode is RuntimeMode.AUTHORITATIVE:
                    return
            try:
                await asyncio.wait_for(self._stop.wait(), timeout=interval)
            except TimeoutError:
                continue

    def _observe(self, state) -> None:
        self._observed_holder = state.holder
        self._epoch = state.epoch
        self._expires_at = state.lease_expires_at
        self._last_observed_at = datetime.now(UTC)
        self._lost_reason = ""


__all__ = ["AuthorityLeaseLifecycle", "AuthorityLifecycleSnapshot"]
