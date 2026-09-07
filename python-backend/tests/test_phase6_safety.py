from datetime import UTC, datetime, timedelta
from uuid import uuid4

from app.risk.domain import (
    ActionClass,
    ActionKind,
    ReasonCode,
    SafetyRequest,
    SafetyState,
    classify_action,
    evaluate,
)


def live_state(**changes):
    state = SafetyState(
        user_active=True,
        can_live_trade=True,
        trading_mode="live",
        token_status="success",
        credential_revision=10,
        reconciled=True,
        reconciliation_revision=10,
        reconciliation_checked_at=datetime.now(UTC),
        broker_evidence=True,
    )
    return SafetyState(**{**state.__dict__, **changes})


def entry(**changes):
    request = SafetyRequest(user_id=uuid4(), action=ActionKind.ENTRY, execution_mode="live", quantity=1, lots=1, snapshot_ready=True, snapshot_current=True)
    return SafetyRequest(**{**request.__dict__, **changes})


def test_semantic_classification_keeps_sl2_as_new_exposure_and_overclose_out():
    assert classify_action(ActionKind.SL2_REVERSAL, quantity=1, attributable_quantity=1) is ActionClass.INCREASE_EXPOSURE
    assert classify_action(ActionKind.STOP_LOSS, quantity=1, attributable_quantity=1) is ActionClass.REDUCE_RISK
    assert classify_action(ActionKind.STOP_LOSS, quantity=2, attributable_quantity=1) is ActionClass.REDUCE_RISK


def test_global_kill_blocks_entries_but_not_valid_protective_exit():
    blocked = evaluate(entry(), live_state(global_kill=True))
    assert not blocked.allowed and blocked.reason_code is ReasonCode.GLOBAL_KILL_SWITCH
    close = SafetyRequest(user_id=uuid4(), action=ActionKind.MANUAL_CLOSE, execution_mode="live", quantity=1, attributable_quantity=1, trade_id=uuid4())
    allowed = evaluate(close, live_state(global_kill=True))
    assert allowed.allowed and allowed.reason_code is ReasonCode.PROTECTIVE_EXIT_ALLOWED


def test_final_recheck_state_change_blocks_stale_approval():
    first = evaluate(entry(), live_state())
    second = evaluate(entry(), live_state(global_kill=True))
    assert first.allowed and not second.allowed


def test_credential_revision_and_freshness_are_fail_closed():
    mismatch = evaluate(entry(), live_state(reconciliation_revision=9))
    stale = evaluate(entry(), live_state(reconciliation_checked_at=datetime.now(UTC) - timedelta(minutes=6)))
    assert mismatch.reason_code is ReasonCode.CREDENTIAL_REVISION_MISMATCH
    assert stale.reason_code is ReasonCode.RECONCILIATION_STALE


def test_broker_read_failure_never_means_flat():
    result = evaluate(entry(), live_state(reconciled=False, broker_evidence=False))
    assert result.reason_code is ReasonCode.BROKER_EVIDENCE_UNAVAILABLE


def test_explicit_egress_failure_blocks_only_explicit_assignment():
    result = evaluate(entry(), live_state(explicit_egress=True, egress_ready=False))
    assert result.reason_code is ReasonCode.EXPLICIT_EGRESS_UNAVAILABLE
    assert evaluate(entry(), live_state(explicit_egress=False, egress_ready=True)).allowed


def test_pending_ambiguous_and_duplicate_exposure_block():
    assert evaluate(entry(), live_state(pending_intent=True)).reason_code is ReasonCode.PENDING_INTENT
    assert evaluate(entry(), live_state(ambiguous_mutation=True)).reason_code is ReasonCode.AMBIGUOUS_MUTATION
    assert evaluate(entry(), live_state(duplicate_exposure=True)).reason_code is ReasonCode.DUPLICATE_EXPOSURE


def test_risk_reduction_still_checks_ownership_and_quantity():
    close = SafetyRequest(user_id=uuid4(), action=ActionKind.TARGET, execution_mode="live", quantity=1, attributable_quantity=1, trade_id=uuid4())
    result = evaluate(close, live_state(owner_matches=False))
    assert result.reason_code is ReasonCode.OWNERSHIP_MISMATCH
    close = SafetyRequest(**{**close.__dict__, "quantity": 2})
    result = evaluate(close, live_state())
    assert result.reason_code is ReasonCode.OVER_CLOSE


def test_demo_isolation_does_not_require_live_readiness_or_broker_evidence():
    request = SafetyRequest(user_id=uuid4(), action=ActionKind.ENTRY, execution_mode="demo", quantity=1, lots=1)
    assert evaluate(request, SafetyState(trading_mode="demo")).allowed


def test_clear_and_non_mutating_actions_are_explicit():
    read = SafetyRequest(user_id=uuid4(), action=ActionKind.BROKER_READ)
    assert evaluate(read, SafetyState()).action_class is ActionClass.NON_MUTATING
    clear = SafetyRequest(user_id=uuid4(), action=ActionKind.CLEAR_TRADES, execution_mode="live", quantity=1, attributable_quantity=1)
    assert evaluate(clear, live_state(global_kill=True)).allowed
