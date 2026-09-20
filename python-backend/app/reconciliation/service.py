from __future__ import annotations

from dataclasses import dataclass, replace
from datetime import UTC, datetime
from decimal import Decimal
from uuid import UUID

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from .domain import (
    ExposureOwnership,
    ManualCloseClassification,
    ManualCloseEvidence,
    ReconciliationSnapshot,
    classify_conditional_ownership,
    classify_manual_broker_close,
    classify_order_ownership,
    classify_position_ownership,
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
                local_contracts = frozenset(
                    (str(item["exchange_segment"] or "").upper(), str(item["contract_token"] or ""))
                    for item in local if item["contract_token"]
                )
                known_broker_ids = frozenset(str(value) for value in (await self.session.execute(text("""
                    SELECT broker_order_id FROM strategy_orders
                     WHERE user_id=:user AND execution_mode='live' AND broker_order_id<>''
                """), {"user": snapshot.user_id})).scalars().all())
                known_client_ids = frozenset(str(value) for value in (await self.session.execute(text("""
                    SELECT client_order_id FROM strategy_orders
                     WHERE user_id=:user AND execution_mode='live' AND client_order_id<>''
                """), {"user": snapshot.user_id})).scalars().all())
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
                observations: list[dict[str, object]] = []
                ownership_counts = {ownership: 0 for ownership in ExposureOwnership}

                def observe(kind: str, reference: str, ownership: ExposureOwnership, *, exchange: str = "", token: str = "", symbol: str = "", side: str = "", quantity: int = 0) -> None:
                    ownership_counts[ownership] += 1
                    observations.append({"kind": kind, "reference": reference, "ownership": ownership.value, "exchange": exchange.upper(), "token": token, "symbol": symbol, "side": side, "quantity": quantity, "evidence": "authoritative broker reconciliation"})

                for position in snapshot.positions.data or ():
                    if position.quantity == 0:
                        continue
                    ownership = classify_position_ownership(
                        position, fills=snapshot.fills.data or (), orders=snapshot.orders.data or (),
                        local_contracts=local_contracts, known_broker_ids=known_broker_ids,
                        known_client_ids=known_client_ids,
                    )
                    observe("position", f"{position.exchange}:{position.token}", ownership,
                            exchange=position.exchange, token=position.token, symbol=position.symbol,
                            side="BUY" if position.quantity > 0 else "SELL",
                            quantity=position.quantity)
                for order in snapshot.orders.data or ():
                    if order.status.lower() in {
                        "complete", "completed", "filled", "cancelled", "canceled", "rejected", "expired"
                    }:
                        continue
                    ownership = classify_order_ownership(
                        order, known_broker_ids=known_broker_ids, known_client_ids=known_client_ids
                    )
                    if not order.status.strip():
                        ownership = ExposureOwnership.AMBIGUOUS
                    observe("order", order.order_id or f"unknown:{len(observations)}", ownership,
                            exchange=order.exchange, token=order.token, symbol=order.symbol,
                            side=order.side, quantity=order.quantity)
                for rule in snapshot.conditional_rules.data or ():
                    if not rule.active:
                        continue
                    ownership = classify_conditional_ownership(rule)
                    observe("conditional", rule.rule_id or f"unknown:{len(observations)}", ownership,
                            exchange=rule.exchange, token=rule.token, symbol=rule.symbol,
                            side=rule.side, quantity=rule.quantity)
                await self.repository.replace_exposure_observations(
                    user_id=snapshot.user_id, credential_revision=snapshot.credential_revision,
                    observations=observations,
                )
                owned = ownership_counts[ExposureOwnership.RULENIX_OWNED]
                ambiguous = ownership_counts[ExposureOwnership.AMBIGUOUS]
                manual = ownership_counts[ExposureOwnership.MANUAL_EXTERNAL]
                blockers = int(bool(owned or ambiguous))
                await self.repository.record_blockers(
                    snapshot, rulenix_owned_exposure=owned,
                    ambiguous_exposure=ambiguous, manual_external_exposure=manual,
                )
            return ReconciliationResult(snapshot.authoritative, healthy, blockers, incidents, "Authoritative reconciliation applied." if healthy else snapshot.failure_detail)

    async def classify_manual_close(self, *, snapshot: ReconciliationSnapshot, local_symbol: str, local_token: str, local_exchange: str, entry_at, remaining_quantity: int, conflicting_executable_sibling: bool = False, local_trade_count: int = 1, local_direction: str | None = None, trade_id: UUID | None = None, evidence_since: datetime | None = None, known_order_ids: frozenset[str] = frozenset()) -> tuple[ManualCloseClassification, Decimal | None]:
        if not snapshot.authoritative:
            return ManualCloseClassification.RECONCILIATION_REQUIRED, None
        broker_position = next((position for position in (snapshot.positions.data or ()) if position.token == local_token), None)
        fills = [fill for fill in (snapshot.fills.data or ()) if fill.token == local_token]
        classification = classify_manual_broker_close(local_open=True, local_symbol=local_symbol, local_token=local_token, local_exchange=local_exchange, entry_at=entry_at, remaining_quantity=remaining_quantity, broker_position=broker_position, fills=fills, order_book_succeeded=True, positions_succeeded=True, fills_succeeded=True, conflicting_executable_sibling=conflicting_executable_sibling, local_trade_count=local_trade_count, local_direction=local_direction, evidence_since=evidence_since, known_order_ids=known_order_ids)
        if classification is not ManualCloseClassification.MANUAL_BROKER_CLOSE:
            return classification, None
        attributable = [
            fill for fill in fills
            if fill.symbol == local_symbol and fill.token == local_token
            and fill.exchange == local_exchange and fill.filled_at >= entry_at
            and (evidence_since is None or fill.filled_at >= evidence_since)
            and not fill.owned_by_rulenix and fill.order_id not in known_order_ids
            and (local_direction is None or fill.side.upper() == ("SELL" if local_direction.upper() == "BUY" else "BUY"))
        ]
        price = weighted_fill_price(attributable)
        if trade_id is not None and price is not None:
            await self.repository.store_manual_close_evidence(ManualCloseEvidence(
                trade_id=trade_id, user_id=snapshot.user_id,
                broker_credential_revision=snapshot.credential_revision,
                exchange=local_exchange, token=local_token, symbol=local_symbol,
                close_side=attributable[0].side.upper(), filled_quantity=remaining_quantity,
                weighted_fill_price=price,
                broker_order_ids=tuple(sorted({fill.order_id for fill in attributable})),
                first_fill_at=min(fill.filled_at for fill in attributable),
                last_fill_at=max(fill.filled_at for fill in attributable),
                observed_at=datetime.now(UTC),
            ))
        return classification, price


__all__ = ["ReconciliationResult", "ReconciliationService"]
