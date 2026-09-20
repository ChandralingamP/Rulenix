from __future__ import annotations

from dataclasses import replace
from decimal import Decimal
from pathlib import Path

import pytest

from app.demo_trial.engine import compare_decisions, evaluate_scenario
from app.demo_trial.models import ExitKind
from app.demo_trial.service import load_scenarios

ROOT = Path(__file__).parents[2]


def test_all_production_derived_lifecycle_oracles_match() -> None:
    scenarios = load_scenarios()
    assert len(scenarios) == 10
    assert {scenario.strategy_key for scenario, _ in scenarios} == {
        "futures_breakout_v3", "supertrend_index_options_v1"
    }
    assert {scenario.exit_kind for scenario, _ in scenarios} >= {
        ExitKind.NO_SIGNAL, ExitKind.TARGET, ExitKind.SL1, ExitKind.SL2,
        ExitKind.EOD, ExitKind.MANUAL_CLOSE, ExitKind.KILL_SWITCH,
    }
    for scenario, oracle in scenarios:
        assert compare_decisions(oracle, evaluate_scenario(scenario).json()) == ("MATCH", "")


def test_material_demo_difference_is_never_normalized() -> None:
    scenario, oracle = load_scenarios()[0]
    actual = evaluate_scenario(replace(scenario, target=scenario.target + Decimal("0.01"))).json()
    classification, reason = compare_decisions(oracle, actual)
    assert classification == "MISMATCH"
    assert "target" in reason and "exit_price" in reason and "pnl" in reason


def test_sl2_reversal_preserves_quantity_lineage_and_pnl() -> None:
    scenario, _ = next(
        item for item in load_scenarios() if item[0].scenario_id == "futures-buy-sl2-reversal"
    )
    decision = evaluate_scenario(scenario)
    assert decision.exit_reason == "SL2"
    assert decision.pnl == Decimal("-30.00")
    assert decision.reversal_side == "SELL"
    assert decision.reversal_entry == scenario.sl2
    assert decision.reversal_pnl == Decimal("14.55")
    assert (decision.intent_count, decision.order_count, decision.trade_count) == (4, 8, 2)


def test_demo_mode_is_server_owned_and_live_fails_closed() -> None:
    scenario, _ = load_scenarios()[0]
    with pytest.raises(ValueError, match="server-owned DEMO"):
        evaluate_scenario(replace(scenario, execution_mode="live"))


def test_kill_switch_and_no_signal_create_no_state() -> None:
    selected = {
        scenario.exit_kind: evaluate_scenario(scenario)
        for scenario, _ in load_scenarios()
        if scenario.exit_kind in {ExitKind.KILL_SWITCH, ExitKind.NO_SIGNAL}
    }
    for decision in selected.values():
        assert decision.signal_count == decision.intent_count == decision.order_count == 0
        assert decision.trade_count == 0


def test_eod_without_open_demo_position_records_signal_but_creates_no_execution_state() -> None:
    scenario, _ = load_scenarios()[0]
    decision = evaluate_scenario(replace(scenario, exit_kind=ExitKind.EOD_NO_POSITION))
    assert decision.reason == "eod_no_open_demo_position"
    assert decision.signal_count == 1
    assert decision.intent_count == decision.order_count == decision.trade_count == 0
    assert decision.side is None and decision.quantity == 0
    assert decision.entry is decision.target is decision.sl1 is decision.sl2 is None
    assert decision.exit_reason == "EOD" and decision.exit_price is None
    assert decision.pnl == Decimal("0.00")


def test_demo_trial_image_excludes_all_broker_and_api_packages() -> None:
    dockerfile = (ROOT / "python-backend" / "Dockerfile.demo-trial").read_text()
    assert "COPY app/demo_trial" in dockerfile
    for forbidden in ("app/broker", "app/api", "app/reconciliation", "app/trading", "app/main.py"):
        assert forbidden not in dockerfile
