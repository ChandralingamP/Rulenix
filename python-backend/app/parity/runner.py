from __future__ import annotations

import asyncio
import json
import subprocess
from collections.abc import Awaitable, Callable, Iterable, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol, cast

from .models import Fixture, RuntimeResult


class RuntimeAdapter(Protocol):
    async def execute(self, fixture: Fixture) -> RuntimeResult: ...


@dataclass(frozen=True)
class CallableAdapter:
    callback: Callable[[Fixture], RuntimeResult | Awaitable[RuntimeResult]]

    async def execute(self, fixture: Fixture) -> RuntimeResult:
        value = self.callback(fixture)
        if asyncio.iscoroutine(value):
            return await value
        return cast(RuntimeResult, value)


@dataclass(frozen=True)
class JsonSubprocessAdapter:
    """Adapter for an isolated Rust/Python fixture runner using JSON stdin/stdout.

    The command must read one JSON request and emit one JSON object containing
    ``status``, ``body`` and optional ``headers``/``state``. Production paths
    are intentionally unsupported: the caller supplies an isolated command.
    """

    command: Sequence[str]
    cwd: str | Path | None = None
    timeout_seconds: float = 30.0

    async def execute(self, fixture: Fixture) -> RuntimeResult:
        payload = json.dumps({"name": fixture.name, "category": fixture.category, "request": fixture.request})

        def run() -> RuntimeResult:
            completed = subprocess.run(
                list(self.command),
                input=payload,
                text=True,
                capture_output=True,
                cwd=self.cwd,
                timeout=self.timeout_seconds,
                check=True,
            )
            value = json.loads(completed.stdout)
            return RuntimeResult(
                status=int(value["status"]),
                body=value.get("body"),
                headers={str(key): str(item) for key, item in value.get("headers", {}).items()},
                state=value.get("state"),
            )

        return await asyncio.to_thread(run)


async def execute_fixtures(adapter: RuntimeAdapter, fixtures: Iterable[Fixture]) -> list[RuntimeResult]:
    """Execute sanitized fixtures in declared order for deterministic reports."""

    return [await adapter.execute(fixture) for fixture in fixtures]
