use crate::{error::AppResult, security::RequestContext, state::AppState};
use axum::{Extension, http::HeaderMap};
use serde_json::Value;
use std::net::IpAddr;
use uuid::Uuid;

pub struct AuditEvent<'a> {
    pub context: Option<&'a RequestContext>,
    pub headers: Option<&'a HeaderMap>,
    pub event_type: &'a str,
    pub actor_user_id: Option<Uuid>,
    pub target_user_id: Option<Uuid>,
    pub summary: &'a str,
    pub metadata: Value,
}

pub async fn record(state: &AppState, event: AuditEvent<'_>) -> AppResult<()> {
    let ip_address = event
        .headers
        .and_then(|headers| headers.get("x-forwarded-for"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| value.parse::<IpAddr>().is_ok());
    sqlx::query(
        "INSERT INTO audit_events (event_type,actor_user_id,target_user_id,request_id,correlation_id,ip_address,summary,metadata) VALUES ($1,$2,$3,$4,$5,$6::inet,$7,$8)",
    )
    .bind(event.event_type)
    .bind(event.actor_user_id)
    .bind(event.target_user_id)
    .bind(event.context.map(|value| value.request_id.as_str()))
    .bind(event.context.map(|value| value.correlation_id.as_str()))
    .bind(ip_address)
    .bind(event.summary)
    .bind(event.metadata)
    .execute(&state.db)
    .await?;
    Ok(())
}

pub fn optional_context(extension: Option<Extension<RequestContext>>) -> Option<RequestContext> {
    extension.map(|Extension(context)| context)
}
