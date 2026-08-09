use crate::config::Config;
use crate::credentials::CredentialStore;
use crate::security::AbusePrevention;
use reqwest::Client;
use sqlx::PgPool;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};
use tokio::sync::{Mutex, broadcast};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub http: Client,
    pub config: Config,
    pub strategy_events: broadcast::Sender<serde_json::Value>,
    pub strategy_feeds: Arc<Mutex<HashSet<String>>>,
    /// Tokens requested by shared strategy feeds, grouped by exchange.
    pub strategy_feed_tokens: Arc<Mutex<HashMap<String, HashSet<String>>>>,
    pub session_checks: Arc<Mutex<HashSet<uuid::Uuid>>>,
    pub angel_api_cooldowns: Arc<Mutex<HashMap<String, Instant>>>,
    pub shared_market_cursor: Arc<Mutex<usize>>,
    pub credentials: CredentialStore,
    pub abuse_prevention: AbusePrevention,
}
