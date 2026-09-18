use crate::{
    auth::{AuthUser, require_admin_permission},
    error::AppResult,
    state::AppState,
};
use axum::{
    Json,
    extract::{Extension, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

pub async fn liveness() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({"status":"ok","service":"Rulenix Rust API"})),
    )
}

fn scheduler_check(state: &AppState) -> Value {
    let snapshot = state.scheduler_health.snapshot_at(Utc::now().timestamp());
    json!({
        "status": if snapshot.stale { "stale" } else if snapshot.leader { "advancing" } else { "standby" },
        "leader": snapshot.leader,
        "last_advance_at": snapshot.last_advance_epoch.and_then(|value| DateTime::<Utc>::from_timestamp(value, 0)),
        "last_successful_dispatch_at": snapshot.last_successful_dispatch_epoch.and_then(|value| DateTime::<Utc>::from_timestamp(value, 0)),
        "dispatch_count": snapshot.dispatch_count,
        "error_count": snapshot.error_count,
    })
}

pub async fn readiness(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let database_ready = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
        .is_ok();
    let scheduler = state.scheduler_health.snapshot_at(Utc::now().timestamp());
    let ready = database_ready && !scheduler.stale;
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "status": if ready { "ready" } else { "unready" },
            "checks": {
                "database": if database_ready { "ok" } else { "unavailable" },
                "strategy_scheduler": scheduler_check(&state),
            },
        })),
    )
}

pub async fn metrics(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
) -> AppResult<Json<Value>> {
    require_admin_permission(&admin)?;
    let active_sessions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_sessions WHERE revoked_at IS NULL AND idle_expires_at>NOW() AND absolute_expires_at>NOW()",
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let market_feed_age_seconds: Option<f64> = sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM (NOW()-MAX(received_at)))::float8 FROM market_price_ticks",
    )
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None)
    .flatten();
    let scheduler_runs = json_rows(
        &state,
        "SELECT COALESCE(jsonb_object_agg(status,total),'{}'::jsonb) FROM (SELECT status,COUNT(*) AS total FROM strategy_scheduler_runs WHERE trade_date=CURRENT_DATE GROUP BY status) counts",
    )
    .await?;
    let orders = json_rows(
        &state,
        "SELECT COALESCE(jsonb_object_agg(status,total),'{}'::jsonb) FROM (SELECT status,COUNT(*) AS total FROM strategy_orders GROUP BY status) counts",
    )
    .await?;
    let execution_intents = json_rows(
        &state,
        "SELECT COALESCE(jsonb_object_agg(status,total),'{}'::jsonb) FROM (SELECT status,COUNT(*) AS total FROM strategy_execution_intents WHERE created_at>NOW()-INTERVAL '24 hours' GROUP BY status) counts",
    )
    .await?;
    let incomplete_signals_today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM strategy_signals WHERE (signal_at AT TIME ZONE 'Asia/Kolkata')::date=(NOW() AT TIME ZONE 'Asia/Kolkata')::date AND status IN ('dispatching','partial','failed')",
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let risk_rejections: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM risk_decisions WHERE allowed=FALSE AND created_at>NOW()-INTERVAL '24 hours'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let reconciliation_unhealthy: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM broker_reconciliation_health WHERE healthy=FALSE OR checked_at<NOW()-INTERVAL '5 minutes'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let broker_errors_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM broker_order_events WHERE (event_type LIKE '%failed%' OR event_type LIKE '%error%') AND created_at>NOW()-INTERVAL '24 hours'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    let scheduler = scheduler_check(&state);
    Ok(Json(json!({
        "active_sessions":active_sessions,
        "market_feed_age_seconds":market_feed_age_seconds,
        "scheduler_runs_today":scheduler_runs,
        "orders":orders,
        "execution_intents_24h":execution_intents,
        "incomplete_signals_today":incomplete_signals_today,
        "risk_rejections_24h":risk_rejections,
        "broker_errors_24h":broker_errors_24h,
        "reconciliation_unhealthy":reconciliation_unhealthy,
        "strategy_scheduler":scheduler,
    })))
}

async fn json_rows(state: &AppState, sql: &str) -> AppResult<Value> {
    Ok(sqlx::query_scalar::<_, Value>(sql)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| json!({})))
}
