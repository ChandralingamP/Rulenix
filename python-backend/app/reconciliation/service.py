from __future__ import annotations

from dataclasses import dataclass, replace
from datetime import UTC, datetime
from decimal import Decimal
from uuid import UUID

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from app.trading.domain import manual_broker_close_pnl

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
                await self._reconcile_local_orders(snapshot)
                broker_positions = {item.token: item for item in (snapshot.positions.data or ())}
                local = (
                    (
                        await self.session.execute(
                            text("""
                    SELECT t.id,t.strategy_key,t.instrument_label,t.status,t.direction,t.quantity,t.execution_mode,
                           t.entry_price,t.pnl,t.entry_datetime,t.safety_status,
                           s.exchange_segment,s.contract_token,s.contract_symbol,s.lot_size
                    FROM trades t LEFT JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
                    WHERE t.user_id=:user AND t.execution_mode='live' AND (
                      t.status='open' OR t.safety_status<>'CLOSED' OR EXISTS(
                        SELECT 1 FROM broker_position_incidents i
                         WHERE i.user_id=t.user_id AND i.trade_id=t.id
                           AND i.status IN ('open','operator_required')
                      ))
                """),
                            {"user": snapshot.user_id},
                        )
                    )
                    .mappings()
                    .all()
                )
                local_contracts = frozenset(
                    (str(item["exchange_segment"] or "").upper(), str(item["contract_token"] or ""))
                    for item in local
                    if item["contract_token"]
                )
                known_broker_ids = frozenset(
                    str(value)
                    for value in (
                        await self.session.execute(
                            text("""
                    SELECT broker_order_id FROM strategy_orders
                     WHERE user_id=:user AND execution_mode='live' AND broker_order_id<>''
                """),
                            {"user": snapshot.user_id},
                        )
                    )
                    .scalars()
                    .all()
                )
                known_client_ids = frozenset(
                    str(value)
                    for value in (
                        await self.session.execute(
                            text("""
                    SELECT client_order_id FROM strategy_orders
                     WHERE user_id=:user AND execution_mode='live' AND client_order_id<>''
                """),
                            {"user": snapshot.user_id},
                        )
                    )
                    .scalars()
                    .all()
                )
                for trade in local:
                    broker = (
                        broker_positions.get(str(trade["contract_token"]))
                        if trade["contract_token"]
                        else None
                    )
                    quantity = int(broker.quantity) if broker else 0
                    symbol = str(trade["contract_symbol"] or "")
                    token = str(trade["contract_token"] or "")
                    expected_quantity = int(trade["quantity"] or 0) * (
                        1 if str(trade["direction"]).upper() == "BUY" else -1
                    )
                    if trade["status"] == "closed":
                        expected_quantity = 0
                    if quantity == 0 and expected_quantity == 0 and broker is None:
                        await self.session.execute(
                            text(
                                "UPDATE trades SET broker_net_quantity=0,last_position_reconciled_at=NOW(),updated_at=NOW() WHERE id=:trade AND user_id=:user"
                            ),
                            {"trade": trade["id"], "user": snapshot.user_id},
                        )
                        await self._resolve_position_incidents(snapshot.user_id, trade)
                        continue
                    if quantity == expected_quantity and broker is not None:
                        await self.session.execute(
                            text(
                                "UPDATE trades SET broker_net_quantity=:quantity,broker_average_price=:price,last_position_reconciled_at=NOW(),updated_at=NOW() WHERE id=:trade AND user_id=:user"
                            ),
                            {
                                "quantity": quantity,
                                "price": float(broker.average_price)
                                if broker and broker.average_price is not None
                                else None,
                                "trade": trade["id"],
                                "user": snapshot.user_id,
                            },
                        )
                        await self._resolve_position_incidents(snapshot.user_id, trade)
                        continue
                    manual_intent = (
                        (
                            await self.session.execute(
                                text("""
                            SELECT requested_at FROM manual_trade_close_intents
                             WHERE trade_id=:trade AND status<>'completed'
                            """),
                                {"trade": trade["id"]},
                            )
                        )
                        .mappings()
                        .first()
                    )
                    if manual_intent is not None and broker is None:
                        local_count = int(
                            await self.session.scalar(
                                text("""
                                SELECT COUNT(*) FROM trades t
                                JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
                                 WHERE t.user_id=:user AND t.execution_mode='live' AND t.status='open'
                                   AND UPPER(s.exchange_segment)=UPPER(:exchange)
                                   AND s.contract_token=:token
                                """),
                                {
                                    "user": snapshot.user_id,
                                    "exchange": trade["exchange_segment"],
                                    "token": trade["contract_token"],
                                },
                            )
                            or 0
                        )
                        conflicting = bool(
                            await self.session.scalar(
                                text("""
                                SELECT EXISTS(SELECT 1 FROM strategy_orders
                                 WHERE trade_id=:trade AND status IN
                                   ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))
                                """),
                                {"trade": trade["id"]},
                            )
                        )
                        classification, price = await self.classify_manual_close(
                            snapshot=snapshot,
                            local_symbol=symbol,
                            local_token=token,
                            local_exchange=str(trade["exchange_segment"] or "").upper(),
                            entry_at=trade["entry_datetime"],
                            remaining_quantity=int(trade["quantity"] or 0),
                            conflicting_executable_sibling=conflicting,
                            local_trade_count=local_count,
                            local_direction=str(trade["direction"]),
                            trade_id=UUID(str(trade["id"])),
                            evidence_since=manual_intent["requested_at"],
                            known_order_ids=known_broker_ids,
                        )
                        if (
                            classification is ManualCloseClassification.MANUAL_BROKER_CLOSE
                            and price is not None
                        ):
                            pnl = manual_broker_close_pnl(
                                direction=str(trade["direction"]),
                                entry_price=trade["entry_price"],
                                weighted_exit_price=price,
                                quantity=int(trade["quantity"]),
                                current_pnl=Decimal(str(trade["pnl"] or 0)),
                                instrument=str(trade["instrument_label"]),
                                lot_size=trade["lot_size"],
                            )
                            await self.session.execute(
                                text("""
                                UPDATE trades SET status='closed',safety_status='CLOSED',remaining_lots=0,
                                  broker_net_quantity=0,last_position_reconciled_at=NOW(),exit_price=:price,
                                  last_price=:price,pnl=:pnl,exit_datetime=NOW(),
                                  exit_reason='MANUAL_BROKER_CLOSE',updated_at=NOW() WHERE id=:trade
                                """),
                                {"price": price, "pnl": pnl, "trade": trade["id"]},
                            )
                            await self.session.execute(
                                text("""
                                UPDATE manual_trade_close_intents SET status='completed',last_error='',
                                  completed_at=NOW(),updated_at=NOW() WHERE trade_id=:trade
                                """),
                                {"trade": trade["id"]},
                            )
                            await self.repository.consume_manual_close_evidence(
                                trade_id=UUID(str(trade["id"])),
                                user_id=snapshot.user_id,
                                credential_revision=snapshot.credential_revision,
                                consumed_at=datetime.now(UTC),
                            )
                            await self._resolve_position_incidents(snapshot.user_id, trade)
                            continue
                    incidents += 1
                    await self.repository.record_position_incident(
                        user_id=snapshot.user_id,
                        strategy_key=str(trade["strategy_key"] or ""),
                        instrument=str(trade["instrument_label"] or ""),
                        exchange_segment=str(trade["exchange_segment"] or ""),
                        contract_token=token,
                        contract_symbol=symbol,
                        incident_type="BROKER_FLAT_LOCAL_OPEN"
                        if broker is None
                        else "POSITION_QUANTITY_MISMATCH",
                        broker_quantity=quantity,
                        local_quantity=int(trade["quantity"] or 0),
                        detail="Authoritative broker position does not match local exposure.",
                        trade_id=trade["id"],
                        broker_average_price=float(broker.average_price)
                        if broker and broker.average_price is not None
                        else None,
                    )
                    await self.session.execute(
                        text(
                            "UPDATE trades SET safety_status='RECONCILIATION_REQUIRED',broker_net_quantity=:quantity,broker_average_price=:price,last_position_reconciled_at=NOW(),updated_at=NOW() WHERE id=:trade AND user_id=:user"
                        ),
                        {
                            "quantity": quantity,
                            "price": float(broker.average_price)
                            if broker and broker.average_price is not None
                            else None,
                            "trade": trade["id"],
                            "user": snapshot.user_id,
                        },
                    )
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

    async def _resolve_position_incidents(self, user_id: UUID, trade) -> None:
        await self.session.execute(
            text("""
            UPDATE broker_position_incidents SET status='resolved',resolved_at=NOW(),last_detected_at=NOW()
             WHERE user_id=:user AND UPPER(exchange_segment)=UPPER(:exchange)
               AND contract_token=:token AND status IN ('open','operator_required')
            """),
            {
                "user": user_id,
                "exchange": trade["exchange_segment"],
                "token": trade["contract_token"],
            },
        )

    async def _reconcile_local_orders(self, snapshot: ReconciliationSnapshot) -> None:
        """Advance local order evidence without initiating or retrying a broker write."""
        local = (
            (
                await self.session.execute(
                    text("""
                SELECT id,status,broker_order_id,client_order_id,filled_quantity,processed_quantity
                  FROM strategy_orders
                 WHERE user_id=:user AND execution_mode='live'
                   AND status IN ('submitting','ambiguous','submitted','partially_filled','processing','cancelling')
                 ORDER BY created_at FOR UPDATE
                """),
                    {"user": snapshot.user_id},
                )
            )
            .mappings()
            .all()
        )
        book = tuple(snapshot.orders.data or ())
        for order in local:
            broker = next(
                (
                    item
                    for item in book
                    if (order["broker_order_id"] and item.order_id == order["broker_order_id"])
                    or (
                        order["client_order_id"]
                        and item.client_order_id == order["client_order_id"]
                    )
                ),
                None,
            )
            if broker is None and order["broker_order_id"]:
                evidence = snapshot.individual_orders.get(str(order["broker_order_id"]))
                if evidence is not None and evidence.status.successful:
                    broker = evidence.data
            if broker is None:
                continue
            status = broker.status.strip().lower().replace(" ", "_")
            cumulative = max(int(order["filled_quantity"] or 0), int(broker.filled_quantity or 0))
            processed = int(order["processed_quantity"] or 0)
            current = str(order["status"])
            if current == "submitting":
                # Preserve the database's acknowledgement transition before
                # a later fill worker consumes the durable watermark.
                target = "submitted"
            elif cumulative > processed:
                target = "partially_filled"
            elif status in {"complete", "completed", "filled"}:
                target = "filled"
            elif status in {"cancelled", "canceled", "rejected", "expired"}:
                target = "rejected" if status == "rejected" else "cancelled"
            elif order["status"] == "cancelling":
                target = "cancelling"
            else:
                target = "submitted"
            allowed = {
                "submitting": {
                    "submitted",
                    "ambiguous",
                    "partially_filled",
                    "filled",
                    "rejected",
                    "cancelled",
                },
                "ambiguous": {
                    "submitted",
                    "partially_filled",
                    "filled",
                    "rejected",
                    "cancelled",
                    "cancelling",
                },
                "submitted": {
                    "submitted",
                    "partially_filled",
                    "filled",
                    "rejected",
                    "cancelled",
                    "cancelling",
                },
                "partially_filled": {
                    "submitted",
                    "partially_filled",
                    "filled",
                    "rejected",
                    "cancelled",
                    "cancelling",
                },
                "processing": {
                    "submitted",
                    "partially_filled",
                    "filled",
                    "rejected",
                    "cancelled",
                    "cancelling",
                },
                "cancelling": {
                    "submitted",
                    "partially_filled",
                    "processing",
                    "filled",
                    "rejected",
                    "cancelled",
                    "cancelling",
                },
            }
            if target not in allowed.get(current, set()):
                target = current
            fill_prices = [
                fill
                for fill in (snapshot.fills.data or ())
                if fill.order_id == broker.order_id and fill.quantity > 0
            ]
            average_price = broker.average_price
            if average_price is None and fill_prices:
                average_price = weighted_fill_price(fill_prices)
            await self.session.execute(
                text("""
                UPDATE strategy_orders
                   SET status=CAST(:status AS varchar(16)),broker_order_id=CASE WHEN broker_order_id='' THEN CAST(:broker AS varchar(96)) ELSE broker_order_id END,
                       filled_quantity=GREATEST(filled_quantity,:filled),
                       average_fill_price=COALESCE(:price,average_fill_price),
                       broker_status=:broker_status,last_reconciled_at=NOW(),
                       broker_error_class=CASE WHEN CAST(:status AS varchar(16))<>'ambiguous' THEN '' ELSE broker_error_class END,
                       state_version=state_version+1,updated_at=NOW()
                 WHERE id=:order
                """),
                {
                    "status": target,
                    "broker": broker.order_id,
                    "filled": cumulative,
                    "price": average_price,
                    "broker_status": broker.status,
                    "order": order["id"],
                },
            )
            if broker.order_id:
                await self.session.execute(
                    text("""
                    UPDATE broker_mutation_attempts
                       SET state='acknowledged',broker_order_id=:broker,completed_at=COALESCE(completed_at,NOW()),
                           diagnostic='Acknowledged by authoritative broker reconciliation.',updated_at=NOW()
                     WHERE strategy_order_id=:order AND operation='place_order'
                       AND state IN ('prepared','submitting','ambiguous')
                    """),
                    {"broker": broker.order_id, "order": order["id"]},
                )
            if target == "cancelled":
                await self.session.execute(
                    text("""
                    UPDATE broker_mutation_attempts
                       SET state='acknowledged',completed_at=COALESCE(completed_at,NOW()),
                           diagnostic='Cancellation confirmed by authoritative broker reconciliation.',updated_at=NOW()
                     WHERE strategy_order_id=:order AND operation='cancel_order'
                       AND state IN ('prepared','submitting','ambiguous')
                    """),
                    {"order": order["id"]},
                )

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
