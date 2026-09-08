from __future__ import annotations

from fastapi import APIRouter, Depends, Request
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, current_user, get_db
from ..errors import DomainError
from ..security import digest, encrypt_broker_secret

router = APIRouter(tags=["home"])


async def _details(db: AsyncSession, user_id: str) -> dict:
    row = (await db.execute(text("""
        SELECT u.username,COALESCE(p.brokerage_user_id,'') AS brokerage_user_id,
               COALESCE(p.broker_credential_revision,0) AS broker_credential_revision,
               COALESCE(p.token_state,'missing') AS token_state,p.token_received_at,
               COALESCE(p.last_token_status,'missing') AS last_token_status,
               COALESCE(p.last_token_message,'') AS last_token_message,
               COALESCE(h.healthy,FALSE) AS reconciliation_healthy,h.checked_at AS reconciliation_checked_at,
               h.broker_credential_revision AS reconciliation_revision,
               CASE WHEN p.brokerage_user_id IS NOT NULL AND p.brokerage_user_id<>'' THEN TRUE ELSE FALSE END AS configured
          FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id
          LEFT JOIN broker_reconciliation_health h ON h.user_id=u.id WHERE u.id=:user
    """), {"user": user_id})).mappings().first()
    if not row:
        raise DomainError(404, "User profile not found.")
    connected = row["last_token_status"] in {"success", "refreshed", "connected"} and row["token_received_at"] is not None
    return {"username": row["username"], "brokerage_user_id": row["brokerage_user_id"], "connection_state": "connected" if connected else "disconnected", "connection_message": row["last_token_message"] or ("Brokerage session is connected." if connected else "Connect Angel One before using broker features."), "last_connected_at": row["token_received_at"], "authenticated": connected, "configured": row["configured"], "deployment_safe": False, "live_permitted": False, "live_ready": bool(row["reconciliation_healthy"] and row["reconciliation_revision"] == row["broker_credential_revision"]), "reconciliation_healthy": bool(row["reconciliation_healthy"]), "reconciliation_checked_at": row["reconciliation_checked_at"], "credential_revision": row["broker_credential_revision"]}


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
