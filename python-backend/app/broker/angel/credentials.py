from base64 import b64decode
from dataclasses import dataclass, field

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from pydantic import SecretStr
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from .errors import BrokerError, BrokerErrorCategory
from .models import AccountContext


@dataclass(frozen=True)
class CredentialSet:
    api_key: SecretStr = field(default_factory=lambda: SecretStr(""))
    jwt_token: SecretStr = field(default_factory=lambda: SecretStr(""))
    refresh_token: SecretStr = field(default_factory=lambda: SecretStr(""))
    feed_token: SecretStr = field(default_factory=lambda: SecretStr(""))

    def __repr__(self) -> str:
        return "CredentialSet(api_key=[REDACTED], jwt_token=[REDACTED], refresh_token=[REDACTED], feed_token=[REDACTED])"


class CredentialCipher:
    def __init__(self, encoded_keys: str, primary_version: int):
        self.primary_version = primary_version
        self._keys: dict[int, bytes] = {}
        for entry in encoded_keys.split(","):
            if not entry.strip():
                continue
            version, encoded = entry.split(":", 1)
            numeric_version = int(version)
            key = b64decode(encoded)
            if numeric_version <= 0 or numeric_version in self._keys or len(key) != 32:
                raise ValueError("credential keys must be unique positive AES-256 keys")
            self._keys[numeric_version] = key
        if primary_version not in self._keys:
            raise ValueError("primary credential key version is not configured")

    def decrypt(self, user_id: str, kind: str, version: int, nonce: bytes, ciphertext: bytes) -> str:
        key = self._keys.get(version)
        if key is None or len(nonce) != 12:
            raise ValueError("encrypted credential is corrupt or key version is unavailable")
        aad = f"rulenix:broker-secret:{user_id}:{kind}:v{version}".encode()
        try:
            return AESGCM(key).decrypt(nonce, ciphertext, aad).decode()
        except Exception as exc:
            raise ValueError("encrypted credential authentication failed") from exc


class PostgresCredentialProvider:
    def __init__(self, db: AsyncSession, cipher: CredentialCipher):
        self.db = db
        self.cipher = cipher

    async def load(self, user_id: str, *, client_local_ip: str = "", client_public_ip: str = "", client_mac_address: str = "") -> AccountContext:
        profile = (await self.db.execute(text("SELECT brokerage_user_id,COALESCE(broker_credential_revision,0) credential_revision FROM user_profiles WHERE user_id=:user_id"), {"user_id": user_id})).mappings().first()
        if not profile:
            raise BrokerError(BrokerErrorCategory.UNKNOWN, "Broker profile not found.", "credential-load")
        rows = (await self.db.execute(text("SELECT secret_kind,key_version,nonce,ciphertext FROM broker_secrets WHERE user_id=:user_id"), {"user_id": user_id})).mappings().all()
        values: dict[str, SecretStr] = {}
        try:
            for row in rows:
                values[row["secret_kind"]] = SecretStr(self.cipher.decrypt(user_id, row["secret_kind"], row["key_version"], bytes(row["nonce"]), bytes(row["ciphertext"])))
        except ValueError as exc:
            raise BrokerError(BrokerErrorCategory.UNKNOWN, "Broker credentials could not be decrypted.", "credential-load", diagnostic="credential_decryption_failed") from exc
        return AccountContext(user_id, profile["brokerage_user_id"], values.get("api_key", SecretStr("")), values.get("jwt_token", SecretStr("")), values.get("refresh_token", SecretStr("")), values.get("feed_token", SecretStr("")), profile["credential_revision"], client_local_ip, client_public_ip, client_mac_address)
