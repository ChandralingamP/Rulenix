//! Test-only HTTP server for safe, broker-free contract comparisons.
//! It exposes only the authoritative public liveness routes and binds loopback
//! port zero. No database, credentials, background worker, or Angel client is
//! initialized.

use anyhow::Result;
use axum::{
    Router,
    extract::{Query, WebSocketUpgrade, ws::{Message, WebSocket}},
    http::{HeaderMap, StatusCode},
    response::Response,
    Json,
    routing::get,
};
use serde::Deserialize;
use serde_json::json;
use std::io::Write;

pub async fn run() -> Result<()> {
    let app = Router::new()
        .route("/api/health", get(phase10_liveness))
        .route("/api/health/live", get(phase10_liveness))
        .route("/api/ws/strategy", get(strategy_ws))
        .route("/api/ws/market", get(market_ws));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    println!("PHASE10_HTTP_READY http://{address}");
    std::io::stdout().flush()?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn phase10_liveness() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "service": "Rulenix Rust API", "live_trading": false}))
}

fn fixture_session(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').any(|part| part.trim() == "rulenix_session=phase10-session"))
}

async fn strategy_ws(headers: HeaderMap, ws: WebSocketUpgrade) -> Result<Response, StatusCode> {
    let authorized = fixture_session(&headers);
    Ok(ws.on_upgrade(move |socket| strategy_socket(socket, authorized)))
}

async fn strategy_socket(mut socket: WebSocket, authorized: bool) {
    if !authorized {
        let _ = socket.send(Message::Close(Some(axum::extract::ws::CloseFrame { code: 4401, reason: "".into() }))).await;
        return;
    }
    let _ = socket.send(Message::Text(json!({"type":"connected"}).to_string().into())).await;
    while let Some(Ok(message)) = socket.recv().await {
        match message {
            Message::Text(_) => {
                if socket.send(Message::Text(json!({"type":"pong"}).to_string().into())).await.is_err() { break; }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

#[derive(Debug, Deserialize)]
struct FixtureMarketQuery {
    tokens: Option<String>,
}

async fn market_ws(
    headers: HeaderMap,
    Query(query): Query<FixtureMarketQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    let authorized = fixture_session(&headers);
    let has_tokens = !query.tokens.as_deref().unwrap_or_default().trim().is_empty();
    Ok(ws.on_upgrade(move |socket| market_socket(socket, authorized, has_tokens)))
}

async fn market_socket(mut socket: WebSocket, authorized: bool, has_tokens: bool) {
    if !authorized {
        let _ = socket.send(Message::Close(Some(axum::extract::ws::CloseFrame { code: 4401, reason: "".into() }))).await;
        return;
    }
    if !has_tokens {
        let _ = socket.send(Message::Close(Some(axum::extract::ws::CloseFrame { code: 4400, reason: "".into() }))).await;
        return;
    }
    let _ = socket
        .send(Message::Text(json!({"type":"connected","provider":"Angel One SmartAPI"}).to_string().into()))
        .await;
    while let Some(Ok(message)) = socket.recv().await {
        if matches!(message, Message::Close(_)) { break; }
    }
}
