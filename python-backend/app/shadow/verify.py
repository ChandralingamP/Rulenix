"""One-shot runtime proof of the production shadow permission boundary."""

from __future__ import annotations

import asyncio
import json
from datetime import UTC, datetime

import asyncpg  # type: ignore[import-untyped]

from .config import ShadowSettings
from .database import ShadowWriter, SourceReader, create_pools
from .models import Observation


async def verify(settings: ShadowSettings) -> dict[str, object]:
    source_pool, sink_pool = await create_pools(settings)
    source = SourceReader(source_pool, settings.pseudonym_key)
    sink = ShadowWriter(sink_pool, settings.release)
    denied: list[str] = []
    try:
        source_proof = await source.prove_boundary()
        sink_proof = await sink.prove_boundary()
        async with source_pool.acquire() as connection:
            try:
                await connection.execute("INSERT INTO public.strategy_signals DEFAULT VALUES")
            except (asyncpg.InsufficientPrivilegeError, asyncpg.ReadOnlySQLTransactionError):
                denied.append("reader_authoritative_insert")
            else:
                raise RuntimeError("reader authoritative INSERT was not denied")
        async with sink_pool.acquire() as connection:
            for name, statement in (
                ("writer_authoritative_insert", "INSERT INTO public.strategy_signals DEFAULT VALUES"),
                ("writer_authoritative_update", "UPDATE public.strategy_orders SET status=status WHERE FALSE"),
                ("writer_authoritative_delete", "DELETE FROM public.trades WHERE FALSE"),
            ):
                try:
                    await connection.execute(statement)
                except asyncpg.InsufficientPrivilegeError:
                    denied.append(name)
                else:
                    raise RuntimeError(f"{name} was not denied")
        probe_id = f"permission-proof:{settings.release}:{datetime.now(UTC).isoformat()}"
        shadow_write = await sink.store(
            Observation(
                "permission_proof",
                probe_id,
                datetime.now(UTC),
                None,
                "permission_boundary",
                "",
                "0" * 64,
                source_proof,
                sink_proof,
                "MATCH",
                "",
                "NONE",
                0.0,
            )
        )
        if not shadow_write:
            raise RuntimeError("shadow permission proof row was not inserted")
        return {
            "source_identity": source_proof["identity"],
            "sink_identity": sink_proof["identity"],
            "authoritative_writes_denied": denied,
            "shadow_write": "WORKING",
            "status": "PASS",
        }
    finally:
        await source_pool.close()
        await sink_pool.close()


if __name__ == "__main__":
    print(json.dumps(asyncio.run(verify(ShadowSettings.from_environment())), sort_keys=True))
