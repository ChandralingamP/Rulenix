"""Strict read-only source and isolated-write persistence for Phase 12."""

from __future__ import annotations

import json
from datetime import datetime
from decimal import Decimal
from typing import Any
from uuid import NAMESPACE_URL, UUID, uuid5

import asyncpg  # type: ignore[import-untyped]

from .config import DemoTrialSettings
from .models import DemoDecision, DemoScenario


async def _source_init(connection: asyncpg.Connection) -> None:
    await connection.set_type_codec("json", encoder=json.dumps, decoder=json.loads, schema="pg_catalog")
    await connection.set_type_codec("jsonb", encoder=json.dumps, decoder=json.loads, schema="pg_catalog")
    await connection.execute("SET default_transaction_read_only=on")
    await connection.execute("SET statement_timeout='5s'")
    await connection.execute("SET lock_timeout='1s'")


async def _sink_init(connection: asyncpg.Connection) -> None:
    await connection.set_type_codec("json", encoder=json.dumps, decoder=json.loads, schema="pg_catalog")
    await connection.set_type_codec("jsonb", encoder=json.dumps, decoder=json.loads, schema="pg_catalog")
    await connection.execute("SET statement_timeout='5s'")
    await connection.execute("SET lock_timeout='1s'")


class ProductionReader:
    def __init__(self, pool: asyncpg.Pool):
        self.pool = pool

    async def prove_boundary(self) -> dict[str, Any]:
        async with self.pool.acquire() as connection:
            row = await connection.fetchrow("""
                SELECT current_user AS identity,current_setting('transaction_read_only') AS read_only,
                  has_table_privilege(current_user,'public.trades','INSERT') AS can_insert,
                  has_table_privilege(current_user,'public.strategy_orders','UPDATE') AS can_update,
                  has_table_privilege(current_user,'public.strategy_signals','DELETE') AS can_delete
            """)
        proof = dict(row or {})
        expected = {
            "identity": "rulenix_demo_trial_reader",
            "read_only": "on",
            "can_insert": False,
            "can_update": False,
            "can_delete": False,
        }
        if proof != expected:
            raise RuntimeError(f"DEMO trial source boundary failed: {proof}")
        return proof

    async def current_market_state(self) -> dict[str, Any]:
        async with self.pool.acquire() as connection:
            row = await connection.fetchrow("""
                WITH clock AS (
                  SELECT (NOW() AT TIME ZONE 'Asia/Kolkata')::date AS trade_date,
                         EXTRACT(ISODOW FROM NOW() AT TIME ZONE 'Asia/Kolkata')::int AS iso_day
                )
                SELECT clock.trade_date,clock.iso_day,
                       COALESCE(calendar.morning_open,TRUE) AS morning_open,
                       COALESCE(calendar.evening_open,TRUE) AS evening_open,
                       COALESCE(calendar.reason,'') AS calendar_reason,
                       (SELECT COUNT(*) FROM public.strategy_scheduler_runs r
                         WHERE r.trade_date=clock.trade_date AND r.status='skipped'
                           AND r.last_error='Weekend') AS weekend_skips,
                       (SELECT COUNT(*) FROM public.strategy_signals s
                         WHERE (s.signal_at AT TIME ZONE 'Asia/Kolkata')::date=clock.trade_date
                           AND s.strategy_key='futures_breakout_v3') AS futures_signals,
                       (SELECT COUNT(*) FROM public.strategy_signals s
                         WHERE (s.signal_at AT TIME ZONE 'Asia/Kolkata')::date=clock.trade_date
                           AND s.strategy_key='supertrend_index_options_v1'
                           AND s.signal_type<>'SQUARE_OFF') AS supertrend_entry_signals,
                       (SELECT COUNT(*) FROM public.strategy_signals s
                         WHERE (s.signal_at AT TIME ZONE 'Asia/Kolkata')::date=clock.trade_date
                           AND s.strategy_key='supertrend_index_options_v1'
                           AND s.signal_type='SQUARE_OFF') AS supertrend_eod_signals,
                       COALESCE((SELECT SUM(s.expected_users) FROM public.strategy_signals s
                         WHERE (s.signal_at AT TIME ZONE 'Asia/Kolkata')::date=clock.trade_date
                           AND s.strategy_key='supertrend_index_options_v1'
                           AND s.signal_type='SQUARE_OFF'),0) AS supertrend_eod_users,
                       COALESCE((SELECT enabled FROM public.risk_kill_switches WHERE user_id IS NULL),FALSE) AS global_kill
                  FROM clock LEFT JOIN public.market_calendar calendar USING(trade_date)
            """)
        return dict(row or {})


class TrialRepository:
    def __init__(self, pool: asyncpg.Pool, release: str):
        self.pool = pool
        self.release = release

    async def prove_boundary(self) -> dict[str, Any]:
        async with self.pool.acquire() as connection:
            row = await connection.fetchrow("""
                WITH relations AS (
                  SELECT c.oid,n.nspname,c.relname FROM pg_catalog.pg_class c
                  JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
                )
                SELECT current_user AS identity,
                  has_table_privilege(current_user,(SELECT oid FROM relations WHERE nspname='public' AND relname='trades'),'INSERT') AS can_insert,
                  has_table_privilege(current_user,(SELECT oid FROM relations WHERE nspname='public' AND relname='strategy_orders'),'UPDATE') AS can_update,
                  has_table_privilege(current_user,(SELECT oid FROM relations WHERE nspname='public' AND relname='strategy_signals'),'DELETE') AS can_delete,
                  has_table_privilege(current_user,(SELECT oid FROM relations WHERE nspname='rulenix_demo_trial' AND relname='cycles'),'INSERT') AS can_write_trial
            """)
        proof = dict(row or {})
        expected = {
            "identity": "rulenix_demo_trial_writer",
            "can_insert": False,
            "can_update": False,
            "can_delete": False,
            "can_write_trial": True,
        }
        if proof != expected:
            raise RuntimeError(f"DEMO trial sink boundary failed: {proof}")
        return proof

    async def acquire_leader(self, connection: asyncpg.Connection) -> bool:
        return bool(await connection.fetchval("SELECT pg_try_advisory_lock(hashtext('rulenix:python_demo_trial'))"))

    async def release_leader(self, connection: asyncpg.Connection) -> None:
        await connection.execute("SELECT pg_advisory_unlock(hashtext('rulenix:python_demo_trial'))")

    async def recover_stale(self) -> int:
        async with self.pool.acquire() as connection:
            result = await connection.execute("""
                UPDATE rulenix_demo_trial.cycles
                   SET status='failed',last_error='Recovered stale Python DEMO trial claim',updated_at=NOW()
                 WHERE status='running' AND claimed_at<NOW()-INTERVAL '30 seconds'
            """)
        return int(result.rsplit(" ", 1)[-1])

    async def claim(
        self,
        *,
        cycle_key: str,
        source_kind: str,
        account_ref: str,
        scenario: DemoScenario,
        oracle: dict[str, Any],
        scheduled_for: datetime,
    ) -> UUID | None:
        cycle_id = uuid5(NAMESPACE_URL, "rulenix-demo-trial:" + cycle_key)
        async with self.pool.acquire() as connection, connection.transaction():
                await connection.execute("""
                    INSERT INTO rulenix_demo_trial.assignments(account_ref,strategy_key,execution_mode,active)
                    VALUES($1,$2,'demo',TRUE)
                    ON CONFLICT(account_ref,strategy_key) DO UPDATE SET active=TRUE,updated_at=NOW()
                """, account_ref, scenario.strategy_key)
                row = await connection.fetchval("""
                    INSERT INTO rulenix_demo_trial.cycles(
                      id,cycle_key,source_kind,account_ref,strategy_key,instrument,scheduled_for,
                      status,attempts,claimed_at,oracle,observer_release)
                    VALUES($1,$2,$3,$4,$5,$6,$7,'running',1,NOW(),$8::jsonb,$9)
                    ON CONFLICT(cycle_key) DO UPDATE SET status='running',attempts=rulenix_demo_trial.cycles.attempts+1,
                      claimed_at=NOW(),last_error='',updated_at=NOW(),observer_release=EXCLUDED.observer_release
                    WHERE rulenix_demo_trial.cycles.status IN ('pending','failed')
                    RETURNING id
                """, cycle_id, cycle_key, source_kind, account_ref, scenario.strategy_key,
                    scenario.instrument, scheduled_for, oracle, self.release)
        return UUID(str(row)) if row else None

    async def complete(
        self,
        cycle_id: UUID,
        scenario: DemoScenario,
        decision: DemoDecision,
        classification: str,
        mismatch_reason: str,
    ) -> None:
        actual = decision.json()
        base = str(cycle_id)
        async with self.pool.acquire() as connection, connection.transaction():
                if decision.signal_count and decision.side is not None and decision.entry is not None:
                    await connection.execute("""
                        INSERT INTO rulenix_demo_trial.signals(id,cycle_id,signal_type,side,price)
                        VALUES($1,$2,'ENTRY',$3,$4) ON CONFLICT(cycle_id) DO NOTHING
                    """, uuid5(NAMESPACE_URL, base + ":signal"), cycle_id, decision.side, decision.entry)
                    roles: list[tuple[str, str, Decimal | None]] = [
                        ("ENTRY", decision.side, decision.entry)
                    ]
                    exit_side = "SELL" if decision.side == "BUY" else "BUY"
                    roles.append((decision.exit_reason or "EXIT", exit_side, decision.exit_price))
                    if decision.reversal_side:
                        roles.extend([
                            ("SL2_REVERSAL", decision.reversal_side, decision.reversal_entry),
                            ("REVERSAL_EXIT", decision.side, decision.reversal_exit),
                        ])
                    for index, (role, side, price) in enumerate(roles):
                        intent_id = uuid5(NAMESPACE_URL, f"{base}:intent:{index}")
                        await connection.execute("""
                            INSERT INTO rulenix_demo_trial.intents(
                              id,cycle_id,role,side,quantity,price,status,idempotency_key)
                            VALUES($1,$2,$3,$4,$5,$6,'completed',$7) ON CONFLICT(idempotency_key) DO NOTHING
                        """, intent_id, cycle_id, role, side, decision.quantity, price,
                            f"{base}:intent:{index}")
                    order_roles = ["ENTRY", "TARGET", "SL1"]
                    if scenario.strategy_key == "futures_breakout_v3":
                        order_roles.append("SL2")
                    if decision.reversal_side:
                        order_roles += ["REVERSAL_ENTRY", "REVERSAL_TARGET", "REVERSAL_SL1", "REVERSAL_SL2"]
                    for index, role in enumerate(order_roles):
                        filled = role == "ENTRY" or role == decision.exit_reason or (
                            decision.reversal_side is not None and role in {"REVERSAL_ENTRY", "REVERSAL_TARGET"}
                        )
                        order_side = (
                            decision.side if role == "ENTRY"
                            else decision.reversal_side if role == "REVERSAL_ENTRY"
                            else decision.side if role.startswith("REVERSAL_")
                            else exit_side
                        )
                        order_price = {
                            "ENTRY": decision.entry,
                            "TARGET": decision.target,
                            "SL1": decision.sl1,
                            "SL2": decision.sl2,
                            "REVERSAL_ENTRY": decision.reversal_entry,
                            "REVERSAL_TARGET": decision.reversal_exit,
                            "REVERSAL_SL1": decision.reversal_entry,
                            "REVERSAL_SL2": decision.reversal_entry,
                        }[role]
                        await connection.execute("""
                            INSERT INTO rulenix_demo_trial.orders(
                              id,cycle_id,role,side,quantity,price,status,idempotency_key)
                            VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(idempotency_key) DO NOTHING
                        """, uuid5(NAMESPACE_URL, f"{base}:order:{index}"), cycle_id, role,
                            order_side,
                            decision.quantity, order_price, "filled" if filled else "cancelled",
                            f"{base}:order:{index}")
                    await connection.execute("""
                        INSERT INTO rulenix_demo_trial.trades(
                          id,cycle_id,lineage,status,side,quantity,entry_price,exit_price,exit_reason,pnl)
                        VALUES($1,$2,'source','closed',$3,$4,$5,$6,$7,$8) ON CONFLICT(cycle_id,lineage) DO NOTHING
                    """, uuid5(NAMESPACE_URL, base + ":trade"), cycle_id, decision.side,
                        decision.quantity, decision.entry, decision.exit_price,
                        decision.exit_reason, decision.pnl)
                    if decision.reversal_side:
                        await connection.execute("""
                            INSERT INTO rulenix_demo_trial.trades(
                              id,cycle_id,lineage,status,side,quantity,entry_price,exit_price,exit_reason,pnl)
                            VALUES($1,$2,'reversal','closed',$3,$4,$5,$6,'TARGET',$7)
                            ON CONFLICT(cycle_id,lineage) DO NOTHING
                        """, uuid5(NAMESPACE_URL, base + ":reversal"), cycle_id,
                            decision.reversal_side, decision.quantity, decision.reversal_entry,
                            decision.reversal_exit, decision.reversal_pnl)
                await connection.execute("""
                    INSERT INTO rulenix_demo_trial.events(id,cycle_id,event_type,payload)
                    VALUES($1,$2,'TERMINAL_STATE',$3::jsonb) ON CONFLICT(cycle_id,event_type) DO NOTHING
                """, uuid5(NAMESPACE_URL, base + ":event"), cycle_id, actual)
                await connection.execute("""
                    UPDATE rulenix_demo_trial.cycles SET status='completed',decision=$2::jsonb,
                      parity_classification=$3,mismatch_reason=$4,completed_at=NOW(),updated_at=NOW()
                    WHERE id=$1 AND status='running'
                """, cycle_id, actual, classification, mismatch_reason)

    async def heartbeat(self, *, healthy: bool, detail: str, counts: dict[str, int]) -> None:
        async with self.pool.acquire() as connection:
            await connection.execute("""
                INSERT INTO rulenix_demo_trial.health(observer_release,healthy,detail,last_poll_at,counts)
                VALUES($1,$2,$3,NOW(),$4::jsonb)
                ON CONFLICT(observer_release) DO UPDATE SET healthy=EXCLUDED.healthy,
                  detail=EXCLUDED.detail,last_poll_at=EXCLUDED.last_poll_at,counts=EXCLUDED.counts
            """, self.release, healthy, detail[:512], counts)

    async def summary(self) -> dict[str, int]:
        async with self.pool.acquire() as connection:
            row = await connection.fetchrow("""
                SELECT COUNT(*) FILTER(WHERE status='completed') AS cycles,
                       COUNT(*) FILTER(WHERE parity_classification='MATCH') AS matches,
                       COUNT(*) FILTER(WHERE parity_classification='MISMATCH') AS mismatches,
                       COUNT(*) FILTER(WHERE status='failed') AS errors,
                       COUNT(*) FILTER(WHERE source_kind='REAL_PRODUCTION_DEMO_OBSERVATION') AS real_observations,
                       COUNT(*) FILTER(WHERE source_kind='DETERMINISTIC_PRODUCTION_DERIVED_REPLAY') AS replay_observations
                  FROM rulenix_demo_trial.cycles WHERE observer_release=$1
            """, self.release)
        return {key: int(value) for key, value in dict(row or {}).items()}


async def create_pools(settings: DemoTrialSettings) -> tuple[asyncpg.Pool, asyncpg.Pool]:
    source = await asyncpg.create_pool(
        settings.source_database_url.replace("postgresql+asyncpg://", "postgresql://", 1),
        min_size=1, max_size=2, init=_source_init, command_timeout=5,
    )
    try:
        sink = await asyncpg.create_pool(
            settings.sink_database_url.replace("postgresql+asyncpg://", "postgresql://", 1),
            min_size=1, max_size=2, init=_sink_init, command_timeout=5,
        )
    except Exception:
        await source.close()
        raise
    return source, sink


__all__ = ["ProductionReader", "TrialRepository", "create_pools"]
