"""Isolated worker supervision and database-backed scheduler leadership."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Callable
from dataclasses import asdict, dataclass
from datetime import UTC, datetime, timedelta
from enum import StrEnum

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncEngine

from app.strategy.runtime import SchedulerHealth

logger = logging.getLogger(__name__)


class RuntimeMode(StrEnum):
    OFF = "off"
    SHADOW = "shadow"
    AUTHORITATIVE = "authoritative"


@dataclass
class WorkerHealth:
    role: str
    critical: bool
    interval_seconds: float
    timeout_seconds: float
    running: bool = False
    last_started_at: datetime | None = None
    last_success_at: datetime | None = None
    last_error_at: datetime | None = None
    last_error: str = ""
    success_count: int = 0
    error_count: int = 0
    progress: dict[str, object] | None = None

    def stale(self, now: datetime | None = None) -> bool:
        observed = now or datetime.now(UTC)
        allowance = max(self.interval_seconds * 3, self.timeout_seconds * 2, 15)
        return bool(
            not self.running
            or self.last_success_at is None
            or observed - self.last_success_at > timedelta(seconds=allowance)
        )

    def json(self, now: datetime | None = None) -> dict[str, object]:
        value = asdict(self)
        value["stale"] = self.stale(now)
        value["healthy"] = not value["stale"] and not self.last_error
        return value


class WorkerSupervisor:
    """Runs independent bounded loops; one failure never kills sibling workers."""

    def __init__(self) -> None:
        self._stop = asyncio.Event()
        self._tasks: dict[str, asyncio.Task[None]] = {}
        self._health: dict[str, WorkerHealth] = {}

    def start(
        self,
        role: str,
        callback: Callable[[], Awaitable[dict[str, object] | None]],
        *,
        interval_seconds: float,
        timeout_seconds: float,
        critical: bool = True,
    ) -> asyncio.Task[None]:
        existing = self._tasks.get(role)
        if existing is not None and not existing.done():
            return existing
        if interval_seconds <= 0 or timeout_seconds <= 0:
            raise ValueError("Worker intervals and timeouts must be positive.")
        health = WorkerHealth(role, critical, interval_seconds, timeout_seconds)
        self._health[role] = health
        self._stop.clear()

        async def loop() -> None:
            health.running = True
            try:
                while not self._stop.is_set():
                    health.last_started_at = datetime.now(UTC)
                    try:
                        progress = await asyncio.wait_for(callback(), timeout=timeout_seconds)
                    except asyncio.CancelledError:
                        raise
                    except Exception as exc:
                        health.error_count += 1
                        health.last_error_at = datetime.now(UTC)
                        health.last_error = f"{type(exc).__name__}: {exc}"[:1000]
                        logger.exception("%s worker cycle failed", role)
                    else:
                        health.success_count += 1
                        health.last_success_at = datetime.now(UTC)
                        health.last_error = ""
                        health.progress = progress or {}
                    try:
                        await asyncio.wait_for(self._stop.wait(), timeout=interval_seconds)
                    except TimeoutError:
                        continue
            finally:
                health.running = False

        task = asyncio.create_task(loop(), name=f"rulenix:{role}")
        self._tasks[role] = task
        return task

    async def stop(self) -> None:
        self._stop.set()
        tasks = tuple(self._tasks.values())
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        self._tasks.clear()

    def snapshots(self) -> dict[str, dict[str, object]]:
        now = datetime.now(UTC)
        return {role: health.json(now) for role, health in sorted(self._health.items())}

    def ready(self) -> bool:
        now = datetime.now(UTC)
        return all(not item.stale(now) and not item.last_error for item in self._health.values() if item.critical)

    @property
    def active_roles(self) -> tuple[str, ...]:
        return tuple(role for role, task in self._tasks.items() if not task.done())


class DatabaseLeaderScheduler:
    """Own a session advisory lock and advance only while that connection lives."""

    def __init__(
        self,
        engine: AsyncEngine,
        health: SchedulerHealth,
        mode: RuntimeMode,
        tick: Callable[[], Awaitable[dict[str, object] | None]],
        *,
        interval_seconds: float = 5,
    ) -> None:
        self.engine = engine
        self.health = health
        self.mode = mode
        self.tick = tick
        self.interval_seconds = interval_seconds
        self._stop = asyncio.Event()
        self._task: asyncio.Task[None] | None = None

    @property
    def lock_name(self) -> str:
        return (
            "rulenix:strategy_scheduler"
            if self.mode is RuntimeMode.AUTHORITATIVE
            else "rulenix:python-shadow-scheduler"
        )

    def start(self) -> asyncio.Task[None]:
        if self._task is not None and not self._task.done():
            return self._task
        self._stop.clear()
        self._task = asyncio.create_task(self._run(), name="rulenix:database-leader-scheduler")
        return self._task

    async def stop(self) -> None:
        self._stop.set()
        if self._task is not None:
            self._task.cancel()
            await asyncio.gather(self._task, return_exceptions=True)
        self._task = None
        self.health.leadership_lost()

    async def _run(self) -> None:
        while not self._stop.is_set():
            self.health.leadership_lost()
            try:
                async with self.engine.connect() as connection:
                    acquired = bool(
                        await connection.scalar(
                            text("SELECT pg_try_advisory_lock(hashtext(:name))"),
                            {"name": self.lock_name},
                        )
                    )
                    if not acquired:
                        await self._wait(5)
                        continue
                    self.health.leadership_acquired()
                    while not self._stop.is_set():
                        alive = await connection.scalar(text("SELECT 1"))
                        if alive != 1:
                            raise RuntimeError("scheduler leadership connection is unhealthy")
                        self.health.record_advance()
                        self.health.record_dispatch()
                        try:
                            await asyncio.wait_for(self.tick(), timeout=max(2, self.interval_seconds * 2))
                        except Exception:
                            self.health.record_error()
                            raise
                        self.health.record_success()
                        await self._wait(self.interval_seconds)
            except asyncio.CancelledError:
                raise
            except Exception:
                self.health.record_error()
                logger.exception("database scheduler leader loop failed; re-election pending")
                await self._wait(5)
            finally:
                self.health.leadership_lost()

    async def _wait(self, seconds: float) -> None:
        try:
            await asyncio.wait_for(self._stop.wait(), timeout=seconds)
        except TimeoutError:
            pass


__all__ = [
    "DatabaseLeaderScheduler",
    "RuntimeMode",
    "WorkerHealth",
    "WorkerSupervisor",
]
