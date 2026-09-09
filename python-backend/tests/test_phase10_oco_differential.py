from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from app.parity.oco_adapter import classify_oco_fixture

ROOT = Path(__file__).parents[1]
FIXTURES = json.loads((ROOT / "tests" / "parity" / "oco_differential_fixtures.json").read_text(encoding="utf-8"))["fixtures"]
NODE_ADAPTER = ROOT.parent / "scripts" / "phase10-oco-adapter.mjs"


@pytest.mark.parametrize("fixture", FIXTURES, ids=[fixture["id"] for fixture in FIXTURES])
def test_authoritative_baseline_and_python_oco_classification_match(fixture: dict[str, object]) -> None:
    completed = subprocess.run(
        ["node", str(NODE_ADAPTER)],
        input=json.dumps(fixture),
        text=True,
        capture_output=True,
        check=True,
        cwd=ROOT.parent,
        timeout=10,
    )
    oracle = json.loads(completed.stdout)
    python = classify_oco_fixture(fixture)
    assert oracle == python == {"classification": fixture["expected"]}
