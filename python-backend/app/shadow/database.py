"""Two-identity database boundary for the production shadow observer."""

from __future__ import annotations

import hashlib
import hmac
import json
from datetime import UTC, datetime, timedelta
from decimal import Decimal
from typing import Any

import asyncpg  # type: ignore[import-untyped]

from .config import ShadowSettings
from .models import Observation


def _json_default(value: Any) -> Any:
    if isinstance(value, Decimal):
        return format(value, "f")
    if isinstance(value, datetime):
        return value.astimezone(UTC).isoformat()
    raise TypeError(f"unsupported JSON value: {type(value).__name__}")


async def _source_connection(connection: asyncpg.Connection) -> None:
    await connection.set_type_codec("json", encoder=json.dumps, decoder=json.loads, schema="pg_catalog")
    await connection.set_type_codec("jsonb", encoder=json.dumps, decoder=json.loads, schema="pg_catalog")
    await connection.execute("SET default_transaction_read_only=on")
    await connection.execute("SET statement_timeout='5s'")
    await connection.execute("SET lock_timeout='1s'")


async def _sink_connection(connection: asyncpg.Connection) -> None:
    await connection.set_type_codec("json", encoder=json.dumps, decoder=json.loads, schema="pg_catalog")
    await connection.set_type_codec("jsonb", encoder=json.dumps, decoder=json.loads, schema="pg_catalog")
    await connection.execute("SET statement_timeout='5s'")
    await connection.execute("SET lock_timeout='1s'")


class SourceReader:
    def __init__(self, pool: asyncpg.Pool, pseudonym_key: bytes):
        self.pool = pool
        self.pseudonym_key = pseudonym_key

    def account_ref(self, user_id: Any) -> str | None:
        if user_id is None:
            return None
        return hmac.new(self.pseudonym_key, str(user_id).encode(), hashlib.sha256).hexdigest()

    async def prove_boundary(self) -> dict[str, Any]:
        async with self.pool.acquire() as connection:
            row = await connection.fetchrow(
                """
                WITH relations AS (
                  SELECT c.oid,n.nspname,c.relname FROM pg_catalog.pg_class c
                  JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
                )
                SELECT current_user AS identity,
                       current_setting('transaction_read_only') AS read_only,
                       has_table_privilege(current_user,(SELECT oid FROM relations WHERE nspname='public' AND relname='trades'),'INSERT') AS can_insert,
                       has_table_privilege(current_user,(SELECT oid FROM relations WHERE nspname='public' AND relname='strategy_orders'),'UPDATE') AS can_update,
                       has_table_privilege(current_user,(SELECT oid FROM relations WHERE nspname='public' AND relname='strategy_signals'),'DELETE') AS can_delete
                """
            )
        proof = dict(row or {})
        if proof != {
            "identity": "rulenix_shadow_reader",
            "read_only": "on",
            "can_insert": False,
            "can_update": False,
            "can_delete": False,
        }:
            raise RuntimeError(f"shadow source database boundary failed: {proof}")
        return proof

    async def signals(self, lookback_hours: int, batch_size: int) -> list[dict[str, Any]]:
        async with self.pool.acquire() as connection:
            rows = await connection.fetch(
                """
                SELECT s.id::text AS source_id,s.strategy_key,s.instrument,s.signal_at,
                       s.created_at AS source_created_at,s.payload,s.signal_type,s.session_key,
                       snap.id::text AS snapshot_id,snap.contract_symbol,snap.contract_token,
                       snap.lot_size,snap.highs,snap.lows,snap.hh2,snap.ll2,snap.hh4,snap.ll4,
                       snap.buy_entry,snap.buy_target,snap.buy_sl1,snap.buy_sl2,
                       snap.sell_entry,snap.sell_target,snap.sell_sl1,snap.sell_sl2,
                       snap.entry_direction,snap.planned_entry,snap.execution_key,
                       COALESCE((
                         SELECT jsonb_agg(jsonb_build_object(
                           'user_id',i.user_id::text,'role',i.role,'side',i.side,
                           'lots',i.lots,'quantity',i.quantity,'price',i.price)
                           ORDER BY i.user_id,i.role)
                         FROM public.strategy_execution_intents i WHERE i.signal_id=s.id
                       ),'[]'::jsonb) AS intents,
                       event.payload AS event_payload
                  FROM public.strategy_signals s
                  LEFT JOIN public.strategy_market_snapshots snap
                    ON snap.id=COALESCE(s.snapshot_id,(
                       SELECT i.snapshot_id FROM public.strategy_execution_intents i
                        WHERE i.signal_id=s.id AND i.snapshot_id IS NOT NULL ORDER BY i.id LIMIT 1))
                  LEFT JOIN LATERAL (
                    SELECT e.payload FROM public.strategy_events e
                     WHERE e.strategy_key=s.strategy_key AND e.instrument=s.instrument
                       AND e.event_type='supertrend_signal'
                       AND e.created_at BETWEEN s.created_at-INTERVAL '10 seconds' AND s.created_at+INTERVAL '10 seconds'
                     ORDER BY ABS(EXTRACT(EPOCH FROM (e.created_at-s.created_at))) LIMIT 1
                  ) event ON TRUE
                 WHERE s.created_at>=NOW()-($1::int*INTERVAL '1 hour')
                   AND s.strategy_key IN ('futures_breakout_v3','supertrend_index_options_v1')
                 ORDER BY s.created_at DESC LIMIT $2
                """,
                lookback_hours,
                batch_size,
            )
        result: list[dict[str, Any]] = []
        for source in rows:
            row = dict(source)
            intents = []
            for raw in row.get("intents") or []:
                item = dict(raw)
                item["account_ref"] = self.account_ref(item.pop("user_id", None))
                intents.append(item)
            row["intents"] = intents
            result.append(row)
        return result

    async def candles(self, instrument: str, signal_at: datetime) -> list[dict[str, Any]]:
        config = {
            "SENSEX": ("BSE", "99919000"),
            "NIFTY": ("NSE", "99926000"),
        }.get(instrument)
        if config is None:
            return []
        async with self.pool.acquire() as connection:
            rows = await connection.fetch(
                """
                SELECT candle_time AS at,open_price AS open,high_price AS high,
                       low_price AS low,close_price AS close
                  FROM public.backtest_market_candles
                 WHERE exchange=$1 AND symbol_token=$2 AND interval_key='FIVE_MINUTE'
                   AND candle_time BETWEEN $3 AND $4 ORDER BY candle_time
                """,
                config[0],
                config[1],
                signal_at - timedelta(days=2),
                signal_at,
            )
        return [dict(row) for row in rows]

    async def readiness(self) -> list[dict[str, Any]]:
        async with self.pool.acquire() as connection:
            rows = await connection.fetch(
                """
                SELECT h.user_id::text,h.healthy,h.checked_at,h.broker_credential_revision,
                       COALESCE(profile.broker_credential_revision,0) AS current_credential_revision,
                       safety.open_live_trades+safety.unresolved_closed_live_trades+
                       safety.unresolved_live_orders+safety.unresolved_live_execution_intents+
                       safety.unresolved_live_reversals+safety.unresolved_live_manual_closes+
                       safety.unresolved_broker_incidents+safety.unresolved_broker_mutations AS blockers,
                       COALESCE(blocker.rulenix_owned_exposure,0) AS rulenix_owned_exposure,
                       COALESCE(blocker.ambiguous_exposure,0) AS ambiguous_exposure,
                       COALESCE(blocker.manual_external_exposure,0) AS manual_external_exposure
                  FROM public.broker_reconciliation_health h
                  JOIN public.broker_deployment_account_safety safety ON safety.user_id=h.user_id
                  JOIN public.user_profiles profile ON profile.user_id=h.user_id
                  LEFT JOIN public.broker_reconciliation_blockers blocker ON blocker.user_id=h.user_id
                """
            )
        result = []
        for source in rows:
            row = dict(source)
            row["account_ref"] = self.account_ref(row.pop("user_id"))
            result.append(row)
        return result


class ShadowWriter:
    def __init__(self, pool: asyncpg.Pool, release: str):
        self.pool = pool
        self.release = release

    async def prove_boundary(self) -> dict[str, Any]:
        async with self.pool.acquire() as connection:
            row = await connection.fetchrow(
                """
                WITH relations AS (
                  SELECT c.oid,n.nspname,c.relname FROM pg_catalog.pg_class c
                  JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
                )
                SELECT current_user AS identity,
                       has_table_privilege(current_user,(SELECT oid FROM relations WHERE nspname='public' AND relname='trades'),'INSERT') AS can_insert,
                       has_table_privilege(current_user,(SELECT oid FROM relations WHERE nspname='public' AND relname='strategy_orders'),'UPDATE') AS can_update,
                       has_table_privilege(current_user,(SELECT oid FROM relations WHERE nspname='public' AND relname='strategy_signals'),'DELETE') AS can_delete,
                       has_table_privilege(current_user,(SELECT oid FROM relations WHERE nspname='rulenix_shadow' AND relname='observations'),'INSERT') AS can_write_shadow
                """
            )
        proof = dict(row or {})
        if proof != {
            "identity": "rulenix_shadow_writer",
            "can_insert": False,
            "can_update": False,
            "can_delete": False,
            "can_write_shadow": True,
        }:
            raise RuntimeError(f"shadow sink database boundary failed: {proof}")
        return proof

    async def store(self, observation: Observation) -> bool:
        payload = (
            observation.source_kind,
            observation.source_id,
            observation.observed_at,
            observation.account_ref,
            observation.strategy,
            observation.instrument,
            observation.input_version,
            observation.rust_decision,
            observation.python_decision,
            observation.classification,
            observation.mismatch_reason,
            observation.severity,
            observation.rust_latency_ms,
            observation.python_latency_ms,
            observation.failure_classification,
            observation.source_created_at,
            self.release,
        )
        async with self.pool.acquire() as connection:
            result = await connection.execute(
                """
                INSERT INTO rulenix_shadow.observations(
                  source_kind,source_id,observed_at,account_ref,strategy,instrument,input_version,
                  rust_decision,python_shadow_decision,parity_classification,mismatch_reason,severity,
                  rust_latency_ms,python_latency_ms,failure_classification,source_created_at,observer_release)
                VALUES($1,$2,$3,$4,$5,$6,$7,$8::jsonb,$9::jsonb,$10,$11,$12,$13,$14,$15,$16,$17)
                ON CONFLICT(source_kind,source_id,observer_release) DO NOTHING
                """,
                *payload,
            )
        return result == "INSERT 0 1"

    async def heartbeat(self, *, healthy: bool, detail: str, counts: dict[str, int]) -> None:
        async with self.pool.acquire() as connection:
            await connection.execute(
                """
                INSERT INTO rulenix_shadow.observer_health(observer_release,healthy,detail,last_poll_at,counts)
                VALUES($1,$2,$3,NOW(),$4::jsonb)
                ON CONFLICT(observer_release) DO UPDATE SET healthy=EXCLUDED.healthy,
                  detail=EXCLUDED.detail,last_poll_at=EXCLUDED.last_poll_at,counts=EXCLUDED.counts
                """,
                self.release,
                healthy,
                detail[:512],
                counts,
            )


async def create_pools(settings: ShadowSettings) -> tuple[asyncpg.Pool, asyncpg.Pool]:
    source = await asyncpg.create_pool(
        settings.source_database_url.replace("postgresql+asyncpg://", "postgresql://", 1),
        min_size=1,
        max_size=2,
        init=_source_connection,
        command_timeout=5,
    )
    try:
        sink = await asyncpg.create_pool(
            settings.sink_database_url.replace("postgresql+asyncpg://", "postgresql://", 1),
            min_size=1,
            max_size=2,
            init=_sink_connection,
            command_timeout=5,
        )
    except Exception:
        await source.close()
        raise
    return source, sink


__all__ = ["ShadowWriter", "SourceReader", "create_pools"]
