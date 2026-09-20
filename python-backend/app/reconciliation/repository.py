from __future__ import annotations

from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from datetime import datetime
from decimal import Decimal
from uuid import UUID, uuid4

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from .domain import ManualCloseEvidence, ReconciliationSnapshot


class ReconciliationRepository:
    """Durable reconciliation writes, always scoped to one account."""

    def __init__(self, session: AsyncSession):
        self.session = session

    @asynccontextmanager
    async def user_lock(self, user_id: UUID) -> AsyncIterator[bool]:
        result = await self.session.execute(
            text("SELECT pg_try_advisory_xact_lock(hashtextextended(:user,0))"), {"user": str(user_id)}
        )
        yield bool(result.scalar())

    async def record_health(self, snapshot: ReconciliationSnapshot) -> bool:
        healthy = snapshot.authoritative
        detail = "Authoritative positions, orders, trades, conditional and account evidence." if healthy else snapshot.failure_detail
        if not detail:
            detail = "Credential revision or account/egress context mismatch."
        await self.session.execute(text("""
            INSERT INTO broker_reconciliation_health(user_id,healthy,detail,checked_at,broker_credential_revision)
            VALUES(:user,:healthy,:detail,NOW(),:revision)
            ON CONFLICT(user_id) DO UPDATE SET healthy=EXCLUDED.healthy,detail=EXCLUDED.detail,
              checked_at=EXCLUDED.checked_at,broker_credential_revision=EXCLUDED.broker_credential_revision
        """), {"user": snapshot.user_id, "healthy": healthy, "detail": detail[:4000], "revision": snapshot.credential_revision if healthy else None})
        return healthy

    async def record_blockers(self, snapshot: ReconciliationSnapshot, *, rulenix_owned_exposure: int = 0, ambiguous_exposure: int = 0, manual_external_exposure: int = 0) -> None:
        if not snapshot.authoritative:
            await self.session.execute(text("""
                INSERT INTO broker_reconciliation_blockers(user_id,status,detail,first_detected_at,last_checked_at)
                VALUES(:user,'open',:detail,NOW(),NOW())
                ON CONFLICT(user_id) DO UPDATE SET status='open',detail=EXCLUDED.detail,last_checked_at=NOW(),resolved_at=NULL
            """), {"user": snapshot.user_id, "detail": f"Incomplete broker evidence: {snapshot.failure_detail}"[:4000]})
            return
        detail = "Authoritative reconciliation found no unresolved Rulenix-owned or ambiguous broker mutations." if not any((rulenix_owned_exposure, ambiguous_exposure)) else "Broker evidence contains unresolved Rulenix-owned or ambiguous state."
        status = "open" if any((rulenix_owned_exposure, ambiguous_exposure)) else "resolved"
        await self.session.execute(text("""
            INSERT INTO broker_reconciliation_blockers(user_id,status,external_active_orders,structurally_unknown_orders,active_conditional_rules,rulenix_owned_exposure,ambiguous_exposure,manual_external_exposure,detail,first_detected_at,last_checked_at,resolved_at)
            VALUES(:user,CAST(:status AS varchar(16)),0,:ambiguous,0,:owned,:ambiguous,:manual,:detail,NOW(),NOW(),CASE WHEN CAST(:status AS varchar(16))='resolved' THEN NOW() ELSE NULL END)
            ON CONFLICT(user_id) DO UPDATE SET status=EXCLUDED.status,external_active_orders=EXCLUDED.external_active_orders,
              structurally_unknown_orders=EXCLUDED.structurally_unknown_orders,active_conditional_rules=EXCLUDED.active_conditional_rules,
              rulenix_owned_exposure=EXCLUDED.rulenix_owned_exposure,ambiguous_exposure=EXCLUDED.ambiguous_exposure,
              manual_external_exposure=EXCLUDED.manual_external_exposure,
              detail=EXCLUDED.detail,last_checked_at=NOW(),resolved_at=EXCLUDED.resolved_at
        """), {"user": snapshot.user_id, "status": status, "owned": rulenix_owned_exposure, "ambiguous": ambiguous_exposure, "manual": manual_external_exposure, "detail": detail})

    async def replace_exposure_observations(
        self, *, user_id: UUID, credential_revision: int,
        observations: list[dict[str, object]],
    ) -> None:
        await self.session.execute(
            text("DELETE FROM broker_exposure_observations WHERE user_id=:user"), {"user": user_id}
        )
        for observation in observations:
            await self.session.execute(text("""
                INSERT INTO broker_exposure_observations(
                  user_id,exposure_kind,broker_reference,ownership_status,exchange_segment,
                  contract_token,contract_symbol,side,quantity,evidence,broker_credential_revision)
                VALUES(:user,:kind,:reference,:ownership,:exchange,:token,:symbol,:side,
                  :quantity,:evidence,:revision)
            """), {"user": user_id, "revision": credential_revision, **observation})

    async def record_position_incident(self, *, user_id: UUID, strategy_key: str, instrument: str, exchange_segment: str, contract_token: str, contract_symbol: str, incident_type: str, broker_quantity: int, local_quantity: int, detail: str, trade_id: UUID | None = None, broker_average_price: float | None = None) -> None:
        await self.session.execute(text("""
            INSERT INTO broker_position_incidents(id,user_id,strategy_key,instrument,exchange_segment,contract_token,contract_symbol,incident_type,status,broker_quantity,local_quantity,broker_average_price,trade_id,detail)
            VALUES(:id,:user,:strategy,:instrument,:exchange,:token,:symbol,:kind,'open',:broker,:local,:average,:trade,:detail)
            ON CONFLICT(user_id,exchange_segment,contract_token,incident_type) DO UPDATE SET status='open',broker_quantity=EXCLUDED.broker_quantity,
              local_quantity=EXCLUDED.local_quantity,broker_average_price=EXCLUDED.broker_average_price,trade_id=EXCLUDED.trade_id,
              detail=EXCLUDED.detail,last_detected_at=NOW(),resolved_at=NULL
        """), {"id": uuid4(), "user": user_id, "strategy": strategy_key, "instrument": instrument, "exchange": exchange_segment, "token": contract_token, "symbol": contract_symbol, "kind": incident_type, "broker": broker_quantity, "local": local_quantity, "average": broker_average_price, "trade": trade_id, "detail": detail[:4000]})

    async def recover_stale_work(self, *, age_seconds: int = 120) -> dict[str, int]:
        if age_seconds < 1:
            raise ValueError("age_seconds must be positive")
        values = {"age": age_seconds}
        execution = await self.session.execute(text("UPDATE strategy_execution_intents SET status='retry_wait',next_attempt_at=NOW(),last_error='Backend restarted while this execution intent was claimed.',updated_at=NOW() WHERE status='claimed' AND claimed_at<NOW()-(:age * INTERVAL '1 second')"), values)
        reversals = await self.session.execute(text("UPDATE strategy_reversal_intents SET status='pending',next_attempt_at=NOW(),last_error='Backend restarted while the reversal was being processed.',updated_at=NOW() WHERE status='processing' AND updated_at<NOW()-(:age * INTERVAL '1 second')"), values)
        manual = await self.session.execute(text("UPDATE manual_trade_close_intents SET status='reconciliation_required',last_error='Recovered uncertain manual-close worker claim.' WHERE status='cancelling_protection' AND updated_at<NOW()-(:age * INTERVAL '1 second')"), values)
        uncertain = await self.session.execute(text("UPDATE strategy_orders SET status='ambiguous',broker_error_class='ambiguous',uncertain_since_at=COALESCE(uncertain_since_at,NOW()),updated_at=NOW() WHERE status='submitting' AND updated_at<NOW()-(:age * INTERVAL '1 second')"), values)
        processing = await self.session.execute(text("UPDATE strategy_orders SET status=CASE WHEN processed_quantity>0 AND processed_quantity<filled_quantity THEN 'partially_filled' ELSE 'submitted' END,broker_status='Fill processing was interrupted; queued for reconciliation.',updated_at=NOW() WHERE status='processing' AND updated_at<NOW()-(:age * INTERVAL '1 second')"), values)
        cancelling = await self.session.execute(text("UPDATE strategy_orders SET status='submitted',broker_status='Cancellation was interrupted; queued for reconciliation.',updated_at=NOW() WHERE status='cancelling' AND updated_at<NOW()-(:age * INTERVAL '1 second')"), values)
        order_count = sum(int(getattr(result, "rowcount", 0) or 0) for result in (uncertain, processing, cancelling))
        return {"execution_intents": int(getattr(execution, "rowcount", 0) or 0), "reversal_intents": int(getattr(reversals, "rowcount", 0) or 0), "manual_close_intents": int(getattr(manual, "rowcount", 0) or 0), "orders": order_count}

    async def deployment_safe(self, user_id: UUID) -> bool:
        row = (await self.session.execute(text("SELECT open_live_trades,unresolved_closed_live_trades,unresolved_live_orders,unresolved_live_execution_intents,unresolved_live_reversals,unresolved_live_manual_closes,unresolved_broker_incidents,unresolved_broker_mutations FROM broker_deployment_account_safety WHERE user_id=:user"), {"user": user_id})).mappings().first()
        return row is not None and all(int(value or 0) == 0 for value in row.values())

    async def store_manual_close_evidence(self, evidence: ManualCloseEvidence) -> None:
        await self.session.execute(text("""
            INSERT INTO manual_broker_close_evidence(
              trade_id,user_id,broker_credential_revision,exchange_segment,contract_token,
              contract_symbol,close_side,filled_quantity,weighted_fill_price,broker_order_ids,
              first_fill_at,last_fill_at,observed_at,consumed_at)
            VALUES(:trade,:user,:revision,:exchange,:token,:symbol,:side,:quantity,:price,
              :orders,:first_fill,:last_fill,:observed,:consumed)
            ON CONFLICT(trade_id) DO UPDATE SET
              broker_credential_revision=EXCLUDED.broker_credential_revision,
              exchange_segment=EXCLUDED.exchange_segment,contract_token=EXCLUDED.contract_token,
              contract_symbol=EXCLUDED.contract_symbol,close_side=EXCLUDED.close_side,
              filled_quantity=EXCLUDED.filled_quantity,weighted_fill_price=EXCLUDED.weighted_fill_price,
              broker_order_ids=EXCLUDED.broker_order_ids,first_fill_at=EXCLUDED.first_fill_at,
              last_fill_at=EXCLUDED.last_fill_at,observed_at=EXCLUDED.observed_at,
              consumed_at=EXCLUDED.consumed_at
        """), {
            "trade": evidence.trade_id, "user": evidence.user_id,
            "revision": evidence.broker_credential_revision, "exchange": evidence.exchange,
            "token": evidence.token, "symbol": evidence.symbol, "side": evidence.close_side,
            "quantity": evidence.filled_quantity, "price": evidence.weighted_fill_price,
            "orders": list(evidence.broker_order_ids), "first_fill": evidence.first_fill_at,
            "last_fill": evidence.last_fill_at, "observed": evidence.observed_at,
            "consumed": evidence.consumed_at,
        })

    async def load_manual_close_evidence(
        self, *, trade_id: UUID, user_id: UUID, credential_revision: int
    ) -> ManualCloseEvidence | None:
        row = (await self.session.execute(text("""
            SELECT trade_id,user_id,broker_credential_revision,exchange_segment,contract_token,
                   contract_symbol,close_side,filled_quantity,weighted_fill_price,broker_order_ids,
                   first_fill_at,last_fill_at,observed_at,consumed_at
              FROM manual_broker_close_evidence
             WHERE trade_id=:trade AND user_id=:user
               AND broker_credential_revision=:revision AND consumed_at IS NULL
        """), {"trade": trade_id, "user": user_id, "revision": credential_revision})).mappings().first()
        if row is None:
            return None
        return ManualCloseEvidence(
            trade_id=row["trade_id"], user_id=row["user_id"],
            broker_credential_revision=int(row["broker_credential_revision"]),
            exchange=str(row["exchange_segment"]), token=str(row["contract_token"]),
            symbol=str(row["contract_symbol"]), close_side=str(row["close_side"]),
            filled_quantity=int(row["filled_quantity"]),
            weighted_fill_price=Decimal(row["weighted_fill_price"]),
            broker_order_ids=tuple(row["broker_order_ids"]), first_fill_at=row["first_fill_at"],
            last_fill_at=row["last_fill_at"], observed_at=row["observed_at"],
            consumed_at=row["consumed_at"],
        )

    async def consume_manual_close_evidence(
        self, *, trade_id: UUID, user_id: UUID, credential_revision: int, consumed_at: datetime
    ) -> bool:
        result = await self.session.execute(text("""
            UPDATE manual_broker_close_evidence SET consumed_at=:consumed
             WHERE trade_id=:trade AND user_id=:user
               AND broker_credential_revision=:revision AND consumed_at IS NULL
        """), {"trade": trade_id, "user": user_id, "revision": credential_revision, "consumed": consumed_at})
        return bool(getattr(result, "rowcount", 0))


__all__ = ["ReconciliationRepository"]
