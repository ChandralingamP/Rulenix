import secrets
from datetime import UTC, datetime
from uuid import uuid4

from fastapi import APIRouter, Depends, Request
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, current_user, get_db
from ..errors import DomainError
from ..security import otp_digest

router = APIRouter(prefix="/account", tags=["account"])

@router.get("/profile")
async def profile(user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    row = (await db.execute(text("SELECT u.username,u.email,COALESCE(p.mobile_number,'') mobile_number,COALESCE(p.brokerage_user_id,'') client_id,COALESCE(p.trading_mode,'demo') trading_mode,u.can_administer,u.can_live_trade FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=:id"), {"id": user.id})).mappings().first()
    if not row:
        raise DomainError(404, "User not found.")
    return {
        "username": row["username"],
        "email": row["email"],
        "mobile_number": row["mobile_number"],
        "client_id": row["client_id"],
        "trading_mode": row["trading_mode"],
        "permissions": {
            "administer_users": bool(row["can_administer"]),
            "live_trading": bool(row["can_live_trade"]),
        },
    }


@router.patch("/profile")
async def update_profile(request: Request, payload: dict, user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    otp = str(payload.get("otp", "")).strip()
    email = str(payload.get("email", "")).strip().lower()
    username = str(payload.get("new_username", "")).strip().upper()
    mobile = str(payload.get("mobile_number", "")).strip()
    client_id = str(payload.get("client_id", "")).strip().upper()
    if len(username) < 3 or len(username) > 64 or not all(char.isalnum() or char in ".-_" for char in username):
        raise DomainError(400, "Username must be 3 to 64 characters and use only letters, numbers, dot, dash, or underscore.")
    if "@" not in email:
        raise DomainError(400, "Enter a valid email address.")
    if len(mobile) != 10 or not mobile.isdigit():
        raise DomainError(400, "Enter a valid 10-digit mobile number.")
    if not client_id or len(client_id) > 64:
        raise DomainError(400, "Client ID is required.")
    if len(otp) != 6 or not otp.isdigit():
        raise DomainError(400, "Invalid or expired OTP.")
    current_email = await db.scalar(text("SELECT email FROM users WHERE id=:user"), {"user": user.id})
    otp_row = (await db.execute(text("SELECT id,otp_hash,expires_at FROM email_otps WHERE LOWER(email)=LOWER(:email) AND purpose='profile_update' AND is_used=FALSE AND invalidated_at IS NULL ORDER BY created_at DESC LIMIT 1 FOR UPDATE"), {"email": current_email})).mappings().first()
    if not otp_row or otp_row["expires_at"] <= datetime.now(UTC) or otp_digest(request.app.state.settings.otp_hash_key, current_email, "profile_update", otp) != otp_row["otp_hash"]:
        raise DomainError(400, "Invalid or expired OTP.")
    await db.execute(text("UPDATE users SET username=:username,email=:email,updated_at=NOW() WHERE id=:user"), {"username": username, "email": email, "user": user.id})
    await db.execute(text("INSERT INTO user_profiles(user_id,brokerage_user_id,api_key,mobile_number) VALUES(:user,:client,'',:mobile) ON CONFLICT(user_id) DO UPDATE SET brokerage_user_id=EXCLUDED.brokerage_user_id,mobile_number=EXCLUDED.mobile_number,updated_at=NOW()"), {"user": user.id, "client": client_id, "mobile": mobile})
    await db.execute(text("UPDATE email_otps SET is_used=TRUE,invalidated_at=NOW() WHERE id=:id"), {"id": otp_row["id"]})
    await db.commit()
    row = (await db.execute(text("SELECT username,email,mobile_number,brokerage_user_id FROM users u JOIN user_profiles p ON p.user_id=u.id WHERE u.id=:user"), {"user": user.id})).mappings().one()
    return {"detail": "Account settings updated.", "profile": dict(row)}

@router.put("/trading-mode")
async def trading_mode(payload: dict, request: Request, user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    mode = str(payload.get("mode", "")).strip().lower()
    if mode not in {"demo", "live"}:
        raise DomainError(400, "Trading mode must be either demo or live.")
    if mode == "live":
        if not user.can_live_trade:
            raise DomainError(403, "Live-trading permission is required.")
        settings = getattr(request.app.state, "settings", None)
        if not settings or not settings.live_trading_enabled:
            raise DomainError(503, "Live trading is disabled in this environment.", code="python_live_mutation_disabled")
        profile_row = (await db.execute(text("""
            SELECT p.brokerage_user_id, p.last_token_status,
                   EXISTS(SELECT 1 FROM broker_secrets s WHERE s.user_id=:user AND s.secret_kind='api_key') AS has_api_key,
                   EXISTS(SELECT 1 FROM broker_secrets s WHERE s.user_id=:user AND s.secret_kind='jwt_token') AS has_jwt,
                   EXISTS(SELECT 1 FROM broker_secrets s WHERE s.user_id=:user AND s.secret_kind='feed_token') AS has_feed
            FROM user_profiles p WHERE p.user_id=:user
        """), {"user": user.id})).mappings().first()
        if (
            not profile_row
            or not profile_row["brokerage_user_id"]
            or not profile_row["has_api_key"]
            or not profile_row["has_jwt"]
            or not profile_row["has_feed"]
            or profile_row["last_token_status"] not in ("success", "refreshed")
        ):
            raise DomainError(400, "A connected and valid broker profile is required for live trading.")

    in_flight = bool(await db.scalar(text("""
        SELECT EXISTS(SELECT 1 FROM trades WHERE user_id=:user AND status='open')
            OR EXISTS(SELECT 1 FROM strategy_orders WHERE user_id=:user AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))
    """), {"user": user.id}))
    if in_flight:
        raise DomainError(400, "Trading mode cannot change while a position or broker order is still active. Close or reconcile it first.")

    await db.execute(text("""
        INSERT INTO user_profiles(user_id,brokerage_user_id,api_key,trading_mode)
        VALUES(:user,'','',:mode)
        ON CONFLICT(user_id) DO UPDATE SET trading_mode=:mode,updated_at=NOW()
    """), {"user": user.id, "mode": mode})
    await db.commit()
    return {"detail": f"Trading mode changed to {mode}.", "profile": await profile(user, db), "trading_mode": mode}


@router.post("/profile/request-otp")
async def profile_otp(request: Request, user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    email = await db.scalar(text("SELECT email FROM users WHERE id=:user"), {"user": user.id})
    if not email:
        raise DomainError(404, "Account email not found.")
    otp = f"{secrets.randbelow(1_000_000):06d}"
    await db.execute(text("UPDATE email_otps SET is_used=TRUE,invalidated_at=NOW() WHERE LOWER(email)=LOWER(:email) AND purpose='profile_update' AND is_used=FALSE"), {"email": email})
    await db.execute(text("INSERT INTO email_otps(id,email,otp_hash,purpose,expires_at) VALUES(:id,:email,:hash,'profile_update',NOW()+INTERVAL '10 minutes')"), {"id": uuid4(), "email": email, "hash": otp_digest(request.app.state.settings.otp_hash_key, email, "profile_update", otp)})
    await db.commit()
    if not request.app.state.settings.smtp_host and request.app.state.settings.app_env == "development":
        request.app.state.last_dev_otp = otp
    return {"detail": "OTP sent to the current account email.", "email": email}

