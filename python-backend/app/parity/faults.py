from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass, field
from typing import NoReturn


@dataclass
class FaultInjector:
    """Deterministic boundary fault plan for isolated tests.

    A boundary is tripped only the configured number of times, making rollback,
    retry and restart tests reproducible rather than dependent on flaky I/O.
    """

    plan: dict[str, list[BaseException]] = field(default_factory=dict)
    hits: dict[str, int] = field(default_factory=lambda: defaultdict(int))

    def fail_once(self, boundary: str, error: BaseException) -> None:
        self.plan.setdefault(boundary, []).append(error)

    def checkpoint(self, boundary: str) -> None:
        self.hits[boundary] += 1
        pending = self.plan.get(boundary, [])
        if pending:
            raise pending.pop(0)

    def assert_exhausted(self) -> None:
        pending = {key: len(value) for key, value in self.plan.items() if value}
        if pending:
            raise AssertionError(f"Unexercised fault boundaries: {pending}")


def raise_fault(injector: FaultInjector, boundary: str) -> NoReturn:
    injector.checkpoint(boundary)
    raise AssertionError(f"Fault boundary {boundary!r} did not raise as configured")
