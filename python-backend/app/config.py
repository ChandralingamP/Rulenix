from functools import lru_cache
from uuid import UUID

from pydantic import AliasChoices, Field
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_file=(".env", "../.env"), extra="ignore")

    app_env: str = Field(default="development", validation_alias="APP_ENV")
    database_url: str = Field(default="", validation_alias="DATABASE_URL")
    frontend_origin: str = Field(default="http://localhost:5173", validation_alias="FRONTEND_ORIGIN")
    session_idle_minutes: int = Field(default=30, validation_alias="SESSION_IDLE_MINUTES")
    session_absolute_hours: int = Field(default=24, validation_alias="SESSION_ABSOLUTE_HOURS")
    otp_hash_key: str = Field(default="development-only-otp-key", validation_alias="OTP_HASH_KEY")
    credential_keys: str = Field(default="", validation_alias="CREDENTIAL_ENCRYPTION_KEYS")
    credential_primary_version: int = Field(default=1, validation_alias=AliasChoices("CREDENTIAL_PRIMARY_VERSION", "CREDENTIAL_ENCRYPTION_PRIMARY_VERSION"))
    smtp_host: str = Field(default="", validation_alias="SMTP_HOST")
    live_trading_enabled: bool = Field(default=False, validation_alias="PYTHON_LIVE_TRADING_ENABLED")
    live_authority_lease_owner: str = Field(
        default="", validation_alias="PYTHON_LIVE_AUTHORITY_LEASE_OWNER"
    )
    runtime_mode: str = Field(default="off", validation_alias="PYTHON_RUNTIME_MODE")
    authority_lease_seconds: int = Field(
        default=30, validation_alias="PYTHON_AUTHORITY_LEASE_SECONDS"
    )
    worker_interval_seconds: int = Field(
        default=5, validation_alias="PYTHON_WORKER_INTERVAL_SECONDS"
    )
    angel_base_url: str = Field(
        default="https://apiconnect.angelone.in",
        validation_alias="ANGEL_BASE_URL",
    )
    angel_websocket_url: str = Field(
        default="wss://smartapisocket.angelone.in/smart-stream",
        validation_alias="ANGEL_WEBSOCKET_URL",
    )
    angel_client_local_ip: str = Field(default="", validation_alias="ANGEL_CLIENT_LOCAL_IP")
    angel_client_public_ip: str = Field(default="", validation_alias="ANGEL_CLIENT_PUBLIC_IP")
    angel_client_mac_address: str = Field(
        default="", validation_alias="ANGEL_CLIENT_MAC_ADDRESS"
    )
    egress_helper_socket: str = Field(default="/run/rulenix-egress/helper.sock", validation_alias="EGRESS_HELPER_SOCKET")
    log_directory: str = Field(default="./logs", validation_alias="RULENIX_LOG_DIRECTORY")

    @property
    def async_database_url(self) -> str:
        url = self.database_url.strip()
        if url.startswith("postgresql://"):
            return "postgresql+asyncpg://" + url.removeprefix("postgresql://")
        if url.startswith("postgres://"):
            return "postgresql+asyncpg://" + url.removeprefix("postgres://")
        return url

    def validate_production(self) -> None:
        mode = self.runtime_mode.strip().lower()
        if mode not in {"off", "shadow", "authoritative"}:
            raise ValueError("PYTHON_RUNTIME_MODE must be off, shadow, or authoritative")
        if not 5 <= self.authority_lease_seconds <= 300:
            raise ValueError("PYTHON_AUTHORITY_LEASE_SECONDS must be between 5 and 300")
        if not 1 <= self.worker_interval_seconds <= 60:
            raise ValueError("PYTHON_WORKER_INTERVAL_SECONDS must be between 1 and 60")
        if self.live_trading_enabled != (mode == "authoritative"):
            raise ValueError(
                "PYTHON_LIVE_TRADING_ENABLED must be true only in authoritative runtime mode"
            )
        if self.live_trading_enabled:
            if not self.async_database_url:
                raise ValueError("DATABASE_URL is required for Python LIVE mutation")
            try:
                UUID(self.live_authority_lease_owner)
            except (TypeError, ValueError) as exc:
                raise ValueError(
                    "PYTHON_LIVE_AUTHORITY_LEASE_OWNER must be an explicit UUID"
                ) from exc
        if self.app_env.lower() in {"production", "staging"}:
            if not self.async_database_url:
                raise ValueError("DATABASE_URL is required outside development")
            if len(self.otp_hash_key.encode()) < 32:
                raise ValueError("OTP_HASH_KEY must contain at least 32 bytes")
            if not self.frontend_origin.startswith("https://"):
                raise ValueError("FRONTEND_ORIGIN must use HTTPS outside development")
            if mode != "off" and not self.credential_keys:
                raise ValueError("CREDENTIAL_ENCRYPTION_KEYS is required for runtime broker reads")
            if mode != "off" and not all(
                (
                    self.angel_client_local_ip,
                    self.angel_client_public_ip,
                    self.angel_client_mac_address,
                )
            ):
                raise ValueError("Angel client network identity is required for runtime broker reads")


@lru_cache
def get_settings() -> Settings:
    settings = Settings()
    settings.validate_production()
    return settings
