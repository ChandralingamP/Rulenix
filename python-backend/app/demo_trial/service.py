"""Leader-owned Phase 12 DEMO trial service and internal health endpoint."""

from __future__ import annotations

import asyncio
import json
import logging
import signal
from datetime import UTC, datetime
from decimal import Decimal
from pathlib import Path
from typing import Any

from .config import DemoTrialSettings
from .database import ProductionReader, TrialRepository, create_pools
from .engine import compare_decisions, evaluate_scenario
from .models import DemoScenario, ExitKind

logger = logging.getLogger(__name__)
FIXTURE = Path(__file__).with_name("phase12_scenarios.json")
TRIAL_ACCOUNTS = ("phase12-demo-a", "phase12-demo-b")


def load_scenarios() -> list[tuple[DemoScenario, dict[str, Any]]]:
    rows = json.loads(FIXTURE.read_text(encoding="utf-8"))
    result = []
    for row in rows:
        values = dict(row)
        oracle = values.pop("oracle")
        for name in ("entry", "target", "sl1", "sl2", "exit_price", "reversal_exit_price"):
            if values.get(name) is not None:
                values[name] = Decimal(values[name])
        values["exit_kind"] = ExitKind(values["exit_kind"])
        result.append((DemoScenario(**values), oracle))
    return result


class DemoTrialService:
    def __init__(self, settings: DemoTrialSettings):
        self.settings = settings
        self.stop = asyncio.Event()
        self.started_at = datetime.now(UTC)
        self.last_success_at: datetime | None = None
        self.last_error = ""
        self.leader = False
        self.last_advance_at: datetime | None = None
        self.counts = {
            "cycles": 0, "matches": 0, "mismatches": 0, "errors": 0,
            "poll_errors": 0, "real_observations": 0, "replay_observations": 0,
        }

    def ready(self) -> bool:
        return bool(
            self.leader and not self.last_error and self.last_success_at
            and (datetime.now(UTC) - self.last_success_at).total_seconds()
            <= self.settings.poll_seconds * 3
        )

    async def _execute(
        self,
        repository: TrialRepository,
        *,
        scenario: DemoScenario,
        oracle: dict[str, Any],
        account_ref: str,
        source_kind: str,
        cycle_key: str,
        scheduled_for: datetime,
    ) -> None:
        cycle_id = await repository.claim(
            cycle_key=cycle_key,
            source_kind=source_kind,
            account_ref=account_ref,
            scenario=scenario,
            oracle=oracle,
            scheduled_for=scheduled_for,
        )
        if cycle_id is None:
            return
        decision = evaluate_scenario(scenario)
        classification, reason = compare_decisions(oracle, decision.json())
        await repository.complete(cycle_id, scenario, decision, classification, reason)
        self.counts["cycles"] += 1
        self.counts["matches" if classification == "MATCH" else "mismatches"] += 1
        self.counts[
            "real_observations" if source_kind == "REAL_PRODUCTION_DEMO_OBSERVATION"
            else "replay_observations"
        ] += 1

    async def poll(self, reader: ProductionReader, repository: TrialRepository) -> None:
        self.last_advance_at = datetime.now(UTC)
        await repository.recover_stale()
        market = await reader.current_market_state()
        for scenario, oracle in load_scenarios():
            for account_ref in TRIAL_ACCOUNTS:
                await self._execute(
                    repository,
                    scenario=scenario,
                    oracle=oracle,
                    account_ref=account_ref,
                    source_kind="DETERMINISTIC_PRODUCTION_DERIVED_REPLAY",
                    cycle_key=(
                        f"replay:{self.settings.release}:{scenario.scenario_id}:{account_ref}"
                    ),
                    scheduled_for=self.started_at,
                )
        closed = int(market["iso_day"]) >= 6 or (
            not bool(market["morning_open"]) and not bool(market["evening_open"])
        )
        if closed:
            actual = [("futures_breakout_v3", ExitKind.NO_SIGNAL, "no-signal")]
            if int(market["supertrend_eod_signals"]) > 0 and int(market["supertrend_eod_users"]) == 0:
                actual.append(
                    ("supertrend_index_options_v1", ExitKind.EOD_NO_POSITION, "eod-no-position")
                )
            elif (
                int(market["supertrend_eod_signals"]) == 0
                and int(market["supertrend_entry_signals"]) == 0
            ):
                actual.append(("supertrend_index_options_v1", ExitKind.NO_SIGNAL, "no-signal"))
            for strategy, exit_kind, observation_kind in actual:
                instrument = "PRODUCTION_SESSION"
                scenario = DemoScenario(
                    f"real-{market['trade_date']}-{strategy}", strategy, instrument, "BUY", 1,
                    Decimal(1), Decimal(1), Decimal(1), Decimal(1), exit_kind,
                )
                oracle = evaluate_scenario(scenario).json()
                await self._execute(
                    repository,
                    scenario=scenario,
                    oracle=oracle,
                    account_ref="phase12-production-session",
                    source_kind="REAL_PRODUCTION_DEMO_OBSERVATION",
                    cycle_key=(
                        f"real:{self.settings.release}:{market['trade_date']}:"
                        f"{strategy}:{observation_kind}"
                    ),
                    scheduled_for=datetime.now(UTC),
                )
        self.counts.update(await repository.summary())
        self.last_success_at = datetime.now(UTC)
        self.last_error = ""
        await repository.heartbeat(
            healthy=True,
            detail=(
                f"DEMO trial poll complete; production_session_closed={closed}; "
                f"weekend_skips={market['weekend_skips']}; "
                f"futures_signals={market['futures_signals']}; "
                f"supertrend_entry_signals={market['supertrend_entry_signals']}; "
                f"supertrend_eod_signals={market['supertrend_eod_signals']}; "
                f"supertrend_eod_users={market['supertrend_eod_users']}"
            ),
            counts=self.counts,
        )

    async def run(self) -> None:
        source_pool, sink_pool = await create_pools(self.settings)
        reader, repository = ProductionReader(source_pool), TrialRepository(sink_pool, self.settings.release)
        leader_connection = await sink_pool.acquire()
        try:
            await reader.prove_boundary()
            await repository.prove_boundary()
            if not await repository.acquire_leader(leader_connection):
                raise RuntimeError("Python DEMO trial leadership is already owned")
            self.leader = True
            await repository.recover_stale()
            while not self.stop.is_set():
                try:
                    await self.poll(reader, repository)
                except Exception as error:
                    self.last_error = type(error).__name__
                    self.counts["poll_errors"] += 1
                    logger.exception("DEMO trial poll failed")
                    await repository.heartbeat(
                        healthy=False,
                        detail=f"DEMO trial poll failed: {type(error).__name__}",
                        counts=self.counts,
                    )
                try:
                    await asyncio.wait_for(self.stop.wait(), timeout=self.settings.poll_seconds)
                except TimeoutError:
                    pass
        finally:
            if self.leader:
                await repository.release_leader(leader_connection)
            self.leader = False
            await sink_pool.release(leader_connection)
            await source_pool.close()
            await sink_pool.close()


async def _health_handler(
    reader: asyncio.StreamReader, writer: asyncio.StreamWriter, service: DemoTrialService
) -> None:
    try:
        await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), timeout=2)
        body = json.dumps({
            "status": "ready" if service.ready() else "not_ready",
            "mode": "isolated_python_demo_trial",
            "leader": service.leader,
            "last_advance_at": service.last_advance_at.isoformat() if service.last_advance_at else None,
            "last_success_at": service.last_success_at.isoformat() if service.last_success_at else None,
            "last_error": service.last_error,
            "stale": not service.ready(),
            "counts": service.counts,
        }, separators=(",", ":")).encode()
        status = b"200 OK" if service.ready() else b"503 Service Unavailable"
        writer.write(
            b"HTTP/1.1 " + status + b"\r\nContent-Type: application/json\r\nContent-Length: "
            + str(len(body)).encode() + b"\r\nConnection: close\r\n\r\n" + body
        )
        await writer.drain()
    finally:
        writer.close()
        await writer.wait_closed()


async def serve(settings: DemoTrialSettings) -> None:
    service = DemoTrialService(settings)
    loop = asyncio.get_running_loop()
    for name in ("SIGINT", "SIGTERM"):
        if hasattr(signal, name):
            try:
                loop.add_signal_handler(getattr(signal, name), service.stop.set)
            except NotImplementedError:
                pass
    server = await asyncio.start_server(
        lambda reader, writer: _health_handler(reader, writer, service),
        "0.0.0.0", settings.health_port,
    )
    try:
        await service.run()
    finally:
        server.close()
        await server.wait_closed()


__all__ = ["DemoTrialService", "load_scenarios", "serve"]
