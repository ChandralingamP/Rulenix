from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass(frozen=True)
class RuntimeResult:
    """A normalized-independent result from either isolated runtime."""

    status: int
    body: Any
    headers: dict[str, str] = field(default_factory=dict)
    state: Any = None


@dataclass(frozen=True)
class Fixture:
    """One sanitized input and its two runtime observations."""

    name: str
    category: str
    request: Any
    rust: RuntimeResult | None = None
    python: RuntimeResult | None = None
    approved_differences: tuple[str, ...] = ()
