import json
import sys
from pathlib import Path

import pytest

from app.parity.compare import compare_results
from app.parity.fixtures import load_fixtures
from app.parity.models import RuntimeResult
from app.parity.normalize import NormalizationRules, normalize_result
from app.parity.runner import JsonSubprocessAdapter

FIXTURES = Path(__file__).with_name("parity") / "phase10_fixtures.json"
SCORECARD = Path(__file__).with_name("parity") / "phase10_scorecard.json"


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


@pytest.mark.parametrize("bad_value", [None, "", [], 0])
def test_normalization_does_not_coerce_financial_values(bad_value):
    result = normalize_result({"quantity": bad_value, "price": "100.00"}, NormalizationRules())
    assert result == {"quantity": bad_value, "price": "100.00"}
