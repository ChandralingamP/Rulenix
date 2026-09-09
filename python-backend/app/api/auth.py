import secrets
from collections.abc import Mapping
from datetime import UTC, datetime, timedelta
from uuid import uuid4

from fastapi import APIRouter, Request, Response
from pydantic import BaseModel, ConfigDict
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import get_db
from ..errors import DomainError
from ..security import (
    digest,
    encrypt_broker_secret,
    hash_password,
    new_token,
    otp_digest,
    password_error,
    valid_email,
    valid_username,
    verify_password,
)

router = APIRouter(prefix="/auth", tags=["auth"])


class LoginRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    username: str
    password: str


class OtpRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    email: str
    username: str


class ResetOtpRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    email: str


class SignupRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    username: str
    user_id: str = ""
    api_key: str = ""
    mobile: str = ""
    email: str
    password: str
    confirm_password: str
    otp: str


class ResetVerify(BaseModel):
    model_config = ConfigDict(extra="forbid")

    email: str
    otp: str


class ResetPassword(ResetVerify):
    password: str
    confirm_password: str


def set_session_cookies(response: Response, token: str, csrf: str, settings, max_age: int) -> None:
    secure = settings.frontend_origin.startswith("https://")
    response.set_cookie("rulenix_session", token, httponly=True, secure=secure, samesite="lax", path="/", max_age=max_age)
    response.set_cookie("rulenix_csrf", csrf, httponly=False, secure=secure, samesite="lax", path="/", max_age=max_age)


async def issue_session(db: AsyncSession, request: Request, user: Mapping[str, object], response: Response) -> dict:
    settings = request.app.state.settings
    token, csrf = new_token(), new_token()
    now = datetime.now(UTC)
    idle = now + timedelta(minutes=settings.session_idle_minutes)
    absolute = now + timedelta(hours=settings.session_absolute_hours)
    await db.execute(text("""INSERT INTO user_sessions (id,user_id,token_hash,csrf_hash,created_at,last_seen_at,idle_expires_at,absolute_expires_at,user_agent,ip_address)
      VALUES (:id,:uid,:token,:csrf,NOW(),NOW(),:idle,:absolute,:ua,:ip)"""), {"id": uuid4(), "uid": user["id"], "token": digest(token), "csrf": digest(csrf), "idle": idle, "absolute": absolute, "ua": request.headers.get("user-agent", "")[:255], "ip": request.client.host if request.client else None})
    await db.commit()
    set_session_cookies(response, token, csrf, settings, int((absolute-now).total_seconds()))
    return {"username": user["username"], "permissions": {"administer_users": bool(user["can_administer"]), "live_trading": bool(user["can_live_trade"]), "backtesting": bool(user["can_backtest"]), "backtesting_on_trading_days": bool(user["can_backtest_on_trading_days"])}, "trading_mode": user["trading_mode"], "idle_expires_in_seconds": settings.session_idle_minutes*60, "absolute_expires_at": absolute.isoformat()}


@router.post("/login/")
async def login(body: LoginRequest, request: Request, response: Response, db: AsyncSession = __import__("fastapi").Depends(get_db)):
    row = (await db.execute(text("""SELECT u.id,u.username,u.password_hash,u.is_active,u.can_administer,u.can_live_trade,u.can_backtest,u.can_backtest_on_trading_days,COALESCE(p.trading_mode,'demo') trading_mode,u.failed_login_attempts,u.locked_until FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE LOWER(u.username)=LOWER(:username)"""), {"username": body.username})).mappings().first()
    if not row or not row["is_active"] or (row["locked_until"] and row["locked_until"] > datetime.now(UTC)) or not verify_password(row["password_hash"], body.password):
        raise DomainError(401, "Invalid username or password.")
    return await issue_session(db, request, dict(row), response)


@router.get("/access/")
async def access(request: Request, db: AsyncSession = __import__("fastapi").Depends(get_db)):
    from ..dependencies import current_user
    user = await current_user(request, db)
    return {"username": user.username, "permissions": {"administer_users": user.can_administer, "live_trading": user.can_live_trade, "backtesting": user.can_backtest, "backtesting_on_trading_days": user.can_backtest_on_trading_days}, "trading_mode": user.trading_mode}


@router.post("/logout/")
async def logout(request: Request, response: Response, db: AsyncSession = __import__("fastapi").Depends(get_db)):
    token = request.cookies.get("rulenix_session")
    if token:
        await db.execute(text("UPDATE user_sessions SET revoked_at=NOW() WHERE token_hash=:token"), {"token": digest(token)})
        await db.commit()
    response.delete_cookie("rulenix_session", path="/")
    response.delete_cookie("rulenix_csrf", path="/")
    return {"detail": "Logged out."}


@router.post("/request-otp/")
async def request_otp(body: OtpRequest, request: Request, db: AsyncSession = __import__("fastapi").Depends(get_db)):
    if not valid_email(body.email) or not valid_username(body.username):
        raise DomainError(400, "A valid email and username are required.")
    otp = f"{secrets.randbelow(1_000_000):06d}"
    settings = request.app.state.settings
    exists = await db.scalar(
        text(
            "SELECT NOT EXISTS(SELECT 1 FROM users "
            "WHERE LOWER(username)=LOWER(:username) OR LOWER(email)=LOWER(:email))"
        ),
        {"username": body.username, "email": body.email},
    )
    if exists:
        await db.execute(text("UPDATE email_otps SET is_used=TRUE,invalidated_at=NOW() WHERE LOWER(email)=LOWER(:email) AND purpose='signup' AND is_used=FALSE"), {"email": body.email})
        await db.execute(text("INSERT INTO email_otps (id,email,otp_hash,purpose,expires_at) VALUES (:id,:email,:hash,'signup',NOW()+INTERVAL '10 minutes')"), {"id": uuid4(), "email": body.email, "hash": otp_digest(settings.otp_hash_key, body.email, "signup", otp)})
        await db.commit()
        if not settings.smtp_host and settings.app_env == "development":
            request.app.state.last_dev_otp = otp
    return {"detail": "If the supplied details can be used, a verification code has been sent."}


@router.post("/signup/")
async def signup(body: SignupRequest, request: Request, response: Response, db: AsyncSession = __import__("fastapi").Depends(get_db)):
    if not valid_username(body.username) or not valid_email(body.email) or body.password != body.confirm_password:
        raise DomainError(400, "Invalid signup details.")
    if (err := password_error(body.username, body.email, body.password)):
        raise DomainError(400, err)
    if not body.user_id or not body.api_key:
        raise DomainError(400, "Invalid signup details.")
    otp = (await db.execute(text("SELECT id,otp_hash,expires_at FROM email_otps WHERE LOWER(email)=LOWER(:email) AND purpose='signup' AND is_used=FALSE AND invalidated_at IS NULL ORDER BY created_at DESC LIMIT 1 FOR UPDATE"), {"email": body.email})).mappings().first()
    if not otp or otp["expires_at"] < datetime.now(UTC) or otp_digest(request.app.state.settings.otp_hash_key, body.email, "signup", body.otp) != otp["otp_hash"]:
        raise DomainError(400, "Invalid or expired OTP.")
    try:
        user_id = uuid4()
        key_version, nonce, ciphertext = encrypt_broker_secret(body.api_key, str(user_id), "api_key", request.app.state.settings.credential_keys, request.app.state.settings.credential_primary_version)
        await db.commit()
        async with db.begin():
            await db.execute(text("INSERT INTO users(id,username,email,password_hash) VALUES(:id,:username,:email,:password_hash)"), {"id": user_id, "username": body.username.upper(), "email": body.email.lower(), "password_hash": hash_password(body.password)})
            await db.execute(text("INSERT INTO user_profiles(user_id,brokerage_user_id,api_key,mobile_number) VALUES(:id,:brokerage,'',:mobile)"), {"id": user_id, "brokerage": body.user_id, "mobile": body.mobile})
            await db.execute(text("INSERT INTO broker_secrets(user_id,secret_kind,key_version,nonce,ciphertext) VALUES(:id,'api_key',:version,:nonce,:ciphertext)"), {"id": user_id, "version": key_version, "nonce": nonce, "ciphertext": ciphertext})
            await db.execute(text("UPDATE email_otps SET is_used=TRUE,invalidated_at=NOW() WHERE id=:id"), {"id": otp["id"]})
        user = {"id": user_id, "username": body.username.upper(), "can_administer": False, "can_live_trade": False, "can_backtest": False, "can_backtest_on_trading_days": False, "trading_mode": "demo"}
        return await issue_session(db, request, user, response)
    except ValueError as exc:
        raise DomainError(503, "Credential encryption is not configured.", code="python_foundation_deferred") from exc


@router.post("/password/request-reset/")
async def request_reset(body: ResetOtpRequest, request: Request, db: AsyncSession = __import__("fastapi").Depends(get_db)):
    if not valid_email(body.email):
        raise DomainError(400, "Enter a valid email address.")
    exists = await db.scalar(
        text("SELECT EXISTS(SELECT 1 FROM users WHERE LOWER(email)=LOWER(:email))"),
        {"email": body.email},
    )
    if exists:
        otp = f"{secrets.randbelow(1_000_000):06d}"
        settings = request.app.state.settings
        await db.execute(text("UPDATE email_otps SET is_used=TRUE,invalidated_at=NOW() WHERE LOWER(email)=LOWER(:email) AND purpose='password_reset' AND is_used=FALSE"), {"email": body.email})
        await db.execute(text("INSERT INTO email_otps (id,email,otp_hash,purpose,expires_at) VALUES (:id,:email,:hash,'password_reset',NOW()+INTERVAL '10 minutes')"), {"id": uuid4(), "email": body.email, "hash": otp_digest(settings.otp_hash_key, body.email, "password_reset", otp)})
        await db.commit()
        if not settings.smtp_host and settings.app_env == "development":
            request.app.state.last_dev_otp = otp
    return {"detail": "If an account matches that email, a verification code has been sent."}


@router.post("/password/verify-otp/")
async def verify_reset(body: ResetVerify, request: Request, db: AsyncSession = __import__("fastapi").Depends(get_db)):
    row = (await db.execute(text("SELECT otp_hash,expires_at FROM email_otps WHERE LOWER(email)=LOWER(:email) AND purpose='password_reset' AND is_used=FALSE AND invalidated_at IS NULL ORDER BY created_at DESC LIMIT 1"), {"email": body.email})).mappings().first()
    if not row or row["expires_at"] < datetime.now(UTC) or otp_digest(request.app.state.settings.otp_hash_key, body.email, "password_reset", body.otp) != row["otp_hash"]:
        raise DomainError(400, "Invalid or expired OTP.")
    return {"detail": "OTP verified."}


@router.post("/password/reset/")
async def reset_password(body: ResetPassword, request: Request, db: AsyncSession = __import__("fastapi").Depends(get_db)):
    if body.password != body.confirm_password:
        raise DomainError(400, "Passwords do not match.")
    row = (await db.execute(text("SELECT id,username FROM users WHERE LOWER(email)=LOWER(:email)"), {"email": body.email})).mappings().first()
    if not row:
        raise DomainError(400, "Invalid or expired OTP.")
    if (err := password_error(row["username"], body.email, body.password)):
        raise DomainError(400, err)
    await db.execute(text("UPDATE users SET password_hash=:hash,password_changed_at=NOW(),failed_login_attempts=0,locked_until=NULL,updated_at=NOW() WHERE id=:id"), {"hash": hash_password(body.password), "id": row["id"]})
    await db.execute(text("UPDATE user_sessions SET revoked_at=NOW() WHERE user_id=:id AND revoked_at IS NULL"), {"id": row["id"]})
    await db.commit()
    return {"detail": "Password reset."}
