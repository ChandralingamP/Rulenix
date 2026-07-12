use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{Json, extract::State, http::StatusCode};
use serde_json::{Value, json};

pub async fn liveness() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({"status":"ok","service":"Rulenix Rust API"})),
    )
}

pub async fn readiness(State(state): State<AppState>) -> AppResult<Json<Value>> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
        .map_err(AppError::Sqlx)?;
    Ok(Json(json!({
        "status":"ready",
        "checks":{"database":"ok"},
    })))
}

pub async fn metrics(State(state): State<AppState>) -> AppResult<Json<Value>> {
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
    Ok(Json(json!({
        "active_sessions":active_sessions,
        "market_feed_age_seconds":market_feed_age_seconds,
        "scheduler_runs_today":scheduler_runs,
        "orders":orders,
        "risk_rejections_24h":risk_rejections,
        "broker_errors_24h":broker_errors_24h,
        "reconciliation_unhealthy":reconciliation_unhealthy,
    })))
}

async fn json_rows(state: &AppState, sql: &str) -> AppResult<Value> {
    Ok(sqlx::query_scalar::<_, Value>(sql)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| json!({})))
}
