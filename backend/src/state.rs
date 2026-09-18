use crate::config::Config;
use crate::credentials::CredentialStore;
use crate::security::AbusePrevention;
use reqwest::Client;
use sqlx::PgPool;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    },
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

const SCHEDULER_STALE_AFTER_SECONDS: i64 = 60;

#[derive(Debug, Default)]
pub struct SchedulerHealth {
    leader: AtomicBool,
    last_advance_epoch: AtomicI64,
    last_successful_dispatch_epoch: AtomicI64,
    dispatch_count: AtomicU64,
    error_count: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerHealthSnapshot {
    pub leader: bool,
    pub last_advance_epoch: Option<i64>,
    pub last_successful_dispatch_epoch: Option<i64>,
    pub dispatch_count: u64,
    pub error_count: u64,
    pub stale: bool,
}

impl SchedulerHealth {
    fn optional_epoch(value: i64) -> Option<i64> {
        (value > 0).then_some(value)
    }

    pub fn leadership_acquired(&self, now_epoch: i64) {
        self.last_advance_epoch.store(now_epoch, Ordering::Release);
        self.leader.store(true, Ordering::Release);
    }

    pub fn leadership_lost(&self) {
        self.leader.store(false, Ordering::Release);
    }

    pub fn record_advance(&self, now_epoch: i64) {
        self.last_advance_epoch.store(now_epoch, Ordering::Release);
    }

    pub fn record_dispatch(&self) {
        self.dispatch_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_dispatch_success(&self, now_epoch: i64) {
        self.last_successful_dispatch_epoch
            .store(now_epoch, Ordering::Release);
    }

    pub fn record_dispatch_error(&self) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot_at(&self, now_epoch: i64) -> SchedulerHealthSnapshot {
        let leader = self.leader.load(Ordering::Acquire);
        let last_advance = self.last_advance_epoch.load(Ordering::Acquire);
        SchedulerHealthSnapshot {
            leader,
            last_advance_epoch: Self::optional_epoch(last_advance),
            last_successful_dispatch_epoch: Self::optional_epoch(
                self.last_successful_dispatch_epoch.load(Ordering::Acquire),
            ),
            dispatch_count: self.dispatch_count.load(Ordering::Relaxed),
            error_count: self.error_count.load(Ordering::Relaxed),
            stale: leader && now_epoch.saturating_sub(last_advance) > SCHEDULER_STALE_AFTER_SECONDS,
        }
    }
}

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
    /// Process-local scheduler liveness. A standby replica remains ready; an
    /// elected leader becomes unready if its dispatch loop stops advancing.
    pub scheduler_health: Arc<SchedulerHealth>,
    /// Bounds simultaneous user-specific risk/order work while still allowing
    /// one confirmed signal to fan out without serially blocking other users.
    pub strategy_execution_permits: Arc<Semaphore>,
    pub credentials: CredentialStore,
    pub abuse_prevention: AbusePrevention,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_health_distinguishes_standby_progress_and_stale_leader() {
        let health = SchedulerHealth::default();
        assert!(!health.snapshot_at(1_000).leader);
        assert!(!health.snapshot_at(1_000).stale);

        health.leadership_acquired(1_000);
        health.record_dispatch();
        health.record_dispatch_success(1_005);
        assert_eq!(health.snapshot_at(1_060).dispatch_count, 1);
        assert!(!health.snapshot_at(1_060).stale);
        assert!(health.snapshot_at(1_061).stale);

        health.record_advance(1_061);
        assert!(!health.snapshot_at(1_120).stale);
        health.record_dispatch_error();
        assert_eq!(health.snapshot_at(1_120).error_count, 1);

        health.leadership_lost();
        assert!(!health.snapshot_at(9_999).stale);
    }
}
