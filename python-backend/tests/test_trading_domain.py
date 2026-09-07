from decimal import Decimal
from uuid import UUID

import pytest

from app.trading.domain import (
    ORDER_TRANSITIONS,
    SAFETY_TRANSITIONS,
    OrderStatus,
    ProtectionStatus,
    Side,
    apply_exit_fill,
    apply_fill,
    futures_pnl_units,
    manual_broker_close_pnl,
    manual_close_session,
    sl2_reversal,
    sl2_reversal_session,
    trade_pnl,
    validate_order_transition,
    validate_safety_transition,
)


@pytest.mark.parametrize(
    ("old", "new"),
    [("pending", "submitting"), ("submitting", "ambiguous"), ("ambiguous", "partially_filled"), ("submitted", "processing"), ("processing", "filled"), ("cancelling", "cancelled"), ("failed", "pending")],
)
def test_rust_order_transition_graph(old: str, new: str):
    validate_order_transition(old, new)


def test_every_rust_order_transition_is_accepted():
    for old, targets in ORDER_TRANSITIONS.items():
        for target in targets:
            validate_order_transition(old, target)


def test_every_allowed_safety_transition_is_accepted():
    for old, targets in SAFETY_TRANSITIONS.items():
        for target in targets:
            validate_safety_transition(old, target)


@pytest.mark.parametrize(("old", "new"), [("filled", "submitted"), ("cancelled", "pending"), ("pending", "filled"), ("submitted", "pending")])
def test_forbidden_order_transition_is_rejected(old: str, new: str):
    with pytest.raises(ValueError):
        validate_order_transition(old, new)


def test_terminal_and_unknown_states_are_not_coerced():
    validate_order_transition(OrderStatus.FILLED, OrderStatus.FILLED)
    with pytest.raises(ValueError):
        validate_order_transition("new_state", "filled")
    with pytest.raises(ValueError):
        from app.trading.domain import validate_intent_transition

        validate_intent_transition("completed", "retry_wait")
    with pytest.raises(ValueError):
        validate_safety_transition(ProtectionStatus.CLOSED, ProtectionStatus.PROTECTED)


def test_partial_duplicate_and_out_of_order_fills_are_cumulative():
    first = apply_fill(order_quantity=100, processed_quantity=0, filled_quantity=0, average_price=None, observed_cumulative=40, fill_price="100.00")
    assert first.delta_quantity == 40 and first.status is OrderStatus.PARTIALLY_FILLED
    second = apply_fill(order_quantity=100, processed_quantity=40, filled_quantity=40, average_price=first.average_price, observed_cumulative=40, fill_price="999.00")
    assert second.delta_quantity == 0 and second.average_price == Decimal("100.00")
    final = apply_fill(order_quantity=100, processed_quantity=40, filled_quantity=40, average_price=first.average_price, observed_cumulative=100, fill_price="110.00")
    assert final.delta_quantity == 60 and final.status is OrderStatus.FILLED
    assert final.average_price == Decimal("106.00")


def test_pnl_and_lot_multipliers_match_rust():
    assert futures_pnl_units("GOLDM", 400, 100) == Decimal(40)
    assert futures_pnl_units("NATGASMINI", 1000, 250) == Decimal(1000)
    assert trade_pnl(Side.BUY, "100", "112.5", "4") == Decimal(50)
    assert trade_pnl(Side.SELL, "100", "87.5", "4") == Decimal(50)


def test_partial_and_full_exit_accounting_preserves_realized_pnl():
    partial = apply_exit_fill(direction=Side.BUY, entry_price="100", current_pnl="0", trade_quantity=10, exit_quantity=4, exit_price="110", instrument="GOLDTEN", lot_size=10)
    assert partial.remaining_quantity == 6 and partial.trade_status.value == "open" and partial.realized_pnl == Decimal(4)
    closed = apply_exit_fill(direction=Side.BUY, entry_price="100", current_pnl=partial.realized_pnl, trade_quantity=6, exit_quantity=6, exit_price="90", instrument="GOLDTEN", lot_size=10)
    assert closed.remaining_quantity == 0 and closed.trade_status.value == "closed" and closed.realized_pnl == Decimal(-2)


def test_manual_broker_close_uses_weighted_attributed_exit_price():
    assert manual_broker_close_pnl(direction=Side.BUY, entry_price="100", weighted_exit_price="110", quantity=10, current_pnl="0", instrument="GOLDTEN", lot_size=10) == Decimal(10)


def test_sl2_reversal_and_stable_manual_close_sessions():
    trade_id = UUID("630e1867-1bb3-4f77-a753-d663f5efc1fe")
    reversal = sl2_reversal(Side.BUY, 5)
    assert reversal and reversal[0] is Side.SELL and reversal[2] == 5
    assert sl2_reversal_session(trade_id) == "r-630e18671bb34f77a753d663f5efc1"
    assert manual_close_session(trade_id) == "mc-630e18671bb34f77"
