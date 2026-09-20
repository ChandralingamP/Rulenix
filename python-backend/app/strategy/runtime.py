"""Durable scheduler-run idempotency and restart-safe strategy dispatch."""

import asyncio
from collections.abc import Awaitable, Callable, Hashable
from dataclasses import dataclass
from datetime import UTC, date, datetime, timedelta
from typing import Any
from uuid import uuid4

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession


@dataclass(frozen=True)
class SchedulerHealthSnapshot:
    leader: bool
    advancing: bool
    stale: bool
    last_advance_at: datetime | None
    last_successful_dispatch_at: datetime | None
    dispatch_count: int
    error_count: int


class SchedulerHealth:
    """In-process liveness state; it never grants or claims DB leadership."""

    def __init__(self) -> None:
        self.leader = False
        self.leader_since: datetime | None = None
        self.last_advance_at: datetime | None = None
        self.last_successful_dispatch_at: datetime | None = None
        self.dispatch_count = 0
        self.error_count = 0

    def leadership_acquired(self, now: datetime | None = None) -> None:
        self.leader = True
        self.leader_since = now or datetime.now(UTC)

    def leadership_lost(self) -> None:
        self.leader = False
        self.leader_since = None

    def record_advance(self, now: datetime | None = None) -> None:
        self.last_advance_at = now or datetime.now(UTC)

    def record_dispatch(self) -> None:
        self.dispatch_count += 1

    def record_success(self, now: datetime | None = None) -> None:
        self.last_successful_dispatch_at = now or datetime.now(UTC)

    def record_error(self) -> None:
        self.error_count += 1

    def snapshot(
        self, *, now: datetime | None = None, stale_after: timedelta = timedelta(seconds=60)
    ) -> SchedulerHealthSnapshot:
        observed_at = now or datetime.now(UTC)
        reference = self.last_advance_at or self.leader_since
        stale = bool(self.leader and reference and observed_at - reference > stale_after)
        return SchedulerHealthSnapshot(
            leader=self.leader,
            advancing=bool(self.leader and self.last_advance_at and not stale),
            stale=stale,
            last_advance_at=self.last_advance_at,
            last_successful_dispatch_at=self.last_successful_dispatch_at,
            dispatch_count=self.dispatch_count,
            error_count=self.error_count,
        )


class SingleFlightDispatcher:
    """Isolate worker failures while preventing concurrent or completed duplicates."""

    def __init__(self, health: SchedulerHealth):
        self.health = health
        self._active: dict[Hashable, asyncio.Task[None]] = {}
        self._completed: set[Hashable] = set()
        self._stopping = False

    def dispatch(
        self,
        key: Hashable,
        work: Callable[[], Awaitable[Any]],
        *,
        timeout_seconds: float,
        completion_key: Hashable | None = None,
    ) -> bool:
        durable_key = completion_key if completion_key is not None else key
        if self._stopping or key in self._active or durable_key in self._completed:
            return False
        self.health.record_dispatch()

        async def run() -> None:
            try:
                await asyncio.wait_for(work(), timeout=timeout_seconds)
            except asyncio.CancelledError:
                raise
            except (Exception, TimeoutError):
                self.health.record_error()
            else:
                self._completed.add(durable_key)
                self.health.record_success()
            finally:
                self._active.pop(key, None)

        self._active[key] = asyncio.create_task(run(), name=f"scheduler-worker-{key}")
        return True

    async def wait_idle(self) -> None:
        if self._active:
            await asyncio.gather(*tuple(self._active.values()), return_exceptions=True)

    async def shutdown(self) -> None:
        self._stopping = True
        tasks = tuple(self._active.values())
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        self.health.leadership_lost()


class StrategyScheduler:
    def __init__(self, session: AsyncSession):
        self.session = session

    async def claim_run(
        self,
        *,
        strategy_key: str,
        instrument: str,
        trade_date: date,
        session_key: str,
        action: str,
        scheduled_for: datetime,
    ) -> bool:
        row = (
            await self.session.execute(
                text("""
            INSERT INTO strategy_scheduler_runs(id,strategy_key,instrument,trade_date,session_key,action,status,scheduled_for,next_attempt_at)
            VALUES(:id,:strategy,:instrument,:date,:session,:action,'running',:scheduled,NOW())
            ON CONFLICT(strategy_key,instrument,trade_date,session_key,action) DO UPDATE
              SET status='running', attempts=strategy_scheduler_runs.attempts+1, started_at=NOW(), updated_at=NOW()
             WHERE strategy_scheduler_runs.status IN ('pending','failed')
            RETURNING id
        """),
                {
                    "id": uuid4(),
                    "strategy": strategy_key,
                    "instrument": instrument,
                    "date": trade_date,
                    "session": session_key,
                    "action": action,
                    "scheduled": scheduled_for,
                },
            )
        ).scalar()
        return row is not None

    async def complete_run(
        self, *, strategy_key: str, instrument: str, trade_date: date, session_key: str, action: str
    ) -> bool:
        result = await self.session.execute(
            text("""
            UPDATE strategy_scheduler_runs SET status='completed',completed_at=NOW(),updated_at=NOW()
             WHERE strategy_key=:strategy AND instrument=:instrument AND trade_date=:date AND session_key=:session AND action=:action AND status='running'
        """),
            {
                "strategy": strategy_key,
                "instrument": instrument,
                "date": trade_date,
                "session": session_key,
                "action": action,
            },
        )
        return bool(getattr(result, "rowcount", 0))

    async def fail_run(
        self,
        *,
        strategy_key: str,
        instrument: str,
        trade_date: date,
        session_key: str,
        action: str,
        error: str,
    ) -> bool:
        result = await self.session.execute(
            text("""
            UPDATE strategy_scheduler_runs SET status='failed',last_error=:error,next_attempt_at=NOW(),updated_at=NOW()
             WHERE strategy_key=:strategy AND instrument=:instrument AND trade_date=:date AND session_key=:session AND action=:action AND status='running'
        """),
            {
                "strategy": strategy_key,
                "instrument": instrument,
                "date": trade_date,
                "session": session_key,
                "action": action,
                "error": error[:2000],
            },
        )
        return bool(getattr(result, "rowcount", 0))


__all__ = [
    "SchedulerHealth",
    "SchedulerHealthSnapshot",
    "SingleFlightDispatcher",
    "StrategyScheduler",
]
