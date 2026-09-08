from datetime import datetime, timezone
from decimal import Decimal
from uuid import uuid4

from app.reconciliation.domain import (
    BrokerFill,
    BrokerOrder,
    BrokerPosition,
    EvidenceStatus,
    ManualCloseClassification,
    OcoClassification,
    ReadEvidence,
    ReconciliationSnapshot,
    classify_manual_broker_close,
    classify_order,
    classify_position,
    is_synthetic_android_oco,
    weighted_fill_price,
)


def _snapshot(*, positions=None, orders=None, fills=None, conditionals=None, individual=None):
    revision = 7
    return ReconciliationSnapshot(
        user_id=uuid4(), account_id="acct", credential_revision=revision, egress_identity="198.51.100.7",
        positions=ReadEvidence.success(positions or [], credential_revision=revision),
        orders=ReadEvidence.success(orders or [], credential_revision=revision),
        fills=ReadEvidence.success(fills or [], credential_revision=revision),
        conditional_rules=ReadEvidence.success(conditionals or [], credential_revision=revision),
        account_validation=ReadEvidence.success(True, credential_revision=revision),
        individual_orders=individual or {},
    )


def test_failed_read_is_not_empty_or_authoritative():
    snapshot = _snapshot()
    failed = ReconciliationSnapshot(
        **{**snapshot.__dict__, "positions": ReadEvidence.failure(EvidenceStatus.TIMED_OUT, "positions timeout", credential_revision=7)}
    )
    assert not failed.authoritative
    assert failed.positions.data is None
    assert "positions=timed_out" in failed.failure_detail


def test_complete_empty_reads_are_authoritative_only_at_current_revision():
    snapshot = _snapshot()
    assert snapshot.authoritative
    old = ReconciliationSnapshot(**{**snapshot.__dict__, "orders": ReadEvidence.success([], credential_revision=6)})
    assert not old.authoritative


def test_position_order_and_fill_classification():
    position = BrokerPosition("T", "GOLD", "NFO", 10)
    assert classify_position(local_quantity=10, local_symbol="GOLD", local_token="T", broker=position, local_open=True).value == "matched"
    assert classify_position(local_quantity=10, local_symbol="GOLD", local_token="T", broker=None, local_open=True).value == "broker_flat_local_open"
    assert classify_order(local_status="submitted", broker=None, read_succeeded=False).value == "ambiguous"


def test_manual_close_requires_exact_attributable_fills_and_uses_weighted_price():
    entry = datetime(2026, 1, 1, tzinfo=timezone.utc)
    fills = [
        BrokerFill("f1", "external-1", "T", "GOLD", "NFO", "SELL", 4, Decimal(101), entry.replace(minute=1)),
        BrokerFill("f2", "external-2", "T", "GOLD", "NFO", "SELL", 6, Decimal(99), entry.replace(minute=2)),
    ]
    result = classify_manual_broker_close(local_open=True, local_symbol="GOLD", local_token="T", local_exchange="NFO", entry_at=entry, remaining_quantity=10, broker_position=None, fills=fills, order_book_succeeded=True, positions_succeeded=True, fills_succeeded=True)
    assert result is ManualCloseClassification.MANUAL_BROKER_CLOSE
    assert weighted_fill_price(fills) == Decimal("99.8")
    assert classify_manual_broker_close(local_open=True, local_symbol="GOLD", local_token="T", local_exchange="NFO", entry_at=entry, remaining_quantity=9, broker_position=None, fills=fills, order_book_succeeded=True, positions_succeeded=True, fills_succeeded=True) is ManualCloseClassification.RECONCILIATION_REQUIRED


def test_synthetic_oco_requires_strict_ab1007_conjunction():
    order = BrokerOrder("oco", android_synthetic=True, order_shape="android_synthetic_oco")
    position = ReadEvidence.success([], credential_revision=1)
    conditionals = ReadEvidence.success([], credential_revision=1)
    fills = ReadEvidence.success([], credential_revision=1)
    not_found = ReadEvidence.failure(EvidenceStatus.FAILED, "AB1007 Order not found", credential_revision=1)
    assert is_synthetic_android_oco(order=order, position=position, individual=not_found, conditional_rules=conditionals, fills=fills, executable_sibling=False)
    assert not is_synthetic_android_oco(order=order, position=position, individual=ReadEvidence.failure(EvidenceStatus.TIMED_OUT, "timeout"), conditional_rules=conditionals, fills=fills, executable_sibling=False)
    assert OcoClassification.SYNTHETIC_ANDROID_OCO.value == "SYNTHETIC_ANDROID_OCO"
