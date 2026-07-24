use crate::{error::AppResult, security::RequestContext, state::AppState};
use axum::{Extension, extract::ConnectInfo, http::HeaderMap};
use ipnet::IpNet;
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

fn attributed_ip(
    context: Option<&RequestContext>,
    headers: Option<&HeaderMap>,
    trusted_proxies: &[IpNet],
) -> Option<IpAddr> {
    let peer = context.and_then(|value| value.peer)?;
    let empty_headers = HeaderMap::new();
    Some(crate::security::client_ip(
        Some(ConnectInfo(peer)),
        headers.unwrap_or(&empty_headers),
        trusted_proxies,
    ))
}

pub async fn record(state: &AppState, event: AuditEvent<'_>) -> AppResult<()> {
    let ip_address = attributed_ip(event.context, event.headers, &state.config.trusted_proxies)
        .map(|value| value.to_string());
    sqlx::query(
        "INSERT INTO audit_events (event_type,actor_user_id,target_user_id,request_id,correlation_id,ip_address,summary,metadata) VALUES ($1,$2,$3,$4,$5,$6::inet,$7,$8)",
    )
    .bind(event.event_type)
    .bind(event.actor_user_id)
    .bind(event.target_user_id)
    .bind(event.context.map(|value| value.request_id.as_str()))
    .bind(event.context.map(|value| value.correlation_id.as_str()))
    .bind(ip_address.as_deref())
    .bind(event.summary)
    .bind(event.metadata)
    .execute(&state.db)
    .await?;
    Ok(())
}

pub fn optional_context(extension: Option<Extension<RequestContext>>) -> Option<RequestContext> {
    extension.map(|Extension(context)| context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::net::SocketAddr;

    fn context(peer: &str) -> RequestContext {
        RequestContext {
            request_id: "request".into(),
            correlation_id: "correlation".into(),
            peer: Some(peer.parse::<SocketAddr>().unwrap()),
        }
    }

    #[test]
    fn untrusted_forwarded_address_cannot_spoof_audit_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.40"));
        let context = context("203.0.113.20:443");
        assert_eq!(
            attributed_ip(Some(&context), Some(&headers), &[]),
            Some("203.0.113.20".parse().unwrap())
        );
    }

    #[test]
    fn trusted_proxy_chain_is_used_for_audit_ip() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.40, 10.0.0.8"),
        );
        let context = context("10.0.0.2:443");
        let trusted = vec!["10.0.0.0/8".parse().unwrap()];
        assert_eq!(
            attributed_ip(Some(&context), Some(&headers), &trusted),
            Some("198.51.100.40".parse().unwrap())
        );
    }
}
