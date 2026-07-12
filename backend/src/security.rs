use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue, header},
    middleware::Next,
    response::Response,
};
use ipnet::IpNet;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub struct AbusePrevention {
    events: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
}

impl AbusePrevention {
    async fn check(&self, keys: &[String], limit: usize, window: Duration) -> AppResult<()> {
        let now = Instant::now();
        let cutoff = now.checked_sub(window).unwrap_or(now);
        let mut events = self.events.lock().await;
        let mut retry = 0;
        for key in keys {
            let bucket = events.entry(key.clone()).or_default();
            while bucket.front().is_some_and(|at| *at <= cutoff) {
                bucket.pop_front();
            }
            if bucket.len() >= limit {
                retry = retry.max(
                    bucket
                        .front()
                        .map(|at| {
                            window
                                .saturating_sub(now.duration_since(*at))
                                .as_secs()
                                .max(1)
                        })
                        .unwrap_or(1),
                );
            }
        }
        if retry > 0 {
            return Err(AppError::RateLimited { retry_after: retry });
        }
        for key in keys {
            events.entry(key.clone()).or_default().push_back(now);
        }
        if events.len() > 20_000 {
            events.retain(|_, bucket| bucket.back().is_some_and(|at| *at > cutoff));
        }
        Ok(())
    }
}

fn trusted(ip: IpAddr, networks: &[IpNet]) -> bool {
    networks.iter().any(|network| network.contains(&ip))
}

fn forwarded_ip(value: &str) -> Option<IpAddr> {
    let value = value.trim().trim_matches('"');
    if let Some(rest) = value.strip_prefix('[') {
        return rest.split_once(']').and_then(|(ip, _)| ip.parse().ok());
    }
    value
        .parse::<IpAddr>()
        .ok()
        .or_else(|| value.parse::<SocketAddr>().ok().map(|socket| socket.ip()))
}

pub fn client_ip(
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
    trusted_proxies: &[IpNet],
) -> IpAddr {
    let peer_ip = peer
        .map(|value| value.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    if !trusted(peer_ip, trusted_proxies) {
        return peer_ip;
    }
    let mut chain: Vec<IpAddr> = headers
        .get(header::FORWARDED)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .filter_map(|part| {
                    part.split(';').find_map(|item| {
                        let (key, value) = item.trim().split_once('=')?;
                        if !key.eq_ignore_ascii_case("for") {
                            return None;
                        }
                        forwarded_ip(value)
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if chain.is_empty() {
        chain = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(',').filter_map(forwarded_ip).collect())
            .unwrap_or_default();
    }
    chain.push(peer_ip);
    chain
        .into_iter()
        .rev()
        .find(|ip| !trusted(*ip, trusted_proxies))
        .unwrap_or(peer_ip)
}

fn opaque(value: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(value.trim().to_lowercase().as_bytes())
    )
}

pub async fn rate_limit(
    state: &AppState,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
    action: &str,
    identities: &[&str],
    limit: usize,
) -> AppResult<()> {
    let ip = client_ip(peer, headers, &state.config.trusted_proxies);
    let mut keys = vec![format!("{action}:ip:{ip}")];
    keys.extend(
        identities
            .iter()
            .filter(|v| !v.trim().is_empty())
            .map(|v| format!("{action}:id:{}", opaque(v))),
    );
    state
        .abuse_prevention
        .check(
            &keys,
            limit,
            Duration::from_secs(state.config.rate_limit_window_seconds),
        )
        .await
}

pub async fn request_ids(mut request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_safe_header_value(value))
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let correlation_id = request
        .headers()
        .get("x-correlation-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_safe_header_value(value))
        .map(str::to_owned)
        .unwrap_or_else(|| request_id.clone());
    request.extensions_mut().insert(RequestContext {
        request_id: request_id.clone(),
        correlation_id: correlation_id.clone(),
    });
    let span = tracing::info_span!(
        "http_request",
        request_id = %request_id,
        correlation_id = %correlation_id
    );
    let _entered = span.enter();
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    if let Ok(value) = HeaderValue::from_str(&correlation_id) {
        response.headers_mut().insert("x-correlation-id", value);
    }
    response
}

#[derive(Clone, Debug)]
pub struct RequestContext {
    pub request_id: String,
    pub correlation_id: String,
}

pub async fn security_headers(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; connect-src 'self' https: wss:; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
        ),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static(
            "camera=(), microphone=(), geolocation=(), payment=(), usb=(), browsing-topics=()",
        ),
    );
    if state.config.is_production() {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    response
}

fn is_safe_header_value(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Body,
        extract::DefaultBodyLimit,
        http::{HeaderValue, Request, StatusCode},
        routing::post,
    };
    use tower::ServiceExt;

    #[test]
    fn forwarded_headers_are_ignored_from_untrusted_peers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.4"));
        let peer = ConnectInfo("203.0.113.2:1234".parse().unwrap());
        assert_eq!(
            client_ip(Some(peer), &headers, &[]),
            "203.0.113.2".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn trusted_proxy_chain_selects_nearest_untrusted_client() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.4, 10.0.0.8"),
        );
        let peer = ConnectInfo("10.0.0.2:1234".parse().unwrap());
        let trusted = vec!["10.0.0.0/8".parse().unwrap()];
        assert_eq!(
            client_ip(Some(peer), &headers, &trusted),
            "198.51.100.4".parse::<IpAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn brute_force_bucket_returns_retry_information() {
        let limiter = AbusePrevention::default();
        let keys = vec!["login:ip:203.0.113.1".to_string()];
        limiter
            .check(&keys, 1, Duration::from_secs(60))
            .await
            .unwrap();
        let error = limiter
            .check(&keys, 1, Duration::from_secs(60))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppError::RateLimited {
                retry_after: 1..=60
            }
        ));
    }

    #[tokio::test]
    async fn oversized_json_is_rejected_before_the_handler() {
        let app = Router::new()
            .route("/", post(|Json(_): Json<serde_json::Value>| async {}))
            .layer(DefaultBodyLimit::max(16));
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"payload":"this is deliberately too large"}"#,
            ))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
