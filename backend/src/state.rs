use crate::config::Config;
use crate::credentials::CredentialStore;
use crate::security::AbusePrevention;
use reqwest::Client;
use sqlx::PgPool;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::{Mutex, broadcast};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub http: Client,
    pub config: Config,
    pub strategy_events: broadcast::Sender<serde_json::Value>,
    pub strategy_feeds: Arc<Mutex<HashSet<String>>>,
    pub session_checks: Arc<Mutex<HashSet<uuid::Uuid>>>,
    pub credentials: CredentialStore,
    pub abuse_prevention: AbusePrevention,
}
