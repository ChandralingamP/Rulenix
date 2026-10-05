from __future__ import annotations

from datetime import UTC, datetime, timedelta
from zoneinfo import ZoneInfo

import httpx
from fastapi import APIRouter, Depends, Request
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..broker.angel.auth import AngelAuthenticator
from ..broker.angel.credentials import CredentialCipher, PostgresCredentialProvider
from ..broker.angel.egress import DatabaseEgressBinding, EgressBindingError, _bound_http_client
from ..broker.angel.errors import BrokerError
from ..dependencies import Principal, current_user, get_db, hmac_compare
from ..errors import DomainError
from ..security import digest, encrypt_broker_secret

router = APIRouter(tags=["home"])


async def _details(db: AsyncSession, user_id: str) -> dict:
    row = (
        (
            await db.execute(
                text("""
        SELECT COALESCE(p.brokerage_user_id,'') AS client_id,p.updated_at AS last_updated,
               COALESCE(p.token_state,'missing') AS token_state,p.token_received_at,
               p.last_token_check_at,COALESCE(p.last_token_status,'missing') AS last_token_status,
               COALESCE(p.last_token_message,'') AS last_token_message,
               EXISTS(SELECT 1 FROM broker_secrets s WHERE s.user_id=u.id AND s.secret_kind='api_key') AS api_key_configured,
               EXISTS(SELECT 1 FROM broker_secrets s WHERE s.user_id=u.id AND s.secret_kind='jwt_token')
               AND EXISTS(SELECT 1 FROM broker_secrets s WHERE s.user_id=u.id AND s.secret_kind='refresh_token')
               AND EXISTS(SELECT 1 FROM broker_secrets s WHERE s.user_id=u.id AND s.secret_kind='feed_token') AS has_all_session_tokens
          FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=:user
    """),
                {"user": user_id},
            )
        )
        .mappings()
        .first()
    )
    if not row:
        raise DomainError(404, "User profile not found.")
    received = row["token_received_at"]
    connected_today = bool(
        row["api_key_configured"]
        and row["has_all_session_tokens"]
        and received is not None
        and received.astimezone(ZoneInfo("Asia/Kolkata")).date()
        == datetime.now(UTC).astimezone(ZoneInfo("Asia/Kolkata")).date()
    )
    token_state = row["token_state"]
    last_status = row["last_token_status"]
    if connected_today and (
        token_state in {"verification_unavailable", "refresh_required"}
        or last_status in {"invalid", "expired", "failed"}
    ):
        connection_state = "unavailable"
    elif connected_today:
        connection_state = "connected"
    elif token_state in {"verification_unavailable", "refresh_required"}:
        connection_state = "unavailable"
    elif last_status in {"invalid", "expired", "unavailable", "failed"}:
        connection_state = last_status
    elif not row["has_all_session_tokens"]:
        connection_state = "idle"
    else:
        connection_state = "expired"
    if connection_state in {"connected", "idle"}:
        message = None
    elif connection_state == "unavailable":
        message = (
            "Today's broker session was preserved. Rulenix will retry automatically; no new login is required."
            if connected_today
            else "Angel One is temporarily unavailable. Rulenix will retry automatically."
        )
    elif connection_state == "expired":
        message = "Daily brokerage session expired. Connect with your MPIN and TOTP for today's session."
    else:
        message = row["last_token_message"]
    return {
        "client_id": row["client_id"],
        "api_key_configured": bool(row["api_key_configured"]),
        "last_updated": row["last_updated"],
        "connection_state": connection_state,
        "token_state": token_state,
        "connection_message": message,
        "connected_for_today": connected_today,
        "last_connected_at": received,
        "last_verified_at": row["last_token_check_at"],
    }


@router.get("/home/status/")
async def status(user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    return await _details(db, user.id)


@router.post("/home/connect/")
async def connect(request: Request, payload: dict | None = None):
    if not payload:
        raise DomainError(
            503,
            "Python broker session connection is unavailable without a configured read-only session.",
            code="python_foundation_deferred",
        )
    if request is None or not getattr(request.app.state, "session_factory", None):
        raise DomainError(503, "Database unavailable.", code="database_unavailable")
    token = request.cookies.get("rulenix_session")
    if not token:
        raise DomainError(401, "Authentication required.")

    async with request.app.state.session_factory() as db:
        row = (
            (
                await db.execute(
                    text("""
                SELECT s.id AS session_id, u.id AS user_id, u.username, s.csrf_hash,
                       s.absolute_expires_at, s.idle_expires_at
                  FROM user_sessions s
                  JOIN users u ON u.id = s.user_id
                 WHERE s.token_hash = :token_hash
                   AND s.revoked_at IS NULL
                   AND s.idle_expires_at > NOW()
                   AND s.absolute_expires_at > NOW()
                   AND u.is_active = TRUE
                   AND s.created_at >= u.password_changed_at
                """),
                    {"token_hash": digest(token)},
                )
            )
            .mappings()
            .first()
        )
        if not row:
            raise DomainError(401, "Authentication required.")

        csrf = request.headers.get("X-CSRF-Token") or request.cookies.get("rulenix_csrf")
        if not csrf or not hmac_compare(digest(csrf), bytes(row["csrf_hash"])):
            raise DomainError(403, "Invalid CSRF token.")

        mpin = str(payload.get("mpin", "")).strip()
        totp = str(payload.get("totp", "")).strip()
        if not (mpin.isdigit() and 4 <= len(mpin) <= 16 and totp.isdigit() and 6 <= len(totp) <= 8):
            raise DomainError(400, "A valid MPIN and numeric TOTP are required.")

        user_id = str(row["user_id"])
        settings = request.app.state.settings

        now = datetime.now(UTC)
        idle = min(
            row["absolute_expires_at"],
            now + timedelta(minutes=settings.session_idle_minutes),
        )
        await db.execute(
            text(
                "UPDATE user_sessions SET last_seen_at = NOW(), idle_expires_at = :idle WHERE id = :id"
            ),
            {"idle": idle, "id": row["session_id"]},
        )

        try:
            cipher = CredentialCipher(settings.credential_keys, settings.credential_primary_version)
        except ValueError as exc:
            raise DomainError(
                503,
                "Credential encryption is not configured.",
                code="python_foundation_deferred",
            ) from exc

        egress = DatabaseEgressBinding(db)
        try:
            selection = await egress.select(user_id)
        except EgressBindingError as exc:
            raise DomainError(503, str(exc), code="egress_unavailable") from exc

        provider = PostgresCredentialProvider(db, cipher)
        try:
            account = await provider.load(
                user_id,
                client_local_ip=settings.angel_client_local_ip,
                client_public_ip=selection.public_ip or settings.angel_client_public_ip,
                client_mac_address=settings.angel_client_mac_address,
            )
        except BrokerError as exc:
            raise DomainError(400, str(exc)) from exc

        if not account.client_code.strip():
            raise DomainError(400, "Add an Angel One Client ID before connecting.")
        if not account.api_key.get_secret_value().strip():
            raise DomainError(400, "Add an Angel One API key before connecting.")

        custom_factory = getattr(request.app.state, "broker_transport_factory", None)
        try:
            if custom_factory is not None:
                transport = custom_factory(selection)
            elif selection.explicit:
                transport = _bound_http_client(selection)
            else:
                transport = httpx.AsyncClient(transport=httpx.AsyncHTTPTransport(trust_env=False))
        except Exception as exc:
            raise DomainError(
                503,
                f"Configured Angel egress IP is unavailable; broker operation blocked: {exc}",
                code="egress_unavailable",
            ) from exc

        await db.execute(
            text("""
            UPDATE user_profiles
               SET token_state = 'connecting',
                   last_token_check_at = NOW(),
                   last_token_status = '',
                   last_token_message = 'Broker connection is being established.',
                   updated_at = NOW()
             WHERE user_id = :user_id
            """),
            {"user_id": user_id},
        )
        await db.commit()

        authenticator = AngelAuthenticator(settings.angel_base_url, transport)
        try:
            session = await authenticator.login(account, mpin, totp)
        except BrokerError as exc:
            await db.execute(
                text("""
                UPDATE user_profiles
                   SET token_state = 'failed',
                       last_token_check_at = NOW(),
                       last_token_status = 'failed',
                       last_token_message = :msg,
                       updated_at = NOW()
                 WHERE user_id = :user_id
                """),
                {
                    "user_id": user_id,
                    "msg": "Broker connection failed. Verify the Client ID, API key, MPIN, and TOTP before retrying.",
                },
            )
            await db.commit()
            raise DomainError(
                400,
                "Broker connection failed. Verify the Client ID, API key, MPIN, and TOTP before retrying.",
            ) from exc
        except Exception as exc:
            await db.execute(
                text("""
                UPDATE user_profiles
                   SET token_state = 'failed',
                       last_token_check_at = NOW(),
                       last_token_status = 'failed',
                       last_token_message = :msg,
                       updated_at = NOW()
                 WHERE user_id = :user_id
                """),
                {
                    "user_id": user_id,
                    "msg": "Broker connection failed. Verify the Client ID, API key, MPIN, and TOTP before retrying.",
                },
            )
            await db.commit()
            raise DomainError(
                503,
                f"Angel One connection failed: {exc}",
                code="broker_connection_failed",
            ) from exc
        finally:
            await transport.aclose()

        tokens = [
            ("jwt_token", session.jwt_token.get_secret_value()),
            ("refresh_token", session.refresh_token.get_secret_value()),
            ("feed_token", session.feed_token.get_secret_value()),
        ]
        for kind, token_val in tokens:
            version, nonce, ciphertext = encrypt_broker_secret(
                token_val,
                user_id,
                kind,
                settings.credential_keys,
                settings.credential_primary_version,
            )
            await db.execute(
                text("""
                INSERT INTO broker_secrets (user_id, secret_kind, key_version, nonce, ciphertext)
                VALUES (:user_id, :kind, :version, :nonce, :ciphertext)
                ON CONFLICT (user_id, secret_kind)
                DO UPDATE SET key_version = EXCLUDED.key_version,
                              nonce = EXCLUDED.nonce,
                              ciphertext = EXCLUDED.ciphertext,
                              updated_at = NOW()
                """),
                {
                    "user_id": user_id,
                    "kind": kind,
                    "version": version,
                    "nonce": nonce,
                    "ciphertext": ciphertext,
                },
            )

        await db.execute(
            text("""
            UPDATE user_profiles
               SET token_state = 'connected',
                   token_received_at = NOW(),
                   last_token_check_at = NOW(),
                   last_token_status = 'success',
                   last_token_message = '',
                   broker_credential_revision = COALESCE(broker_credential_revision, 0) + 1,
                   updated_at = NOW()
             WHERE user_id = :user_id
            """),
            {"user_id": user_id},
        )
        await db.commit()

        details = await _details(db, user_id)
        return {
            "message": "Brokerage session established successfully.",
            "last_connected_at": details["last_connected_at"],
            "details": details,
        }


@router.patch("/home/profile/")
async def update_profile(
    payload: dict,
    request: Request,
    user: Principal = Depends(current_user),
    db: AsyncSession = Depends(get_db),
):
    api_key = str(payload.get("api_key", "")).strip()
    if not api_key or len(api_key) > 128:
        raise DomainError(400, "API key must be between 1 and 128 characters.")
    try:
        version, nonce, ciphertext = encrypt_broker_secret(
            api_key,
            str(user.id),
            "api_key",
            request.app.state.settings.credential_keys,
            request.app.state.settings.credential_primary_version,
        )
    except ValueError as exc:
        raise DomainError(
            503, "Credential encryption is not configured.", code="python_foundation_deferred"
        ) from exc
    await db.execute(
        text(
            "INSERT INTO broker_secrets(user_id,secret_kind,key_version,nonce,ciphertext) VALUES(:user,'api_key',:version,:nonce,:ciphertext) ON CONFLICT(user_id,secret_kind) DO UPDATE SET key_version=EXCLUDED.key_version,nonce=EXCLUDED.nonce,ciphertext=EXCLUDED.ciphertext,updated_at=NOW()"
        ),
        {"user": user.id, "version": version, "nonce": nonce, "ciphertext": ciphertext},
    )
    await db.execute(
        text(
            "UPDATE user_profiles SET broker_credential_revision=COALESCE(broker_credential_revision,0)+1,token_state='invalid',last_token_status='invalid',token_received_at=NULL,last_token_check_at=NOW(),last_token_message='The broker API key changed. Establish the broker connection again.',updated_at=NOW() WHERE user_id=:user"
        ),
        {"user": user.id},
    )
    await db.commit()
    return {"message": "Profile updated successfully.", "details": await _details(db, user.id)}
