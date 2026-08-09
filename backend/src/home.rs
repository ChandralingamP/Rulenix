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
use chrono::{DateTime, Duration, FixedOffset, Utc};
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

const SAME_DAY_SESSION_PRESERVED_MESSAGE: &str = "Today's broker session was preserved. Rulenix will retry automatically; no new login is required.";

fn ist_date(value: &DateTime<Utc>) -> chrono::NaiveDate {
    value
        .with_timezone(&FixedOffset::east_opt(19_800).expect("valid IST offset"))
        .date_naive()
}

fn same_ist_day(left: &DateTime<Utc>, right: &DateTime<Utc>) -> bool {
    ist_date(left) == ist_date(right)
}

fn has_session_tokens(credentials: &BrokerCredentials) -> bool {
    !credentials.api_key.is_empty()
        && !credentials.jwt_token.is_empty()
        && !credentials.refresh_token.is_empty()
        && !credentials.feed_token.is_empty()
}

fn connected_for_today_at(
    profile: &BrokerageProfile,
    credentials: &BrokerCredentials,
    now: &DateTime<Utc>,
) -> bool {
    has_session_tokens(credentials)
        && profile
            .token_received_at
            .as_ref()
            .is_some_and(|received| same_ist_day(received, now))
}

fn connected_for_today(profile: &BrokerageProfile, credentials: &BrokerCredentials) -> bool {
    connected_for_today_at(profile, credentials, &Utc::now())
}

fn details(p: &BrokerageProfile, credentials: &BrokerCredentials) -> Value {
    let connected_today = connected_for_today(p, credentials);
    let connection_state = match (
        connected_today,
        p.token_state.as_str(),
        p.last_token_status.as_str(),
    ) {
        (true, "verification_unavailable" | "refresh_required", _) => "unavailable",
        (true, _, "invalid" | "expired" | "failed") => "unavailable",
        (true, _, _) => "connected",
        (false, "verification_unavailable" | "refresh_required", _) => "unavailable",
        (false, _, "invalid") => "invalid",
        (false, _, "expired") => "expired",
        (false, _, "unavailable") => "unavailable",
        (false, _, "failed") => "failed",
        (false, _, _) if credentials.jwt_token.is_empty() => "idle",
        (false, _, _) => "connected",
    };
    json!({
        "client_id": p.brokerage_user_id,
        "api_key_configured": !credentials.api_key.is_empty(),
        "last_updated": p.updated_at,
        "connection_state": connection_state,
        "token_state": p.token_state,
        "connection_message": match connection_state {
            "connected" | "idle" => Value::Null,
            "unavailable" if connected_today => json!(SAME_DAY_SESSION_PRESERVED_MESSAGE),
            "unavailable" => json!("Angel One is temporarily unavailable. Rulenix will retry automatically."),
            _ => json!(p.last_token_message),
        },
        "connected_for_today": connected_today,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum InvalidationOutcome {
    Invalidated,
    Preserved,
    Unchanged,
}

async fn invalidate_if_revision(
    state: &AppState,
    user_id: Uuid,
    expected_revision: Option<i64>,
    message: &str,
) -> AppResult<InvalidationOutcome> {
    let mut transaction = state.db.begin().await?;
    lock_user(&mut transaction, user_id).await?;
    let current: Option<(i64, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT broker_credential_revision,token_received_at FROM user_profiles WHERE user_id=$1 FOR UPDATE",
    )
        .bind(user_id)
        .fetch_optional(&mut *transaction)
        .await?;
    let Some((current_revision, token_received_at)) = current else {
        transaction.rollback().await?;
        return Ok(InvalidationOutcome::Unchanged);
    };
    if expected_revision.is_some_and(|expected| expected != current_revision) {
        transaction.rollback().await?;
        return Ok(InvalidationOutcome::Unchanged);
    }
    if token_received_at
        .as_ref()
        .is_some_and(|received| same_ist_day(received, &Utc::now()))
    {
        let credentials = state
            .credentials
            .load_in_transaction(&mut transaction, user_id)
            .await?;
        if has_session_tokens(&credentials) {
            sqlx::query("UPDATE user_profiles SET token_state='refresh_required',last_token_check_at=NOW(),last_token_status=CASE WHEN last_token_status IN ('success','refreshed') THEN last_token_status ELSE 'unavailable' END,last_token_message=$2,updated_at=NOW() WHERE user_id=$1")
                .bind(user_id)
                .bind(SAME_DAY_SESSION_PRESERVED_MESSAGE)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            return Ok(InvalidationOutcome::Preserved);
        }
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
    Ok(InvalidationOutcome::Invalidated)
}

async fn alert_invalidation(
    state: &AppState,
    user_id: Uuid,
    outcome: InvalidationOutcome,
    message: &str,
) {
    match outcome {
        InvalidationOutcome::Invalidated => {
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
        InvalidationOutcome::Preserved => {
            crate::strategy::operational_alert(
                state,
                Some(user_id),
                "",
                "broker_session_recovery_deferred",
                "warning",
                SAME_DAY_SESSION_PRESERVED_MESSAGE,
            )
            .await;
        }
        InvalidationOutcome::Unchanged => {}
    }
}

pub(crate) async fn mark_invalid(state: &AppState, user_id: Uuid, message: &str) -> AppResult<()> {
    let outcome = invalidate_if_revision(state, user_id, None, message).await?;
    alert_invalidation(state, user_id, outcome, message).await;
    Ok(())
}

async fn mark_invalid_at_revision(
    state: &AppState,
    user_id: Uuid,
    expected_revision: i64,
    message: &str,
) -> AppResult<()> {
    let outcome = invalidate_if_revision(state, user_id, Some(expected_revision), message).await?;
    alert_invalidation(state, user_id, outcome, message).await;
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
                true,
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
    let snapshot = session_snapshot(state, user_id).await?;
    if snapshot.credentials.jwt_token.is_empty() {
        return Ok(());
    }
    let refresh_required = angel::jwt_expires_within(&snapshot.credentials.jwt_token, 0)
        || matches!(
            snapshot.profile.token_state.as_str(),
            "verification_unavailable" | "refresh_required"
        );
    if !refresh_required {
        return Ok(());
    }
    let recovery_deferred = snapshot
        .profile
        .last_token_check_at
        .is_some_and(|checked| checked > Utc::now() - Duration::minutes(5));
    if recovery_deferred {
        return Ok(());
    }
    refresh_tokens(state, &snapshot).await?;
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
    let mut snapshot = session_snapshot(&state, user.id).await?;
    if connected_for_today(&snapshot.profile, &snapshot.credentials) {
        maintain_user_session(&state, user.id).await?;
        let refreshed = session_snapshot(&state, user.id).await?;
        if connected_for_today(&refreshed.profile, &refreshed.credentials) {
            return Ok(Json(json!({
                "message":"Brokerage session is already established for today.",
                "last_connected_at":refreshed.profile.token_received_at,
                "details":details(&refreshed.profile, &refreshed.credentials),
            })));
        }
        snapshot = refreshed;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn profile(received: DateTime<Utc>) -> BrokerageProfile {
        BrokerageProfile {
            user_id: Uuid::nil(),
            brokerage_user_id: "CLIENT01".into(),
            broker_credential_revision: 1,
            token_state: "connected".into(),
            token_received_at: Some(received),
            last_token_check_at: None,
            last_token_status: "success".into(),
            last_token_message: String::new(),
            updated_at: timestamp("2026-07-28T03:30:00Z"),
        }
    }

    fn credentials() -> BrokerCredentials {
        BrokerCredentials {
            api_key: "api".into(),
            jwt_token: "jwt".into(),
            refresh_token: "refresh".into(),
            feed_token: "feed".into(),
        }
    }

    #[test]
    fn same_day_connection_uses_ist_calendar_boundaries() {
        let before_midnight = timestamp("2026-07-28T18:20:00Z");
        let after_midnight = timestamp("2026-07-28T18:40:00Z");
        assert!(!same_ist_day(&before_midnight, &after_midnight));
        assert!(same_ist_day(
            &timestamp("2026-07-28T03:30:00Z"),
            &before_midnight
        ));
    }

    #[test]
    fn same_day_connection_requires_every_session_token() {
        let now = timestamp("2026-07-28T12:00:00Z");
        let profile = profile(timestamp("2026-07-28T03:30:00Z"));
        let mut credentials = credentials();
        assert!(connected_for_today_at(&profile, &credentials, &now));

        credentials.feed_token.clear();
        assert!(!connected_for_today_at(&profile, &credentials, &now));
    }
}
