"""Bounded shadow telemetry values."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import Any


@dataclass(frozen=True)
class Observation:
    source_kind: str
    source_id: str
    observed_at: datetime
    account_ref: str | None
    strategy: str
    instrument: str
    input_version: str
    rust_decision: dict[str, Any]
    python_decision: dict[str, Any]
    classification: str
    mismatch_reason: str
    severity: str
    python_latency_ms: float
    rust_latency_ms: float | None = None
    failure_classification: str | None = None
    source_created_at: datetime | None = None


__all__ = ["Observation"]
