import base64
import hashlib
import hmac
import re
import secrets

from argon2 import PasswordHasher
from argon2.exceptions import VerificationError, VerifyMismatchError
from cryptography.hazmat.primitives.ciphers.aead import AESGCM

PASSWORD_HASHER = PasswordHasher()
USERNAME_RE = re.compile(r"^[A-Za-z0-9_.-]{3,64}$")


def valid_email(value: str) -> bool:
    if not value or len(value) > 254 or any(ch.isspace() for ch in value) or value.count("@") != 1:
        return False
    local, domain = value.rsplit("@", 1)
    return bool(local and domain and "." in domain and re.fullmatch(r"[A-Za-z0-9.-]+", domain))


def valid_username(value: str) -> bool:
    return bool(USERNAME_RE.fullmatch(value))


def password_error(username: str, email: str, password: str) -> str | None:
    if not 12 <= len(password) <= 128:
        return "Password must be 12-128 characters and include upper, lower, digit, and symbol."
    if any(ch.isspace() for ch in password) or not re.search(r"[A-Z]", password) or not re.search(r"[a-z]", password) or not re.search(r"\d", password) or not re.search(r"[^A-Za-z0-9]", password):
        return "Password must be 12-128 characters and include upper, lower, digit, and symbol."
    local = email.split("@", 1)[0].lower()
    if username.lower() in password.lower() or (local and local in password.lower()):
        return "Password must not contain the username or email local part."
    return None


def new_token() -> str:
    return secrets.token_urlsafe(32)


def digest(value: str) -> bytes:
    return hashlib.sha256(value.encode()).digest()


def otp_digest(key: str, email: str, purpose: str, otp: str) -> str:
    raw = hmac.new(key.encode(), f"{email.lower()}\0{purpose}\0{otp}".encode(), hashlib.sha256).digest()
    return base64.b64encode(raw).decode()


def hash_password(password: str) -> str:
    return PASSWORD_HASHER.hash(password)


def verify_password(encoded: str, password: str) -> bool:
    try:
        return PASSWORD_HASHER.verify(encoded, password)
    except (VerifyMismatchError, VerificationError, ValueError):
        return False


def encrypt_broker_secret(value: str, user_id: str, kind: str, keys: str, primary_version: int) -> tuple[int, bytes, bytes]:
    """Encrypt a credential using the Rust-compatible AES-256-GCM envelope."""
    import base64
    key_map: dict[int, bytes] = {}
    for item in keys.split(","):
        if not item.strip():
            continue
        version, encoded = item.split(":", 1)
        key_map[int(version)] = base64.b64decode(encoded)
    key = key_map.get(primary_version)
    if key is None or len(key) != 32:
        raise ValueError("credential encryption key is not configured")
    nonce = secrets.token_bytes(12)
    aad = f"rulenix:broker-secret:{user_id}:{kind}:v{primary_version}".encode()
    return primary_version, nonce, AESGCM(key).encrypt(nonce, value.encode(), aad)
