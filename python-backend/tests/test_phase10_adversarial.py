from datetime import UTC, datetime, timedelta
from uuid import uuid4

import pytest

from app.risk.domain import ActionKind, SafetyRequest, SafetyState, evaluate
from app.strategy.common import supertrend_eod_due
from app.trading.domain import (
    ORDER_TRANSITIONS,
    OrderRole,
    OrderStatus,
    Side,
    apply_fill,
    sl2_reversal,
    validate_order_transition,
)


def _live_state(**overrides):
    base = SafetyState(
        user_active=True,
        can_live_trade=True,
        trading_mode="live",
        token_status="success",
        credential_revision=4,
        reconciled=True,
        reconciliation_revision=4,
        reconciliation_checked_at=datetime.now(UTC),
        broker_evidence=True,
    )
    return SafetyState(**{**base.__dict__, **overrides})


def test_systematic_fill_matrix_never_exceeds_intended_quantity():
    for intended in range(1, 13):
        for observed in range(intended + 1):
            result = apply_fill(
                order_quantity=intended,
                processed_quantity=0,
                filled_quantity=0,
                average_price=None,
                observed_cumulative=observed,
                fill_price="100.00",
            )
            assert 0 <= result.cumulative_quantity <= intended
            assert 0 <= result.delta_quantity <= intended


def test_terminal_order_states_cannot_reopen_without_explicit_recovery_edge():
    terminal = {OrderStatus.FILLED.value, OrderStatus.CANCELLED.value, OrderStatus.REJECTED.value}
    for state in terminal:
        for target in ORDER_TRANSITIONS:
            if target != state:
                with pytest.raises(ValueError):
                    validate_order_transition(state, target)


def test_kill_switch_and_revision_changes_block_stale_exposure_approval():
    request = SafetyRequest(
        user_id=uuid4(),
        action=ActionKind.ENTRY,
        execution_mode="live",
        quantity=1,
        lots=1,
        snapshot_ready=True,
        snapshot_current=True,
    )
    assert evaluate(request, _live_state()).allowed
    assert not evaluate(request, _live_state(global_kill=True)).allowed
    assert not evaluate(request, _live_state(credential_revision=5)).allowed
    assert not evaluate(request, _live_state(reconciliation_checked_at=datetime.now(UTC) - timedelta(minutes=6))).allowed


def test_over_close_and_demo_live_boundaries_are_fail_closed():
    close = SafetyRequest(
        user_id=uuid4(),
        action=ActionKind.MANUAL_CLOSE,
        execution_mode="live",
        quantity=11,
        attributable_quantity=10,
        trade_id=uuid4(),
    )
    assert not evaluate(close, _live_state()).allowed
    demo = SafetyRequest(user_id=uuid4(), action=ActionKind.ENTRY, execution_mode="demo", quantity=1, lots=1)
    assert evaluate(demo, SafetyState(trading_mode="demo")).allowed


def test_sl2_is_new_exposure_and_has_one_directional_plan():
    assert sl2_reversal("BUY", 1) == (Side.SELL, OrderRole.SELL_ENTRY, 1)
    assert sl2_reversal("SELL", 3) == (Side.BUY, OrderRole.BUY_ENTRY, 3)
    assert sl2_reversal("BUY", 0) is None


def test_supertrend_eod_boundary_is_exactly_1510_ist():
    from zoneinfo import ZoneInfo

    zone = ZoneInfo("Asia/Kolkata")
    before = datetime(2026, 9, 8, 15, 9, 59, tzinfo=zone)
    boundary = datetime(2026, 9, 8, 15, 10, 0, tzinfo=zone)
    after = boundary + timedelta(seconds=1)
    assert not supertrend_eod_due(before)
    assert supertrend_eod_due(boundary)
    assert supertrend_eod_due(after)
    assert supertrend_eod_due(boundary.astimezone(UTC))
