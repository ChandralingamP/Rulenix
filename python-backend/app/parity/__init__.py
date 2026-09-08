"""Reusable, fixture-first Rust/Python parity audit helpers.

The package deliberately does not know how to contact production Rust or Angel
One.  Callers provide isolated runtime adapters and sanitized fixtures.
"""

from .compare import Difference, compare_results
from .models import Fixture, RuntimeResult
from .normalize import NormalizationRules, normalize_result
from .runner import CallableAdapter, JsonSubprocessAdapter, execute_fixtures

__all__ = [
    "CallableAdapter",
    "Difference",
    "Fixture",
    "JsonSubprocessAdapter",
    "NormalizationRules",
    "RuntimeResult",
    "compare_results",
    "execute_fixtures",
    "normalize_result",
]
