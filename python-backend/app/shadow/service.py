"""Lifecycle-owned production shadow poller and internal health endpoint."""

from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import signal
from dataclasses import dataclass, field
from datetime import UTC, datetime, timedelta
from time import perf_counter
from typing import Any

from app.strategy.common import supertrend_eod_due

from .config import ShadowSettings
from .database import ShadowWriter, SourceReader, create_pools
from .evaluate import evaluate_futures_signal, evaluate_readiness, evaluate_supertrend_signal
from .models import Observation

logger = logging.getLogger(__name__)


@dataclass
class HealthState:
    started_at: datetime = field(default_factory=lambda: datetime.now(UTC))
    last_success_at: datetime | None = None
    last_error: str = ""
    consecutive_failures: int = 0
    counts: dict[str, int] = field(
        default_factory=lambda: {"decisions": 0, "matches": 0, "mismatches": 0, "errors": 0}
    )

    def ready(self, poll_seconds: int) -> bool:
        return bool(
            self.last_success_at
            and self.last_success_at >= datetime.now(UTC).replace(microsecond=0)
            - timedelta(seconds=poll_seconds * 3)
        )


def _input_version(row: dict[str, Any]) -> str:
    material = "|".join(
        str(row.get(key) or "")
        for key in ("source_id", "snapshot_id", "execution_key", "signal_at", "checked_at")
    )
    return hashlib.sha256(material.encode()).hexdigest()


def _square_off(row: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any], str, str, str]:
    intents = row.get("intents") or []
    rust = {
        "eligible": bool(intents),
        "timing": row["signal_at"].astimezone(UTC).isoformat(),
        "actions": [item.get("role") for item in intents],
    }
    python = {
        "eligible": supertrend_eod_due(row["signal_at"]),
        "timing": row["signal_at"].astimezone(UTC).isoformat(),
        "actions": ["EMERGENCY_CLOSE" for _ in intents],
    }
    mismatches = [key for key in rust if rust[key] != python[key]]
    return rust, python, ("MATCH" if not mismatches else "MISMATCH"), (
        "" if not mismatches else "fields: " + ",".join(mismatches)
    ), ("NONE" if not mismatches else "HIGH")


class ShadowObserver:
    def __init__(self, settings: ShadowSettings):
        self.settings = settings
        self.health = HealthState()
        self.stop = asyncio.Event()
        self._seen: set[tuple[str, str]] = set()

    def _remember(self, source_kind: str, source_id: str) -> None:
        # The database uniqueness constraint is the durable deduplication boundary.
        # This bounded process-local cache avoids repeating candle reads and strategy
        # calculations for the same recent rows on every poll.
        if len(self._seen) >= 10_000:
            self._seen.clear()
        self._seen.add((source_kind, source_id))

    async def _observe_signal(
        self, source: SourceReader, sink: ShadowWriter, row: dict[str, Any]
    ) -> None:
        source_id = str(row["source_id"])
        if ("strategy_signal", source_id) in self._seen:
            return
        started = perf_counter()
        try:
            if row["strategy_key"] == "futures_breakout_v3":
                result = evaluate_futures_signal(row)
            elif row["signal_type"] == "SQUARE_OFF":
                result = _square_off(row)
            else:
                row["candles"] = await source.candles(row["instrument"], row["signal_at"])
                result = evaluate_supertrend_signal(row)
            rust, python, classification, reason, severity = result
            failure = None
        except Exception as error:
            rust, python = {}, {}
            classification, reason, severity = "ERROR", "shadow evaluation failed", "HIGH"
            failure = type(error).__name__
            logger.exception("shadow evaluation failed source=%s", row["source_id"])
        observation = Observation(
            "strategy_signal",
            source_id,
            datetime.now(UTC),
            None,
            row["strategy_key"],
            row["instrument"],
            _input_version(row),
            rust,
            python,
            classification,
            reason,
            severity,
            (perf_counter() - started) * 1000,
            None,
            failure,
            row.get("source_created_at"),
        )
        inserted = await sink.store(observation)
        self._remember("strategy_signal", source_id)
        if inserted:
            self.health.counts["decisions"] += 1
            key = {
                "MATCH": "matches",
                "MISMATCH": "mismatches",
                "ERROR": "errors",
            }[classification]
            self.health.counts[key] += 1

    async def _observe_readiness(self, sink: ShadowWriter, row: dict[str, Any]) -> None:
        started = perf_counter()
        rust, python, classification, reason, severity = evaluate_readiness(
            row, datetime.now(UTC)
        )
        source_id = f"{row['account_ref']}:{row['checked_at'].astimezone(UTC).isoformat()}"
        if ("readiness", source_id) in self._seen:
            return
        observation = Observation(
            "readiness",
            source_id,
            datetime.now(UTC),
            row["account_ref"],
            "platform_readiness",
            "",
            _input_version(row),
            rust,
            python,
            classification,
            reason,
            severity,
            (perf_counter() - started) * 1000,
            source_created_at=row["checked_at"],
        )
        inserted = await sink.store(observation)
        self._remember("readiness", source_id)
        if inserted:
            self.health.counts["decisions"] += 1
            self.health.counts[
                "matches" if classification == "MATCH" else "mismatches"
            ] += 1

    async def poll(self, source: SourceReader, sink: ShadowWriter) -> None:
        for row in await source.signals(self.settings.lookback_hours, self.settings.batch_size):
            await self._observe_signal(source, sink, row)
        for row in await source.readiness():
            await self._observe_readiness(sink, row)
        self.health.last_success_at = datetime.now(UTC)
        self.health.last_error = ""
        await sink.heartbeat(healthy=True, detail="shadow observation poll completed", counts=self.health.counts)

    async def run(self) -> None:
        source_pool, sink_pool = await create_pools(self.settings)
        source, sink = (
            SourceReader(source_pool, self.settings.pseudonym_key),
            ShadowWriter(sink_pool, self.settings.release),
        )
        try:
            await source.prove_boundary()
            await sink.prove_boundary()
            while not self.stop.is_set():
                try:
                    await self.poll(source, sink)
                    self.health.consecutive_failures = 0
                except Exception as error:
                    self.health.last_error = type(error).__name__
                    self.health.consecutive_failures += 1
                    self.health.counts["errors"] += 1
                    logger.exception("shadow poll failed")
                    try:
                        await sink.heartbeat(
                            healthy=False,
                            detail=f"shadow poll failed: {type(error).__name__}",
                            counts=self.health.counts,
                        )
                    except Exception:
                        logger.exception("shadow heartbeat write failed")
                    if self.health.consecutive_failures >= 5:
                        raise RuntimeError("shadow observer failed five consecutive polls") from error
                try:
                    await asyncio.wait_for(self.stop.wait(), timeout=self.settings.poll_seconds)
                except TimeoutError:
                    pass
        finally:
            await source_pool.close()
            await sink_pool.close()


async def _health_handler(
    reader: asyncio.StreamReader,
    writer: asyncio.StreamWriter,
    observer: ShadowObserver,
) -> None:
    try:
        await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), timeout=2)
        ready = observer.health.ready(observer.settings.poll_seconds)
        body = json.dumps(
            {
                "status": "ready" if ready else "not_ready",
                "last_success_at": observer.health.last_success_at.isoformat()
                if observer.health.last_success_at
                else None,
                "last_error": observer.health.last_error,
                "counts": observer.health.counts,
                "mode": "shadow_observer_only",
            },
            separators=(",", ":"),
        ).encode()
        status = b"200 OK" if ready else b"503 Service Unavailable"
        writer.write(
            b"HTTP/1.1 "
            + status
            + b"\r\nContent-Type: application/json\r\nContent-Length: "
            + str(len(body)).encode()
            + b"\r\nConnection: close\r\n\r\n"
            + body
        )
        await writer.drain()
    except Exception as error:
        logger.debug("shadow health request failed: %s", type(error).__name__)
    finally:
        writer.close()
        await writer.wait_closed()


async def serve(settings: ShadowSettings) -> None:
    observer = ShadowObserver(settings)
    loop = asyncio.get_running_loop()
    for name in ("SIGINT", "SIGTERM"):
        if hasattr(signal, name):
            try:
                loop.add_signal_handler(getattr(signal, name), observer.stop.set)
            except NotImplementedError:
                pass
    server = await asyncio.start_server(
        lambda reader, writer: _health_handler(reader, writer, observer),
        "0.0.0.0",
        settings.health_port,
        limit=4096,
    )
    async with server:
        task = asyncio.create_task(observer.run(), name="rulenix-python-shadow-observer")
        stop_task = asyncio.create_task(observer.stop.wait(), name="rulenix-python-shadow-stop")
        done, _ = await asyncio.wait((task, stop_task), return_when=asyncio.FIRST_COMPLETED)
        if task in done:
            stop_task.cancel()
            await asyncio.gather(stop_task, return_exceptions=True)
            await task
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)


__all__ = ["HealthState", "ShadowObserver", "serve"]
