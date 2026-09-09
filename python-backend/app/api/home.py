from __future__ import annotations

from datetime import UTC, datetime
from zoneinfo import ZoneInfo

from fastapi import APIRouter, Depends, Request
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, current_user, get_db
from ..errors import DomainError
from ..security import digest, encrypt_broker_secret

router = APIRouter(tags=["home"])


async def _details(db: AsyncSession, user_id: str) -> dict:
    row = (await db.execute(text("""
        SELECT COALESCE(p.brokerage_user_id,'') AS client_id,p.updated_at AS last_updated,
               COALESCE(p.token_state,'missing') AS token_state,p.token_received_at,
               p.last_token_check_at,COALESCE(p.last_token_status,'missing') AS last_token_status,
               COALESCE(p.last_token_message,'') AS last_token_message,
               EXISTS(SELECT 1 FROM broker_secrets s WHERE s.user_id=u.id AND s.secret_kind='api_key') AS api_key_configured,
               EXISTS(SELECT 1 FROM broker_secrets s WHERE s.user_id=u.id AND s.secret_kind='jwt_token')
               AND EXISTS(SELECT 1 FROM broker_secrets s WHERE s.user_id=u.id AND s.secret_kind='refresh_token')
               AND EXISTS(SELECT 1 FROM broker_secrets s WHERE s.user_id=u.id AND s.secret_kind='feed_token') AS has_all_session_tokens
          FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=:user
    """), {"user": user_id})).mappings().first()
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
        connection_state = "connected"
    if connection_state in {"connected", "idle"}:
        message = None
    elif connection_state == "unavailable":
        message = "Angel One is temporarily unavailable. Rulenix will retry automatically."
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
        raise DomainError(503, "Python broker session connection is unavailable without a configured read-only session.", code="python_foundation_deferred")
    mpin, totp = str(payload.get("mpin", "")), str(payload.get("totp", ""))
    if not (mpin.isdigit() and 4 <= len(mpin) <= 16 and totp.isdigit() and 6 <= len(totp) <= 8):
        raise DomainError(400, "A valid MPIN and numeric TOTP are required.")
    if request is None or not request.app.state.session_factory:
        raise DomainError(503, "Database unavailable.", code="database_unavailable")
    token = request.cookies.get("rulenix_session")
    if not token:
        raise DomainError(401, "Authentication required.")
    async with request.app.state.session_factory() as db:
        valid = await db.scalar(text("SELECT EXISTS(SELECT 1 FROM user_sessions s JOIN users u ON u.id=s.user_id WHERE s.token_hash=:token AND s.revoked_at IS NULL AND s.idle_expires_at>NOW() AND s.absolute_expires_at>NOW() AND u.is_active)"), {"token": digest(token)})
    if not valid:
        raise DomainError(401, "Authentication required.")
    raise DomainError(503, "Broker connection is not enabled in the Python migration shadow.", code="python_broker_session_deferred")


@router.patch("/home/profile/")
async def update_profile(payload: dict, request: Request, user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    api_key = str(payload.get("api_key", "")).strip()
    if not api_key or len(api_key) > 128:
        raise DomainError(400, "API key must be between 1 and 128 characters.")
    try:
        version, nonce, ciphertext = encrypt_broker_secret(api_key, str(user.id), "api_key", request.app.state.settings.credential_keys, request.app.state.settings.credential_primary_version)
    except ValueError as exc:
        raise DomainError(503, "Credential encryption is not configured.", code="python_foundation_deferred") from exc
    await db.execute(text("INSERT INTO broker_secrets(user_id,secret_kind,key_version,nonce,ciphertext) VALUES(:user,'api_key',:version,:nonce,:ciphertext) ON CONFLICT(user_id,secret_kind) DO UPDATE SET key_version=EXCLUDED.key_version,nonce=EXCLUDED.nonce,ciphertext=EXCLUDED.ciphertext,updated_at=NOW()"), {"user": user.id, "version": version, "nonce": nonce, "ciphertext": ciphertext})
    await db.execute(text("UPDATE user_profiles SET broker_credential_revision=COALESCE(broker_credential_revision,0)+1,token_state='invalid',last_token_status='invalid',token_received_at=NULL,last_token_check_at=NOW(),last_token_message='The broker API key changed. Establish the broker connection again.',updated_at=NOW() WHERE user_id=:user"), {"user": user.id})
    await db.commit()
    return {"message": "Profile updated successfully.", "details": await _details(db, user.id)}
