from __future__ import annotations

from dataclasses import dataclass, replace
from decimal import Decimal

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from .domain import (
    ManualCloseClassification,
    ReconciliationSnapshot,
    classify_manual_broker_close,
    weighted_fill_price,
)
from .repository import ReconciliationRepository


@dataclass(frozen=True)
class ReconciliationResult:
    authoritative: bool
    healthy: bool
    blockers: int
    incidents: int
    detail: str


class ReconciliationService:
    """Applies read-only broker truth; it never invokes Angel mutation methods."""

    def __init__(self, session: AsyncSession):
        self.session = session
        self.repository = ReconciliationRepository(session)

    async def apply_snapshot(self, snapshot: ReconciliationSnapshot) -> ReconciliationResult:
        async with self.session.begin(), self.repository.user_lock(snapshot.user_id) as acquired:
            if not acquired:
                return ReconciliationResult(False, False, 1, 0, "Another reconciliation owns this account.")
            current_revision = await self.session.scalar(text("SELECT broker_credential_revision FROM user_profiles WHERE user_id=:user"), {"user": snapshot.user_id})
            if current_revision is not None:
                snapshot = replace(snapshot, current_credential_revision=int(current_revision))
            healthy = await self.repository.record_health(snapshot)
            incidents = 0
            blockers = 0
            if not snapshot.authoritative:
                blockers = 1
                await self.repository.record_blockers(snapshot)
            else:
                broker_positions = {item.token: item for item in (snapshot.positions.data or ())}
                local = (await self.session.execute(text("""
                    SELECT t.id,t.strategy_key,t.instrument_label,t.status,t.quantity,t.execution_mode,
                           s.exchange_segment,s.contract_token,s.contract_symbol
                    FROM trades t LEFT JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
                    WHERE t.user_id=:user AND t.execution_mode='live' AND (t.status='open' OR t.safety_status<>'CLOSED')
                """), {"user": snapshot.user_id})).mappings().all()
                for trade in local:
                    broker = broker_positions.get(str(trade["contract_token"])) if trade["contract_token"] else None
                    quantity = int(broker.quantity) if broker else 0
                    symbol = str(trade["contract_symbol"] or "")
                    token = str(trade["contract_token"] or "")
                    if quantity == int(trade["quantity"] or 0) and broker is not None:
                        await self.session.execute(text("UPDATE trades SET broker_net_quantity=:quantity,broker_average_price=:price,last_position_reconciled_at=NOW(),updated_at=NOW() WHERE id=:trade AND user_id=:user"), {"quantity": quantity, "price": float(broker.average_price) if broker and broker.average_price is not None else None, "trade": trade["id"], "user": snapshot.user_id})
                        continue
                    incidents += 1
                    await self.repository.record_position_incident(user_id=snapshot.user_id, strategy_key=str(trade["strategy_key"] or ""), instrument=str(trade["instrument_label"] or ""), exchange_segment=str(trade["exchange_segment"] or ""), contract_token=token, contract_symbol=symbol, incident_type="BROKER_FLAT_LOCAL_OPEN" if broker is None else "POSITION_QUANTITY_MISMATCH", broker_quantity=quantity, local_quantity=int(trade["quantity"] or 0), detail="Authoritative broker position does not match local exposure.", trade_id=trade["id"], broker_average_price=float(broker.average_price) if broker and broker.average_price is not None else None)
                    await self.session.execute(text("UPDATE trades SET safety_status='RECONCILIATION_REQUIRED',broker_net_quantity=:quantity,broker_average_price=:price,last_position_reconciled_at=NOW(),updated_at=NOW() WHERE id=:trade AND user_id=:user"), {"quantity": quantity, "price": float(broker.average_price) if broker and broker.average_price is not None else None, "trade": trade["id"], "user": snapshot.user_id})
                active_external = sum(1 for order in (snapshot.orders.data or ()) if not order.owned_by_rulenix and order.status.lower() in {"open", "trigger pending", "pending", "open pending"})
                unknown = sum(1 for order in (snapshot.orders.data or ()) if not order.order_id or order.status.lower() in {"unknown", "ambiguous"})
                conditional = sum(1 for rule in (snapshot.conditional_rules.data or ()) if rule.active)
                blockers = int(bool(active_external or unknown or conditional))
                await self.repository.record_blockers(snapshot, external_active_orders=active_external, structurally_unknown_orders=unknown, active_conditional_rules=conditional)
            return ReconciliationResult(snapshot.authoritative, healthy, blockers, incidents, "Authoritative reconciliation applied." if healthy else snapshot.failure_detail)

    async def classify_manual_close(self, *, snapshot: ReconciliationSnapshot, local_symbol: str, local_token: str, local_exchange: str, entry_at, remaining_quantity: int, conflicting_executable_sibling: bool = False, local_trade_count: int = 1, local_direction: str | None = None) -> tuple[ManualCloseClassification, Decimal | None]:
        if not snapshot.authoritative:
            return ManualCloseClassification.RECONCILIATION_REQUIRED, None
        broker_position = next((position for position in (snapshot.positions.data or ()) if position.token == local_token), None)
        fills = [fill for fill in (snapshot.fills.data or ()) if fill.token == local_token]
        classification = classify_manual_broker_close(local_open=True, local_symbol=local_symbol, local_token=local_token, local_exchange=local_exchange, entry_at=entry_at, remaining_quantity=remaining_quantity, broker_position=broker_position, fills=fills, order_book_succeeded=True, positions_succeeded=True, fills_succeeded=True, conflicting_executable_sibling=conflicting_executable_sibling, local_trade_count=local_trade_count, local_direction=local_direction)
        return classification, weighted_fill_price(fills) if classification is ManualCloseClassification.MANUAL_BROKER_CLOSE else None


__all__ = ["ReconciliationResult", "ReconciliationService"]
