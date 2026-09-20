from __future__ import annotations

import os
from datetime import UTC, datetime, timedelta
from uuid import uuid4

import asyncpg
import pytest

from app.demo_trial.database import (
    ProductionReader,
    TrialRepository,
    _sink_init,
    _source_init,
)
from app.demo_trial.engine import compare_decisions, evaluate_scenario
from app.demo_trial.service import load_scenarios


def _database_url() -> str:
    value = os.environ.get("TEST_DATABASE_URL", "")
    if "/rulenix_test" not in value:
        pytest.skip("TEST_DATABASE_URL must point to an isolated PostgreSQL test database")
    if os.environ.get("PHASE12_DEMO_TRIAL_ROLES_READY") != "true":
        pytest.skip("Phase 12 trial roles must be explicitly provisioned")
    return value.replace("postgresql+asyncpg://", "postgresql://", 1)


def _role_url(database_url: str, role: str, password: str) -> str:
    _, location = database_url.split("://", 1)
    _, host = location.split("@", 1)
    return f"postgresql://{role}:{password}@{host}"


@pytest.mark.asyncio
async def test_trial_permissions_idempotency_restart_and_account_isolation() -> None:
    database_url = _database_url()
    reader_url = _role_url(database_url, "rulenix_demo_trial_reader", "phase12-local-reader")
    writer_url = _role_url(database_url, "rulenix_demo_trial_writer", "phase12-local-writer")
    source_pool = await asyncpg.create_pool(reader_url, min_size=1, max_size=1, init=_source_init)
    sink_pool = await asyncpg.create_pool(writer_url, min_size=1, max_size=2, init=_sink_init)
    reader, repository = ProductionReader(source_pool), TrialRepository(sink_pool, "phase12-test")
    token = uuid4().hex
    scenario, oracle = load_scenarios()[0]
    now = datetime.now(UTC)
    try:
        await reader.prove_boundary()
        await repository.prove_boundary()
        assert "trade_date" in await reader.current_market_state()
        leader_one = await sink_pool.acquire()
        leader_two = await sink_pool.acquire()
        try:
            assert await repository.acquire_leader(leader_one)
            assert not await repository.acquire_leader(leader_two)
            await repository.release_leader(leader_one)
            assert await repository.acquire_leader(leader_two)
            await repository.release_leader(leader_two)
        finally:
            await sink_pool.release(leader_one)
            await sink_pool.release(leader_two)
        async with source_pool.acquire() as connection:
            with pytest.raises((asyncpg.InsufficientPrivilegeError, asyncpg.ReadOnlySQLTransactionError)):
                await connection.execute("INSERT INTO public.trades DEFAULT VALUES")
        async with sink_pool.acquire() as connection:
            for statement in (
                "INSERT INTO public.strategy_signals DEFAULT VALUES",
                "UPDATE public.strategy_orders SET status=status WHERE FALSE",
                "DELETE FROM public.trades WHERE FALSE",
            ):
                with pytest.raises(asyncpg.InsufficientPrivilegeError):
                    await connection.execute(statement)

        cycle_id = await repository.claim(
            cycle_key=f"postgres:{token}:a", source_kind="TEST", account_ref=f"a-{token}",
            scenario=scenario, oracle=oracle, scheduled_for=now,
        )
        assert cycle_id is not None
        async with sink_pool.acquire() as connection:
            await connection.execute(
                "UPDATE rulenix_demo_trial.cycles SET claimed_at=$2 WHERE id=$1",
                cycle_id, now - timedelta(minutes=2),
            )
        assert await repository.recover_stale() >= 1
        reclaimed = await repository.claim(
            cycle_key=f"postgres:{token}:a", source_kind="TEST", account_ref=f"a-{token}",
            scenario=scenario, oracle=oracle, scheduled_for=now,
        )
        assert reclaimed == cycle_id
        decision = evaluate_scenario(scenario)
        classification, reason = compare_decisions(oracle, decision.json())
        await repository.complete(cycle_id, scenario, decision, classification, reason)
        assert await repository.claim(
            cycle_key=f"postgres:{token}:a", source_kind="TEST", account_ref=f"a-{token}",
            scenario=scenario, oracle=oracle, scheduled_for=now,
        ) is None

        second = await repository.claim(
            cycle_key=f"postgres:{token}:b", source_kind="TEST", account_ref=f"b-{token}",
            scenario=scenario, oracle=oracle, scheduled_for=now,
        )
        assert second is not None
        await repository.complete(second, scenario, decision, classification, reason)
        async with sink_pool.acquire() as connection:
            counts = await connection.fetchrow("""
                SELECT COUNT(DISTINCT account_ref) AS accounts,
                       COUNT(*) FILTER(WHERE parity_classification='MATCH') AS matches
                  FROM rulenix_demo_trial.cycles WHERE cycle_key LIKE $1
            """, f"postgres:{token}:%")
            assert dict(counts or {}) == {"accounts": 2, "matches": 2}

        reversal, reversal_oracle = load_scenarios()[2]
        reversal_id = await repository.claim(
            cycle_key=f"postgres:{token}:reversal", source_kind="TEST",
            account_ref=f"a-{token}", scenario=reversal,
            oracle=reversal_oracle, scheduled_for=now,
        )
        assert reversal_id is not None
        reversal_decision = evaluate_scenario(reversal)
        classification, reason = compare_decisions(reversal_oracle, reversal_decision.json())
        await repository.complete(
            reversal_id, reversal, reversal_decision, classification, reason
        )
        async with sink_pool.acquire() as connection:
            lifecycle = await connection.fetchrow("""
                SELECT
                  (SELECT COUNT(*) FROM rulenix_demo_trial.signals WHERE cycle_id=$1) AS signals,
                  (SELECT COUNT(*) FROM rulenix_demo_trial.intents WHERE cycle_id=$1) AS intents,
                  (SELECT COUNT(*) FROM rulenix_demo_trial.orders WHERE cycle_id=$1) AS orders,
                  (SELECT COUNT(*) FROM rulenix_demo_trial.trades WHERE cycle_id=$1) AS trades,
                  (SELECT COUNT(*) FROM rulenix_demo_trial.trades WHERE cycle_id=$1
                    AND lineage='reversal' AND side='SELL' AND pnl=14.55) AS reversal
            """, reversal_id)
            assert dict(lifecycle or {}) == {
                "signals": 1, "intents": 4, "orders": 8, "trades": 2, "reversal": 1,
            }
    finally:
        await source_pool.close()
        await sink_pool.close()
