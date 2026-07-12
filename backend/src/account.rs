use crate::{
    angel,
    auth::{AuthUser, create_otp, valid_email, valid_username, verify_otp},
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    Json,
    extract::{ConnectInfo, Extension, State},
    http::HeaderMap,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::FromRow;
use std::net::SocketAddr;
use uuid::Uuid;

const INITIAL_DEMO_BALANCE: f64 = 200_000.0;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileOtpRequest {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileChange {
    pub otp: String,
    pub new_username: String,
    pub email: String,
    pub mobile_number: String,
    pub client_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopUpRequest {
    pub amount: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BalanceAction {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TradingModeChange {
    pub mode: String,
    pub confirm_live: Option<bool>,
}

#[derive(FromRow)]
struct AccountRecord {
    id: Uuid,
    username: String,
    email: String,
    can_administer: bool,
    can_live_trade: bool,
    trading_mode: String,
    brokerage_user_id: Option<String>,
    mobile_number: Option<String>,
    last_token_status: Option<String>,
    demo_balance: Option<f64>,
}

async fn account(state: &AppState, user_id: Uuid) -> AppResult<AccountRecord> {
    sqlx::query_as(
        "SELECT u.id,u.username,u.email,u.can_administer,u.can_live_trade,COALESCE(p.trading_mode,'demo') AS trading_mode,p.brokerage_user_id,p.mobile_number,p.last_token_status,p.demo_balance::float8 AS demo_balance \
         FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=$1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("User not found.".into()))
}

fn profile_json(record: &AccountRecord) -> Value {
    json!({
        "username":record.username,
        "email":record.email,
        "mobile_number":record.mobile_number.as_deref().unwrap_or(""),
        "client_id":record.brokerage_user_id.as_deref().unwrap_or(""),
        "trading_mode":record.trading_mode,
        "permissions":{"administer_users":record.can_administer,"live_trading":record.can_live_trade},
    })
}

pub async fn get_profile(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> AppResult<Json<Value>> {
    Ok(Json(profile_json(&account(&state, user.id).await?)))
}

pub async fn request_profile_otp(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(_input): Json<ProfileOtpRequest>,
) -> AppResult<Json<Value>> {
    let identity = user.id.to_string();
    crate::security::rate_limit(
        &state,
        Some(peer),
        &headers,
        "profile_otp",
        &[&identity],
        state.config.otp_rate_limit,
    )
    .await?;
    let record = account(&state, user.id).await?;
    let _ = create_otp(&state, &record.email, "profile_update").await?;
    let response = json!({
        "detail":"OTP sent to the current account email.",
        "email":record.email,
    });
    Ok(Json(response))
}

pub async fn update_profile(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
    Json(input): Json<ProfileChange>,
) -> AppResult<Json<Value>> {
    let identity = user.id.to_string();
    crate::security::rate_limit(
        &state,
        Some(peer),
        &headers,
        "profile_update",
        &[&identity],
        state.config.sensitive_rate_limit,
    )
    .await?;
    let current = account(&state, user.id).await?;
    let new_username = input.new_username.trim().to_uppercase();
    let email = input.email.trim().to_lowercase();
    let mobile = input.mobile_number.trim();
    let client_id = input.client_id.trim().to_uppercase();
    if !valid_username(&new_username) {
        return Err(AppError::BadRequest(
            "Username must be 3 to 64 characters and use only letters, numbers, dot, dash, or underscore.".into(),
        ));
    }
    if !valid_email(&email) {
        return Err(AppError::BadRequest("Enter a valid email address.".into()));
    }
    if mobile.len() != 10 || !mobile.chars().all(|value| value.is_ascii_digit()) {
        return Err(AppError::BadRequest(
            "Enter a valid 10-digit mobile number.".into(),
        ));
    }
    if client_id.is_empty() || client_id.len() > 64 {
        return Err(AppError::BadRequest("Client ID is required.".into()));
    }
    verify_otp(
        &state,
        &current.email,
        "profile_update",
        input.otp.trim(),
        true,
    )
    .await?;

    let client_changed = current
        .brokerage_user_id
        .as_deref()
        .unwrap_or("")
        .ne(client_id.as_str());
    let mut transaction = state.db.begin().await?;
    let user_update =
        sqlx::query("UPDATE users SET username=$1,email=$2,updated_at=NOW() WHERE id=$3")
            .bind(&new_username)
            .bind(&email)
            .bind(current.id)
            .execute(&mut *transaction)
            .await;
    if let Err(error) = user_update {
        if let sqlx::Error::Database(database) = &error
            && database.is_unique_violation()
        {
            return Err(AppError::BadRequest(
                "Username or email is already in use.".into(),
            ));
        }
        return Err(error.into());
    }
    sqlx::query(
        "INSERT INTO user_profiles (user_id,brokerage_user_id,api_key,mobile_number) VALUES ($1,$2,'',$3) \
         ON CONFLICT (user_id) DO UPDATE SET brokerage_user_id=EXCLUDED.brokerage_user_id,mobile_number=EXCLUDED.mobile_number, \
         token_state=CASE WHEN $4 THEN '' ELSE user_profiles.token_state END,updated_at=NOW()",
    )
    .bind(current.id)
    .bind(&client_id)
    .bind(mobile)
    .bind(client_changed)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    if client_changed {
        state.credentials.clear_tokens(user.id).await?;
    }
    let updated = account(&state, user.id).await?;
    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: request_context.as_ref(),
            headers: Some(&headers),
            event_type: "account_profile_changed",
            actor_user_id: Some(user.id),
            target_user_id: Some(user.id),
            summary: "User changed account profile",
            metadata: json!({"username":&updated.username,"email":&updated.email,"client_id_changed":client_changed}),
        },
    )
    .await
    {
        tracing::warn!(%error, "could not write profile audit event");
    }
    Ok(Json(
        json!({"detail":"Account settings updated.","profile":profile_json(&updated)}),
    ))
}

fn validate_mode_change(
    mode: &str,
    can_live_trade: bool,
    broker_valid: bool,
    confirmed: bool,
) -> AppResult<()> {
    match mode {
        "demo" => Ok(()),
        "live" if !can_live_trade => Err(AppError::Forbidden(
            "Live-trading permission is required.".into(),
        )),
        "live" if !confirmed => Err(AppError::BadRequest(
            "Explicit live-trading confirmation is required.".into(),
        )),
        "live" if !broker_valid => Err(AppError::BadRequest(
            "A connected and valid broker profile is required for live trading.".into(),
        )),
        "live" => Ok(()),
        _ => Err(AppError::BadRequest(
            "Trading mode must be either demo or live.".into(),
        )),
    }
}

pub async fn update_trading_mode(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
    Json(input): Json<TradingModeChange>,
) -> AppResult<Json<Value>> {
    let record = account(&state, user.id).await?;
    let credentials = state.credentials.load(user.id).await?;
    let mode = input.mode.trim().to_lowercase();
    if mode == "live" && state.config.force_demo_trading {
        return Err(AppError::Forbidden(
            "Live trading is disabled in this environment.".into(),
        ));
    }
    let broker_valid = record
        .brokerage_user_id
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        && !credentials.api_key.is_empty()
        && !credentials.jwt_token.is_empty()
        && !credentials.feed_token.is_empty()
        && matches!(
            record.last_token_status.as_deref(),
            Some("success" | "refreshed")
        );
    validate_mode_change(
        &mode,
        record.can_live_trade,
        broker_valid,
        input.confirm_live.unwrap_or(false),
    )?;
    sqlx::query("UPDATE user_profiles SET trading_mode=$1,updated_at=NOW() WHERE user_id=$2")
        .bind(&mode)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    let updated = account(&state, user.id).await?;
    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: request_context.as_ref(),
            headers: Some(&headers),
            event_type: "trading_mode_changed",
            actor_user_id: Some(user.id),
            target_user_id: Some(user.id),
            summary: "User changed trading mode",
            metadata: json!({"mode":&mode,"confirmed_live":input.confirm_live.unwrap_or(false)}),
        },
    )
    .await
    {
        tracing::warn!(%error, "could not write trading mode audit event");
    }
    Ok(Json(json!({
        "detail":format!("Trading mode changed to {mode}."),
        "trading_mode":mode,
        "profile":profile_json(&updated),
    })))
}

pub async fn get_balance(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> AppResult<Json<Value>> {
    let record = account(&state, user.id).await?;
    if record.trading_mode == "demo" {
        return Ok(Json(json!({
            "mode":"demo",
            "balance":record.demo_balance.unwrap_or(INITIAL_DEMO_BALANCE),
            "currency":"INR",
        })));
    }
    if !record.can_live_trade {
        return Err(AppError::Forbidden(
            "Live-trading permission has been revoked.".into(),
        ));
    }
    let credentials = state.credentials.load(user.id).await?;
    if credentials.api_key.is_empty() {
        return Err(AppError::BadRequest(
            "Add an Angel One API key before loading live balance.".into(),
        ));
    }
    if credentials.jwt_token.is_empty() {
        return Err(AppError::BadRequest(
            "Connect your Angel One session before loading live balance.".into(),
        ));
    }
    let margin = angel::get_margin(&state, &credentials.api_key, &credentials.jwt_token).await?;
    Ok(Json(json!({
        "mode":"live",
        "balance":margin["available_balance"],
        "currency":"INR",
        "provider":"Angel One",
    })))
}

pub async fn top_up_demo(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Json(input): Json<TopUpRequest>,
) -> AppResult<Json<Value>> {
    if !input.amount.is_finite() || input.amount <= 0.0 || input.amount > 10_000_000.0 {
        return Err(AppError::BadRequest(
            "Top-up must be between ₹1 and ₹1,00,00,000.".into(),
        ));
    }
    let record = account(&state, user.id).await?;
    if record.trading_mode != "demo" {
        return Err(AppError::Forbidden(
            "Live balances can only be changed through Angel One.".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO user_profiles (user_id,brokerage_user_id,api_key,mobile_number,demo_balance) VALUES ($1,'','','',200000.00+$2) \
         ON CONFLICT (user_id) DO UPDATE SET demo_balance=user_profiles.demo_balance+$2,updated_at=NOW()",
    )
    .bind(record.id)
    .bind(input.amount)
    .execute(&state.db)
    .await?;
    get_balance(State(state), Extension(user)).await
}

pub async fn reset_demo(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Json(_input): Json<BalanceAction>,
) -> AppResult<Json<Value>> {
    let record = account(&state, user.id).await?;
    if record.trading_mode != "demo" {
        return Err(AppError::Forbidden(
            "Live balances can only be changed through Angel One.".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO user_profiles (user_id,brokerage_user_id,api_key,mobile_number,demo_balance) VALUES ($1,'','','',$2) \
         ON CONFLICT (user_id) DO UPDATE SET demo_balance=$2,updated_at=NOW()",
    )
    .bind(record.id)
    .bind(INITIAL_DEMO_BALANCE)
    .execute(&state.db)
    .await?;
    get_balance(State(state), Extension(user)).await
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn account_mutations_reject_client_asserted_identity() {
        let result = serde_json::from_value::<TopUpRequest>(
            json!({"username":"ANOTHER_USER","amount":100.0}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn live_mode_requires_permission_confirmation_and_valid_broker() {
        assert!(matches!(
            validate_mode_change("live", false, true, true),
            Err(AppError::Forbidden(_))
        ));
        assert!(matches!(
            validate_mode_change("live", true, true, false),
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            validate_mode_change("live", true, false, true),
            Err(AppError::BadRequest(_))
        ));
        assert!(validate_mode_change("live", true, true, true).is_ok());
        assert!(validate_mode_change("demo", false, false, false).is_ok());
        assert!(validate_mode_change("paper", true, true, true).is_err());
    }
}
