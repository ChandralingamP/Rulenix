from dataclasses import dataclass
from decimal import Decimal, InvalidOperation
from enum import StrEnum
from uuid import UUID


class TradeMode(StrEnum):
    DEMO = "demo"
    LIVE = "live"


class Side(StrEnum):
    BUY = "BUY"
    SELL = "SELL"

    @property
    def opposite(self) -> "Side":
        return Side.SELL if self is Side.BUY else Side.BUY


class TradeStatus(StrEnum):
    OPEN = "open"
    CLOSED = "closed"


class ProtectionStatus(StrEnum):
    DEMO = "DEMO"
    PROTECTION_REQUIRED = "PROTECTION_REQUIRED"
    PROTECTION_SUBMITTING = "PROTECTION_SUBMITTING"
    PROTECTION_UNCERTAIN = "PROTECTION_UNCERTAIN"
    PROTECTED = "PROTECTED"
    PROTECTION_FAILED = "PROTECTION_FAILED"
    CLOSING = "CLOSING"
    EMERGENCY_CLOSING = "EMERGENCY_CLOSING"
    RECONCILIATION_REQUIRED = "RECONCILIATION_REQUIRED"
    CLOSED = "CLOSED"


class ReconciliationStatus(StrEnum):
    HEALTHY = "healthy"
    UNHEALTHY = "unhealthy"
    REQUIRED = "reconciliation_required"
    UNKNOWN = "unknown"


class OrderRole(StrEnum):
    BUY_ENTRY = "BUY_ENTRY"
    SELL_ENTRY = "SELL_ENTRY"
    TARGET = "TARGET"
    SL1 = "SL1"
    SL2 = "SL2"
    EMERGENCY_CLOSE = "EMERGENCY_CLOSE"


class OrderStatus(StrEnum):
    PENDING = "pending"
    SUBMITTING = "submitting"
    AMBIGUOUS = "ambiguous"
    SUBMITTED = "submitted"
    PARTIALLY_FILLED = "partially_filled"
    PROCESSING = "processing"
    FILLED = "filled"
    FAILED = "failed"
    REJECTED = "rejected"
    CANCELLING = "cancelling"
    CANCELLED = "cancelled"


class IntentStatus(StrEnum):
    PENDING = "pending"
    CLAIMED = "claimed"
    RETRY_WAIT = "retry_wait"
    SUBMITTED = "submitted"
    COMPLETED = "completed"
    SKIPPED = "skipped"
    FAILED = "failed"
    EXPIRED = "expired"


class ReversalStatus(StrEnum):
    PENDING = "pending"
    PROCESSING = "processing"
    WAITING = "waiting"
    SUBMITTED = "submitted"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


class ManualCloseStatus(StrEnum):
    REQUESTED = "requested"
    CANCELLING_PROTECTION = "cancelling_protection"
    SUBMITTED = "submitted"
    PARTIALLY_FILLED = "partially_filled"
    AMBIGUOUS = "ambiguous"
    COMPLETED = "completed"
    FAILED = "failed"
    RECONCILIATION_REQUIRED = "reconciliation_required"


class ExitReason(StrEnum):
    MARKET_CLOSED = "MARKET_CLOSED"
    SIGNAL_REVERSAL = "SIGNAL_REVERSAL"
    MANUAL_RULENIX_CLOSE = "MANUAL_RULENIX_CLOSE"
    EMERGENCY_CLOSE = "EMERGENCY_CLOSE"
    TP1 = "TP1"
    SL1 = "SL1"
    SL2 = "SL2"
    TP = "TP"
    SL = "SL"
    MANUAL_BROKER_CLOSE = "MANUAL_BROKER_CLOSE"


class ExposureOrigin(StrEnum):
    STRATEGY_ENTRY = "strategy_entry"
    BROKER_OVER_CLOSE = "broker_over_close"


ORDER_TRANSITIONS: dict[OrderStatus, frozenset[OrderStatus]] = {
    OrderStatus.PENDING: frozenset({OrderStatus.SUBMITTING, OrderStatus.SUBMITTED, OrderStatus.FAILED, OrderStatus.REJECTED, OrderStatus.CANCELLED}),
    OrderStatus.SUBMITTING: frozenset({OrderStatus.SUBMITTED, OrderStatus.AMBIGUOUS, OrderStatus.FAILED, OrderStatus.REJECTED, OrderStatus.CANCELLED}),
    OrderStatus.AMBIGUOUS: frozenset({OrderStatus.SUBMITTED, OrderStatus.PARTIALLY_FILLED, OrderStatus.PROCESSING, OrderStatus.FILLED, OrderStatus.REJECTED, OrderStatus.CANCELLED, OrderStatus.CANCELLING}),
    OrderStatus.SUBMITTED: frozenset({OrderStatus.PARTIALLY_FILLED, OrderStatus.PROCESSING, OrderStatus.FILLED, OrderStatus.REJECTED, OrderStatus.CANCELLED, OrderStatus.CANCELLING}),
    OrderStatus.PARTIALLY_FILLED: frozenset({OrderStatus.SUBMITTED, OrderStatus.PROCESSING, OrderStatus.FILLED, OrderStatus.REJECTED, OrderStatus.CANCELLED, OrderStatus.CANCELLING}),
    OrderStatus.PROCESSING: frozenset({OrderStatus.SUBMITTED, OrderStatus.PARTIALLY_FILLED, OrderStatus.FILLED, OrderStatus.REJECTED, OrderStatus.CANCELLED, OrderStatus.CANCELLING}),
    OrderStatus.CANCELLING: frozenset({OrderStatus.SUBMITTED, OrderStatus.PARTIALLY_FILLED, OrderStatus.PROCESSING, OrderStatus.FILLED, OrderStatus.REJECTED, OrderStatus.CANCELLED}),
    OrderStatus.FAILED: frozenset({OrderStatus.PENDING}),
}

SAFETY_TRANSITIONS: dict[ProtectionStatus, frozenset[ProtectionStatus]] = {
    ProtectionStatus.DEMO: frozenset({ProtectionStatus.DEMO, ProtectionStatus.CLOSED}),
    ProtectionStatus.PROTECTION_REQUIRED: frozenset({ProtectionStatus.PROTECTION_SUBMITTING, ProtectionStatus.PROTECTION_UNCERTAIN, ProtectionStatus.PROTECTED, ProtectionStatus.PROTECTION_FAILED, ProtectionStatus.CLOSING, ProtectionStatus.EMERGENCY_CLOSING, ProtectionStatus.RECONCILIATION_REQUIRED, ProtectionStatus.CLOSED}),
    ProtectionStatus.PROTECTION_SUBMITTING: frozenset({ProtectionStatus.PROTECTION_SUBMITTING, ProtectionStatus.PROTECTION_UNCERTAIN, ProtectionStatus.PROTECTED, ProtectionStatus.PROTECTION_FAILED, ProtectionStatus.CLOSING, ProtectionStatus.EMERGENCY_CLOSING, ProtectionStatus.RECONCILIATION_REQUIRED, ProtectionStatus.CLOSED}),
    ProtectionStatus.PROTECTION_UNCERTAIN: frozenset({ProtectionStatus.PROTECTION_SUBMITTING, ProtectionStatus.PROTECTION_UNCERTAIN, ProtectionStatus.PROTECTION_FAILED, ProtectionStatus.CLOSING, ProtectionStatus.EMERGENCY_CLOSING, ProtectionStatus.RECONCILIATION_REQUIRED, ProtectionStatus.CLOSED}),
    ProtectionStatus.PROTECTED: frozenset({ProtectionStatus.PROTECTION_REQUIRED, ProtectionStatus.CLOSING, ProtectionStatus.EMERGENCY_CLOSING, ProtectionStatus.RECONCILIATION_REQUIRED, ProtectionStatus.CLOSED}),
    ProtectionStatus.PROTECTION_FAILED: frozenset({ProtectionStatus.PROTECTION_SUBMITTING, ProtectionStatus.PROTECTION_UNCERTAIN, ProtectionStatus.EMERGENCY_CLOSING, ProtectionStatus.RECONCILIATION_REQUIRED, ProtectionStatus.CLOSED}),
    ProtectionStatus.CLOSING: frozenset({ProtectionStatus.CLOSING, ProtectionStatus.EMERGENCY_CLOSING, ProtectionStatus.RECONCILIATION_REQUIRED, ProtectionStatus.CLOSED}),
    ProtectionStatus.EMERGENCY_CLOSING: frozenset({ProtectionStatus.EMERGENCY_CLOSING, ProtectionStatus.RECONCILIATION_REQUIRED, ProtectionStatus.CLOSED}),
    ProtectionStatus.RECONCILIATION_REQUIRED: frozenset({ProtectionStatus.RECONCILIATION_REQUIRED, ProtectionStatus.PROTECTION_SUBMITTING, ProtectionStatus.EMERGENCY_CLOSING, ProtectionStatus.CLOSED}),
    ProtectionStatus.CLOSED: frozenset({ProtectionStatus.CLOSED}),
}

INTENT_TRANSITIONS: dict[IntentStatus, frozenset[IntentStatus]] = {
    IntentStatus.PENDING: frozenset({IntentStatus.CLAIMED, IntentStatus.RETRY_WAIT, IntentStatus.SKIPPED, IntentStatus.EXPIRED, IntentStatus.FAILED}),
    IntentStatus.CLAIMED: frozenset({IntentStatus.SUBMITTED, IntentStatus.COMPLETED, IntentStatus.SKIPPED, IntentStatus.RETRY_WAIT, IntentStatus.FAILED}),
    IntentStatus.RETRY_WAIT: frozenset({IntentStatus.CLAIMED, IntentStatus.EXPIRED, IntentStatus.FAILED}),
    IntentStatus.SUBMITTED: frozenset({IntentStatus.COMPLETED, IntentStatus.SKIPPED, IntentStatus.FAILED, IntentStatus.RETRY_WAIT}),
    IntentStatus.COMPLETED: frozenset({IntentStatus.COMPLETED}),
    IntentStatus.SKIPPED: frozenset({IntentStatus.SKIPPED}),
    IntentStatus.FAILED: frozenset({IntentStatus.FAILED, IntentStatus.RETRY_WAIT}),
    IntentStatus.EXPIRED: frozenset({IntentStatus.EXPIRED}),
}

REVERSAL_TRANSITIONS: dict[ReversalStatus, frozenset[ReversalStatus]] = {
    ReversalStatus.PENDING: frozenset({ReversalStatus.PROCESSING, ReversalStatus.WAITING, ReversalStatus.CANCELLED, ReversalStatus.FAILED}),
    ReversalStatus.PROCESSING: frozenset({ReversalStatus.WAITING, ReversalStatus.SUBMITTED, ReversalStatus.COMPLETED, ReversalStatus.FAILED, ReversalStatus.CANCELLED}),
    ReversalStatus.WAITING: frozenset({ReversalStatus.PROCESSING, ReversalStatus.FAILED, ReversalStatus.CANCELLED}),
    ReversalStatus.SUBMITTED: frozenset({ReversalStatus.COMPLETED, ReversalStatus.FAILED, ReversalStatus.CANCELLED}),
    ReversalStatus.COMPLETED: frozenset({ReversalStatus.COMPLETED}),
    ReversalStatus.FAILED: frozenset({ReversalStatus.PROCESSING, ReversalStatus.FAILED, ReversalStatus.CANCELLED}),
    ReversalStatus.CANCELLED: frozenset({ReversalStatus.CANCELLED}),
}

MANUAL_CLOSE_TRANSITIONS: dict[ManualCloseStatus, frozenset[ManualCloseStatus]] = {
    ManualCloseStatus.REQUESTED: frozenset({ManualCloseStatus.CANCELLING_PROTECTION, ManualCloseStatus.SUBMITTED, ManualCloseStatus.FAILED, ManualCloseStatus.RECONCILIATION_REQUIRED}),
    ManualCloseStatus.CANCELLING_PROTECTION: frozenset({ManualCloseStatus.SUBMITTED, ManualCloseStatus.AMBIGUOUS, ManualCloseStatus.FAILED, ManualCloseStatus.RECONCILIATION_REQUIRED}),
    ManualCloseStatus.SUBMITTED: frozenset({ManualCloseStatus.PARTIALLY_FILLED, ManualCloseStatus.COMPLETED, ManualCloseStatus.AMBIGUOUS, ManualCloseStatus.FAILED, ManualCloseStatus.RECONCILIATION_REQUIRED}),
    ManualCloseStatus.PARTIALLY_FILLED: frozenset({ManualCloseStatus.PARTIALLY_FILLED, ManualCloseStatus.COMPLETED, ManualCloseStatus.AMBIGUOUS, ManualCloseStatus.FAILED, ManualCloseStatus.RECONCILIATION_REQUIRED}),
    ManualCloseStatus.AMBIGUOUS: frozenset({ManualCloseStatus.SUBMITTED, ManualCloseStatus.PARTIALLY_FILLED, ManualCloseStatus.COMPLETED, ManualCloseStatus.FAILED, ManualCloseStatus.RECONCILIATION_REQUIRED}),
    ManualCloseStatus.COMPLETED: frozenset({ManualCloseStatus.COMPLETED}),
    ManualCloseStatus.FAILED: frozenset({ManualCloseStatus.FAILED, ManualCloseStatus.RECONCILIATION_REQUIRED}),
    ManualCloseStatus.RECONCILIATION_REQUIRED: frozenset({ManualCloseStatus.RECONCILIATION_REQUIRED, ManualCloseStatus.SUBMITTED, ManualCloseStatus.COMPLETED}),
}


def _enum(value: str | StrEnum, cls: type[StrEnum]) -> StrEnum:
    try:
        return cls(value)
    except ValueError as exc:
        raise ValueError(f"Unknown {cls.__name__} value: {value!r}") from exc


def validate_order_transition(current: str | OrderStatus, new: str | OrderStatus) -> None:
    old = OrderStatus(current)
    target = OrderStatus(new)
    if old != target and target not in ORDER_TRANSITIONS.get(old, frozenset()):
        raise ValueError(f"Invalid strategy order transition {old.value} -> {target.value}.")


def validate_safety_transition(current: str | ProtectionStatus, new: str | ProtectionStatus) -> None:
    old = ProtectionStatus(current)
    target = ProtectionStatus(new)
    if target not in SAFETY_TRANSITIONS[old]:
        raise ValueError(f"Invalid trade safety transition {old.value} -> {target.value}.")


def validate_trade_transition(current: str | TradeStatus, new: str | TradeStatus) -> None:
    old, target = TradeStatus(current), TradeStatus(new)
    if old is TradeStatus.CLOSED and target is not old:
        raise ValueError("Closed trade cannot regress.")
    if old is TradeStatus.OPEN and target not in {TradeStatus.OPEN, TradeStatus.CLOSED}:
        raise ValueError(f"Invalid trade transition {old.value} -> {target.value}.")


def validate_intent_transition(current: str | IntentStatus, new: str | IntentStatus) -> None:
    old, target = IntentStatus(current), IntentStatus(new)
    if target not in INTENT_TRANSITIONS[old]:
        raise ValueError(f"Invalid intent transition {old.value} -> {target.value}.")


def validate_reversal_transition(current: str | ReversalStatus, new: str | ReversalStatus) -> None:
    old, target = ReversalStatus(current), ReversalStatus(new)
    if target not in REVERSAL_TRANSITIONS[old]:
        raise ValueError(f"Invalid reversal transition {old.value} -> {target.value}.")


def validate_manual_close_transition(current: str | ManualCloseStatus, new: str | ManualCloseStatus) -> None:
    old, target = ManualCloseStatus(current), ManualCloseStatus(new)
    if target not in MANUAL_CLOSE_TRANSITIONS[old]:
        raise ValueError(f"Invalid manual-close transition {old.value} -> {target.value}.")


def futures_pnl_units(instrument: str, quantity: int, lot_size: int | None) -> Decimal:
    quantity = max(quantity, 0)
    if instrument not in {"GOLDTEN", "GOLDM", "SILVERM", "SILVERMIC", "NATGASMINI"}:
        return Decimal(quantity)
    multipliers = {"GOLDM": Decimal(10), "GOLDTEN": Decimal(1), "SILVERM": Decimal(5), "SILVERMIC": Decimal(1), "NATGASMINI": Decimal(250)}
    lots = Decimal(quantity) / Decimal(max(lot_size or 1, 1))
    return lots * multipliers[instrument]


def trade_pnl(direction: str | Side, entry: Decimal | str, exit: Decimal | str, units: Decimal | str) -> Decimal:
    side = Side(direction)
    try:
        entry_value, exit_value, unit_value = Decimal(str(entry)), Decimal(str(exit)), Decimal(str(units))
    except (InvalidOperation, ValueError) as exc:
        raise ValueError("P&L inputs must be finite decimal values.") from exc
    return (exit_value - entry_value if side is Side.BUY else entry_value - exit_value) * unit_value


@dataclass(frozen=True)
class FillApplication:
    cumulative_quantity: int
    delta_quantity: int
    average_price: Decimal
    status: OrderStatus


@dataclass(frozen=True)
class ExitApplication:
    closed_quantity: int
    remaining_quantity: int
    realized_pnl: Decimal
    trade_status: TradeStatus


def apply_fill(*, order_quantity: int, processed_quantity: int, filled_quantity: int, average_price: Decimal | str | None, observed_cumulative: int, fill_price: Decimal | str) -> FillApplication:
    if order_quantity <= 0 or processed_quantity < 0 or filled_quantity < 0 or observed_cumulative < 0:
        raise ValueError("Order and fill quantities must be non-negative, with a positive order quantity.")
    if observed_cumulative > order_quantity:
        raise ValueError("Broker cumulative fill exceeds the ordered quantity.")
    previous = max(filled_quantity, processed_quantity)
    cumulative = max(previous, observed_cumulative)
    delta = max(0, cumulative - processed_quantity)
    price = Decimal(str(fill_price))
    if delta == 0:
        weighted = Decimal(str(average_price)) if average_price is not None else price
    elif processed_quantity > 0 and average_price is not None:
        weighted = (Decimal(str(average_price)) * processed_quantity + price * delta) / cumulative
    else:
        weighted = price
    status = OrderStatus.FILLED if cumulative >= order_quantity else (OrderStatus.PARTIALLY_FILLED if cumulative > 0 else OrderStatus.SUBMITTED)
    return FillApplication(cumulative, delta, weighted, status)


def apply_exit_fill(*, direction: str | Side, entry_price: Decimal | str, current_pnl: Decimal | str, trade_quantity: int, exit_quantity: int, exit_price: Decimal | str, instrument: str, lot_size: int | None) -> ExitApplication:
    if trade_quantity < 0 or exit_quantity < 0:
        raise ValueError("Trade and exit quantities cannot be negative.")
    closed = min(trade_quantity, exit_quantity)
    units = futures_pnl_units(instrument, closed, lot_size)
    realized = Decimal(str(current_pnl)) + trade_pnl(direction, entry_price, exit_price, units)
    remaining = trade_quantity - closed
    return ExitApplication(closed, remaining, realized, TradeStatus.CLOSED if remaining == 0 else TradeStatus.OPEN)


def manual_broker_close_pnl(*, direction: str | Side, entry_price: Decimal | str, weighted_exit_price: Decimal | str, quantity: int, current_pnl: Decimal | str, instrument: str, lot_size: int | None) -> Decimal:
    """Compute the Rust ``MANUAL_BROKER_CLOSE`` result from attributed fills."""
    return trade_pnl(direction, entry_price, weighted_exit_price, futures_pnl_units(instrument, quantity, lot_size)) + Decimal(str(current_pnl))


def sl2_reversal(source_direction: str | Side, original_lots: int) -> tuple[Side, OrderRole, int] | None:
    if original_lots <= 0:
        return None
    side = Side(source_direction)
    reversal = side.opposite
    role = OrderRole.BUY_ENTRY if reversal is Side.BUY else OrderRole.SELL_ENTRY
    return reversal, role, original_lots


def manual_close_session(trade_id: UUID) -> str:
    return f"mc-{trade_id.hex[:16]}"


def sl2_reversal_session(trade_id: UUID) -> str:
    return f"r-{trade_id.hex[:30]}"
