import asyncio
import json
from datetime import UTC, datetime, timedelta
from decimal import Decimal
from pathlib import Path
from uuid import uuid4

import pytest

from app.reconciliation.domain import (
    BrokerFill,
    BrokerOrder,
    BrokerPosition,
    ConditionalRule,
    ExposureOwnership,
    ManualCloseClassification,
    classify_conditional_ownership,
    classify_manual_broker_close,
    classify_order_ownership,
    classify_position_ownership,
    deployment_account_decision,
    exact_contract_collision,
)
from app.risk.domain import ActionKind, ReasonCode, SafetyRequest, SafetyState, evaluate
from app.shadow.evaluate import evaluate_readiness
from app.strategy.runtime import SchedulerHealth, SingleFlightDispatcher


@pytest.mark.asyncio
async def test_scheduler_replay_dispatches_all_66_once_and_recovers_worker_failure() -> None:
    fixture = json.loads(
        (Path(__file__).parent / "parity" / "sep_14_18_demo_replay.json").read_text()
    )
    health = SchedulerHealth()
    health.leadership_acquired()
    dispatcher = SingleFlightDispatcher(health)
    evaluated: list[str] = []

    async def evaluate_path(key: str) -> None:
        evaluated.append(key)

    expected = sum(int(event["demo_users"]) for event in fixture)
    for event_index, event in enumerate(fixture):
        health.record_advance()
        for user_index in range(int(event["demo_users"])):
            key = f"{event_index}:{user_index}"
            assert dispatcher.dispatch(key, lambda key=key: evaluate_path(key), timeout_seconds=1)
            assert not dispatcher.dispatch(
                key, lambda key=key: evaluate_path(key), timeout_seconds=1
            )
    await dispatcher.wait_idle()
    assert expected == 66
    assert len(evaluated) == expected
    assert len(set(evaluated)) == expected

    attempts = 0

    async def flaky() -> None:
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            raise RuntimeError("isolated worker failure")

    assert dispatcher.dispatch("retry", flaky, timeout_seconds=1)
    await dispatcher.wait_idle()
    assert dispatcher.dispatch("retry", flaky, timeout_seconds=1)
    await dispatcher.wait_idle()
    snapshot = health.snapshot()
    assert snapshot.advancing and not snapshot.stale
    assert snapshot.error_count == 1

    async def blocked() -> None:
        await asyncio.Event().wait()

    assert dispatcher.dispatch("timeout", blocked, timeout_seconds=0.01)
    await dispatcher.wait_idle()
    assert health.snapshot().error_count == 2
    assert dispatcher.dispatch("timeout", lambda: evaluate_path("timeout"), timeout_seconds=1)
    await dispatcher.wait_idle()
    await dispatcher.shutdown()
    assert not health.snapshot().leader


def test_scheduler_stale_detection_and_leadership_reacquisition() -> None:
    health = SchedulerHealth()
    started = datetime(2026, 9, 18, tzinfo=UTC)
    health.leadership_acquired(started)
    assert health.snapshot(now=started + timedelta(seconds=61)).stale
    health.leadership_lost()
    assert not health.snapshot(now=started + timedelta(minutes=2)).stale
    health.leadership_acquired(started + timedelta(minutes=2))
    health.record_advance(started + timedelta(minutes=2, seconds=1))
    assert health.snapshot(now=started + timedelta(minutes=2, seconds=2)).advancing


def _fill(order: str, quantity: int, *, token: str = "T", minutes: int = 1) -> BrokerFill:
    return BrokerFill(
        order, order, token, "GOLD", "MCX", "SELL", quantity, Decimal(101),
        datetime(2026, 9, 18, 10, minutes, tzinfo=UTC),
    )


def test_external_close_is_exact_fresh_and_fail_closed() -> None:
    entry = datetime(2026, 9, 18, 10, 0, tzinfo=UTC)
    base = {
        "local_open": True, "local_symbol": "GOLD", "local_token": "T",
        "local_exchange": "MCX", "entry_at": entry, "remaining_quantity": 10,
        "broker_position": None, "order_book_succeeded": True,
        "positions_succeeded": True, "fills_succeeded": True,
        "local_direction": "BUY", "evidence_since": entry,
    }
    assert classify_manual_broker_close(**base, fills=[_fill("a", 4), _fill("b", 6, minutes=2)]) is ManualCloseClassification.MANUAL_BROKER_CLOSE
    for fills in ([_fill("a", 9)], [_fill("a", 11)], [_fill("a", 10, token="OTHER")]):
        assert classify_manual_broker_close(**base, fills=fills) is ManualCloseClassification.RECONCILIATION_REQUIRED
    stale = _fill("old", 10, minutes=1)
    assert classify_manual_broker_close(**{**base, "evidence_since": entry + timedelta(minutes=2)}, fills=[stale]) is ManualCloseClassification.RECONCILIATION_REQUIRED
    assert classify_manual_broker_close(**{**base, "fills_succeeded": False}, fills=[]) is ManualCloseClassification.RECONCILIATION_REQUIRED
    assert classify_manual_broker_close(**base, fills=[]) is ManualCloseClassification.RECONCILIATION_REQUIRED


def test_ownership_tri_state_never_promotes_unknown_to_manual() -> None:
    manual = BrokerOrder("manual", "T", "GOLD", "MCX", "BUY", "open", 10)
    ambiguous = BrokerOrder("", status="unknown")
    owned = BrokerOrder("owned", "T", "GOLD", "MCX", "BUY", "open", 10)
    assert classify_order_ownership(manual) is ExposureOwnership.MANUAL_EXTERNAL
    assert classify_order_ownership(ambiguous) is ExposureOwnership.AMBIGUOUS
    assert classify_order_ownership(owned, known_broker_ids=frozenset({"owned"})) is ExposureOwnership.RULENIX_OWNED
    position = BrokerPosition("T", "GOLD", "MCX", 10)
    opening_fill = BrokerFill(
        "manual", "manual", "T", "GOLD", "MCX", "BUY", 10, Decimal(101),
        datetime(2026, 9, 18, 10, 1, tzinfo=UTC),
    )
    assert classify_position_ownership(position, fills=[opening_fill], orders=[manual]) is ExposureOwnership.MANUAL_EXTERNAL
    tagged_fill = BrokerFill(
        "tagged", "", "T", "GOLD", "MCX", "BUY", 10, Decimal(101),
        datetime(2026, 9, 18, 10, 1, tzinfo=UTC), order_tag="known-tag",
    )
    assert classify_position_ownership(
        position, fills=[tagged_fill], orders=[], known_client_ids=frozenset({"known-tag"})
    ) is ExposureOwnership.RULENIX_OWNED
    assert classify_conditional_ownership(ConditionalRule("gtt")) is ExposureOwnership.AMBIGUOUS


def test_deployment_readiness_and_exact_contract_collision_are_separate() -> None:
    offline = deployment_account_decision(
        broker_readable=False, broker_exposure_observed=False,
        rulenix_owned_exposure=0, ambiguous_exposure=0, local_unresolved=0,
    )
    assert offline.deployment_safe and not offline.live_ready
    assert offline.classification == "offline_locally_flat"
    manual_only = deployment_account_decision(
        broker_readable=True, broker_exposure_observed=True,
        rulenix_owned_exposure=0, ambiguous_exposure=0, local_unresolved=0,
    )
    assert manual_only.deployment_safe and manual_only.live_ready
    assert not deployment_account_decision(
        broker_readable=True, broker_exposure_observed=True,
        rulenix_owned_exposure=1, ambiguous_exposure=0, local_unresolved=0,
    ).deployment_safe
    assert not deployment_account_decision(
        broker_readable=True, broker_exposure_observed=True,
        rulenix_owned_exposure=0, ambiguous_exposure=1, local_unresolved=0,
    ).deployment_safe
    observations = [("MCX", "GOLD", ExposureOwnership.MANUAL_EXTERNAL)]
    assert exact_contract_collision(execution_mode="live", exchange="MCX", token="GOLD", observations=observations)
    assert not exact_contract_collision(execution_mode="live", exchange="MCX", token="SILVER", observations=observations)
    assert not exact_contract_collision(execution_mode="demo", exchange="MCX", token="GOLD", observations=observations)

    now = datetime.now(UTC)
    ready = SafetyState(
        user_active=True, can_live_trade=True, trading_mode="live", token_status="success",
        credential_revision=1, reconciled=True, reconciliation_revision=1,
        reconciliation_checked_at=now, broker_contract_collision=True,
    )
    blocked = evaluate(
        SafetyRequest(user_id=uuid4(), action=ActionKind.ENTRY, execution_mode="live", quantity=1),
        ready,
    )
    assert blocked.reason_code is ReasonCode.BROKER_INSTRUMENT_COLLISION
    demo = evaluate(
        SafetyRequest(user_id=uuid4(), action=ActionKind.ENTRY, execution_mode="demo", quantity=1),
        ready,
    )
    assert demo.allowed


def test_shadow_reports_offline_clean_as_deployment_safe_but_not_live_ready() -> None:
    now = datetime.now(UTC)
    _, python, _, _, _ = evaluate_readiness({
        "healthy": False, "checked_at": now, "broker_credential_revision": None,
        "current_credential_revision": None, "blockers": 0,
        "rulenix_owned_exposure": 0, "ambiguous_exposure": 0,
        "manual_external_exposure": 0,
    }, now)
    assert python["deployment_safe"] is True
    assert python["deployment_classification"] == "offline_locally_flat"
    assert python["ready"] is False
