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
use sqlx::{Postgres, Transaction};
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

struct SessionSnapshot {
    profile: BrokerageProfile,
    credentials: BrokerCredentials,
}

async fn lock_user(transaction: &mut Transaction<'_, Postgres>, user_id: Uuid) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,0))")
        .bind(user_id)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn session_snapshot(state: &AppState, user_id: Uuid) -> AppResult<SessionSnapshot> {
    let mut transaction = state.db.begin().await?;
    lock_user(&mut transaction, user_id).await?;
    let profile: BrokerageProfile =
        sqlx::query_as("SELECT * FROM user_profiles WHERE user_id=$1 FOR UPDATE")
            .bind(user_id)
            .fetch_one(&mut *transaction)
            .await?;
    let credentials = state
        .credentials
        .load_in_transaction(&mut transaction, user_id)
        .await?;
    transaction.commit().await?;
    Ok(SessionSnapshot {
        profile,
        credentials,
    })
}

async fn begin_session_attempt(
    state: &AppState,
    user_id: Uuid,
    expected_revision: i64,
    token_state: &str,
    message: &str,
) -> AppResult<Option<i64>> {
    let mut transaction = state.db.begin().await?;
    lock_user(&mut transaction, user_id).await?;
    let revision: Option<i64> = sqlx::query_scalar("UPDATE user_profiles SET broker_credential_revision=broker_credential_revision+1,token_state=$3,last_token_check_at=NOW(),last_token_status='',last_token_message=$4,updated_at=NOW() WHERE user_id=$1 AND broker_credential_revision=$2 RETURNING broker_credential_revision")
        .bind(user_id)
        .bind(expected_revision)
        .bind(token_state)
        .bind(message)
        .fetch_optional(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(revision)
}

async fn finalize_session_tokens(
    state: &AppState,
    user_id: Uuid,
    expected_revision: i64,
    values: &[(&str, &str)],
    token_state: &str,
    token_status: &str,
    received_now: bool,
) -> AppResult<bool> {
    let mut transaction = state.db.begin().await?;
    lock_user(&mut transaction, user_id).await?;
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT broker_credential_revision FROM user_profiles WHERE user_id=$1 FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if current != Some(expected_revision) {
        transaction.rollback().await?;
        return Ok(false);
    }
    state
        .credentials
        .put_in_transaction(&mut transaction, user_id, values)
        .await?;
    let updated = sqlx::query("UPDATE user_profiles SET token_state=$3,token_received_at=CASE WHEN $5 THEN NOW() ELSE token_received_at END,last_token_check_at=NOW(),last_token_status=$4,last_token_message='',updated_at=NOW() WHERE user_id=$1 AND broker_credential_revision=$2")
        .bind(user_id)
        .bind(expected_revision)
        .bind(token_state)
        .bind(token_status)
        .bind(received_now)
        .execute(&mut *transaction)
        .await?;
    if updated.rows_affected() == 0 {
        transaction.rollback().await?;
        return Ok(false);
    }
    transaction.commit().await?;
    Ok(true)
}

async fn mark_attempt_failed(
    state: &AppState,
    user_id: Uuid,
    expected_revision: i64,
    message: &str,
) -> AppResult<()> {
    let mut transaction = state.db.begin().await?;
    lock_user(&mut transaction, user_id).await?;
    sqlx::query("UPDATE user_profiles SET token_state='failed',last_token_check_at=NOW(),last_token_status='failed',last_token_message=$3,updated_at=NOW() WHERE user_id=$1 AND broker_credential_revision=$2")
        .bind(user_id)
        .bind(expected_revision)
        .bind(message)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn invalidate_if_revision(
    state: &AppState,
    user_id: Uuid,
    expected_revision: Option<i64>,
    message: &str,
) -> AppResult<bool> {
    let mut transaction = state.db.begin().await?;
    lock_user(&mut transaction, user_id).await?;
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT broker_credential_revision FROM user_profiles WHERE user_id=$1 FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(current) = current else {
        transaction.rollback().await?;
        return Ok(false);
    };
    if expected_revision.is_some_and(|expected| expected != current) {
        transaction.rollback().await?;
        return Ok(false);
    }
    state
        .credentials
        .clear_tokens_in_transaction(&mut transaction, user_id)
        .await?;
    sqlx::query("UPDATE user_profiles SET broker_credential_revision=broker_credential_revision+1,token_state='invalid',last_token_check_at=NOW(),last_token_status='invalid',last_token_message=$2,updated_at=NOW() WHERE user_id=$1")
        .bind(user_id)
        .bind(message)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(true)
}

pub(crate) async fn mark_invalid(state: &AppState, user_id: Uuid, message: &str) -> AppResult<()> {
    if invalidate_if_revision(state, user_id, None, message).await? {
        crate::strategy::operational_alert(
            state,
            Some(user_id),
            "",
            "broker_session_invalid",
            "error",
            message,
        )
        .await;
    }
    Ok(())
}

async fn mark_invalid_at_revision(
    state: &AppState,
    user_id: Uuid,
    expected_revision: i64,
    message: &str,
) -> AppResult<()> {
    if invalidate_if_revision(state, user_id, Some(expected_revision), message).await? {
        crate::strategy::operational_alert(
            state,
            Some(user_id),
            "",
            "broker_session_invalid",
            "error",
            message,
        )
        .await;
    }
    Ok(())
}

async fn mark_unavailable_at_revision(
    state: &AppState,
    user_id: Uuid,
    expected_revision: i64,
    message: &str,
) -> AppResult<()> {
    let mut transaction = state.db.begin().await?;
    lock_user(&mut transaction, user_id).await?;
    // A provider outage is not proof that the encrypted credentials are invalid.
    let updated = sqlx::query("UPDATE user_profiles SET token_state='verification_unavailable',last_token_check_at=NOW(),last_token_status=CASE WHEN last_token_status IN ('success','refreshed') THEN last_token_status ELSE 'unavailable' END,last_token_message=$3,updated_at=NOW() WHERE user_id=$1 AND broker_credential_revision=$2")
        .bind(user_id)
        .bind(expected_revision)
        .bind(message)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    if updated.rows_affected() > 0 {
        crate::strategy::operational_alert(
            state,
            Some(user_id),
            "",
            "broker_session_unavailable",
            "warning",
            message,
        )
        .await;
    }
    Ok(())
}

async fn mark_valid_at_revision(
    state: &AppState,
    user_id: Uuid,
    expected_revision: i64,
    token_state: &str,
) -> AppResult<()> {
    let mut transaction = state.db.begin().await?;
    lock_user(&mut transaction, user_id).await?;
    sqlx::query("UPDATE user_profiles SET token_state=$3,last_token_check_at=NOW(),last_token_status='success',last_token_message='',updated_at=NOW() WHERE user_id=$1 AND broker_credential_revision=$2")
        .bind(user_id)
        .bind(expected_revision)
        .bind(token_state)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn refresh_tokens(state: &AppState, snapshot: &SessionSnapshot) -> AppResult<bool> {
    let Some(attempt_revision) = begin_session_attempt(
        state,
        snapshot.profile.user_id,
        snapshot.profile.broker_credential_revision,
        "refreshing",
        "Broker session refresh is in progress.",
    )
    .await?
    else {
        return Ok(false);
    };
    match angel::refresh_session(
        state,
        &snapshot.credentials.api_key,
        &snapshot.credentials.jwt_token,
        &snapshot.credentials.refresh_token,
    )
    .await
    {
        angel::RefreshCheck::Refreshed(tokens) => {
            let refresh_token = tokens
                .refresh_token
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or(&snapshot.credentials.refresh_token);
            let finalized = finalize_session_tokens(
                state,
                snapshot.profile.user_id,
                attempt_revision,
                &[
                    ("jwt_token", &tokens.jwt_token),
                    ("refresh_token", refresh_token),
                    ("feed_token", &tokens.feed_token),
                ],
                "refreshed",
                "refreshed",
                false,
            )
            .await?;
            if finalized {
                tracing::info!(user_id=%snapshot.profile.user_id, "Angel One session tokens refreshed");
            }
            Ok(finalized)
        }
        angel::RefreshCheck::Invalid(message) => {
            mark_invalid_at_revision(state, snapshot.profile.user_id, attempt_revision, &message)
                .await?;
            Ok(false)
        }
        angel::RefreshCheck::Unavailable(message) => {
            mark_unavailable_at_revision(
                state,
                snapshot.profile.user_id,
                attempt_revision,
                &message,
            )
            .await?;
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
    let mut snapshot = session_snapshot(state, user_id).await?;
    if snapshot.credentials.jwt_token.is_empty() {
        return Ok(());
    }
    let near_expiry = angel::jwt_expires_within(&snapshot.credentials.jwt_token, 600);
    let recently_verified = matches!(
        snapshot.profile.last_token_status.as_str(),
        "success" | "refreshed"
    ) && snapshot
        .profile
        .last_token_check_at
        .is_some_and(|checked| checked > Utc::now() - Duration::seconds(30));
    if recently_verified && !near_expiry {
        return Ok(());
    }

    let mut refreshed = false;
    if near_expiry {
        refreshed = refresh_tokens(state, &snapshot).await?;
        if !refreshed {
            return Ok(());
        }
        snapshot = session_snapshot(state, user_id).await?;
    }

    let mut check = angel::verify_session(
        state,
        &snapshot.credentials.api_key,
        &snapshot.credentials.jwt_token,
        &snapshot.credentials.refresh_token,
    )
    .await;
    if matches!(check, angel::SessionCheck::Expired(_)) && !refreshed {
        refreshed = refresh_tokens(state, &snapshot).await?;
        if !refreshed {
            return Ok(());
        }
        snapshot = session_snapshot(state, user_id).await?;
        check = angel::verify_session(
            state,
            &snapshot.credentials.api_key,
            &snapshot.credentials.jwt_token,
            &snapshot.credentials.refresh_token,
        )
        .await;
    }
    match check {
        angel::SessionCheck::Valid => {
            mark_valid_at_revision(
                state,
                user_id,
                snapshot.profile.broker_credential_revision,
                if refreshed { "refreshed" } else { "connected" },
            )
            .await?;
        }
        angel::SessionCheck::Expired(_) => {
            mark_invalid_at_revision(
                state,
                user_id,
                snapshot.profile.broker_credential_revision,
                "Angel One API token is invalid or expired. Please establish the broker connection again.",
            )
            .await?;
        }
        angel::SessionCheck::Unavailable(message) => {
            mark_unavailable_at_revision(
                state,
                user_id,
                snapshot.profile.broker_credential_revision,
                &message,
            )
            .await?;
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
    maintain_user_session(&state, user.id).await?;
    let snapshot = session_snapshot(&state, user.id).await?;
    Ok(Json(details(&snapshot.profile, &snapshot.credentials)))
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
    let snapshot = session_snapshot(&state, user.id).await?;
    if snapshot.credentials.api_key.is_empty() {
        return Err(AppError::BadRequest(
            "Add an Angel One API key before connecting.".into(),
        ));
    }
    if snapshot.profile.brokerage_user_id.is_empty() {
        return Err(AppError::BadRequest(
            "Add an Angel One Client ID before connecting.".into(),
        ));
    }
    let Some(attempt_revision) = begin_session_attempt(
        &state,
        snapshot.profile.user_id,
        snapshot.profile.broker_credential_revision,
        "connecting",
        "Broker connection is being established.",
    )
    .await?
    else {
        return Err(AppError::BadRequest(
            "The broker profile changed while connection was starting. Try again.".into(),
        ));
    };
    let session = match angel::create_session(
        &state,
        &snapshot.profile.brokerage_user_id,
        &snapshot.credentials.api_key,
        &input.mpin,
        &input.totp,
    )
    .await
    {
        Ok(session) => session,
        Err(error) => {
            mark_attempt_failed(
                &state,
                snapshot.profile.user_id,
                attempt_revision,
                "Broker connection failed. Verify the Client ID, API key, MPIN, and TOTP before retrying.",
            )
            .await?;
            return Err(error);
        }
    };
    let finalized = finalize_session_tokens(
        &state,
        snapshot.profile.user_id,
        attempt_revision,
        &[
            ("jwt_token", &session.jwt_token),
            ("refresh_token", &session.refresh_token),
            ("feed_token", &session.feed_token),
        ],
        "connected",
        "success",
        true,
    )
    .await?;
    if !finalized {
        return Err(AppError::BadRequest(
            "The broker profile changed while Angel One was connecting. The stale response was discarded; connect again.".into(),
        ));
    }
    let refreshed = session_snapshot(&state, user.id).await?;
    crate::logs::append(&user.username, "BROKER SESSION connected to Angel One").await;
    crate::strategy::refresh_after_broker_connect(state.clone());
    Ok(Json(
        json!({"message":"Brokerage session established successfully.","last_connected_at":refreshed.profile.token_received_at,"details":details(&refreshed.profile, &refreshed.credentials)}),
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
    let mut tx = state.db.begin().await?;
    lock_user(&mut tx, user.id).await?;
    let execution_in_flight: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trades WHERE user_id=$1 AND status='open') OR EXISTS(SELECT 1 FROM strategy_orders WHERE user_id=$1 AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))")
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
    if execution_in_flight {
        return Err(AppError::BadRequest(
            "The broker API key cannot change while a position or broker order is active. Close or reconcile it first.".into(),
        ));
    }
    state
        .credentials
        .put_in_transaction(
            &mut tx,
            user.id,
            &[
                ("api_key", key),
                ("jwt_token", ""),
                ("refresh_token", ""),
                ("feed_token", ""),
            ],
        )
        .await?;
    let updated = sqlx::query("UPDATE user_profiles SET broker_credential_revision=broker_credential_revision+1,token_state='invalid',token_received_at=NULL,last_token_check_at=NOW(),last_token_status='invalid',last_token_message='The broker API key changed. Establish the broker connection again.',updated_at=NOW() WHERE user_id=$1")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound("User profile not found.".into()));
    }
    tx.commit().await?;
    let refreshed = session_snapshot(&state, user.id).await?;
    Ok(Json(
        json!({"message":"Profile updated successfully.","details":details(&refreshed.profile, &refreshed.credentials)}),
    ))
}
