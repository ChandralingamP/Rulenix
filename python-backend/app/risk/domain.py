"""Typed, broker-independent risk and trading-safety decisions.

This module deliberately contains no transport code.  It is the contract that a
future execution worker must evaluate immediately before a broker mutation.
"""

from collections.abc import Mapping
from dataclasses import dataclass, field
from datetime import UTC, datetime
from decimal import Decimal
from enum import Enum
from typing import TypedDict
from uuid import UUID


class ActionClass(str, Enum):
    INCREASE_EXPOSURE = "increase_exposure"
    REDUCE_RISK = "reduce_risk"
    NON_MUTATING = "non_mutating"


class ActionKind(str, Enum):
    ENTRY = "entry"
    SL2_REVERSAL = "sl2_reversal"
    STOP_LOSS = "stop_loss"
    TARGET = "target"
    MANUAL_CLOSE = "manual_close"
    EOD_SQUARE_OFF = "eod_square_off"
    EMERGENCY_CLOSE = "emergency_close"
    PROTECTION_RECOVERY = "protection_recovery"
    CANCEL_ENTRY = "cancel_entry"
    CLEAR_TRADES = "clear_trades"
    BROKER_READ = "broker_read"
    RECONCILIATION_READ = "reconciliation_read"


class ReasonCode(str, Enum):
    ALLOWED = "allowed"
    PROTECTIVE_EXIT_ALLOWED = "protective_exit_allowed"
    GLOBAL_KILL_SWITCH = "global_kill_switch"
    USER_KILL_SWITCH = "user_kill_switch"
    LIVE_NOT_PERMITTED = "live_not_permitted"
    LIVE_NOT_READY = "live_not_ready"
    BROKER_EVIDENCE_UNAVAILABLE = "broker_evidence_unavailable"
    CREDENTIAL_REVISION_MISMATCH = "credential_revision_mismatch"
    RECONCILIATION_STALE = "reconciliation_stale"
    EXPLICIT_EGRESS_UNAVAILABLE = "explicit_egress_unavailable"
    OPEN_LIVE_EXPOSURE = "open_live_exposure"
    PENDING_INTENT = "pending_intent"
    AMBIGUOUS_MUTATION = "ambiguous_mutation"
    PROTECTION_INCIDENT = "protection_incident"
    DUPLICATE_EXPOSURE = "duplicate_exposure"
    BROKER_INSTRUMENT_COLLISION = "broker_instrument_collision"
    OVER_CLOSE = "over_close"
    OWNERSHIP_MISMATCH = "ownership_mismatch"
    ACCOUNT_UNSAFE = "unsafe_account"
    UNSAFE_SNAPSHOT = "unsafe_snapshot"
    UNSAFE_MARKET_FEED = "unsafe_market_feed"
    INVALID_ORDER = "invalid_order"
    LIMIT_EXCEEDED = "limit_exceeded"
    CLEAR_TRADES_UNSAFE = "clear_trades_unsafe"
    INTERNAL_ERROR = "internal_error"


class _Identity(TypedDict):
    account_id: UUID | None
    user_id: UUID | None
    trade_id: UUID | None
    intent_id: UUID | None


@dataclass(frozen=True)
class SafetyDecision:
    allowed: bool
    action_class: ActionClass
    reason_code: ReasonCode
    human_reason: str
    blocking_conditions: tuple[str, ...] = ()
    account_id: UUID | None = None
    user_id: UUID | None = None
    trade_id: UUID | None = None
    intent_id: UUID | None = None
    evaluated_at: datetime | None = None
    details: Mapping[str, object] = field(default_factory=dict)

    @classmethod
    def allow(
        cls,
        action_class: ActionClass,
        *,
        account_id: UUID | None = None,
        user_id: UUID | None = None,
        trade_id: UUID | None = None,
        intent_id: UUID | None = None,
        reason_code: ReasonCode = ReasonCode.ALLOWED,
        human_reason: str = "Safety checks passed.",
        details: Mapping[str, object] | None = None,
    ) -> "SafetyDecision":
        return cls(
            True,
            action_class,
            reason_code,
            human_reason,
            (),
            account_id,
            user_id,
            trade_id,
            intent_id,
            datetime.now(UTC),
            details=details or {},
        )

    @classmethod
    def block(
        cls,
        action_class: ActionClass,
        reason_code: ReasonCode,
        human_reason: str,
        *,
        conditions: tuple[str, ...] = (),
        account_id: UUID | None = None,
        user_id: UUID | None = None,
        trade_id: UUID | None = None,
        intent_id: UUID | None = None,
        details: Mapping[str, object] | None = None,
    ) -> "SafetyDecision":
        return cls(
            False,
            action_class,
            reason_code,
            human_reason,
            conditions,
            account_id,
            user_id,
            trade_id,
            intent_id,
            datetime.now(UTC),
            details=details or {},
        )


@dataclass(frozen=True)
class SafetyRequest:
    user_id: UUID
    action: ActionKind | str
    execution_mode: str = "live"
    account_id: UUID | None = None
    trade_id: UUID | None = None
    intent_id: UUID | None = None
    order_id: UUID | None = None
    strategy_key: str | None = None
    instrument: str | None = None
    exchange_segment: str | None = None
    contract_token: str | None = None
    side: str | None = None
    quantity: int = 0
    attributable_quantity: int = 0
    lots: int = 0
    requested_egress_id: UUID | None = None
    snapshot_ready: bool = True
    snapshot_current: bool = True
    idempotency_key: str | None = None

    @property
    def kind(self) -> ActionKind:
        return ActionKind(self.action)


@dataclass(frozen=True)
class SafetyState:
    global_kill: bool = False
    user_kill: bool = False
    user_active: bool = True
    can_live_trade: bool = False
    trading_mode: str = "demo"
    token_status: str = ""
    credential_revision: int | None = None
    reconciled: bool = False
    reconciliation_revision: int | None = None
    reconciliation_checked_at: datetime | None = None
    explicit_egress: bool = False
    egress_ready: bool = True
    blockers: tuple[str, ...] = ()
    pending_intent: bool = False
    ambiguous_mutation: bool = False
    duplicate_exposure: bool = False
    broker_contract_collision: bool = False
    existing_quantity: int = 0
    limits: Mapping[str, Decimal | int | float | None] = field(default_factory=dict)
    projected: Mapping[str, Decimal | int | float] = field(default_factory=dict)
    owner_matches: bool = True
    trade_open: bool = True
    broker_evidence: bool = True
    internal_error: str | None = None


def classify_action(
    action: ActionKind | str,
    *,
    quantity: int = 0,
    attributable_quantity: int = 0,
    execution_mode: str = "live",
) -> ActionClass:
    """Classify by trading semantics, not by endpoint name."""
    kind = ActionKind(action)
    if kind in {ActionKind.BROKER_READ, ActionKind.RECONCILIATION_READ}:
        return ActionClass.NON_MUTATING
    if kind is ActionKind.SL2_REVERSAL or kind is ActionKind.ENTRY:
        return ActionClass.INCREASE_EXPOSURE
    if kind is ActionKind.CANCEL_ENTRY:
        return ActionClass.REDUCE_RISK
    if kind in {
        ActionKind.STOP_LOSS,
        ActionKind.TARGET,
        ActionKind.MANUAL_CLOSE,
        ActionKind.EOD_SQUARE_OFF,
        ActionKind.EMERGENCY_CLOSE,
        ActionKind.PROTECTION_RECOVERY,
    }:
        # Keep exit intent classification stable; quantity validation below
        # must report OVER_CLOSE rather than turning an unsafe exit into entry.
        return ActionClass.REDUCE_RISK
    if kind is ActionKind.CLEAR_TRADES:
        return (
            ActionClass.REDUCE_RISK
            if quantity <= max(attributable_quantity, 0)
            else ActionClass.INCREASE_EXPOSURE
        )
    return ActionClass.NON_MUTATING


def evaluate(request: SafetyRequest, state: SafetyState) -> SafetyDecision:
    """Pure safety evaluation used by both unit tests and the DB repository."""
    try:
        action_class = classify_action(
            request.action,
            quantity=request.quantity,
            attributable_quantity=request.attributable_quantity or state.existing_quantity,
            execution_mode=request.execution_mode,
        )
    except (TypeError, ValueError):
        return SafetyDecision.block(
            ActionClass.INCREASE_EXPOSURE,
            ReasonCode.INTERNAL_ERROR,
            "Unknown safety action; refusing to trade.",
            user_id=request.user_id,
        )

    common: _Identity = {
        "account_id": request.account_id,
        "user_id": request.user_id,
        "trade_id": request.trade_id,
        "intent_id": request.intent_id,
    }
    if state.internal_error:
        return SafetyDecision.block(
            action_class,
            ReasonCode.INTERNAL_ERROR,
            "Risk state could not be determined; action blocked.",
            conditions=(state.internal_error,),
            **common,
        )
    if action_class is ActionClass.NON_MUTATING:
        return SafetyDecision.allow(
            action_class,
            reason_code=ReasonCode.ALLOWED,
            human_reason="Non-mutating safety operation.",
            **common,
        )
    if request.lots < 0 or (
        request.quantity <= 0 and request.kind is not ActionKind.CANCEL_ENTRY
    ):
        return SafetyDecision.block(
            action_class, ReasonCode.INVALID_ORDER, "Quantity and lots must be positive.", **common
        )
    if not request.snapshot_ready or (
        action_class is ActionClass.INCREASE_EXPOSURE and not request.snapshot_current
    ):
        return SafetyDecision.block(
            action_class,
            ReasonCode.UNSAFE_SNAPSHOT,
            "Market snapshot is missing, stale, or unsafe.",
            **common,
        )

    if action_class is ActionClass.REDUCE_RISK:
        if request.kind is ActionKind.CANCEL_ENTRY:
            if not state.owner_matches:
                return SafetyDecision.block(
                    action_class,
                    ReasonCode.OWNERSHIP_MISMATCH,
                    "Order is not owned by this account.",
                    **common,
                )
            return SafetyDecision.allow(
                action_class,
                reason_code=ReasonCode.PROTECTIVE_EXIT_ALLOWED,
                human_reason="Cancellation reduces pending exposure.",
                **common,
            )
        if not state.owner_matches:
            return SafetyDecision.block(
                action_class,
                ReasonCode.OWNERSHIP_MISMATCH,
                "Trade is not owned by this account.",
                **common,
            )
        if not state.trade_open:
            return SafetyDecision.block(
                action_class,
                ReasonCode.OVER_CLOSE,
                "The attributable trade is already closed.",
                **common,
            )
        available = request.attributable_quantity or state.existing_quantity
        if available <= 0 or request.quantity > available:
            return SafetyDecision.block(
                action_class,
                ReasonCode.OVER_CLOSE,
                "Risk-reducing quantity exceeds attributable exposure.",
                **common,
            )
        return SafetyDecision.allow(
            action_class,
            reason_code=ReasonCode.PROTECTIVE_EXIT_ALLOWED,
            human_reason="Risk-reducing action remains eligible.",
            **common,
        )

    # Global/user pauses intentionally apply to entries, including SL2 reversal.
    if state.global_kill:
        return SafetyDecision.block(
            action_class,
            ReasonCode.GLOBAL_KILL_SWITCH,
            "Trading is paused by the global emergency kill switch.",
            **common,
        )
    if state.user_kill:
        return SafetyDecision.block(
            action_class,
            ReasonCode.USER_KILL_SWITCH,
            "Trading is paused for this account.",
            **common,
        )
    if request.execution_mode == "live":
        if not state.user_active or not state.can_live_trade or state.trading_mode != "live":
            return SafetyDecision.block(
                action_class,
                ReasonCode.LIVE_NOT_PERMITTED,
                "LIVE trading is not permitted for this account.",
                **common,
            )
        if not state.token_status or state.token_status not in {"success", "refreshed"}:
            return SafetyDecision.block(
                action_class,
                ReasonCode.ACCOUNT_UNSAFE,
                "Broker session credentials are not valid.",
                **common,
            )
        if not state.reconciled or not state.broker_evidence:
            return SafetyDecision.block(
                action_class,
                ReasonCode.BROKER_EVIDENCE_UNAVAILABLE,
                "Authoritative broker reconciliation evidence is unavailable.",
                **common,
            )
        if (
            state.credential_revision is None
            or state.reconciliation_revision != state.credential_revision
        ):
            return SafetyDecision.block(
                action_class,
                ReasonCode.CREDENTIAL_REVISION_MISMATCH,
                "Reconciliation belongs to an older credential revision.",
                **common,
            )
        if state.reconciliation_checked_at is None:
            return SafetyDecision.block(
                action_class, ReasonCode.LIVE_NOT_READY, "The account is not LIVE-ready.", **common
            )
        from datetime import UTC, timedelta

        if state.reconciliation_checked_at < datetime.now(UTC) - timedelta(minutes=5):
            return SafetyDecision.block(
                action_class,
                ReasonCode.RECONCILIATION_STALE,
                "Broker reconciliation is older than the production five-minute window.",
                **common,
            )
        if state.explicit_egress and not state.egress_ready:
            return SafetyDecision.block(
                action_class,
                ReasonCode.EXPLICIT_EGRESS_UNAVAILABLE,
                "The explicit broker egress assignment is unavailable.",
                **common,
            )
    if state.ambiguous_mutation:
        return SafetyDecision.block(
            action_class,
            ReasonCode.AMBIGUOUS_MUTATION,
            "An earlier broker mutation has an ambiguous outcome.",
            **common,
        )
    if state.pending_intent:
        return SafetyDecision.block(
            action_class,
            ReasonCode.PENDING_INTENT,
            "Durable execution work is already in progress.",
            **common,
        )
    if state.duplicate_exposure:
        return SafetyDecision.block(
            action_class,
            ReasonCode.DUPLICATE_EXPOSURE,
            "Equivalent exposure already exists or is pending.",
            **common,
        )
    if request.execution_mode == "live" and state.broker_contract_collision:
        return SafetyDecision.block(
            action_class,
            ReasonCode.BROKER_INSTRUMENT_COLLISION,
            "Manual or ambiguous broker exposure already exists for this exact contract.",
            **common,
        )
    if state.blockers:
        return SafetyDecision.block(
            action_class,
            ReasonCode.PROTECTION_INCIDENT,
            "Unresolved trading-safety blockers remain.",
            conditions=state.blockers,
            **common,
        )
    for key, limit in state.limits.items():
        if limit is not None and key in state.projected and state.projected[key] > limit:
            return SafetyDecision.block(
                action_class,
                ReasonCode.LIMIT_EXCEEDED,
                f"Configured {key} limit would be exceeded.",
                conditions=(key,),
                **common,
            )
    return SafetyDecision.allow(action_class, **common)


__all__ = [
    "ActionClass",
    "ActionKind",
    "ReasonCode",
    "SafetyDecision",
    "SafetyRequest",
    "SafetyState",
    "classify_action",
    "evaluate",
]
