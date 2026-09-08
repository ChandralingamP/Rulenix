import json
import os
import sys
from pathlib import Path

import pytest

from app.parity.compare import compare_results
from app.parity.fixtures import load_fixtures
from app.parity.models import RuntimeResult
from app.parity.normalize import NormalizationRules, normalize_result
from app.parity.runner import CallableAdapter, JsonSubprocessAdapter, execute_fixtures
from app.parity.strategy_adapter import execute_strategy_result

FIXTURES = Path(__file__).with_name("parity") / "phase10_fixtures.json"
SCORECARD = Path(__file__).with_name("parity") / "phase10_scorecard.json"
STRATEGY_FIXTURES = Path(__file__).with_name("parity") / "strategy_differential_fixtures.json"
DIFFERENTIAL_RESULTS = Path(__file__).with_name("parity") / "phase10_differential_results.json"


def test_fixture_bundle_is_reproducible_and_approved_differences_are_explicit():
    payload = json.loads(FIXTURES.read_text(encoding="utf-8"))
    fixtures = load_fixtures(FIXTURES)
    assert len(fixtures) == 4
    assert payload["schema_version"] == 1
    assert fixtures[2].approved_differences == ("body.detail",)


def test_scorecard_freezes_baselines_and_exposes_blockers():
    scorecard = json.loads(SCORECARD.read_text(encoding="utf-8"))
    assert scorecard["rust_baseline_sha"] == "3f788f2a842ef9b1b66366d439431867850e3753"
    assert scorecard["python_baseline_sha"] == "89332b5701e80237094b54f9e88d7496bd326009"
    assert scorecard["mutation_boundary"] == {"reachable_paths": 0, "network_requests": 0, "fake_live_successes": 0}
    assert scorecard["blockers"]


def test_differential_results_record_exact_financial_mismatch_without_normalization():
    results = json.loads(DIFFERENTIAL_RESULTS.read_text(encoding="utf-8"))
    assert results["strategy"]["future_breakout"]["status"] == "BLOCKED"
    assert results["strategy"]["future_breakout"]["mismatched"] == 12
    assert results["strategy"]["supertrend"]["matched"] == 6
    assert results["normalization"] == "none for financial values; only explicit fixture normalizers may be used"


def test_normalization_is_path_scoped_and_does_not_hide_safety_differences():
    rules = NormalizationRules(uuid_paths=frozenset({"body.request_id"}), timestamp_paths=frozenset())
    left = {"request_id": "a", "allowed": False, "reason_code": "global_kill_switch"}
    right = {"request_id": "b", "allowed": True, "reason_code": "global_kill_switch"}
    normalized_left = normalize_result(left, rules, path="body")
    normalized_right = normalize_result(right, rules, path="body")
    assert normalized_left["request_id"] == normalized_right["request_id"] == "<UUID>"
    differences = compare_results(RuntimeResult(200, normalized_left), RuntimeResult(200, normalized_right))
    assert [(item.path, item.rust, item.python) for item in differences] == [("body.allowed", False, True)]


def test_fixture_ordering_is_only_ignored_at_declared_unordered_path():
    fixture = load_fixtures(FIXTURES)[3]
    rules = NormalizationRules(unordered_paths=frozenset({"body.items"}))
    rust = normalize_result(fixture.rust.body, rules, path="body")
    python = normalize_result(fixture.python.body, rules, path="body")
    assert compare_results(RuntimeResult(200, rust), RuntimeResult(200, python)) == []


def test_status_and_broker_safety_differences_are_never_approved_implicitly():
    differences = compare_results(
        RuntimeResult(503, {"safe_to_clear": False}),
        RuntimeResult(200, {"safe_to_clear": True}),
    )
    assert {difference.path for difference in differences} == {"status", "body.safe_to_clear"}


@pytest.mark.asyncio
async def test_json_subprocess_adapter_is_isolated_and_structured():
    command = [
        sys.executable,
        "-c",
        "import json,sys; value=json.load(sys.stdin); print(json.dumps({'status': 200, 'body': {'name': value['name']}}))",
    ]
    result = await JsonSubprocessAdapter(command).execute(load_fixtures(FIXTURES)[0])
    assert result.status == 200 and result.body["name"] == "risk-kill-switch-blocks-entry"


@pytest.mark.asyncio
async def test_rust_python_strategy_differential_fixtures():
    fixtures = load_fixtures(STRATEGY_FIXTURES)
    adapter_path = os.environ.get("RULENIX_RUST_ADAPTER")
    if not adapter_path:
        candidate = Path(__file__).parents[2] / "backend" / "target" / "debug" / "rulenix-backend.exe"
        adapter_path = str(candidate) if candidate.exists() else ""
    if not adapter_path or not Path(adapter_path).exists():
        pytest.skip("Rust phase10 adapter is not built; run scripts/run-phase10-audit.ps1")
    rust = await execute_fixtures(
        JsonSubprocessAdapter([adapter_path, "--phase10-fixture-adapter"]), fixtures
    )

    def python_result(fixture):
        operation = fixture.request["operation"]
        status, body = execute_strategy_result(operation, fixture.request["request"])
        return type(rust[0])(status=status, body=body)

    python = await execute_fixtures(CallableAdapter(python_result), fixtures)
    mismatches = []
    for fixture, left, right in zip(fixtures, rust, python):
        differences = compare_results(left, right)
        if differences:
            mismatches.append({"fixture": fixture.name, "differences": [difference.path for difference in differences]})
    mismatch_names = {item["fixture"] for item in mismatches}
    allowed_numeric_names = {
        "fb-neutral", "fb-gap-up", "fb-gap-down", "fb-open-equals-hh4", "fb-open-equals-ll4",
        "fb-buy", "fb-sell", "fb-buffer-target-sl", "fb-missed-boundary-buy",
        "fb-missed-boundary-sell", "fb-duplicate-evaluation", "fb-insufficient-history",
    }
    allowed_paths = {"body.message", "body.buy_entry", "body.sell_entry", "body.buy_sl1", "body.buy_sl2", "body.sell_sl1", "body.sell_sl2", "body.sell_target", "body.exit.sl1", "body.exit.sl2", "body.exit.target"}
    assert mismatch_names == allowed_numeric_names
    assert all(set(item["differences"]) <= allowed_paths for item in mismatches)


@pytest.mark.parametrize("bad_value", [None, "", [], 0])
def test_normalization_does_not_coerce_financial_values(bad_value):
    result = normalize_result({"quantity": bad_value, "price": "100.00"}, NormalizationRules())
    assert result == {"quantity": bad_value, "price": "100.00"}
