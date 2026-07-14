mod account;
mod alerts;
mod angel;
mod audit;
mod auth;
mod backtesting;
mod config;
mod credentials;
mod error;
mod home;
mod jobs;
mod logs;
mod margin;
mod market_ws;
mod models;
mod ops;
mod pnl;
mod risk;
mod security;
mod state;
mod strategy;

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{
        HeaderName, HeaderValue, Method,
        header::{ACCEPT, CONTENT_TYPE},
    },
    middleware,
    routing::{get, patch, post},
};
use config::Config;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use state::AppState;
use std::{net::SocketAddr, str::FromStr};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let app_env_for_logs = std::env::var("APP_ENV").unwrap_or_else(|_| "development".into());
    if app_env_for_logs.eq_ignore_ascii_case("production")
        || app_env_for_logs.eq_ignore_ascii_case("staging")
    {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "rulenix_backend=info,tower_http=info".into()),
            )
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "rulenix_backend=info,tower_http=info".into()),
            )
            .with(tracing_subscriber::fmt::layer())
            .init();
    }

    let config = Config::from_env()?;
    let mut database_options =
        PgConnectOptions::from_str(&config.database_url).context("invalid DATABASE_URL")?;
    if config.app_env.eq_ignore_ascii_case("production") {
        database_options = database_options.ssl_mode(PgSslMode::VerifyFull);
    }
    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect_with(database_options)
        .await
        .context("could not connect to PostgreSQL")?;
    sqlx::query("SELECT pg_advisory_lock(hashtext('rulenix:migrations'))")
        .execute(&db)
        .await
        .context("could not acquire migration advisory lock")?;
    let migration_result = sqlx::migrate!().run(&db).await;
    let _ = sqlx::query("SELECT pg_advisory_unlock(hashtext('rulenix:migrations'))")
        .execute(&db)
        .await;
    migration_result.context("database migration failed")?;
    let credential_store = credentials::CredentialStore::from_env(db.clone())
        .context("credential encryption configuration is invalid")?;
    let migrated = credential_store
        .migrate_plaintext()
        .await
        .context("plaintext broker credential migration failed")?;
    if migrated > 0 {
        tracing::info!(
            records = migrated,
            "encrypted legacy broker credential records"
        );
    }
    if config.force_demo_trading {
        let reset = sqlx::query("UPDATE user_profiles SET trading_mode='demo',updated_at=NOW() WHERE trading_mode='live'")
            .execute(&db)
            .await
            .context("could not force demo trading mode")?
            .rows_affected();
        if reset > 0 {
            tracing::warn!(
                profiles = reset,
                "forced live profiles back to demo trading mode"
            );
        }
    }

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--rotate-credentials") {
        let rotated = credential_store.rotate_all().await?;
        println!("Rotated {rotated} encrypted broker secret records.");
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--create-admin-from-env") {
        let username = std::env::var("INITIAL_ADMIN_USERNAME")
            .context("INITIAL_ADMIN_USERNAME is required")?;
        let email =
            std::env::var("INITIAL_ADMIN_EMAIL").context("INITIAL_ADMIN_EMAIL is required")?;
        let password = std::env::var("INITIAL_ADMIN_PASSWORD")
            .context("INITIAL_ADMIN_PASSWORD is required")?;
        auth::bootstrap_admin(&db, &username, &email, &password).await?;
        println!("Configured Rulenix administrator is ready.");
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--create-admin") {
        let username = args
            .get(2)
            .context("usage: rulenix-backend --create-admin USERNAME EMAIL")?;
        let email = args
            .get(3)
            .context("usage: rulenix-backend --create-admin USERNAME EMAIL")?;
        let password = rpassword::prompt_password("Admin password: ")?;
        auth::bootstrap_admin(&db, username, email, &password).await?;
        println!("Rulenix administrator {username} is ready.");
        return Ok(());
    }

    if let (Ok(username), Ok(email), Ok(password)) = (
        std::env::var("INITIAL_ADMIN_USERNAME"),
        std::env::var("INITIAL_ADMIN_EMAIL"),
        std::env::var("INITIAL_ADMIN_PASSWORD"),
    ) {
        auth::ensure_admin(&db, &username, &email, &password)
            .await
            .context("could not initialize the configured administrator")?;
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let (strategy_events, _) = tokio::sync::broadcast::channel(1_024);
    let state = AppState {
        db,
        http,
        config: config.clone(),
        strategy_events,
        strategy_feeds: Default::default(),
        session_checks: Default::default(),
        credentials: credential_store,
        abuse_prevention: Default::default(),
    };
    strategy::start(state.clone());
    home::start_session_maintenance(state.clone());
    auth::start_session_cleanup(state.clone());

    let public_api = Router::new()
        .route("/health", get(ops::liveness))
        .route("/health/live", get(ops::liveness))
        .route("/health/ready", get(ops::readiness))
        .route("/metrics", get(ops::metrics))
        .route("/auth/request-otp/", post(auth::request_otp))
        .route("/auth/signup/", post(auth::signup))
        .route("/auth/login/", post(auth::login))
        .route("/auth/password/request-reset/", post(auth::request_reset))
        .route("/auth/password/verify-otp/", post(auth::verify_reset))
        .route("/auth/password/reset/", post(auth::reset_password));
    let protected_api = Router::new()
        .route("/auth/access/", get(auth::access_status))
        .route("/auth/logout/", post(auth::logout))
        .route(
            "/auth/admin/users/",
            get(auth::list_users)
                .patch(auth::update_user)
                .delete(auth::delete_user),
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
        .route("/account/balance", get(account::get_balance))
        .route("/account/balance/top-up", post(account::top_up_demo))
        .route("/account/balance/reset", post(account::reset_demo))
        .route(
            "/account/trading-mode",
            axum::routing::put(account::update_trading_mode),
        )
        .route("/pnl", get(pnl::list))
        .route("/pnl/export", get(pnl::export))
        .route("/backtesting/runs", get(backtesting::history))
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
            axum::routing::put(risk::update_global_kill),
        )
        .route(
            "/risk/admin/kill-switch/{user_id}",
            axum::routing::put(risk::update_user_kill),
        )
        .route("/ws/market", get(market_ws::upgrade))
        .route(
            "/strategy/futures-breakout",
            get(strategy::status).put(strategy::update),
        )
        .route("/strategies", get(strategy::catalog))
        .route(
            "/strategies/{strategy_key}/activation",
            axum::routing::put(strategy::update_activation),
        )
        .route("/ws/strategy", get(strategy::events_upgrade))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticated,
        ));
    let api = public_api.merge(protected_api);

    let frontend_origins = config
        .frontend_origins
        .iter()
        .map(|origin| origin.parse::<HeaderValue>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("invalid FRONTEND_ORIGINS")?;
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(frontend_origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            ACCEPT,
            CONTENT_TYPE,
            HeaderName::from_static("x-csrf-token"),
        ]);
    let cors = cors.allow_credentials(true);
    let app = Router::new()
        .nest("/api", api)
        .with_state(state.clone())
        .layer(DefaultBodyLimit::max(config.max_request_body_bytes))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security::security_headers,
        ))
        .layer(middleware::from_fn(security::request_ids))
        .layer(cors)
        .layer(TraceLayer::new_for_http());
    let address = SocketAddr::new(config.host, config.port);
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "Rulenix backend listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to install SIGTERM handler");
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
