//! Test-only HTTP server for broker-free runtime contract comparisons.
//!
//! HTTP routes use the production handlers and authentication middleware.
//! The adapter accepts only an explicitly disposable loopback PostgreSQL
//! database named `rulenix_test_*`, starts no workers, and binds port zero.
//! The already-proven WebSocket fixtures remain isolated because the
//! production sockets start broker/session feed machinery after upgrade.

use crate::{
    account, auth, backtesting, config::Config, credentials::CredentialStore, egress, home, jobs,
    logs, ops, pnl, risk, security::AbusePrevention, state::AppState, strategy,
};
use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{
        Query, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    middleware,
    response::Response,
    routing::{get, patch, post},
};
use serde::Deserialize;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::{io::Write, net::SocketAddr, sync::Arc};

pub async fn run() -> Result<()> {
    let database_url = isolated_database_url()?;
    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await?;
    let config = Config::for_isolated_test(&database_url, "http://127.0.0.1:9");
    let (strategy_events, _) = tokio::sync::broadcast::channel(64);
    let state = AppState {
        db: db.clone(),
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .build()?,
        config,
        strategy_events,
        strategy_feeds: Default::default(),
        strategy_feed_tokens: Default::default(),
        live_index_candles: Default::default(),
        strategy_tick_sequences: Default::default(),
        session_checks: Default::default(),
        angel_api_cooldowns: Default::default(),
        angel_request_history: Default::default(),
        shared_historical_cooldowns: Default::default(),
        shared_market_cursor: Default::default(),
        strategy_execution_permits: Arc::new(tokio::sync::Semaphore::new(8)),
        credentials: CredentialStore::for_isolated_test(db),
        abuse_prevention: AbusePrevention::default(),
    };

    let public_api = Router::new()
        .route("/health", get(ops::liveness))
        .route("/health/live", get(ops::liveness))
        .route("/health/ready", get(ops::readiness))
        .route("/auth/request-otp/", post(auth::request_otp))
        .route("/auth/signup/", post(auth::signup))
        .route("/auth/login/", post(auth::login))
        .route("/auth/password/request-reset/", post(auth::request_reset))
        .route("/auth/password/verify-otp/", post(auth::verify_reset))
        .route("/auth/password/reset/", post(auth::reset_password));
    let protected_api = Router::new()
        .route("/metrics", get(ops::metrics))
        .route("/auth/access/", get(auth::access_status))
        .route("/auth/logout/", post(auth::logout))
        .route(
            "/auth/admin/users/",
            get(auth::list_users)
                .patch(auth::update_user)
                .delete(auth::delete_user),
        )
        .route(
            "/auth/admin/users/trade-logs/",
            axum::routing::delete(auth::clear_user_trade_logs),
        )
        .route("/auth/admin/trades/daily/", get(auth::daily_trade_report))
        .route("/admin/egress-ips", get(egress::list).post(egress::add))
        .route("/admin/egress-ips/{id}/verify", post(egress::verify))
        .route(
            "/admin/users/{user_id}/angel-egress",
            axum::routing::put(egress::assign),
        )
        .route("/home/status/", get(home::status))
        .route("/home/connect/", post(home::connect))
        .route("/home/profile/", patch(home::update_profile))
        .route(
            "/account/profile",
            get(account::get_profile).patch(account::update_profile),
        )
        .route(
            "/account/profile/request-otp",
            post(account::request_profile_otp),
        )
        .route(
            "/account/trading-mode",
            axum::routing::put(account::update_trading_mode),
        )
        .route("/pnl", get(pnl::list))
        .route("/pnl/export", get(pnl::export))
        .route(
            "/pnl/trades/{trade_id}/close",
            post(strategy::manual_close_trade),
        )
        .route("/backtesting/runs", get(backtesting::history))
        .route(
            "/backtesting/runs/{run_id}/export",
            get(backtesting::export),
        )
        .route("/backtesting/run", post(backtesting::run))
        .route("/logs/files/", get(logs::files))
        .route("/logs/content/", get(logs::content))
        .route("/scheduler/jobs/", get(jobs::list))
        .route("/scheduler/trigger/", post(jobs::trigger))
        .route("/risk/admin", get(risk::admin_status))
        .route(
            "/risk/admin/limits",
            axum::routing::put(risk::update_global_limits),
        )
        .route(
            "/risk/admin/limits/{user_id}",
            axum::routing::put(risk::update_user_limits),
        )
        .route(
            "/risk/admin/kill-switch",
            get(risk::global_kill_state).put(risk::update_global_kill),
        )
        .route(
            "/risk/admin/kill-switch/{user_id}",
            axum::routing::put(risk::update_user_kill),
        )
        .route(
            "/strategy/futures-breakout",
            get(strategy::status).put(strategy::update),
        )
        .route("/strategies", get(strategy::catalog))
        .route(
            "/strategies/admin/executions",
            get(strategy::admin_execution_report),
        )
        .route(
            "/strategies/admin/executions/retry",
            post(strategy::admin_retry_execution_intent),
        )
        .route(
            "/strategies/{strategy_key}/activation",
            axum::routing::put(strategy::update_activation),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticated,
        ));
    let app = Router::new()
        .nest("/api", public_api.merge(protected_api))
        .route("/api/ws/strategy", get(strategy_ws))
        .route("/api/ws/market", get(market_ws))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    println!("PHASE10_HTTP_READY http://{address}");
    std::io::stdout().flush()?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn isolated_database_url() -> Result<String> {
    let raw = std::env::var("TEST_DATABASE_URL").context("TEST_DATABASE_URL is required")?;
    let url = raw
        .strip_prefix("postgresql+asyncpg://")
        .map_or(raw.clone(), |value| format!("postgresql://{value}"));
    let parsed = url::Url::parse(&url).context("invalid TEST_DATABASE_URL")?;
    let database = parsed.path().trim_start_matches('/');
    if !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1"))
        || !database.starts_with("rulenix_test_")
    {
        anyhow::bail!("HTTP adapter requires a loopback rulenix_test_* database");
    }
    Ok(url)
}

fn fixture_session(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .any(|part| part.trim() == "rulenix_session=phase10-session")
        })
}

async fn strategy_ws(headers: HeaderMap, ws: WebSocketUpgrade) -> Result<Response, StatusCode> {
    let authorized = fixture_session(&headers);
    Ok(ws.on_upgrade(move |socket| strategy_socket(socket, authorized)))
}

async fn strategy_socket(mut socket: WebSocket, authorized: bool) {
    if !authorized {
        let _ = socket
            .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                code: 4401,
                reason: "".into(),
            })))
            .await;
        return;
    }
    let _ = socket
        .send(Message::Text(
            json!({"type":"connected"}).to_string().into(),
        ))
        .await;
    while let Some(Ok(message)) = socket.recv().await {
        match message {
            Message::Text(_) => {
                if socket
                    .send(Message::Text(json!({"type":"pong"}).to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
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
    let has_tokens = !query
        .tokens
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty();
    Ok(ws.on_upgrade(move |socket| market_socket(socket, authorized, has_tokens)))
}

async fn market_socket(mut socket: WebSocket, authorized: bool, has_tokens: bool) {
    if !authorized {
        let _ = socket
            .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                code: 4401,
                reason: "".into(),
            })))
            .await;
        return;
    }
    if !has_tokens {
        let _ = socket
            .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                code: 4400,
                reason: "".into(),
            })))
            .await;
        return;
    }
    let _ = socket
        .send(Message::Text(
            json!({"type":"connected","provider":"Angel One SmartAPI"})
                .to_string()
                .into(),
        ))
        .await;
    while let Some(Ok(message)) = socket.recv().await {
        if matches!(message, Message::Close(_)) {
            break;
        }
    }
}
