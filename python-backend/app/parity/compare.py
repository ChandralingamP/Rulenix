from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from .models import RuntimeResult


@dataclass(frozen=True)
class Difference:
    path: str
    rust: Any
    python: Any
    approved: bool = False


def _walk(rust: Any, python: Any, path: str, differences: list[Difference], approved: set[str]) -> None:
    if type(rust) is not type(python):
        differences.append(Difference(path, rust, python, path in approved))
        return
    if isinstance(rust, dict):
        for key in sorted(set(rust) | set(python)):
            child = f"{path}.{key}" if path else key
            if key not in rust or key not in python:
                differences.append(Difference(child, rust.get(key), python.get(key), child in approved))
            else:
                _walk(rust[key], python[key], child, differences, approved)
        return
    if isinstance(rust, list):
        if len(rust) != len(python):
            differences.append(Difference(path, rust, python, path in approved))
            return
        for index, (left, right) in enumerate(zip(rust, python)):
            _walk(left, right, f"{path}.{index}", differences, approved)
        return
    if rust != python:
        differences.append(Difference(path, rust, python, path in approved))


def compare_results(rust: RuntimeResult, python: RuntimeResult, *, approved_paths: set[str] | None = None) -> list[Difference]:
    approved = approved_paths or set()
    differences: list[Difference] = []
    if rust.status != python.status:
        differences.append(Difference("status", rust.status, python.status, "status" in approved))
    _walk(rust.body, python.body, "body", differences, approved)
    _walk(rust.state, python.state, "state", differences, approved)
    return differences
