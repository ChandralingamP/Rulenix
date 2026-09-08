from functools import lru_cache

from pydantic import AliasChoices, Field, field_validator
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
    egress_helper_socket: str = Field(default="/run/rulenix-egress/helper.sock", validation_alias="EGRESS_HELPER_SOCKET")
    log_directory: str = Field(default="./logs", validation_alias="RULENIX_LOG_DIRECTORY")

    @field_validator("live_trading_enabled")
    @classmethod
    def reject_live(cls, value: bool) -> bool:
        if value:
            raise ValueError("Python foundation cannot enable live trading")
        return value

    @property
    def async_database_url(self) -> str:
        url = self.database_url.strip()
        if url.startswith("postgresql://"):
            return "postgresql+asyncpg://" + url.removeprefix("postgresql://")
        if url.startswith("postgres://"):
            return "postgresql+asyncpg://" + url.removeprefix("postgres://")
        return url

    def validate_production(self) -> None:
        if self.app_env.lower() in {"production", "staging"}:
            if not self.async_database_url:
                raise ValueError("DATABASE_URL is required outside development")
            if len(self.otp_hash_key.encode()) < 32:
                raise ValueError("OTP_HASH_KEY must contain at least 32 bytes")
            if not self.frontend_origin.startswith("https://"):
                raise ValueError("FRONTEND_ORIGIN must use HTTPS outside development")


@lru_cache
def get_settings() -> Settings:
    settings = Settings()
    settings.validate_production()
    return settings
