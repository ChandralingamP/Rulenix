import asyncio
from datetime import UTC, datetime
from typing import Any

import httpx
from pydantic import BaseModel, SecretStr

from .errors import BrokerError, BrokerErrorCategory
from .models import AccountContext, BrokerSession


class LoginRequest(BaseModel):
    clientcode: str
    password: SecretStr
    totp: SecretStr
    state: str = "STATE_VARIABLE"


class SessionTokens(BaseModel):
    jwt_token: SecretStr
    refresh_token: SecretStr
    feed_token: SecretStr


def _tokens(data: Any, operation: str) -> SessionTokens:
    if not isinstance(data, dict):
        raise BrokerError(BrokerErrorCategory.MALFORMED_RESPONSE, "Broker session data is malformed.", operation, diagnostic="session_data_not_object", raw=data)
    try:
        return SessionTokens(
            jwt_token=data.get("jwtToken", ""),
            refresh_token=data.get("refreshToken", ""),
            feed_token=data.get("feedToken", ""),
        )
    except Exception as exc:
        raise BrokerError(BrokerErrorCategory.MALFORMED_RESPONSE, "Broker session tokens are malformed.", operation, diagnostic="session_token_schema", raw=data) from exc


class AngelAuthenticator:
    def __init__(self, base_url: str, transport: httpx.AsyncClient):
        self.base_url = base_url.rstrip("/")
        self.transport = transport

    async def login(self, account: AccountContext, mpin: str, totp: str) -> BrokerSession:
        request = LoginRequest(clientcode=account.client_code, password=SecretStr(mpin), totp=SecretStr(totp))
        response = None
        for attempt in range(2):
            try:
                response = await self.transport.post(
                    f"{self.base_url}/rest/auth/angelbroking/user/v1/loginByPassword",
                    headers=_base_headers(account),
                    json=request.model_dump(mode="json"),
                    timeout=8.0,
                )
                break
            except (httpx.ConnectError, httpx.TimeoutException) as exc:
                if attempt == 1:
                    raise _transport_error("login", exc, retryable=False) from exc
                await asyncio.sleep(0.35)
        assert response is not None
        data = _decode_envelope(response, "login")
        tokens = _tokens(data, "login")
        return BrokerSession(account.user_id, account.client_code, tokens.jwt_token, tokens.refresh_token, tokens.feed_token, datetime.now(UTC), account.credential_revision + 1)

    async def refresh(self, account: AccountContext) -> SessionTokens:
        if not account.refresh_token.get_secret_value().strip():
            raise BrokerError(BrokerErrorCategory.AUTHENTICATION_INVALID, "Refresh token is missing; reconnect is required.", "refresh")
        for attempt in range(2):
            try:
                response = await self.transport.post(
                    f"{self.base_url}/rest/auth/angelbroking/jwt/v1/generateTokens",
                    headers=_authenticated_headers(account),
                    json={"refreshToken": account.refresh_token.get_secret_value()},
                )
                data = _decode_envelope(response, "refresh")
                return _tokens(data, "refresh")
            except BrokerError as exc:
                if attempt == 1 or exc.category in {BrokerErrorCategory.AUTHENTICATION_INVALID, BrokerErrorCategory.RATE_LIMITED}:
                    raise
                await asyncio.sleep(0.4)
            except (httpx.ConnectError, httpx.TimeoutException) as exc:
                if attempt == 1:
                    raise _transport_error("refresh", exc, retryable=False) from exc
                await asyncio.sleep(0.4)
        raise RuntimeError("unreachable")


def _base_headers(account: AccountContext) -> dict[str, str]:
    return {"Content-Type": "application/json", "Accept": "application/json", "x-privatekey": account.api_key.get_secret_value(), "x-usertype": "USER", "x-sourceid": "WEB", "x-clientlocalip": account.client_local_ip, "x-clientpublicip": account.client_public_ip, "x-macaddress": account.client_mac_address}


def _authenticated_headers(account: AccountContext) -> dict[str, str]:
    headers = _base_headers(account)
    headers["Authorization"] = f"Bearer {account.jwt_token.get_secret_value()}"
    return headers


def _transport_error(operation: str, exc: Exception, retryable: bool) -> BrokerError:
    category = BrokerErrorCategory.TIMEOUT if isinstance(exc, httpx.TimeoutException) else BrokerErrorCategory.TRANSPORT_FAILURE
    return BrokerError(category, f"Angel One {operation} transport failed.", operation, retryable=retryable, diagnostic=type(exc).__name__)


def _decode_envelope(response: httpx.Response, operation: str) -> Any:
    try:
        payload = response.json()
    except ValueError as exc:
        raise BrokerError(BrokerErrorCategory.MALFORMED_RESPONSE, f"Angel One {operation} returned malformed JSON.", operation, status_code=response.status_code, retryable=False) from exc
    return _validate_envelope(payload, response.status_code, operation)


def _validate_envelope(payload: Any, status_code: int, operation: str) -> Any:
    if not isinstance(payload, dict):
        raise BrokerError(BrokerErrorCategory.MALFORMED_RESPONSE, f"Angel One {operation} returned a malformed envelope.", operation, status_code=status_code, raw=payload)
    status = payload.get("status", payload.get("success"))
    message = str(payload.get("message") or "")
    code = payload.get("errorcode", payload.get("errorCode"))
    if status_code == 408:
        raise BrokerError(BrokerErrorCategory.TIMEOUT, f"Angel One {operation} timed out.", operation, status_code=status_code, retryable=True, raw=payload)
    if status_code >= 500:
        raise BrokerError(BrokerErrorCategory.TRANSPORT_FAILURE, f"Angel One {operation} returned a server failure.", operation, status_code=status_code, retryable=True, raw=payload)
    if status_code == 429 or "rate limit" in message.lower() or "access rate" in message.lower():
        raise BrokerError(BrokerErrorCategory.RATE_LIMITED, "Angel One rate limit is active.", operation, status_code=status_code, code=code, retry_after_seconds=90, retryable=False, raw=payload)
    if status_code in {401, 403} or code in {"AG8001", "AG8002", "AG8003", "AG8004", "AB1010"} or any(token in message.lower() for token in ("invalid token", "token expired", "invalid session", "unauthorized")):
        category = BrokerErrorCategory.AUTHENTICATION_EXPIRED if code in {"AG8001", "AG8002", "AB1010"} or "expired" in message.lower() else BrokerErrorCategory.AUTHENTICATION_INVALID
        raise BrokerError(category, "Angel One authentication is not valid.", operation, status_code=status_code, code=code, raw=payload)
    if not 200 <= status_code < 300 or status is False:
        category = BrokerErrorCategory.ORDER_NOT_FOUND if code == "AB1007" or "order not found" in message.lower() else BrokerErrorCategory.BROKER_REJECTED
        raise BrokerError(category, message or f"Angel One rejected {operation}.", operation, status_code=status_code, code=code, raw=payload)
    return payload.get("data")
