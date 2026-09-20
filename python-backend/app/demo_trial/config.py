"""Fail-closed configuration for the Phase 12 DEMO trial."""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlparse


def _secret(name: str) -> str:
    file_name = os.getenv(f"{name}_FILE", "").strip()
    if not file_name:
        raise ValueError(f"{name}_FILE is required")
    value = Path(file_name).read_text(encoding="utf-8").strip()
    if not value:
        raise ValueError(f"{name}_FILE is empty")
    return value


def _identity(url: str) -> str:
    parsed = urlparse(url.replace("postgresql+asyncpg://", "postgresql://", 1))
    if parsed.scheme not in {"postgres", "postgresql"} or not parsed.hostname or not parsed.path:
        raise ValueError("trial database URL must be a complete PostgreSQL URL")
    return parsed.username or ""


@dataclass(frozen=True)
class DemoTrialSettings:
    source_database_url: str
    sink_database_url: str
    release: str
    poll_seconds: int = 15
    health_port: int = 8091

    @classmethod
    def from_environment(cls) -> DemoTrialSettings:
        if os.getenv("RULENIX_DEMO_TRIAL_ONLY") != "required":
            raise ValueError("RULENIX_DEMO_TRIAL_ONLY=required is mandatory")
        source = _secret("DEMO_TRIAL_SOURCE_DATABASE_URL")
        sink = _secret("DEMO_TRIAL_SINK_DATABASE_URL")
        if _identity(source) != "rulenix_demo_trial_reader":
            raise ValueError("source database identity must be rulenix_demo_trial_reader")
        if _identity(sink) != "rulenix_demo_trial_writer":
            raise ValueError("sink database identity must be rulenix_demo_trial_writer")
        release = os.getenv("DEMO_TRIAL_RELEASE", "").strip()
        if len(release) < 7:
            raise ValueError("DEMO_TRIAL_RELEASE is required")
        poll = int(os.getenv("DEMO_TRIAL_POLL_SECONDS", "15"))
        port = int(os.getenv("DEMO_TRIAL_HEALTH_PORT", "8091"))
        if not 5 <= poll <= 300 or not 1024 <= port <= 65535:
            raise ValueError("trial poll/health settings are outside safe bounds")
        return cls(source, sink, release, poll, port)


__all__ = ["DemoTrialSettings"]
