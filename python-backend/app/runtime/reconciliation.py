"""Account-scoped production broker reads with explicit shadow isolation."""

from __future__ import annotations

from collections.abc import Awaitable, Callable, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime
from decimal import Decimal, InvalidOperation
from uuid import UUID
from zoneinfo import ZoneInfo

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from app.broker.angel.client import AngelClient
from app.broker.angel.errors import BrokerError, BrokerErrorCategory
from app.broker.angel.models import BrokerReadSuccess
from app.reconciliation.domain import (
    BrokerFill,
    BrokerOrder,
    BrokerPosition,
    ConditionalRule,
    EvidenceStatus,
    ReadEvidence,
    ReconciliationSnapshot,
)
from app.reconciliation.service import ReconciliationService

from .supervisor import RuntimeMode

IST = ZoneInfo("Asia/Kolkata")


@dataclass(frozen=True)
class ReconciliationCycle:
    accounts: int
    authoritative: int
    unhealthy: int
    blockers: int
    incidents: int
    shadow: bool

    def json(self) -> dict[str, object]:
        return {
            "accounts": self.accounts,
            "authoritative": self.authoritative,
            "unhealthy": self.unhealthy,
            "blockers": self.blockers,
            "incidents": self.incidents,
            "shadow": self.shadow,
        }


def _number(value: object, default: int = 0) -> int:
    try:
        return int(Decimal(str(value or default)))
    except (InvalidOperation, TypeError, ValueError):
        return default


def _decimal(value: object) -> Decimal | None:
    try:
        return Decimal(str(value)) if value not in (None, "") else None
    except (InvalidOperation, TypeError, ValueError):
        return None


def _time(value: object) -> datetime:
    if isinstance(value, datetime):
        return value if value.tzinfo else value.replace(tzinfo=IST)
    raw = str(value or "").strip()
    try:
        parsed = datetime.fromisoformat(raw)
        return parsed if parsed.tzinfo else parsed.replace(tzinfo=IST)
    except ValueError:
        pass
    for pattern in ("%d-%b-%Y %H:%M:%S", "%d/%m/%Y %H:%M:%S"):
        try:
            return datetime.strptime(raw, pattern).replace(tzinfo=IST)
        except ValueError:
            continue
    return datetime.now(UTC)


def _status(error: BrokerError) -> EvidenceStatus:
    if error.category in {
        BrokerErrorCategory.AUTHENTICATION_EXPIRED,
        BrokerErrorCategory.AUTHENTICATION_INVALID,
    }:
        return EvidenceStatus.AUTH_FAILED
    if error.category is BrokerErrorCategory.TIMEOUT:
        return EvidenceStatus.TIMED_OUT
    if error.category is BrokerErrorCategory.MALFORMED_RESPONSE:
        return EvidenceStatus.MALFORMED
    return EvidenceStatus.FAILED


def _evidence(result, mapper: Callable[[object], object], revision: int):
    if isinstance(result, BrokerReadSuccess):
        return ReadEvidence.success(
            tuple(mapper(item) for item in result.data), credential_revision=revision
        )
    error = result.error
    return ReadEvidence.failure(
        _status(error),
        f"{error.operation}:{error.category.value}",
        credential_revision=revision,
    )


def _position(item) -> BrokerPosition:
    return BrokerPosition(
        str(item.token or ""),
        str(item.symbol or ""),
        str(item.exchange or "").upper(),
        _number(item.net_quantity),
        _decimal(item.average_price),
    )


def _order(item) -> BrokerOrder:
    raw = item.raw
    return BrokerOrder(
        str(item.order_id or ""),
        str(raw.get("symboltoken") or raw.get("symbolToken") or ""),
        str(raw.get("tradingsymbol") or raw.get("tradingSymbol") or ""),
        str(raw.get("exchange") or "").upper(),
        str(item.transaction_type or "").upper(),
        str(item.status or ""),
        _number(item.quantity),
        _number(item.filled_quantity),
        _decimal(raw.get("averageprice") or raw.get("averagePrice")),
        client_order_id=str(raw.get("ordertag") or raw.get("orderTag") or ""),
        order_shape=str(item.order_type or ""),
        updated_at=_time(item.timestamp),
    )


def _fill(item) -> BrokerFill:
    raw = item.raw
    return BrokerFill(
        str(item.trade_id or ""),
        str(item.order_id or ""),
        str(raw.get("symboltoken") or raw.get("symbolToken") or ""),
        str(raw.get("tradingsymbol") or raw.get("tradingSymbol") or ""),
        str(raw.get("exchange") or "").upper(),
        str(raw.get("transactiontype") or raw.get("transactionType") or "").upper(),
        _number(item.quantity),
        _decimal(item.fill_price) or Decimal(0),
        _time(item.timestamp),
        order_tag=str(raw.get("ordertag") or raw.get("orderTag") or ""),
    )


def _conditional(item) -> ConditionalRule:
    raw = item.raw
    status = str(item.status or "").upper()
    return ConditionalRule(
        str(item.rule_id or ""),
        active=status not in {"CANCELLED", "CANCELED", "REJECTED", "EXPIRED", "COMPLETED"},
        order_id=item.broker_order_id,
        exchange=str(raw.get("exchange") or "").upper(),
        token=str(raw.get("symboltoken") or raw.get("symbolToken") or ""),
        symbol=str(raw.get("tradingsymbol") or raw.get("tradingSymbol") or ""),
        side=str(raw.get("transactiontype") or raw.get("transactionType") or "").upper(),
        quantity=_number(raw.get("qty") or raw.get("quantity")),
    )


class AccountReconciliationWorker:
    """Read every relevant account independently; shadow mode never applies writes."""

    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        client_factory: Callable[[UUID], Awaitable[AngelClient]],
        mode: RuntimeMode,
    ) -> None:
        self.session_factory = session_factory
        self.client_factory = client_factory
        self.mode = mode

    async def relevant_accounts(self) -> Sequence[UUID]:
        async with self.session_factory() as session:
            values = (
                await session.execute(
                    text("""
                    SELECT u.id
                      FROM users u
                      JOIN user_profiles p ON p.user_id=u.id
                     WHERE u.is_active=TRUE
                       AND (u.can_live_trade=TRUE OR p.trading_mode='live'
                         OR EXISTS(SELECT 1 FROM trades t WHERE t.user_id=u.id AND t.execution_mode='live' AND t.status='open')
                         OR EXISTS(SELECT 1 FROM strategy_orders o WHERE o.user_id=u.id AND o.execution_mode='live' AND o.status IN ('submitting','ambiguous','submitted','partially_filled','processing','cancelling')))
                     ORDER BY u.id
                    """)
                )
            ).scalars().all()
        return tuple(UUID(str(value)) for value in values)

    async def run_once(self) -> dict[str, object]:
        accounts = await self.relevant_accounts()
        authoritative = unhealthy = blockers = incidents = 0
        for user_id in accounts:
            try:
                snapshot = await self.read_account(user_id)
                if snapshot.authoritative:
                    authoritative += 1
                else:
                    unhealthy += 1
                if self.mode is RuntimeMode.AUTHORITATIVE:
                    async with self.session_factory() as session:
                        result = await ReconciliationService(session).apply_snapshot(snapshot)
                    blockers += result.blockers
                    incidents += result.incidents
            except Exception:
                unhealthy += 1
                if self.mode is RuntimeMode.AUTHORITATIVE:
                    await self._record_failure(user_id)
        return ReconciliationCycle(
            len(accounts), authoritative, unhealthy, blockers, incidents,
            self.mode is RuntimeMode.SHADOW,
        ).json()

    async def _record_failure(self, user_id: UUID) -> None:
        async with self.session_factory() as session, session.begin():
            await session.execute(
                text("""
                INSERT INTO broker_reconciliation_health(user_id,healthy,detail,checked_at,broker_credential_revision)
                VALUES(:user,FALSE,'Python broker reconciliation cycle failed closed.',NOW(),NULL)
                ON CONFLICT(user_id) DO UPDATE SET healthy=FALSE,detail=EXCLUDED.detail,
                  checked_at=NOW(),broker_credential_revision=NULL
                """),
                {"user": user_id},
            )

    async def read_account(self, user_id: UUID) -> ReconciliationSnapshot:
        client = await self.client_factory(user_id)
        revision = client.account.credential_revision
        try:
            positions, orders, fills, conditionals = await __import__("asyncio").gather(
                client.rest.safe_positions(),
                client.rest.safe_order_book(),
                client.rest.safe_trade_book(),
                client.rest.safe_conditional_rules(),
            )
            try:
                await client.rest.rms_limits()
                account = ReadEvidence.success(True, credential_revision=revision)
            except BrokerError as error:
                account = ReadEvidence.failure(
                    _status(error),
                    f"{error.operation}:{error.category.value}",
                    credential_revision=revision,
                )

            order_evidence = _evidence(orders, _order, revision)
            individual: dict[str, ReadEvidence[BrokerOrder | None]] = {}
            if order_evidence.status is EvidenceStatus.SUCCESS:
                broker_ids = {item.order_id for item in order_evidence.data or ()}
                async with self.session_factory() as session:
                    missing = (
                        await session.execute(
                            text("""
                            SELECT broker_order_id FROM strategy_orders
                             WHERE user_id=:user AND execution_mode='live'
                               AND broker_order_id<>''
                               AND status IN ('submitting','ambiguous','submitted','partially_filled','processing','cancelling')
                            """),
                            {"user": user_id},
                        )
                    ).scalars().all()
                for order_id in sorted({str(value) for value in missing} - broker_ids):
                    result = await client.rest.safe_individual_order(order_id)
                    if isinstance(result, BrokerReadSuccess):
                        individual[order_id] = ReadEvidence.success(
                            _order(result.data), credential_revision=revision
                        )
                    elif result.error.category is BrokerErrorCategory.ORDER_NOT_FOUND:
                        individual[order_id] = ReadEvidence.success(
                            None, credential_revision=revision
                        )
                    else:
                        individual[order_id] = ReadEvidence.failure(
                            _status(result.error),
                            f"{result.error.operation}:{result.error.category.value}",
                            credential_revision=revision,
                        )

            return ReconciliationSnapshot(
                user_id=user_id,
                account_id=client.account.client_code,
                credential_revision=revision,
                egress_identity=client.account.client_public_ip or "os-default",
                positions=_evidence(positions, _position, revision),
                orders=order_evidence,
                fills=_evidence(fills, _fill, revision),
                conditional_rules=_evidence(conditionals, _conditional, revision),
                account_validation=account,
                individual_orders=individual,
                current_credential_revision=revision,
            )
        finally:
            await client.close()


__all__ = ["AccountReconciliationWorker", "ReconciliationCycle"]
