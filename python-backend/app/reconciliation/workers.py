from __future__ import annotations

import asyncio
import logging
from collections.abc import Callable
from dataclasses import dataclass
from datetime import datetime, timezone
from uuid import UUID

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from .repository import ReconciliationRepository

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class WorkerRole:
    name: str
    cadence_seconds: int
    account_scoped: bool
    broker_reads: tuple[str, ...] = ()
    mutations: tuple[str, ...] = ()


RUST_WORKER_ROLES: tuple[WorkerRole, ...] = (
    WorkerRole("strategy_scheduler", 5, False, ("positions", "orders", "trades", "conditional"), ("place/cancel guarded",)),
    WorkerRole("broker_reconciliation", 5, True, ("positions", "orders", "trades", "individual order", "conditional"), ("none in Python" ,)),
    WorkerRole("protection_recovery", 5, True, ("positions", "orders", "trades"), ("place/cancel guarded",)),
    WorkerRole("execution_intent_recovery", 5, True, ("orders", "trades"), ("place guarded",)),
    WorkerRole("sl2_reversal_recovery", 5, True, ("positions", "orders", "trades"), ("place guarded",)),
    WorkerRole("manual_close_recovery", 5, True, ("positions", "orders", "trades", "individual order"), ("close/cancel guarded",)),
    WorkerRole("square_off_recovery", 5, True, ("positions", "orders", "trades"), ("close guarded",)),
    WorkerRole("market_feed", 5, True, ("websocket ticks",), ()),
    WorkerRole("session_maintenance", 1800, True, ("session validation",), ("refresh guarded" ,)),
    WorkerRole("session_cleanup", 3600, False, (), ()),
    WorkerRole("notifications", 60, True, (), ()),
    WorkerRole("otp_cleanup", 86400, False, (), ()),
    WorkerRole("admin_job_runner", 0, False, (), ()),
)


@dataclass(frozen=True)
class WorkerRun:
    role: str
    acquired: bool
    recovered: dict[str, int]


class RecoveryWorker:
    """Bounded, one-tick worker.  The caller owns its lifecycle."""

    def __init__(self, session_factory: async_sessionmaker[AsyncSession], *, role: str = "broker_reconciliation", age_seconds: int = 120, clock: Callable[[], datetime] | None = None):
        self.session_factory = session_factory
        self.role = role
        self.age_seconds = age_seconds
        self.clock = clock or (lambda: datetime.now(timezone.utc))

    async def run_once(self, *, user_id: UUID | None = None) -> WorkerRun:
        async with self.session_factory() as session, session.begin():
                if user_id is None:
                    acquired = bool((await session.execute(text("SELECT pg_try_advisory_xact_lock(hashtext(:role))"), {"role": f"rulenix:{self.role}"})).scalar())
                else:
                    acquired = bool((await session.execute(text("SELECT pg_try_advisory_xact_lock(hashtextextended(:user,0))"), {"user": str(user_id)})).scalar())
                if not acquired:
                    return WorkerRun(self.role, False, {})
                recovered = await ReconciliationRepository(session).recover_stale_work(age_seconds=self.age_seconds)
                return WorkerRun(self.role, True, recovered)

    async def run(self, stop: asyncio.Event, *, interval_seconds: int = 5, user_id: UUID | None = None) -> None:
        while not stop.is_set():
            try:
                await self.run_once(user_id=user_id)
            except asyncio.CancelledError:
                raise
            except Exception:
                logger.exception("%s worker tick failed", self.role)
            try:
                await asyncio.wait_for(stop.wait(), timeout=interval_seconds)
            except TimeoutError:
                continue


class BackgroundWorkerManager:
    """Owns worker tasks and makes repeated start/stop calls idempotent."""

    def __init__(self):
        self._stop = asyncio.Event()
        self._tasks: dict[str, asyncio.Task[None]] = {}

    def start(self, role: str, worker: RecoveryWorker, *, interval_seconds: int = 5, user_id: UUID | None = None) -> asyncio.Task[None]:
        existing = self._tasks.get(role)
        if existing and not existing.done():
            return existing
        self._stop.clear()
        task = asyncio.create_task(worker.run(self._stop, interval_seconds=interval_seconds, user_id=user_id), name=f"rulenix:{role}")
        self._tasks[role] = task
        return task

    async def stop(self) -> None:
        self._stop.set()
        tasks = tuple(self._tasks.values())
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        self._tasks.clear()

    @property
    def active_roles(self) -> tuple[str, ...]:
        return tuple(role for role, task in self._tasks.items() if not task.done())


__all__ = ["RUST_WORKER_ROLES", "BackgroundWorkerManager", "RecoveryWorker", "WorkerRole", "WorkerRun"]
