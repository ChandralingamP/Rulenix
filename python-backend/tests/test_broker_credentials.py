from base64 import b64encode

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from pydantic import SecretStr

from app.broker.angel.credentials import CredentialCipher, CredentialSet


def test_rust_compatible_credential_aad_and_redacted_repr():
    key = bytes(range(32))
    encoded = b64encode(key).decode()
    cipher = CredentialCipher(f"1:{encoded}", 1)
    nonce = bytes(range(12))
    user_id = "00000000-0000-0000-0000-000000000001"
    aad = f"rulenix:broker-secret:{user_id}:api_key:v1".encode()
    encrypted = AESGCM(key).encrypt(nonce, b"secret-api-key", aad)
    assert cipher.decrypt(user_id, "api_key", 1, nonce, encrypted) == "secret-api-key"
    assert "secret-api-key" not in repr(CredentialSet(SecretStr("secret-api-key")))

