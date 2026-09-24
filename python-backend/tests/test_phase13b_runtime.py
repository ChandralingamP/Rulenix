import asyncio
from datetime import UTC, datetime, timedelta
from uuid import uuid4

import pytest

from app.broker.angel.errors import BrokerError, BrokerErrorCategory
from app.broker.angel.models import BrokerReadFailure, BrokerReadSuccess, PositionRecord
from app.config import Settings
from app.reconciliation.domain import EvidenceStatus, ReadEvidence, ReconciliationSnapshot
from app.runtime.reconciliation import _evidence, _position, _time
from app.runtime.service import ProductionRuntime
from app.runtime.supervisor import WorkerSupervisor
from app.strategy.runtime import SchedulerHealth


@pytest.mark.asyncio
async def test_worker_supervisor_isolates_failure_and_reports_readiness() -> None:
    supervisor = WorkerSupervisor()
    healthy_runs = 0

    async def healthy():
        nonlocal healthy_runs
        healthy_runs += 1
        return {"runs": healthy_runs}

    async def broken():
        raise RuntimeError("controlled worker failure")

    supervisor.start(
        "healthy", healthy, interval_seconds=0.01, timeout_seconds=0.1
    )
    supervisor.start(
        "broken", broken, interval_seconds=0.01, timeout_seconds=0.1
    )
    try:
        await asyncio.sleep(0.05)
        snapshots = supervisor.snapshots()
        assert healthy_runs >= 2
        assert snapshots["healthy"]["healthy"] is True
        assert snapshots["broken"]["error_count"] >= 1
        assert supervisor.ready() is False
        assert set(supervisor.active_roles) == {"healthy", "broken"}
    finally:
        await supervisor.stop()
    assert not supervisor.active_roles


@pytest.mark.asyncio
async def test_worker_staleness_is_fail_closed() -> None:
    supervisor = WorkerSupervisor()

    async def never_run():
        return None

    supervisor.start(
        "critical", never_run, interval_seconds=1, timeout_seconds=1
    )
    try:
        # Before the first successful heartbeat a required worker cannot be ready.
        assert supervisor.ready() is False
    finally:
        await supervisor.stop()


def test_broker_read_failure_is_not_flat_evidence() -> None:
    error = BrokerError(
        BrokerErrorCategory.TIMEOUT,
        "read timed out",
        "positions",
    )
    failed = _evidence(BrokerReadFailure(error=error), _position, 9)
    assert failed.status is EvidenceStatus.TIMED_OUT
    assert failed.data is None

    empty = _evidence(BrokerReadSuccess(data=[]), _position, 9)
    assert empty.status is EvidenceStatus.SUCCESS
    assert empty.data == ()


def test_position_mapping_preserves_contract_identity() -> None:
    position = PositionRecord.from_payload(
        {
            "exchange": "MCX",
            "tradingsymbol": "GOLDTEN26OCTFUT",
            "symboltoken": "12345",
            "netqty": "-20",
            "netprice": "100.50",
        }
    )
    mapped = _position(position)
    assert (mapped.exchange, mapped.token, mapped.quantity) == ("MCX", "12345", -20)


def test_naive_broker_timestamp_is_interpreted_in_ist() -> None:
    observed = _time("23-Sep-2026 15:10:00")
    assert observed.utcoffset() == timedelta(hours=5, minutes=30)


def test_required_individual_order_failure_makes_snapshot_non_authoritative() -> None:
    revision = 7
    snapshot = ReconciliationSnapshot(
        user_id=uuid4(),
        account_id="fixture",
        credential_revision=revision,
        egress_identity="os-default",
        positions=ReadEvidence.success((), credential_revision=revision),
        orders=ReadEvidence.success((), credential_revision=revision),
        fills=ReadEvidence.success((), credential_revision=revision),
        conditional_rules=ReadEvidence.success((), credential_revision=revision),
        account_validation=ReadEvidence.success(True, credential_revision=revision),
        individual_orders={
            "missing-local-order": ReadEvidence.failure(
                EvidenceStatus.TIMED_OUT,
                "individual-order:timeout",
                credential_revision=revision,
            )
        },
    )
    assert not snapshot.authoritative
    assert "individual_order[missing-local-order]=timed_out" in snapshot.failure_detail


def test_runtime_configuration_separates_shadow_and_authoritative_modes() -> None:
    Settings(
        PYTHON_RUNTIME_MODE="shadow",
        PYTHON_LIVE_TRADING_ENABLED=False,
    ).validate_production()
    with pytest.raises(ValueError, match="true only in authoritative"):
        Settings(
            PYTHON_RUNTIME_MODE="shadow",
            PYTHON_LIVE_TRADING_ENABLED=True,
            PYTHON_LIVE_AUTHORITY_LEASE_OWNER="00000000-0000-0000-0000-000000000001",
        ).validate_production()
    with pytest.raises(ValueError, match="true only in authoritative"):
        Settings(
            PYTHON_RUNTIME_MODE="authoritative",
            PYTHON_LIVE_TRADING_ENABLED=False,
        ).validate_production()


@pytest.mark.asyncio
async def test_uncertified_authoritative_runtime_remains_fail_closed() -> None:
    settings = Settings(
        PYTHON_RUNTIME_MODE="authoritative",
        PYTHON_LIVE_TRADING_ENABLED=True,
        PYTHON_LIVE_AUTHORITY_LEASE_OWNER="00000000-0000-0000-0000-000000000001",
    )
    runtime = ProductionRuntime(None, None, settings, SchedulerHealth())  # type: ignore[arg-type]
    with pytest.raises(RuntimeError, match="remains disabled"):
        await runtime.start()


def test_worker_health_stale_window_is_bounded() -> None:
    from app.runtime.supervisor import WorkerHealth

    now = datetime.now(UTC)
    health = WorkerHealth("worker", True, 5, 5, running=True)
    health.last_success_at = now - timedelta(seconds=16)
    assert health.stale(now)
    health.last_success_at = now - timedelta(seconds=14)
    assert not health.stale(now)
