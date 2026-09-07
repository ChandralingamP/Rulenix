"""Durable scheduler-run idempotency and restart-safe strategy dispatch."""

from datetime import date, datetime
from uuid import uuid4

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession


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


__all__ = ["StrategyScheduler"]
