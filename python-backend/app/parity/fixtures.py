from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from .models import Fixture, RuntimeResult


def _result(value: dict[str, Any] | None) -> RuntimeResult | None:
    if value is None:
        return None
    return RuntimeResult(
        status=int(value["status"]),
        body=value.get("body"),
        headers={str(k): str(v) for k, v in value.get("headers", {}).items()},
        state=value.get("state"),
    )


def load_fixtures(path: str | Path) -> list[Fixture]:
    """Load a JSON fixture bundle without contacting a runtime."""

    payload = json.loads(Path(path).read_text(encoding="utf-8"))
    return [
        Fixture(
            name=str(item["name"]),
            category=str(item["category"]),
            request=item.get("request"),
            rust=_result(item.get("rust")),
            python=_result(item.get("python")),
            approved_differences=tuple(item.get("approved_differences", ())),
        )
        for item in payload["fixtures"]
    ]
