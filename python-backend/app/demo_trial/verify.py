"""One-shot production permission proof for the Phase 12 image."""

from __future__ import annotations

import asyncio
import json

import asyncpg  # type: ignore[import-untyped]

from .config import DemoTrialSettings
from .database import ProductionReader, TrialRepository, create_pools


async def verify(settings: DemoTrialSettings) -> dict[str, object]:
    source_pool, sink_pool = await create_pools(settings)
    reader, repository = ProductionReader(source_pool), TrialRepository(sink_pool, settings.release)
    denied: list[str] = []
    try:
        await reader.prove_boundary()
        await repository.prove_boundary()
        async with source_pool.acquire() as connection:
            try:
                await connection.execute("INSERT INTO public.trades DEFAULT VALUES")
            except (asyncpg.InsufficientPrivilegeError, asyncpg.ReadOnlySQLTransactionError):
                denied.append("reader_authoritative_insert")
        async with sink_pool.acquire() as connection:
            for label, statement in (
                ("writer_authoritative_insert", "INSERT INTO public.strategy_signals DEFAULT VALUES"),
                ("writer_authoritative_update", "UPDATE public.strategy_orders SET status=status WHERE FALSE"),
                ("writer_authoritative_delete", "DELETE FROM public.trades WHERE FALSE"),
            ):
                try:
                    await connection.execute(statement)
                except asyncpg.InsufficientPrivilegeError:
                    denied.append(label)
        if len(denied) != 4:
            raise RuntimeError(f"authoritative write denial proof failed: {denied}")
        await repository.heartbeat(
            healthy=True,
            detail="Phase 12 permission proof",
            counts={"cycles": 0, "matches": 0, "mismatches": 0, "errors": 0},
        )
        return {
            "status": "PASS",
            "source_identity": "rulenix_demo_trial_reader",
            "sink_identity": "rulenix_demo_trial_writer",
            "authoritative_writes_denied": denied,
            "isolated_trial_write": "WORKING",
        }
    finally:
        await source_pool.close()
        await sink_pool.close()


if __name__ == "__main__":
    print(json.dumps(asyncio.run(verify(DemoTrialSettings.from_environment())), sort_keys=True))
