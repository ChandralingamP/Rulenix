use crate::{
    error::{AppError, AppResult},
    models::AdminUser,
    state::AppState,
};
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json,
    extract::{ConnectInfo, Extension, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    transport::smtp::authentication::Credentials,
};
use rand::{Rng, RngCore, rngs::OsRng as TokenRng};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OtpRequest {
    pub email: String,
    pub username: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetOtpRequest {
    pub email: String,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct SignupRequest {
    pub username: String,
    pub user_id: String,
    pub api_key: String,
    pub mobile: String,
    pub email: String,
    pub password: String,
    pub confirm_password: String,
    pub otp: String,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct ResetVerify {
    pub email: String,
    pub otp: String,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct ResetPassword {
    pub email: String,
    pub otp: String,
    pub password: String,
    pub confirm_password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminMutation {
    pub username: String,
    pub can_administer: Option<bool>,
    pub can_live_trade: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct AuthUser {
    pub id: Uuid,
    pub username: String,
    pub can_administer: bool,
    pub can_live_trade: bool,
    pub trading_mode: String,
    pub session_id: Uuid,
}

const SESSION_COOKIE: &str = "rulenix_session";
const CSRF_COOKIE: &str = "rulenix_csrf";
type SessionAuthRow = (Uuid, Uuid, String, bool, bool, String, Vec<u8>);
type LoginUserRow = (
    Uuid,
    String,
    String,
    bool,
    bool,
    bool,
    String,
    i32,
    Option<chrono::DateTime<Utc>>,
);

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    TokenRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn token_hash(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then(|| value.to_owned())
        })
}

fn cookie_headers(state: &AppState, session: &str, csrf: &str) -> AppResult<HeaderMap> {
    let secure = if state.config.app_env.eq_ignore_ascii_case("production") {
        "; Secure"
    } else {
        ""
    };
    let max_age = state.config.session_absolute_hours * 3600;
    let mut headers = HeaderMap::new();
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
        ))
        .map_err(|e| AppError::Internal(e.into()))?,
    );
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{CSRF_COOKIE}={csrf}; Path=/; SameSite=Lax; Max-Age={max_age}{secure}"
        ))
        .map_err(|e| AppError::Internal(e.into()))?,
    );
    Ok(headers)
}

fn clear_cookie_headers(state: &AppState) -> HeaderMap {
    let secure = if state.config.app_env.eq_ignore_ascii_case("production") {
        "; Secure"
    } else {
        ""
    };
    let mut headers = HeaderMap::new();
    for (name, http_only) in [(SESSION_COOKIE, "; HttpOnly"), (CSRF_COOKIE, "")] {
        headers.append(
            header::SET_COOKIE,
            HeaderValue::from_str(&format!(
                "{name}=; Path=/; SameSite=Lax; Max-Age=0{http_only}{secure}"
            ))
            .expect("static cookie header"),
        );
    }
    headers
}

pub async fn authenticated(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let unauthorized = || AppError::Unauthorized("Authentication required.".into()).into_response();
    let Some(raw_token) = cookie_value(request.headers(), SESSION_COOKIE) else {
        return unauthorized();
    };
    let row: Result<Option<SessionAuthRow>, sqlx::Error> = sqlx::query_as(
        "SELECT s.id,u.id,u.username,u.can_administer,u.can_live_trade,COALESCE(p.trading_mode,'demo'),s.csrf_hash FROM user_sessions s JOIN users u ON u.id=s.user_id LEFT JOIN user_profiles p ON p.user_id=u.id WHERE s.token_hash=$1 AND s.revoked_at IS NULL AND s.idle_expires_at>NOW() AND s.absolute_expires_at>NOW() AND u.is_active=TRUE AND s.created_at>=u.password_changed_at"
    ).bind(token_hash(&raw_token)).fetch_optional(&state.db).await;
    let Some((session_id, id, username, can_administer, can_live_trade, trading_mode, csrf_hash)) =
        (match row {
            Ok(value) => value,
            Err(error) => return AppError::Sqlx(error).into_response(),
        })
    else {
        return unauthorized();
    };

    if matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        let supplied = request
            .headers()
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok());
        if supplied.map(token_hash).as_deref() != Some(csrf_hash.as_slice()) {
            return AppError::Forbidden("Invalid CSRF token.".into()).into_response();
        }
    }
    let idle = state.config.session_idle_minutes;
    if let Err(error) = sqlx::query("UPDATE user_sessions SET last_seen_at=NOW(),idle_expires_at=LEAST(absolute_expires_at,NOW()+($2 * INTERVAL '1 minute')) WHERE id=$1")
        .bind(session_id).bind(idle).execute(&state.db).await {
        return AppError::Sqlx(error).into_response();
    }
    request.extensions_mut().insert(AuthUser {
        id,
        username,
        can_administer,
        can_live_trade,
        trading_mode,
        session_id,
    });
    next.run(request).await
}

pub fn require_admin_permission(user: &AuthUser) -> AppResult<()> {
    if user.can_administer {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "User administration permission required.".into(),
        ))
    }
}

fn normalize(value: &str) -> String {
    value.trim().to_uppercase()
}
fn otp_digest(key: &[u8], email: &str, purpose: &str, otp: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(email.trim().to_lowercase().as_bytes());
    mac.update(b"\0");
    mac.update(purpose.as_bytes());
    mac.update(b"\0");
    mac.update(otp.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

fn otp_matches(key: &[u8], email: &str, purpose: &str, otp: &str, stored: &str) -> bool {
    let Ok(expected) = STANDARD.decode(stored) else {
        return false;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(email.trim().to_lowercase().as_bytes());
    mac.update(b"\0");
    mac.update(purpose.as_bytes());
    mac.update(b"\0");
    mac.update(otp.as_bytes());
    mac.verify_slice(&expected).is_ok()
}

pub(crate) fn valid_email(value: &str) -> bool {
    let value = value.trim();
    value.len() <= 254
        && !value.contains(char::is_whitespace)
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && local.len() <= 64
                && !domain.contains('@')
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && domain
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
        })
}

pub(crate) fn valid_username(value: &str) -> bool {
    let value = value.trim();
    (3..=64).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn validate_password(password: &str, username: &str, email: &str) -> AppResult<()> {
    let strong = (12..=128).contains(&password.len())
        && password.chars().any(|c| c.is_ascii_lowercase())
        && password.chars().any(|c| c.is_ascii_uppercase())
        && password.chars().any(|c| c.is_ascii_digit())
        && password
            .chars()
            .any(|c| !c.is_ascii_alphanumeric() && !c.is_whitespace())
        && !password.chars().any(char::is_whitespace);
    let lower = password.to_lowercase();
    let local = email.split('@').next().unwrap_or("").to_lowercase();
    if !strong
        || lower.contains(&username.trim().to_lowercase())
        || (!local.is_empty() && lower.contains(&local))
    {
        return Err(AppError::BadRequest("Password must be 12 to 128 characters and include uppercase, lowercase, number, and symbol; it cannot contain your username or email name.".into()));
    }
    Ok(())
}

fn validate_otp(value: &str) -> AppResult<()> {
    if value.len() == 6 && value.chars().all(|c| c.is_ascii_digit()) {
        Ok(())
    } else {
        Err(AppError::BadRequest("OTP must be exactly 6 digits.".into()))
    }
}

fn password_hash(password: &str) -> AppResult<String> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|v| v.to_string())
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e.to_string())))
}

pub async fn bootstrap_admin(
    db: &PgPool,
    username: &str,
    email: &str,
    password: &str,
) -> AppResult<()> {
    if !valid_username(username) || !valid_email(email) {
        return Err(AppError::BadRequest(
            "A valid username and email are required.".into(),
        ));
    }
    validate_password(password, username, email)?;
    sqlx::query(
        "INSERT INTO users (id,username,email,password_hash,can_administer,can_live_trade) VALUES ($1,$2,$3,$4,TRUE,FALSE) \
         ON CONFLICT ((LOWER(username))) DO UPDATE SET email=EXCLUDED.email,password_hash=EXCLUDED.password_hash,password_changed_at=NOW(),can_administer=TRUE,updated_at=NOW()",
    )
    .bind(Uuid::new_v4())
    .bind(normalize(username))
    .bind(email.trim().to_lowercase())
    .bind(password_hash(password)?)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn ensure_admin(
    db: &PgPool,
    username: &str,
    email: &str,
    password: &str,
) -> AppResult<()> {
    if !valid_username(username) || !valid_email(email) {
        return Err(AppError::BadRequest(
            "Initial administrator configuration is incomplete.".into(),
        ));
    }
    let existing: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE LOWER(username)=LOWER($1))")
            .bind(username)
            .fetch_one(db)
            .await?;
    if existing {
        sqlx::query(
            "UPDATE users SET can_administer=TRUE,updated_at=NOW() WHERE LOWER(username)=LOWER($1)",
        )
        .bind(username)
        .execute(db)
        .await?;
        return Ok(());
    }
    validate_password(password, username, email).map_err(|_| {
        AppError::BadRequest(
            "Initial administrator password does not meet the password policy.".into(),
        )
    })?;
    sqlx::query(
        "INSERT INTO users (id,username,email,password_hash,can_administer,can_live_trade) VALUES ($1,$2,$3,$4,TRUE,FALSE) \
         ON CONFLICT ((LOWER(username))) DO UPDATE SET can_administer=TRUE,updated_at=NOW()",
    )
    .bind(Uuid::new_v4())
    .bind(normalize(username))
    .bind(email.trim().to_lowercase())
    .bind(password_hash(password)?)
    .execute(db)
    .await?;
    Ok(())
}

pub(crate) async fn create_otp(state: &AppState, email: &str, purpose: &str) -> AppResult<String> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(LOWER($1) || ':' || $2, 0))")
        .bind(email)
        .bind(purpose)
        .execute(&mut *tx)
        .await?;
    let latest: Option<chrono::DateTime<Utc>> = sqlx::query_scalar("SELECT created_at FROM email_otps WHERE LOWER(email)=LOWER($1) AND purpose=$2 ORDER BY created_at DESC LIMIT 1")
        .bind(email).bind(purpose).fetch_optional(&mut *tx).await?;
    if let Some(created) = latest {
        let available = created + Duration::seconds(state.config.otp_resend_cooldown_seconds);
        if available > Utc::now() {
            return Err(AppError::RateLimited {
                retry_after: (available - Utc::now()).num_seconds().max(1) as u64,
            });
        }
    }
    let otp = format!("{:06}", rand::thread_rng().gen_range(0..1_000_000));
    sqlx::query("UPDATE email_otps SET is_used=TRUE,invalidated_at=NOW() WHERE LOWER(email)=LOWER($1) AND purpose=$2 AND is_used=FALSE")
        .bind(email).bind(purpose).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO email_otps (id, email, otp_hash, purpose, expires_at) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(Uuid::new_v4())
    .bind(email.trim())
    .bind(otp_digest(&state.config.otp_hash_key, email, purpose, &otp))
    .bind(purpose)
    .bind(Utc::now() + Duration::minutes(state.config.otp_ttl_minutes))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    send_otp_email(state, email, &otp, purpose).await?;
    Ok(otp)
}

async fn send_otp_email(state: &AppState, email: &str, otp: &str, purpose: &str) -> AppResult<()> {
    let Some(host) = &state.config.smtp_host else {
        tracing::warn!(
            email,
            otp,
            purpose,
            "SMTP not configured; development OTP emitted"
        );
        return Ok(());
    };
    let message = Message::builder()
        .from(
            state
                .config
                .smtp_from
                .parse()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("invalid SMTP_FROM: {e}")))?,
        )
        .to(email
            .parse()
            .map_err(|e| AppError::BadRequest(format!("invalid email address: {e}")))?)
        .subject("Your Rulenix verification code")
        .body(format!(
            "Your Rulenix verification code is {otp}. It expires in {} minutes.",
            state.config.otp_ttl_minutes
        ))
        .map_err(|e| AppError::Internal(e.into()))?;
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
        .map_err(|e| AppError::Internal(e.into()))?
        .port(state.config.smtp_port);
    if let (Some(user), Some(password)) = (&state.config.smtp_username, &state.config.smtp_password)
    {
        builder = builder.credentials(Credentials::new(user.clone(), password.clone()));
    }
    builder
        .build()
        .send(message)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

pub(crate) async fn verify_otp(
    state: &AppState,
    email: &str,
    purpose: &str,
    otp: &str,
    consume: bool,
) -> AppResult<Uuid> {
    validate_otp(otp)?;
    let mut tx = state.db.begin().await?;
    let row: Option<(Uuid, String, i32, chrono::DateTime<Utc>)> = sqlx::query_as("SELECT id,otp_hash,attempt_count,expires_at FROM email_otps WHERE LOWER(email)=LOWER($1) AND purpose=$2 AND is_used=FALSE AND invalidated_at IS NULL ORDER BY created_at DESC LIMIT 1 FOR UPDATE")
        .bind(email).bind(purpose).fetch_optional(&mut *tx).await?;
    let Some((id, stored, attempts, expires_at)) = row else {
        return Err(AppError::BadRequest("Invalid or expired OTP.".into()));
    };
    if expires_at < Utc::now()
        || !otp_matches(&state.config.otp_hash_key, email, purpose, otp, &stored)
    {
        let next = attempts + 1;
        sqlx::query("UPDATE email_otps SET attempt_count=$2,invalidated_at=CASE WHEN $2 >= $3 OR expires_at < NOW() THEN NOW() ELSE invalidated_at END,is_used=CASE WHEN $2 >= $3 OR expires_at < NOW() THEN TRUE ELSE is_used END WHERE id=$1")
            .bind(id).bind(next).bind(state.config.otp_max_attempts).execute(&mut *tx).await?;
        tx.commit().await?;
        return Err(AppError::BadRequest("Invalid or expired OTP.".into()));
    }
    if consume {
        sqlx::query("UPDATE email_otps SET is_used=TRUE WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(id)
}

pub async fn request_otp(
    State(state): State<AppState>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<OtpRequest>,
) -> AppResult<Json<Value>> {
    let username = input.username.as_str();
    if !valid_email(&input.email) || !valid_username(username) {
        return Err(AppError::BadRequest(
            "A valid email and username are required.".into(),
        ));
    }
    crate::security::rate_limit(
        &state,
        Some(peer),
        &headers,
        "signup_otp",
        &[&input.email, username],
        state.config.otp_rate_limit,
    )
    .await?;
    let available: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM users WHERE LOWER(username)=LOWER($1) OR LOWER(email)=LOWER($2))")
        .bind(username).bind(&input.email).fetch_one(&state.db).await?;
    if available {
        let _ = create_otp(&state, &input.email, "signup").await?;
    }
    Ok(Json(
        json!({"detail":"If the supplied details can be used, a verification code has been sent."}),
    ))
}

pub async fn signup(
    State(state): State<AppState>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<SignupRequest>,
) -> AppResult<(StatusCode, Json<Value>)> {
    crate::security::rate_limit(
        &state,
        Some(peer),
        &headers,
        "signup",
        &[&input.email, &input.username],
        state.config.sensitive_rate_limit,
    )
    .await?;
    if !valid_username(&input.username)
        || !valid_email(&input.email)
        || input.user_id.trim().is_empty()
        || input.user_id.len() > 64
        || input.api_key.trim().is_empty()
        || input.api_key.len() > 128
        || input.mobile.len() != 10
        || !input.mobile.chars().all(|c| c.is_ascii_digit())
    {
        return Err(AppError::BadRequest("Invalid signup details.".into()));
    }
    if input.password != input.confirm_password {
        return Err(AppError::BadRequest("Passwords do not match.".into()));
    }
    validate_password(&input.password, &input.username, &input.email)?;
    verify_otp(&state, &input.email, "signup", &input.otp, true).await?;
    let id = Uuid::new_v4();
    let username = normalize(&input.username);
    let mut tx = state.db.begin().await?;
    let inserted =
        sqlx::query("INSERT INTO users (id,username,email,password_hash) VALUES ($1,$2,$3,$4)")
            .bind(id)
            .bind(&username)
            .bind(input.email.trim().to_lowercase())
            .bind(password_hash(&input.password)?)
            .execute(&mut *tx)
            .await;
    if let Err(error) = inserted {
        if let sqlx::Error::Database(db) = &error
            && db.is_unique_violation()
        {
            return Err(AppError::BadRequest(
                "Username or email is already registered.".into(),
            ));
        }
        return Err(error.into());
    }
    sqlx::query("INSERT INTO user_profiles (user_id,brokerage_user_id,api_key,mobile_number) VALUES ($1,$2,'',$3)")
        .bind(id).bind(input.user_id.trim().to_uppercase()).bind(input.mobile.trim()).execute(&mut *tx).await?;
    tx.commit().await?;
    if let Err(error) = state
        .credentials
        .put(id, &[("api_key", input.api_key.trim())])
        .await
    {
        let _ = sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(id)
            .execute(&state.db)
            .await;
        return Err(error);
    }
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"username":username,"permissions":{"administer_users":false,"live_trading":false},"trading_mode":"demo"}),
        ),
    ))
}

pub async fn login(
    State(state): State<AppState>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
    Json(input): Json<LoginRequest>,
) -> AppResult<(HeaderMap, Json<Value>)> {
    if !valid_username(&input.username) || input.password.len() > 128 {
        return Err(AppError::Unauthorized(
            "Invalid username or password.".into(),
        ));
    }
    crate::security::rate_limit(
        &state,
        Some(peer),
        &headers,
        "login",
        &[&input.username],
        state.config.login_rate_limit,
    )
    .await?;
    let user: Option<LoginUserRow> = sqlx::query_as("SELECT u.id,u.username,u.password_hash,u.is_active,u.can_administer,u.can_live_trade,COALESCE(p.trading_mode,'demo'),u.failed_login_attempts,u.locked_until FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE LOWER(u.username)=LOWER($1)")
        .bind(input.username.trim()).fetch_optional(&state.db).await?;
    if let Some((_, _, _, _, _, _, _, _, Some(until))) = &user
        && *until > Utc::now()
    {
        return Err(AppError::RateLimited {
            retry_after: (*until - Utc::now()).num_seconds().max(1) as u64,
        });
    }
    let verified = if let Some((_, _, hash, active, _, _, _, _, _)) = &user {
        *active
            && PasswordHash::new(hash).ok().is_some_and(|parsed| {
                Argon2::default()
                    .verify_password(input.password.as_bytes(), &parsed)
                    .is_ok()
            })
    } else {
        let dummy = password_hash("Dummy-password-93!")?;
        let parsed = PasswordHash::new(&dummy).expect("generated hash is valid");
        let _ = Argon2::default().verify_password(input.password.as_bytes(), &parsed);
        false
    };
    if !verified {
        if let Some((id, _, _, _, _, _, _, attempts, _)) = &user {
            let next = attempts.saturating_add(1);
            let locked_until = if next >= state.config.login_lockout_threshold {
                let exponent = (next - state.config.login_lockout_threshold).clamp(0, 20) as u32;
                let seconds = state
                    .config
                    .login_lockout_base_seconds
                    .saturating_mul(2_i64.saturating_pow(exponent))
                    .min(state.config.login_lockout_max_seconds);
                Some(Utc::now() + Duration::seconds(seconds))
            } else {
                None
            };
            sqlx::query("UPDATE users SET failed_login_attempts=$2,locked_until=$3,last_failed_login_at=NOW() WHERE id=$1")
                .bind(id).bind(next).bind(locked_until).execute(&state.db).await?;
            if let Some(until) = locked_until {
                return Err(AppError::RateLimited {
                    retry_after: (until - Utc::now()).num_seconds().max(1) as u64,
                });
            }
        }
        return Err(AppError::Unauthorized(
            "Invalid username or password.".into(),
        ));
    }
    let (id, username, _, _, can_administer, can_live_trade, trading_mode, _, _) =
        user.expect("verified user exists");
    sqlx::query("UPDATE users SET failed_login_attempts=0,locked_until=NULL,last_failed_login_at=NULL WHERE id=$1").bind(id).execute(&state.db).await?;
    let session_token = random_token();
    let csrf_token = random_token();
    let absolute = Utc::now() + Duration::hours(state.config.session_absolute_hours);
    let idle = std::cmp::min(
        absolute,
        Utc::now() + Duration::minutes(state.config.session_idle_minutes),
    );
    let ip = crate::security::client_ip(Some(peer), &headers, &state.config.trusted_proxies);
    sqlx::query("INSERT INTO user_sessions (id,user_id,token_hash,csrf_hash,idle_expires_at,absolute_expires_at,user_agent,ip_address) VALUES ($1,$2,$3,$4,$5,$6,$7,$8::inet)")
        .bind(Uuid::new_v4()).bind(id).bind(token_hash(&session_token)).bind(token_hash(&csrf_token))
        .bind(idle).bind(absolute).bind(headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).map(|v| &v[..v.len().min(512)]))
        .bind(ip.to_string())
        .execute(&state.db).await?;
    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: request_context.as_ref(),
            headers: Some(&headers),
            event_type: "login",
            actor_user_id: Some(id),
            target_user_id: Some(id),
            summary: "User logged in",
            metadata: json!({"username":&username,"trading_mode":&trading_mode}),
        },
    )
    .await
    {
        tracing::warn!(%error, "could not write login audit event");
    }
    Ok((
        cookie_headers(&state, &session_token, &csrf_token)?,
        Json(
            json!({"username":username,"permissions":{"administer_users":can_administer,"live_trading":can_live_trade},"trading_mode":trading_mode,"idle_expires_in_seconds":state.config.session_idle_minutes*60,"absolute_expires_at":absolute}),
        ),
    ))
}

pub async fn access_status(Extension(user): Extension<AuthUser>) -> AppResult<Json<Value>> {
    Ok(Json(json!({
        "username":user.username,
        "permissions":{"administer_users":user.can_administer,"live_trading":user.can_live_trade},
        "trading_mode":user.trading_mode,
    })))
}

pub async fn logout(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
) -> AppResult<(HeaderMap, Json<Value>)> {
    sqlx::query("UPDATE user_sessions SET revoked_at=NOW() WHERE id=$1")
        .bind(user.session_id)
        .execute(&state.db)
        .await?;
    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: request_context.as_ref(),
            headers: Some(&headers),
            event_type: "logout",
            actor_user_id: Some(user.id),
            target_user_id: Some(user.id),
            summary: "User logged out",
            metadata: json!({"username":user.username}),
        },
    )
    .await
    {
        tracing::warn!(%error, "could not write logout audit event");
    }
    Ok((
        clear_cookie_headers(&state),
        Json(json!({"detail":"Logged out."})),
    ))
}

pub fn start_session_cleanup(state: AppState) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            timer.tick().await;
            if let Err(error) = sqlx::query("DELETE FROM user_sessions WHERE revoked_at IS NOT NULL OR absolute_expires_at < NOW() OR idle_expires_at < NOW()")
                .execute(&state.db).await { tracing::warn!(%error, "session cleanup failed"); }
        }
    });
}

pub async fn request_reset(
    State(state): State<AppState>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<ResetOtpRequest>,
) -> AppResult<Json<Value>> {
    if !valid_email(&input.email) {
        return Err(AppError::BadRequest("Enter a valid email address.".into()));
    }
    crate::security::rate_limit(
        &state,
        Some(peer),
        &headers,
        "reset_otp",
        &[&input.email],
        state.config.otp_rate_limit,
    )
    .await?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE LOWER(email)=LOWER($1))")
            .bind(&input.email)
            .fetch_one(&state.db)
            .await?;
    if exists {
        let _ = create_otp(&state, &input.email, "password_reset").await?;
    }
    Ok(Json(
        json!({"detail":"If an account matches that email, a verification code has been sent."}),
    ))
}

pub async fn verify_reset(
    State(state): State<AppState>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<ResetVerify>,
) -> AppResult<Json<Value>> {
    crate::security::rate_limit(
        &state,
        Some(peer),
        &headers,
        "verify_reset_otp",
        &[&input.email],
        state.config.otp_rate_limit,
    )
    .await?;
    verify_otp(&state, &input.email, "password_reset", &input.otp, false).await?;
    Ok(Json(json!({"detail":"OTP verified."})))
}

pub async fn reset_password(
    State(state): State<AppState>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<ResetPassword>,
) -> AppResult<Json<Value>> {
    crate::security::rate_limit(
        &state,
        Some(peer),
        &headers,
        "reset_password",
        &[&input.email],
        state.config.sensitive_rate_limit,
    )
    .await?;
    if input.password != input.confirm_password {
        return Err(AppError::BadRequest("Passwords do not match.".into()));
    }
    verify_otp(&state, &input.email, "password_reset", &input.otp, true).await?;
    let username: String =
        sqlx::query_scalar("SELECT username FROM users WHERE LOWER(email)=LOWER($1)")
            .bind(&input.email)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::BadRequest("Invalid or expired OTP.".into()))?;
    validate_password(&input.password, &username, &input.email)?;
    let mut tx = state.db.begin().await?;
    let user_id: Option<Uuid> = sqlx::query_scalar(
        "UPDATE users SET password_hash=$1,password_changed_at=NOW(),updated_at=NOW() WHERE LOWER(email)=LOWER($2) RETURNING id",
    )
    .bind(password_hash(&input.password)?)
    .bind(&input.email)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(user_id) = user_id else {
        return Err(AppError::BadRequest("Unable to reset password.".into()));
    };
    sqlx::query(
        "UPDATE user_sessions SET revoked_at=NOW() WHERE user_id=$1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"detail":"Password updated successfully."})))
}

pub async fn list_users(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
) -> AppResult<Json<Vec<AdminUser>>> {
    require_admin_permission(&admin)?;
    let users = sqlx::query_as("SELECT u.id,u.username,u.email,u.can_administer,u.can_live_trade,COALESCE(p.trading_mode,'demo') AS trading_mode,u.is_active,u.created_at FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id ORDER BY u.username").fetch_all(&state.db).await?;
    Ok(Json(users))
}

pub async fn update_user(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
    Json(input): Json<AdminMutation>,
) -> AppResult<Json<Value>> {
    require_admin_permission(&admin)?;
    if admin.username.eq_ignore_ascii_case(&input.username) && input.can_administer.is_some() {
        return Err(AppError::BadRequest(
            "You cannot change your own administration permission.".into(),
        ));
    }
    if input.can_administer.is_none() && input.can_live_trade.is_none() {
        return Err(AppError::BadRequest(
            "At least one permission is required.".into(),
        ));
    }
    let mut tx = state.db.begin().await?;
    let changed_id: Option<Uuid> = sqlx::query_scalar("UPDATE users SET can_administer=COALESCE($1,can_administer),can_live_trade=COALESCE($2,can_live_trade),updated_at=NOW() WHERE LOWER(username)=LOWER($3) RETURNING id")
        .bind(input.can_administer).bind(input.can_live_trade).bind(&input.username).fetch_optional(&mut *tx).await?;
    let changed_id = changed_id.ok_or_else(|| AppError::NotFound("User not found.".into()))?;
    if input.can_live_trade == Some(false) {
        sqlx::query(
            "UPDATE user_profiles SET trading_mode='demo',updated_at=NOW() WHERE user_id=$1",
        )
        .bind(changed_id)
        .execute(&mut *tx)
        .await?;
    }
    let user: AdminUser = sqlx::query_as("SELECT u.id,u.username,u.email,u.can_administer,u.can_live_trade,COALESCE(p.trading_mode,'demo') AS trading_mode,u.is_active,u.created_at FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=$1")
        .bind(changed_id).fetch_one(&mut *tx).await?;
    sqlx::query(
        "UPDATE user_sessions SET revoked_at=NOW() WHERE user_id=$1 AND revoked_at IS NULL",
    )
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: request_context.as_ref(),
            headers: Some(&headers),
            event_type: "admin_user_permissions_changed",
            actor_user_id: Some(admin.id),
            target_user_id: Some(user.id),
            summary: "Administrator changed user permissions",
            metadata: json!({"username":&user.username,"can_administer":user.can_administer,"can_live_trade":user.can_live_trade,"trading_mode":&user.trading_mode}),
        },
    )
    .await
    {
        tracing::warn!(%error, "could not write admin permission audit event");
    }
    Ok(Json(json!({"user":user})))
}

pub async fn delete_user(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
    Json(input): Json<AdminMutation>,
) -> AppResult<StatusCode> {
    require_admin_permission(&admin)?;
    if admin.username.eq_ignore_ascii_case(&input.username) {
        return Err(AppError::BadRequest(
            "You cannot delete your own account.".into(),
        ));
    }
    let result = sqlx::query("DELETE FROM users WHERE LOWER(username)=LOWER($1)")
        .bind(&input.username)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("User not found.".into()));
    }
    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: request_context.as_ref(),
            headers: Some(&headers),
            event_type: "admin_user_deleted",
            actor_user_id: Some(admin.id),
            target_user_id: None,
            summary: "Administrator deleted a user",
            metadata: json!({"username":input.username}),
        },
    )
    .await
    {
        tracing::warn!(%error, "could not write admin delete audit event");
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod security_tests {
    use super::*;

    fn principal(can_administer: bool, can_live_trade: bool) -> AuthUser {
        AuthUser {
            id: Uuid::new_v4(),
            username: "USER".into(),
            can_administer,
            can_live_trade,
            trading_mode: "demo".into(),
            session_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn session_tokens_are_random_and_only_hashes_are_persistable() {
        let first = random_token();
        let second = random_token();
        assert_ne!(first, second);
        assert_ne!(token_hash(&first), first.as_bytes());
        assert_eq!(token_hash(&first).len(), 32);
    }

    #[test]
    fn client_cannot_claim_staff_identity() {
        let parsed = serde_json::from_value::<AdminMutation>(json!({
            "admin_username":"ADMIN", "username":"VICTIM", "can_live_trade":true
        }));
        assert!(parsed.is_err());
        assert!(matches!(
            require_admin_permission(&principal(false, false)),
            Err(AppError::Forbidden(_))
        ));
        assert!(require_admin_permission(&principal(true, false)).is_ok());
    }

    #[test]
    fn administration_and_live_trading_permissions_are_independent() {
        let live_only = principal(false, true);
        assert!(live_only.can_live_trade);
        assert!(matches!(
            require_admin_permission(&live_only),
            Err(AppError::Forbidden(_))
        ));

        let admin_only = principal(true, false);
        assert!(require_admin_permission(&admin_only).is_ok());
        assert!(!admin_only.can_live_trade);

        let live_update: AdminMutation = serde_json::from_value(json!({
            "username":"TRADER", "can_live_trade":true
        }))
        .unwrap();
        assert_eq!(live_update.can_live_trade, Some(true));
        assert_eq!(live_update.can_administer, None);
    }

    #[test]
    fn otp_hashes_are_keyed_and_constant_time_verifiable() {
        let first = otp_digest(
            b"first-secret-key",
            "person@example.com",
            "signup",
            "123456",
        );
        let second = otp_digest(
            b"second-secret-key",
            "person@example.com",
            "signup",
            "123456",
        );
        assert_ne!(first, second);
        assert!(otp_matches(
            b"first-secret-key",
            "PERSON@example.com",
            "signup",
            "123456",
            &first
        ));
        assert!(!otp_matches(
            b"first-secret-key",
            "person@example.com",
            "signup",
            "654321",
            &first
        ));
    }

    #[test]
    fn backend_rejects_weak_passwords_and_unexpected_fields() {
        assert!(validate_password("short", "trader", "person@example.com").is_err());
        assert!(validate_password("Very-Strong-93!", "trader", "person@example.com").is_ok());
        let login = serde_json::from_value::<LoginRequest>(json!({
            "username":"trader", "password":"Very-Strong-93!", "is_admin":true
        }));
        assert!(login.is_err());
    }
}
