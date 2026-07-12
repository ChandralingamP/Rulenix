use crate::{
    angel,
    auth::AuthUser,
    credentials::BrokerCredentials,
    error::{AppError, AppResult},
    models::BrokerageProfile,
    state::AppState,
};
use axum::{
    Json,
    extract::{ConnectInfo, Extension, State},
    http::HeaderMap,
};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use tokio::time::{MissedTickBehavior, interval};
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct ConnectRequest {
    pub mpin: String,
    pub totp: String,
}
#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct ProfileUpdate {
    pub api_key: String,
}

async fn profile_by_id(state: &AppState, user_id: Uuid) -> AppResult<BrokerageProfile> {
    sqlx::query_as("SELECT * FROM user_profiles WHERE user_id=$1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .map_err(Into::into)
}

fn details(p: &BrokerageProfile, credentials: &BrokerCredentials) -> Value {
    let connection_state = match (p.token_state.as_str(), p.last_token_status.as_str()) {
        ("verification_unavailable", _) => "unavailable",
        (_, "invalid") => "invalid",
        (_, "expired") => "expired",
        (_, "unavailable") => "unavailable",
        (_, "failed") => "failed",
        _ if credentials.jwt_token.is_empty() => "idle",
        _ => "connected",
    };
    json!({
        "client_id": p.brokerage_user_id,
        "api_key_configured": !credentials.api_key.is_empty(),
        "last_updated": p.updated_at,
        "connection_state": connection_state,
        "token_state": p.token_state,
        "connection_message": match connection_state {
            "connected" | "idle" => Value::Null,
            "unavailable" => json!("Angel One verification is temporarily unavailable. Rulenix will check the session again automatically."),
            _ => json!(p.last_token_message),
        },
        "last_connected_at": p.token_received_at,
        "last_verified_at": p.last_token_check_at,
    })
}

async fn mark_invalid(state: &AppState, user_id: Uuid, message: &str) -> AppResult<()> {
    state.credentials.clear_tokens(user_id).await?;
    sqlx::query("UPDATE user_profiles SET token_state='invalid',last_token_check_at=NOW(),last_token_status='invalid',last_token_message=$2 WHERE user_id=$1")
        .bind(user_id).bind(message).execute(&state.db).await?;
    crate::strategy::operational_alert(
        state,
        Some(user_id),
        "",
        "broker_session_invalid",
        "error",
        message,
    )
    .await;
    Ok(())
}

async fn mark_unavailable(state: &AppState, user_id: Uuid, message: &str) -> AppResult<()> {
    // A network/provider outage is not proof that the encrypted session is invalid.
    // Preserve a previous successful verification so read-only broker operations can recover.
    sqlx::query("UPDATE user_profiles SET token_state='verification_unavailable',last_token_check_at=NOW(),last_token_status=CASE WHEN last_token_status IN ('success','refreshed') THEN last_token_status ELSE 'unavailable' END,last_token_message=$2 WHERE user_id=$1")
        .bind(user_id).bind(message).execute(&state.db).await?;
    crate::strategy::operational_alert(
        state,
        Some(user_id),
        "",
        "broker_session_unavailable",
        "warning",
        message,
    )
    .await;
    Ok(())
}

async fn refresh_tokens(
    state: &AppState,
    p: &mut BrokerageProfile,
    credentials: &mut BrokerCredentials,
) -> AppResult<bool> {
    match angel::refresh_session(
        state,
        &credentials.api_key,
        &credentials.jwt_token,
        &credentials.refresh_token,
    )
    .await
    {
        angel::RefreshCheck::Refreshed(tokens) => {
            let refresh_token = tokens
                .refresh_token
                .clone()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| credentials.refresh_token.clone());
            state
                .credentials
                .put(
                    p.user_id,
                    &[
                        ("jwt_token", &tokens.jwt_token),
                        ("refresh_token", &refresh_token),
                        ("feed_token", &tokens.feed_token),
                    ],
                )
                .await?;
            sqlx::query("UPDATE user_profiles SET token_state='refreshed',last_token_check_at=NOW(),last_token_status='refreshed',last_token_message='',updated_at=NOW() WHERE user_id=$1")
                .bind(p.user_id).execute(&state.db).await?;
            credentials.jwt_token = tokens.jwt_token.clone();
            credentials.refresh_token = refresh_token;
            credentials.feed_token = tokens.feed_token.clone();
            tracing::info!(user_id=%p.user_id, "Angel One session tokens refreshed");
            Ok(true)
        }
        angel::RefreshCheck::Invalid(message) => {
            mark_invalid(state, p.user_id, &message).await?;
            Ok(false)
        }
        angel::RefreshCheck::Unavailable(message) => {
            mark_unavailable(state, p.user_id, &message).await?;
            Ok(false)
        }
    }
}

async fn maintain_user_session(state: &AppState, user_id: Uuid) -> AppResult<()> {
    {
        let mut active = state.session_checks.lock().await;
        if !active.insert(user_id) {
            return Ok(());
        }
    }
    let result = maintain_user_session_inner(state, user_id).await;
    state.session_checks.lock().await.remove(&user_id);
    result
}

async fn maintain_user_session_inner(state: &AppState, user_id: Uuid) -> AppResult<()> {
    let mut p = profile_by_id(state, user_id).await?;
    let mut credentials = state.credentials.load(user_id).await?;
    if credentials.jwt_token.is_empty() {
        return Ok(());
    }
    let near_expiry = angel::jwt_expires_within(&credentials.jwt_token, 600);
    let recently_verified = p.last_token_status == "success"
        && p.last_token_check_at
            .is_some_and(|checked| checked > Utc::now() - Duration::seconds(30));
    if recently_verified && !near_expiry {
        return Ok(());
    }

    let mut refreshed = false;
    if near_expiry {
        refreshed = refresh_tokens(state, &mut p, &mut credentials).await?;
        if !refreshed {
            return Ok(());
        }
    }

    let mut check = angel::verify_session(
        state,
        &credentials.api_key,
        &credentials.jwt_token,
        &credentials.refresh_token,
    )
    .await;
    if matches!(check, angel::SessionCheck::Expired(_)) && !refreshed {
        refreshed = refresh_tokens(state, &mut p, &mut credentials).await?;
        if !refreshed {
            return Ok(());
        }
        check = angel::verify_session(
            state,
            &credentials.api_key,
            &credentials.jwt_token,
            &credentials.refresh_token,
        )
        .await;
    }
    match check {
        angel::SessionCheck::Valid => {
            sqlx::query("UPDATE user_profiles SET token_state=$2,last_token_check_at=NOW(),last_token_status='success',last_token_message='' WHERE user_id=$1")
                .bind(user_id).bind(if refreshed { "refreshed" } else { "connected" }).execute(&state.db).await?;
        }
        angel::SessionCheck::Expired(_) => {
            mark_invalid(
                state,
                user_id,
                "Angel One token is invalid or revoked. Please reconnect.",
            )
            .await?;
        }
        angel::SessionCheck::Unavailable(message) => {
            mark_unavailable(state, user_id, &message).await?;
        }
    }
    Ok(())
}

pub fn start_session_maintenance(state: AppState) {
    tokio::spawn(async move {
        let mut timer = interval(std::time::Duration::from_secs(60));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            let users: Vec<Uuid> = sqlx::query_scalar(
                "SELECT p.user_id FROM user_profiles p WHERE EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='jwt_token') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='refresh_token')",
            )
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
            let mut tasks = tokio::task::JoinSet::new();
            for user_id in users {
                let state = state.clone();
                tasks.spawn(async move {
                    if let Err(error) = maintain_user_session(&state, user_id).await {
                        tracing::warn!(%user_id, %error, "broker session maintenance failed");
                    }
                });
            }
            while tasks.join_next().await.is_some() {}
        }
    });
}

pub async fn status(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> AppResult<Json<Value>> {
    let mut p = profile_by_id(&state, user.id).await?;
    maintain_user_session(&state, p.user_id).await?;
    p = profile_by_id(&state, user.id).await?;
    let credentials = state.credentials.load(user.id).await?;
    Ok(Json(details(&p, &credentials)))
}

pub async fn connect(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<ConnectRequest>,
) -> AppResult<Json<Value>> {
    let identity = user.id.to_string();
    crate::security::rate_limit(
        &state,
        Some(peer),
        &headers,
        "broker_connect",
        &[&identity],
        state.config.sensitive_rate_limit,
    )
    .await?;
    if !(4..=16).contains(&input.mpin.len())
        || !input.mpin.chars().all(|c| c.is_ascii_digit())
        || !(6..=8).contains(&input.totp.len())
        || !input.totp.chars().all(|c| c.is_ascii_digit())
    {
        return Err(AppError::BadRequest(
            "A valid MPIN and numeric TOTP are required.".into(),
        ));
    }
    let p = profile_by_id(&state, user.id).await?;
    let credentials = state.credentials.load(user.id).await?;
    if credentials.api_key.is_empty() {
        return Err(AppError::BadRequest(
            "Add an Angel One API key before connecting.".into(),
        ));
    }
    let session = angel::create_session(
        &state,
        &p.brokerage_user_id,
        &credentials.api_key,
        &input.mpin,
        &input.totp,
    )
    .await?;
    state
        .credentials
        .put(
            p.user_id,
            &[
                ("jwt_token", &session.jwt_token),
                ("refresh_token", &session.refresh_token),
                ("feed_token", &session.feed_token),
            ],
        )
        .await?;
    sqlx::query("UPDATE user_profiles SET token_state='connected',token_received_at=NOW(),last_token_check_at=NOW(),last_token_status='success',last_token_message='',updated_at=NOW() WHERE user_id=$1")
        .bind(p.user_id).execute(&state.db).await?;
    let refreshed = profile_by_id(&state, user.id).await?;
    crate::logs::append(&user.username, "BROKER SESSION connected to Angel One").await;
    crate::strategy::refresh_after_broker_connect(state.clone());
    Ok(Json(
        json!({"message":"Brokerage session established successfully.","last_connected_at":refreshed.token_received_at,"details":details(&refreshed, &state.credentials.load(user.id).await?)}),
    ))
}

pub async fn update_profile(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Json(input): Json<ProfileUpdate>,
) -> AppResult<Json<Value>> {
    let key = input.api_key.trim();
    if key.is_empty() || key.len() > 128 {
        return Err(AppError::BadRequest(
            "API key must be between 1 and 128 characters.".into(),
        ));
    }
    let p = profile_by_id(&state, user.id).await?;
    state
        .credentials
        .put(
            p.user_id,
            &[
                ("api_key", key),
                ("jwt_token", ""),
                ("refresh_token", ""),
                ("feed_token", ""),
            ],
        )
        .await?;
    sqlx::query("UPDATE user_profiles SET token_state='',last_token_status='',last_token_message='',updated_at=NOW() WHERE user_id=$1")
        .bind(p.user_id).execute(&state.db).await?;
    let refreshed = profile_by_id(&state, user.id).await?;
    Ok(Json(
        json!({"message":"Profile updated successfully.","details":details(&refreshed, &state.credentials.load(user.id).await?)}),
    ))
}
