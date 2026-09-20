"""Strict configuration for the structurally isolated shadow process."""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlparse


def _secret(name: str) -> str:
    file_name = os.getenv(f"{name}_FILE", "").strip()
    if not file_name:
        raise ValueError(f"{name}_FILE is required")
    path = Path(file_name)
    value = path.read_text(encoding="utf-8").strip()
    if not value:
        raise ValueError(f"{name}_FILE is empty")
    return value


def _bounded_int(name: str, default: int, minimum: int, maximum: int) -> int:
    value = int(os.getenv(name, str(default)))
    if not minimum <= value <= maximum:
        raise ValueError(f"{name} must be between {minimum} and {maximum}")
    return value


def _database_identity(url: str) -> str:
    parsed = urlparse(url.replace("postgresql+asyncpg://", "postgresql://", 1))
    if parsed.scheme not in {"postgresql", "postgres"} or not parsed.hostname or not parsed.path:
        raise ValueError("shadow database URL must be a complete PostgreSQL URL")
    return parsed.username or ""


@dataclass(frozen=True)
class ShadowSettings:
    source_database_url: str
    sink_database_url: str
    pseudonym_key: bytes
    release: str
    poll_seconds: int = 15
    batch_size: int = 100
    lookback_hours: int = 24
    health_port: int = 8090

    @classmethod
    def from_environment(cls) -> ShadowSettings:
        if os.getenv("RULENIX_SHADOW_ONLY") != "required":
            raise ValueError("RULENIX_SHADOW_ONLY=required is mandatory")
        source = _secret("SHADOW_SOURCE_DATABASE_URL")
        sink = _secret("SHADOW_SINK_DATABASE_URL")
        if _database_identity(source) != "rulenix_shadow_reader":
            raise ValueError("source database identity must be rulenix_shadow_reader")
        if _database_identity(sink) != "rulenix_shadow_writer":
            raise ValueError("sink database identity must be rulenix_shadow_writer")
        pseudonym_key = _secret("SHADOW_PSEUDONYM_KEY").encode()
        if len(pseudonym_key) < 32:
            raise ValueError("shadow pseudonym key must contain at least 32 bytes")
        release = os.getenv("SHADOW_RELEASE", "").strip()
        if len(release) < 7:
            raise ValueError("SHADOW_RELEASE is required")
        return cls(
            source,
            sink,
            pseudonym_key,
            release,
            _bounded_int("SHADOW_POLL_SECONDS", 15, 5, 300),
            _bounded_int("SHADOW_BATCH_SIZE", 100, 1, 500),
            _bounded_int("SHADOW_LOOKBACK_HOURS", 24, 1, 336),
            _bounded_int("SHADOW_HEALTH_PORT", 8090, 1024, 65535),
        )


__all__ = ["ShadowSettings"]
