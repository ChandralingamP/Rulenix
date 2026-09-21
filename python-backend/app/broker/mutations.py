"""Durable, authority-fenced Angel mutation coordination."""

from __future__ import annotations

import hashlib
import json
from collections.abc import Awaitable, Callable, Mapping
from dataclasses import dataclass
from decimal import Decimal
from enum import StrEnum
from uuid import UUID, uuid4

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from app.risk import ActionKind, RiskSafetyService, SafetyRequest

from .angel.client import AngelClient
from .angel.errors import BrokerError, BrokerErrorCategory
from .angel.models import CancelOrderRequest, OrderMutationRequest
from .authority import AuthorityError, AuthorityRuntime, LiveMutationAuthority


class MutationState(StrEnum):
    ACKNOWLEDGED = "acknowledged"
    AMBIGUOUS = "ambiguous"
    REJECTED = "rejected"
    FAILED = "failed"
    BLOCKED = "blocked"


class MutationPendingError(RuntimeError):
    """The same durable write is already unresolved and cannot be replayed."""


@dataclass(frozen=True)
class MutationOutcome:
    attempt_id: UUID
    state: MutationState
    broker_order_id: str = ""
    error_category: BrokerErrorCategory | None = None


def _fingerprint(payload: Mapping[str, object]) -> str:
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def _money(value) -> str:
    return f"{Decimal(str(value or 0)):.2f}"


def _action(role: str, operation: str) -> ActionKind:
    if operation == "cancel_order":
        return ActionKind.CANCEL_ENTRY
    return {
        "TARGET": ActionKind.TARGET,
        "SL1": ActionKind.STOP_LOSS,
        "SL2": ActionKind.STOP_LOSS,
        "EMERGENCY_CLOSE": ActionKind.EMERGENCY_CLOSE,
    }.get(role, ActionKind.ENTRY)


class LiveMutationCoordinator:
    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        authority: LiveMutationAuthority,
        client_factory: Callable[[UUID], Awaitable[AngelClient]],
        *,
        lease_owner: UUID,
        enabled: bool,
    ):
        self.session_factory = session_factory
        self.authority = authority
        self.client_factory = client_factory
        self.lease_owner = lease_owner
        self.enabled = enabled

    async def _order(self, session: AsyncSession, order_id: UUID, *, lock: bool = False):
        suffix = " FOR UPDATE OF o" if lock else ""
        return (
            await session.execute(
                text(f"""
                SELECT o.id,o.user_id,o.snapshot_id,o.trade_id,o.role,o.side,o.execution_mode,
                       o.lots,o.quantity,o.price,o.trigger_price,o.order_type,o.exchange_segment,
                       o.product_type,o.status,o.broker_order_id,o.client_order_id,o.idempotency_key,
                       s.strategy_key,s.instrument,s.contract_symbol,s.contract_token,
                       COALESCE(i.id,NULL) AS execution_intent_id
                  FROM strategy_orders o
                  JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
                  LEFT JOIN strategy_execution_intents i ON i.strategy_order_id=o.id
                 WHERE o.id=:order{suffix}
                """),
                {"order": order_id},
            )
        ).mappings().first()

    @staticmethod
    def _place_request(order) -> OrderMutationRequest:
        order_type = str(order["order_type"])
        return OrderMutationRequest(
            variety="STOPLOSS" if order_type.startswith("STOPLOSS") else "NORMAL",
            trading_symbol=str(order["contract_symbol"]),
            symbol_token=str(order["contract_token"]),
            transaction_type=str(order["side"]),
            exchange=str(order["exchange_segment"]),
            order_type=order_type,
            product_type=str(order["product_type"]),
            price="0" if order_type in {"MARKET", "STOPLOSS_MARKET"} else _money(order["price"]),
            quantity=int(order["quantity"]),
            trigger_price=_money(order["trigger_price"]),
            client_reference=str(order["client_order_id"]),
        )

    async def _prepare(
        self, *, order_id: UUID, operation: str, payload: Mapping[str, object]
    ) -> tuple[UUID, Mapping[str, object]]:
        async with self.session_factory() as session, session.begin():
            order = await self._order(session, order_id, lock=True)
            if order is None:
                raise ValueError("Strategy order does not exist.")
            if str(order["execution_mode"]) != "live":
                raise ValueError("Broker mutation requires a LIVE strategy order.")
            key = f"{order['idempotency_key']}:{operation}"
            existing = (
                await session.execute(
                    text("""
                    SELECT id,state,broker_order_id,network_started_at
                      FROM broker_mutation_attempts
                     WHERE idempotency_key=:key
                    """),
                    {"key": key},
                )
            ).mappings().first()
            if existing is not None:
                retryable_before_network = (
                    str(existing["state"]) in {"failed", "blocked"}
                    and existing["network_started_at"] is None
                )
                if not retryable_before_network:
                    raise MutationPendingError(
                        "A durable mutation attempt already exists and requires reconciliation."
                    )
                attempt_id = UUID(str(existing["id"]))
                await session.execute(
                    text("""
                    UPDATE broker_mutation_attempts
                       SET state='prepared',request_fingerprint=:fingerprint,
                           broker_error_class='',broker_error_code='',broker_http_status=NULL,
                           diagnostic='',completed_at=NULL,updated_at=NOW()
                     WHERE id=:id
                    """),
                    {"id": attempt_id, "fingerprint": _fingerprint(payload)},
                )
                if operation == "place_order" and str(order["status"]) == "failed":
                    await session.execute(
                        text("UPDATE strategy_orders SET status='pending',updated_at=NOW() WHERE id=:order"),
                        {"order": order_id},
                    )
                    order = dict(order)
                    order["status"] = "pending"
            else:
                attempt_id = uuid4()
                await session.execute(
                    text("""
                    INSERT INTO broker_mutation_attempts(
                        id,user_id,strategy_order_id,execution_intent_id,trade_id,operation,state,
                        idempotency_key,request_fingerprint,client_reference)
                    VALUES(:id,:user,:order,:intent,:trade,:operation,'prepared',:key,:fingerprint,:client)
                    """),
                    {
                        "id": attempt_id,
                        "user": order["user_id"],
                        "order": order_id,
                        "intent": order["execution_intent_id"],
                        "trade": order["trade_id"],
                        "operation": operation,
                        "key": key,
                        "fingerprint": _fingerprint(payload),
                        "client": order["client_order_id"],
                    },
                )
            expected = {"pending"} if operation == "place_order" else {
                "ambiguous", "submitted", "partially_filled", "processing"
            }
            if str(order["status"]) not in expected:
                raise MutationPendingError(
                    f"Order state {order['status']} is not eligible for {operation}."
                )
            target = "submitting" if operation == "place_order" else "cancelling"
            await session.execute(
                text("""
                UPDATE strategy_orders
                   SET status=:target,submission_attempts=submission_attempts+1,
                       state_version=state_version+1,updated_at=NOW()
                 WHERE id=:order
                """),
                {"target": target, "order": order_id},
            )
            await session.execute(
                text("""
                INSERT INTO broker_order_events(
                    order_id,user_id,from_state,to_state,event_type,diagnostic)
                VALUES(:order,:user,:from_state,:to_state,:event,:diagnostic)
                """),
                {
                    "order": order_id,
                    "user": order["user_id"],
                    "from_state": order["status"],
                    "to_state": target,
                    "event": "submission_prepared" if operation == "place_order" else "cancellation_prepared",
                    "diagnostic": f"attempt_id={attempt_id}",
                },
            )
            return attempt_id, order

    async def _mark_network(self, attempt_id: UUID, proof) -> None:
        async with self.session_factory() as session, session.begin():
            result = await session.execute(
                text("""
                UPDATE broker_mutation_attempts
                   SET state='submitting',authority_holder=:holder,authority_epoch=:epoch,
                       authority_lease_owner=:owner,network_started_at=NOW(),updated_at=NOW()
                 WHERE id=:id AND state='prepared'
                """),
                {
                    "id": attempt_id,
                    "holder": proof.runtime.value,
                    "epoch": proof.epoch,
                    "owner": proof.lease_owner,
                },
            )
            if getattr(result, "rowcount", 0) != 1:
                raise MutationPendingError("Mutation attempt is no longer prepared.")

    async def _finish(
        self,
        *,
        attempt_id: UUID,
        order_id: UUID,
        user_id: UUID,
        operation: str,
        state: MutationState,
        broker_order_id: str = "",
        error: BrokerError | None = None,
    ) -> None:
        order_target = {
            MutationState.ACKNOWLEDGED: "submitted" if operation == "place_order" else "cancelled",
            MutationState.AMBIGUOUS: "ambiguous" if operation == "place_order" else "submitted",
            MutationState.REJECTED: "rejected" if operation == "place_order" else "submitted",
            MutationState.FAILED: "failed" if operation == "place_order" else "submitted",
            MutationState.BLOCKED: "failed" if operation == "place_order" else "submitted",
        }[state]
        async with self.session_factory() as session, session.begin():
            await session.execute(
                text("""
                UPDATE broker_mutation_attempts
                   SET state=CAST(:state AS varchar(24)),broker_order_id=:broker_order_id,
                       broker_error_class=:error_class,broker_error_code=:error_code,
                       broker_http_status=:http_status,diagnostic=:diagnostic,
                       completed_at=CASE WHEN CAST(:state AS varchar(24)) IN ('acknowledged','rejected','failed','blocked')
                                         THEN NOW() ELSE NULL END,
                       updated_at=NOW()
                 WHERE id=:id AND state IN ('prepared','submitting')
                """),
                {
                    "id": attempt_id,
                    "state": state.value,
                    "broker_order_id": broker_order_id,
                    "error_class": error.category.value if error else "",
                    "error_code": error.code or "" if error else "",
                    "http_status": error.status_code if error else None,
                    "diagnostic": (error.diagnostic or error.message)[:2000] if error else "",
                },
            )
            await session.execute(
                text("""
                UPDATE strategy_orders
                   SET status=CAST(:status AS varchar(16)),
                       broker_order_id=CASE WHEN CAST(:broker_id AS varchar(96))='' THEN broker_order_id ELSE CAST(:broker_id AS varchar(96)) END,
                       broker_error_class=:error_class,broker_error_code=:error_code,
                       broker_http_status=:http_status,
                       uncertain_since_at=CASE WHEN CAST(:status AS varchar(16))='ambiguous' THEN COALESCE(uncertain_since_at,NOW()) ELSE uncertain_since_at END,
                       state_version=state_version+1,updated_at=NOW()
                 WHERE id=:order
                """),
                {
                    "order": order_id,
                    "status": order_target,
                    "broker_id": broker_order_id,
                    "error_class": error.category.value if error else "",
                    "error_code": error.code or "" if error else "",
                    "http_status": error.status_code if error else None,
                },
            )
            await session.execute(
                text("""
                INSERT INTO broker_order_events(
                    order_id,user_id,from_state,to_state,event_type,broker_order_id,
                    error_class,error_code,http_status,diagnostic)
                VALUES(:order,:user,:from_state,:to_state,:event,:broker_id,
                       :error_class,:error_code,:http_status,:diagnostic)
                """),
                {
                    "order": order_id,
                    "user": user_id,
                    "from_state": "submitting" if operation == "place_order" else "cancelling",
                    "to_state": order_target,
                    "event": f"{operation}_{state.value}",
                    "broker_id": broker_order_id,
                    "error_class": error.category.value if error else "",
                    "error_code": error.code or "" if error else "",
                    "http_status": error.status_code if error else None,
                    "diagnostic": (error.diagnostic or error.message)[:2000] if error else "",
                },
            )

    async def _safety(self, order, order_id: UUID, operation: str):
        async with self.session_factory() as session:
            return await RiskSafetyService(session).final_pre_mutation_check(
                SafetyRequest(
                    user_id=UUID(str(order["user_id"])),
                    action=_action(str(order["role"]), operation),
                    execution_mode="live",
                    trade_id=UUID(str(order["trade_id"])) if order["trade_id"] else None,
                    intent_id=UUID(str(order["execution_intent_id"])) if order["execution_intent_id"] else None,
                    order_id=order_id,
                    strategy_key=str(order["strategy_key"]),
                    instrument=str(order["instrument"]),
                    exchange_segment=str(order["exchange_segment"]),
                    contract_token=str(order["contract_token"]),
                    side=str(order["side"]),
                    quantity=int(order["quantity"]),
                    attributable_quantity=int(order["quantity"]),
                    lots=int(order["lots"]),
                    idempotency_key=str(order["idempotency_key"]),
                )
            )

    async def place_order(self, order_id: UUID) -> MutationOutcome:
        if not self.enabled:
            raise MutationPendingError("Python LIVE mutation is disabled by configuration.")
        async with self.session_factory() as session:
            initial = await self._order(session, order_id)
        if initial is None:
            raise ValueError("Strategy order does not exist.")
        request = self._place_request(initial)
        attempt_id, order = await self._prepare(
            order_id=order_id, operation="place_order", payload=request.angel_payload()
        )
        user_id = UUID(str(order["user_id"]))
        client: AngelClient | None = None
        try:
            async with self.authority.mutation_permit(
                runtime=AuthorityRuntime.PYTHON,
                lease_owner=self.lease_owner,
                user_id=user_id,
            ) as proof:
                decision = await self._safety(order, order_id, "place_order")
                if not decision.allowed:
                    await self._finish(
                        attempt_id=attempt_id,
                        order_id=order_id,
                        user_id=user_id,
                        operation="place_order",
                        state=MutationState.BLOCKED,
                    )
                    return MutationOutcome(attempt_id, MutationState.BLOCKED)
                await self._mark_network(attempt_id, proof)
                client = await self.client_factory(user_id)
                response = await client.place_order(request, proof=proof)
            await self._finish(
                attempt_id=attempt_id,
                order_id=order_id,
                user_id=user_id,
                operation="place_order",
                state=MutationState.ACKNOWLEDGED,
                broker_order_id=response.broker_order_id,
            )
            return MutationOutcome(
                attempt_id, MutationState.ACKNOWLEDGED, response.broker_order_id
            )
        except BrokerError as error:
            state = (
                MutationState.AMBIGUOUS
                if error.category in {
                    BrokerErrorCategory.AMBIGUOUS,
                    BrokerErrorCategory.TIMEOUT,
                }
                else MutationState.REJECTED
                if error.category in {
                    BrokerErrorCategory.BROKER_REJECTED,
                    BrokerErrorCategory.ORDER_NOT_FOUND,
                }
                else MutationState.FAILED
            )
            await self._finish(
                attempt_id=attempt_id,
                order_id=order_id,
                user_id=user_id,
                operation="place_order",
                state=state,
                error=error,
            )
            return MutationOutcome(attempt_id, state, error_category=error.category)
        except AuthorityError:
            await self._finish(
                attempt_id=attempt_id,
                order_id=order_id,
                user_id=user_id,
                operation="place_order",
                state=MutationState.BLOCKED,
            )
            return MutationOutcome(attempt_id, MutationState.BLOCKED)
        finally:
            if client is not None:
                await client.close()

    async def cancel_order(self, order_id: UUID, *, variety: str) -> MutationOutcome:
        if not self.enabled:
            raise MutationPendingError("Python LIVE mutation is disabled by configuration.")
        async with self.session_factory() as session:
            initial = await self._order(session, order_id)
        if initial is None or not str(initial["broker_order_id"]):
            raise ValueError("Cancellation requires a known broker order ID.")
        request = CancelOrderRequest(
            variety=variety, order_id=str(initial["broker_order_id"])
        )
        attempt_id, order = await self._prepare(
            order_id=order_id, operation="cancel_order", payload=request.angel_payload()
        )
        user_id = UUID(str(order["user_id"]))
        client: AngelClient | None = None
        try:
            async with self.authority.mutation_permit(
                runtime=AuthorityRuntime.PYTHON,
                lease_owner=self.lease_owner,
                user_id=user_id,
            ) as proof:
                decision = await self._safety(order, order_id, "cancel_order")
                if not decision.allowed:
                    await self._finish(
                        attempt_id=attempt_id,
                        order_id=order_id,
                        user_id=user_id,
                        operation="cancel_order",
                        state=MutationState.BLOCKED,
                    )
                    return MutationOutcome(attempt_id, MutationState.BLOCKED)
                await self._mark_network(attempt_id, proof)
                client = await self.client_factory(user_id)
                response = await client.cancel_order(request, proof=proof)
            await self._finish(
                attempt_id=attempt_id,
                order_id=order_id,
                user_id=user_id,
                operation="cancel_order",
                state=MutationState.ACKNOWLEDGED,
                broker_order_id=response.broker_order_id,
            )
            return MutationOutcome(
                attempt_id, MutationState.ACKNOWLEDGED, response.broker_order_id
            )
        except BrokerError as error:
            # A cancellation failure never proves the original order terminal.
            state = MutationState.AMBIGUOUS
            await self._finish(
                attempt_id=attempt_id,
                order_id=order_id,
                user_id=user_id,
                operation="cancel_order",
                state=state,
                error=error,
            )
            return MutationOutcome(attempt_id, state, error_category=error.category)
        except AuthorityError:
            await self._finish(
                attempt_id=attempt_id,
                order_id=order_id,
                user_id=user_id,
                operation="cancel_order",
                state=MutationState.BLOCKED,
            )
            return MutationOutcome(attempt_id, MutationState.BLOCKED)
        finally:
            if client is not None:
                await client.close()


__all__ = [
    "LiveMutationCoordinator",
    "MutationOutcome",
    "MutationPendingError",
    "MutationState",
]
