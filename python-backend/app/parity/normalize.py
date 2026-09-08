from __future__ import annotations

from dataclasses import dataclass, field
from datetime import date, datetime
from typing import Any
from uuid import UUID


@dataclass(frozen=True)
class NormalizationRules:
    """Narrow, path-based normalization for legitimate nondeterminism only.

    UUIDs and timestamps are normalized only at explicitly listed JSON paths;
    prices, quantities, sides, statuses, reason codes and safety fields are
    intentionally never normalized.
    """

    uuid_paths: frozenset[str] = field(default_factory=frozenset)
    timestamp_paths: frozenset[str] = field(default_factory=frozenset)
    unordered_paths: frozenset[str] = field(default_factory=frozenset)


def _path(parent: str, key: str | int) -> str:
    return f"{parent}.{key}" if parent else str(key)


def normalize_result(value: Any, rules: NormalizationRules, *, path: str = "") -> Any:
    """Return a JSON-compatible normalized copy without mutating input."""

    if path in rules.uuid_paths:
        return "<UUID>"
    if path in rules.timestamp_paths:
        return "<TIMESTAMP>"
    if isinstance(value, dict):
        result = {
            key: normalize_result(item, rules, path=_path(path, key))
            for key, item in value.items()
        }
        return result
    if isinstance(value, (list, tuple)):
        items = [normalize_result(item, rules, path=_path(path, index)) for index, item in enumerate(value)]
        if path in rules.unordered_paths:
            return sorted(items, key=lambda item: repr(item))
        return items
    if isinstance(value, (UUID, datetime, date)):
        return str(value)
    return value
