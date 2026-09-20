"""Typed, fail-closed broker reconciliation values.

The broker client deliberately returns read results, while this module gives those
results a durable meaning.  In particular, a failed read is never represented by
an empty list.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from datetime import datetime, timezone
from decimal import Decimal
from enum import StrEnum
from typing import TypeVar
from uuid import UUID


class EvidenceStatus(StrEnum):
    SUCCESS = "success"
    FAILED = "failed"
    TIMED_OUT = "timed_out"
    AUTH_FAILED = "auth_failed"
    MALFORMED = "malformed"
    UNAVAILABLE = "unavailable"

    @property
    def successful(self) -> bool:
        return self is EvidenceStatus.SUCCESS


class ExposureOwnership(StrEnum):
    RULENIX_OWNED = "rulenix_owned"
    MANUAL_EXTERNAL = "manual_external"
    AMBIGUOUS = "ambiguous"


T = TypeVar("T")


@dataclass(frozen=True)
class ReadEvidence[T]:
    status: EvidenceStatus
    data: T | None = None
    observed_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    credential_revision: int | None = None
    error: str = ""

    def __post_init__(self) -> None:
        if self.observed_at.tzinfo is None:
            raise ValueError("Evidence timestamps must be timezone-aware.")
        if self.status is EvidenceStatus.SUCCESS and self.data is None:
            raise ValueError("Successful evidence must include data, including an empty collection.")
        if self.status is not EvidenceStatus.SUCCESS and self.error == "":
            object.__setattr__(self, "error", self.status.value)

    @classmethod
    def success(cls, data: T, *, credential_revision: int | None = None, observed_at: datetime | None = None) -> ReadEvidence[T]:
        return cls(EvidenceStatus.SUCCESS, data, observed_at or datetime.now(timezone.utc), credential_revision)

    @classmethod
    def failure(cls, status: EvidenceStatus, error: str, *, credential_revision: int | None = None, observed_at: datetime | None = None) -> ReadEvidence[T]:
        if status is EvidenceStatus.SUCCESS:
            raise ValueError("Use success() for successful evidence.")
        return cls(status, None, observed_at or datetime.now(timezone.utc), credential_revision, error)


@dataclass(frozen=True)
class BrokerPosition:
    token: str
    symbol: str
    exchange: str
    quantity: int
    average_price: Decimal | None = None


@dataclass(frozen=True)
class BrokerOrder:
    order_id: str
    token: str = ""
    symbol: str = ""
    exchange: str = ""
    side: str = ""
    status: str = ""
    quantity: int = 0
    filled_quantity: int = 0
    average_price: Decimal | None = None
    owned_by_rulenix: bool = False
    android_synthetic: bool = False
    order_shape: str = ""
    updated_at: datetime | None = None
    client_order_id: str = ""
    ownership: ExposureOwnership | None = None


@dataclass(frozen=True)
class BrokerFill:
    trade_id: str
    order_id: str
    token: str
    symbol: str
    exchange: str
    side: str
    quantity: int
    price: Decimal
    filled_at: datetime
    owned_by_rulenix: bool = False
    order_tag: str = ""


@dataclass(frozen=True)
class ConditionalRule:
    rule_id: str
    active: bool = True
    order_id: str | None = None
    exchange: str = ""
    token: str = ""
    symbol: str = ""
    side: str = ""
    quantity: int = 0


@dataclass(frozen=True)
class DeploymentDecision:
    deployment_safe: bool
    live_ready: bool
    classification: str
    local_unresolved: int


@dataclass(frozen=True)
class ManualCloseEvidence:
    trade_id: UUID
    user_id: UUID
    broker_credential_revision: int
    exchange: str
    token: str
    symbol: str
    close_side: str
    filled_quantity: int
    weighted_fill_price: Decimal
    broker_order_ids: tuple[str, ...]
    first_fill_at: datetime
    last_fill_at: datetime
    observed_at: datetime
    consumed_at: datetime | None = None


@dataclass(frozen=True)
class ReconciliationSnapshot:
    """An account-scoped snapshot whose authority is explicit and reviewable."""

    user_id: UUID
    account_id: str
    credential_revision: int
    egress_identity: str
    positions: ReadEvidence[Sequence[BrokerPosition]]
    orders: ReadEvidence[Sequence[BrokerOrder]]
    fills: ReadEvidence[Sequence[BrokerFill]]
    conditional_rules: ReadEvidence[Sequence[ConditionalRule]]
    account_validation: ReadEvidence[bool]
    individual_orders: Mapping[str, ReadEvidence[BrokerOrder | None]] = field(default_factory=dict)
    current_credential_revision: int | None = None

    @property
    def evidence(self) -> tuple[ReadEvidence[object], ...]:
        return (self.positions, self.orders, self.fills, self.conditional_rules, self.account_validation)  # type: ignore[return-value]

    @property
    def order_book(self) -> ReadEvidence[Sequence[BrokerOrder]]:
        return self.orders

    @property
    def trade_book(self) -> ReadEvidence[Sequence[BrokerFill]]:
        return self.fills

    @property
    def conditional_inventory(self) -> ReadEvidence[Sequence[ConditionalRule]]:
        return self.conditional_rules

    @property
    def authoritative(self) -> bool:
        return bool(
            self.account_id
            and self.egress_identity
            and all(item.status is EvidenceStatus.SUCCESS for item in self.evidence)
            and all(item.credential_revision == self.credential_revision for item in self.evidence)
            and bool(self.account_validation.data)
            and (self.current_credential_revision is None or self.current_credential_revision == self.credential_revision)
        )

    @property
    def failure_detail(self) -> str:
        return "; ".join(f"{name}={item.status.value}:{item.error}" for name, item in (
            ("positions", self.positions), ("orders", self.orders), ("fills", self.fills),
            ("conditional_rules", self.conditional_rules), ("account", self.account_validation),
        ) if item.status is not EvidenceStatus.SUCCESS)


class PositionClassification(StrEnum):
    MATCHED = "matched"
    BROKER_FLAT_LOCAL_OPEN = "broker_flat_local_open"
    BROKER_OPEN_LOCAL_CLOSED = "broker_open_local_closed"
    QUANTITY_MISMATCH = "quantity_mismatch"
    SIDE_OR_SYMBOL_MISMATCH = "side_or_symbol_mismatch"
    UNKNOWN = "unknown"


class OrderClassification(StrEnum):
    MATCHED = "matched"
    FILLED = "filled"
    PARTIAL = "partial"
    REJECTED = "rejected"
    CANCELLED = "cancelled"
    MISSING = "missing"
    AMBIGUOUS = "ambiguous"


class ManualCloseClassification(StrEnum):
    MANUAL_BROKER_CLOSE = "MANUAL_BROKER_CLOSE"
    RECONCILIATION_REQUIRED = "RECONCILIATION_REQUIRED"


class OcoClassification(StrEnum):
    SYNTHETIC_ANDROID_OCO = "SYNTHETIC_ANDROID_OCO"
    UNKNOWN_UNSAFE = "UNKNOWN_UNSAFE"


def classify_position(*, local_quantity: int, local_symbol: str, local_token: str, broker: BrokerPosition | None, local_open: bool) -> PositionClassification:
    if broker is None:
        return PositionClassification.BROKER_FLAT_LOCAL_OPEN if local_open else PositionClassification.MATCHED
    if not local_open:
        return PositionClassification.BROKER_OPEN_LOCAL_CLOSED
    if broker.symbol != local_symbol or broker.token != local_token:
        return PositionClassification.SIDE_OR_SYMBOL_MISMATCH
    if broker.quantity != local_quantity:
        return PositionClassification.QUANTITY_MISMATCH
    return PositionClassification.MATCHED


def classify_order(*, local_status: str, broker: BrokerOrder | None, read_succeeded: bool) -> OrderClassification:
    if not read_succeeded:
        return OrderClassification.AMBIGUOUS
    if broker is None:
        return OrderClassification.MISSING
    status = broker.status.lower()
    if status in {"rejected", "reject", "error"}:
        return OrderClassification.REJECTED
    if status in {"cancelled", "canceled"}:
        return OrderClassification.CANCELLED
    if broker.filled_quantity >= broker.quantity > 0 or status in {"complete", "filled"}:
        return OrderClassification.FILLED
    if broker.filled_quantity > 0 or status in {"partial", "partially filled", "partially_filled"}:
        return OrderClassification.PARTIAL
    return OrderClassification.MATCHED


def weighted_fill_price(fills: Sequence[BrokerFill]) -> Decimal:
    if not fills or any(fill.quantity <= 0 for fill in fills):
        raise ValueError("At least one positive attributable fill is required.")
    quantity = sum(fill.quantity for fill in fills)
    return sum((fill.price * fill.quantity for fill in fills), Decimal(0)) / quantity


def classify_manual_broker_close(
    *,
    local_open: bool,
    local_symbol: str,
    local_token: str,
    local_exchange: str,
    entry_at: datetime,
    remaining_quantity: int,
    broker_position: BrokerPosition | None,
    fills: Sequence[BrokerFill],
    order_book_succeeded: bool,
    positions_succeeded: bool,
    fills_succeeded: bool,
    conflicting_executable_sibling: bool = False,
    local_trade_count: int = 1,
    local_direction: str | None = None,
    evidence_since: datetime | None = None,
    known_order_ids: frozenset[str] = frozenset(),
) -> ManualCloseClassification:
    if not (local_open and local_trade_count == 1 and positions_succeeded and order_book_succeeded and fills_succeeded):
        return ManualCloseClassification.RECONCILIATION_REQUIRED
    if broker_position is not None and broker_position.quantity != 0:
        return ManualCloseClassification.RECONCILIATION_REQUIRED
    attributable = [
        fill for fill in fills
        if fill.symbol == local_symbol and fill.token == local_token and fill.exchange == local_exchange
        and fill.filled_at >= entry_at and fill.quantity > 0
        and (evidence_since is None or fill.filled_at >= evidence_since)
        and not fill.owned_by_rulenix
        and fill.order_id not in known_order_ids
        and (local_direction is None or fill.side.upper() == ("SELL" if local_direction.upper() == "BUY" else "BUY"))
    ]
    if conflicting_executable_sibling or sum(fill.quantity for fill in attributable) != remaining_quantity:
        return ManualCloseClassification.RECONCILIATION_REQUIRED
    if not attributable:
        return ManualCloseClassification.RECONCILIATION_REQUIRED
    return ManualCloseClassification.MANUAL_BROKER_CLOSE


def _rulenix_order_tag(value: str) -> bool:
    normalized = value.strip().upper()
    return (
        len(normalized) == 20
        and normalized.startswith("RX")
        and all(character in "0123456789ABCDEF" for character in normalized[2:])
    )


def classify_order_ownership(
    order: BrokerOrder,
    *,
    known_broker_ids: frozenset[str] = frozenset(),
    known_client_ids: frozenset[str] = frozenset(),
) -> ExposureOwnership:
    if order.ownership is not None:
        return order.ownership
    if order.order_id in known_broker_ids or (
        order.client_order_id and order.client_order_id in known_client_ids
    ):
        return ExposureOwnership.RULENIX_OWNED
    if order.owned_by_rulenix:
        return ExposureOwnership.RULENIX_OWNED
    if _rulenix_order_tag(order.client_order_id):
        return ExposureOwnership.AMBIGUOUS
    if (
        order.order_id
        and order.exchange
        and order.token
        and order.side.upper() in {"BUY", "SELL"}
        and order.quantity > 0
    ):
        return ExposureOwnership.MANUAL_EXTERNAL
    return ExposureOwnership.AMBIGUOUS


def classify_position_ownership(
    position: BrokerPosition,
    *,
    fills: Sequence[BrokerFill],
    orders: Sequence[BrokerOrder],
    local_contracts: frozenset[tuple[str, str]] = frozenset(),
    known_broker_ids: frozenset[str] = frozenset(),
    known_client_ids: frozenset[str] = frozenset(),
) -> ExposureOwnership:
    contract = (position.exchange.upper(), position.token)
    if contract in local_contracts:
        return ExposureOwnership.RULENIX_OWNED
    order_by_id = {order.order_id: order for order in orders if order.order_id}
    totals = {ownership: 0 for ownership in ExposureOwnership}
    matched = 0
    for fill in fills:
        if (fill.exchange.upper(), fill.token) != contract:
            continue
        matched += 1
        source = order_by_id.get(fill.order_id)
        if source is not None:
            ownership = classify_order_ownership(
                source,
                known_broker_ids=known_broker_ids,
                known_client_ids=known_client_ids,
            )
        elif (
            fill.owned_by_rulenix
            or fill.order_id in known_broker_ids
            or (fill.order_tag and fill.order_tag in known_client_ids)
        ):
            ownership = ExposureOwnership.RULENIX_OWNED
        elif _rulenix_order_tag(fill.order_tag):
            ownership = ExposureOwnership.AMBIGUOUS
        elif fill.order_id:
            ownership = ExposureOwnership.MANUAL_EXTERNAL
        else:
            ownership = ExposureOwnership.AMBIGUOUS
        if fill.side.upper() not in {"BUY", "SELL"}:
            return ExposureOwnership.AMBIGUOUS
        signed = fill.quantity if fill.side.upper() == "BUY" else -fill.quantity
        totals[ownership] += signed
    if (
        matched
        and totals[ExposureOwnership.RULENIX_OWNED] == position.quantity
        and totals[ExposureOwnership.MANUAL_EXTERNAL] == 0
        and totals[ExposureOwnership.AMBIGUOUS] == 0
    ):
        return ExposureOwnership.RULENIX_OWNED
    if (
        matched
        and totals[ExposureOwnership.MANUAL_EXTERNAL] == position.quantity
        and totals[ExposureOwnership.RULENIX_OWNED] == 0
        and totals[ExposureOwnership.AMBIGUOUS] == 0
    ):
        return ExposureOwnership.MANUAL_EXTERNAL
    return ExposureOwnership.AMBIGUOUS


def classify_conditional_ownership(rule: ConditionalRule) -> ExposureOwnership:
    if rule.rule_id and rule.exchange and rule.token and rule.quantity > 0:
        return ExposureOwnership.MANUAL_EXTERNAL
    return ExposureOwnership.AMBIGUOUS


def deployment_account_decision(
    *,
    broker_readable: bool,
    broker_exposure_observed: bool,
    rulenix_owned_exposure: int,
    ambiguous_exposure: int,
    local_unresolved: int,
) -> DeploymentDecision:
    local_unresolved = max(local_unresolved, 0)
    if broker_readable:
        safe = (
            rulenix_owned_exposure <= 0
            and ambiguous_exposure <= 0
            and local_unresolved == 0
        )
        classification = (
            "readable_rulenix_owned_exposure"
            if rulenix_owned_exposure > 0
            else "readable_ambiguous_exposure"
            if ambiguous_exposure > 0
            else "readable_local_unresolved_live_state"
            if local_unresolved
            else "readable_safe"
        )
        return DeploymentDecision(safe, safe, classification, local_unresolved)
    if broker_exposure_observed:
        return DeploymentDecision(
            False, False, "unreadable_broker_exposure_observed", local_unresolved
        )
    safe = local_unresolved == 0
    return DeploymentDecision(
        safe,
        False,
        "offline_locally_flat" if safe else "offline_with_unresolved_live_state",
        local_unresolved,
    )


def exact_contract_collision(
    *,
    execution_mode: str,
    exchange: str,
    token: str,
    observations: Sequence[tuple[str, str, ExposureOwnership]],
) -> bool:
    if execution_mode != "live":
        return False
    contract = (exchange.upper(), token)
    return any(
        (observed_exchange.upper(), observed_token) == contract
        and ownership in {ExposureOwnership.MANUAL_EXTERNAL, ExposureOwnership.AMBIGUOUS}
        for observed_exchange, observed_token, ownership in observations
    )


def is_synthetic_android_oco(*, order: BrokerOrder, position: ReadEvidence[Sequence[BrokerPosition]], individual: ReadEvidence[BrokerOrder | None], conditional_rules: ReadEvidence[Sequence[ConditionalRule]], fills: ReadEvidence[Sequence[BrokerFill]], executable_sibling: bool) -> bool:
    """Exact conjunction used by the production safety classifier.

    The individual lookup must specifically return AB1007; generic failures are
    unsafe and are intentionally not treated as "not found".
    """
    if not order.android_synthetic or order.order_shape != "android_synthetic_oco":
        return False
    if any(item.status is not EvidenceStatus.SUCCESS for item in (position, conditional_rules, fills)):
        return False
    # SmartAPI represents a proven-absent individual order as a failed lookup
    # with the exact AB1007 error.  A timeout/auth/malformed response is unsafe.
    if individual.status is not EvidenceStatus.FAILED or individual.error != "AB1007 Order not found":
        return False
    if any(item.quantity != 0 for item in (position.data or ())):
        return False
    if any(rule.active and rule.order_id == order.order_id for rule in (conditional_rules.data or ())):
        return False
    if any(fill.order_id == order.order_id for fill in (fills.data or ())):
        return False
    return not executable_sibling


__all__ = [name for name in globals() if not name.startswith("_")]
