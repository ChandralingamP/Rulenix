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
    row = (await db.execute(text("SELECT u.id,u.username,u.email,COALESCE(p.mobile_number,'') mobile_number,COALESCE(p.brokerage_user_id,'') brokerage_user_id,COALESCE(p.trading_mode,'demo') trading_mode FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=:id"), {"id": user.id})).mappings().first()
    return dict(row) if row else {"username": user.username, "trading_mode": user.trading_mode}


@router.patch("/profile")
async def update_profile(request: Request, payload: dict, user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    otp = str(payload.get("otp", "")).strip()
    email = str(payload.get("email", "")).strip().lower()
    username = str(payload.get("new_username", "")).strip().upper()
    mobile = str(payload.get("mobile_number", "")).strip()
    client_id = str(payload.get("client_id", "")).strip().upper()
    if len(otp) != 6 or not otp.isdigit() or len(username) < 3 or len(username) > 64 or "@" not in email or len(mobile) != 10 or not mobile.isdigit() or not client_id:
        raise DomainError(400, "Invalid account profile details.")
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
async def trading_mode(payload: dict, user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    mode = str(payload.get("mode", "")).strip().lower()
    if mode not in {"demo", "live"}:
        raise DomainError(400, "Trading mode must be either demo or live.")
    if mode == "live":
        if not user.can_live_trade:
            raise DomainError(403, "Live-trading permission is required.")
        raise DomainError(503, "LIVE mode is disabled in the Python migration shadow.", code="python_live_mutation_disabled")
    await db.execute(text("INSERT INTO user_profiles(user_id,brokerage_user_id,api_key,trading_mode) VALUES(:user,'','',:mode) ON CONFLICT(user_id) DO UPDATE SET trading_mode='demo',updated_at=NOW()"), {"user": user.id, "mode": mode})
    await db.commit()
    return {"detail": "Trading mode updated.", "mode": mode, "trading_mode": mode}


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

