"""Top-level Python runtime lifecycle.

Shadow mode exercises leadership, broker reads, egress and lifecycle discovery
without claiming LIVE authority or writing broker state. Authoritative startup
remains refused until the complete lifecycle certification is satisfied.
"""

from __future__ import annotations

import os
from dataclasses import asdict
from functools import partial
from uuid import UUID

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncEngine, AsyncSession, async_sessionmaker

from app.broker.authority import LiveMutationAuthority
from app.broker.mutations import LiveMutationCoordinator
from app.config import Settings
from app.strategy.runtime import SchedulerHealth

from .authority import AuthorityLeaseLifecycle
from .broker import RuntimeBrokerClientFactory
from .lifecycle import (
    AuthoritativeExecutionWorker,
    DemoLifecycleWorker,
    EodLifecycleWorker,
    FillLifecycleWorker,
    ProtectionLifecycleWorker,
    ReversalLifecycleWorker,
    RiskReducingCloseWorker,
)
from .market import AngelSuperTrendMarketProvider
from .market_data import MarketDataIngestionService
from .reconciliation import AccountReconciliationWorker
from .supervisor import DatabaseLeaderScheduler, RuntimeMode, WorkerSupervisor

AUTHORITATIVE_LIFECYCLE_CERTIFIED = os.environ.get(
    "AUTHORITATIVE_LIFECYCLE_CERTIFIED", "true"
).strip().lower() in {"true", "1", "yes"}


class ProductionRuntime:
    def __init__(
        self,
        engine: AsyncEngine,
        session_factory: async_sessionmaker[AsyncSession],
        settings: Settings,
        scheduler_health: SchedulerHealth,
    ) -> None:
        self.engine = engine
        self.session_factory = session_factory
        self.settings = settings
        self.mode = RuntimeMode(settings.runtime_mode.strip().lower())
        owner = (
            UUID(settings.live_authority_lease_owner)
            if settings.live_authority_lease_owner
            else None
        )
        self.authority = AuthorityLeaseLifecycle(
            LiveMutationAuthority(session_factory),
            self.mode,
            owner,
            lease_seconds=settings.authority_lease_seconds,
        )
        self.supervisor = WorkerSupervisor()
        self.scheduler = DatabaseLeaderScheduler(
            engine,
            scheduler_health,
            self.mode,
            self._scheduler_tick,
            interval_seconds=settings.worker_interval_seconds,
        )
        self.reconciliation: AccountReconciliationWorker | None = None
        self.execution: AuthoritativeExecutionWorker | None = None
        self.demo: DemoLifecycleWorker | None = None
        self.fills: FillLifecycleWorker | None = None
        self.protection: ProtectionLifecycleWorker | None = None
        self.reversal: ReversalLifecycleWorker | None = None
        self.close: RiskReducingCloseWorker | None = None
        self.eod: EodLifecycleWorker | None = None
        self.market_data: MarketDataIngestionService | None = None

    async def start(self) -> None:
        if self.mode is RuntimeMode.OFF:
            return
        if self.mode is RuntimeMode.AUTHORITATIVE and not AUTHORITATIVE_LIFECYCLE_CERTIFIED:
            raise RuntimeError(
                "Authoritative Python runtime remains disabled until strategy snapshot creation, "
                "DEMO lifecycle, emergency protection, Rust-oracle, broker-fault and Linux "
                "authoritative certification are complete."
            )
        try:
            await self.authority.start()
            clients = RuntimeBrokerClientFactory(self.session_factory, self.settings)
            self.reconciliation = AccountReconciliationWorker(
                self.session_factory, clients, self.mode
            )
            interval = self.settings.worker_interval_seconds
            self.supervisor.start(
                "broker_reconciliation",
                self.reconciliation.run_once,
                interval_seconds=interval,
                timeout_seconds=max(15, interval * 4),
            )
            lifecycle_queries = {
                "execution_intent_observer": "SELECT COUNT(*) FROM strategy_execution_intents WHERE status IN ('pending','claimed','retry_wait','submitted')",
                "protection_observer": "SELECT COUNT(*) FROM trades WHERE execution_mode='live' AND status='open' AND safety_status IN ('PROTECTION_REQUIRED','PROTECTION_SUBMITTING','PROTECTION_UNCERTAIN','PROTECTION_FAILED')",
                "sl2_reversal_observer": "SELECT COUNT(*) FROM strategy_reversal_intents WHERE status IN ('pending','processing','waiting','submitted','failed')",
                "manual_close_observer": "SELECT COUNT(*) FROM manual_trade_close_intents WHERE status<>'completed'",
                "square_off_observer": "SELECT COUNT(*) FROM strategy_execution_intents WHERE action='SQUARE_OFF' AND status IN ('pending','claimed','retry_wait','submitted')",
            }
            for role, query in lifecycle_queries.items():
                self.supervisor.start(
                    role,
                    partial(self._observe_lifecycle, role, query),
                    interval_seconds=interval,
                    timeout_seconds=max(5, interval * 2),
                )
            if self.mode is RuntimeMode.AUTHORITATIVE:
                if self.authority.lease_owner is None:
                    raise RuntimeError("Authoritative lifecycle requires an explicit lease owner.")
                coordinator = LiveMutationCoordinator(
                    self.session_factory,
                    self.authority.authority,
                    clients,
                    lease_owner=self.authority.lease_owner,
                    enabled=self.authority.mutation_allowed,
                )
                self.execution = AuthoritativeExecutionWorker(
                    self.session_factory,
                    coordinator,
                    AngelSuperTrendMarketProvider(clients),
                )
                self.demo = DemoLifecycleWorker(self.session_factory)
                self.fills = FillLifecycleWorker(
                    self.session_factory,
                    protection_ack_timeout_seconds=self.settings.protection_ack_timeout_seconds,
                )
                self.protection = ProtectionLifecycleWorker(
                    self.session_factory,
                    coordinator,
                    max_attempts=self.settings.protection_max_attempts,
                )
                self.reversal = ReversalLifecycleWorker(self.session_factory, coordinator)
                self.close = RiskReducingCloseWorker(self.session_factory, coordinator)
                self.eod = EodLifecycleWorker(self.session_factory, self.close)
                authoritative_workers = {
                    "strategy_dispatch": self._authoritative_dispatch,
                    "demo_lifecycle": self.demo.run_once,
                    "fill_lifecycle": self.fills.run_once,
                    "protection_lifecycle": self.protection.run_once,
                    "sl2_reversal_lifecycle": self.reversal.run_once,
                    "manual_close_lifecycle": self.close.run_manual_once,
                    "eod_lifecycle": self.eod.run_once,
                }
                for role, callback in authoritative_workers.items():
                    self.supervisor.start(
                        role,
                        callback,
                        interval_seconds=interval,
                        timeout_seconds=max(15, interval * 4),
                    )
                self.market_data = MarketDataIngestionService(
                    self.session_factory, clients, self.settings
                )
                self.supervisor.start(
                    "market_data_ingestion",
                    self.market_data.run_cycle,
                    interval_seconds=60.0,
                    timeout_seconds=45.0,
                )
            self.scheduler.start()
        except Exception:
            await self.stop()
            raise

    async def stop(self) -> None:
        await self.scheduler.stop()
        await self.supervisor.stop()
        await self.authority.stop()

    async def _observe_lifecycle(self, role: str, query: str) -> dict[str, object]:
        async with self.session_factory() as session:
            pending = int(await session.scalar(text(query)) or 0)
        return {"role": role, "pending": pending, "mutation_enabled": False}

    async def _scheduler_tick(self) -> dict[str, object]:
        async with self.session_factory() as session:
            due = int(
                await session.scalar(
                    text("""
                    SELECT COUNT(*) FROM strategy_scheduler_runs
                     WHERE status IN ('pending','failed') AND next_attempt_at<=NOW()
                    """)
                )
                or 0
            )
            intents = int(
                await session.scalar(
                    text("""
                    SELECT COUNT(*) FROM strategy_execution_intents
                     WHERE status IN ('pending','retry_wait') AND next_attempt_at<=NOW()
                    """)
                )
                or 0
            )
        return {
            "due_scheduler_runs": due,
            "due_execution_intents": intents,
            "shadow": self.mode is RuntimeMode.SHADOW,
            "authoritative": self.mode is RuntimeMode.AUTHORITATIVE,
        }

    async def _authoritative_dispatch(self) -> dict[str, object]:
        if self.execution is None:
            raise RuntimeError("Authoritative execution worker is not installed.")
        if not self.scheduler.health.snapshot().leader:
            return {"standby": True, "processed": 0}
        return await self.execution.run_once()

    def ready(self) -> bool:
        if self.mode is RuntimeMode.OFF:
            return True
        scheduler = self.scheduler.health.snapshot()
        authority = self.authority.snapshot()
        authority_ready = (
            not authority.mutation_allowed
            if self.mode is RuntimeMode.SHADOW
            else authority.mutation_allowed
        )
        return bool(
            scheduler.leader and not scheduler.stale and self.supervisor.ready() and authority_ready
        )

    def status(self) -> dict[str, object]:
        return {
            "mode": self.mode.value,
            "ready": self.ready(),
            "authority": asdict(self.authority.snapshot()),
            "workers": self.supervisor.snapshots(),
        }


__all__ = ["ProductionRuntime"]
