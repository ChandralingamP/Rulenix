from __future__ import annotations

from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from uuid import UUID, uuid4

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from .domain import ReconciliationSnapshot


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

    async def record_blockers(self, snapshot: ReconciliationSnapshot, *, external_active_orders: int = 0, structurally_unknown_orders: int = 0, active_conditional_rules: int = 0) -> None:
        if not snapshot.authoritative:
            await self.session.execute(text("""
                INSERT INTO broker_reconciliation_blockers(user_id,status,detail,first_detected_at,last_checked_at)
                VALUES(:user,'open',:detail,NOW(),NOW())
                ON CONFLICT(user_id) DO UPDATE SET status='open',detail=EXCLUDED.detail,last_checked_at=NOW(),resolved_at=NULL
            """), {"user": snapshot.user_id, "detail": f"Incomplete broker evidence: {snapshot.failure_detail}"[:4000]})
            return
        detail = "Authoritative reconciliation found no unresolved broker mutations." if not any((external_active_orders, structurally_unknown_orders, active_conditional_rules)) else "Broker evidence contains unresolved external state."
        status = "open" if any((external_active_orders, structurally_unknown_orders, active_conditional_rules)) else "resolved"
        await self.session.execute(text("""
            INSERT INTO broker_reconciliation_blockers(user_id,status,external_active_orders,structurally_unknown_orders,active_conditional_rules,detail,first_detected_at,last_checked_at,resolved_at)
            VALUES(:user,CAST(:status AS varchar(16)),:external,:unknown,:conditional,:detail,NOW(),NOW(),CASE WHEN CAST(:status AS varchar(16))='resolved' THEN NOW() ELSE NULL END)
            ON CONFLICT(user_id) DO UPDATE SET status=EXCLUDED.status,external_active_orders=EXCLUDED.external_active_orders,
              structurally_unknown_orders=EXCLUDED.structurally_unknown_orders,active_conditional_rules=EXCLUDED.active_conditional_rules,
              detail=EXCLUDED.detail,last_checked_at=NOW(),resolved_at=EXCLUDED.resolved_at
        """), {"user": snapshot.user_id, "status": status, "external": external_active_orders, "unknown": structurally_unknown_orders, "conditional": active_conditional_rules, "detail": detail})

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


__all__ = ["ReconciliationRepository"]
