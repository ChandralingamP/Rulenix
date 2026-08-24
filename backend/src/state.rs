use crate::config::Config;
use crate::credentials::CredentialStore;
use crate::security::AbusePrevention;
use reqwest::Client;
use sqlx::PgPool;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Instant,
};
use tokio::sync::{Mutex, Semaphore, broadcast};

#[derive(Debug, Clone, Copy)]
pub struct LiveIndexCandle {
    pub bucket_epoch: i64,
    pub first_tick_epoch_ms: i64,
    pub last_tick_epoch_ms: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

pub type LiveIndexCandleKey = (String, String, i64);
pub type LiveIndexCandleStore = Arc<Mutex<HashMap<LiveIndexCandleKey, LiveIndexCandle>>>;
pub type StrategyTickSequenceKey = (String, String);
pub type StrategyTickSequence = (i64, i64);
pub type StrategyTickSequenceStore =
    Arc<Mutex<HashMap<StrategyTickSequenceKey, StrategyTickSequence>>>;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub http: Client,
    pub config: Config,
    pub strategy_events: broadcast::Sender<serde_json::Value>,
    /// Generation id of the live shared-feed task for each exchange.  A
    /// generation prevents an old task from clearing the lease of a newer
    /// replacement after an admin reload or reconnect race.
    pub strategy_feeds: Arc<Mutex<HashMap<String, uuid::Uuid>>>,
    /// Tokens requested by shared strategy feeds, grouped by exchange.
    pub strategy_feed_tokens: Arc<Mutex<HashMap<String, HashSet<String>>>>,
    /// In-progress five-minute index candles built from the shared Angel feed.
    /// The key is `(exchange, token, five-minute UTC bucket)`.
    pub live_index_candles: LiveIndexCandleStore,
    /// Last accepted `(exchange timestamp ms, sequence)` per broker token.
    pub strategy_tick_sequences: StrategyTickSequenceStore,
    pub session_checks: Arc<Mutex<HashSet<uuid::Uuid>>>,
    pub angel_api_cooldowns: Arc<Mutex<HashMap<String, Instant>>>,
    /// Sliding request histories keyed by a one-way API-key hash and endpoint
    /// class.  This proactively stays below Angel One's per-client limits.
    pub angel_request_history: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    pub shared_historical_cooldowns: Arc<Mutex<HashMap<String, Instant>>>,
    pub shared_market_cursor: Arc<Mutex<usize>>,
    /// Bounds simultaneous user-specific risk/order work while still allowing
    /// one confirmed signal to fan out without serially blocking other users.
    pub strategy_execution_permits: Arc<Semaphore>,
    pub credentials: CredentialStore,
    pub abuse_prevention: AbusePrevention,
}
