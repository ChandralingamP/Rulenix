use crate::{
    angel,
    auth::AuthUser,
    contract_master::{self, MasterContract},
    credentials::BrokerCredentials,
    error::{AppError, AppResult},
    instruments::{
        FUTURES_BREAKOUT_INSTRUMENTS, futures_breakout_label, futures_pnl_units,
        is_futures_breakout_instrument,
    },
    risk,
    state::{AppState, LiveIndexCandle},
};
use axum::{
    Json,
    extract::{
        Extension, Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::HeaderMap,
    response::Response,
};
use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone,
    Timelike, Utc, Weekday,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::time::{MissedTickBehavior, interval};
use uuid::Uuid;

pub const STRATEGY_KEY: &str = "futures_breakout_v3";
pub const SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY: &str = "supertrend_index_options_v1";
const SENSEX_INDEX_TOKEN: &str = "99919000";
const NIFTY_INDEX_TOKEN: &str = "99926000";
const OPTION_INTERVAL: &str = "FIVE_MINUTE";
const OPTION_PRODUCT_TYPE: &str = "INTRADAY";
const SUPERTREND_ENTRY_START_MINUTE: u32 = 9 * 60 + 15;
const OPTION_SQUARE_OFF_MINUTE: u32 = 15 * 60 + 10;
const OPTION_SCHEDULER_END_MINUTE: u32 = 15 * 60 + 30;
const FUTURES_EXPIRY_SQUARE_OFF_MINUTE: u32 = 15 * 60 + 20;
const SHARED_MARKET_CREDENTIAL_LIMIT: i64 = 8;
const SUPERTREND_ATR_PERIOD: usize = 7;
const SUPERTREND_FACTOR: f64 = 2.0;
const SUPERTREND_LOOKBACK_DAYS: i64 = 14;
const SUPERTREND_MAX_ENTRY_DELAY_SECONDS: i64 = 90;
const SUPERTREND_CANDLE_RETRY_ATTEMPTS: usize = 4;
const SUPERTREND_SENSEX_DEFAULT_TARGET_POINTS: f64 = 40.0;
const SUPERTREND_SENSEX_DEFAULT_STOP_POINTS: f64 = 25.0;
const SUPERTREND_NIFTY_DEFAULT_TARGET_POINTS: f64 = 25.0;
const SUPERTREND_NIFTY_DEFAULT_STOP_POINTS: f64 = 15.0;
const SHARED_HISTORICAL_RATE_LIMIT_BACKOFF: std::time::Duration =
    std::time::Duration::from_secs(10 * 60);
#[derive(Debug, Clone)]
struct OptionContract {
    token: String,
    symbol: String,
    expiry: NaiveDate,
    lot_size: i32,
    strike: f64,
    option_type: &'static str,
    premium: f64,
}

#[derive(Debug, Clone)]
struct SuperTrendMarketSelection {
    contract: OptionContract,
    underlying_ltp: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct IntradayCandle {
    pub(crate) at: NaiveDateTime,
    pub(crate) open: f64,
    pub(crate) high: f64,
    pub(crate) low: f64,
    pub(crate) close: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexOptionSide {
    Call,
    Put,
}

impl IndexOptionSide {
    fn option_type(self) -> &'static str {
        match self {
            Self::Call => "CE",
            Self::Put => "PE",
        }
    }

    fn entry_role(self) -> &'static str {
        "BUY_ENTRY"
    }

    fn entry_side(self) -> &'static str {
        "BUY"
    }

    fn exit_side(self) -> &'static str {
        "SELL"
    }

    fn opposite(self) -> Self {
        match self {
            Self::Call => Self::Put,
            Self::Put => Self::Call,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct IndexOptionConfig {
    instrument: &'static str,
    index_exchange: &'static str,
    index_token: &'static str,
    option_exchange: &'static str,
    option_name: &'static str,
    label: &'static str,
    default_target_points: f64,
    default_stop_loss_points: f64,
}

impl IndexOptionConfig {
    fn option_instrument(self, side: IndexOptionSide) -> String {
        format!("{}_{}", self.instrument, side.option_type())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuperTrendDirection {
    Up,
    Down,
}

impl SuperTrendDirection {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Up => "UP",
            Self::Down => "DOWN",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SuperTrendPoint {
    pub(crate) candle: IntradayCandle,
    pub(crate) value: f64,
    pub(crate) direction: SuperTrendDirection,
}

#[derive(Debug, Clone, Copy)]
struct SuperTrendSignal {
    side: IndexOptionSide,
    signal_at: NaiveDateTime,
    index_close: f64,
    supertrend: f64,
    previous_direction: SuperTrendDirection,
    direction: SuperTrendDirection,
}

#[derive(Debug, Clone, FromRow)]
struct SuperTrendRunner {
    user_id: Uuid,
    username: String,
    instrument: String,
    lots: i32,
    run_day_session: bool,
    run_evening_session: bool,
    trading_mode: String,
    target_points: f64,
    stop_loss_points: f64,
}

impl From<SuperTrendRunner> for Runner {
    fn from(value: SuperTrendRunner) -> Self {
        Self {
            user_id: value.user_id,
            username: value.username,
            instrument: value.instrument,
            lots: value.lots,
            run_day_session: value.run_day_session,
            run_evening_session: value.run_evening_session,
            trading_mode: value.trading_mode,
        }
    }
}

type ResidualProtectionTradeRow = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    i32,
    i32,
    Option<f64>,
    Option<f64>,
    bool,
);
type ExitFillTradeRow = (
    String,
    i32,
    i32,
    i32,
    f64,
    f64,
    Option<f64>,
    Option<f64>,
    String,
    String,
);
type OrderTradeQuantityRow = (i32, String, Option<i32>, Option<DateTime<Utc>>);

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Snapshot {
    pub id: Uuid,
    pub strategy_key: String,
    pub instrument: String,
    pub trade_date: NaiveDate,
    pub status: String,
    pub error: Option<String>,
    pub contract_token: Option<String>,
    pub contract_symbol: Option<String>,
    pub contract_expiry: Option<NaiveDate>,
    pub lot_size: Option<i32>,
    pub exchange_segment: String,
    pub product_type: String,
    pub execution_key: String,
    pub underlying_token: String,
    pub candle_dates: Vec<NaiveDate>,
    pub highs: Vec<f64>,
    pub lows: Vec<f64>,
    pub hh2: Option<f64>,
    pub ll2: Option<f64>,
    pub hh4: Option<f64>,
    pub ll4: Option<f64>,
    pub buy_entry: Option<f64>,
    pub buy_target: Option<f64>,
    pub buy_sl1: Option<f64>,
    pub buy_sl2: Option<f64>,
    pub sell_entry: Option<f64>,
    pub sell_target: Option<f64>,
    pub sell_sl1: Option<f64>,
    pub sell_sl2: Option<f64>,
    pub previous_close: Option<f64>,
    pub market_open: Option<f64>,
    pub gap_direction: Option<String>,
    pub entry_direction: Option<String>,
    pub entry_source: Option<String>,
    pub gap_plan_status: Option<String>,
    pub opening_range_high: Option<f64>,
    pub opening_range_low: Option<f64>,
    pub planned_entry: Option<f64>,
    pub planned_target: Option<f64>,
    pub planned_sl1: Option<f64>,
    pub planned_sl2: Option<f64>,
    pub gap_planned_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Levels {
    pub(crate) hh2: f64,
    pub(crate) ll2: f64,
    pub(crate) hh4: f64,
    pub(crate) ll4: f64,
    pub(crate) buy_entry: f64,
    pub(crate) buy_target: f64,
    pub(crate) buy_sl1: f64,
    pub(crate) buy_sl2: f64,
    pub(crate) sell_entry: f64,
    pub(crate) sell_target: f64,
    pub(crate) sell_sl1: f64,
    pub(crate) sell_sl2: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FuturesExitLevels {
    pub target: f64,
    pub sl1: f64,
    pub sl2: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FuturesMissedEntryPlan {
    pub buy_missed: bool,
    pub sell_missed: bool,
}

impl FuturesMissedEntryPlan {
    pub(crate) fn as_str(self) -> &'static str {
        match (self.buy_missed, self.sell_missed) {
            (true, true) => "BOTH_MISSED",
            (true, false) => "BUY_MISSED",
            (false, true) => "SELL_MISSED",
            (false, false) => "NONE_MISSED",
        }
    }
}

pub(crate) fn futures_missed_entry_plan(
    market_open: f64,
    buy_entry: f64,
    sell_entry: f64,
) -> Option<FuturesMissedEntryPlan> {
    if [market_open, buy_entry, sell_entry]
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
        || buy_entry <= sell_entry
    {
        return None;
    }
    Some(FuturesMissedEntryPlan {
        buy_missed: market_open >= buy_entry,
        sell_missed: market_open <= sell_entry,
    })
}

pub(crate) fn futures_opening_range_entries(
    plan: FuturesMissedEntryPlan,
    opening_range_high: f64,
    opening_range_low: f64,
) -> Option<(Option<f64>, Option<f64>)> {
    if [opening_range_high, opening_range_low]
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
        || opening_range_high < opening_range_low
    {
        return None;
    }
    Some((
        plan.buy_missed
            .then_some(opening_range_high * (1.0 + 0.0012)),
        plan.sell_missed
            .then_some(opening_range_low * (1.0 - 0.0012)),
    ))
}

pub(crate) fn futures_exit_levels_for_entry(
    direction: &str,
    entry: f64,
    hh2: f64,
    ll2: f64,
    hh4: f64,
    ll4: f64,
) -> Option<FuturesExitLevels> {
    if [entry, hh2, ll2, hh4, ll4]
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return None;
    }
    let (target, sl1, sl2) = match direction {
        "BUY" => {
            let percentage_stop = entry * (1.0 - 0.015);
            (
                entry * (1.0 + 0.015),
                percentage_stop.max(ll2 * (1.0 - 0.0012)),
                percentage_stop.max(ll4 * (1.0 - 0.0012)),
            )
        }
        "SELL" => {
            let percentage_stop = entry * (1.0 + 0.015);
            (
                entry * (1.0 - 0.015),
                percentage_stop.min(hh2 * (1.0 + 0.0012)),
                percentage_stop.min(hh4 * (1.0 + 0.0012)),
            )
        }
        _ => return None,
    };
    [target, sl1, sl2]
        .iter()
        .all(|value| value.is_finite() && *value > 0.0)
        .then_some(FuturesExitLevels { target, sl1, sl2 })
}

pub(crate) fn calculate(highs: &[f64], lows: &[f64]) -> Option<Levels> {
    if highs.len() != 4 || lows.len() != 4 {
        return None;
    }
    let max = |values: &[f64]| values.iter().copied().reduce(f64::max);
    let min = |values: &[f64]| values.iter().copied().reduce(f64::min);
    let hh2 = max(&highs[2..])?;
    let ll2 = min(&lows[2..])?;
    let hh4 = max(highs)?;
    let ll4 = min(lows)?;
    let buy_entry = hh4 * (1.0 + 0.0012);
    let sell_entry = ll4 * (1.0 - 0.0012);
    let buy = futures_exit_levels_for_entry("BUY", buy_entry, hh2, ll2, hh4, ll4)?;
    let sell = futures_exit_levels_for_entry("SELL", sell_entry, hh2, ll2, hh4, ll4)?;
    Some(Levels {
        hh2,
        ll2,
        hh4,
        ll4,
        buy_entry,
        buy_target: buy.target,
        buy_sl1: buy.sl1,
        buy_sl2: buy.sl2,
        sell_entry,
        sell_target: sell.target,
        sell_sl1: sell.sl1,
        sell_sl2: sell.sl2,
    })
}

fn true_ranges(candles: &[IntradayCandle]) -> Vec<f64> {
    candles
        .iter()
        .enumerate()
        .map(|(index, candle)| {
            if index == 0 {
                candle.high - candle.low
            } else {
                let previous_close = candles[index - 1].close;
                (candle.high - candle.low)
                    .max((candle.high - previous_close).abs())
                    .max((candle.low - previous_close).abs())
            }
        })
        .collect()
}

fn rma(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut result = vec![None; values.len()];
    if period == 0 || values.len() < period {
        return result;
    }
    let seed = values[..period].iter().sum::<f64>() / period as f64;
    result[period - 1] = Some(seed);
    let mut previous = seed;
    for (index, value) in values.iter().enumerate().skip(period) {
        previous = (previous * (period as f64 - 1.0) + *value) / period as f64;
        result[index] = Some(previous);
    }
    result
}

pub(crate) fn supertrend_points(
    candles: &[IntradayCandle],
    atr_period: usize,
    factor: f64,
) -> Vec<SuperTrendPoint> {
    if atr_period == 0 || !factor.is_finite() || factor <= 0.0 {
        return Vec::new();
    }
    let atr = rma(&true_ranges(candles), atr_period);
    let mut points = Vec::new();
    let mut previous_upper = None;
    let mut previous_lower = None;
    let mut previous_direction = None;
    for (index, candle) in candles.iter().enumerate() {
        let Some(atr_value) = atr[index] else {
            continue;
        };
        let hl2 = (candle.high + candle.low) / 2.0;
        let basic_upper = hl2 + factor * atr_value;
        let basic_lower = hl2 - factor * atr_value;
        let (final_upper, final_lower, direction) = if !points.is_empty() {
            let previous_close = candles[index.saturating_sub(1)].close;
            let previous_upper = previous_upper.unwrap_or(basic_upper);
            let previous_lower = previous_lower.unwrap_or(basic_lower);
            let final_upper = if basic_upper < previous_upper || previous_close > previous_upper {
                basic_upper
            } else {
                previous_upper
            };
            let final_lower = if basic_lower > previous_lower || previous_close < previous_lower {
                basic_lower
            } else {
                previous_lower
            };
            let direction = match previous_direction.unwrap_or(SuperTrendDirection::Down) {
                SuperTrendDirection::Down => {
                    if candle.close > final_upper {
                        SuperTrendDirection::Up
                    } else {
                        SuperTrendDirection::Down
                    }
                }
                SuperTrendDirection::Up => {
                    if candle.close < final_lower {
                        SuperTrendDirection::Down
                    } else {
                        SuperTrendDirection::Up
                    }
                }
            };
            (final_upper, final_lower, direction)
        } else {
            // TradingView's ta.supertrend starts on the upper band when the
            // previous ATR is unavailable.  Seeding from candle colour made
            // the first session in a cache window diverge from TradingView.
            (basic_upper, basic_lower, SuperTrendDirection::Down)
        };
        let value = match direction {
            SuperTrendDirection::Up => final_lower,
            SuperTrendDirection::Down => final_upper,
        };
        previous_upper = Some(final_upper);
        previous_lower = Some(final_lower);
        previous_direction = Some(direction);
        points.push(SuperTrendPoint {
            candle: *candle,
            value,
            direction,
        });
    }
    points
}

#[allow(dead_code)]
fn supertrend_signal(points: &[SuperTrendPoint]) -> Option<SuperTrendSignal> {
    let previous = points.get(points.len().checked_sub(2)?)?;
    let latest = points.last()?;
    if previous.direction == latest.direction {
        return None;
    }
    let side = match (previous.direction, latest.direction) {
        (SuperTrendDirection::Down, SuperTrendDirection::Up) => IndexOptionSide::Call,
        (SuperTrendDirection::Up, SuperTrendDirection::Down) => IndexOptionSide::Put,
        _ => return None,
    };
    Some(SuperTrendSignal {
        side,
        signal_at: latest.candle.at,
        index_close: latest.candle.close,
        supertrend: latest.value,
        previous_direction: previous.direction,
        direction: latest.direction,
    })
}

fn supertrend_signal_from_transition(
    previous: &SuperTrendPoint,
    latest: &SuperTrendPoint,
) -> Option<SuperTrendSignal> {
    if previous.direction == latest.direction {
        return None;
    }
    let side = match (previous.direction, latest.direction) {
        (SuperTrendDirection::Down, SuperTrendDirection::Up) => IndexOptionSide::Call,
        (SuperTrendDirection::Up, SuperTrendDirection::Down) => IndexOptionSide::Put,
        _ => return None,
    };
    Some(SuperTrendSignal {
        side,
        signal_at: latest.candle.at,
        index_close: latest.candle.close,
        supertrend: latest.value,
        previous_direction: previous.direction,
        direction: latest.direction,
    })
}

fn current_supertrend_signal(
    points: &[SuperTrendPoint],
    now: DateTime<FixedOffset>,
) -> Option<SuperTrendSignal> {
    let latest_allowed = option_latest_completed_candle_time(now);
    let pair = points.windows(2).find(|pair| {
        pair.get(1)
            .is_some_and(|latest| latest.candle.at == latest_allowed)
    })?;
    supertrend_signal_from_transition(pair.first()?, pair.get(1)?)
}

fn supertrend_signal_is_fresh(signal: SuperTrendSignal, now: DateTime<FixedOffset>) -> bool {
    let closed_at = signal.signal_at + Duration::minutes(5);
    let delay = now.naive_local() - closed_at;
    delay >= Duration::zero() && delay <= Duration::seconds(SUPERTREND_MAX_ENTRY_DELAY_SECONDS)
}

fn supertrend_snapshot_underlying(instrument: &str) -> Option<&str> {
    instrument
        .strip_suffix("_CE")
        .or_else(|| instrument.strip_suffix("_PE"))
        .filter(|underlying| is_supertrend_index_option_instrument(underlying))
}

fn supertrend_snapshot_side(instrument: &str) -> Option<IndexOptionSide> {
    if instrument.ends_with("_CE") {
        Some(IndexOptionSide::Call)
    } else if instrument.ends_with("_PE") {
        Some(IndexOptionSide::Put)
    } else {
        None
    }
}

fn supertrend_config_points(snapshot: &Snapshot) -> Option<(f64, f64)> {
    match supertrend_snapshot_side(&snapshot.instrument)? {
        IndexOptionSide::Call => Some((snapshot.buy_target?, snapshot.buy_sl1?)),
        IndexOptionSide::Put => Some((snapshot.sell_target?, snapshot.sell_sl1?)),
    }
}

fn option_minute_of_day(now: DateTime<FixedOffset>) -> u32 {
    now.hour() * 60 + now.minute()
}

pub(crate) fn supertrend_entry_allowed(now: DateTime<FixedOffset>) -> bool {
    let minute = option_minute_of_day(now);
    (SUPERTREND_ENTRY_START_MINUTE..OPTION_SQUARE_OFF_MINUTE).contains(&minute)
}

pub(crate) fn option_square_off_due(now: DateTime<FixedOffset>) -> bool {
    option_minute_of_day(now) >= OPTION_SQUARE_OFF_MINUTE
}

fn option_expiry_checkpoint_due(expiry: NaiveDate, now: DateTime<FixedOffset>) -> bool {
    expiry < now.date_naive() || (expiry == now.date_naive() && option_square_off_due(now))
}

fn futures_expiry_checkpoint_due(expiry: NaiveDate, now: DateTime<FixedOffset>) -> bool {
    expiry < now.date_naive()
        || (expiry == now.date_naive()
            && option_minute_of_day(now) >= FUTURES_EXPIRY_SQUARE_OFF_MINUTE)
}

fn parse_expiry(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(&value.to_uppercase(), "%d%b%Y").ok()
}

fn weekdays_until(start: NaiveDate, expiry: NaiveDate) -> i64 {
    let mut cursor = start;
    let mut count = 0;
    while cursor < expiry {
        cursor += Duration::days(1);
        if !matches!(cursor.weekday(), Weekday::Sat | Weekday::Sun) {
            count += 1;
        }
    }
    count
}

fn select_contract(
    contracts: &[MasterContract],
    instrument: &str,
    date: NaiveDate,
) -> Option<(MasterContract, NaiveDate)> {
    contracts
        .iter()
        .filter(|item| {
            item.exch_seg == "MCX"
                && item.name.eq_ignore_ascii_case(instrument)
                && item.instrumenttype == "FUTCOM"
        })
        .filter_map(|item| parse_expiry(&item.expiry).map(|expiry| (item.clone(), expiry)))
        .filter(|(_, expiry)| *expiry >= date && weekdays_until(date, *expiry) >= 10)
        .min_by_key(|(_, expiry)| *expiry)
}

fn parse_lot_size(value: &str) -> Option<i32> {
    value
        .parse::<i32>()
        .ok()
        .or_else(|| value.parse::<f64>().ok().map(|value| value as i32))
        .filter(|value| *value > 0)
}

fn parse_tick_size(value: &str) -> Option<f64> {
    // Angel One's OpenAPI instrument master expresses tick size in paise.
    let tick = value.trim().parse::<f64>().ok()? / 100.0;
    (tick.is_finite() && tick > 0.0).then_some(tick)
}

pub(crate) fn normalize_to_tick(price: f64, tick_size: f64, side: &str) -> Option<f64> {
    const SCALE: f64 = 1_000_000.0;
    if !price.is_finite() || price <= 0.0 || !tick_size.is_finite() || tick_size <= 0.0 {
        return None;
    }
    let price_units = (price * SCALE).round();
    let tick_units = (tick_size * SCALE).round();
    if price_units > i64::MAX as f64 || tick_units < 1.0 || tick_units > i64::MAX as f64 {
        return None;
    }
    let price_units = price_units as i64;
    let tick_units = tick_units as i64;
    let ticks = match side {
        "BUY" => price_units.saturating_add(tick_units - 1) / tick_units,
        "SELL" => price_units / tick_units,
        _ => return None,
    };
    let normalized_units = ticks.checked_mul(tick_units)?;
    let normalized = normalized_units as f64 / SCALE;
    (normalized.is_finite() && normalized > 0.0).then_some(normalized)
}

fn valid_contract_quantity(quantity: i32, lot_size: i32, lots: i32) -> bool {
    quantity > 0 && lot_size > 0 && lots > 0 && quantity % lot_size == 0
}

#[derive(Debug, Clone, Copy)]
enum OrderQuantityPolicy {
    NormalEntry { lot_size: i32, lots: i32 },
    NormalExit { remaining_quantity: i32 },
    BrokerResidual { broker_quantity: i32 },
}

fn quantity_matches_policy(quantity: i32, policy: OrderQuantityPolicy) -> bool {
    if quantity <= 0 {
        return false;
    }
    match policy {
        OrderQuantityPolicy::NormalEntry { lot_size, lots } => lot_size
            .checked_mul(lots)
            .is_some_and(|expected| lot_size > 0 && lots > 0 && quantity == expected),
        OrderQuantityPolicy::NormalExit { remaining_quantity } => {
            remaining_quantity > 0 && quantity <= remaining_quantity
        }
        OrderQuantityPolicy::BrokerResidual { broker_quantity } => {
            broker_quantity > 0 && quantity == broker_quantity
        }
    }
}

async fn current_contract_order_metadata(
    state: &AppState,
    snapshot: &Snapshot,
    entry_order: bool,
) -> AppResult<(i32, f64)> {
    let token = snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Snapshot has no contract token.".into()))?;
    let symbol = snapshot
        .contract_symbol
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Snapshot has no contract symbol.".into()))?;
    let contracts = if entry_order {
        load_contract_master(state).await?
    } else {
        contract_master::load_allow_stale(state).await?
    };
    let contract = contracts
        .iter()
        .find(|contract| {
            contract.token == token
                && contract
                    .exch_seg
                    .eq_ignore_ascii_case(&snapshot.exchange_segment)
        })
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "Contract {symbol} ({token}) is absent from today's Angel One instrument master."
            ))
        })?;
    if contract.symbol != symbol {
        return Err(AppError::BadRequest(format!(
            "Stale contract metadata: token {token} now maps to {}, not {symbol}.",
            contract.symbol
        )));
    }
    let lot_size = parse_lot_size(&contract.lotsize)
        .ok_or_else(|| AppError::BadRequest("Current contract lot size is invalid.".into()))?;
    let tick_size = parse_tick_size(&contract.tick_size)
        .ok_or_else(|| AppError::BadRequest("Current contract tick size is invalid.".into()))?;
    let expiry = parse_expiry(&contract.expiry)
        .ok_or_else(|| AppError::BadRequest("Current contract expiry is invalid.".into()))?;
    if entry_order
        && snapshot
            .contract_expiry
            .is_some_and(|stored| stored != expiry)
    {
        return Err(AppError::BadRequest(format!(
            "Stale contract metadata: stored expiry does not match today's master for {symbol}."
        )));
    }
    if entry_order && expiry < ist_now().date_naive() {
        return Err(AppError::BadRequest(format!(
            "Contract {symbol} is expired; no replacement order was submitted."
        )));
    }
    Ok((lot_size, tick_size))
}

fn parse_option_strike(value: &str) -> Option<f64> {
    value.parse::<f64>().ok().map(|value| value / 100.0)
}

fn index_option_config(instrument: &str) -> Option<IndexOptionConfig> {
    match instrument {
        "SENSEX" => Some(IndexOptionConfig {
            instrument: "SENSEX",
            index_exchange: "BSE",
            index_token: SENSEX_INDEX_TOKEN,
            option_exchange: "BFO",
            option_name: "SENSEX",
            label: "SENSEX ATM Options",
            default_target_points: SUPERTREND_SENSEX_DEFAULT_TARGET_POINTS,
            default_stop_loss_points: SUPERTREND_SENSEX_DEFAULT_STOP_POINTS,
        }),
        "NIFTY" => Some(IndexOptionConfig {
            instrument: "NIFTY",
            index_exchange: "NSE",
            index_token: NIFTY_INDEX_TOKEN,
            option_exchange: "NFO",
            option_name: "NIFTY",
            label: "NIFTY ATM Options",
            default_target_points: SUPERTREND_NIFTY_DEFAULT_TARGET_POINTS,
            default_stop_loss_points: SUPERTREND_NIFTY_DEFAULT_STOP_POINTS,
        }),
        _ => None,
    }
}

fn is_supertrend_index_option_instrument(instrument: &str) -> bool {
    index_option_config(instrument).is_some()
}

fn supertrend_option_candidates(
    contracts: &[MasterContract],
    config: IndexOptionConfig,
    date: NaiveDate,
    side: IndexOptionSide,
) -> Vec<OptionContract> {
    let option_type = side.option_type();
    let mut candidates: Vec<OptionContract> = contracts
        .iter()
        .filter(|item| {
            item.exch_seg == config.option_exchange
                && item.name.eq_ignore_ascii_case(config.option_name)
                && item.instrumenttype == "OPTIDX"
                && item.symbol.ends_with(option_type)
        })
        .filter_map(|item| {
            let expiry = parse_expiry(&item.expiry)?;
            let lot_size = parse_lot_size(&item.lotsize)?;
            let strike = parse_option_strike(&item.strike)?;
            (expiry >= date).then_some(OptionContract {
                token: item.token.clone(),
                symbol: item.symbol.clone(),
                expiry,
                lot_size,
                strike,
                option_type,
                premium: 0.0,
            })
        })
        .collect();
    candidates.sort_by(|left, right| {
        left.expiry
            .cmp(&right.expiry)
            .then_with(|| left.strike.total_cmp(&right.strike))
    });
    candidates
}

fn supertrend_option_expiry_preview(
    contracts: &[MasterContract],
    config: IndexOptionConfig,
    date: NaiveDate,
) -> Option<(NaiveDate, i32)> {
    [IndexOptionSide::Call, IndexOptionSide::Put]
        .into_iter()
        .flat_map(|side| supertrend_option_candidates(contracts, config, date, side))
        .min_by_key(|contract| contract.expiry)
        .map(|contract| (contract.expiry, contract.lot_size))
}

#[allow(dead_code)]
fn choose_atm_contract(
    candidates: &[OptionContract],
    underlying_ltp: f64,
) -> Option<OptionContract> {
    candidates.iter().cloned().min_by(|left, right| {
        (left.strike - underlying_ltp)
            .abs()
            .total_cmp(&(right.strike - underlying_ltp).abs())
            .then_with(|| left.expiry.cmp(&right.expiry))
            .then_with(|| left.strike.total_cmp(&right.strike))
    })
}

fn quote_string(map: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(|value| match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    })
}

fn quote_number(map: &serde_json::Map<String, Value>, key: &str) -> Option<f64> {
    map.get(key)
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .filter(|value| value.is_finite() && *value > 0.0)
}

fn quote_price(map: &serde_json::Map<String, Value>) -> Option<f64> {
    for key in [
        "ltp",
        "LTP",
        "last_traded_price",
        "lastTradedPrice",
        "last_price",
        "close",
    ] {
        if let Some(price) = quote_number(map, key) {
            return Some(price);
        }
    }
    None
}

fn collect_quote_ltps(value: &Value, prices: &mut HashMap<String, f64>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_quote_ltps(value, prices);
            }
        }
        Value::Object(map) => {
            let token = ["symbolToken", "symboltoken", "symbol_token", "token"]
                .iter()
                .find_map(|key| quote_string(map, key));
            if let (Some(token), Some(price)) = (token, quote_price(map)) {
                prices.insert(token, price);
            }
            for value in map.values() {
                collect_quote_ltps(value, prices);
            }
        }
        _ => {}
    }
}

fn extract_quote_ltps(value: &Value) -> HashMap<String, f64> {
    let mut prices = HashMap::new();
    collect_quote_ltps(value, &mut prices);
    prices
}

fn quote_ltp_for_token(value: &Value, token: &str) -> Option<f64> {
    extract_quote_ltps(value).get(token).copied()
}

fn quote_price_band_for_token(value: &Value, token: &str) -> Option<(f64, f64)> {
    fn visit(value: &Value, token: &str) -> Option<(f64, f64)> {
        match value {
            Value::Array(values) => values.iter().find_map(|value| visit(value, token)),
            Value::Object(map) => {
                let item_token = ["symbolToken", "symboltoken", "symbol_token", "token"]
                    .iter()
                    .find_map(|key| quote_string(map, key));
                if item_token.as_deref() == Some(token) {
                    let lower = ["lowerCircuit", "lowercircuit", "lower_circuit"]
                        .iter()
                        .find_map(|key| quote_number(map, key));
                    let upper = ["upperCircuit", "uppercircuit", "upper_circuit"]
                        .iter()
                        .find_map(|key| quote_number(map, key));
                    if let (Some(lower), Some(upper)) = (lower, upper)
                        && lower.is_finite()
                        && upper.is_finite()
                        && lower > 0.0
                        && upper >= lower
                    {
                        return Some((lower, upper));
                    }
                }
                map.values().find_map(|value| visit(value, token))
            }
            _ => None,
        }
    }
    visit(value, token)
}

fn collect_quote_opens(value: &Value, prices: &mut HashMap<String, f64>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_quote_opens(value, prices);
            }
        }
        Value::Object(map) => {
            let token = ["symbolToken", "symboltoken", "symbol_token", "token"]
                .iter()
                .find_map(|key| quote_string(map, key));
            let open = ["open", "openPrice", "open_price", "open_price_of_the_day"]
                .iter()
                .find_map(|key| quote_number(map, key));
            if let (Some(token), Some(open)) = (token, open) {
                prices.insert(token, open);
            }
            for value in map.values() {
                collect_quote_opens(value, prices);
            }
        }
        _ => {}
    }
}

fn extract_quote_opens(value: &Value) -> HashMap<String, f64> {
    let mut prices = HashMap::new();
    collect_quote_opens(value, &mut prices);
    prices
}

fn find_quote_ltp(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64().filter(|price| *price > 0.0),
        Value::Array(values) => values.iter().find_map(find_quote_ltp),
        Value::Object(map) => {
            for key in [
                "ltp",
                "LTP",
                "last_traded_price",
                "lastTradedPrice",
                "last_price",
                "close",
            ] {
                if let Some(price) = map.get(key).and_then(Value::as_f64)
                    && price > 0.0
                {
                    return Some(price);
                }
            }
            map.values().find_map(find_quote_ltp)
        }
        _ => None,
    }
}

fn parse_intraday_candles(raw: &Value) -> Vec<IntradayCandle> {
    let mut candles = BTreeMap::new();
    for candle in raw.as_array().into_iter().flatten().filter_map(|row| {
        let values = row.as_array()?;
        let timestamp = values.first()?.as_str()?;
        let timestamp = NaiveDateTime::parse_from_str(
            timestamp.get(..19).unwrap_or(timestamp),
            "%Y-%m-%dT%H:%M:%S",
        )
        .or_else(|_| {
            NaiveDateTime::parse_from_str(
                timestamp.get(..16).unwrap_or(timestamp),
                "%Y-%m-%d %H:%M",
            )
        })
        .ok()?;
        let parse = |index: usize| {
            values
                .get(index)?
                .as_f64()
                .or_else(|| values.get(index)?.as_str()?.parse().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
        };
        let candle = IntradayCandle {
            at: timestamp,
            open: parse(1)?,
            high: parse(2)?,
            low: parse(3)?,
            close: parse(4)?,
        };
        (candle.high >= candle.open.max(candle.close)
            && candle.low <= candle.open.min(candle.close)
            && candle.high >= candle.low)
            .then_some(candle)
    }) {
        // Angel occasionally repeats a candle at the REST/live boundary. The
        // last valid observation wins deterministically for that identity.
        candles.insert(candle.at, candle);
    }
    candles.into_values().collect()
}

fn snapshot_select() -> &'static str {
    "SELECT id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,candle_dates,highs,lows,hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2,previous_close,market_open,gap_direction,entry_direction,entry_source,gap_plan_status,opening_range_high,opening_range_low,planned_entry,planned_target,planned_sl1,planned_sl2,gap_planned_at,fetched_at FROM strategy_market_snapshots"
}

async fn load_snapshot(
    state: &AppState,
    instrument: &str,
    date: NaiveDate,
) -> AppResult<Option<Snapshot>> {
    let query = format!(
        "{} WHERE strategy_key=$1 AND instrument=$2 AND trade_date=$3",
        snapshot_select()
    );
    Ok(sqlx::query_as(&query)
        .bind(STRATEGY_KEY)
        .bind(instrument)
        .bind(date)
        .fetch_optional(&state.db)
        .await?)
}

fn has_contract_metadata(snapshot: &Snapshot) -> bool {
    snapshot
        .contract_token
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && snapshot
            .contract_symbol
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && snapshot.contract_expiry.is_some()
        && snapshot.lot_size.is_some_and(|value| value > 0)
}

fn has_valid_contract_metadata(snapshot: &Snapshot, date: NaiveDate) -> bool {
    if !has_contract_metadata(snapshot) {
        return false;
    }
    let Some(expiry) = snapshot.contract_expiry else {
        return false;
    };
    if expiry < date {
        return false;
    }
    if snapshot.strategy_key == STRATEGY_KEY {
        return weekdays_until(date, expiry) >= 10;
    }
    true
}

async fn select_contract_with_master_refresh(
    state: &AppState,
    contracts: &[MasterContract],
    instrument: &str,
    date: NaiveDate,
) -> AppResult<(MasterContract, NaiveDate)> {
    if let Some(selected) = select_contract(contracts, instrument, date) {
        return Ok(selected);
    }
    contract_master::invalidate_cache().await;
    let refreshed = load_contract_master(state).await?;
    select_contract(&refreshed, instrument, date).ok_or_else(|| {
        AppError::BadRequest(format!(
            "No eligible MCX {instrument} FUTCOM contract is at least 10 trading days from expiry in the latest Angel One contract master."
        ))
    })
}

async fn upsert_contract_metadata(
    state: &AppState,
    contracts: &[MasterContract],
    instrument: &str,
    date: NaiveDate,
) -> AppResult<Snapshot> {
    let (contract, expiry) =
        select_contract_with_master_refresh(state, contracts, instrument, date).await?;
    let lot_size = parse_lot_size(&contract.lotsize)
        .filter(|value| *value > 0)
        .ok_or_else(|| AppError::BadRequest("Selected contract has an invalid lot size.".into()))?;
    let previous = load_snapshot(state, instrument, date).await?;
    let contract_changed = previous.as_ref().is_none_or(|snapshot| {
        snapshot.contract_token.as_deref() != Some(contract.token.as_str())
            || snapshot.contract_symbol.as_deref() != Some(contract.symbol.as_str())
            || snapshot.contract_expiry != Some(expiry)
            || snapshot.lot_size != Some(lot_size)
    });
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size) VALUES ($1,$2,$3,$4,'missing','Daily market levels are pending.',$5,$6,$7,$8) ON CONFLICT (strategy_key,instrument,trade_date,execution_key) DO UPDATE SET status=CASE WHEN $9 THEN 'missing' ELSE strategy_market_snapshots.status END,error=CASE WHEN $9 THEN 'Daily market levels are pending after contract rollover.' WHEN strategy_market_snapshots.status='ready' THEN strategy_market_snapshots.error ELSE EXCLUDED.error END,contract_token=EXCLUDED.contract_token,contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,fetched_at=NOW()")
        .bind(Uuid::new_v4()).bind(STRATEGY_KEY).bind(instrument).bind(date)
        .bind(&contract.token).bind(&contract.symbol).bind(expiry).bind(lot_size)
        .bind(contract_changed)
        .execute(&state.db).await?;
    let snapshot = load_snapshot(state, instrument, date)
        .await?
        .expect("contract metadata upserted");
    emit(
        state,
        None,
        instrument,
        "contract_selected",
        json!({"contract_token":snapshot.contract_token,"contract_symbol":snapshot.contract_symbol,"contract_expiry":snapshot.contract_expiry,"lot_size":snapshot.lot_size}),
    )
    .await;
    Ok(snapshot)
}

async fn ensure_contract_metadata(
    state: &AppState,
    instrument: &str,
    date: NaiveDate,
) -> AppResult<Snapshot> {
    if let Some(snapshot) = load_snapshot(state, instrument, date).await?
        && has_valid_contract_metadata(&snapshot, date)
    {
        return Ok(snapshot);
    }
    let contracts = load_contract_master(state).await?;
    upsert_contract_metadata(state, &contracts, instrument, date).await
}

async fn ensure_supported_contract_metadata(
    state: &AppState,
    date: NaiveDate,
) -> AppResult<HashMap<String, Snapshot>> {
    let mut snapshots = HashMap::new();
    let mut missing = Vec::new();
    for instrument in FUTURES_BREAKOUT_INSTRUMENTS {
        match load_snapshot(state, instrument, date).await? {
            Some(snapshot) if has_valid_contract_metadata(&snapshot, date) => {
                snapshots.insert(instrument.to_string(), snapshot);
            }
            _ => missing.push(instrument),
        }
    }
    if missing.is_empty() {
        return Ok(snapshots);
    }
    let contracts = load_contract_master(state).await?;
    for instrument in missing {
        let snapshot = upsert_contract_metadata(state, &contracts, instrument, date).await?;
        snapshots.insert(instrument.to_string(), snapshot);
    }
    Ok(snapshots)
}

async fn force_refresh_futures_contract_snapshot(
    state: &AppState,
    instrument: &str,
    date: NaiveDate,
) -> AppResult<Option<Snapshot>> {
    let previous = load_snapshot(state, instrument, date).await?;
    contract_master::invalidate_cache().await;
    let contracts = load_contract_master(state).await?;
    let refreshed = upsert_contract_metadata(state, &contracts, instrument, date).await?;
    let changed = previous.as_ref().is_none_or(|snapshot| {
        snapshot.contract_token != refreshed.contract_token
            || snapshot.contract_symbol != refreshed.contract_symbol
            || snapshot.contract_expiry != refreshed.contract_expiry
            || snapshot.lot_size != refreshed.lot_size
    });
    if changed {
        Ok(Some(create_snapshot(state, instrument, date).await?))
    } else {
        Ok(None)
    }
}

async fn create_snapshot(
    state: &AppState,
    instrument: &str,
    date: NaiveDate,
) -> AppResult<Snapshot> {
    if let Some(snapshot) = load_snapshot(state, instrument, date).await?
        && snapshot.status == "ready"
        && has_valid_contract_metadata(&snapshot, date)
        && snapshot
            .previous_close
            .is_some_and(|value| value.is_finite() && value > 0.0)
    {
        return Ok(snapshot);
    }
    let contract_snapshot = ensure_contract_metadata(state, instrument, date).await?;
    let token = contract_snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Selected contract token is missing.".into()))?;
    let symbol = contract_snapshot
        .contract_symbol
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Selected contract symbol is missing.".into()))?;
    let expiry = contract_snapshot
        .contract_expiry
        .ok_or_else(|| AppError::BadRequest("Selected contract expiry is missing.".into()))?;
    let lot_size = contract_snapshot
        .lot_size
        .ok_or_else(|| AppError::BadRequest("Selected contract lot size is missing.".into()))?;
    let from = date - Duration::days(20);
    let to = date - Duration::days(1);
    let raw = shared_market_candles(
        state,
        "MCX",
        token,
        "ONE_DAY",
        &format!("{} 00:00", from.format("%Y-%m-%d")),
        &format!("{} 23:59", to.format("%Y-%m-%d")),
    )
    .await?;
    let mut candles: Vec<(NaiveDate, f64, f64, f64)> = raw
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let values = row.as_array()?;
            let day = values.first()?.as_str()?.get(..10)?.parse().ok()?;
            let high = values
                .get(2)?
                .as_f64()
                .or_else(|| values.get(2)?.as_str()?.parse().ok())?;
            let low = values
                .get(3)?
                .as_f64()
                .or_else(|| values.get(3)?.as_str()?.parse().ok())?;
            let close = values
                .get(4)?
                .as_f64()
                .or_else(|| values.get(4)?.as_str()?.parse().ok())?;
            (day < date
                && high.is_finite()
                && high > 0.0
                && low.is_finite()
                && low > 0.0
                && close.is_finite()
                && close > 0.0)
                .then_some((day, high, low, close))
        })
        .collect();
    candles.sort_by_key(|row| row.0);
    candles.dedup_by_key(|row| row.0);
    if candles.len() > 4 {
        candles = candles.split_off(candles.len() - 4);
    }
    let id = Uuid::new_v4();
    let dates: Vec<NaiveDate> = candles.iter().map(|row| row.0).collect();
    let highs: Vec<f64> = candles.iter().map(|row| row.1).collect();
    let lows: Vec<f64> = candles.iter().map(|row| row.2).collect();
    let previous_close = candles.last().map(|row| row.3);
    let levels = calculate(&highs, &lows);
    let status = if levels.is_some() { "ready" } else { "missing" };
    let error = (levels.is_none()).then(|| {
        format!(
            "Expected 4 completed trading days, received {}.",
            candles.len()
        )
    });
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,candle_dates,highs,lows,hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2,previous_close,fetched_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,NOW()) ON CONFLICT (strategy_key,instrument,trade_date,execution_key) DO UPDATE SET status=EXCLUDED.status,error=EXCLUDED.error,contract_token=EXCLUDED.contract_token,contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,candle_dates=EXCLUDED.candle_dates,highs=EXCLUDED.highs,lows=EXCLUDED.lows,hh2=EXCLUDED.hh2,ll2=EXCLUDED.ll2,hh4=EXCLUDED.hh4,ll4=EXCLUDED.ll4,buy_entry=EXCLUDED.buy_entry,buy_target=EXCLUDED.buy_target,buy_sl1=EXCLUDED.buy_sl1,buy_sl2=EXCLUDED.buy_sl2,sell_entry=EXCLUDED.sell_entry,sell_target=EXCLUDED.sell_target,sell_sl1=EXCLUDED.sell_sl1,sell_sl2=EXCLUDED.sell_sl2,previous_close=EXCLUDED.previous_close,fetched_at=NOW()")
        .bind(id).bind(STRATEGY_KEY).bind(instrument).bind(date).bind(status).bind(&error)
        .bind(token).bind(symbol).bind(expiry).bind(lot_size)
        .bind(&dates).bind(&highs).bind(&lows)
        .bind(levels.map(|v|v.hh2)).bind(levels.map(|v|v.ll2)).bind(levels.map(|v|v.hh4)).bind(levels.map(|v|v.ll4))
        .bind(levels.map(|v|v.buy_entry)).bind(levels.map(|v|v.buy_target)).bind(levels.map(|v|v.buy_sl1)).bind(levels.map(|v|v.buy_sl2))
        .bind(levels.map(|v|v.sell_entry)).bind(levels.map(|v|v.sell_target)).bind(levels.map(|v|v.sell_sl1)).bind(levels.map(|v|v.sell_sl2))
        .bind(previous_close)
        .execute(&state.db).await?;
    let snapshot = load_snapshot(state, instrument, date)
        .await?
        .expect("snapshot upserted");
    emit(
        state,
        None,
        instrument,
        "snapshot_updated",
        json!({"snapshot":snapshot}),
    )
    .await;
    Ok(snapshot)
}

struct MarketCredential {
    profile_id: Uuid,
    credentials: BrokerCredentials,
}

async fn shared_market_session_count(state: &AppState) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_profiles p JOIN users u ON u.id=p.user_id WHERE u.is_active=TRUE AND p.last_token_status IN ('success','refreshed') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='api_key') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='jwt_token')",
    )
    .fetch_one(&state.db)
    .await?)
}

async fn shared_market_credentials(state: &AppState) -> AppResult<Vec<MarketCredential>> {
    let profile_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT p.user_id FROM user_profiles p JOIN users u ON u.id=p.user_id WHERE u.is_active=TRUE AND p.last_token_status IN ('success','refreshed') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='api_key') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='jwt_token') ORDER BY CASE WHEN EXISTS (SELECT 1 FROM user_strategy_activations a WHERE a.user_id=p.user_id AND a.is_active=TRUE) THEN 0 ELSE 1 END,p.token_received_at DESC NULLS LAST LIMIT $1",
    )
    .bind(SHARED_MARKET_CREDENTIAL_LIMIT)
    .fetch_all(&state.db)
    .await?;
    if profile_ids.is_empty() {
        return Err(AppError::BadRequest(
            "No connected Angel One session is available for shared market data.".into(),
        ));
    }
    let mut credentials = Vec::new();
    for profile_id in profile_ids {
        match state.credentials.load(profile_id).await {
            Ok(profile_credentials)
                if !profile_credentials.api_key.is_empty()
                    && !profile_credentials.jwt_token.is_empty() =>
            {
                credentials.push(MarketCredential {
                    profile_id,
                    credentials: profile_credentials,
                });
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(
                %profile_id,
                %error,
                "could not load shared market-data credentials"
            ),
        }
    }
    if credentials.is_empty() {
        return Err(AppError::BadRequest(
            "No usable Angel One session is available for shared market data.".into(),
        ));
    }
    let start = {
        let mut cursor = state.shared_market_cursor.lock().await;
        let start = *cursor % credentials.len();
        *cursor = cursor.wrapping_add(1);
        start
    };
    credentials.rotate_left(start);
    Ok(credentials)
}

async fn handle_shared_market_error(
    state: &AppState,
    profile_id: Uuid,
    error: AppError,
) -> AppResult<(String, bool)> {
    let message = error.to_string();
    if angel::is_invalid_api_key_error(&message) {
        crate::home::mark_invalid(
            state,
            profile_id,
            "Angel One API token is invalid. Please establish the broker connection again.",
        )
        .await?;
        return Ok((message, true));
    }
    Ok((message.clone(), angel::is_rate_limit_error(&message)))
}

async fn shared_market_quote(
    state: &AppState,
    mode: &str,
    exchange_tokens: Value,
) -> AppResult<Value> {
    let credentials = shared_market_credentials(state).await?;
    let total = credentials.len();
    let mut last_error = None;
    for (attempt, credential) in credentials.into_iter().enumerate() {
        match angel::market_quote(
            state,
            credential.profile_id,
            &credential.credentials.api_key,
            &credential.credentials.jwt_token,
            mode,
            exchange_tokens.clone(),
        )
        .await
        {
            Ok(value) => {
                if attempt > 0 {
                    tracing::info!(
                        attempt = attempt + 1,
                        total,
                        "shared market quote recovered with alternate Angel One session"
                    );
                }
                return Ok(value);
            }
            Err(error) => {
                let (message, try_next) =
                    handle_shared_market_error(state, credential.profile_id, error).await?;
                tracing::warn!(
                    profile_id = %credential.profile_id,
                    attempt = attempt + 1,
                    total,
                    error = %message,
                    "shared market quote failed"
                );
                last_error = Some(message);
                if !try_next {
                    break;
                }
            }
        }
    }
    Err(AppError::BadRequest(format!(
        "All shared Angel One market-data sessions are unavailable. Last error: {}",
        last_error.unwrap_or_else(|| "unknown market-data failure".into())
    )))
}

#[allow(clippy::too_many_arguments)]
async fn shared_market_candles(
    state: &AppState,
    exchange: &str,
    token: &str,
    interval: &str,
    from_date: &str,
    to_date: &str,
) -> AppResult<Value> {
    let credentials = shared_market_credentials(state).await?;
    let total = credentials.len();
    let mut last_error = None;
    for (attempt, credential) in credentials.into_iter().enumerate() {
        match angel::get_candles_with_exchange_interval(
            state,
            credential.profile_id,
            &credential.credentials.api_key,
            &credential.credentials.jwt_token,
            exchange,
            token,
            interval,
            from_date,
            to_date,
        )
        .await
        {
            Ok(value) => {
                if attempt > 0 {
                    tracing::info!(
                        attempt = attempt + 1,
                        total,
                        "shared market candles recovered with alternate Angel One session"
                    );
                }
                return Ok(value);
            }
            Err(error) => {
                let (message, try_next) =
                    handle_shared_market_error(state, credential.profile_id, error).await?;
                tracing::warn!(
                    profile_id = %credential.profile_id,
                    attempt = attempt + 1,
                    total,
                    error = %message,
                    "shared market candles failed"
                );
                last_error = Some(message);
                if !try_next {
                    break;
                }
            }
        }
    }
    Err(AppError::BadRequest(format!(
        "All shared Angel One historical-data sessions are unavailable. Last error: {}",
        last_error.unwrap_or_else(|| "unknown historical-data failure".into())
    )))
}

fn historical_cooldown_key(exchange: &str, token: &str, interval: &str) -> String {
    format!(
        "{}:{}:{}",
        exchange.to_uppercase(),
        token.trim(),
        interval.to_uppercase()
    )
}

async fn shared_historical_cooldown_active(
    state: &AppState,
    exchange: &str,
    token: &str,
    interval: &str,
) -> bool {
    let key = historical_cooldown_key(exchange, token, interval);
    let now = std::time::Instant::now();
    let mut cooldowns = state.shared_historical_cooldowns.lock().await;
    cooldowns.retain(|_, until| *until > now);
    cooldowns.get(&key).is_some_and(|until| *until > now)
}

async fn activate_shared_historical_cooldown(
    state: &AppState,
    exchange: &str,
    token: &str,
    interval: &str,
) {
    let key = historical_cooldown_key(exchange, token, interval);
    let until = std::time::Instant::now() + SHARED_HISTORICAL_RATE_LIMIT_BACKOFF;
    let mut cooldowns = state.shared_historical_cooldowns.lock().await;
    cooldowns
        .entry(key)
        .and_modify(|current| *current = (*current).max(until))
        .or_insert(until);
}

async fn first_session_open(state: &AppState, snapshot: &Snapshot) -> AppResult<f64> {
    let token = snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Selected contract token is missing.".into()))?;
    let date = snapshot.trade_date;
    let raw = shared_market_candles(
        state,
        &snapshot.exchange_segment,
        token,
        "ONE_MINUTE",
        &format!("{} 09:00", date.format("%Y-%m-%d")),
        &format!("{} 09:02", date.format("%Y-%m-%d")),
    )
    .await?;
    parse_intraday_candles(&raw)
        .into_iter()
        .find(|candle| {
            candle.at.date() == date
                && candle.at.time() == NaiveTime::from_hms_opt(9, 0, 0).expect("valid market open")
        })
        .map(|candle| candle.open)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "Angel One returned no 09:00 open for {}.",
                snapshot.instrument
            ))
        })
}

async fn ensure_futures_gap_plans(
    state: &AppState,
    date: NaiveDate,
    required_instrument: &str,
) -> AppResult<()> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("rulenix:futures-gap-plan:{date}"))
        .execute(&mut *tx)
        .await?;
    let query = format!(
        "{} WHERE strategy_key=$1 AND trade_date=$2 ORDER BY instrument",
        snapshot_select()
    );
    let snapshots: Vec<Snapshot> = sqlx::query_as(&query)
        .bind(STRATEGY_KEY)
        .bind(date)
        .fetch_all(&mut *tx)
        .await?;
    let pending: Vec<Snapshot> = snapshots
        .into_iter()
        .filter(|snapshot| {
            snapshot.status == "ready"
                && !matches!(
                    snapshot.gap_plan_status.as_deref(),
                    Some("READY" | "WAITING_RANGE")
                )
        })
        .collect();
    if pending.is_empty() {
        tx.commit().await?;
        return Ok(());
    }

    let tokens: Vec<String> = pending
        .iter()
        .filter_map(|snapshot| snapshot.contract_token.clone())
        .collect();
    if tokens.is_empty() {
        return Err(AppError::BadRequest(
            "No selected futures contract tokens are available for the gap plan.".into(),
        ));
    }
    let quote = shared_market_quote(state, "FULL", json!({"MCX":tokens})).await?;
    let market_opens = extract_quote_opens(&quote);
    let mut planned = Vec::new();
    let mut errors = HashMap::new();
    for snapshot in pending {
        let plan = async {
            let token = snapshot.contract_token.as_deref().ok_or_else(|| {
                AppError::BadRequest("Selected contract token is missing.".into())
            })?;
            let market_open = match market_opens.get(token).copied() {
                Some(value) => value,
                None => first_session_open(state, &snapshot).await?,
            };
            let buy_entry = required_exit_level(snapshot.buy_entry, "buy entry")?;
            let sell_entry = required_exit_level(snapshot.sell_entry, "sell entry")?;
            let missed =
                futures_missed_entry_plan(market_open, buy_entry, sell_entry).ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "Could not validate missed breakout entries for {}.",
                        snapshot.instrument
                    ))
                })?;
            let (source, status) = if missed.buy_missed || missed.sell_missed {
                ("OPENING_RANGE", "WAITING_RANGE")
            } else {
                ("STANDARD", "READY")
            };
            Ok::<_, AppError>((market_open, missed, source, status))
        };
        let (market_open, missed, source, status) = match plan.await {
            Ok(value) => value,
            Err(error) => {
                errors.insert(snapshot.instrument.clone(), error.to_string());
                continue;
            }
        };
        sqlx::query(
            "UPDATE strategy_market_snapshots
             SET market_open=$2,gap_direction=$3,entry_direction=$4,entry_source=$5,
                  gap_plan_status=$6,opening_range_high=NULL,opening_range_low=NULL,
                  planned_entry=NULL,planned_target=NULL,planned_sl1=NULL,planned_sl2=NULL,
                  gap_planned_at=NOW()
              WHERE id=$1",
        )
        .bind(snapshot.id)
        .bind(market_open)
        .bind(missed.as_str())
        .bind("BOTH")
        .bind(source)
        .bind(status)
        .execute(&mut *tx)
        .await?;
        planned.push((
            snapshot.instrument,
            json!({
                "previous_close": snapshot.previous_close,
                "market_open": market_open,
                "missed_entry_plan": missed.as_str(),
                "buy_missed": missed.buy_missed,
                "sell_missed": missed.sell_missed,
                "entry_direction": "BOTH",
                "entry_source": source,
                "status": status,
                "normal_buy_entry": snapshot.buy_entry,
                "normal_sell_entry": snapshot.sell_entry,
            }),
        ));
    }
    tx.commit().await?;
    for (instrument, payload) in planned {
        emit(state, None, &instrument, "gap_entry_plan_updated", payload).await;
    }
    match errors.remove(required_instrument) {
        Some(error) => Err(AppError::BadRequest(error)),
        None => Ok(()),
    }
}

fn snapshot_missed_entry_plan(snapshot: &Snapshot) -> AppResult<FuturesMissedEntryPlan> {
    match snapshot.gap_direction.as_deref() {
        Some("BUY_MISSED") => Ok(FuturesMissedEntryPlan {
            buy_missed: true,
            sell_missed: false,
        }),
        Some("SELL_MISSED") => Ok(FuturesMissedEntryPlan {
            buy_missed: false,
            sell_missed: true,
        }),
        Some("BOTH_MISSED") => Ok(FuturesMissedEntryPlan {
            buy_missed: true,
            sell_missed: true,
        }),
        Some("NONE_MISSED") => Ok(FuturesMissedEntryPlan {
            buy_missed: false,
            sell_missed: false,
        }),
        _ => Err(AppError::BadRequest(format!(
            "{} has no valid missed-entry plan.",
            snapshot.instrument
        ))),
    }
}

async fn resolve_futures_opening_range_plan(
    state: &AppState,
    snapshot: &Snapshot,
) -> AppResult<Snapshot> {
    if snapshot.gap_plan_status.as_deref() == Some("READY") {
        return Ok(snapshot.clone());
    }
    if snapshot.gap_plan_status.as_deref() != Some("WAITING_RANGE") {
        return Err(AppError::BadRequest(format!(
            "{} has no opening-range entry pending.",
            snapshot.instrument
        )));
    }
    let token = snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Selected contract token is missing.".into()))?;
    let date = snapshot.trade_date;
    let raw = shared_market_candles(
        state,
        &snapshot.exchange_segment,
        token,
        "FIFTEEN_MINUTE",
        &format!("{} 09:00", date.format("%Y-%m-%d")),
        &format!("{} 09:15", date.format("%Y-%m-%d")),
    )
    .await?;
    let start = NaiveTime::from_hms_opt(9, 0, 0).expect("valid opening range");
    let end = NaiveTime::from_hms_opt(9, 15, 0).expect("valid opening range");
    let opening: Vec<IntradayCandle> = parse_intraday_candles(&raw)
        .into_iter()
        .filter(|candle| {
            candle.at.date() == date && candle.at.time() >= start && candle.at.time() < end
        })
        .collect();
    let opening_range_high = opening
        .iter()
        .map(|candle| candle.high)
        .reduce(f64::max)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "Angel One returned no completed 09:00-09:15 range for {}.",
                snapshot.instrument
            ))
        })?;
    let opening_range_low = opening
        .iter()
        .map(|candle| candle.low)
        .reduce(f64::min)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "Angel One returned no completed 09:00-09:15 range for {}.",
                snapshot.instrument
            ))
        })?;
    let missed = snapshot_missed_entry_plan(snapshot)?;
    let (recovered_buy, recovered_sell) =
        futures_opening_range_entries(missed, opening_range_high, opening_range_low).ok_or_else(
            || {
                AppError::BadRequest(format!(
                    "Could not calculate opening-range entries for {}.",
                    snapshot.instrument
                ))
            },
        )?;
    let hh2 = required_exit_level(snapshot.hh2, "HH2")?;
    let ll2 = required_exit_level(snapshot.ll2, "LL2")?;
    let hh4 = required_exit_level(snapshot.hh4, "HH4")?;
    let ll4 = required_exit_level(snapshot.ll4, "LL4")?;
    let buy_exits = recovered_buy
        .and_then(|entry| futures_exit_levels_for_entry("BUY", entry, hh2, ll2, hh4, ll4));
    let sell_exits = recovered_sell
        .and_then(|entry| futures_exit_levels_for_entry("SELL", entry, hh2, ll2, hh4, ll4));
    if recovered_buy.is_some() != buy_exits.is_some()
        || recovered_sell.is_some() != sell_exits.is_some()
    {
        return Err(AppError::BadRequest(format!(
            "Could not calculate opening-range exit levels for {}.",
            snapshot.instrument
        )));
    }
    sqlx::query(
        "UPDATE strategy_market_snapshots
         SET opening_range_high=$2,opening_range_low=$3,
              buy_entry=COALESCE($4,buy_entry),buy_target=COALESCE($5,buy_target),
              buy_sl1=COALESCE($6,buy_sl1),buy_sl2=COALESCE($7,buy_sl2),
              sell_entry=COALESCE($8,sell_entry),sell_target=COALESCE($9,sell_target),
              sell_sl1=COALESCE($10,sell_sl1),sell_sl2=COALESCE($11,sell_sl2),
              gap_plan_status='READY',gap_planned_at=NOW()
         WHERE id=$1 AND gap_plan_status='WAITING_RANGE'",
    )
    .bind(snapshot.id)
    .bind(opening_range_high)
    .bind(opening_range_low)
    .bind(recovered_buy)
    .bind(buy_exits.map(|value| value.target))
    .bind(buy_exits.map(|value| value.sl1))
    .bind(buy_exits.map(|value| value.sl2))
    .bind(recovered_sell)
    .bind(sell_exits.map(|value| value.target))
    .bind(sell_exits.map(|value| value.sl1))
    .bind(sell_exits.map(|value| value.sl2))
    .execute(&state.db)
    .await?;
    let resolved = load_snapshot(state, &snapshot.instrument, date)
        .await?
        .ok_or_else(|| AppError::BadRequest("Resolved market snapshot is missing.".into()))?;
    emit(
        state,
        None,
        &snapshot.instrument,
        "opening_range_entry_ready",
        json!({
            "missed_entry_plan": missed.as_str(),
            "entry_direction": "BOTH",
            "entry_source": "OPENING_RANGE",
            "opening_range_high": opening_range_high,
            "opening_range_low": opening_range_low,
            "buy_entry": recovered_buy,
            "buy_target": buy_exits.map(|value| value.target),
            "buy_sl1": buy_exits.map(|value| value.sl1),
            "buy_sl2": buy_exits.map(|value| value.sl2),
            "sell_entry": recovered_sell,
            "sell_target": sell_exits.map(|value| value.target),
            "sell_sl1": sell_exits.map(|value| value.sl1),
            "sell_sl2": sell_exits.map(|value| value.sl2),
        }),
    )
    .await;
    Ok(resolved)
}

async fn load_contract_master(state: &AppState) -> AppResult<Arc<Vec<MasterContract>>> {
    contract_master::load(state).await
}

async fn ensure_supertrend_option_contract_metadata(
    state: &AppState,
    date: NaiveDate,
) -> AppResult<()> {
    let mut contracts = load_contract_master(state).await?;
    let mut refreshed = false;
    for instrument in ["SENSEX", "NIFTY"] {
        let Some(config) = index_option_config(instrument) else {
            continue;
        };
        let mut preview = supertrend_option_expiry_preview(&contracts, config, date);
        if preview.is_none() && !refreshed {
            contract_master::invalidate_cache().await;
            contracts = load_contract_master(state).await?;
            refreshed = true;
            preview = supertrend_option_expiry_preview(&contracts, config, date);
        }
        let Some((expiry, lot_size)) = preview else {
            return Err(AppError::BadRequest(format!(
                "No current {} option expiry is available in the refreshed Angel One contract master for {date}.",
                config.label
            )));
        };
        tracing::debug!(
            %date,
            %expiry,
            lot_size,
            instrument = config.instrument,
            "SuperTrend option contract metadata warmed"
        );
    }
    Ok(())
}

async fn index_ltp(state: &AppState, config: IndexOptionConfig) -> AppResult<f64> {
    let quote = shared_market_quote(
        state,
        "LTP",
        json!({config.index_exchange:[config.index_token]}),
    )
    .await?;
    find_quote_ltp(&quote).ok_or_else(|| {
        AppError::BadRequest(format!(
            "Angel One {} quote did not include LTP.",
            config.instrument
        ))
    })
}

async fn select_supertrend_atm_option_contract(
    state: &AppState,
    contracts: &[MasterContract],
    config: IndexOptionConfig,
    date: NaiveDate,
    side: IndexOptionSide,
    underlying_ltp: f64,
    excluded_tokens: &HashSet<String>,
) -> AppResult<Option<OptionContract>> {
    let mut candidates = supertrend_option_candidates(contracts, config, date, side);
    candidates.retain(|contract| !excluded_tokens.contains(&contract.token));
    candidates.sort_by(|left, right| {
        left.expiry
            .cmp(&right.expiry)
            .then_with(|| {
                (left.strike - underlying_ltp)
                    .abs()
                    .total_cmp(&(right.strike - underlying_ltp).abs())
            })
            .then_with(|| left.strike.total_cmp(&right.strike))
    });
    if candidates.is_empty() {
        return Ok(None);
    }

    let mut expiries: Vec<NaiveDate> = candidates.iter().map(|contract| contract.expiry).collect();
    expiries.sort();
    expiries.dedup();
    for expiry in expiries {
        let mut bucket: Vec<OptionContract> = candidates
            .iter()
            .filter(|contract| contract.expiry == expiry)
            .cloned()
            .collect();
        bucket.sort_by(|left, right| {
            (left.strike - underlying_ltp)
                .abs()
                .total_cmp(&(right.strike - underlying_ltp).abs())
                .then_with(|| left.strike.total_cmp(&right.strike))
        });
        for mut selected in bucket {
            match shared_market_quote(
                state,
                "LTP",
                json!({config.option_exchange:[selected.token.clone()]}),
            )
            .await
            {
                Ok(quote) => {
                    if let Some(premium) = quote_ltp_for_token(&quote, &selected.token) {
                        selected.premium = premium;
                        return Ok(Some(selected));
                    }
                    tracing::warn!(
                        instrument = config.instrument,
                        option_type = side.option_type(),
                        token = %selected.token,
                        symbol = %selected.symbol,
                        "skipping ATM option candidate without contract-token LTP"
                    );
                }
                Err(error) if angel::is_contract_unavailable_error(&error.to_string()) => {
                    tracing::warn!(
                        instrument = config.instrument,
                        option_type = side.option_type(),
                        token = %selected.token,
                        symbol = %selected.symbol,
                        error = %error,
                        "skipping unavailable ATM option candidate"
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }
    Ok(None)
}

async fn select_supertrend_market_for_signal(
    state: &AppState,
    config: IndexOptionConfig,
    side: IndexOptionSide,
    date: NaiveDate,
    signal_at: NaiveDateTime,
) -> AppResult<SuperTrendMarketSelection> {
    let underlying = index_ltp(state, config).await?;
    let excluded_tokens = HashSet::new();
    let contracts = load_contract_master(state).await?;
    let mut contract = select_supertrend_atm_option_contract(
        state,
        &contracts,
        config,
        date,
        side,
        underlying,
        &excluded_tokens,
    )
    .await?;
    if contract.is_none() {
        contract_master::invalidate_cache().await;
        let refreshed = load_contract_master(state).await?;
        contract = select_supertrend_atm_option_contract(
            state,
            &refreshed,
            config,
            date,
            side,
            underlying,
            &excluded_tokens,
        )
        .await?;
    }
    let contract = contract.ok_or_else(|| {
        AppError::BadRequest(format!(
            "No {} {} ATM option contract is available for {date}; Rulenix refreshed the Angel One contract master and could not find a quoteable contract.",
            config.instrument,
            side.option_type(),
        ))
    })?;
    risk::record_tick(
        state,
        config.option_exchange,
        &contract.token,
        contract.premium,
    )
    .await?;
    emit_for(
        state,
        SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
        None,
        config.instrument,
        "supertrend_atm_contract_selected",
        json!({"symbol":contract.symbol,"token":contract.token,"expiry":contract.expiry,"strike":contract.strike,"option_type":contract.option_type,"premium":contract.premium,"underlying_ltp":underlying,"signal_at":signal_at}),
    )
    .await;
    Ok(SuperTrendMarketSelection {
        contract,
        underlying_ltp: underlying,
    })
}

#[allow(clippy::too_many_arguments)]
async fn supertrend_option_snapshot_for_signal(
    state: &AppState,
    config: IndexOptionConfig,
    side: IndexOptionSide,
    date: NaiveDate,
    signal_at: NaiveDateTime,
    user_id: Uuid,
    target_points: f64,
    stop_loss_points: f64,
    selection: &SuperTrendMarketSelection,
) -> AppResult<Snapshot> {
    let contract = &selection.contract;
    let id = Uuid::new_v4();
    let option_instrument = config.option_instrument(side);
    let execution_key = format!(
        "{}-{}-{}",
        signal_at.format("%Y%m%d%H%M"),
        contract.symbol,
        user_id.simple()
    );
    let now = Utc::now();
    let (buy_target, buy_sl1, sell_target, sell_sl1) = match side {
        IndexOptionSide::Call => (
            Some(target_points),
            Some(stop_loss_points),
            None::<f64>,
            None::<f64>,
        ),
        IndexOptionSide::Put => (
            None::<f64>,
            None::<f64>,
            Some(target_points),
            Some(stop_loss_points),
        ),
    };
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,buy_target,buy_sl1,sell_target,sell_sl1,previous_close,fetched_at) VALUES ($1,$2,$3,$4,'ready','',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,NOW()) ON CONFLICT (strategy_key,instrument,trade_date,execution_key) DO UPDATE SET status='ready',error='',contract_token=EXCLUDED.contract_token,contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,exchange_segment=EXCLUDED.exchange_segment,product_type=EXCLUDED.product_type,underlying_token=EXCLUDED.underlying_token,buy_target=EXCLUDED.buy_target,buy_sl1=EXCLUDED.buy_sl1,sell_target=EXCLUDED.sell_target,sell_sl1=EXCLUDED.sell_sl1,previous_close=EXCLUDED.previous_close,fetched_at=NOW()")
        .bind(id)
        .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
        .bind(&option_instrument)
        .bind(date)
        .bind(&contract.token)
        .bind(&contract.symbol)
        .bind(contract.expiry)
        .bind(contract.lot_size)
        .bind(config.option_exchange)
        .bind(OPTION_PRODUCT_TYPE)
        .bind(&execution_key)
        .bind(config.index_token)
        .bind(buy_target)
        .bind(buy_sl1)
        .bind(sell_target)
        .bind(sell_sl1)
        .bind(selection.underlying_ltp)
        .execute(&state.db)
        .await?;
    let query = format!(
        "{} WHERE strategy_key=$1 AND instrument=$2 AND trade_date=$3 AND execution_key=$4",
        snapshot_select()
    );
    let mut snapshot: Snapshot = sqlx::query_as(&query)
        .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
        .bind(&option_instrument)
        .bind(date)
        .bind(&execution_key)
        .fetch_one(&state.db)
        .await?;
    snapshot.fetched_at = now;
    Ok(snapshot)
}

async fn supertrend_retry_snapshot(
    state: &AppState,
    original: &Snapshot,
    user_id: Uuid,
) -> AppResult<Option<(Snapshot, f64)>> {
    let Some(underlying_name) = supertrend_snapshot_underlying(&original.instrument) else {
        return Ok(None);
    };
    let Some(config) = index_option_config(underlying_name) else {
        return Ok(None);
    };
    let Some(side) = supertrend_snapshot_side(&original.instrument) else {
        return Ok(None);
    };
    let Some((target_points, stop_loss_points)) = supertrend_config_points(original) else {
        return Ok(None);
    };
    let old_token = original.contract_token.clone().unwrap_or_default();
    let mut excluded_tokens = HashSet::new();
    if !old_token.trim().is_empty() {
        excluded_tokens.insert(old_token);
    }
    contract_master::invalidate_cache().await;
    let underlying = index_ltp(state, config).await?;
    let contracts = load_contract_master(state).await?;
    let Some(contract) = select_supertrend_atm_option_contract(
        state,
        &contracts,
        config,
        original.trade_date,
        side,
        underlying,
        &excluded_tokens,
    )
    .await?
    else {
        return Ok(None);
    };
    risk::record_tick(
        state,
        config.option_exchange,
        &contract.token,
        contract.premium,
    )
    .await?;
    let id = Uuid::new_v4();
    let execution_key = format!(
        "{}-retry-{}",
        contract.symbol,
        &user_id.simple().to_string()[..8]
    );
    let (buy_target, buy_sl1, sell_target, sell_sl1) = match side {
        IndexOptionSide::Call => (
            Some(target_points),
            Some(stop_loss_points),
            None::<f64>,
            None::<f64>,
        ),
        IndexOptionSide::Put => (
            None::<f64>,
            None::<f64>,
            Some(target_points),
            Some(stop_loss_points),
        ),
    };
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,buy_target,buy_sl1,sell_target,sell_sl1,previous_close,fetched_at) VALUES ($1,$2,$3,$4,'ready','',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,NOW()) ON CONFLICT (strategy_key,instrument,trade_date,execution_key) DO UPDATE SET status='ready',error='',contract_token=EXCLUDED.contract_token,contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,exchange_segment=EXCLUDED.exchange_segment,product_type=EXCLUDED.product_type,underlying_token=EXCLUDED.underlying_token,buy_target=EXCLUDED.buy_target,buy_sl1=EXCLUDED.buy_sl1,sell_target=EXCLUDED.sell_target,sell_sl1=EXCLUDED.sell_sl1,previous_close=EXCLUDED.previous_close,fetched_at=NOW()")
        .bind(id)
        .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
        .bind(&original.instrument)
        .bind(original.trade_date)
        .bind(&contract.token)
        .bind(&contract.symbol)
        .bind(contract.expiry)
        .bind(contract.lot_size)
        .bind(config.option_exchange)
        .bind(OPTION_PRODUCT_TYPE)
        .bind(&execution_key)
        .bind(config.index_token)
        .bind(buy_target)
        .bind(buy_sl1)
        .bind(sell_target)
        .bind(sell_sl1)
        .bind(underlying)
        .execute(&state.db)
        .await?;
    let query = format!(
        "{} WHERE strategy_key=$1 AND instrument=$2 AND trade_date=$3 AND execution_key=$4",
        snapshot_select()
    );
    let snapshot = sqlx::query_as(&query)
        .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
        .bind(&original.instrument)
        .bind(original.trade_date)
        .bind(&execution_key)
        .fetch_one(&state.db)
        .await?;
    Ok(Some((snapshot, contract.premium)))
}

async fn supertrend_retry_contract_snapshot(
    state: &AppState,
    snapshot: &Snapshot,
    user_id: Uuid,
) -> AppResult<Option<(Snapshot, f64)>> {
    if snapshot.strategy_key == SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY {
        supertrend_retry_snapshot(state, snapshot, user_id).await
    } else {
        Ok(None)
    }
}

fn is_supertrend_index_token(exchange: &str, token: &str) -> bool {
    (exchange.eq_ignore_ascii_case("BSE") && token == SENSEX_INDEX_TOKEN)
        || (exchange.eq_ignore_ascii_case("NSE") && token == NIFTY_INDEX_TOKEN)
}

async fn record_supertrend_index_tick(
    state: &AppState,
    exchange: &str,
    token: &str,
    price: f64,
    tick_at: DateTime<Utc>,
) {
    if !price.is_finite() || price <= 0.0 || !is_supertrend_index_token(exchange, token) {
        return;
    }
    let offset = FixedOffset::east_opt(19_800).expect("valid IST offset");
    let local = tick_at.with_timezone(&offset);
    let minute = local.hour() * 60 + local.minute();
    if matches!(local.weekday(), Weekday::Sat | Weekday::Sun)
        || !(SUPERTREND_ENTRY_START_MINUTE..=OPTION_SCHEDULER_END_MINUTE).contains(&minute)
    {
        return;
    }
    let bucket_epoch = tick_at.timestamp().div_euclid(300) * 300;
    let tick_epoch_ms = tick_at.timestamp_millis();
    let key = (exchange.to_uppercase(), token.to_owned(), bucket_epoch);
    let mut candles = state.live_index_candles.lock().await;
    candles
        .entry(key)
        .and_modify(|candle| {
            candle.high = candle.high.max(price);
            candle.low = candle.low.min(price);
            if tick_epoch_ms < candle.first_tick_epoch_ms {
                candle.first_tick_epoch_ms = tick_epoch_ms;
                candle.open = price;
            }
            if tick_epoch_ms >= candle.last_tick_epoch_ms {
                candle.last_tick_epoch_ms = tick_epoch_ms;
                candle.close = price;
            }
        })
        .or_insert(LiveIndexCandle {
            bucket_epoch,
            first_tick_epoch_ms: tick_epoch_ms,
            last_tick_epoch_ms: tick_epoch_ms,
            open: price,
            high: price,
            low: price,
            close: price,
        });
    let oldest = bucket_epoch - 2 * 24 * 60 * 60;
    candles.retain(|(_, _, bucket), _| *bucket >= oldest);
}

fn live_index_candle_is_complete(candle: LiveIndexCandle) -> bool {
    let bucket_ms = candle.bucket_epoch * 1_000;
    candle.first_tick_epoch_ms <= bucket_ms + 30_000
        && candle.last_tick_epoch_ms >= bucket_ms + 270_000
}

async fn flush_completed_live_index_candles(
    state: &AppState,
    config: IndexOptionConfig,
    through: NaiveDateTime,
) -> AppResult<usize> {
    let through_epoch = ist_naive_to_utc(through)?.timestamp();
    let exchange = config.index_exchange.to_owned();
    let token = config.index_token.to_owned();
    let ready: Vec<((String, String, i64), LiveIndexCandle)> = {
        let candles = state.live_index_candles.lock().await;
        candles
            .iter()
            .filter(|((item_exchange, item_token, bucket), candle)| {
                item_exchange == &exchange
                    && item_token == &token
                    && *bucket <= through_epoch
                    && live_index_candle_is_complete(**candle)
            })
            .map(|(key, candle)| (key.clone(), *candle))
            .collect()
    };
    if ready.is_empty() {
        return Ok(0);
    }
    let offset = FixedOffset::east_opt(19_800).expect("valid IST offset");
    let candles: Vec<IntradayCandle> = ready
        .iter()
        .filter_map(|(_, candle)| {
            DateTime::<Utc>::from_timestamp(candle.bucket_epoch, 0).map(|at| IntradayCandle {
                at: at.with_timezone(&offset).naive_local(),
                open: candle.open,
                high: candle.high,
                low: candle.low,
                close: candle.close,
            })
        })
        .collect();
    cache_index_candles(state, config, &candles).await?;
    let mut live = state.live_index_candles.lock().await;
    for (key, _) in &ready {
        live.remove(key);
    }
    Ok(candles.len())
}

fn supertrend_session_candles(
    candles: Vec<IntradayCandle>,
    today: NaiveDate,
    latest_expected: NaiveDateTime,
) -> Result<(Vec<IntradayCandle>, NaiveDate), String> {
    let market_open = NaiveTime::from_hms_opt(9, 15, 0).expect("valid market open");
    let market_close = NaiveTime::from_hms_opt(15, 30, 0).expect("valid market close");
    let mut candles: Vec<IntradayCandle> = candles
        .into_iter()
        .filter(|candle| {
            candle.at <= latest_expected
                && candle.at.time() >= market_open
                && candle.at.time() < market_close
        })
        .collect();
    candles.sort_by_key(|candle| candle.at);
    candles.dedup_by_key(|candle| candle.at);

    let previous_session = candles
        .iter()
        .filter_map(|candle| (candle.at.date() < today).then_some(candle.at.date()))
        .max()
        .ok_or_else(|| "previous trading-session candles are missing".to_owned())?;
    let previous_count = candles
        .iter()
        .filter(|candle| candle.at.date() == previous_session)
        .count();
    if previous_count < SUPERTREND_ATR_PERIOD + 2 {
        return Err(format!(
            "previous trading session {previous_session} has only {previous_count} candles"
        ));
    }

    if latest_expected.date() == today && latest_expected.time() >= market_open {
        let mut expected = today.and_time(market_open);
        while expected <= latest_expected {
            if candles
                .binary_search_by_key(&expected, |candle| candle.at)
                .is_err()
            {
                return Err(format!(
                    "completed index candle {expected} is not available yet"
                ));
            }
            expected += Duration::minutes(5);
        }
    }
    Ok((candles, previous_session))
}

async fn index_candles(
    state: &AppState,
    config: IndexOptionConfig,
    lookback: Duration,
    to: DateTime<FixedOffset>,
) -> AppResult<Vec<IntradayCandle>> {
    let to_candle = option_latest_completed_candle_time(to);
    let from_candle = to_candle - lookback;
    let mut last_error = String::new();
    for attempt in 0..SUPERTREND_CANDLE_RETRY_ATTEMPTS {
        flush_completed_live_index_candles(state, config, to_candle).await?;
        let candles = cached_index_candles(state, config, from_candle, to_candle).await?;
        match supertrend_session_candles(candles, to.date_naive(), to_candle) {
            Ok((candles, previous_session)) => {
                tracing::debug!(
                    instrument = config.instrument,
                    %previous_session,
                    latest_candle = %to_candle,
                    candle_count = candles.len(),
                    "continuous SuperTrend candle window ready"
                );
                return Ok(candles);
            }
            Err(error) => last_error = error,
        }
        if attempt + 1 < SUPERTREND_CANDLE_RETRY_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    Err(AppError::BadRequest(format!(
        "{} SuperTrend candle continuity check failed: {last_error}",
        config.instrument
    )))
}

fn option_latest_completed_candle_time(now: DateTime<FixedOffset>) -> NaiveDateTime {
    let minute = now.hour() * 60 + now.minute();
    let rounded = minute - (minute % 5);
    let latest_minute = rounded.saturating_sub(5);
    now.date_naive()
        .and_hms_opt(latest_minute / 60, latest_minute % 60, 0)
        .expect("valid option candle time")
}

fn ist_naive_to_utc(value: NaiveDateTime) -> AppResult<DateTime<Utc>> {
    FixedOffset::east_opt(19_800)
        .expect("valid IST offset")
        .from_local_datetime(&value)
        .single()
        .map(|value| value.with_timezone(&Utc))
        .ok_or_else(|| AppError::BadRequest("Invalid IST candle timestamp.".into()))
}

async fn load_cached_index_candles(
    state: &AppState,
    config: IndexOptionConfig,
    from_utc: DateTime<Utc>,
    to_utc: DateTime<Utc>,
) -> AppResult<Vec<IntradayCandle>> {
    let rows: Vec<(DateTime<Utc>, f64, f64, f64, f64)> = sqlx::query_as(
        "SELECT candle_time,open_price,high_price,low_price,close_price
         FROM backtest_market_candles
         WHERE exchange=$1 AND symbol_token=$2 AND interval_key=$3
           AND candle_time BETWEEN $4 AND $5
         ORDER BY candle_time",
    )
    .bind(config.index_exchange)
    .bind(config.index_token)
    .bind(OPTION_INTERVAL)
    .bind(from_utc)
    .bind(to_utc)
    .fetch_all(&state.db)
    .await?;
    let offset = FixedOffset::east_opt(19_800).expect("valid IST offset");
    Ok(rows
        .into_iter()
        .map(|(at, open, high, low, close)| IntradayCandle {
            at: at.with_timezone(&offset).naive_local(),
            open,
            high,
            low,
            close,
        })
        .collect())
}

async fn cache_index_candles(
    state: &AppState,
    config: IndexOptionConfig,
    candles: &[IntradayCandle],
) -> AppResult<()> {
    for candle in candles {
        let candle_time = ist_naive_to_utc(candle.at)?;
        sqlx::query(
            "INSERT INTO backtest_market_candles
             (id,exchange,instrument,symbol_token,trading_symbol,interval_key,candle_time,open_price,high_price,low_price,close_price,volume)
             VALUES ($1,$2,$3,$4,$3,$5,$6,$7,$8,$9,$10,0)
             ON CONFLICT (exchange,symbol_token,interval_key,candle_time)
             DO UPDATE SET instrument=EXCLUDED.instrument,trading_symbol=EXCLUDED.trading_symbol,
                open_price=EXCLUDED.open_price,high_price=EXCLUDED.high_price,
                low_price=EXCLUDED.low_price,close_price=EXCLUDED.close_price,fetched_at=NOW()",
        )
        .bind(Uuid::new_v4())
        .bind(config.index_exchange)
        .bind(config.instrument)
        .bind(config.index_token)
        .bind(OPTION_INTERVAL)
        .bind(candle_time)
        .bind(candle.open)
        .bind(candle.high)
        .bind(candle.low)
        .bind(candle.close)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

async fn cached_index_candles(
    state: &AppState,
    config: IndexOptionConfig,
    from_candle: NaiveDateTime,
    to_candle: NaiveDateTime,
) -> AppResult<Vec<IntradayCandle>> {
    let from_utc = ist_naive_to_utc(from_candle)?;
    let to_utc = ist_naive_to_utc(to_candle)?;
    let max_cached: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT MAX(candle_time)
         FROM backtest_market_candles
         WHERE exchange=$1 AND symbol_token=$2 AND interval_key=$3
           AND candle_time BETWEEN $4 AND $5",
    )
    .bind(config.index_exchange)
    .bind(config.index_token)
    .bind(OPTION_INTERVAL)
    .bind(from_utc)
    .bind(to_utc)
    .fetch_one(&state.db)
    .await?;

    let fetch_from = max_cached
        .filter(|cached| *cached >= from_utc)
        .map(|cached| {
            cached
                .with_timezone(&FixedOffset::east_opt(19_800).expect("valid IST offset"))
                .naive_local()
                + Duration::minutes(5)
        })
        .unwrap_or(from_candle);

    let mut fetch_error: Option<String> = None;
    if fetch_from <= to_candle
        && !shared_historical_cooldown_active(
            state,
            config.index_exchange,
            config.index_token,
            OPTION_INTERVAL,
        )
        .await
    {
        let fetched = shared_market_candles(
            state,
            config.index_exchange,
            config.index_token,
            OPTION_INTERVAL,
            &format!("{}", fetch_from.format("%Y-%m-%d %H:%M")),
            &format!("{}", to_candle.format("%Y-%m-%d %H:%M")),
        )
        .await;
        match fetched {
            Ok(raw) => {
                let candles = parse_intraday_candles(&raw);
                cache_index_candles(state, config, &candles).await?;
            }
            Err(error) => {
                let error_text = error.to_string();
                if angel::is_rate_limit_error(&error.to_string()) {
                    activate_shared_historical_cooldown(
                        state,
                        config.index_exchange,
                        config.index_token,
                        OPTION_INTERVAL,
                    )
                    .await;
                }
                fetch_error = Some(error_text.clone());
                tracing::warn!(
                    instrument = config.instrument,
                    error = %error_text,
                    "Angel historical fetch failed; falling back to cached index candles if available"
                );
            }
        }
    }

    let candles = load_cached_index_candles(state, config, from_utc, to_utc).await?;
    if candles.is_empty() {
        return Err(AppError::BadRequest(match fetch_error {
            Some(error) => format!(
                "No cached {} candles are available for SuperTrend after Angel historical fetch failed: {error}",
                config.instrument
            ),
            None => format!(
                "No cached or broker-returned {} candles are available for SuperTrend.",
                config.instrument
            ),
        }));
    }
    if let Some(error) = fetch_error
        && let Some(latest) = candles.last()
    {
        tracing::warn!(
            instrument = config.instrument,
            latest_candle = %latest.at,
            requested_to = %to_candle,
            %error,
            "using cached index candles after Angel historical fetch failed"
        );
    }
    Ok(candles)
}

async fn option_execution_ltp(state: &AppState, snapshot: &Snapshot) -> AppResult<f64> {
    let token = snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Option snapshot has no contract token.".into()))?;
    let mut token_map = serde_json::Map::new();
    token_map.insert(snapshot.exchange_segment.clone(), json!([token]));
    let quote = shared_market_quote(state, "LTP", Value::Object(token_map)).await?;
    let ltp = quote_ltp_for_token(&quote, token).ok_or_else(|| {
        let contract = snapshot
            .contract_symbol
            .as_deref()
            .unwrap_or(&snapshot.instrument);
        AppError::BadRequest(format!(
            "Angel One option quote for {contract} did not include contract-token LTP."
        ))
    })?;
    risk::record_tick(state, &snapshot.exchange_segment, token, ltp).await?;
    Ok(ltp)
}

async fn refresh_snapshot_market_tick(state: &AppState, snapshot: &Snapshot) -> AppResult<()> {
    let token = snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Snapshot has no contract token.".into()))?;
    let has_recent_tick: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM market_price_ticks WHERE exchange_segment=$1 AND contract_token=$2 AND received_at>NOW()-INTERVAL '5 seconds')",
    )
    .bind(&snapshot.exchange_segment)
    .bind(token)
    .fetch_one(&state.db)
    .await?;
    if has_recent_tick {
        return Ok(());
    }

    let mut token_map = serde_json::Map::new();
    token_map.insert(snapshot.exchange_segment.clone(), json!([token]));
    let quote = shared_market_quote(state, "LTP", Value::Object(token_map)).await?;
    let ltp = quote_ltp_for_token(&quote, token).ok_or_else(|| {
        let contract = snapshot
            .contract_symbol
            .as_deref()
            .unwrap_or(&snapshot.instrument);
        AppError::BadRequest(format!(
            "Angel One quote for {contract} did not include contract-token LTP."
        ))
    })?;
    risk::record_tick(state, &snapshot.exchange_segment, token, ltp).await?;
    Ok(())
}

async fn record_snapshot_failure(state: &AppState, instrument: &str, date: NaiveDate, error: &str) {
    if let Err(database_error) = sqlx::query("UPDATE strategy_market_snapshots SET status='failed',error=$4,fetched_at=NOW() WHERE strategy_key=$1 AND instrument=$2 AND trade_date=$3 AND status<>'ready'")
        .bind(STRATEGY_KEY).bind(instrument).bind(date).bind(error).execute(&state.db).await {
        tracing::warn!(%database_error, "could not persist market snapshot failure");
    }
}

#[derive(Debug, Clone, FromRow)]
pub(crate) struct Runner {
    pub user_id: Uuid,
    pub username: String,
    pub instrument: String,
    pub lots: i32,
    pub run_day_session: bool,
    pub run_evening_session: bool,
    pub trading_mode: String,
}

#[derive(Debug, Clone)]
pub(crate) struct NewOrder {
    pub role: &'static str,
    pub side: &'static str,
    pub order_type: &'static str,
    pub lots: i32,
    pub price: f64,
    pub trigger: Option<f64>,
    pub trade_id: Option<Uuid>,
    pub quantity: Option<i32>,
}

#[derive(Debug, Clone)]
struct PreparedExecutionIntent {
    user_id: Uuid,
    snapshot_id: Uuid,
    strategy_key: String,
    instrument: String,
    session_key: String,
    action: &'static str,
    role: &'static str,
    side: &'static str,
    order_type: &'static str,
    lots: i32,
    quantity: Option<i32>,
    price: f64,
    trigger_price: Option<f64>,
    trade_id: Option<Uuid>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
struct ExecutionIntent {
    id: Uuid,
    signal_id: Uuid,
    user_id: Uuid,
    snapshot_id: Option<Uuid>,
    trade_id: Option<Uuid>,
    strategy_key: String,
    instrument: String,
    session_key: String,
    action: String,
    role: String,
    side: String,
    order_type: String,
    lots: i32,
    quantity: Option<i32>,
    price: f64,
    trigger_price: Option<f64>,
    attempts: i32,
    expires_at: Option<DateTime<Utc>>,
}

fn execution_intent_columns() -> &'static str {
    "id,signal_id,user_id,snapshot_id,trade_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,trigger_price,attempts,expires_at"
}

fn intent_static_role(value: &str) -> Option<&'static str> {
    match value {
        "BUY_ENTRY" => Some("BUY_ENTRY"),
        "SELL_ENTRY" => Some("SELL_ENTRY"),
        "TARGET" => Some("TARGET"),
        "SL1" => Some("SL1"),
        "SL2" => Some("SL2"),
        _ => None,
    }
}

fn intent_static_side(value: &str) -> Option<&'static str> {
    match value {
        "BUY" => Some("BUY"),
        "SELL" => Some("SELL"),
        _ => None,
    }
}

fn intent_static_order_type(value: &str) -> Option<&'static str> {
    match value {
        "MARKET" => Some("MARKET"),
        "LIMIT" => Some("LIMIT"),
        "STOPLOSS_LIMIT" => Some("STOPLOSS_LIMIT"),
        "STOPLOSS_MARKET" => Some("STOPLOSS_MARKET"),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn materialize_signal_intents(
    state: &AppState,
    strategy_key: &str,
    instrument: &str,
    session_key: &str,
    signal_type: &str,
    signal_at: DateTime<Utc>,
    snapshot_id: Option<Uuid>,
    payload: Value,
    intents: &[PreparedExecutionIntent],
) -> AppResult<(Uuid, bool)> {
    let mut tx = state.db.begin().await?;
    let signal_id = Uuid::new_v4();
    let expected_users = intents
        .iter()
        .map(|intent| intent.user_id)
        .collect::<HashSet<_>>()
        .len() as i32;
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO strategy_signals(id,strategy_key,instrument,session_key,signal_at,snapshot_id,signal_type,expected_users,payload)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)
         ON CONFLICT(strategy_key,instrument,session_key,signal_type) DO NOTHING
         RETURNING id",
    )
    .bind(signal_id)
    .bind(strategy_key)
    .bind(instrument)
    .bind(session_key)
    .bind(signal_at)
    .bind(snapshot_id)
    .bind(signal_type)
    .bind(expected_users)
    .bind(payload)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(signal_id) = inserted else {
        let existing: Uuid = sqlx::query_scalar(
            "SELECT id FROM strategy_signals WHERE strategy_key=$1 AND instrument=$2 AND session_key=$3 AND signal_type=$4",
        )
        .bind(strategy_key)
        .bind(instrument)
        .bind(session_key)
        .bind(signal_type)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok((existing, false));
    };
    for intent in intents {
        sqlx::query(
            "INSERT INTO strategy_execution_intents(id,signal_id,user_id,snapshot_id,trade_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,trigger_price,expires_at)
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)
             ON CONFLICT DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(signal_id)
        .bind(intent.user_id)
        .bind(intent.snapshot_id)
        .bind(intent.trade_id)
        .bind(&intent.strategy_key)
        .bind(&intent.instrument)
        .bind(&intent.session_key)
        .bind(intent.action)
        .bind(intent.role)
        .bind(intent.side)
        .bind(intent.order_type)
        .bind(intent.lots)
        .bind(intent.quantity)
        .bind(intent.price)
        .bind(intent.trigger_price)
        .bind(intent.expires_at)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE strategy_signals SET status='dispatching',updated_at=NOW() WHERE id=$1")
        .bind(signal_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((signal_id, true))
}

async fn eligible_runner_for_entry_intent(
    state: &AppState,
    intent: &ExecutionIntent,
) -> AppResult<Option<Runner>> {
    Ok(sqlx::query_as(
        "SELECT c.user_id,u.username,c.instrument,c.lots,c.run_day_session,c.run_evening_session,p.trading_mode
         FROM user_strategy_configs c
         JOIN user_strategy_activations a ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key
         JOIN users u ON u.id=c.user_id
         JOIN user_profiles p ON p.user_id=c.user_id
         WHERE c.user_id=$1 AND c.strategy_key=$2 AND c.instrument=$3
           AND c.enabled=TRUE AND a.is_active=TRUE AND u.is_active=TRUE
           AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))",
    )
    .bind(intent.user_id)
    .bind(&intent.strategy_key)
    .bind(&intent.instrument)
    .fetch_optional(&state.db)
    .await?)
}

fn entry_intent_retryable(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    angel::is_rate_limit_error(message)
        || angel::is_authentication_error(message)
        || angel::is_contract_unavailable_error(message)
        || [
            "temporar",
            "unavailable",
            "timeout",
            "connection",
            "market data",
            "no fresh valid market price",
            "database",
            "deadlock",
            "could not be reached",
        ]
        .iter()
        .any(|phrase| lower.contains(phrase))
}

async fn execute_entry_intent(state: &AppState, intent: ExecutionIntent) {
    let result: AppResult<Option<(Uuid, String, String)>> = async {
        if intent.action != "ENTRY" {
            return Err(AppError::BadRequest("Unsupported execution intent action.".into()));
        }
        if intent.expires_at.is_some_and(|expires| expires <= Utc::now()) {
            sqlx::query("UPDATE strategy_execution_intents SET status='expired',last_error='Signal execution window expired before order submission.',completed_at=NOW(),updated_at=NOW() WHERE id=$1")
                .bind(intent.id).execute(&state.db).await?;
            return Ok(None);
        }
        let snapshot_id = intent.snapshot_id.ok_or_else(|| AppError::BadRequest("Execution intent has no strategy snapshot.".into()))?;
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query).bind(snapshot_id).fetch_one(&state.db).await?;
        if intent.strategy_key == STRATEGY_KEY {
            let open_position = user_has_breakout_open_position(state,intent.user_id,&intent.instrument).await?;
            let other_active_entry: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
                 WHERE o.user_id=$1 AND s.strategy_key=$2 AND s.instrument=$3 AND (o.session_key<>$4 OR s.trade_date<>$5)
                   AND o.role IN ('BUY_ENTRY','SELL_ENTRY') AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))",
            )
            .bind(intent.user_id).bind(STRATEGY_KEY).bind(&intent.instrument).bind(&intent.session_key).bind(snapshot.trade_date)
            .fetch_one(&state.db).await?;
            if open_position || other_active_entry {
                sqlx::query("UPDATE strategy_execution_intents SET status='skipped',last_error=$2,completed_at=NOW(),updated_at=NOW() WHERE id=$1")
                    .bind(intent.id)
                    .bind(if open_position { "An open Futures Breakout position already exists for this instrument." } else { "Another Futures Breakout entry batch is already active for this instrument." })
                    .execute(&state.db).await?;
                return Ok(None);
            }
        }
        let runner = if intent.strategy_key == SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY {
            let Some(config) = index_option_config(&intent.instrument) else {
                return Err(AppError::BadRequest("SuperTrend intent has an invalid underlying instrument.".into()));
            };
            let supertrend_runner: Option<SuperTrendRunner> = sqlx::query_as(
                "SELECT c.user_id,u.username,c.instrument,c.lots,c.run_day_session,c.run_evening_session,p.trading_mode,
                        CASE WHEN c.target_points>0 THEN c.target_points ELSE $4 END AS target_points,
                        CASE WHEN c.stop_loss_points>0 THEN c.stop_loss_points ELSE $5 END AS stop_loss_points
                 FROM user_strategy_configs c
                 JOIN user_strategy_activations a ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key
                 JOIN users u ON u.id=c.user_id JOIN user_profiles p ON p.user_id=c.user_id
                 WHERE c.user_id=$1 AND c.strategy_key=$2 AND c.instrument=$3 AND c.enabled=TRUE AND a.is_active=TRUE AND u.is_active=TRUE
                   AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))",
            )
            .bind(intent.user_id)
            .bind(&intent.strategy_key)
            .bind(&intent.instrument)
            .bind(config.default_target_points)
            .bind(config.default_stop_loss_points)
            .fetch_optional(&state.db)
            .await?;
            let Some(supertrend_runner) = supertrend_runner else {
                sqlx::query("UPDATE strategy_execution_intents SET status='skipped',last_error='User, strategy, or instrument was no longer eligible at execution time.',completed_at=NOW(),updated_at=NOW() WHERE id=$1")
                    .bind(intent.id).execute(&state.db).await?;
                return Ok(None);
            };
            let signal_side = supertrend_snapshot_side(&snapshot.instrument).ok_or_else(|| AppError::BadRequest("SuperTrend intent snapshot has an invalid option side.".into()))?;
            let opposite = signal_side.opposite();
            cancel_supertrend_active_entries_for_side(state,intent.user_id,&intent.instrument,opposite,"SuperTrend reversal confirmed; cancelling stale opposite entry.").await?;
            close_supertrend_open_trades_for_side(state,&supertrend_runner,config,opposite,ist_now(),"SuperTrend reversal confirmed.").await?;
            if user_has_supertrend_side_exposure(state,intent.user_id,&intent.instrument,opposite).await? {
                return Err(AppError::BadRequest("Temporarily waiting for the opposite SuperTrend broker position to be confirmed flat before replacement entry.".into()));
            }
            if user_has_supertrend_side_exposure(state,intent.user_id,&intent.instrument,signal_side).await? {
                sqlx::query("UPDATE strategy_execution_intents SET status='skipped',last_error='A same-side SuperTrend position is already open.',completed_at=NOW(),updated_at=NOW() WHERE id=$1")
                    .bind(intent.id).execute(&state.db).await?;
                return Ok(None);
            }
            Runner::from(supertrend_runner)
        } else {
            let Some(runner) = eligible_runner_for_entry_intent(state, &intent).await? else {
                sqlx::query("UPDATE strategy_execution_intents SET status='skipped',last_error='User, strategy, or instrument was no longer eligible at execution time.',completed_at=NOW(),updated_at=NOW() WHERE id=$1")
                    .bind(intent.id).execute(&state.db).await?;
                return Ok(None);
            };
            runner
        };
        let role = intent_static_role(&intent.role).ok_or_else(|| AppError::BadRequest("Execution intent has an invalid role.".into()))?;
        let side = intent_static_side(&intent.side).ok_or_else(|| AppError::BadRequest("Execution intent has an invalid side.".into()))?;
        let order_type = intent_static_order_type(&intent.order_type).ok_or_else(|| AppError::BadRequest("Execution intent has an invalid order type.".into()))?;
        let signal_at: DateTime<Utc> = sqlx::query_scalar(
            "SELECT signal_at FROM strategy_signals WHERE id=$1",
        )
        .bind(intent.signal_id)
        .fetch_one(&state.db)
        .await?;
        place_strategy_order_for_signal(state, &runner, &snapshot, &intent.session_key, NewOrder {
            role, side, order_type, lots: intent.lots, price: intent.price,
            trigger: intent.trigger_price, trade_id: intent.trade_id, quantity: intent.quantity,
        }, signal_at).await?;
        let order: Option<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id,status,broker_status FROM strategy_orders WHERE user_id=$1 AND role=$2 AND (session_key=$3 OR session_key LIKE $3 || ':%') ORDER BY created_at DESC LIMIT 1",
        )
        .bind(intent.user_id).bind(&intent.role).bind(&intent.session_key)
        .fetch_optional(&state.db).await?;
        Ok(order)
    }.await;
    match result {
        Ok(order) => {
            let (order_id, status, detail) = match order {
                Some((order_id, order_status, broker_status)) => {
                    let status = match order_status.as_str() {
                        "filled" => "completed",
                        "cancelled" => "skipped",
                        "failed" | "rejected" => "failed",
                        _ => "submitted",
                    };
                    (Some(order_id), status, broker_status)
                }
                None => (None, "claimed", String::new()),
            };
            if let Err(error) = sqlx::query("UPDATE strategy_execution_intents SET status=$3,strategy_order_id=$2,last_error=$4,completed_at=CASE WHEN $3 IN ('completed','skipped','failed') THEN NOW() ELSE completed_at END,updated_at=NOW() WHERE id=$1 AND status='claimed'")
                .bind(intent.id).bind(order_id).bind(status).bind(detail).execute(&state.db).await {
                tracing::warn!(intent_id=%intent.id,%error,"could not complete strategy execution intent");
            }
        }
        Err(error) => {
            let message = error.to_string();
            let retryable = entry_intent_retryable(&message)
                && intent.attempts < 12
                && intent.expires_at.is_none_or(|expires| expires > Utc::now());
            let mut delay = recoverable_retry_delay_seconds(&message, intent.attempts);
            if let Some(expires_at) = intent.expires_at {
                let remaining = (expires_at - Utc::now()).num_seconds().max(15) as i32;
                delay = delay.min((remaining / 3).max(15));
            }
            let status = if retryable { "retry_wait" } else { "failed" };
            if let Err(database_error) = sqlx::query("UPDATE strategy_execution_intents SET status=$2,next_attempt_at=CASE WHEN $2='retry_wait' THEN NOW()+($3::text || ' seconds')::interval ELSE next_attempt_at END,last_error=$4,completed_at=CASE WHEN $2='failed' THEN NOW() ELSE NULL END,updated_at=NOW() WHERE id=$1")
                .bind(intent.id).bind(status).bind(delay).bind(&message).execute(&state.db).await {
                tracing::warn!(intent_id=%intent.id,%database_error,"could not persist execution intent failure");
            }
            operational_alert_for(
                state,
                &intent.strategy_key,
                Some(intent.user_id),
                &intent.instrument,
                "execution_intent_failed",
                retry_alert_severity(&message),
                &format!(
                    "{} {} execution {}: {}",
                    intent.instrument,
                    intent.role,
                    if retryable {
                        "will retry automatically"
                    } else {
                        "failed"
                    },
                    message
                ),
            )
            .await;
        }
    }
}

async fn process_execution_intents(state: &AppState, signal_id: Option<Uuid>) -> AppResult<usize> {
    sqlx::query("UPDATE strategy_execution_intents SET status='retry_wait',next_attempt_at=NOW(),last_error='Execution claim heartbeat expired before completion; safe retry queued.',updated_at=NOW() WHERE action='ENTRY' AND status='claimed' AND claimed_at<NOW()-INTERVAL '2 minutes'")
        .execute(&state.db).await?;
    sqlx::query(
        "UPDATE strategy_execution_intents i SET
           status=CASE
             WHEN o.status='filled' THEN 'completed'
             WHEN o.status='failed' AND i.attempts<12 AND (i.expires_at IS NULL OR i.expires_at>NOW()) THEN 'retry_wait'
             WHEN o.status='cancelled' THEN 'skipped'
             WHEN o.status='rejected' THEN 'failed'
             ELSE i.status END,
           next_attempt_at=CASE WHEN o.status='failed' THEN NOW() ELSE i.next_attempt_at END,
           last_error=CASE WHEN o.status IN ('failed','rejected','cancelled') THEN o.broker_status ELSE i.last_error END,
           completed_at=CASE WHEN o.status IN ('filled','rejected','cancelled') THEN NOW() ELSE i.completed_at END,
           updated_at=NOW()
         FROM strategy_orders o
         WHERE i.strategy_order_id=o.id AND i.action='ENTRY' AND i.status='submitted'
           AND o.status IN ('filled','failed','rejected','cancelled')",
    )
    .execute(&state.db)
    .await?;
    sqlx::query("UPDATE strategy_execution_intents SET status='expired',last_error=CASE WHEN last_error='' THEN 'Signal execution window expired before successful submission.' ELSE last_error END,completed_at=NOW(),updated_at=NOW() WHERE action='ENTRY' AND status IN ('pending','retry_wait') AND expires_at<=NOW()")
        .execute(&state.db).await?;
    let query = format!(
        "UPDATE strategy_execution_intents SET status='claimed',attempts=attempts+1,claimed_at=NOW(),updated_at=NOW()
         WHERE id IN (
           SELECT id FROM strategy_execution_intents
           WHERE action='ENTRY' AND status IN ('pending','retry_wait') AND next_attempt_at<=NOW()
             AND ($1::uuid IS NULL OR signal_id=$1)
           ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 100
         ) RETURNING {}",
        execution_intent_columns()
    );
    let intents: Vec<ExecutionIntent> = sqlx::query_as(&query)
        .bind(signal_id)
        .fetch_all(&state.db)
        .await?;
    let count = intents.len();
    let mut tasks = tokio::task::JoinSet::new();
    for intent in intents {
        let state = state.clone();
        tasks.spawn(async move {
            let permit = state
                .strategy_execution_permits
                .clone()
                .acquire_owned()
                .await;
            if permit.is_err() {
                return;
            }
            let _permit = permit.expect("checked execution permit");
            execute_entry_intent(&state, intent).await;
        });
    }
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            tracing::warn!(%error,"strategy execution intent task failed");
        }
    }
    // ENTRY and SQUARE_OFF refreshes run concurrently. Lock each scope in the
    // same stable order before updating so PostgreSQL cannot invert row locks.
    let mut status_transaction = state.db.begin().await?;
    let _status_signal_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT s.id
         FROM strategy_signals s
         WHERE ($1::uuid IS NULL OR s.id=$1)
           AND EXISTS(
             SELECT 1 FROM strategy_execution_intents i
             WHERE i.signal_id=s.id AND i.action='ENTRY'
           )
         ORDER BY s.id
         FOR UPDATE OF s",
    )
    .bind(signal_id)
    .fetch_all(&mut *status_transaction)
    .await?;
    sqlx::query(
        "UPDATE strategy_signals s SET status=summary.status,updated_at=NOW()
         FROM (
           SELECT signal_id,
             CASE
               WHEN BOOL_OR(status IN ('pending','claimed','retry_wait')) THEN 'dispatching'
               WHEN BOOL_OR(status='failed') AND BOOL_OR(status IN ('submitted','completed')) THEN 'partial'
               WHEN BOOL_OR(status='failed') THEN 'failed'
               ELSE 'completed'
             END AS status
           FROM strategy_execution_intents
           WHERE action='ENTRY' AND ($1::uuid IS NULL OR signal_id=$1)
           GROUP BY signal_id
         ) summary WHERE s.id=summary.signal_id",
    )
    .bind(signal_id)
    .execute(&mut *status_transaction)
    .await?;
    status_transaction.commit().await?;
    Ok(count)
}

fn live_submission_rejection(
    force_demo: bool,
    global_kill: bool,
    user_kill: bool,
    account: Option<(bool, bool, &str, &str)>,
    broker_credentials_present: bool,
    broker_reconciled: bool,
) -> Option<(&'static str, &'static str)> {
    if force_demo {
        Some((
            "force_demo_trading",
            "Live submission stopped because the server is restricted to demo trading.",
        ))
    } else if global_kill {
        Some((
            "global_kill_switch",
            "Live submission stopped by the global emergency kill switch.",
        ))
    } else if user_kill {
        Some((
            "user_kill_switch",
            "Live submission stopped by the account emergency kill switch.",
        ))
    } else {
        match account {
            None => Some((
                "account_missing",
                "Live submission stopped because the account no longer exists.",
            )),
            Some((false, _, _, _)) => Some((
                "account_inactive",
                "Live submission stopped because the account is inactive.",
            )),
            Some((_, false, _, _)) => Some((
                "live_permission_revoked",
                "Live submission stopped because live-trading permission was revoked.",
            )),
            Some((_, _, mode, _)) if mode != "live" => Some((
                "trading_mode_changed",
                "Live submission stopped because the account is no longer in live mode.",
            )),
            Some((_, _, _, token_status)) if !matches!(token_status, "success" | "refreshed") => {
                Some((
                    "broker_session_invalid",
                    "Live submission stopped because the broker session is not valid.",
                ))
            }
            Some(_) if !broker_credentials_present => Some((
                "broker_session_missing",
                "Live submission stopped because broker credentials are unavailable.",
            )),
            Some(_) if !broker_reconciled => Some((
                "broker_reconciliation",
                "Live submission stopped until a full broker reconciliation succeeds.",
            )),
            Some(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct AngelRmsFunds {
    available_cash: f64,
    net: Option<f64>,
    available_limit_margin: Option<f64>,
}

fn finite_number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|item| item.as_f64().or_else(|| item.as_str()?.parse().ok()))
        .filter(|amount| amount.is_finite())
}

fn parse_angel_rms_funds(value: &Value) -> Option<AngelRmsFunds> {
    let available_cash = finite_number(
        value
            .get("availablecash")
            .or_else(|| value.get("availableCash")),
    )?;
    (available_cash >= 0.0).then_some(AngelRmsFunds {
        available_cash,
        net: finite_number(value.get("net")),
        available_limit_margin: finite_number(
            value
                .get("availablelimitmargin")
                .or_else(|| value.get("availableLimitMargin")),
        ),
    })
}

fn broker_available_funds(value: &Value) -> Option<f64> {
    // Angel One identifies `availablecash` as the available fund balance.
    // `net` and `availablelimitmargin` are retained for diagnostics but are
    // not undocumented fallbacks; absence/malformed available cash fails
    // closed for a new live entry.
    parse_angel_rms_funds(value).map(|funds| funds.available_cash)
}

async fn validate_live_order_price_band(
    state: &AppState,
    user_id: Uuid,
    credentials: &crate::credentials::BrokerCredentials,
    snapshot: &Snapshot,
    order: &NewOrder,
) -> AppResult<()> {
    let token = snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Snapshot has no contract token.".into()))?;
    let mut token_map = serde_json::Map::new();
    token_map.insert(snapshot.exchange_segment.clone(), json!([token]));
    let quote = angel::market_quote(
        state,
        user_id,
        &credentials.api_key,
        &credentials.jwt_token,
        "FULL",
        Value::Object(token_map),
    )
    .await?;
    let (lower, upper) = quote_price_band_for_token(&quote, token).ok_or_else(|| {
        AppError::BadRequest(format!(
            "Angel One FULL quote for {} did not include authoritative circuit limits.",
            snapshot
                .contract_symbol
                .as_deref()
                .unwrap_or(&snapshot.instrument)
        ))
    })?;
    let price_is_actionable = !matches!(order.order_type, "MARKET" | "STOPLOSS_MARKET");
    for (label, price) in [
        ("limit price", price_is_actionable.then_some(order.price)),
        ("trigger price", order.trigger),
    ] {
        if let Some(price) = price
            && (price < lower || price > upper)
        {
            return Err(AppError::BadRequest(format!(
                "Order {label} {price:.6} is outside Angel One circuit limits {lower:.6}..={upper:.6}."
            )));
        }
    }
    Ok(())
}

async fn validate_live_entry_margin(
    state: &AppState,
    user_id: Uuid,
    credentials: &crate::credentials::BrokerCredentials,
    snapshot: &Snapshot,
    order: &NewOrder,
    quantity: i32,
) -> AppResult<()> {
    let token = snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Snapshot has no contract token.".into()))?;
    let required = angel::margin_required(
        state,
        user_id,
        &credentials.api_key,
        &credentials.jwt_token,
        json!({
            "exchange":snapshot.exchange_segment,
            "orderType":order.order_type,
            "qty":quantity.to_string(),
            "price":if order.order_type=="MARKET" { "0".to_owned() } else { format!("{:.6}",order.price) },
            "productType":snapshot.product_type,
            "token":token,
            "tradeType":order.side,
        }),
    )
    .await?;
    let rms =
        angel::rms_limits(state, user_id, &credentials.api_key, &credentials.jwt_token).await?;
    let available = broker_available_funds(&rms).ok_or_else(|| {
        AppError::BadRequest("Angel One RMS response has no valid available-funds field.".into())
    })?;
    let buffered_required = required * (1.0 + state.config.margin_safety_buffer_percent / 100.0);
    if !buffered_required.is_finite() || available + 1e-9 < buffered_required {
        return Err(AppError::BadRequest(format!(
            "Insufficient broker funds: available {available:.2}, required {required:.2} plus {:.2}% safety buffer ({buffered_required:.2}).",
            state.config.margin_safety_buffer_percent
        )));
    }
    Ok(())
}

async fn confirm_original_terminal_nonfill(
    state: &AppState,
    user_id: Uuid,
    credentials: &crate::credentials::BrokerCredentials,
    snapshot: &Snapshot,
    client_order_id: &str,
) -> AppResult<bool> {
    let orders =
        angel::order_book(state, user_id, &credentials.api_key, &credentials.jwt_token).await?;
    let order_book_nonfill = if let Some(item) = orders
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| broker_text(item, &["ordertag", "orderTag"]) == Some(client_order_id))
    {
        let status = broker_text(item, &["status", "orderstatus", "orderStatus"])
            .unwrap_or("")
            .to_lowercase();
        let filled = broker_i32(
            item,
            &[
                "filledshares",
                "filledShares",
                "filledquantity",
                "filledQuantity",
            ],
        )
        .unwrap_or(0);
        filled == 0 && matches!(status.as_str(), "rejected" | "cancelled" | "canceled")
    } else {
        // The direct place-order response was a broker rejection. Absence from
        // the order book is acceptable only when the position book is flat.
        true
    };
    let positions =
        angel::positions(state, user_id, &credentials.api_key, &credentials.jwt_token).await?;
    let exchange = snapshot.exchange_segment.to_uppercase();
    let token = snapshot.contract_token.as_deref().unwrap_or("");
    Ok(order_book_nonfill
        && parse_broker_positions(&positions)
            .into_iter()
            .find(|position| position.exchange == exchange && position.token == token)
            .is_none_or(|position| position.net_quantity == 0))
}

pub(crate) async fn place_strategy_order(
    state: &AppState,
    runner: &Runner,
    snapshot: &Snapshot,
    session: &str,
    order: NewOrder,
) -> AppResult<()> {
    place_strategy_order_inner(state, runner, snapshot, session, order, None).await
}

async fn place_strategy_order_for_signal(
    state: &AppState,
    runner: &Runner,
    snapshot: &Snapshot,
    session: &str,
    order: NewOrder,
    signal_at: DateTime<Utc>,
) -> AppResult<()> {
    place_strategy_order_inner(state, runner, snapshot, session, order, Some(signal_at)).await
}

async fn place_strategy_order_inner(
    state: &AppState,
    runner: &Runner,
    snapshot: &Snapshot,
    session: &str,
    mut order: NewOrder,
    originated_at: Option<DateTime<Utc>>,
) -> AppResult<()> {
    let protective = matches!(order.role, "TARGET" | "SL1" | "SL2" | "EMERGENCY_CLOSE");
    if snapshot.strategy_key == STRATEGY_KEY && matches!(order.role, "BUY_ENTRY" | "SELL_ENTRY") {
        let (target, sl1, sl2) = if order.role == "BUY_ENTRY" {
            (snapshot.buy_target, snapshot.buy_sl1, snapshot.buy_sl2)
        } else {
            (snapshot.sell_target, snapshot.sell_sl1, snapshot.sell_sl2)
        };
        required_exit_level(target, "target")?;
        required_exit_level(sl1, "initial stop loss")?;
        required_exit_level(sl2, "continuation stop loss")?;
    }
    let entry_order = matches!(order.role, "BUY_ENTRY" | "SELL_ENTRY");
    let (lot_size, tick_size) =
        current_contract_order_metadata(state, snapshot, entry_order).await?;
    if entry_order && snapshot.lot_size != Some(lot_size) {
        return Err(AppError::BadRequest(format!(
            "Stale contract lot size: snapshot {:?}, current Angel One master {lot_size}.",
            snapshot.lot_size
        )));
    }
    let token = snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Snapshot has no contract token.".into()))?;
    let symbol = snapshot
        .contract_symbol
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Snapshot has no contract symbol.".into()))?;
    if order.order_type != "MARKET" {
        order.price = normalize_to_tick(order.price, tick_size, order.side).ok_or_else(|| {
            AppError::BadRequest(
                "Order price cannot be normalized to the contract tick size.".into(),
            )
        })?;
    }
    if let Some(trigger) = order.trigger {
        order.trigger = Some(
            normalize_to_tick(trigger, tick_size, order.side).ok_or_else(|| {
                AppError::BadRequest(
                    "Trigger price cannot be normalized to the contract tick size.".into(),
                )
            })?,
        );
    }
    let quantity = order.quantity.unwrap_or(
        lot_size
            .checked_mul(order.lots)
            .ok_or_else(|| AppError::BadRequest("Order quantity overflow.".into()))?,
    );
    if quantity <= 0 || lot_size <= 0 || order.lots <= 0 {
        return Err(AppError::BadRequest(format!(
            "Invalid contract quantity: quantity {quantity}, lot size {lot_size}, and reporting lots {} must all be positive.",
            order.lots
        )));
    }
    if entry_order {
        let expected_quantity = lot_size
            .checked_mul(order.lots)
            .ok_or_else(|| AppError::BadRequest("Order quantity overflow.".into()))?;
        if !quantity_matches_policy(
            quantity,
            OrderQuantityPolicy::NormalEntry {
                lot_size,
                lots: order.lots,
            },
        ) {
            return Err(AppError::BadRequest(format!(
                "Futures Breakout quantity mismatch: {order_lots} lots × contract lot {lot_size} must be {expected_quantity}, got {quantity}.",
                order_lots = order.lots
            )));
        }
        if let Some(source_trade_id) = order.trade_id {
            let valid_reversal_source: bool = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1
                    FROM strategy_reversal_intents i
                    JOIN trades source ON source.id=i.source_trade_id
                    WHERE i.source_trade_id=$1
                      AND i.user_id=$2
                      AND i.snapshot_id=$3
                      AND i.order_session_key=$4
                      AND i.status='processing'
                      AND source.status='closed'
                      AND source.exit_reason='SL2'
                      AND i.reversal_direction=$5
                )",
            )
            .bind(source_trade_id)
            .bind(runner.user_id)
            .bind(snapshot.id)
            .bind(session)
            .bind(order.side)
            .fetch_one(&state.db)
            .await?;
            if !valid_reversal_source {
                return Err(AppError::BadRequest(
                    "Entry order has no claimed, authoritative SL2 reversal intent.".into(),
                ));
            }
        } else if snapshot.strategy_key == STRATEGY_KEY
            && user_has_breakout_open_position(state, runner.user_id, &snapshot.instrument).await?
        {
            return Err(AppError::BadRequest(format!(
                "{} entry skipped because an open Futures Breakout position already exists for {}.",
                order.role, snapshot.instrument
            )));
        }
    } else if let Some(trade_id) = order.trade_id {
        let trade_quantity: Option<OrderTradeQuantityRow> =
            sqlx::query_as("SELECT quantity,exposure_origin,broker_net_quantity,last_position_reconciled_at FROM trades WHERE id=$1 AND status='open'")
                .bind(trade_id)
                .fetch_optional(&state.db)
                .await?;
        let (remaining_quantity, exposure_origin, broker_net_quantity, reconciled_at) =
            trade_quantity.ok_or_else(|| {
                AppError::BadRequest("Exit order has no open local trade exposure.".into())
            })?;
        let broker_verified_emergency = order.role == "EMERGENCY_CLOSE"
            && exposure_origin == "broker_over_close"
            && reconciled_at.is_some()
            && broker_net_quantity
                .map(|broker_quantity| broker_quantity.unsigned_abs().min(i32::MAX as u32) as i32)
                .is_some_and(|broker_quantity| {
                    quantity_matches_policy(
                        quantity,
                        OrderQuantityPolicy::BrokerResidual { broker_quantity },
                    )
                });
        if exposure_origin == "broker_over_close"
            && order.role == "EMERGENCY_CLOSE"
            && !broker_verified_emergency
        {
            return Err(AppError::BadRequest(
                "Emergency close quantity does not equal the latest broker-confirmed residual exposure."
                    .into(),
            ));
        }
        if !broker_verified_emergency
            && !quantity_matches_policy(
                quantity,
                OrderQuantityPolicy::NormalExit { remaining_quantity },
            )
        {
            return Err(AppError::BadRequest(format!(
                "Normal exit quantity {quantity} exceeds intended remaining quantity {remaining_quantity}."
            )));
        }
    } else if !valid_contract_quantity(quantity, lot_size, order.lots) {
        return Err(AppError::BadRequest(
            "Non-entry order without a trade must use a positive whole-lot quantity.".into(),
        ));
    }
    let key = format!(
        "{}:{}:{}:{}:{}",
        runner.user_id,
        snapshot.id,
        session,
        order.role,
        order.trade_id.map(|v| v.to_string()).unwrap_or_default()
    );
    let mut live_reconciled = runner.trading_mode != "live" || protective;
    let entry_credentials = if runner.trading_mode == "live" {
        Some(state.credentials.load(runner.user_id).await?)
    } else {
        None
    };
    let has_actionable_price =
        !matches!(order.order_type, "MARKET" | "STOPLOSS_MARKET") || order.trigger.is_some();
    if has_actionable_price
        && let Some(credentials) = entry_credentials.as_ref()
        && let Err(error) =
            validate_live_order_price_band(state, runner.user_id, credentials, snapshot, &order)
                .await
    {
        let outside_band = error
            .to_string()
            .contains("outside Angel One circuit limits");
        operational_alert_for(
            state,
            &snapshot.strategy_key,
            Some(runner.user_id),
            &runner.instrument,
            if outside_band {
                "broker_price_band_rejected"
            } else {
                "broker_price_band_unavailable"
            },
            if protective { "error" } else { "warning" },
            &error.to_string(),
        )
        .await;
        // A known out-of-band protective price must enter normal protection
        // recovery instead of being sent. If circuit data itself is
        // unavailable, an urgent protective order is still sent for Angel
        // One's authoritative broker-side validation; new entries fail closed.
        if outside_band || !protective {
            return Err(error);
        }
    }
    if !protective
        && let Some(credentials) = entry_credentials.as_ref()
        && let Err(error) = validate_live_entry_margin(
            state,
            runner.user_id,
            credentials,
            snapshot,
            &order,
            quantity,
        )
        .await
    {
        operational_alert_for(
            state,
            &snapshot.strategy_key,
            Some(runner.user_id),
            &runner.instrument,
            "broker_margin_validation_failed",
            "warning",
            &error.to_string(),
        )
        .await;
        return Err(error);
    }
    if runner.trading_mode == "live" && !protective {
        Box::pin(reconcile_live_user_readiness(state, runner.user_id)).await?;
        live_reconciled = true;
    }
    let price_refresh_error = if !protective {
        match refresh_snapshot_market_tick(state, snapshot).await {
            Ok(()) => None,
            Err(error) => {
                let message = error.to_string();
                tracing::warn!(
                    user_id = %runner.user_id,
                    instrument = %runner.instrument,
                    exchange = %snapshot.exchange_segment,
                    token,
                    error = %message,
                    "could not refresh strategy market price before risk check"
                );
                Some(message)
            }
        }
    } else {
        None
    };
    let active_id = match risk::assess_and_reserve(
        state,
        &risk::OrderRisk {
            user_id: runner.user_id,
            snapshot_id: snapshot.id,
            trade_id: order.trade_id,
            session,
            role: order.role,
            side: order.side,
            mode: &runner.trading_mode,
            lots: order.lots,
            quantity,
            price: order.price,
            trigger_price: order.trigger,
            idempotency_key: &key,
            snapshot_ready: snapshot.status == "ready",
            snapshot_current: snapshot.trade_date == ist_now().date_naive()
                && Utc::now() - snapshot.fetched_at < Duration::hours(26),
            exchange_segment: &snapshot.exchange_segment,
            contract_token: token,
            live_reconciled,
            originated_at,
        },
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let mut message = error.to_string();
            if message.contains("no fresh valid market price")
                && let Some(refresh_error) = price_refresh_error.as_ref()
            {
                message = format!("{message} Last quote refresh failed: {refresh_error}");
            }
            operational_alert_for(
                state,
                &snapshot.strategy_key,
                Some(runner.user_id),
                &runner.instrument,
                "risk_rejected",
                "warning",
                &message,
            )
            .await;
            let contract_label = contract_log_label(&runner.instrument, Some(symbol));
            crate::logs::append(
                &runner.username,
                &format!(
                    "RISK REJECTED {} {} {}: {}",
                    order.role, order.side, contract_label, message
                ),
            )
            .await;
            return Err(error);
        }
    };
    let Some(id) = active_id else {
        return Ok(());
    };
    let client_order_id = format!("RX{}", &id.simple().to_string()[..18]).to_uppercase();
    sqlx::query("UPDATE strategy_orders SET client_order_id=$2,order_type=$3,exchange_segment=$4,product_type=$5,updated_at=NOW() WHERE id=$1")
        .bind(id)
        .bind(&client_order_id)
        .bind(order.order_type)
        .bind(&snapshot.exchange_segment)
        .bind(&snapshot.product_type)
        .execute(&state.db)
        .await?;
    if protective {
        // At this boundary the durable intent exists, but no broker request
        // has started. A restart may safely terminalize the stale `pending`
        // row and create a new deterministic attempt.
        trip_execution_failpoint("after_protection_intent_before_submission").await?;
    }
    let result = if runner.trading_mode == "live" {
        let claimed = if protective {
            sqlx::query("UPDATE strategy_orders SET status='submitting',submission_attempts=submission_attempts+1,state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status='pending'")
                .bind(id).execute(&state.db).await?.rows_affected() > 0
        } else {
            let mut tx = state.db.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtext('rulenix:risk:global'))")
                .execute(&mut *tx)
                .await?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::uuid::text,0))")
                .bind(runner.user_id)
                .execute(&mut *tx)
                .await?;
            let kills: (bool, bool) = sqlx::query_as("SELECT COALESCE((SELECT enabled FROM risk_kill_switches WHERE user_id IS NULL),FALSE),COALESCE((SELECT enabled FROM risk_kill_switches WHERE user_id=$1),FALSE)")
                .bind(runner.user_id)
                .fetch_one(&mut *tx)
                .await?;
            let account: Option<(bool, bool, String, String, bool)> = sqlx::query_as("SELECT u.is_active,u.can_live_trade,COALESCE(p.trading_mode,'demo'),COALESCE(p.last_token_status,''),EXISTS(SELECT 1 FROM broker_reconciliation_health h WHERE h.user_id=u.id AND h.healthy=TRUE AND h.broker_credential_revision=p.broker_credential_revision AND h.checked_at>NOW()-INTERVAL '5 minutes') FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=$1")
                .bind(runner.user_id)
                .fetch_optional(&mut *tx)
                .await?;
            let rejection = live_submission_rejection(
                state.config.force_demo_trading,
                kills.0,
                kills.1,
                account
                    .as_ref()
                    .map(|value| (value.0, value.1, value.2.as_str(), value.3.as_str())),
                entry_credentials.as_ref().is_some_and(|credentials| {
                    !credentials.api_key.is_empty() && !credentials.jwt_token.is_empty()
                }),
                account.as_ref().is_some_and(|value| value.4),
            );
            let claimed = if let Some((code, message)) = rejection {
                let rejected = sqlx::query("UPDATE strategy_orders SET status='rejected',broker_error_class='risk',broker_error_code=$2,broker_status=$3,state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status='pending'")
                    .bind(id)
                    .bind(code)
                    .bind(message)
                    .execute(&mut *tx)
                    .await?;
                if rejected.rows_affected() > 0 {
                    sqlx::query("INSERT INTO broker_order_events(order_id,user_id,from_state,to_state,event_type,error_class,error_code,diagnostic) VALUES($1,$2,'pending','rejected','submission_blocked','risk',$3,$4)")
                        .bind(id).bind(runner.user_id).bind(code).bind(message).execute(&mut *tx).await?;
                }
                false
            } else {
                let submitted = sqlx::query("UPDATE strategy_orders SET status='submitting',submission_attempts=submission_attempts+1,state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status='pending'")
                    .bind(id).execute(&mut *tx).await?;
                if submitted.rows_affected() > 0 {
                    sqlx::query("INSERT INTO broker_order_events(order_id,user_id,from_state,to_state,event_type,diagnostic) VALUES($1,$2,'pending','submitting','submission_started',$3)")
                        .bind(id).bind(runner.user_id).bind(format!("client_order_id={client_order_id}")).execute(&mut *tx).await?;
                }
                submitted.rows_affected() > 0
            };
            tx.commit().await?;
            if let Some((code, message)) = rejection {
                operational_alert_for(
                    state,
                    &snapshot.strategy_key,
                    Some(runner.user_id),
                    &runner.instrument,
                    code,
                    "error",
                    message,
                )
                .await;
                return Err(AppError::Forbidden(message.into()));
            }
            claimed
        };
        if !claimed {
            return Ok(());
        }
        if protective {
            sqlx::query("INSERT INTO broker_order_events(order_id,user_id,from_state,to_state,event_type,diagnostic) VALUES($1,$2,'pending','submitting','submission_started',$3)")
                .bind(id).bind(runner.user_id).bind(format!("client_order_id={client_order_id}")).execute(&state.db).await?;
        }
        let credentials = entry_credentials
            .as_ref()
            .ok_or_else(|| AppError::Unauthorized("Angel One session is not connected.".into()))?;
        if credentials.jwt_token.is_empty() || credentials.api_key.is_empty() {
            Err(angel::BrokerError {
                class: angel::BrokerErrorClass::Authentication,
                status: None,
                code: "session_missing".into(),
                message: "Angel One session is not connected.".into(),
                diagnostic: "Required broker credentials are absent.".into(),
            })
        } else {
            angel::place_order(
                state,
                runner.user_id,
                &credentials.api_key,
                &credentials.jwt_token,
                &angel::OrderRequest {
                    symbol,
                    token,
                    exchange: &snapshot.exchange_segment,
                    product_type: &snapshot.product_type,
                    side: order.side,
                    order_type: order.order_type,
                    quantity,
                    price: order.price,
                    trigger_price: order.trigger,
                    client_order_id: &client_order_id,
                },
            )
            .await
        }
    } else {
        Ok(format!("DEMO-{id}"))
    };
    match result {
        Ok(broker_id) => {
            trip_execution_failpoint("after_broker_accept_before_local_ack").await?;
            sqlx::query("UPDATE strategy_orders SET status='submitted',broker_order_id=$2,broker_error_class='',broker_error_code='',broker_http_status=NULL,state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status IN ('pending','submitting')")
                .bind(id).bind(&broker_id).execute(&state.db).await?;
            sqlx::query("INSERT INTO broker_order_events(order_id,user_id,from_state,to_state,event_type,broker_order_id) VALUES($1,$2,$3,'submitted','submission_acknowledged',$4)")
                .bind(id).bind(runner.user_id).bind(if runner.trading_mode=="live"{"submitting"}else{"pending"}).bind(&broker_id).execute(&state.db).await?;
            emit_for(state, &snapshot.strategy_key, Some(runner.user_id), &runner.instrument, "order_submitted", json!({"order_id":id,"broker_order_id":broker_id,"role":order.role,"side":order.side,"order_type":order.order_type,"price":order.price,"trigger_price":order.trigger,"lots":order.lots,"mode":runner.trading_mode})).await;
            let contract_label = contract_log_label(&runner.instrument, Some(symbol));
            crate::logs::append(
                &runner.username,
                &format!(
                    "STRATEGY {} {} {} {} lots @ {:.2}",
                    order.role, order.side, contract_label, order.lots, order.price
                ),
            )
            .await;
            if runner.trading_mode == "demo" && order.order_type == "MARKET" {
                let stored: StoredOrder = sqlx::query_as("SELECT id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,broker_order_id,client_order_id,status,filled_quantity,processed_quantity,average_fill_price::float8 FROM strategy_orders WHERE id=$1")
                    .bind(id).fetch_one(&state.db).await?;
                Box::pin(complete_order(state, stored, order.price)).await?;
            }
            Ok(())
        }
        Err(error) => {
            let contract_unavailable = !protective
                && error.class == angel::BrokerErrorClass::Rejected
                && matches!(
                    snapshot.strategy_key.as_str(),
                    STRATEGY_KEY | SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
                )
                && angel::is_contract_unavailable_error(&format!("{} {}", error, error.diagnostic));
            let status = if angel::may_retry_submission(error.class)
                || error.class == angel::BrokerErrorClass::Authentication
            {
                "failed"
            } else if error.class == angel::BrokerErrorClass::Ambiguous {
                "ambiguous"
            } else {
                "rejected"
            };
            let diagnostic = format!("{}; {}", error, error.diagnostic);
            sqlx::query("UPDATE strategy_orders SET status=$2,broker_status=$3,broker_error_class=$4,broker_error_code=$5,broker_http_status=$6,uncertain_since_at=CASE WHEN $2='ambiguous' THEN COALESCE(uncertain_since_at,NOW()) ELSE uncertain_since_at END,state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status='submitting'")
                .bind(id).bind(status).bind(&diagnostic).bind(error.class.as_str()).bind(&error.code).bind(error.status.map(i32::from)).execute(&state.db).await?;
            sqlx::query("INSERT INTO broker_order_events(order_id,user_id,from_state,to_state,event_type,error_class,error_code,http_status,diagnostic) VALUES($1,$2,'submitting',$3,'submission_failed',$4,$5,$6,$7)")
                .bind(id).bind(runner.user_id).bind(status).bind(error.class.as_str()).bind(&error.code).bind(error.status.map(i32::from)).bind(&diagnostic).execute(&state.db).await?;
            if error.class == angel::BrokerErrorClass::Ambiguous
                && matches!(order.role, "SL1" | "SL2")
            {
                mark_ambiguous_protection(
                    state,
                    id,
                    &client_order_id,
                    runner,
                    snapshot,
                    &order,
                    &diagnostic,
                )
                .await?;
            }
            emit_for(
                state,
                &snapshot.strategy_key,
                Some(runner.user_id),
                &runner.instrument,
                "order_failed",
                json!({"order_id":id,"role":order.role,"error":error.to_string(),"classification":error.class}),
            )
            .await;
            operational_alert_for(
                state,
                &snapshot.strategy_key,
                Some(runner.user_id),
                &runner.instrument,
                "order_submission_failed",
                "error",
                &format!(
                    "{} {} order was not confirmed and requires automatic retry or review: {}",
                    order.role, order.side, error
                ),
            )
            .await;
            let terminal_nonfill_confirmed = if contract_unavailable {
                match entry_credentials.as_ref() {
                    Some(credentials) => confirm_original_terminal_nonfill(
                        state,
                        runner.user_id,
                        credentials,
                        snapshot,
                        &client_order_id,
                    )
                    .await
                    .unwrap_or(false),
                    None => false,
                }
            } else {
                false
            };
            if contract_unavailable && !terminal_nonfill_confirmed {
                operational_alert_for(state,&snapshot.strategy_key,Some(runner.user_id),&runner.instrument,"contract_roll_ambiguity","critical",&format!("Contract replacement was blocked because original order {id} / client tag {client_order_id} could not be proven terminal and unfilled.")).await;
            }
            if contract_unavailable && terminal_nonfill_confirmed && !session.contains(":oroll") {
                if snapshot.strategy_key != STRATEGY_KEY
                    && matches!(order.role, "BUY_ENTRY" | "SELL_ENTRY")
                    && let Some((refreshed_snapshot, refreshed_price)) =
                        supertrend_retry_contract_snapshot(state, snapshot, runner.user_id).await?
                {
                    let retry_session = session_with_suffix(session, "oroll");
                    let mut retry_order = order.clone();
                    retry_order.price = refreshed_price;
                    retry_order.trigger = None;
                    operational_alert_for(
                        state,
                        &snapshot.strategy_key,
                        Some(runner.user_id),
                        &runner.instrument,
                        "option_contract_rolled_forward",
                        "warning",
                        &format!(
                            "{} was unavailable at Angel One, so Rulenix refreshed the contract master and is retrying with {}.",
                            symbol,
                            refreshed_snapshot
                                .contract_symbol
                                .as_deref()
                                .unwrap_or("a fresh option contract")
                        ),
                    )
                    .await;
                    return Box::pin(place_strategy_order(
                        state,
                        runner,
                        &refreshed_snapshot,
                        &retry_session,
                        retry_order,
                    ))
                    .await;
                }
                if snapshot.strategy_key != STRATEGY_KEY {
                    operational_alert_for(
                        state,
                        &snapshot.strategy_key,
                        Some(runner.user_id),
                        &runner.instrument,
                        "option_contract_roll_forward_unavailable",
                        "error",
                        "Angel One rejected the selected option contract, and the refreshed contract master did not provide another quoteable contract.",
                    )
                    .await;
                    return Err(match error.class {
                        angel::BrokerErrorClass::Authentication => {
                            AppError::Unauthorized(error.to_string())
                        }
                        angel::BrokerErrorClass::Rejected => {
                            AppError::BadRequest(error.to_string())
                        }
                        angel::BrokerErrorClass::Retryable | angel::BrokerErrorClass::Ambiguous => {
                            AppError::BadRequest(error.to_string())
                        }
                    });
                }
                match force_refresh_futures_contract_snapshot(
                    state,
                    &snapshot.instrument,
                    snapshot.trade_date,
                )
                .await
                {
                    Ok(Some(refreshed_snapshot)) => {
                        let retry_session = session_with_suffix(session, "croll");
                        operational_alert_for(
                            state,
                            &snapshot.strategy_key,
                            Some(runner.user_id),
                            &runner.instrument,
                            "contract_rolled_forward",
                            "warning",
                            &format!(
                                "{} was unavailable at Angel One, so Rulenix refreshed the contract master and is retrying with {}.",
                                symbol,
                                refreshed_snapshot
                                    .contract_symbol
                                    .as_deref()
                                    .unwrap_or("the next eligible contract")
                            ),
                        )
                        .await;
                        return Box::pin(place_strategy_order(
                            state,
                            runner,
                            &refreshed_snapshot,
                            &retry_session,
                            order.clone(),
                        ))
                        .await;
                    }
                    Ok(None) => {
                        operational_alert_for(
                            state,
                            &snapshot.strategy_key,
                            Some(runner.user_id),
                            &runner.instrument,
                            "contract_roll_forward_unavailable",
                            "error",
                            "Angel One rejected the selected contract, and the refreshed contract master did not provide a different eligible expiry.",
                        )
                        .await;
                    }
                    Err(refresh_error) => {
                        operational_alert_for(
                            state,
                            &snapshot.strategy_key,
                            Some(runner.user_id),
                            &runner.instrument,
                            "contract_roll_forward_failed",
                            "error",
                            &format!(
                                "Angel One rejected the selected contract, and contract refresh failed: {refresh_error}"
                            ),
                        )
                        .await;
                    }
                }
            }
            Err(match error.class {
                angel::BrokerErrorClass::Authentication => {
                    AppError::Unauthorized(error.to_string())
                }
                angel::BrokerErrorClass::Rejected => AppError::BadRequest(error.to_string()),
                angel::BrokerErrorClass::Retryable | angel::BrokerErrorClass::Ambiguous => {
                    AppError::BadRequest(error.to_string())
                }
            })
        }
    }
}

fn planned_breakout_entry_orders(
    snapshot: &Snapshot,
) -> AppResult<Vec<(&'static str, &'static str, f64)>> {
    Ok(match snapshot.entry_direction.as_deref() {
        Some("BUY") => vec![(
            "BUY_ENTRY",
            "BUY",
            required_exit_level(snapshot.planned_entry, "planned buy entry")?,
        )],
        Some("SELL") => vec![(
            "SELL_ENTRY",
            "SELL",
            required_exit_level(snapshot.planned_entry, "planned sell entry")?,
        )],
        Some("BOTH") => vec![
            (
                "BUY_ENTRY",
                "BUY",
                required_exit_level(snapshot.buy_entry, "buy entry")?,
            ),
            (
                "SELL_ENTRY",
                "SELL",
                required_exit_level(snapshot.sell_entry, "sell entry")?,
            ),
        ],
        _ => {
            return Err(AppError::BadRequest(format!(
                "{} gap entry direction is missing.",
                snapshot.instrument
            )));
        }
    })
}

async fn user_has_breakout_open_position(
    state: &AppState,
    user_id: Uuid,
    instrument: &str,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1
            FROM trades
            WHERE user_id=$1
              AND strategy_key=$2
              AND instrument_label=$3
              AND status='open'
              AND remaining_lots>0
        )",
    )
    .bind(user_id)
    .bind(STRATEGY_KEY)
    .bind(instrument)
    .fetch_one(&state.db)
    .await?)
}

async fn run_entries(
    state: AppState,
    instrument: String,
    date: NaiveDate,
    session: &'static str,
    resolve_opening_range: bool,
) -> AppResult<()> {
    let runners: Vec<Runner> = sqlx::query_as("SELECT c.user_id,u.username,c.instrument,c.lots,c.run_day_session,c.run_evening_session,p.trading_mode FROM user_strategy_configs c JOIN user_strategy_activations a ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key JOIN users u ON u.id=c.user_id JOIN user_profiles p ON p.user_id=c.user_id WHERE c.enabled=TRUE AND a.is_active=TRUE AND c.strategy_key=$1 AND c.instrument=$2 AND u.is_active=TRUE AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))")
        .bind(STRATEGY_KEY).bind(&instrument).fetch_all(&state.db).await?;
    let runners: Vec<Runner> = runners
        .into_iter()
        .filter(|runner| {
            if session == "day" {
                runner.run_day_session
            } else {
                runner.run_evening_session
            }
        })
        .collect();
    if runners.is_empty() {
        return Ok(());
    }
    let snapshot = create_snapshot(&state, &instrument, date).await?;
    if snapshot.status != "ready" {
        return Err(AppError::BadRequest(
            snapshot
                .error
                .unwrap_or_else(|| "Strategy snapshot is not ready.".into()),
        ));
    }
    ensure_futures_gap_plans(&state, date, &instrument).await?;
    let mut snapshot = load_snapshot(&state, &instrument, date)
        .await?
        .ok_or_else(|| AppError::BadRequest("Strategy snapshot is missing.".into()))?;
    if snapshot.gap_plan_status.as_deref() == Some("WAITING_RANGE") {
        if !resolve_opening_range {
            emit(
                &state,
                None,
                &instrument,
                "opening_range_entry_waiting",
                json!({
                    "trade_date": date,
                    "entry_direction": snapshot.entry_direction,
                    "available_after": "09:15 IST",
                }),
            )
            .await;
            return Ok(());
        }
        snapshot = resolve_futures_opening_range_plan(&state, &snapshot).await?;
    }
    if snapshot.gap_plan_status.as_deref() != Some("READY") {
        return Err(AppError::BadRequest(format!(
            "{} gap entry plan is not ready.",
            instrument
        )));
    }
    // Snapshot/contract recovery may take seconds. Re-read and freeze the
    // eligible audience immediately before creating the durable signal.
    let runners: Vec<Runner> = sqlx::query_as("SELECT c.user_id,u.username,c.instrument,c.lots,c.run_day_session,c.run_evening_session,p.trading_mode FROM user_strategy_configs c JOIN user_strategy_activations a ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key JOIN users u ON u.id=c.user_id JOIN user_profiles p ON p.user_id=c.user_id WHERE c.enabled=TRUE AND a.is_active=TRUE AND c.strategy_key=$1 AND c.instrument=$2 AND u.is_active=TRUE AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))")
        .bind(STRATEGY_KEY).bind(&instrument).fetch_all(&state.db).await?;
    let runners: Vec<Runner> = runners
        .into_iter()
        .filter(|runner| {
            if session == "day" {
                runner.run_day_session
            } else {
                runner.run_evening_session
            }
        })
        .collect();
    if runners.is_empty() {
        return Ok(());
    }
    let expiry_local = if session == "day" {
        date.and_hms_opt(15, 19, 45)
    } else {
        date.and_hms_opt(23, 25, 0)
    }
    .ok_or_else(|| AppError::BadRequest("Invalid Futures Breakout execution window.".into()))?;
    let expires_at = ist_naive_to_utc(expiry_local)?;
    let orders = planned_breakout_entry_orders(&snapshot)?;
    let mut intents = Vec::with_capacity(runners.len() * orders.len());
    for runner in &runners {
        for (role, side, price) in &orders {
            intents.push(PreparedExecutionIntent {
                user_id: runner.user_id,
                snapshot_id: snapshot.id,
                strategy_key: STRATEGY_KEY.into(),
                instrument: instrument.clone(),
                session_key: session.into(),
                action: "ENTRY",
                role,
                side,
                order_type: "STOPLOSS_LIMIT",
                lots: runner.lots,
                quantity: None,
                price: *price,
                trigger_price: Some(*price),
                trade_id: None,
                expires_at: Some(expires_at),
            });
        }
    }
    let signal_session = format!(
        "fb-{}-{}-{}",
        date.format("%Y%m%d"),
        session,
        snapshot.entry_source.as_deref().unwrap_or("standard")
    );
    let (signal_id, _) = materialize_signal_intents(
        &state,
        STRATEGY_KEY,
        &instrument,
        &signal_session,
        "ENTRY",
        Utc::now(),
        Some(snapshot.id),
        json!({
            "entry_direction":snapshot.entry_direction,
            "entry_source":snapshot.entry_source,
            "planned_entry":snapshot.planned_entry,
            "buy_entry":snapshot.buy_entry,
            "sell_entry":snapshot.sell_entry,
            "contract_symbol":snapshot.contract_symbol,
            "expected_users":runners.len()
        }),
        &intents,
    )
    .await?;
    process_execution_intents(&state, Some(signal_id)).await?;
    Ok(())
}

async fn supertrend_runners(
    state: &AppState,
    config: IndexOptionConfig,
) -> AppResult<Vec<SuperTrendRunner>> {
    Ok(sqlx::query_as(
        "SELECT c.user_id,u.username,c.instrument,c.lots,c.run_day_session,c.run_evening_session,p.trading_mode,
                CASE WHEN c.target_points>0 THEN c.target_points ELSE $3 END AS target_points,
                CASE WHEN c.stop_loss_points>0 THEN c.stop_loss_points ELSE $4 END AS stop_loss_points
         FROM user_strategy_configs c
         JOIN user_strategy_activations a ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key
         JOIN users u ON u.id=c.user_id
         JOIN user_profiles p ON p.user_id=c.user_id
         WHERE c.enabled=TRUE
           AND a.is_active=TRUE
           AND c.strategy_key=$1
           AND c.instrument=$2
           AND u.is_active=TRUE
           AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))",
    )
    .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
    .bind(config.instrument)
    .bind(config.default_target_points)
    .bind(config.default_stop_loss_points)
    .fetch_all(&state.db)
    .await?)
}

async fn user_has_supertrend_side_exposure(
    state: &AppState,
    user_id: Uuid,
    underlying: &str,
    side: IndexOptionSide,
) -> AppResult<bool> {
    let instrument = format!("{}_{}", underlying, side.option_type());
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trades t WHERE t.user_id=$1 AND t.strategy_key=$2 AND t.instrument_label=$3 AND t.status='open' AND t.remaining_lots>0) OR EXISTS(SELECT 1 FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.user_id=$1 AND s.strategy_key=$2 AND s.instrument=$3 AND o.role IN ('BUY_ENTRY','SELL_ENTRY') AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE))")
        .bind(user_id)
        .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
        .bind(instrument)
        .fetch_one(&state.db)
        .await?)
}

async fn cancel_supertrend_active_entries_for_side(
    state: &AppState,
    user_id: Uuid,
    underlying: &str,
    side: IndexOptionSide,
    reason: &str,
) -> AppResult<()> {
    let instrument = format!("{}_{}", underlying, side.option_type());
    let orders: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
        "SELECT o.id,o.broker_order_id,o.execution_mode,o.order_type,o.status
         FROM strategy_orders o
         JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
         WHERE o.user_id=$1
           AND s.strategy_key=$2
           AND s.instrument=$3
           AND o.role IN ('BUY_ENTRY','SELL_ENTRY')
           AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')",
    )
    .bind(user_id)
    .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
    .bind(&instrument)
    .fetch_all(&state.db)
    .await?;
    if orders.is_empty() {
        return Ok(());
    }

    let credentials = if orders.iter().any(|(_, _, mode, _, status)| {
        mode == "live" && matches!(status.as_str(), "submitted" | "partially_filled")
    }) {
        Some(state.credentials.load(user_id).await?)
    } else {
        None
    };
    let mut errors = Vec::new();
    for (id, broker_id, mode, order_type, status) in orders {
        if status == "pending" || mode == "demo" {
            sqlx::query("UPDATE strategy_orders SET status='cancelled',broker_status=$2,state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status IN ('pending','submitted','partially_filled')")
                .bind(id)
                .bind(reason)
                .execute(&state.db)
                .await?;
            continue;
        }
        if mode == "live" && matches!(status.as_str(), "submitted" | "partially_filled") {
            let Some(credentials) = credentials.as_ref() else {
                errors.push(format!("{id}: live broker credentials are unavailable"));
                continue;
            };
            if broker_id.is_empty() {
                errors.push(format!("{id}: live entry order has no broker order id"));
                continue;
            }
            let variety = if order_type.starts_with("STOPLOSS") {
                "STOPLOSS"
            } else {
                "NORMAL"
            };
            match angel::cancel_order(
                state,
                user_id,
                &credentials.api_key,
                &credentials.jwt_token,
                &broker_id,
                variety,
            )
            .await
            {
                Ok(()) => {
                    sqlx::query("UPDATE strategy_orders SET status='cancelling',broker_status=$2,state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status IN ('submitted','partially_filled')")
                        .bind(id)
                        .bind(reason)
                        .execute(&state.db)
                        .await?;
                }
                Err(error) => errors.push(format!("{id}: {error}")),
            }
            continue;
        }
        errors.push(format!("{id}: entry order is already {status}"));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::BadRequest(errors.join("; ")))
    }
}

async fn close_supertrend_open_trades_for_side(
    state: &AppState,
    runner: &SuperTrendRunner,
    config: IndexOptionConfig,
    side: IndexOptionSide,
    now: DateTime<FixedOffset>,
    reason: &str,
) -> AppResult<()> {
    let instrument = config.option_instrument(side);
    let trades: Vec<(Uuid, String, String, i32, i32, Option<Uuid>)> = sqlx::query_as(
        "SELECT id,instrument_label,execution_mode,quantity,remaining_lots,strategy_snapshot_id
         FROM trades
         WHERE user_id=$1
           AND strategy_key=$2
           AND instrument_label=$3
           AND status='open'
           AND remaining_lots>0
           AND strategy_snapshot_id IS NOT NULL",
    )
    .bind(runner.user_id)
    .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
    .bind(&instrument)
    .fetch_all(&state.db)
    .await?;
    if trades.is_empty() {
        return Ok(());
    }

    let mut errors = Vec::new();
    let base_runner = Runner::from(runner.clone());
    for (trade_id, instrument, execution_mode, quantity, remaining_lots, snapshot_id) in trades {
        let Some(snapshot_id) = snapshot_id else {
            continue;
        };
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = match sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                errors.push(format!("{instrument} {trade_id}: {error}"));
                continue;
            }
        };
        if snapshot
            .contract_expiry
            .is_some_and(|expiry| option_expiry_checkpoint_due(expiry, now))
        {
            tracing::info!(%trade_id,user_id=%runner.user_id,instrument=%instrument,contract=?snapshot.contract_symbol,expiry=?snapshot.contract_expiry,"deferring expired SuperTrend reversal square-off to expiry checkpoint");
            continue;
        }
        let price = match option_execution_ltp(state, &snapshot).await {
            Ok(price) => price,
            Err(error) => {
                errors.push(format!("{instrument} {trade_id}: {error}"));
                continue;
            }
        };
        let close_runner = if execution_mode == "live" {
            match protection_runner(
                state,
                runner.user_id,
                SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                &snapshot,
            )
            .await
            {
                Ok(runner) => runner,
                Err(error) => {
                    errors.push(format!("{instrument} {trade_id}: {error}"));
                    continue;
                }
            }
        } else {
            base_runner.clone()
        };
        let active_exit_order_types = active_option_exit_order_types(state, trade_id).await?;
        if active_exit_order_types
            .iter()
            .any(|order_type| order_type == "MARKET")
        {
            continue;
        }
        if !active_exit_order_types.is_empty() {
            sqlx::query("UPDATE trades SET safety_status=CASE WHEN safety_status='EMERGENCY_CLOSING' THEN safety_status ELSE 'CLOSING' END,updated_at=NOW() WHERE id=$1 AND status='open'")
                .bind(trade_id).execute(&state.db).await?;
            if let Err(error) = cancel_active_exits(state, runner.user_id, trade_id).await {
                errors.push(format!("{instrument} {trade_id}: {error}"));
                continue;
            }
            if !active_option_exit_order_types(state, trade_id)
                .await?
                .is_empty()
            {
                errors.push(format!(
                    "{instrument} {trade_id}: active protective exit is still pending cancellation"
                ));
                continue;
            }
        }
        let session = format!(
            "strev-{}-{}-{}-{}",
            config.instrument,
            now.format("%Y%m%d"),
            now.format("%H%M"),
            side.option_type()
        );
        sqlx::query("UPDATE trades SET safety_status=CASE WHEN safety_status='EMERGENCY_CLOSING' THEN safety_status ELSE 'CLOSING' END,updated_at=NOW() WHERE id=$1 AND status='open'")
            .bind(trade_id).execute(&state.db).await?;
        if let Err(error) = place_strategy_order(
            state,
            &close_runner,
            &snapshot,
            &session,
            NewOrder {
                role: "EMERGENCY_CLOSE",
                side: side.exit_side(),
                order_type: "MARKET",
                lots: remaining_lots.max(1),
                price,
                trigger: None,
                trade_id: Some(trade_id),
                quantity: Some(quantity.max(1)),
            },
        )
        .await
        {
            errors.push(format!("{instrument} {trade_id}: {error}"));
        } else {
            emit_for(
                state,
                SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                Some(runner.user_id),
                config.instrument,
                "supertrend_reversal_square_off",
                json!({"trade_id":trade_id,"square_off_at":now,"closed_side":side.option_type(),"option_execution_price":price,"reason":reason}),
            )
            .await;
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::BadRequest(errors.join("; ")))
    }
}

async fn close_lingering_expired_contract_trades(
    state: &AppState,
    now: DateTime<FixedOffset>,
) -> AppResult<()> {
    let rows: Vec<(Uuid, Uuid, String, String, Option<String>, NaiveDate)> = sqlx::query_as(
        "WITH candidates AS (
            SELECT t.id,t.user_id,t.strategy_key,t.instrument_label,t.contract_symbol,s.contract_expiry
            FROM trades t
            JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
            WHERE t.status='open'
              AND t.remaining_lots>0
              AND t.execution_mode='demo'
              AND t.strategy_key IN ($1,$2)
              AND s.contract_expiry IS NOT NULL
              AND (
                    s.contract_expiry<CURRENT_DATE
                 OR (s.contract_expiry=CURRENT_DATE AND $3::boolean)
              )
        ),
        changed AS (
            UPDATE trades t
            SET status='closed',
                safety_status='CLOSED',
                exit_price=COALESCE(t.last_price,t.entry_price),
                last_price=COALESCE(t.last_price,t.entry_price),
                pnl=(
                    t.pnl::float8
                    + CASE
                        WHEN t.direction='BUY'
                            THEN COALESCE(t.last_price,t.entry_price)::float8-t.entry_price::float8
                        ELSE t.entry_price::float8-COALESCE(t.last_price,t.entry_price)::float8
                    END
                    * CASE
                        WHEN t.strategy_key='futures_breakout_v3' AND t.instrument_label='GOLDM'
                            THEN COALESCE(NULLIF(t.remaining_lots,0)::float8*10.0,t.quantity::float8/10.0)
                        WHEN t.strategy_key='futures_breakout_v3' AND t.instrument_label='GOLDTEN'
                            THEN COALESCE(NULLIF(t.remaining_lots,0)::float8,t.quantity::float8/10.0)
                        WHEN t.strategy_key='futures_breakout_v3' AND t.instrument_label='SILVERM'
                            THEN COALESCE(NULLIF(t.remaining_lots,0)::float8*5.0,t.quantity::float8)
                        WHEN t.strategy_key='futures_breakout_v3' AND t.instrument_label='SILVERMIC'
                            THEN COALESCE(NULLIF(t.remaining_lots,0)::float8,t.quantity::float8)
                        WHEN t.strategy_key='futures_breakout_v3' AND t.instrument_label='NATGASMINI'
                            THEN COALESCE(NULLIF(t.remaining_lots,0)::float8*250.0,t.quantity::float8)
                        ELSE t.quantity::float8
                    END
                )::numeric,
                exit_datetime=$4,
                remaining_lots=0,
                exit_reason='MARKET_CLOSED',
                notes=CONCAT(COALESCE(t.notes,''), CASE WHEN COALESCE(t.notes,'')='' THEN '' ELSE '; ' END, 'Auto-closed by expiry checkpoint'),
                updated_at=NOW()
            FROM candidates c
            WHERE t.id=c.id
            RETURNING t.id,t.user_id,t.strategy_key,t.instrument_label,t.contract_symbol,c.contract_expiry
        )
        SELECT * FROM changed",
    )
    .bind(STRATEGY_KEY)
    .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
    .bind(option_square_off_due(now) || futures_expiry_checkpoint_due(now.date_naive(), now))
    .bind(now.with_timezone(&Utc))
    .fetch_all(&state.db)
    .await?;

    for (trade_id, user_id, strategy_key, instrument, contract_symbol, expiry) in rows {
        sqlx::query(
            "UPDATE strategy_orders
             SET status='cancelled',
                 broker_status='Cancelled by expiry checkpoint',
                 updated_at=NOW()
             WHERE trade_id=$1
               AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')",
        )
        .bind(trade_id)
        .execute(&state.db)
        .await?;
        emit_for(
            state,
            &strategy_key,
            Some(user_id),
            &instrument,
            "contract_expiry_checkpoint_closed",
            json!({
                "trade_id":trade_id,
                "contract_symbol":contract_symbol,
                "contract_expiry":expiry,
                "closed_at":now,
                "exit_reason":"MARKET_CLOSED"
            }),
        )
        .await;
        append_user_log(
            state,
            user_id,
            &format!(
                "CONTRACT EXPIRY CHECKPOINT closed {} [{} expiry {}] as MARKET_CLOSED",
                contract_log_label(&instrument, contract_symbol.as_deref()),
                strategy_key,
                expiry
            ),
        )
        .await;
    }
    let unresolved_live: Vec<(Uuid, Uuid, String, String, Option<String>, NaiveDate)> =
        sqlx::query_as(
            "UPDATE trades t SET safety_status='EMERGENCY_CLOSING',protection_deadline_at=NOW(),last_protection_error='Contract expiry checkpoint requires broker-confirmed flattening',updated_at=NOW()
             FROM strategy_market_snapshots s
             WHERE s.id=t.strategy_snapshot_id AND t.execution_mode='live' AND t.status='open' AND t.remaining_lots>0
               AND t.strategy_key IN ($1,$2) AND s.contract_expiry IS NOT NULL
               AND (s.contract_expiry<CURRENT_DATE OR (s.contract_expiry=CURRENT_DATE AND $3::boolean))
             RETURNING t.id,t.user_id,t.strategy_key,t.instrument_label,t.contract_symbol,s.contract_expiry",
        )
        .bind(STRATEGY_KEY)
        .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
        .bind(option_square_off_due(now) || futures_expiry_checkpoint_due(now.date_naive(), now))
        .fetch_all(&state.db)
        .await?;
    for (trade_id, user_id, strategy_key, instrument, contract_symbol, expiry) in unresolved_live {
        operational_alert_for(
            state,
            &strategy_key,
            Some(user_id),
            &instrument,
            "expired_live_trade_requires_reconciliation",
            "critical",
            &format!(
                "Live trade {trade_id} for {} reached expiry {expiry} without a confirmed broker exit. It is now durably EMERGENCY_CLOSING until the broker position is flat; reconcile immediately if the broker rejects post-expiry closure.",
                contract_log_label(&instrument, contract_symbol.as_deref())
            ),
        )
        .await;
    }
    Ok(())
}

async fn active_option_exit_order_types(
    state: &AppState,
    trade_id: Uuid,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT order_type
         FROM strategy_orders
         WHERE trade_id=$1
           AND role IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE')
           AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')",
    )
    .bind(trade_id)
    .fetch_all(&state.db)
    .await?)
}

async fn ensure_square_off_intents(
    state: &AppState,
    strategy_key: &str,
    now: DateTime<FixedOffset>,
) -> AppResult<Uuid> {
    let session_key = format!("squareoff-{}-1510", now.format("%Y%m%d"));
    let signal_at = ist_naive_to_utc(
        now.date_naive()
            .and_hms_opt(15, 10, 0)
            .ok_or_else(|| AppError::BadRequest("Invalid square-off time.".into()))?,
    )?;
    let signal_id = Uuid::new_v4();
    let stored_id: Uuid = sqlx::query_scalar(
        "INSERT INTO strategy_signals(id,strategy_key,instrument,session_key,signal_at,signal_type,expected_users,payload,status)
         SELECT $1,$2,'ALL',$3,$4,'SQUARE_OFF',COUNT(DISTINCT user_id)::int4,jsonb_build_object('scheduled_for','15:10 IST'),'dispatching'
         FROM trades WHERE strategy_key=$2 AND status='open' AND remaining_lots>0
         ON CONFLICT(strategy_key,instrument,session_key,signal_type)
         DO UPDATE SET expected_users=GREATEST(strategy_signals.expected_users,EXCLUDED.expected_users),updated_at=NOW()
         RETURNING id",
    )
    .bind(signal_id)
    .bind(strategy_key)
    .bind(&session_key)
    .bind(signal_at)
    .fetch_one(&state.db)
    .await?;
    sqlx::query(
        "INSERT INTO strategy_execution_intents(id,signal_id,user_id,snapshot_id,trade_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,status)
         SELECT gen_random_uuid(),$1,t.user_id,t.strategy_snapshot_id,t.id,t.strategy_key,
                split_part(t.instrument_label,'_',1),
                'stsq-' || $2 || '-1510',
                'SQUARE_OFF','EMERGENCY_CLOSE','SELL','MARKET',GREATEST(t.remaining_lots,1),GREATEST(t.quantity,1),t.last_price::float8,'pending'
         FROM trades t
         WHERE t.strategy_key=$3 AND t.status='open' AND t.remaining_lots>0 AND t.strategy_snapshot_id IS NOT NULL
         ON CONFLICT DO NOTHING",
    )
    .bind(stored_id)
    .bind(now.format("%Y%m%d").to_string())
    .bind(strategy_key)
    .execute(&state.db)
    .await?;
    Ok(stored_id)
}

async fn mark_square_off_intent(state: &AppState, trade_id: Uuid, status: &str, error: &str) {
    if let Err(database_error) = sqlx::query(
        "UPDATE strategy_execution_intents SET status=$2,last_error=$3,next_attempt_at=CASE WHEN $2='retry_wait' THEN NOW()+INTERVAL '5 seconds' ELSE next_attempt_at END,completed_at=CASE WHEN $2 IN ('completed','failed','skipped') THEN NOW() ELSE completed_at END,updated_at=NOW() WHERE trade_id=$1 AND action='SQUARE_OFF'",
    )
    .bind(trade_id)
    .bind(status)
    .bind(error)
    .execute(&state.db)
    .await
    {
        tracing::warn!(%trade_id,%database_error,"could not update square-off intent");
    }
}

async fn reconcile_square_off_intents(state: &AppState) -> AppResult<()> {
    sqlx::query(
        "UPDATE strategy_execution_intents i SET status='completed',last_error='',completed_at=NOW(),updated_at=NOW()
         FROM trades t WHERE i.trade_id=t.id AND i.action='SQUARE_OFF' AND t.status='closed' AND i.status<>'completed'",
    )
    .execute(&state.db)
    .await?;
    // Match the ENTRY refresh lock order while keeping the action scopes disjoint.
    let mut status_transaction = state.db.begin().await?;
    let _status_signal_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT s.id
         FROM strategy_signals s
         WHERE EXISTS(
           SELECT 1 FROM strategy_execution_intents i
           WHERE i.signal_id=s.id AND i.action='SQUARE_OFF'
         )
         ORDER BY s.id
         FOR UPDATE OF s",
    )
    .fetch_all(&mut *status_transaction)
    .await?;
    sqlx::query(
        "UPDATE strategy_signals s SET status=summary.status,updated_at=NOW()
         FROM (SELECT signal_id,CASE WHEN BOOL_OR(status IN ('pending','claimed','retry_wait','submitted')) THEN 'dispatching' WHEN BOOL_OR(status='failed') THEN 'partial' ELSE 'completed' END status
               FROM strategy_execution_intents WHERE action='SQUARE_OFF' GROUP BY signal_id) summary
         WHERE s.id=summary.signal_id",
    )
    .execute(&mut *status_transaction)
    .await?;
    status_transaction.commit().await?;
    Ok(())
}

async fn place_supertrend_entries_for_signal(
    state: &AppState,
    config: IndexOptionConfig,
    runners: &[SuperTrendRunner],
    signal: SuperTrendSignal,
    now: DateTime<FixedOffset>,
) -> AppResult<()> {
    if !supertrend_signal_is_fresh(signal, now) {
        emit_for(
            state,
            SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
            None,
            config.instrument,
            "supertrend_entry_skipped",
            json!({"signal_at":signal.signal_at,"reason":"STALE_SIGNAL","message":"The crossover was recovered after its safe entry window; no delayed trade was placed."}),
        )
        .await;
        return Ok(());
    }
    let session = format!(
        "st-{}-{}-{}-{}",
        config.instrument,
        signal.signal_at.format("%Y%m%d"),
        signal.signal_at.format("%H%M"),
        signal.side.option_type()
    );
    let user_ids: Vec<Uuid> = runners.iter().map(|runner| runner.user_id).collect();
    let processed_user_ids: HashSet<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT user_id FROM (
            SELECT user_id FROM strategy_orders
            WHERE user_id=ANY($1)
              AND session_key=$2
              AND role IN ('BUY_ENTRY','SELL_ENTRY')
              AND status<>'failed'
            UNION ALL
            SELECT user_id FROM strategy_events
            WHERE user_id=ANY($1)
              AND strategy_key=$3
              AND event_type='supertrend_entry_skipped'
              AND payload->>'session_key'=$2
              AND payload->>'reason'='SAME_SIDE_POSITION_ALREADY_OPEN'
         ) processed",
    )
    .bind(&user_ids)
    .bind(&session)
    .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let pending_runners: Vec<_> = runners
        .iter()
        .filter(|runner| !processed_user_ids.contains(&runner.user_id))
        .cloned()
        .collect();
    if pending_runners.is_empty() {
        return Ok(());
    }
    let selection = select_supertrend_market_for_signal(
        state,
        config,
        signal.side,
        signal.signal_at.date(),
        signal.signal_at,
    )
    .await?;
    if !supertrend_signal_is_fresh(signal, ist_now()) {
        emit_for(
            state,
            SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
            None,
            config.instrument,
            "supertrend_entry_skipped",
            json!({"signal_at":signal.signal_at,"reason":"STALE_DURING_CONTRACT_SELECTION","message":"ATM contract selection completed too late; no delayed trade was placed."}),
        )
        .await;
        return Ok(());
    }
    crate::market_ws::ensure_strategy_feed(
        state.clone(),
        config.option_exchange.to_string(),
        selection.contract.token.clone(),
    )
    .await;

    let option_cutoff = ist_naive_to_utc(
        signal
            .signal_at
            .date()
            .and_hms_opt(15, 19, 45)
            .ok_or_else(|| AppError::BadRequest("Invalid SuperTrend execution window.".into()))?,
    )?;
    let signal_closed_at = signal.signal_at + Duration::minutes(5);
    let expires_at = (ist_naive_to_utc(signal_closed_at)?
        + Duration::seconds(SUPERTREND_MAX_ENTRY_DELAY_SECONDS))
    .min(option_cutoff);
    let mut intents = Vec::with_capacity(pending_runners.len());
    for runner in &pending_runners {
        if runner.target_points <= 0.0 || runner.stop_loss_points <= 0.0 {
            operational_alert_for(
                state,
                SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                Some(runner.user_id),
                config.instrument,
                "supertrend_user_entry_failed",
                "error",
                "SuperTrend TP and SL points must be positive.",
            )
            .await;
            continue;
        }
        let snapshot = supertrend_option_snapshot_for_signal(
            state,
            config,
            signal.side,
            signal.signal_at.date(),
            signal.signal_at,
            runner.user_id,
            runner.target_points,
            runner.stop_loss_points,
            &selection,
        )
        .await?;
        intents.push(PreparedExecutionIntent {
            user_id: runner.user_id,
            snapshot_id: snapshot.id,
            strategy_key: SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY.into(),
            instrument: config.instrument.into(),
            session_key: session.clone(),
            action: "ENTRY",
            role: signal.side.entry_role(),
            side: signal.side.entry_side(),
            order_type: "MARKET",
            lots: runner.lots,
            quantity: None,
            price: selection.contract.premium,
            trigger_price: None,
            trade_id: None,
            expires_at: Some(expires_at),
        });
    }
    if intents.is_empty() {
        return Ok(());
    }
    emit_for(
        state,
        SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
        None,
        config.instrument,
        "supertrend_signal",
        json!({
            "side":signal.side.option_type(),"signal_at":signal.signal_at,
            "index_close":signal.index_close,"index_ltp":selection.underlying_ltp,
            "supertrend":signal.supertrend,"previous_direction":signal.previous_direction.as_str(),
            "direction":signal.direction.as_str(),"option_execution_price":selection.contract.premium,
            "contract_symbol":selection.contract.symbol,"expected_users":intents.len()
        }),
    )
    .await;
    let (signal_id, _) = materialize_signal_intents(
        state,
        SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
        config.instrument,
        &session,
        "ENTRY",
        ist_naive_to_utc(signal.signal_at)?,
        None,
        json!({
            "side":signal.side.option_type(),"index_close":signal.index_close,
            "supertrend":signal.supertrend,"option_execution_price":selection.contract.premium,
            "contract_symbol":selection.contract.symbol,"expected_users":intents.len()
        }),
        &intents,
    )
    .await?;
    process_execution_intents(state, Some(signal_id)).await?;
    Ok(())
}

async fn process_supertrend_instrument(
    state: &AppState,
    config: IndexOptionConfig,
    now: DateTime<FixedOffset>,
) -> AppResult<()> {
    let runners = supertrend_runners(state, config).await?;
    if runners.is_empty() {
        return Ok(());
    }
    crate::market_ws::ensure_strategy_feed(
        state.clone(),
        config.index_exchange.to_owned(),
        config.index_token.to_owned(),
    )
    .await;
    let candles =
        index_candles(state, config, Duration::days(SUPERTREND_LOOKBACK_DAYS), now).await?;
    let points = supertrend_points(&candles, SUPERTREND_ATR_PERIOD, SUPERTREND_FACTOR);
    let Some(signal) = current_supertrend_signal(&points, now) else {
        return Ok(());
    };
    let date = now.date_naive();
    if signal.signal_at.date() != date {
        return Ok(());
    }
    let runners = supertrend_runners(state, config).await?;
    if runners.is_empty() {
        return Ok(());
    }
    place_supertrend_entries_for_signal(state, config, &runners, signal, now).await
}

async fn process_supertrend_square_off(
    state: &AppState,
    now: DateTime<FixedOffset>,
) -> AppResult<()> {
    ensure_square_off_intents(state, SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY, now).await?;
    type SquareOffTradeRow = (Uuid, Uuid, String, String, i32, i32, Option<Uuid>);
    let trades: Vec<SquareOffTradeRow> = sqlx::query_as("SELECT id,user_id,instrument_label,execution_mode,quantity,remaining_lots,strategy_snapshot_id FROM trades WHERE strategy_key=$1 AND status='open' AND remaining_lots>0 AND strategy_snapshot_id IS NOT NULL")
        .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
        .fetch_all(&state.db)
        .await?;
    let mut errors = Vec::new();
    for (trade_id, user_id, instrument, execution_mode, quantity, remaining_lots, snapshot_id) in
        trades
    {
        let Some(snapshot_id) = snapshot_id else {
            continue;
        };
        let Some(underlying) = supertrend_snapshot_underlying(&instrument) else {
            continue;
        };
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await?;
        // Resolve a usable quote before touching the acknowledged protective
        // order. A quote failure therefore leaves the existing stop intact.
        let price = match option_execution_ltp(state, &snapshot).await {
            Ok(price) => price,
            Err(error) => {
                let message = error.to_string();
                mark_square_off_intent(state, trade_id, "retry_wait", &message).await;
                errors.push(format!("{instrument} {trade_id}: {message}"));
                continue;
            }
        };
        let runner = if execution_mode == "live" {
            protection_runner(
                state,
                user_id,
                SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                &snapshot,
            )
            .await
        } else {
            runner_for_strategy(
                state,
                user_id,
                SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                underlying,
            )
            .await
        };
        let runner = match runner {
            Ok(runner) => runner,
            Err(error) => {
                let message = error.to_string();
                mark_square_off_intent(state, trade_id, "retry_wait", &message).await;
                errors.push(format!("{instrument} {trade_id}: {message}"));
                continue;
            }
        };
        let active_exit_order_types = active_option_exit_order_types(state, trade_id).await?;
        if active_exit_order_types
            .iter()
            .any(|order_type| order_type == "MARKET")
        {
            mark_square_off_intent(state, trade_id, "submitted", "").await;
            continue;
        }
        if !active_exit_order_types.is_empty() {
            sqlx::query("UPDATE trades SET safety_status=CASE WHEN safety_status='EMERGENCY_CLOSING' THEN safety_status ELSE 'CLOSING' END,updated_at=NOW() WHERE id=$1 AND status='open'")
                .bind(trade_id).execute(&state.db).await?;
            if let Err(error) = cancel_active_exits(state, user_id, trade_id).await {
                let message = error.to_string();
                mark_square_off_intent(state, trade_id, "retry_wait", &message).await;
                errors.push(format!("{instrument} {trade_id}: {message}"));
                continue;
            }
            if !active_option_exit_order_types(state, trade_id)
                .await?
                .is_empty()
            {
                mark_square_off_intent(
                    state,
                    trade_id,
                    "retry_wait",
                    "Waiting for protective-order cancellation confirmation.",
                )
                .await;
                continue;
            }
        }
        sqlx::query("UPDATE trades SET safety_status=CASE WHEN safety_status='EMERGENCY_CLOSING' THEN safety_status ELSE 'CLOSING' END,updated_at=NOW() WHERE id=$1 AND status='open'")
            .bind(trade_id).execute(&state.db).await?;
        trip_execution_failpoint("after_square_off_intent_before_market_close").await?;
        let base_session = format!("stsq-{}-{}-1510", underlying, now.format("%Y%m%d"));
        let session =
            terminal_retry_session(state, trade_id, "EMERGENCY_CLOSE", &base_session).await?;
        if let Err(error) = place_strategy_order(
            state,
            &runner,
            &snapshot,
            &session,
            NewOrder {
                role: "EMERGENCY_CLOSE",
                side: "SELL",
                order_type: "MARKET",
                lots: remaining_lots.max(1),
                price,
                trigger: None,
                trade_id: Some(trade_id),
                quantity: Some(quantity.max(1)),
            },
        )
        .await
        {
            let message = error.to_string();
            sqlx::query("UPDATE trades SET safety_status='EMERGENCY_CLOSING',last_protection_error=$2,updated_at=NOW() WHERE id=$1 AND status='open'")
                .bind(trade_id).bind(&message).execute(&state.db).await?;
            mark_square_off_intent(state, trade_id, "retry_wait", &message).await;
            errors.push(message);
        } else {
            mark_square_off_intent(state, trade_id, "submitted", "").await;
            emit_for(
                state,
                SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                Some(user_id),
                underlying,
                "supertrend_intraday_square_off",
                json!({"trade_id":trade_id,"square_off_at":now,"option_execution_price":price}),
            )
            .await;
        }
    }
    reconcile_square_off_intents(state).await?;
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "SuperTrend square-off had {} non-fatal error(s): {}",
            errors.len(),
            errors.join("; ")
        )))
    }
}

async fn run_supertrend_cycle(state: &AppState, now: DateTime<FixedOffset>) -> AppResult<()> {
    let (open, reason) = session_is_open(state, now.date_naive(), "day").await?;
    if !open {
        operational_alert_for(
            state,
            SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
            None,
            "",
            "session_skipped",
            "warning",
            &format!("SuperTrend strategy skipped: {reason}"),
        )
        .await;
        return Ok(());
    }
    if option_square_off_due(now) {
        return Ok(());
    }
    if !supertrend_entry_allowed(now) {
        return Ok(());
    }
    let mut tasks = tokio::task::JoinSet::new();
    for instrument in ["SENSEX", "NIFTY"] {
        let Some(config) = index_option_config(instrument) else {
            continue;
        };
        let cloned = state.clone();
        tasks.spawn(async move {
            (
                instrument,
                process_supertrend_instrument(&cloned, config, now).await,
            )
        });
    }
    let mut errors = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok((instrument, Ok(()))) => {
                tracing::debug!(instrument, "SuperTrend instrument cycle completed");
            }
            Ok((instrument, Err(error))) => {
                tracing::warn!(
                    instrument,
                    %error,
                    "SuperTrend instrument cycle failed; the other instrument remains independent"
                );
                errors.push(format!("{instrument}: {error}"));
            }
            Err(error) => {
                tracing::warn!(%error, "SuperTrend instrument task failed");
                errors.push(format!("instrument task: {error}"));
            }
        }
    }
    if !errors.is_empty() {
        return Err(AppError::BadRequest(format!(
            "SuperTrend cycle had {} non-fatal instrument error(s): {}",
            errors.len(),
            errors.join("; ")
        )));
    }
    Ok(())
}

async fn runner_for(state: &AppState, user_id: Uuid, instrument: &str) -> AppResult<Runner> {
    runner_for_strategy(state, user_id, STRATEGY_KEY, instrument).await
}

pub(crate) async fn runner_for_strategy(
    state: &AppState,
    user_id: Uuid,
    strategy_key: &str,
    instrument: &str,
) -> AppResult<Runner> {
    Ok(sqlx::query_as("SELECT c.user_id,u.username,c.instrument,c.lots,c.run_day_session,c.run_evening_session,p.trading_mode FROM user_strategy_configs c JOIN users u ON u.id=c.user_id JOIN user_profiles p ON p.user_id=c.user_id WHERE c.user_id=$1 AND c.strategy_key=$2 AND c.instrument=$3 AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))")
        .bind(user_id).bind(strategy_key).bind(instrument).fetch_one(&state.db).await?)
}

#[derive(Debug, FromRow)]
struct OpenTrade {
    id: Uuid,
    user_id: Uuid,
    direction: String,
    quantity: i32,
    remaining_lots: i32,
    total_lots: i32,
    target_done: bool,
    entry_datetime: Option<DateTime<Utc>>,
    entry_price: f64,
    target_price: Option<f64>,
    reversal_of_trade_id: Option<Uuid>,
    strategy_snapshot_id: Option<Uuid>,
    instrument_label: String,
}

fn carry_exit_role(action: &str, target_done: bool) -> Option<&'static str> {
    match (action, target_done) {
        ("TARGET", false) => Some("TARGET"),
        ("TARGET", true) => None,
        ("STOP", false) => Some("SL1"),
        ("STOP", true) => Some("SL2"),
        _ => None,
    }
}

fn may_submit_exit_replacement(has_previous_nonterminal_order: bool) -> bool {
    !has_previous_nonterminal_order
}

fn recorded_exit_reason(strategy_key: &str, role: &str, session_key: &str) -> &'static str {
    if session_key.starts_with("stsq-") {
        "MARKET_CLOSED"
    } else if session_key.starts_with("strev-") {
        "SIGNAL_REVERSAL"
    } else if session_key.starts_with("mc-") {
        "MANUAL_RULENIX_CLOSE"
    } else if role == "EMERGENCY_CLOSE" {
        "EMERGENCY_CLOSE"
    } else if strategy_key == STRATEGY_KEY {
        match role {
            "TARGET" => "TP1",
            "SL2" => "SL2",
            _ => "SL1",
        }
    } else if role == "TARGET" {
        "TP"
    } else {
        "SL"
    }
}

async fn cancel_active_exit_role(
    state: &AppState,
    user_id: Uuid,
    trade_id: Uuid,
    target_role: &str,
    exclude_session: &str,
) -> AppResult<bool> {
    let orders: Vec<(Uuid, String, String, String)> = if target_role == "TARGET" {
        sqlx::query_as("SELECT id,broker_order_id,execution_mode,order_type FROM strategy_orders WHERE trade_id=$1 AND role='TARGET' AND session_key<>$2 AND status IN ('submitted','partially_filled')")
            .bind(trade_id)
            .bind(exclude_session)
            .fetch_all(&state.db)
            .await?
    } else {
        sqlx::query_as("SELECT id,broker_order_id,execution_mode,order_type FROM strategy_orders WHERE trade_id=$1 AND role IN ('SL1','SL2') AND session_key<>$2 AND status IN ('submitted','partially_filled')")
            .bind(trade_id)
            .bind(exclude_session)
            .fetch_all(&state.db)
            .await?
    };
    cancel_exit_orders(state, user_id, orders).await?;
    let active: bool = if target_role == "TARGET" {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=$1 AND role='TARGET' AND session_key<>$2 AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))")
            .bind(trade_id)
            .bind(exclude_session)
            .fetch_one(&state.db)
            .await?
    } else {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=$1 AND role IN ('SL1','SL2') AND session_key<>$2 AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))")
            .bind(trade_id)
            .bind(exclude_session)
            .fetch_one(&state.db)
            .await?
    };
    Ok(may_submit_exit_replacement(active))
}

async fn place_carry_orders(
    state: &AppState,
    date: NaiveDate,
    session: &str,
    role: &str,
    instrument: &str,
) -> AppResult<()> {
    let snapshot = create_snapshot(state, instrument, date).await?;
    if snapshot.status != "ready" {
        return Err(AppError::BadRequest(
            snapshot
                .error
                .clone()
                .unwrap_or_else(|| "Strategy snapshot is not ready.".into()),
        ));
    }
    let trades: Vec<OpenTrade> = sqlx::query_as("SELECT trade.id,trade.user_id,trade.direction,trade.quantity,trade.remaining_lots,trade.total_lots,EXISTS(SELECT 1 FROM strategy_orders target_order WHERE target_order.trade_id=trade.id AND target_order.role='TARGET' AND target_order.processed_quantity>0) AS target_done,trade.entry_datetime,trade.entry_price::float8 AS entry_price,trade.target_price::float8 AS target_price,trade.reversal_of_trade_id,trade.strategy_snapshot_id,trade.instrument_label FROM trades trade WHERE trade.status='open' AND trade.strategy_key=$1 AND trade.instrument_label=$2 AND trade.remaining_lots>0")
        .bind(STRATEGY_KEY).bind(instrument).fetch_all(&state.db).await?;
    let mut errors = Vec::new();
    for trade in trades {
        if trade.strategy_snapshot_id.is_none() {
            continue;
        }
        let runner = runner_for(state, trade.user_id, &trade.instrument_label).await?;
        let Some(exit_role) = carry_exit_role(role, trade.target_done) else {
            continue;
        };
        let entry_date = trade.entry_datetime.map(|value| {
            value
                .with_timezone(&FixedOffset::east_opt(19_800).expect("valid IST offset"))
                .date_naive()
        });
        let exit_levels = match if entry_date == Some(date) {
            snapshot_order_exit_levels(
                &snapshot,
                &trade.direction,
                trade.entry_price,
                trade.reversal_of_trade_id.is_some(),
            )
        } else {
            snapshot_exit_levels(
                &snapshot,
                &trade.direction,
                trade.entry_price,
                trade.reversal_of_trade_id.is_some(),
            )
        } {
            Ok(levels) => levels,
            Err(error) => {
                errors.push(error.to_string());
                continue;
            }
        };
        let (side, price, trigger) = match (trade.direction.as_str(), exit_role) {
            ("BUY", "TARGET") => ("SELL", trade.target_price, None),
            ("SELL", "TARGET") => ("BUY", trade.target_price, None),
            ("BUY", "SL1") => ("SELL", Some(exit_levels.sl1), Some(exit_levels.sl1)),
            ("SELL", "SL1") => ("BUY", Some(exit_levels.sl1), Some(exit_levels.sl1)),
            ("BUY", "SL2") => ("SELL", Some(exit_levels.sl2), Some(exit_levels.sl2)),
            ("SELL", "SL2") => ("BUY", Some(exit_levels.sl2), Some(exit_levels.sl2)),
            _ => continue,
        };
        if let Some(price) = price.filter(|value| value.is_finite() && *value > 0.0) {
            let key = format!("carry-{}-{}", date, session);
            let lots = if exit_role == "TARGET" {
                target_exit_lots(trade.total_lots)
            } else {
                trade.remaining_lots
            };
            if exit_role != "TARGET" {
                current_contract_order_metadata(state, &snapshot, false).await?;
                sqlx::query("UPDATE trades SET safety_status=CASE WHEN execution_mode='live' THEN 'PROTECTION_SUBMITTING' ELSE safety_status END,protection_deadline_at=CASE WHEN execution_mode='live' THEN NOW()+($2::text || ' seconds')::interval ELSE protection_deadline_at END,protection_attempts=CASE WHEN execution_mode='live' THEN protection_attempts+1 ELSE protection_attempts END,updated_at=NOW() WHERE id=$1 AND status='open'")
                    .bind(trade.id).bind(state.config.protection_ack_timeout_seconds).execute(&state.db).await?;
            }
            match cancel_active_exit_role(state, trade.user_id, trade.id, role, &key).await {
                Ok(true) => {}
                Ok(false) => {
                    errors.push(format!(
                        "Trade {} is waiting for the previous {exit_role} broker order cancellation to be confirmed.",
                        trade.id
                    ));
                    continue;
                }
                Err(error) => {
                    errors.push(error.to_string());
                    continue;
                }
            }
            if let Err(error) = place_strategy_order(
                state,
                &runner,
                &snapshot,
                &key,
                NewOrder {
                    role: exit_role,
                    side,
                    order_type: if exit_role == "TARGET" {
                        "LIMIT"
                    } else {
                        "STOPLOSS_MARKET"
                    },
                    lots,
                    price,
                    trigger,
                    trade_id: Some(trade.id),
                    quantity: Some(if exit_role == "TARGET" {
                        (lots * snapshot.lot_size.unwrap_or(1).max(1)).min(trade.quantity)
                    } else {
                        trade.quantity
                    }),
                },
            )
            .await
            {
                if exit_role != "TARGET" {
                    sqlx::query("UPDATE trades SET safety_status=CASE WHEN execution_mode='live' THEN 'PROTECTION_FAILED' ELSE safety_status END,last_protection_error=$2,updated_at=NOW() WHERE id=$1 AND status='open'")
                        .bind(trade.id).bind(error.to_string()).execute(&state.db).await?;
                    operational_alert_for(state,STRATEGY_KEY,Some(trade.user_id),instrument,"carry_stop_replacement_failed","critical",&format!("Carry stop replacement failed after cancellation; recovery/emergency close is active: {error}")).await;
                }
                errors.push(error.to_string());
                continue;
            }
            if exit_role == "TARGET" {
                sqlx::query("UPDATE trades SET strategy_snapshot_id=$2,updated_at=NOW() WHERE id=$1 AND status='open'")
                    .bind(trade.id)
                    .bind(snapshot.id)
                    .execute(&state.db)
                    .await?;
            } else {
                sqlx::query("UPDATE trades SET strategy_snapshot_id=$2,sl1_price=$3,sl2_price=$4,updated_at=NOW() WHERE id=$1 AND status='open'")
                    .bind(trade.id)
                    .bind(snapshot.id)
                    .bind(exit_levels.sl1)
                    .bind(exit_levels.sl2)
                    .execute(&state.db)
                    .await?;
            }
        } else {
            errors.push(format!(
                "Trade {} has no valid fixed {exit_role} price.",
                trade.id
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::BadRequest(errors.join("; ")))
    }
}

fn ist_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&FixedOffset::east_opt(19_800).expect("valid IST offset"))
}

async fn session_is_open(
    state: &AppState,
    date: NaiveDate,
    session: &str,
) -> AppResult<(bool, String)> {
    let override_row: Option<(bool, bool, String)> = sqlx::query_as(
        "SELECT morning_open,evening_open,reason FROM market_calendar WHERE trade_date=$1",
    )
    .bind(date)
    .fetch_optional(&state.db)
    .await?;
    if let Some((morning, evening, reason)) = override_row {
        return Ok((if session == "day" { morning } else { evening }, reason));
    }
    let weekend = matches!(date.weekday(), Weekday::Sat | Weekday::Sun);
    let reason = if weekend { "Weekend" } else { "" };
    Ok((!weekend, reason.into()))
}

fn scheduler_session_flags(
    calendar: AppResult<((bool, String), (bool, String))>,
) -> AppResult<(bool, bool)> {
    calendar.map(|(day, evening)| (day.0, evening.0))
}

async fn futures_runtime_is_open(
    state: &AppState,
    now: DateTime<FixedOffset>,
) -> AppResult<(bool, String)> {
    let date = now.date_naive();
    let minute = now.hour() * 60 + now.minute();
    let (day_open, day_reason) = session_is_open(state, date, "day").await?;
    let (evening_open, evening_reason) = session_is_open(state, date, "evening").await?;
    if day_open && (9 * 60..=15 * 60 + 20).contains(&minute) {
        return Ok((true, String::new()));
    }
    if evening_open && (17 * 60..=23 * 60 + 25).contains(&minute) {
        return Ok((true, String::new()));
    }
    let reason = if !day_open && !evening_open {
        if !day_reason.is_empty() {
            day_reason
        } else if !evening_reason.is_empty() {
            evening_reason
        } else {
            "market session is closed".into()
        }
    } else {
        "market session is closed".into()
    };
    Ok((false, reason))
}

async fn mark_run_skipped(
    state: &AppState,
    instrument: &str,
    date: NaiveDate,
    session: &str,
    action: &str,
    scheduled_for: DateTime<FixedOffset>,
    reason: &str,
) -> AppResult<()> {
    let changed = sqlx::query("INSERT INTO strategy_scheduler_runs (id,strategy_key,instrument,trade_date,session_key,action,status,scheduled_for,next_attempt_at,completed_at,last_error) VALUES ($1,$2,$3,$4,$5,$6,'skipped',$7,NOW(),NOW(),$8) ON CONFLICT (strategy_key,instrument,trade_date,session_key,action) DO UPDATE SET status='skipped',completed_at=NOW(),last_error=EXCLUDED.last_error,updated_at=NOW() WHERE strategy_scheduler_runs.status NOT IN ('completed','skipped')")
        .bind(Uuid::new_v4()).bind(STRATEGY_KEY).bind(instrument).bind(date).bind(session).bind(action).bind(scheduled_for).bind(reason)
        .execute(&state.db).await?;
    if changed.rows_affected() > 0 && reason != "Weekend" {
        operational_alert(
            state,
            None,
            instrument,
            "session_skipped",
            "warning",
            &format!("{session} {action} skipped: {reason}"),
        )
        .await;
    }
    Ok(())
}

async fn run_scheduled_action(
    state: &AppState,
    instrument: &str,
    date: NaiveDate,
    session: &'static str,
    action: &str,
    scheduled_for: DateTime<FixedOffset>,
) -> AppResult<()> {
    sqlx::query("INSERT INTO strategy_scheduler_runs (id,strategy_key,instrument,trade_date,session_key,action,status,scheduled_for,next_attempt_at) VALUES ($1,$2,$3,$4,$5,$6,'pending',$7,NOW()) ON CONFLICT (strategy_key,instrument,trade_date,session_key,action) DO NOTHING")
        .bind(Uuid::new_v4()).bind(STRATEGY_KEY).bind(instrument).bind(date).bind(session).bind(action).bind(scheduled_for)
        .execute(&state.db).await?;
    let claimed: Option<(Uuid, i32)> = sqlx::query_as("UPDATE strategy_scheduler_runs SET status='running',attempts=attempts+1,started_at=NOW(),updated_at=NOW() WHERE strategy_key=$1 AND instrument=$2 AND trade_date=$3 AND session_key=$4 AND action=$5 AND status IN ('pending','failed') AND next_attempt_at<=NOW() RETURNING id,attempts")
        .bind(STRATEGY_KEY).bind(instrument).bind(date).bind(session).bind(action)
        .fetch_optional(&state.db).await?;
    let Some((run_id, attempts)) = claimed else {
        return Ok(());
    };
    let result = match action {
        "target" => place_carry_orders(state, date, session, "TARGET", instrument).await,
        "stop" => place_carry_orders(state, date, session, "STOP", instrument).await,
        "entry" => {
            run_entries(
                state.clone(),
                instrument.to_string(),
                date,
                session,
                session == "evening",
            )
            .await
        }
        "gap_entry" => {
            run_entries(state.clone(), instrument.to_string(), date, session, true).await
        }
        _ => Err(AppError::BadRequest(format!(
            "Unknown strategy scheduler action: {action}"
        ))),
    };
    match result {
        Ok(()) => {
            sqlx::query("UPDATE strategy_scheduler_runs SET status='completed',completed_at=NOW(),last_error='',updated_at=NOW() WHERE id=$1")
                .bind(run_id).execute(&state.db).await?;
        }
        Err(error) => {
            let message = error.to_string();
            let delay_seconds = recoverable_retry_delay_seconds(&message, attempts);
            let severity = retry_alert_severity(&message);
            sqlx::query("UPDATE strategy_scheduler_runs SET status='failed',next_attempt_at=NOW()+($3::int * INTERVAL '1 second'),last_error=$2,updated_at=NOW() WHERE id=$1")
                .bind(run_id)
                .bind(&message)
                .bind(delay_seconds)
                .execute(&state.db)
                .await?;
            operational_alert(
                state,
                None,
                instrument,
                "scheduler_retry",
                severity,
                &format!(
                    "{session} {action} failed; retrying in about {delay_seconds} seconds: {message}"
                ),
            )
            .await;
        }
    }
    Ok(())
}

async fn schedule_session(
    state: &AppState,
    now: DateTime<FixedOffset>,
    instrument: &str,
    session: &'static str,
    base_hour: u32,
) -> AppResult<()> {
    let date = now.date_naive();
    let (open, reason) = session_is_open(state, date, session).await?;
    let current_minute = now.hour() * 60 + now.minute();
    let mut actions = vec![("target", 0_u32), ("stop", 10_u32), ("entry", 10_u32)];
    if session == "day" {
        actions.push(("gap_entry", 16_u32));
    }
    for (action, minute_offset) in actions {
        let due_minute = base_hour * 60 + minute_offset;
        if current_minute < due_minute {
            continue;
        }
        let time = NaiveTime::from_hms_opt(base_hour, minute_offset, 0).expect("valid schedule");
        let scheduled_for = now
            .offset()
            .from_local_datetime(&date.and_time(time))
            .single()
            .expect("unambiguous IST schedule");
        if !open {
            mark_run_skipped(
                state,
                instrument,
                date,
                session,
                action,
                scheduled_for,
                &reason,
            )
            .await?;
        } else if within_catchup_window(current_minute, due_minute) {
            run_scheduled_action(state, instrument, date, session, action, scheduled_for).await?;
        } else {
            mark_run_skipped(
                state,
                instrument,
                date,
                session,
                action,
                scheduled_for,
                "safe 15-minute catch-up window elapsed",
            )
            .await?;
        }
    }
    Ok(())
}

fn within_catchup_window(current_minute: u32, due_minute: u32) -> bool {
    current_minute >= due_minute && current_minute <= due_minute + 15
}

fn recoverable_retry_delay_seconds(message: &str, attempts: i32) -> i32 {
    let lower = message.to_ascii_lowercase();
    if angel::is_rate_limit_error(message) {
        return 300;
    }
    if angel::is_authentication_error(message)
        || lower.contains("no connected angel one session")
        || lower.contains("broker session health is unsafe")
    {
        return 15 * 60;
    }
    if lower.contains("market data is temporarily unavailable")
        || lower.contains("no fresh valid market price")
        || lower.contains("shared market")
        || lower.contains("temporarily unavailable")
    {
        return 5 * 60;
    }
    let exponential = 30_i32.saturating_mul(2_i32.saturating_pow(attempts.clamp(0, 6) as u32));
    exponential.clamp(30, 30 * 60)
}

fn retry_alert_severity(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if angel::is_authentication_error(message)
        || angel::is_rate_limit_error(message)
        || lower.contains("no connected angel one session")
        || lower.contains("market data is temporarily unavailable")
        || lower.contains("no fresh valid market price")
    {
        "warning"
    } else {
        "error"
    }
}

struct BackgroundLease(Arc<AtomicBool>);

impl BackgroundLease {
    fn try_acquire(flag: &Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(flag.clone()))
    }
}

impl Drop for BackgroundLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Default)]
struct SchedulerLeaseRegistry(Arc<StdMutex<HashMap<String, Arc<AtomicBool>>>>);

impl SchedulerLeaseRegistry {
    fn try_acquire(&self, key: impl Into<String>) -> Option<BackgroundLease> {
        let key = key.into();
        let flag = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(key)
            .or_default()
            .clone();
        BackgroundLease::try_acquire(&flag)
    }
}

#[derive(Clone, Default)]
struct SchedulerDispatchTracker(Arc<StdMutex<HashSet<String>>>);

impl SchedulerDispatchTracker {
    fn completed(&self, key: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(key)
    }

    fn mark_completed(&self, key: String) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key);
    }

    fn retain_date(&self, date: NaiveDate) {
        let prefix = date.to_string();
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|key| key.starts_with(&prefix));
    }
}

#[derive(Clone, Default)]
struct SchedulerJobRegistry {
    leases: SchedulerLeaseRegistry,
    dispatches: SchedulerDispatchTracker,
}

fn spawn_scheduler_job<F>(
    state: AppState,
    jobs: &SchedulerJobRegistry,
    key: impl Into<String>,
    completion_key: Option<String>,
    job_name: &'static str,
    timeout: Option<std::time::Duration>,
    job: F,
) -> bool
where
    F: Future<Output = AppResult<()>> + Send + 'static,
{
    if completion_key
        .as_deref()
        .is_some_and(|key| jobs.dispatches.completed(key))
    {
        return false;
    }
    let Some(lease) = jobs.leases.try_acquire(key) else {
        return false;
    };
    state.scheduler_health.record_dispatch();
    let dispatches = jobs.dispatches.clone();
    tokio::spawn(async move {
        // The monitor owns the lease, so a worker panic also releases it and a
        // later scheduler tick can retry without restarting the service.
        let outcome = tokio::spawn(async move {
            if let Some(timeout) = timeout {
                tokio::time::timeout(timeout, job)
                    .await
                    .map(Some)
                    .unwrap_or(None)
            } else {
                Some(job.await)
            }
        })
        .await;
        match outcome {
            Ok(Some(Ok(()))) => {
                if let Some(key) = completion_key {
                    dispatches.mark_completed(key);
                }
                state
                    .scheduler_health
                    .record_dispatch_success(Utc::now().timestamp());
            }
            Ok(Some(Err(error))) => {
                state.scheduler_health.record_dispatch_error();
                tracing::warn!(job = job_name, %error, "scheduler job failed and will retry");
            }
            Ok(None) => {
                state.scheduler_health.record_dispatch_error();
                tracing::error!(job = job_name, "scheduler job timed out and will retry");
            }
            Err(error) => {
                state.scheduler_health.record_dispatch_error();
                tracing::error!(job = job_name, %error, "scheduler job panicked and will retry");
            }
        }
        drop(lease);
    });
    true
}

struct SchedulerLeadershipGuard(Arc<crate::state::SchedulerHealth>);

impl Drop for SchedulerLeadershipGuard {
    fn drop(&mut self) {
        self.0.leadership_lost();
    }
}

pub fn start(state: AppState) {
    let watchdog_state = state.clone();
    tokio::spawn(async move {
        let mut timer = interval(std::time::Duration::from_secs(30));
        let mut alerted = false;
        loop {
            timer.tick().await;
            let snapshot = watchdog_state
                .scheduler_health
                .snapshot_at(Utc::now().timestamp());
            if snapshot.stale && !alerted {
                alerted = true;
                let age = Utc::now()
                    .timestamp()
                    .saturating_sub(snapshot.last_advance_epoch.unwrap_or_default());
                tracing::error!(age_seconds = age, "strategy scheduler heartbeat is stale");
                operational_alert_for(
                    &watchdog_state,
                    STRATEGY_KEY,
                    None,
                    "",
                    "strategy_scheduler_stalled",
                    "critical",
                    &format!("Strategy scheduler has not advanced for {age} seconds."),
                )
                .await;
            } else if !snapshot.stale {
                alerted = false;
            }
        }
    });

    tokio::spawn(async move {
        loop {
            state.scheduler_health.leadership_lost();
            let runner = tokio::spawn(run_scheduler_leader(state.clone())).await;
            state.scheduler_health.leadership_lost();
            match runner {
                Ok(()) => tracing::warn!("strategy scheduler leader loop exited; restarting"),
                Err(error) => {
                    state.scheduler_health.record_dispatch_error();
                    tracing::error!(%error, "strategy scheduler leader loop panicked; restarting");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
}

async fn run_scheduler_leader(state: AppState) {
    let mut leader_connection = loop {
        match state.db.acquire().await {
            Ok(mut connection) => {
                // Advisory locks are session-scoped. Never return this
                // connection to the pool with the leadership lock held.
                connection.close_on_drop();
                let acquired: bool = sqlx::query_scalar(
                    "SELECT pg_try_advisory_lock(hashtext('rulenix:strategy_scheduler'))",
                )
                .fetch_one(&mut *connection)
                .await
                .unwrap_or(false);
                if acquired {
                    tracing::info!("strategy scheduler leadership acquired");
                    break connection;
                }
                operational_alert(
                    &state,
                    None,
                    "",
                    "scheduler_leadership_unavailable",
                    "warning",
                    "This backend replica is not the active scheduler leader.",
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(%error, "could not acquire scheduler leadership connection");
                operational_alert(
                    &state,
                    None,
                    "",
                    "scheduler_leadership_loss",
                    "error",
                    "The backend could not acquire a database connection for scheduler leadership.",
                )
                .await;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    };
    state
        .scheduler_health
        .leadership_acquired(Utc::now().timestamp());
    let _leadership = SchedulerLeadershipGuard(state.scheduler_health.clone());
    let mut timer = interval(std::time::Duration::from_secs(5));
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let jobs = SchedulerJobRegistry::default();
    let startup_state = state.clone();
    spawn_scheduler_job(
        state.clone(),
        &jobs,
        "startup-recovery",
        Some("startup-recovery".into()),
        "startup recovery",
        Some(std::time::Duration::from_secs(30)),
        async move {
            sqlx::query("UPDATE strategy_scheduler_runs SET status='failed',next_attempt_at=NOW(),last_error='Backend restarted while this action was running',updated_at=NOW() WHERE status='running'")
                    .execute(&startup_state.db).await?;
            sqlx::query("UPDATE strategy_execution_intents SET status='retry_wait',next_attempt_at=NOW(),last_error='Backend restarted while this execution intent was claimed.',updated_at=NOW() WHERE status='claimed'")
                    .execute(&startup_state.db).await?;
            Ok(())
        },
    );
    loop {
        timer.tick().await;
        let leader_alive = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&mut *leader_connection),
        )
        .await
        .is_ok_and(|result| result.is_ok());
        if !leader_alive {
            tracing::error!("scheduler leadership connection was lost; entering re-election");
            return;
        }
        state
            .scheduler_health
            .record_advance(Utc::now().timestamp());
        let now = ist_now();
        let date = now.date_naive();
        jobs.dispatches.retain_date(date);

        let expire_key = format!("{date}:expire");
        let expire_state = state.clone();
        spawn_scheduler_job(
            state.clone(),
            &jobs,
            "daily-expiry",
            Some(expire_key.clone()),
            "prior-day DEMO order expiry",
            Some(std::time::Duration::from_secs(30)),
            async move {
                sqlx::query("UPDATE strategy_orders o SET status='cancelled',broker_status='Demo DAY order expired',updated_at=NOW() FROM strategy_market_snapshots s WHERE s.id=o.snapshot_id AND s.trade_date<$1 AND o.execution_mode='demo' AND o.status IN ('pending','submitted')")
                        .bind(date).execute(&expire_state.db).await?;
                Ok(())
            },
        );

        let expiry_key = format!("{date}:contract-expiry-checkpoint:{}", now.format("%H%M"));
        let expiry_state = state.clone();
        spawn_scheduler_job(
            state.clone(),
            &jobs,
            "expired-contract-close",
            Some(expiry_key),
            "expired contract close",
            None,
            async move { close_lingering_expired_contract_trades(&expiry_state, now).await },
        );

        let contract_bucket = format!("{date}:contracts:{}:{}", now.hour(), now.minute() / 5);
        let contract_state = state.clone();
        spawn_scheduler_job(
            state.clone(),
            &jobs,
            "futures-contract-metadata",
            Some(contract_bucket),
            "Futures contract metadata",
            Some(std::time::Duration::from_secs(120)),
            async move {
                ensure_supported_contract_metadata(&contract_state, date)
                    .await
                    .map(|_| ())
            },
        );

        let option_contract_bucket = format!(
            "{date}:supertrend-option-contracts:{}:{}",
            now.hour(),
            now.minute() / 5
        );
        let option_contract_state = state.clone();
        spawn_scheduler_job(
            state.clone(),
            &jobs,
            "supertrend-contract-metadata",
            Some(option_contract_bucket),
            "SuperTrend contract metadata",
            Some(std::time::Duration::from_secs(120)),
            async move {
                ensure_supertrend_option_contract_metadata(&option_contract_state, date).await
            },
        );

        if (now.hour(), now.minute()) >= (8, 30) {
            for instrument in FUTURES_BREAKOUT_INSTRUMENTS {
                let snapshot_key =
                    format!("{date}:levels:{instrument}:{}:{}", now.hour(), now.minute());
                let snapshot_state = state.clone();
                spawn_scheduler_job(
                    state.clone(),
                    &jobs,
                    format!("snapshot:{instrument}"),
                    Some(snapshot_key),
                    "Futures snapshot preparation",
                    Some(std::time::Duration::from_secs(120)),
                    async move {
                        let calendar = tokio::try_join!(
                            session_is_open(&snapshot_state, date, "day"),
                            session_is_open(&snapshot_state, date, "evening")
                        );
                        let (day_open, evening_open) = scheduler_session_flags(calendar)?;
                        if !day_open && !evening_open {
                            return Ok(());
                        }
                        let snapshot = load_snapshot(&snapshot_state, instrument, date).await?;
                        let metadata_ready = snapshot
                            .as_ref()
                            .is_some_and(|value| has_valid_contract_metadata(value, date));
                        let levels_ready = snapshot.is_some_and(|value| {
                            value.status == "ready"
                                && value
                                    .previous_close
                                    .is_some_and(|price| price.is_finite() && price > 0.0)
                        });
                        if !metadata_ready || levels_ready {
                            return Ok(());
                        }
                        let snapshot = create_snapshot(&snapshot_state, instrument, date).await?;
                        if snapshot.status != "ready" {
                            let reason = snapshot
                                .error
                                .as_deref()
                                .unwrap_or("Daily market levels are not ready.");
                            operational_alert(&snapshot_state, None, instrument, "futures_snapshot_missing", "error", &format!("Futures snapshot is still missing after the preparation window opened: {reason}")).await;
                        }
                        Ok(())
                    },
                );
            }
        }
        for instrument in FUTURES_BREAKOUT_INSTRUMENTS {
            for (session, hour) in [("day", 9), ("evening", 17)] {
                let session_state = state.clone();
                spawn_scheduler_job(
                    state.clone(),
                    &jobs,
                    format!("futures-session:{instrument}:{session}"),
                    None,
                    "Futures session dispatch",
                    None,
                    async move {
                        schedule_session(&session_state, now, instrument, session, hour).await
                    },
                );
            }
        }
        let minute_of_day = now.hour() * 60 + now.minute();
        if option_square_off_due(now) {
            let squareoff_key = format!("{date}:mandatory-option-squareoff:{}", now.format("%H%M"));
            let squareoff_state = state.clone();
            spawn_scheduler_job(
                state.clone(),
                &jobs,
                "mandatory-option-squareoff",
                Some(squareoff_key),
                "SuperTrend mandatory square-off",
                None,
                async move {
                    process_supertrend_square_off(&squareoff_state, now).await?;
                    close_lingering_expired_contract_trades(&squareoff_state, now).await?;
                    reconcile_square_off_intents(&squareoff_state).await
                },
            );
        }
        if (SUPERTREND_ENTRY_START_MINUTE..=OPTION_SCHEDULER_END_MINUTE).contains(&minute_of_day)
            && minute_of_day.is_multiple_of(5)
        {
            let supertrend_key = format!("{date}:supertrend-options:{}", now.format("%H%M"));
            let supertrend_state = state.clone();
            spawn_scheduler_job(
                state.clone(),
                &jobs,
                "supertrend-cycle",
                Some(supertrend_key),
                "SuperTrend cycle",
                None,
                async move { run_supertrend_cycle(&supertrend_state, now).await },
            );
        }
        let feed_state = state.clone();
        spawn_scheduler_job(
            state.clone(),
            &jobs,
            "active-feed-refresh",
            None,
            "active market feed refresh",
            Some(std::time::Duration::from_secs(30)),
            async move {
                let active_tokens: Vec<(String, String)> = sqlx::query_as("SELECT DISTINCT s.exchange_segment,s.contract_token FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND s.contract_token IS NOT NULL AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE) UNION SELECT DISTINCT s.exchange_segment,s.contract_token FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id WHERE t.status='open' AND s.contract_token IS NOT NULL AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE)")
                        .fetch_all(&feed_state.db).await?;
                for (exchange, token) in active_tokens {
                    crate::market_ws::ensure_strategy_feed(feed_state.clone(), exchange, token)
                        .await;
                }
                Ok(())
            },
        );
        let shutdown_state = state.clone();
        spawn_scheduler_job(
            state.clone(),
            &jobs,
            "entry-shutdown",
            None,
            "entry shutdown enforcement",
            None,
            async move { enforce_entry_shutdowns(&shutdown_state).await },
        );
        let protection_state = state.clone();
        spawn_scheduler_job(
            state.clone(),
            &jobs,
            "protection-recovery",
            None,
            "trade protection recovery",
            None,
            async move { recover_unprotected_trades(&protection_state).await },
        );
        let reconcile_state = state.clone();
        spawn_scheduler_job(
            state.clone(),
            &jobs,
            "live-reconciliation",
            None,
            "LIVE reconciliation",
            None,
            async move { reconcile_live(&reconcile_state).await },
        );
        let reversal_state = state.clone();
        spawn_scheduler_job(
            state.clone(),
            &jobs,
            "sl2-reversal-recovery",
            None,
            "SL2 reversal recovery",
            None,
            async move { recover_sl2_reversal_intents(&reversal_state).await },
        );
        let execution_state = state.clone();
        spawn_scheduler_job(
            state.clone(),
            &jobs,
            "execution-intent-recovery",
            None,
            "execution intent recovery",
            None,
            async move {
                process_execution_intents(&execution_state, None)
                    .await
                    .map(|_| ())
            },
        );
    }
}

pub fn refresh_after_broker_connect(state: AppState, user_id: Uuid) {
    tokio::spawn(async move {
        if let Err(error) = reconcile_live_user_readiness(&state, user_id).await {
            tracing::warn!(%user_id, %error, "broker-connect full reconciliation failed; LIVE entries remain blocked");
        }
        let now = ist_now();
        if matches!(now.weekday(), Weekday::Sat | Weekday::Sun) {
            return;
        }
        if let Err(error) = ensure_supported_contract_metadata(&state, now.date_naive()).await {
            tracing::warn!(%error, "broker-connect contract selection failed");
            return;
        }
        for instrument in FUTURES_BREAKOUT_INSTRUMENTS {
            let result = if (now.hour(), now.minute()) >= (8, 30) {
                create_snapshot(&state, instrument, now.date_naive()).await
            } else {
                ensure_contract_metadata(&state, instrument, now.date_naive()).await
            };
            if let Err(error) = result {
                record_snapshot_failure(&state, instrument, now.date_naive(), &error.to_string())
                    .await;
                tracing::warn!(%instrument, %error, "broker-connect snapshot refresh failed");
            }
        }
    });
}

pub async fn admin_reload(state: &AppState) -> AppResult<Value> {
    let now = ist_now();
    let date = now.date_naive();
    contract_master::invalidate_cache().await;
    crate::market_ws::reset_strategy_feeds(state).await;
    close_lingering_expired_contract_trades(state, now).await?;
    sqlx::query("UPDATE strategy_scheduler_runs SET status='failed',next_attempt_at=NOW(),last_error=CONCAT(COALESCE(NULLIF(last_error,''), 'Admin reload requested'), '; admin reload requested'),updated_at=NOW() WHERE strategy_key=$1 AND trade_date=$2 AND status IN ('running','failed')")
        .bind(STRATEGY_KEY)
        .bind(date)
        .execute(&state.db)
        .await?;
    sqlx::query("UPDATE strategy_reversal_intents SET status=CASE WHEN status='waiting' THEN 'failed' ELSE status END,next_attempt_at=NOW(),last_error=CONCAT(COALESCE(NULLIF(last_error,''), 'Admin reload requested'), '; admin reload requested'),updated_at=NOW() WHERE status IN ('pending','waiting','failed')")
        .execute(&state.db)
        .await?;
    sqlx::query("UPDATE strategy_execution_intents SET status='retry_wait',next_attempt_at=NOW(),last_error=CONCAT(COALESCE(NULLIF(last_error,''), 'Admin reload requested'), '; admin reload requested'),updated_at=NOW() WHERE action='ENTRY' AND status IN ('pending','claimed','retry_wait','failed') AND (expires_at IS NULL OR expires_at>NOW())")
        .execute(&state.db)
        .await?;

    let mut snapshot_errors = Vec::new();
    match ensure_supported_contract_metadata(state, date).await {
        Ok(_) => {
            if (now.hour(), now.minute()) >= (8, 30) {
                for instrument in FUTURES_BREAKOUT_INSTRUMENTS {
                    if let Err(error) = create_snapshot(state, instrument, date).await {
                        record_snapshot_failure(state, instrument, date, &error.to_string()).await;
                        snapshot_errors.push(format!("{instrument}: {error}"));
                    }
                }
            }
        }
        Err(error) => snapshot_errors.push(format!("contract metadata: {error}")),
    }
    if let Err(error) = ensure_supertrend_option_contract_metadata(state, date).await {
        snapshot_errors.push(format!("SuperTrend option metadata: {error}"));
    }

    let due_runs: Vec<(String, String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT instrument,session_key,action,scheduled_for
         FROM strategy_scheduler_runs
         WHERE strategy_key=$1
           AND trade_date=$2
           AND status IN ('pending','failed')
           AND next_attempt_at<=NOW()
         ORDER BY scheduled_for,action
         LIMIT 50",
    )
    .bind(STRATEGY_KEY)
    .bind(date)
    .fetch_all(&state.db)
    .await?;
    let mut retried_runs = 0_usize;
    let mut retry_errors = Vec::new();
    for (instrument, session, action, scheduled_for) in due_runs {
        if !is_futures_breakout_instrument(&instrument) {
            continue;
        }
        let session: &'static str = match session.as_str() {
            "day" => "day",
            "evening" => "evening",
            _ => continue,
        };
        if !matches!(action.as_str(), "target" | "stop" | "entry" | "gap_entry") {
            continue;
        }
        let scheduled_for = scheduled_for.with_timezone(now.offset());
        retried_runs += 1;
        if let Err(error) =
            run_scheduled_action(state, &instrument, date, session, &action, scheduled_for).await
        {
            retry_errors.push(format!("{instrument} {session} {action}: {error}"));
        }
    }
    recover_sl2_reversal_intents(state).await?;
    let retried_execution_intents = process_execution_intents(state, None).await?;
    Ok(json!({
        "detail":"Strategy reload completed.",
        "date":date,
        "retried_scheduler_runs":retried_runs,
        "snapshot_errors":snapshot_errors,
        "retry_errors":retry_errors,
        "retried_execution_intents":retried_execution_intents
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionReportQuery {
    pub date: Option<NaiveDate>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryExecutionIntentRequest {
    pub intent_id: Uuid,
}

pub async fn admin_execution_report(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
    Query(query): Query<ExecutionReportQuery>,
) -> AppResult<Json<Value>> {
    crate::auth::require_admin_permission(&admin)?;
    let date = query.date.unwrap_or_else(|| ist_now().date_naive());
    let signals: Vec<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object(
            'signal_id',s.id,'strategy_key',s.strategy_key,'instrument',s.instrument,'session_key',s.session_key,
            'signal_type',s.signal_type,'signal_at',s.signal_at,'status',s.status,'expected_users',s.expected_users,
            'pending',COUNT(i.id) FILTER (WHERE i.status IN ('pending','claimed','retry_wait')),
            'submitted',COUNT(i.id) FILTER (WHERE i.status='submitted'),
            'completed',COUNT(i.id) FILTER (WHERE i.status='completed'),
            'skipped',COUNT(i.id) FILTER (WHERE i.status IN ('skipped','expired')),
            'failed',COUNT(i.id) FILTER (WHERE i.status='failed'),
            'intents',COALESCE(jsonb_agg(jsonb_build_object(
                'intent_id',i.id,'user_id',i.user_id,'username',u.username,'instrument',i.instrument,'action',i.action,
                'role',i.role,'status',i.status,'attempts',i.attempts,'last_error',i.last_error,'updated_at',i.updated_at
            ) ORDER BY u.username,i.role) FILTER (WHERE i.id IS NOT NULL),'[]'::jsonb)
         )
         FROM strategy_signals s
         LEFT JOIN strategy_execution_intents i ON i.signal_id=s.id
         LEFT JOIN users u ON u.id=i.user_id
         WHERE (s.signal_at AT TIME ZONE 'Asia/Kolkata')::date=$1
         GROUP BY s.id ORDER BY s.signal_at DESC",
    )
    .bind(date)
    .fetch_all(&state.db)
    .await?;
    let totals: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object(
            'expected_users',COALESCE(SUM(s.expected_users),0),
            'pending',COUNT(i.id) FILTER (WHERE i.status IN ('pending','claimed','retry_wait')),
            'submitted',COUNT(i.id) FILTER (WHERE i.status='submitted'),
            'completed',COUNT(i.id) FILTER (WHERE i.status='completed'),
            'skipped',COUNT(i.id) FILTER (WHERE i.status IN ('skipped','expired')),
            'failed',COUNT(i.id) FILTER (WHERE i.status='failed'))
         FROM strategy_signals s LEFT JOIN strategy_execution_intents i ON i.signal_id=s.id
         WHERE (s.signal_at AT TIME ZONE 'Asia/Kolkata')::date=$1",
    )
    .bind(date)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(
        json!({"date":date,"timezone":"Asia/Kolkata","totals":totals,"signals":signals}),
    ))
}

pub async fn admin_retry_execution_intent(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
    Json(input): Json<RetryExecutionIntentRequest>,
) -> AppResult<Json<Value>> {
    crate::auth::require_admin_permission(&admin)?;
    let signal_id: Option<Uuid> = sqlx::query_scalar(
        "UPDATE strategy_execution_intents SET status='retry_wait',next_attempt_at=NOW(),last_error='Admin retry requested.',updated_at=NOW()
         WHERE id=$1 AND action='ENTRY' AND status IN ('failed','retry_wait') AND (expires_at IS NULL OR expires_at>NOW()) RETURNING signal_id",
    )
    .bind(input.intent_id)
    .fetch_optional(&state.db)
    .await?;
    let signal_id = signal_id.ok_or_else(|| {
        AppError::BadRequest(
            "Only failed or waiting entry intents inside their safe execution window can be retried.".into(),
        )
    })?;
    let processed = process_execution_intents(&state, Some(signal_id)).await?;
    Ok(Json(
        json!({"detail":"Execution intent retry requested.","processed":processed}),
    ))
}

#[derive(Debug, Clone, FromRow)]
pub(crate) struct StoredOrder {
    pub id: Uuid,
    pub user_id: Uuid,
    pub snapshot_id: Uuid,
    pub trade_id: Option<Uuid>,
    pub session_key: String,
    pub role: String,
    pub side: String,
    pub order_type: String,
    pub execution_mode: String,
    pub lots: i32,
    pub quantity: i32,
    pub price: f64,
    pub broker_order_id: String,
    pub client_order_id: String,
    pub status: String,
    pub filled_quantity: i32,
    pub processed_quantity: i32,
    pub average_fill_price: Option<f64>,
}

impl StoredOrder {
    /// `quantity` is the business delta passed to a fill handler while
    /// `filled_quantity` remains the broker's cumulative fill watermark.
    pub(crate) fn cumulative_fill_quantity(&self) -> i32 {
        self.filled_quantity
            .max(self.processed_quantity.saturating_add(self.quantity))
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ReconciliationAudience {
    connected: Vec<Uuid>,
    needs_full_readiness: Vec<Uuid>,
    disconnected: Vec<Uuid>,
}

async fn reconciliation_audience(state: &AppState) -> AppResult<ReconciliationAudience> {
    let rows: Vec<(Uuid, bool, bool)> = sqlx::query_as(
        "SELECT u.id,
                COALESCE(p.last_token_status IN ('success','refreshed'),FALSE)
                AND EXISTS(
                    SELECT 1 FROM broker_secrets api
                    WHERE api.user_id=u.id AND api.secret_kind='api_key'
                )
                AND EXISTS(
                    SELECT 1 FROM broker_secrets jwt
                    WHERE jwt.user_id=u.id AND jwt.secret_kind='jwt_token'
                ) AS connected,
                NOT EXISTS(
                    SELECT 1
                    FROM broker_reconciliation_health h
                    WHERE h.user_id=u.id
                      AND h.healthy=TRUE
                      AND h.broker_credential_revision=p.broker_credential_revision
                      AND h.checked_at>NOW()-INTERVAL '5 minutes'
                )
                AND (
                    NOT EXISTS(SELECT 1 FROM broker_reconciliation_health h WHERE h.user_id=u.id)
                    OR EXISTS(
                        SELECT 1 FROM broker_reconciliation_health h
                        WHERE h.user_id=u.id
                          AND (
                              h.healthy=FALSE
                              AND h.checked_at<=NOW()-INTERVAL '30 seconds'
                              OR
                              h.broker_credential_revision IS DISTINCT FROM p.broker_credential_revision
                              OR h.checked_at<=NOW()-INTERVAL '5 minutes'
                              OR h.detail='Full broker readiness reconciliation is required after deployment.'
                              OR h.detail='Fresh Angel session established; full broker reconciliation is pending.'
                          )
                    )
                ) AS needs_full_readiness
         FROM users u
         LEFT JOIN user_profiles p ON p.user_id=u.id
         WHERE (
                 u.is_active=TRUE
                 AND u.can_live_trade=TRUE
                 AND p.trading_mode='live'
               )
            OR EXISTS(
                 SELECT 1 FROM strategy_orders o
                 WHERE o.user_id=u.id
                   AND o.execution_mode='live'
                   AND o.status IN ('submitting','ambiguous','submitted','partially_filled','processing','cancelling')
               )
            OR EXISTS(
                 SELECT 1 FROM trades t
                 WHERE t.user_id=u.id
                   AND t.execution_mode='live'
                   AND t.status='open'
               )
            OR EXISTS(
                 SELECT 1 FROM broker_position_incidents i
                 WHERE i.user_id=u.id
                   AND i.status IN ('open','operator_required')
               )
         ORDER BY u.id",
    )
    .fetch_all(&state.db)
    .await?;
    let mut audience = ReconciliationAudience::default();
    for (user_id, connected, needs_full_readiness) in rows {
        if connected {
            if needs_full_readiness {
                audience.needs_full_readiness.push(user_id);
            } else {
                audience.connected.push(user_id);
            }
        } else {
            audience.disconnected.push(user_id);
        }
    }
    Ok(audience)
}

async fn reconcile_live(state: &AppState) -> AppResult<()> {
    // `pending` is only the short pre-submission reservation state. If the
    // process dies before it atomically claims `submitting`, no broker request
    // was made and the order is safe to retry through its durable strategy
    // action/idempotency key.
    sqlx::query("UPDATE strategy_orders SET status='failed',broker_error_class='retryable',broker_error_code='interrupted_before_submission',broker_status='Backend restarted before broker submission began; safe retry is allowed.',state_version=state_version+1,updated_at=NOW() WHERE status='pending' AND updated_at<NOW()-INTERVAL '30 seconds'")
        .execute(&state.db)
        .await?;
    sqlx::query("UPDATE strategy_orders SET status='ambiguous',broker_error_class='ambiguous',broker_error_code='crash_during_submission',broker_status='Backend restarted while submission was in progress; reconciling without retry.',uncertain_since_at=COALESCE(uncertain_since_at,updated_at),state_version=state_version+1,updated_at=NOW() WHERE execution_mode='live' AND status='submitting' AND updated_at<NOW()-INTERVAL '30 seconds'")
        .execute(&state.db).await?;
    sqlx::query("UPDATE strategy_orders SET status=CASE WHEN filled_quantity>0 AND filled_quantity<quantity THEN 'partially_filled' ELSE 'submitted' END,broker_status='Recovered after interruption during fill processing',state_version=state_version+1,updated_at=NOW() WHERE execution_mode='live' AND status='processing' AND processed_quantity<filled_quantity AND updated_at<NOW()-INTERVAL '30 seconds'")
        .execute(&state.db).await?;
    let audience = reconciliation_audience(state).await?;
    for user_id in audience.disconnected {
        let _ = risk::set_reconciliation_health(
            state,
            user_id,
            false,
            "Angel session is disconnected; LIVE entries remain blocked.",
        )
        .await;
        operational_alert(
            state,
            Some(user_id),
            "",
            "broker_disconnected",
            "error",
            "Live orders are awaiting reconciliation. Reconnect Angel One.",
        )
        .await;
    }
    let mut tasks = tokio::task::JoinSet::new();
    for user_id in audience.needs_full_readiness {
        let state = state.clone();
        tasks.spawn(async move { reconcile_live_user_readiness(&state, user_id).await });
    }
    for user_id in audience.connected {
        let state = state.clone();
        tasks.spawn(async move { reconcile_live_user(&state, user_id).await });
    }
    while let Some(result) = tasks.join_next().await {
        match result {
            Err(error) => tracing::warn!(%error,"broker reconciliation task panicked"),
            Ok(Err(error)) => tracing::warn!(%error,"broker reconciliation failed for one user"),
            Ok(Ok(())) => {}
        }
    }
    recover_residual_protective_orders(state).await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResidualProtectionPlan {
    quantity: i32,
    exit_processed: i32,
}

fn residual_protection_plan(
    trade_open: bool,
    live: bool,
    quantity: i32,
    exit_processed: i32,
    has_nonterminal_exit: bool,
) -> Option<ResidualProtectionPlan> {
    (trade_open && live && quantity > 0 && exit_processed > 0 && !has_nonterminal_exit).then_some(
        ResidualProtectionPlan {
            quantity,
            exit_processed,
        },
    )
}

fn residual_protection_session(trade_id: Uuid, exit_processed: i32) -> String {
    format!(
        "rp-{}-{:x}",
        &trade_id.simple().to_string()[..16],
        exit_processed.max(0)
    )
}

/// Re-protection is deliberately driven from durable broker reconciliation,
/// not directly from a partial-fill handler. A cancelling sibling is still a
/// live broker order and therefore blocks replacement. Once every sibling is
/// terminal, this creates one deterministic stop order for the actual residual
/// exposure. The order idempotency key makes concurrent/crash retries converge.
async fn recover_residual_protective_orders(state: &AppState) -> AppResult<()> {
    let trade_ids: Vec<Uuid> = sqlx::query_scalar("SELECT t.id FROM trades t WHERE t.execution_mode='live' AND t.status='open' AND t.strategy_key=$1 AND EXISTS (SELECT 1 FROM strategy_orders filled WHERE filled.trade_id=t.id AND filled.role IN ('TARGET','SL1','SL2') AND filled.processed_quantity>0) AND NOT EXISTS (SELECT 1 FROM strategy_orders active WHERE active.trade_id=t.id AND active.role IN ('TARGET','SL1','SL2') AND active.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')) ORDER BY t.entry_datetime LIMIT 100")
        .bind(STRATEGY_KEY)
        .fetch_all(&state.db)
        .await?;
    for trade_id in trade_ids {
        let mut tx = state.db.begin().await?;
        let lock_key = format!("residual-protection:{trade_id}");
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(&lock_key)
            .execute(&mut *tx)
            .await?;
        let trade: Option<ResidualProtectionTradeRow> =
            sqlx::query_as("SELECT trade.user_id,trade.strategy_snapshot_id,trade.strategy_key,trade.instrument_label,trade.direction,trade.quantity,trade.remaining_lots,trade.sl1_price::float8,trade.sl2_price::float8,EXISTS(SELECT 1 FROM strategy_orders target_order WHERE target_order.trade_id=trade.id AND target_order.role='TARGET' AND target_order.processed_quantity>0) AS target_done FROM trades trade WHERE trade.id=$1 AND trade.execution_mode='live' AND trade.status='open' FOR UPDATE")
                .bind(trade_id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((
            user_id,
            snapshot_id,
            strategy_key,
            instrument,
            direction,
            quantity,
            remaining_lots,
            sl1_price,
            sl2_price,
            target_done,
        )) = trade
        else {
            tx.commit().await?;
            continue;
        };
        let (exit_processed, has_nonterminal): (i32, bool) = sqlx::query_as("SELECT COALESCE(SUM(processed_quantity),0)::int4,COALESCE(BOOL_OR(status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')),FALSE) FROM strategy_orders WHERE trade_id=$1 AND role IN ('TARGET','SL1','SL2')")
            .bind(trade_id)
            .fetch_one(&mut *tx)
            .await?;
        let plan = residual_protection_plan(true, true, quantity, exit_processed, has_nonterminal);
        tx.commit().await?;
        let Some(plan) = plan else {
            continue;
        };

        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await?;
        if strategy_key != STRATEGY_KEY {
            continue;
        }
        let (role, stop) = if target_done {
            ("SL2", sl2_price)
        } else {
            ("SL1", sl1_price)
        };
        let stop = stop
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "The open {instrument} trade has no valid residual stop level."
                ))
            })?;
        let lots = remaining_lots.max(1);
        let mut runner = runner_for_strategy(state, user_id, &strategy_key, &instrument).await?;
        runner.trading_mode = "live".into();
        let session = residual_protection_session(trade_id, plan.exit_processed);
        if let Err(error) = place_strategy_order(
            state,
            &runner,
            &snapshot,
            &session,
            NewOrder {
                role,
                side: if direction == "BUY" { "SELL" } else { "BUY" },
                order_type: "STOPLOSS_MARKET",
                lots,
                price: stop,
                trigger: Some(stop),
                trade_id: Some(trade_id),
                quantity: Some(plan.quantity),
            },
        )
        .await
        {
            operational_alert_for(
                state,
                &strategy_key,
                Some(user_id),
                &instrument,
                "residual_protection_failed",
                "critical",
                &format!("Residual position protection will retry: {error}"),
            )
            .await;
        }
    }
    Ok(())
}

fn broker_text<'a>(item: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| item.get(*name).and_then(Value::as_str))
}
fn broker_i32(item: &Value, names: &[&str]) -> Option<i32> {
    names.iter().find_map(|name| {
        item.get(*name)
            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
            .and_then(|v| i32::try_from(v).ok())
    })
}
fn broker_f64(item: &Value, names: &[&str]) -> Option<f64> {
    names.iter().find_map(|name| {
        item.get(*name)
            .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
    })
}

#[derive(Debug, Clone, PartialEq)]
struct BrokerNetPosition {
    exchange: String,
    token: String,
    symbol: String,
    product: String,
    net_quantity: i32,
    average_price: f64,
    raw: Value,
}

#[derive(Debug, Clone, PartialEq)]
struct BrokerTradeFill {
    order_id: String,
    order_tag: String,
    exchange: String,
    token: String,
    symbol: String,
    side: String,
    quantity: i32,
    price: f64,
    filled_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrokerExposureOwnership {
    RulenixOwned,
    ManualExternal,
    Ambiguous,
}

impl BrokerExposureOwnership {
    fn as_str(self) -> &'static str {
        match self {
            Self::RulenixOwned => "rulenix_owned",
            Self::ManualExternal => "manual_external",
            Self::Ambiguous => "ambiguous",
        }
    }
}

fn is_rulenix_order_tag(value: &str) -> bool {
    value.len() == 20
        && value.starts_with("RX")
        && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn broker_order_ownership(
    item: &Value,
    known_broker_ids: &HashSet<String>,
    known_client_ids: &HashSet<String>,
) -> BrokerExposureOwnership {
    let broker_id = broker_text(item, &["orderid", "orderId"])
        .unwrap_or("")
        .trim();
    let client_id = broker_text(item, &["ordertag", "orderTag"])
        .unwrap_or("")
        .trim();
    if (!broker_id.is_empty() && known_broker_ids.contains(broker_id))
        || (!client_id.is_empty() && known_client_ids.contains(client_id))
    {
        return BrokerExposureOwnership::RulenixOwned;
    }
    if is_rulenix_order_tag(&client_id.to_uppercase()) {
        return BrokerExposureOwnership::Ambiguous;
    }
    let exchange = broker_text(item, &["exchange"]).unwrap_or("").trim();
    let token = broker_text(item, &["symboltoken", "symbolToken"])
        .unwrap_or("")
        .trim();
    let side = broker_text(item, &["transactiontype", "transactionType", "side"])
        .unwrap_or("")
        .trim()
        .to_uppercase();
    let quantity = broker_i32(
        item,
        &[
            "quantity",
            "fillsize",
            "fillSize",
            "filledshares",
            "filledShares",
        ],
    );
    if !broker_id.is_empty()
        && !exchange.is_empty()
        && !token.is_empty()
        && matches!(side.as_str(), "BUY" | "SELL")
        && quantity.is_some_and(|value| value > 0)
    {
        BrokerExposureOwnership::ManualExternal
    } else {
        BrokerExposureOwnership::Ambiguous
    }
}

fn position_fill_ownership(
    position: &BrokerNetPosition,
    fills: &[BrokerTradeFill],
    order_book: &[Value],
    known_broker_ids: &HashSet<String>,
    known_client_ids: &HashSet<String>,
) -> BrokerExposureOwnership {
    let broker_orders: HashMap<&str, &Value> = order_book
        .iter()
        .filter_map(|item| {
            broker_text(item, &["orderid", "orderId"])
                .filter(|value| !value.trim().is_empty())
                .map(|value| (value.trim(), item))
        })
        .collect();
    let mut totals = [0_i64; 3];
    let mut matched = 0_i64;
    for fill in fills
        .iter()
        .filter(|fill| fill.exchange == position.exchange && fill.token == position.token)
    {
        let ownership = broker_orders.get(fill.order_id.as_str()).map_or_else(
            || {
                if known_broker_ids.contains(&fill.order_id)
                    || (!fill.order_tag.is_empty() && known_client_ids.contains(&fill.order_tag))
                {
                    BrokerExposureOwnership::RulenixOwned
                } else if is_rulenix_order_tag(&fill.order_tag.to_uppercase()) {
                    BrokerExposureOwnership::Ambiguous
                } else if !fill.order_id.is_empty() {
                    BrokerExposureOwnership::ManualExternal
                } else {
                    BrokerExposureOwnership::Ambiguous
                }
            },
            |item| broker_order_ownership(item, known_broker_ids, known_client_ids),
        );
        let signed = if fill.side == "BUY" {
            i64::from(fill.quantity)
        } else {
            -i64::from(fill.quantity)
        };
        let index = match ownership {
            BrokerExposureOwnership::RulenixOwned => 0,
            BrokerExposureOwnership::ManualExternal => 1,
            BrokerExposureOwnership::Ambiguous => 2,
        };
        totals[index] += signed;
        matched += 1;
    }
    let broker_net = i64::from(position.net_quantity);
    if matched > 0 && totals[0] == broker_net && totals[1] == 0 && totals[2] == 0 {
        BrokerExposureOwnership::RulenixOwned
    } else if matched > 0 && totals[1] == broker_net && totals[0] == 0 && totals[2] == 0 {
        BrokerExposureOwnership::ManualExternal
    } else {
        BrokerExposureOwnership::Ambiguous
    }
}

#[derive(Debug, Clone, PartialEq)]
struct AttributedManualClose {
    quantity: i32,
    weighted_price: f64,
    order_ids: Vec<String>,
    first_fill_at: DateTime<Utc>,
    last_fill_at: DateTime<Utc>,
}

fn parse_broker_fill_time(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(value) = DateTime::parse_from_rfc3339(value) {
        return Some(value.with_timezone(&Utc));
    }
    let raw = value.trim();
    for format in [
        "%d-%b-%Y %H:%M:%S",
        "%d%b%Y %H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(value) = NaiveDateTime::parse_from_str(raw, format) {
            return FixedOffset::east_opt(19_800)?
                .from_local_datetime(&value)
                .single()
                .map(|value| value.with_timezone(&Utc));
        }
    }
    None
}

fn broker_book_items<'a>(value: &'a Value, name: &str) -> AppResult<&'a [Value]> {
    match value {
        Value::Null => Ok(&[]),
        Value::Array(items) => Ok(items),
        _ => Err(AppError::BadRequest(format!(
            "Angel One returned malformed {name} data; broker state is unknown."
        ))),
    }
}

fn parse_broker_trade_fills(value: &Value) -> AppResult<Vec<BrokerTradeFill>> {
    broker_book_items(value, "trade-book")?
        .iter()
        .map(|item| {
            let quantity = broker_i32(
                item,
                &[
                    "fillsize",
                    "fillSize",
                    "filledshares",
                    "filledShares",
                    "quantity",
                ],
            )
            .ok_or_else(|| {
                AppError::BadRequest(
                    "Angel One returned a trade-book fill without an authoritative quantity."
                        .into(),
                )
            })?;
            let price = broker_f64(
                item,
                &["fillprice", "fillPrice", "averageprice", "averagePrice", "price"],
            )
            .ok_or_else(|| {
                AppError::BadRequest(
                    "Angel One returned a trade-book fill without an authoritative price.".into(),
                )
            })?;
            let side = broker_text(item, &["transactiontype", "transactionType", "side"])
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "Angel One returned a trade-book fill without a side.".into(),
                    )
                })?
                .trim()
                .to_uppercase();
            let filled_at = broker_text(
                item,
                &[
                    "filltime",
                    "fillTime",
                    "tradetime",
                    "tradeTime",
                    "updatetime",
                    "updateTime",
                    "exchtime",
                    "exchangeTime",
                ],
            )
            .and_then(parse_broker_fill_time)
            .ok_or_else(|| {
                AppError::BadRequest(
                    "Angel One returned a trade-book fill without an authoritative execution time."
                        .into(),
                )
            })?;
            let fill = BrokerTradeFill {
                order_id: broker_text(item, &["orderid", "orderId"])
                    .ok_or_else(|| {
                        AppError::BadRequest(
                            "Angel One returned a trade-book fill without an order ID.".into(),
                        )
                    })?
                    .trim()
                    .to_owned(),
                order_tag: broker_text(item, &["ordertag", "orderTag"])
                    .unwrap_or("")
                    .trim()
                    .to_owned(),
                exchange: broker_text(item, &["exchange"])
                    .ok_or_else(|| {
                        AppError::BadRequest(
                            "Angel One returned a trade-book fill without an exchange.".into(),
                        )
                    })?
                    .trim()
                    .to_uppercase(),
                token: broker_text(item, &["symboltoken", "symbolToken"])
                    .ok_or_else(|| {
                        AppError::BadRequest(
                            "Angel One returned a trade-book fill without a contract token.".into(),
                        )
                    })?
                    .trim()
                    .to_owned(),
                symbol: broker_text(item, &["tradingsymbol", "tradingSymbol"])
                    .unwrap_or("")
                    .trim()
                    .to_owned(),
                side,
                quantity,
                price,
                filled_at,
            };
            if !fill.order_id.is_empty()
                && !fill.exchange.is_empty()
                && !fill.token.is_empty()
                && matches!(fill.side.as_str(), "BUY" | "SELL")
                && fill.quantity > 0
                && fill.price.is_finite()
                && fill.price > 0.0
            {
                Ok(fill)
            } else {
                Err(AppError::BadRequest(
                    "Angel One returned a structurally invalid trade-book fill; broker state is unknown."
                        .into(),
                ))
            }
        })
        .collect()
}

struct ManualFillExpectation<'a> {
    exchange: &'a str,
    token: &'a str,
    symbol: &'a str,
    direction: &'a str,
    quantity: i32,
    entry_at: DateTime<Utc>,
    evidence_since: DateTime<Utc>,
    known_order_ids: &'a HashSet<String>,
}

fn attributable_manual_flat_fill(
    fills: &[BrokerTradeFill],
    expected: &ManualFillExpectation<'_>,
) -> Option<AttributedManualClose> {
    let exit_side = if expected.direction == "BUY" {
        "SELL"
    } else {
        "BUY"
    };
    let external: Vec<&BrokerTradeFill> = fills
        .iter()
        .filter(|fill| {
            fill.exchange.eq_ignore_ascii_case(expected.exchange)
                && fill.token == expected.token
                && fill.filled_at >= expected.entry_at
                && fill.filled_at >= expected.evidence_since
                && !expected.known_order_ids.contains(&fill.order_id)
        })
        .collect();
    if external.is_empty()
        || external.iter().any(|fill| {
            (!expected.symbol.is_empty() && !fill.symbol.eq_ignore_ascii_case(expected.symbol))
                || fill.side != exit_side
        })
    {
        return None;
    }
    let total: i64 = external.iter().map(|fill| i64::from(fill.quantity)).sum();
    if total != i64::from(expected.quantity) || total <= 0 {
        return None;
    }
    let mut order_ids: Vec<String> = external.iter().map(|fill| fill.order_id.clone()).collect();
    order_ids.sort();
    order_ids.dedup();
    Some(AttributedManualClose {
        quantity: expected.quantity,
        weighted_price: external
            .iter()
            .map(|fill| fill.price * f64::from(fill.quantity))
            .sum::<f64>()
            / total as f64,
        order_ids,
        first_fill_at: external.iter().map(|fill| fill.filled_at).min()?,
        last_fill_at: external.iter().map(|fill| fill.filled_at).max()?,
    })
}

fn position_mismatch_type(broker_quantity: i32, local_quantity: i32) -> Option<&'static str> {
    if broker_quantity == local_quantity {
        None
    } else if broker_quantity == 0 {
        Some("LOCAL_POSITION_BROKER_FLAT")
    } else {
        Some("QUANTITY_OR_DIRECTION_MISMATCH")
    }
}

fn is_aggregate_position_mismatch(incident_type: &str) -> bool {
    matches!(
        incident_type,
        "LOCAL_POSITION_BROKER_FLAT" | "QUANTITY_OR_DIRECTION_MISMATCH" | "AVERAGE_ENTRY_MISMATCH"
    )
}

fn parse_broker_positions(value: &Value) -> Vec<BrokerNetPosition> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let exchange = broker_text(item, &["exchange"])?.trim().to_uppercase();
            let token = broker_text(item, &["symboltoken", "symbolToken"])?
                .trim()
                .to_owned();
            let symbol = broker_text(item, &["tradingsymbol", "tradingSymbol"])
                .unwrap_or("")
                .trim()
                .to_owned();
            let product = broker_text(item, &["producttype", "productType", "product"])
                .unwrap_or("")
                .trim()
                .to_owned();
            let net_quantity = broker_i32(item, &["netqty", "netQty"])?;
            let average_price =
                broker_f64(item, &["avgnetprice", "avgNetPrice", "netprice"]).unwrap_or(0.0);
            (!exchange.is_empty()
                && !token.is_empty()
                && average_price.is_finite()
                && average_price >= 0.0)
                .then_some(BrokerNetPosition {
                    exchange,
                    token,
                    symbol,
                    product,
                    net_quantity,
                    average_price,
                    raw: item.clone(),
                })
        })
        .collect()
}

fn parse_authoritative_broker_positions(value: &Value) -> AppResult<Vec<BrokerNetPosition>> {
    let items = broker_book_items(value, "position-book")?;
    let parsed = parse_broker_positions(value);
    if parsed.len() != items.len() {
        return Err(AppError::BadRequest(
            "Angel One returned a structurally invalid position record; broker state is unknown."
                .into(),
        ));
    }
    Ok(parsed)
}

#[allow(clippy::too_many_arguments)]
async fn record_position_incident(
    state: &AppState,
    user_id: Uuid,
    strategy_key: &str,
    instrument: &str,
    exchange: &str,
    token: &str,
    symbol: &str,
    incident_type: &str,
    broker_quantity: i32,
    local_quantity: i32,
    broker_average_price: Option<f64>,
    trade_id: Option<Uuid>,
    detail: &str,
    product: &str,
    raw_broker_position: Option<&Value>,
) -> AppResult<()> {
    let mut transaction = state.db.begin().await?;
    let alert_needed: bool = !sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM broker_position_incidents WHERE user_id=$1 AND exchange_segment=$2 AND contract_token=$3 AND incident_type=$4 AND status IN ('open','operator_required'))")
        .bind(user_id).bind(exchange).bind(token).bind(incident_type).fetch_one(&mut *transaction).await?;
    let operator_required = matches!(
        incident_type,
        "ORPHAN_POSITION"
            | "UNMAPPED_BROKER_POSITION"
            | "AMBIGUOUS_ORDER_DEADLINE"
            | "AMBIGUOUS_PROTECTION"
            | "OVER_CLOSE_POSITION"
    );
    let status = if operator_required {
        "operator_required"
    } else {
        "open"
    };
    let ownership_status = if matches!(
        incident_type,
        "ORPHAN_POSITION" | "UNMAPPED_BROKER_POSITION"
    ) {
        "ambiguous"
    } else {
        "strategy_related"
    };
    if is_aggregate_position_mismatch(incident_type) {
        sqlx::query("UPDATE broker_position_incidents SET status='resolved',resolved_at=NOW(),last_detected_at=NOW() WHERE user_id=$1 AND exchange_segment=$2 AND contract_token=$3 AND incident_type IN ('LOCAL_POSITION_BROKER_FLAT','QUANTITY_OR_DIRECTION_MISMATCH','AVERAGE_ENTRY_MISMATCH') AND incident_type<>$4 AND status IN ('open','operator_required')")
            .bind(user_id).bind(exchange).bind(token).bind(incident_type)
            .execute(&mut *transaction).await?;
    }
    sqlx::query("INSERT INTO broker_position_incidents(id,user_id,strategy_key,instrument,exchange_segment,contract_token,contract_symbol,incident_type,status,broker_quantity,local_quantity,broker_average_price,trade_id,detail,product_type,ownership_status,raw_broker_position) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17) ON CONFLICT(user_id,exchange_segment,contract_token,incident_type) DO UPDATE SET status=EXCLUDED.status,resolved_at=NULL,broker_quantity=EXCLUDED.broker_quantity,local_quantity=EXCLUDED.local_quantity,broker_average_price=EXCLUDED.broker_average_price,trade_id=EXCLUDED.trade_id,detail=EXCLUDED.detail,product_type=EXCLUDED.product_type,ownership_status=EXCLUDED.ownership_status,raw_broker_position=EXCLUDED.raw_broker_position,last_detected_at=NOW()")
        .bind(Uuid::new_v4()).bind(user_id).bind(strategy_key).bind(instrument).bind(exchange).bind(token).bind(symbol).bind(incident_type).bind(status).bind(broker_quantity).bind(local_quantity).bind(broker_average_price).bind(trade_id).bind(detail).bind(product).bind(ownership_status).bind(raw_broker_position.cloned().unwrap_or_else(|| json!({}))).execute(&mut *transaction).await?;
    if let Some(trade_id) = trade_id {
        sqlx::query("UPDATE trades SET safety_status='RECONCILIATION_REQUIRED',broker_net_quantity=$2,broker_average_price=$3,last_position_reconciled_at=NOW(),updated_at=NOW() WHERE id=$1 AND status='open'")
            .bind(trade_id).bind(broker_quantity).bind(broker_average_price).execute(&mut *transaction).await?;
    }
    transaction.commit().await?;
    if alert_needed {
        operational_alert_for(
            state,
            strategy_key,
            Some(user_id),
            instrument,
            "broker_position_mismatch",
            "critical",
            detail,
        )
        .await;
    }
    Ok(())
}

async fn mark_ambiguous_protection(
    state: &AppState,
    order_id: Uuid,
    client_order_id: &str,
    runner: &Runner,
    snapshot: &Snapshot,
    order: &NewOrder,
    diagnostic: &str,
) -> AppResult<()> {
    let Some(trade_id) = order.trade_id else {
        return Ok(());
    };
    let trade: Option<(String, i32, Option<i32>)> = sqlx::query_as(
        "UPDATE trades
         SET safety_status='PROTECTION_UNCERTAIN',
             last_protection_error=$2,
             updated_at=NOW()
         WHERE id=$1 AND status='open'
         RETURNING direction,quantity,broker_net_quantity",
    )
    .bind(trade_id)
    .bind(diagnostic)
    .fetch_optional(&state.db)
    .await?;
    let Some((direction, quantity, broker_net_quantity)) = trade else {
        return Ok(());
    };
    let local_quantity = if direction == "BUY" {
        quantity
    } else {
        -quantity
    };
    let detail = format!(
        "Protection submission outcome is ambiguous for trade {trade_id}, local order {order_id}, client tag {client_order_id}. No duplicate stop or market close will be submitted until Angel One order and position books become authoritative. Diagnostic: {diagnostic}"
    );
    record_position_incident(
        state,
        runner.user_id,
        &snapshot.strategy_key,
        &snapshot.instrument,
        &snapshot.exchange_segment,
        snapshot.contract_token.as_deref().unwrap_or(""),
        snapshot.contract_symbol.as_deref().unwrap_or(""),
        "AMBIGUOUS_PROTECTION",
        broker_net_quantity.unwrap_or(local_quantity),
        local_quantity,
        None,
        Some(trade_id),
        &detail,
        &snapshot.product_type,
        None,
    )
    .await?;
    operational_alert_for(
        state,
        &snapshot.strategy_key,
        Some(runner.user_id),
        &snapshot.instrument,
        "ambiguous_protection",
        "critical",
        &detail,
    )
    .await;
    Ok(())
}

async fn mark_protection_submission_failure(
    state: &AppState,
    trade_id: Uuid,
    error: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE trades t
         SET safety_status=CASE
               WHEN EXISTS(
                 SELECT 1 FROM strategy_orders o
                 WHERE o.trade_id=t.id
                   AND o.role IN ('SL1','SL2')
                   AND o.status='ambiguous'
               ) THEN 'PROTECTION_UNCERTAIN'
               ELSE 'PROTECTION_FAILED'
             END,
             last_protection_error=$2,
             updated_at=NOW()
         WHERE t.id=$1 AND t.status='open'",
    )
    .bind(trade_id)
    .bind(error)
    .execute(&state.db)
    .await?;
    Ok(())
}

async fn escalate_ambiguous_order(
    state: &AppState,
    order: &StoredOrder,
    positions: &Value,
) -> AppResult<()> {
    let metadata: Option<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT strategy_key,instrument,exchange_segment,COALESCE(contract_token,''),COALESCE(contract_symbol,'') FROM strategy_market_snapshots WHERE id=$1",
    )
    .bind(order.snapshot_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((strategy_key, instrument, exchange, token, symbol)) = metadata else {
        return Ok(());
    };
    let broker = parse_broker_positions(positions)
        .into_iter()
        .find(|position| position.exchange == exchange.to_uppercase() && position.token == token);
    let local_quantity: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(CASE WHEN t.direction='BUY' THEN t.quantity ELSE -t.quantity END),0) FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id WHERE t.user_id=$1 AND t.execution_mode='live' AND t.status='open' AND s.exchange_segment=$2 AND s.contract_token=$3")
        .bind(order.user_id).bind(&exchange).bind(&token).fetch_one(&state.db).await?;
    let detail = format!(
        "Ambiguous {} order {} (client {}, broker {}) remained absent from the broker order book beyond the configured deadline. No retry was attempted; broker net quantity is {} and local signed quantity is {}.",
        order.role,
        order.id,
        order.client_order_id,
        if order.broker_order_id.is_empty() {
            "unknown"
        } else {
            &order.broker_order_id
        },
        broker.as_ref().map_or(0, |value| value.net_quantity),
        local_quantity
    );
    let already_escalated: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM broker_position_incidents WHERE user_id=$1 AND exchange_segment=$2 AND contract_token=$3 AND incident_type='AMBIGUOUS_ORDER_DEADLINE' AND status IN ('open','operator_required'))")
        .bind(order.user_id).bind(&exchange).bind(&token).fetch_one(&state.db).await?;
    record_position_incident(
        state,
        order.user_id,
        &strategy_key,
        &instrument,
        &exchange,
        &token,
        broker
            .as_ref()
            .map_or(symbol.as_str(), |value| value.symbol.as_str()),
        "AMBIGUOUS_ORDER_DEADLINE",
        broker.as_ref().map_or(0, |value| value.net_quantity),
        i32::try_from(local_quantity).unwrap_or(if local_quantity < 0 {
            i32::MIN
        } else {
            i32::MAX
        }),
        broker.as_ref().map(|value| value.average_price),
        order.trade_id,
        &detail,
        broker.as_ref().map_or("", |value| value.product.as_str()),
        broker.as_ref().map(|value| &value.raw),
    )
    .await?;
    if let Some(trade_id) = order.trade_id
        && matches!(order.role.as_str(), "SL1" | "SL2" | "EMERGENCY_CLOSE")
    {
        sqlx::query("UPDATE trades SET safety_status=CASE WHEN $3 IN ('SL1','SL2') THEN 'PROTECTION_UNCERTAIN' ELSE 'EMERGENCY_CLOSING' END,last_protection_error=$2,updated_at=NOW() WHERE id=$1 AND status='open'")
            .bind(trade_id).bind(&detail).bind(&order.role).execute(&state.db).await?;
    }
    if !already_escalated {
        operational_alert_for(
            state,
            &strategy_key,
            Some(order.user_id),
            &instrument,
            "ambiguous_order_deadline",
            "critical",
            &detail,
        )
        .await;
    }
    Ok(())
}

async fn resolve_position_incidents(
    state: &AppState,
    user_id: Uuid,
    exchange: &str,
    token: &str,
) -> AppResult<()> {
    sqlx::query("UPDATE broker_position_incidents SET status='resolved',resolved_at=NOW(),last_detected_at=NOW() WHERE user_id=$1 AND exchange_segment=$2 AND contract_token=$3 AND incident_type IN ('LOCAL_POSITION_BROKER_FLAT','QUANTITY_OR_DIRECTION_MISMATCH','AVERAGE_ENTRY_MISMATCH') AND status IN ('open','operator_required')")
        .bind(user_id).bind(exchange).bind(token).execute(&state.db).await?;
    Ok(())
}

type LocalBrokerPositionRow = (
    Uuid,
    String,
    String,
    String,
    String,
    String,
    i32,
    f64,
    String,
    String,
    Option<i32>,
    DateTime<Utc>,
    String,
    f64,
);

async fn reconcile_broker_positions(
    state: &AppState,
    user_id: Uuid,
    broker_credential_revision: i64,
    value: &Value,
    trade_book: Option<&Value>,
    order_book: &Value,
) -> AppResult<()> {
    let broker_positions = parse_authoritative_broker_positions(value)?;
    let broker_trade_fills = trade_book.map(parse_broker_trade_fills).transpose()?;
    let broker_orders = broker_book_items(order_book, "order-book")?;
    let known_orders: Vec<(String, String)> = sqlx::query_as(
        "SELECT broker_order_id,client_order_id FROM strategy_orders
         WHERE user_id=$1 AND execution_mode='live'
           AND (broker_order_id<>'' OR client_order_id<>'')",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let known_broker_ids: HashSet<String> = known_orders
        .iter()
        .map(|(broker_id, _)| broker_id.clone())
        .filter(|value| !value.is_empty())
        .collect();
    let known_client_ids: HashSet<String> = known_orders
        .iter()
        .map(|(_, client_id)| client_id.clone())
        .filter(|value| !value.is_empty())
        .collect();
    let broker_by_key: HashMap<(String, String), BrokerNetPosition> = broker_positions
        .into_iter()
        .map(|position| {
            (
                (position.exchange.clone(), position.token.clone()),
                position,
            )
        })
        .collect();
    let recently_closed: Vec<(Uuid, String, String)> = sqlx::query_as("SELECT t.id,s.exchange_segment,s.contract_token FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id WHERE t.user_id=$1 AND t.execution_mode='live' AND t.status='closed' AND t.exit_datetime>NOW()-INTERVAL '1 day' AND s.contract_token IS NOT NULL")
        .bind(user_id).fetch_all(&state.db).await?;
    for (trade_id, exchange, token) in recently_closed {
        let broker = broker_by_key.get(&(exchange.to_uppercase(), token));
        sqlx::query("UPDATE trades SET broker_net_quantity=$2,broker_average_price=$3,last_position_reconciled_at=NOW(),updated_at=NOW() WHERE id=$1")
            .bind(trade_id)
            .bind(broker.map_or(0, |position| position.net_quantity))
            .bind(broker.map(|position| position.average_price).filter(|value| *value>0.0))
            .execute(&state.db).await?;
    }
    let locals: Vec<LocalBrokerPositionRow> = sqlx::query_as("SELECT t.id,t.strategy_key,t.instrument_label,t.direction,s.exchange_segment,s.contract_token,t.quantity,t.entry_price::float8,t.safety_status,t.exposure_origin,s.lot_size,t.entry_datetime,COALESCE(t.contract_symbol,''),t.pnl::float8 FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id WHERE t.user_id=$1 AND t.execution_mode='live' AND t.status='open' AND s.contract_token IS NOT NULL")
        .bind(user_id).fetch_all(&state.db).await?;
    let mut local_groups: HashMap<(String, String), Vec<LocalBrokerPositionRow>> = HashMap::new();
    for mut local in locals {
        let key = (local.4.to_uppercase(), local.5.clone());
        let broker_quantity = broker_by_key
            .get(&key)
            .map_or(0, |position| position.net_quantity);
        if local.8 == "EMERGENCY_CLOSING" && local.9 == "broker_over_close" && broker_quantity != 0
        {
            let verified_quantity = broker_quantity.unsigned_abs().min(i32::MAX as u32) as i32;
            let verified_direction = if broker_quantity > 0 { "BUY" } else { "SELL" };
            let lot_size = local.10.unwrap_or(1).max(1);
            let reporting_lots = ((verified_quantity + lot_size - 1) / lot_size).max(1);
            sqlx::query("UPDATE trades SET direction=$2,quantity=$3,remaining_lots=$4,broker_net_quantity=$5,broker_average_price=$6,last_position_reconciled_at=NOW(),updated_at=NOW() WHERE id=$1 AND status='open' AND exposure_origin='broker_over_close'")
                .bind(local.0)
                .bind(verified_direction)
                .bind(verified_quantity)
                .bind(reporting_lots)
                .bind(broker_quantity)
                .bind(broker_by_key.get(&key).map(|position| position.average_price))
                .execute(&state.db)
                .await?;
            local.3 = verified_direction.to_owned();
            local.6 = verified_quantity;
        }
        local_groups.entry(key).or_default().push(local);
    }
    let local_keys: HashSet<(String, String)> = local_groups.keys().cloned().collect();
    for ((exchange_key, token_key), group) in &local_groups {
        let first = &group[0];
        let local_signed_i64: i64 = group
            .iter()
            .map(|local| {
                if local.3 == "BUY" {
                    i64::from(local.6)
                } else {
                    -i64::from(local.6)
                }
            })
            .sum();
        let local_signed = i32::try_from(local_signed_i64).unwrap_or(if local_signed_i64 < 0 {
            i32::MIN
        } else {
            i32::MAX
        });
        let broker = broker_by_key.get(&(exchange_key.clone(), token_key.clone()));
        let broker_quantity = broker.map_or(0, |position| position.net_quantity);
        let flat_position_symbol_matches = broker.is_none_or(|position| {
            first.12.is_empty() || position.symbol.eq_ignore_ascii_case(&first.12)
        });
        if broker_quantity == 0 && group.len() == 1 && flat_position_symbol_matches {
            let local = &group[0];
            let known_close_fill: Option<(f64, String)> = sqlx::query_as(
                "SELECT average_fill_price::float8,session_key
                 FROM strategy_orders
                 WHERE trade_id=$1 AND role='EMERGENCY_CLOSE' AND status='filled'
                   AND processed_quantity=quantity AND processed_quantity>= $2 AND average_fill_price>0
                 ORDER BY updated_at DESC,id DESC LIMIT 1",
            )
            .bind(local.0)
            .bind(local.6)
            .fetch_optional(&state.db)
            .await?;
            if let Some((exit_price, session_key)) = known_close_fill {
                cancel_active_exits(state, user_id, local.0).await?;
                let active_protection: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=$1 AND role IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE') AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))")
                    .bind(local.0).fetch_one(&state.db).await?;
                if !active_protection {
                    let realized = trade_pnl(
                        &local.3,
                        local.7,
                        exit_price,
                        runtime_pnl_units(&local.2, local.6, local.10),
                    );
                    let reason = recorded_exit_reason(&local.1, "EMERGENCY_CLOSE", &session_key);
                    sqlx::query("UPDATE trades SET status='closed',safety_status='CLOSED',remaining_lots=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=($3::float8)::numeric,exit_datetime=NOW(),exit_reason=$4,broker_net_quantity=0,last_position_reconciled_at=NOW(),notes=CONCAT(notes,'; broker flatness confirmed after attributable Rulenix close fill'),updated_at=NOW() WHERE id=$1 AND status='open'")
                        .bind(local.0).bind(exit_price).bind(local.13 + realized).bind(reason).execute(&state.db).await?;
                    sqlx::query("UPDATE manual_trade_close_intents SET status='completed',completed_at=NOW(),last_error='',updated_at=NOW() WHERE trade_id=$1")
                        .bind(local.0).execute(&state.db).await?;
                    resolve_position_incidents(state, user_id, &local.4, &local.5).await?;
                }
                continue;
            }
            let known_order_ids: HashSet<String> = sqlx::query_scalar(
                "SELECT broker_order_id FROM strategy_orders WHERE trade_id=$1 AND broker_order_id<>''",
            )
            .bind(local.0)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .collect();
            let close_side = if local.3 == "BUY" { "SELL" } else { "BUY" };
            let saved_evidence: Option<(i32, f64, DateTime<Utc>, DateTime<Utc>)> = sqlx::query_as(
                "SELECT filled_quantity,weighted_fill_price::float8,first_fill_at,last_fill_at
                 FROM manual_broker_close_evidence
                 WHERE trade_id=$1 AND user_id=$2 AND broker_credential_revision=$3
                   AND exchange_segment=$4 AND contract_token=$5 AND contract_symbol=$6
                   AND close_side=$7 AND filled_quantity=$8 AND consumed_at IS NULL",
            )
            .bind(local.0)
            .bind(user_id)
            .bind(broker_credential_revision)
            .bind(exchange_key)
            .bind(token_key)
            .bind(&local.12)
            .bind(close_side)
            .bind(local.6)
            .fetch_optional(&state.db)
            .await?;
            let mut manual_evidence =
                saved_evidence.map(|(quantity, weighted_price, first_fill_at, last_fill_at)| {
                    AttributedManualClose {
                        quantity,
                        weighted_price,
                        order_ids: Vec::new(),
                        first_fill_at,
                        last_fill_at,
                    }
                });
            if manual_evidence.is_none() {
                let expected_signed = if local.3 == "BUY" { local.6 } else { -local.6 };
                let prior_position: (Option<i32>, Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
                    sqlx::query_as("SELECT broker_net_quantity,last_position_reconciled_at,last_exact_broker_exposure_at FROM trades WHERE id=$1")
                        .bind(local.0).fetch_one(&state.db).await?;
                let evidence_since = prior_position.2.or_else(|| {
                    (prior_position.0 == Some(expected_signed))
                        .then_some(prior_position.1)
                        .flatten()
                });
                manual_evidence = evidence_since.and_then(|evidence_since| {
                    attributable_manual_flat_fill(
                        broker_trade_fills.as_deref().unwrap_or_default(),
                        &ManualFillExpectation {
                            exchange: exchange_key,
                            token: token_key,
                            symbol: &local.12,
                            direction: &local.3,
                            quantity: local.6,
                            entry_at: local.11,
                            evidence_since,
                            known_order_ids: &known_order_ids,
                        },
                    )
                });
                if let Some(evidence) = &manual_evidence {
                    sqlx::query(
                        "INSERT INTO manual_broker_close_evidence
                         (trade_id,user_id,broker_credential_revision,exchange_segment,contract_token,contract_symbol,close_side,
                          filled_quantity,weighted_fill_price,broker_order_ids,first_fill_at,last_fill_at)
                         VALUES($1,$2,$3,$4,$5,$6,$7,$8,($9::float8)::numeric,$10,$11,$12)
                         ON CONFLICT(trade_id) DO NOTHING",
                    )
                    .bind(local.0)
                    .bind(user_id)
                    .bind(broker_credential_revision)
                    .bind(exchange_key)
                    .bind(token_key)
                    .bind(&local.12)
                    .bind(close_side)
                    .bind(evidence.quantity)
                    .bind(evidence.weighted_price)
                    .bind(&evidence.order_ids)
                    .bind(evidence.first_fill_at)
                    .bind(evidence.last_fill_at)
                    .execute(&state.db)
                    .await?;
                }
            }
            if let Some(evidence) = manual_evidence {
                let exit_price = evidence.weighted_price;
                sqlx::query("UPDATE trades SET safety_status='RECONCILIATION_REQUIRED',broker_net_quantity=0,last_position_reconciled_at=NOW(),updated_at=NOW() WHERE id=$1 AND status='open'")
                    .bind(local.0).execute(&state.db).await?;
                cancel_active_exits(state, user_id, local.0).await?;
                let active_protection: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=$1 AND role IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE') AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))")
                    .bind(local.0).fetch_one(&state.db).await?;
                if !active_protection {
                    let realized = trade_pnl(
                        &local.3,
                        local.7,
                        exit_price,
                        runtime_pnl_units(&local.2, local.6, local.10),
                    );
                    let mut close_tx = state.db.begin().await?;
                    let changed = sqlx::query("UPDATE trades SET status='closed',safety_status='CLOSED',remaining_lots=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=($3::float8)::numeric,exit_datetime=$4,exit_reason='MANUAL_BROKER_CLOSE',broker_net_quantity=0,last_position_reconciled_at=NOW(),notes=CONCAT(notes,'; broker-side manual close verified from position and trade books'),updated_at=NOW() WHERE id=$1 AND status='open'")
                        .bind(local.0).bind(exit_price).bind(local.13 + realized).bind(evidence.last_fill_at).execute(&mut *close_tx).await?;
                    if changed.rows_affected() > 0 {
                        sqlx::query("UPDATE manual_trade_close_intents SET status='completed',completed_at=NOW(),last_error='',updated_at=NOW() WHERE trade_id=$1")
                            .bind(local.0).execute(&mut *close_tx).await?;
                        sqlx::query("UPDATE manual_broker_close_evidence SET consumed_at=COALESCE(consumed_at,NOW()) WHERE trade_id=$1")
                            .bind(local.0).execute(&mut *close_tx).await?;
                        close_tx.commit().await?;
                        resolve_position_incidents(state, user_id, &local.4, &local.5).await?;
                        operational_alert_for(state,&local.1,Some(user_id),&local.2,"broker_manual_close_reconciled","info",&format!("Broker position and attributable trade-book fills confirmed manual closure of trade {} at weighted fill price {:.4}.",local.0,exit_price)).await;
                    } else {
                        close_tx.rollback().await?;
                    }
                }
                continue;
            }
        }
        if let Some(incident_type) = position_mismatch_type(broker_quantity, local_signed) {
            let detail = format!(
                "Broker/local aggregate exposure mismatch for {}: broker net quantity {broker_quantity}, local signed quantity {local_signed} across {} open trade row(s).",
                first.2,
                group.len()
            );
            record_position_incident(
                state,
                user_id,
                &first.1,
                &first.2,
                &first.4,
                &first.5,
                broker.map_or("", |position| position.symbol.as_str()),
                incident_type,
                broker_quantity,
                local_signed,
                broker.map(|position| position.average_price),
                Some(first.0),
                &detail,
                broker.map_or("", |position| position.product.as_str()),
                broker.map(|position| &position.raw),
            )
            .await?;
            for local in group.iter().skip(1) {
                sqlx::query("UPDATE trades SET safety_status='RECONCILIATION_REQUIRED',broker_net_quantity=$2,broker_average_price=$3,last_position_reconciled_at=NOW(),updated_at=NOW() WHERE id=$1 AND status='open'")
                    .bind(local.0).bind(broker_quantity).bind(broker.map(|position|position.average_price)).execute(&state.db).await?;
            }
            continue;
        }
        resolve_position_incidents(state, user_id, &first.4, &first.5).await?;
        let broker_average = broker
            .map(|position| position.average_price)
            .filter(|value| *value > 0.0);
        for local in group {
            sqlx::query("UPDATE trades SET broker_net_quantity=$2,broker_average_price=$3,last_position_reconciled_at=NOW(),last_exact_broker_exposure_at=CASE WHEN $2=(CASE WHEN direction='BUY' THEN quantity ELSE -quantity END) THEN NOW() ELSE last_exact_broker_exposure_at END,safety_status=CASE WHEN safety_status='RECONCILIATION_REQUIRED' AND (SELECT COALESCE(SUM(GREATEST(quantity-processed_quantity,0)),0) FROM strategy_orders WHERE trade_id=$1 AND role IN ('SL1','SL2') AND status IN ('submitted','partially_filled') AND broker_order_id<>'' AND last_reconciled_at IS NOT NULL)>=$4 THEN 'PROTECTED' WHEN safety_status='RECONCILIATION_REQUIRED' THEN 'PROTECTION_REQUIRED' ELSE safety_status END,updated_at=NOW() WHERE id=$1")
                .bind(local.0).bind(broker_quantity).bind(broker_average).bind(local.6).execute(&state.db).await?;
        }
        let same_direction = group.iter().all(|local| local.3 == first.3);
        let local_average = if same_direction {
            let total_quantity: i64 = group.iter().map(|local| i64::from(local.6)).sum();
            (total_quantity > 0).then(|| {
                group
                    .iter()
                    .map(|local| local.7 * local.6 as f64)
                    .sum::<f64>()
                    / total_quantity as f64
            })
        } else {
            None
        };
        if let (Some(broker_average), Some(local_average)) = (broker_average, local_average)
            && (broker_average - local_average).abs() > 0.01
        {
            let detail = format!(
                "Broker/local average entry mismatch for {}: broker {broker_average:.4}, local aggregate {local_average:.4}.",
                first.2
            );
            record_position_incident(
                state,
                user_id,
                &first.1,
                &first.2,
                &first.4,
                &first.5,
                broker.map_or("", |position| position.symbol.as_str()),
                "AVERAGE_ENTRY_MISMATCH",
                broker_quantity,
                local_signed,
                Some(broker_average),
                Some(first.0),
                &detail,
                broker.map_or("", |position| position.product.as_str()),
                broker.map(|position| &position.raw),
            )
            .await?;
        }
    }
    for ((exchange, token), broker) in broker_by_key {
        if broker.net_quantity == 0 || local_keys.contains(&(exchange.clone(), token.clone())) {
            continue;
        }
        let exposure_ownership = position_fill_ownership(
            &broker,
            broker_trade_fills.as_deref().unwrap_or_default(),
            broker_orders,
            &known_broker_ids,
            &known_client_ids,
        );
        if exposure_ownership == BrokerExposureOwnership::ManualExternal {
            sqlx::query("UPDATE broker_position_incidents SET status='resolved',ownership_status='manual_external',resolved_at=NOW(),last_detected_at=NOW() WHERE user_id=$1 AND exchange_segment=$2 AND contract_token=$3 AND incident_type IN ('ORPHAN_POSITION','UNMAPPED_BROKER_POSITION') AND status IN ('open','operator_required')")
                .bind(user_id).bind(&exchange).bind(&token).execute(&state.db).await?;
            continue;
        }
        let broker_exposure_lock = format!("broker-exposure:{user_id}:{exchange}:{token}");
        let mut broker_exposure_guard = state.db.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,0))")
            .bind(&broker_exposure_lock)
            .execute(&mut *broker_exposure_guard)
            .await?;
        let local_now_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1
                 FROM trades t
                 JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
                 WHERE t.user_id=$1
                   AND t.execution_mode='live'
                   AND t.status='open'
                   AND UPPER(s.exchange_segment)=$2
                   AND s.contract_token=$3
             )",
        )
        .bind(user_id)
        .bind(&exchange)
        .bind(&token)
        .fetch_one(&mut *broker_exposure_guard)
        .await?;
        if local_now_exists {
            broker_exposure_guard.commit().await?;
            continue;
        }
        type OverCloseSourceRow = (Uuid, String, String, Uuid, i32, String, Option<i32>);
        let over_close_source: Option<OverCloseSourceRow> = if exposure_ownership
            == BrokerExposureOwnership::RulenixOwned
        {
            sqlx::query_as(
            "SELECT t.id,t.strategy_key,t.instrument_label,t.strategy_snapshot_id,t.quantity,COALESCE(t.contract_symbol,''),s.lot_size
             FROM trades t
             JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
             WHERE t.user_id=$1 AND t.execution_mode='live' AND t.status='closed'
               AND s.exchange_segment=$2 AND s.contract_token=$3
               AND (SELECT COALESCE(SUM(o.processed_quantity),0) FROM strategy_orders o WHERE o.trade_id=t.id AND o.role IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE'))
                   >(SELECT COALESCE(SUM(entry.processed_quantity),0) FROM strategy_orders entry WHERE entry.trade_id=t.id AND entry.role IN ('BUY_ENTRY','SELL_ENTRY'))
             ORDER BY t.exit_datetime DESC NULLS LAST LIMIT 1",
        )
        .bind(user_id)
        .bind(&exchange)
        .bind(&token)
        .fetch_optional(&state.db)
        .await?
        } else {
            None
        };
        if let Some((
            source_trade_id,
            strategy_key,
            instrument,
            snapshot_id,
            _,
            contract_symbol,
            lot_size,
        )) = over_close_source
        {
            let quantity = broker.net_quantity.unsigned_abs().min(i32::MAX as u32) as i32;
            let lot_size = lot_size.unwrap_or(1).max(1);
            let lots = ((quantity + lot_size - 1) / lot_size).max(1);
            let trade_id = Uuid::new_v4();
            let direction = if broker.net_quantity > 0 {
                "BUY"
            } else {
                "SELL"
            };
            let entry_price = if broker.average_price.is_finite() && broker.average_price > 0.0 {
                broker.average_price
            } else {
                0.05
            };
            sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,reversal_of_trade_id,safety_status,protection_deadline_at,broker_net_quantity,broker_average_price,last_position_reconciled_at,exposure_origin) VALUES($1,$2,'live','open',$3,$4,($5::float8)::numeric,($5::float8)::numeric,0,NOW(),$6,$7,'Broker-confirmed TP/SL over-close exposure reconstructed for deterministic flattening',$8,$9,$10,$10,$11,'EMERGENCY_CLOSING',NOW(),$12,$5,NOW(),'broker_over_close')")
                .bind(trade_id).bind(user_id).bind(direction).bind(quantity).bind(entry_price).bind(&instrument).bind(&contract_symbol).bind(&strategy_key).bind(snapshot_id).bind(lots).bind(source_trade_id).bind(broker.net_quantity).execute(&state.db).await?;
            let detail = format!(
                "Exit orders over-closed source trade {source_trade_id}; broker now holds unintended {} {} units. Exposure was reconstructed as trade {trade_id} and marked EMERGENCY_CLOSING.",
                direction, quantity
            );
            record_position_incident(
                state,
                user_id,
                &strategy_key,
                &instrument,
                &exchange,
                &token,
                &broker.symbol,
                "OVER_CLOSE_POSITION",
                broker.net_quantity,
                broker.net_quantity,
                Some(broker.average_price),
                Some(trade_id),
                &detail,
                &broker.product,
                Some(&broker.raw),
            )
            .await?;
            sqlx::query("UPDATE trades SET safety_status='EMERGENCY_CLOSING' WHERE id=$1")
                .bind(trade_id)
                .execute(&state.db)
                .await?;
            operational_alert_for(
                state,
                &strategy_key,
                Some(user_id),
                &instrument,
                "tp_sl_overfill",
                "critical",
                &detail,
            )
            .await;
            broker_exposure_guard.commit().await?;
            continue;
        }
        let related:Option<(String,String)>=sqlx::query_as("SELECT strategy_key,instrument FROM strategy_market_snapshots WHERE strategy_key IN ($1,$2) AND exchange_segment=$3 AND contract_token=$4 ORDER BY fetched_at DESC LIMIT 1")
            .bind(STRATEGY_KEY).bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY).bind(&exchange).bind(&token).fetch_optional(&state.db).await?;
        let (strategy_key, instrument, incident_type, classification) = match related {
            Some((strategy_key, instrument)) => (
                strategy_key,
                instrument,
                "ORPHAN_POSITION",
                "known strategy contract with no local open exposure",
            ),
            None => (
                "unattributed_broker_exposure".to_owned(),
                if broker.symbol.is_empty() {
                    format!("UNMAPPED:{exchange}:{token}")
                } else {
                    broker.symbol.clone()
                },
                "UNMAPPED_BROKER_POSITION",
                "no matching local trade or historical strategy snapshot",
            ),
        };
        let detail = format!(
            "Detected broker exposure {} ({exchange}/{token}, product {}) with net quantity {} and average price {:.4}: {classification}. Ownership was not assumed; operator attribution is required.",
            if broker.symbol.is_empty() {
                "<symbol unavailable>"
            } else {
                &broker.symbol
            },
            if broker.product.is_empty() {
                "<unavailable>"
            } else {
                &broker.product
            },
            broker.net_quantity,
            broker.average_price
        );
        record_position_incident(
            state,
            user_id,
            &strategy_key,
            &instrument,
            &exchange,
            &token,
            &broker.symbol,
            incident_type,
            broker.net_quantity,
            0,
            Some(broker.average_price),
            None,
            &detail,
            &broker.product,
            Some(&broker.raw),
        )
        .await?;
        broker_exposure_guard.commit().await?;
    }
    let orphan_incidents: Vec<(String, String)> = sqlx::query_as("SELECT exchange_segment,contract_token FROM broker_position_incidents WHERE user_id=$1 AND incident_type IN ('ORPHAN_POSITION','UNMAPPED_BROKER_POSITION','OVER_CLOSE_POSITION') AND status IN ('open','operator_required')")
        .bind(user_id).fetch_all(&state.db).await?;
    let reported = parse_broker_positions(value)
        .into_iter()
        .map(|position| ((position.exchange, position.token), position.net_quantity))
        .collect::<HashMap<_, _>>();
    for (exchange, token) in orphan_incidents {
        if reported
            .get(&(exchange.clone(), token.clone()))
            .copied()
            .unwrap_or(0)
            == 0
        {
            sqlx::query("UPDATE broker_position_incidents SET status='resolved',resolved_at=NOW(),last_detected_at=NOW() WHERE user_id=$1 AND exchange_segment=$2 AND contract_token=$3 AND incident_type IN ('ORPHAN_POSITION','UNMAPPED_BROKER_POSITION','OVER_CLOSE_POSITION') AND status IN ('open','operator_required')")
                .bind(user_id).bind(exchange).bind(token).execute(&state.db).await?;
        }
    }
    Ok(())
}

fn valid_order_transition(from: &str, to: &str) -> bool {
    from == to
        || matches!(
            (from, to),
            (
                "pending",
                "submitting" | "submitted" | "failed" | "rejected" | "cancelled"
            ) | (
                "submitting",
                "submitted" | "ambiguous" | "failed" | "rejected"
            ) | (
                "ambiguous",
                "submitted" | "partially_filled" | "rejected" | "cancelled" | "cancelling"
            ) | (
                "submitted",
                "partially_filled"
                    | "processing"
                    | "filled"
                    | "rejected"
                    | "cancelled"
                    | "cancelling"
            ) | (
                "partially_filled",
                "submitted" | "processing" | "filled" | "rejected" | "cancelled" | "cancelling"
            ) | (
                "processing",
                "submitted" | "partially_filled" | "filled" | "cancelled" | "rejected"
            ) | (
                "cancelling",
                "submitted"
                    | "partially_filled"
                    | "filled"
                    | "rejected"
                    | "cancelled"
                    | "processing"
            ) | ("failed", "pending")
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReconciliationPlan {
    prepare_state: &'static str,
    terminal_state: Option<&'static str>,
    process_delta: bool,
    request_cancel: bool,
    cancellation_in_flight: bool,
}

fn broker_terminal_state(status: &str) -> Option<&'static str> {
    if matches!(status, "complete" | "completed" | "filled") {
        Some("filled")
    } else if status == "rejected" {
        Some("rejected")
    } else if matches!(status, "cancelled" | "canceled") {
        Some("cancelled")
    } else {
        None
    }
}

fn broker_fill_watermark(reported: i32, stored: i32, processed: i32, requested: i32) -> i32 {
    reported
        .max(stored)
        .max(processed)
        .clamp(0, requested.max(0))
}

fn incremental_fill_price(
    processed: i32,
    previous_average: Option<f64>,
    cumulative_filled: i32,
    cumulative_average: f64,
) -> f64 {
    let delta = cumulative_filled.saturating_sub(processed);
    if processed <= 0 || delta <= 0 {
        return cumulative_average;
    }
    let Some(previous_average) = previous_average.filter(|value| value.is_finite() && *value > 0.0)
    else {
        return cumulative_average;
    };
    let delta_price = (cumulative_average * cumulative_filled as f64
        - previous_average * processed as f64)
        / delta as f64;
    if delta_price.is_finite() && delta_price > 0.0 {
        delta_price
    } else {
        cumulative_average
    }
}

fn reconciliation_plan(
    local_status: &str,
    broker_status: &str,
    cumulative_filled: i32,
    processed: i32,
) -> ReconciliationPlan {
    let terminal_state = broker_terminal_state(broker_status);
    let process_delta = cumulative_filled > processed;
    let cancellation_in_flight = local_status == "cancelling" && terminal_state.is_none();
    let request_cancel =
        terminal_state.is_none() && cumulative_filled > 0 && local_status != "cancelling";
    let prepare_state = if process_delta {
        // `complete_order` claims only reconcilable fill states. Cancellation
        // intent is restored after the newly observed delta is committed.
        "submitted"
    } else if let Some(terminal_state) = terminal_state {
        terminal_state
    } else if cancellation_in_flight {
        "cancelling"
    } else if cumulative_filled > 0 {
        "partially_filled"
    } else {
        "submitted"
    };
    ReconciliationPlan {
        prepare_state,
        terminal_state,
        process_delta,
        request_cancel,
        cancellation_in_flight,
    }
}

fn reconciled_state(status: &str, filled: i32) -> &'static str {
    if matches!(status, "complete" | "completed" | "filled") {
        "filled"
    } else if status == "rejected" {
        "rejected"
    } else if matches!(status, "cancelled" | "canceled") {
        "cancelled"
    } else if filled > 0 {
        "partially_filled"
    } else {
        "submitted"
    }
}

fn broker_order_is_terminal(status: &str) -> bool {
    matches!(
        status,
        "complete" | "completed" | "filled" | "cancelled" | "canceled" | "rejected" | "expired"
    )
}

fn conditional_rule_is_active(rule: &Value) -> bool {
    let status = broker_text(rule, &["status", "ruleStatus", "rulestatus"])
        .unwrap_or("")
        .trim()
        .to_uppercase();
    !matches!(
        status.as_str(),
        "CANCELLED" | "CANCELED" | "REJECTED" | "EXPIRED" | "COMPLETED" | "COMPLETE"
    )
}

type ExposureObservation = (
    String,
    String,
    BrokerExposureOwnership,
    String,
    String,
    String,
    String,
    i32,
    String,
);

async fn replace_broker_exposure_observations(
    state: &AppState,
    user_id: Uuid,
    broker_credential_revision: i64,
    positions: &[BrokerNetPosition],
    order_book: &Value,
    trade_book: &Value,
    conditional_rules: &[Value],
) -> AppResult<(i64, i64, i64)> {
    let orders = broker_book_items(order_book, "order-book")?;
    let fills = parse_broker_trade_fills(trade_book)?;
    let known_orders: Vec<(String, String)> = sqlx::query_as(
        "SELECT broker_order_id,client_order_id FROM strategy_orders
         WHERE user_id=$1 AND execution_mode='live'
           AND (broker_order_id<>'' OR client_order_id<>'')",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;
    let known_broker_ids: HashSet<String> = known_orders
        .iter()
        .map(|(broker_id, _)| broker_id.clone())
        .filter(|value| !value.is_empty())
        .collect();
    let known_client_ids: HashSet<String> = known_orders
        .iter()
        .map(|(_, client_id)| client_id.clone())
        .filter(|value| !value.is_empty())
        .collect();
    let local_contracts: HashSet<(String, String)> = sqlx::query_as(
        "SELECT UPPER(s.exchange_segment),s.contract_token
         FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
         WHERE t.user_id=$1 AND t.execution_mode='live' AND t.status='open'
           AND s.contract_token IS NOT NULL",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let mut observations: Vec<ExposureObservation> = Vec::new();
    for position in positions
        .iter()
        .filter(|position| position.net_quantity != 0)
    {
        let ownership =
            if local_contracts.contains(&(position.exchange.clone(), position.token.clone())) {
                BrokerExposureOwnership::RulenixOwned
            } else {
                position_fill_ownership(
                    position,
                    &fills,
                    orders,
                    &known_broker_ids,
                    &known_client_ids,
                )
            };
        observations.push((
            "position".into(),
            format!("{}:{}", position.exchange, position.token),
            ownership,
            position.exchange.clone(),
            position.token.clone(),
            position.symbol.clone(),
            if position.net_quantity > 0 {
                "BUY"
            } else {
                "SELL"
            }
            .into(),
            position.net_quantity,
            match ownership {
                BrokerExposureOwnership::RulenixOwned => {
                    "open local trade or exclusively Rulenix-attributable fills"
                }
                BrokerExposureOwnership::ManualExternal => {
                    "net position exclusively matches complete external broker fills"
                }
                BrokerExposureOwnership::Ambiguous => {
                    "position fill ownership is incomplete or mixed"
                }
            }
            .into(),
        ));
    }
    for (index, item) in orders.iter().enumerate() {
        let status = broker_text(item, &["status", "orderstatus", "orderStatus"])
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if broker_order_is_terminal(&status) {
            continue;
        }
        let mut ownership = broker_order_ownership(item, &known_broker_ids, &known_client_ids);
        if status.is_empty() {
            ownership = BrokerExposureOwnership::Ambiguous;
        }
        let broker_id = broker_text(item, &["orderid", "orderId"])
            .unwrap_or("")
            .trim();
        let client_id = broker_text(item, &["ordertag", "orderTag"])
            .unwrap_or("")
            .trim();
        observations.push((
            "order".into(),
            if !broker_id.is_empty() {
                broker_id.into()
            } else if !client_id.is_empty() {
                client_id.into()
            } else {
                format!("unknown:{index}")
            },
            ownership,
            broker_text(item, &["exchange"])
                .unwrap_or("")
                .trim()
                .to_uppercase(),
            broker_text(item, &["symboltoken", "symbolToken"])
                .unwrap_or("")
                .trim()
                .into(),
            broker_text(item, &["tradingsymbol", "tradingSymbol"])
                .unwrap_or("")
                .trim()
                .into(),
            broker_text(item, &["transactiontype", "transactionType", "side"])
                .unwrap_or("")
                .trim()
                .to_uppercase(),
            broker_i32(item, &["quantity"]).unwrap_or(0),
            match ownership {
                BrokerExposureOwnership::RulenixOwned => {
                    "durable local broker order ID or client tag"
                }
                BrokerExposureOwnership::ManualExternal => {
                    "complete unmatched broker order without a Rulenix tag"
                }
                BrokerExposureOwnership::Ambiguous => {
                    "unmatched Rulenix tag or incomplete broker order"
                }
            }
            .into(),
        ));
    }
    for (index, rule) in conditional_rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| conditional_rule_is_active(rule))
    {
        let reference = broker_text(rule, &["id", "ruleid", "ruleId", "uniqueid", "uniqueId"])
            .unwrap_or("")
            .trim();
        let exchange = broker_text(rule, &["exchange"])
            .unwrap_or("")
            .trim()
            .to_uppercase();
        let token = broker_text(rule, &["symboltoken", "symbolToken"])
            .unwrap_or("")
            .trim();
        let quantity = broker_i32(rule, &["qty", "quantity"]).unwrap_or(0);
        let ownership =
            if !reference.is_empty() && !exchange.is_empty() && !token.is_empty() && quantity > 0 {
                BrokerExposureOwnership::ManualExternal
            } else {
                BrokerExposureOwnership::Ambiguous
            };
        observations.push((
            "conditional".into(),
            if reference.is_empty() {
                format!("unknown:{index}")
            } else {
                reference.into()
            },
            ownership,
            exchange,
            token.into(),
            broker_text(rule, &["tradingsymbol", "tradingSymbol"])
                .unwrap_or("")
                .trim()
                .into(),
            broker_text(rule, &["transactiontype", "transactionType", "side"])
                .unwrap_or("")
                .trim()
                .to_uppercase(),
            quantity,
            if ownership == BrokerExposureOwnership::ManualExternal {
                "complete external conditional rule"
            } else {
                "incomplete conditional rule"
            }
            .into(),
        ));
    }
    let counts = observations
        .iter()
        .fold((0_i64, 0_i64, 0_i64), |mut counts, row| {
            match row.2 {
                BrokerExposureOwnership::RulenixOwned => counts.0 += 1,
                BrokerExposureOwnership::ManualExternal => counts.1 += 1,
                BrokerExposureOwnership::Ambiguous => counts.2 += 1,
            }
            counts
        });
    let mut transaction = state.db.begin().await?;
    sqlx::query("DELETE FROM broker_exposure_observations WHERE user_id=$1")
        .bind(user_id)
        .execute(&mut *transaction)
        .await?;
    for row in observations {
        sqlx::query("INSERT INTO broker_exposure_observations(user_id,exposure_kind,broker_reference,ownership_status,exchange_segment,contract_token,contract_symbol,side,quantity,evidence,broker_credential_revision,observed_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,NOW())")
            .bind(user_id).bind(row.0).bind(row.1).bind(row.2.as_str()).bind(row.3).bind(row.4).bind(row.5).bind(row.6).bind(row.7).bind(row.8).bind(broker_credential_revision).execute(&mut *transaction).await?;
    }
    transaction.commit().await?;
    Ok(counts)
}

async fn live_clear_rejection(state: &AppState, user_id: Uuid, detail: String) -> AppError {
    let _ = risk::set_reconciliation_health(state, user_id, false, &detail).await;
    AppError::BadRequest(detail)
}

/// Perform the authoritative, read-only broker inventory required before an
/// administrator may remove local LIVE history. This function deliberately
/// exposes no broker mutation path: any unavailable or structurally unknown
/// response fails closed and marks LIVE reconciliation unhealthy.
pub(crate) async fn verify_broker_safe_for_live_clear(
    state: &AppState,
    user_id: Uuid,
) -> AppResult<()> {
    let credentials = match state.credentials.load(user_id).await {
        Ok(credentials) => credentials,
        Err(error) => {
            return Err(live_clear_rejection(
                state,
                user_id,
                format!("Clear LIVE requires a connected, readable Angel account: {error}"),
            )
            .await);
        }
    };
    let order_book = match angel::order_book(
        state,
        user_id,
        &credentials.api_key,
        &credentials.jwt_token,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ =
                risk::set_reconciliation_health(state, user_id, false, &error.to_string()).await;
            return Err(AppError::BadRequest(format!(
                "Clear LIVE refused because the Angel order book is unreadable; broker state is unknown: {error}"
            )));
        }
    };
    let positions = match angel::positions(
        state,
        user_id,
        &credentials.api_key,
        &credentials.jwt_token,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ =
                risk::set_reconciliation_health(state, user_id, false, &error.to_string()).await;
            return Err(AppError::BadRequest(format!(
                "Clear LIVE refused because Angel positions are unreadable; broker state is unknown: {error}"
            )));
        }
    };
    let trade_book = match angel::trade_book(
        state,
        user_id,
        &credentials.api_key,
        &credentials.jwt_token,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ =
                risk::set_reconciliation_health(state, user_id, false, &error.to_string()).await;
            return Err(AppError::BadRequest(format!(
                "Clear LIVE refused because the Angel trade book is unreadable; broker state is unknown: {error}"
            )));
        }
    };
    let conditional_rules = match angel::conditional_rules(
        state,
        user_id,
        &credentials.api_key,
        &credentials.jwt_token,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ =
                risk::set_reconciliation_health(state, user_id, false, &error.to_string()).await;
            return Err(AppError::BadRequest(format!(
                "Clear LIVE refused because Angel conditional/GTT state is unreadable; broker state is unknown: {error}"
            )));
        }
    };

    let broker_positions = if positions.is_null() {
        Vec::new()
    } else {
        let Some(raw) = positions.as_array() else {
            return Err(live_clear_rejection(
                state,
                user_id,
                "Clear LIVE refused because Angel returned malformed position data.".into(),
            )
            .await);
        };
        let parsed = parse_broker_positions(&positions);
        if parsed.len() != raw.len() {
            return Err(live_clear_rejection(
                state,
                user_id,
                "Clear LIVE refused because an Angel position could not be classified safely."
                    .into(),
            )
            .await);
        }
        parsed
    };
    let open_positions = broker_positions
        .iter()
        .filter(|position| position.net_quantity != 0)
        .count();
    if open_positions > 0 {
        return Err(live_clear_rejection(
            state,
            user_id,
            format!("Clear LIVE refused because Angel reports {open_positions} open broker position(s). Close and reconcile broker exposure first."),
        )
        .await);
    }

    if !order_book.is_null() {
        let Some(orders) = order_book.as_array() else {
            return Err(live_clear_rejection(
                state,
                user_id,
                "Clear LIVE refused because Angel returned malformed order-book data.".into(),
            )
            .await);
        };
        let unsafe_orders = orders
            .iter()
            .filter(|order| {
                let status = broker_text(order, &["status", "orderstatus", "orderStatus"])
                    .unwrap_or("")
                    .trim()
                    .to_lowercase();
                status.is_empty() || !broker_order_is_terminal(&status)
            })
            .count();
        if unsafe_orders > 0 {
            return Err(live_clear_rejection(
                state,
                user_id,
                format!("Clear LIVE refused because Angel reports {unsafe_orders} active or structurally unknown broker order(s)."),
            )
            .await);
        }
    }
    if !trade_book.is_null() && !trade_book.is_array() {
        return Err(live_clear_rejection(
            state,
            user_id,
            "Clear LIVE refused because Angel returned malformed trade-book data.".into(),
        )
        .await);
    }
    let active_conditionals = conditional_rules
        .iter()
        .filter(|rule| conditional_rule_is_active(rule))
        .count();
    if active_conditionals > 0 {
        return Err(live_clear_rejection(
            state,
            user_id,
            format!("Clear LIVE refused because Angel reports {active_conditionals} active conditional/GTT rule(s)."),
        )
        .await);
    }
    Ok(())
}

async fn reconcile_live_user_with_scope(
    state: &AppState,
    user_id: Uuid,
    full_readiness: bool,
) -> AppResult<()> {
    let broker_credential_revision: i64 =
        sqlx::query_scalar("SELECT broker_credential_revision FROM user_profiles WHERE user_id=$1")
            .bind(user_id)
            .fetch_one(&state.db)
            .await?;
    let credentials = state.credentials.load(user_id).await?;
    let values =
        match angel::order_book(state, user_id, &credentials.api_key, &credentials.jwt_token).await
        {
            Ok(values) => values,
            Err(error) => {
                let _ = risk::set_reconciliation_health(state, user_id, false, &error.to_string())
                    .await;
                operational_alert(
                state,
                Some(user_id),
                "",
                "broker_reconcile_failed",
                "error",
                &format!(
                    "Angel One order reconciliation failed; it will retry automatically: {error}"
                ),
            )
            .await;
                return Err(error);
            }
        };
    let positions = match angel::positions(
        state,
        user_id,
        &credentials.api_key,
        &credentials.jwt_token,
    )
    .await
    {
        Ok(values) => values,
        Err(error) => {
            let _ =
                risk::set_reconciliation_health(state, user_id, false, &error.to_string()).await;
            operational_alert(state,Some(user_id),"","broker_position_reconcile_failed","error",&format!("Angel One net-position reconciliation failed; new live entries are blocked: {error}")).await;
            return Err(error);
        }
    };
    if let Err(error) = broker_book_items(&values, "order-book") {
        let _ = risk::set_reconciliation_health(state, user_id, false, &error.to_string()).await;
        return Err(error);
    }
    let broker_positions = match parse_authoritative_broker_positions(&positions) {
        Ok(positions) => positions,
        Err(error) => {
            let _ =
                risk::set_reconciliation_health(state, user_id, false, &error.to_string()).await;
            return Err(error);
        }
    };
    let current_credential_revision: i64 =
        sqlx::query_scalar("SELECT broker_credential_revision FROM user_profiles WHERE user_id=$1")
            .bind(user_id)
            .fetch_one(&state.db)
            .await?;
    if current_credential_revision != broker_credential_revision {
        let detail =
            "Broker credentials changed during reconciliation; broker state was discarded.";
        let _ = risk::set_reconciliation_health(state, user_id, false, detail).await;
        return Err(AppError::BadRequest(detail.into()));
    }
    let local_contracts: Vec<(Uuid, String, String, bool)> = sqlx::query_as(
        "SELECT t.id,UPPER(s.exchange_segment),s.contract_token,
                EXISTS(SELECT 1 FROM manual_broker_close_evidence evidence
                       WHERE evidence.trade_id=t.id AND evidence.user_id=t.user_id
                         AND evidence.broker_credential_revision=$2
                         AND evidence.consumed_at IS NULL)
         FROM trades t
         JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
         WHERE t.user_id=$1 AND t.execution_mode='live' AND t.status='open'
           AND s.contract_token IS NOT NULL",
    )
    .bind(user_id)
    .bind(broker_credential_revision)
    .fetch_all(&state.db)
    .await?;
    let needs_trade_book = local_contracts
        .iter()
        .any(|(_, exchange, token, has_evidence)| {
            if *has_evidence {
                return false;
            }
            broker_positions
                .iter()
                .find(|position| position.exchange == *exchange && position.token == *token)
                .is_none_or(|position| position.net_quantity == 0)
        });
    let trade_book = if full_readiness || needs_trade_book {
        match angel::trade_book(state, user_id, &credentials.api_key, &credentials.jwt_token).await
        {
            Ok(value) => match parse_broker_trade_fills(&value) {
                Ok(_) => Some(value),
                Err(error) => {
                    let _ =
                        risk::set_reconciliation_health(state, user_id, false, &error.to_string())
                            .await;
                    return Err(error);
                }
            },
            Err(error) => {
                let _ = risk::set_reconciliation_health(state, user_id, false, &error.to_string())
                    .await;
                operational_alert(state,Some(user_id),"","broker_trade_reconcile_failed","error",&format!("Angel One trade-book reconciliation failed; a broker-flat local trade will remain unresolved: {error}")).await;
                return Err(error);
            }
        }
    } else {
        None
    };
    let conditional_rules = if full_readiness {
        match angel::conditional_rules(state, user_id, &credentials.api_key, &credentials.jwt_token)
            .await
        {
            Ok(value) => Some(value),
            Err(error) => {
                let _ = risk::set_reconciliation_health(state, user_id, false, &error.to_string())
                    .await;
                operational_alert(state,Some(user_id),"","broker_conditional_reconcile_failed","error",&format!("Angel One conditional-order reconciliation failed; new live entries remain blocked: {error}")).await;
                return Err(error);
            }
        }
    } else {
        None
    };
    let mut by_id = HashMap::new();
    let mut by_tag = HashMap::new();
    for item in values.as_array().into_iter().flatten() {
        if let Some(id) = broker_text(item, &["orderid", "orderId"]) {
            by_id.insert(id.to_string(), item);
        }
        if let Some(tag) = broker_text(item, &["ordertag", "orderTag"]) {
            by_tag.insert(tag.to_string(), item);
        }
    }
    let orders: Vec<StoredOrder>=sqlx::query_as("SELECT id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,broker_order_id,client_order_id,status,filled_quantity,processed_quantity,average_fill_price::float8 FROM strategy_orders WHERE user_id=$1 AND execution_mode='live' AND status IN ('submitting','ambiguous','submitted','partially_filled','processing','cancelling') ORDER BY CASE WHEN role IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE') THEN 0 ELSE 1 END,created_at")
            .bind(user_id).fetch_all(&state.db).await?;
    for mut order in orders {
        let current_status: Option<String> =
            sqlx::query_scalar("SELECT status FROM strategy_orders WHERE id=$1")
                .bind(order.id)
                .fetch_optional(&state.db)
                .await?;
        let Some(current_status) = current_status else {
            continue;
        };
        order.status = current_status;
        if !matches!(
            order.status.as_str(),
            "submitting"
                | "ambiguous"
                | "submitted"
                | "partially_filled"
                | "processing"
                | "cancelling"
        ) {
            continue;
        }
        let item = if !order.broker_order_id.is_empty() {
            by_id.get(&order.broker_order_id)
        } else {
            None
        }
        .or_else(|| by_tag.get(&order.client_order_id));
        let Some(item) = item else {
            if matches!(order.status.as_str(), "ambiguous" | "submitting") {
                sqlx::query("UPDATE strategy_orders SET last_reconciled_at=NOW(),broker_status='Ambiguous submission not present in the latest broker order book; no retry was attempted.',updated_at=NOW() WHERE id=$1").bind(order.id).execute(&state.db).await?;
                let deadline_elapsed: bool = sqlx::query_scalar("SELECT COALESCE(uncertain_since_at,created_at)<NOW()-($2::text || ' seconds')::interval FROM strategy_orders WHERE id=$1")
                    .bind(order.id).bind(state.config.ambiguous_order_timeout_seconds).fetch_one(&state.db).await?;
                if deadline_elapsed {
                    escalate_ambiguous_order(state, &order, &positions).await?;
                }
            }
            continue;
        };
        sqlx::query("UPDATE broker_position_incidents i SET status='resolved',resolved_at=NOW(),last_detected_at=NOW() WHERE i.user_id=$1 AND i.incident_type IN ('AMBIGUOUS_ORDER_DEADLINE','AMBIGUOUS_PROTECTION') AND i.status IN ('open','operator_required') AND i.exchange_segment=(SELECT exchange_segment FROM strategy_market_snapshots WHERE id=$2) AND i.contract_token=(SELECT contract_token FROM strategy_market_snapshots WHERE id=$2) AND NOT EXISTS(SELECT 1 FROM strategy_orders other JOIN strategy_market_snapshots os ON os.id=other.snapshot_id WHERE other.user_id=$1 AND other.id<>$3 AND other.status IN ('submitting','ambiguous') AND os.exchange_segment=i.exchange_segment AND os.contract_token=i.contract_token AND COALESCE(other.uncertain_since_at,other.created_at)<NOW()-($4::text || ' seconds')::interval)")
            .bind(user_id).bind(order.snapshot_id).bind(order.id).bind(state.config.ambiguous_order_timeout_seconds).execute(&state.db).await?;
        let broker_id =
            broker_text(item, &["orderid", "orderId"]).unwrap_or(&order.broker_order_id);
        let status = broker_text(item, &["status", "orderstatus", "orderStatus"])
            .unwrap_or("")
            .to_lowercase();
        let reported_filled = broker_i32(
            item,
            &[
                "filledshares",
                "filledShares",
                "filledquantity",
                "filledQuantity",
            ],
        )
        .unwrap_or(
            if matches!(status.as_str(), "complete" | "completed" | "filled") {
                order.quantity
            } else {
                0
            },
        )
        .clamp(0, order.quantity);
        let filled = broker_fill_watermark(
            reported_filled,
            order.filled_quantity,
            order.processed_quantity,
            order.quantity,
        );
        let cumulative_price = broker_f64(item, &["averageprice", "averagePrice"])
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(order.price);
        let next = reconciled_state(&status, filled);
        let plan = reconciliation_plan(&order.status, &status, filled, order.processed_quantity);
        if !valid_order_transition(&order.status, plan.prepare_state) {
            operational_alert(
                state,
                Some(user_id),
                "",
                "invalid_order_transition",
                "error",
                &format!(
                    "Blocked invalid order transition {} -> {} for {}",
                    order.status, plan.prepare_state, order.id
                ),
            )
            .await;
            continue;
        }
        sqlx::query("UPDATE strategy_orders SET status=$2,broker_order_id=$3,filled_quantity=GREATEST(filled_quantity,$4),average_fill_price=$5,last_reconciled_at=NOW(),broker_status=$6,state_version=state_version+1,updated_at=NOW() WHERE id=$1")
                .bind(order.id).bind(plan.prepare_state).bind(broker_id).bind(filled).bind(cumulative_price).bind(format!("status={status}; filled_quantity={filled}; average_fill_price={cumulative_price:.4}")).execute(&state.db).await?;
        sqlx::query("INSERT INTO broker_order_events(order_id,user_id,from_state,to_state,event_type,broker_order_id,diagnostic,broker_payload) VALUES($1,$2,$3,$4,'reconciled',$5,$6,$7)")
                .bind(order.id).bind(user_id).bind(&order.status).bind(next).bind(broker_id).bind(format!("broker_status={status}; filled={filled}/{}; processed={}",order.quantity,order.processed_quantity)).bind(json!({"status":status,"filled_quantity":filled,"processed_quantity":order.processed_quantity,"average_fill_price":cumulative_price,"broker_order_id":broker_id,"client_order_id":order.client_order_id})).execute(&state.db).await?;

        let mut cancellation_acknowledged = plan.cancellation_in_flight;
        if plan.request_cancel {
            let variety = if order.order_type.starts_with("STOPLOSS") {
                "STOPLOSS"
            } else {
                "NORMAL"
            };
            match angel::cancel_order(
                state,
                user_id,
                &credentials.api_key,
                &credentials.jwt_token,
                broker_id,
                variety,
            )
            .await
            {
                Ok(()) => {
                    cancellation_acknowledged = true;
                    sqlx::query("UPDATE strategy_orders SET broker_status='Partial fill detected; unfilled remainder cancellation requested; awaiting broker reconciliation.',state_version=state_version+1,updated_at=NOW() WHERE id=$1")
                        .bind(order.id).execute(&state.db).await?;
                }
                Err(error) => {
                    operational_alert(
                        state,
                        Some(user_id),
                        "",
                        "partial_fill_cancel_failed",
                        "error",
                        &format!(
                            "A partially filled order could not be frozen at the broker: {error}"
                        ),
                    )
                    .await;
                }
            }
        }

        if plan.process_delta {
            let fill_delta = filled - order.processed_quantity;
            let requested_quantity = order.quantity.max(1) as i64;
            let cumulative_lots = ((order.lots as i64 * filled as i64 + requested_quantity - 1)
                / requested_quantity) as i32;
            let processed_lots =
                ((order.lots as i64 * order.processed_quantity as i64 + requested_quantity - 1)
                    / requested_quantity) as i32;
            let fill_price = incremental_fill_price(
                order.processed_quantity,
                order.average_fill_price,
                filled,
                cumulative_price,
            );
            let mut filled_order = order.clone();
            filled_order.broker_order_id = broker_id.to_string();
            filled_order.filled_quantity = filled;
            filled_order.quantity = fill_delta;
            filled_order.lots = (cumulative_lots - processed_lots).max(0);
            filled_order.status = "submitted".into();
            if let Err(error) = complete_order(state, filled_order, fill_price).await {
                operational_alert(
                    state,
                    Some(user_id),
                    "",
                    "fill_processing_failed",
                    "error",
                    &format!("Broker fill could not be processed; it will retry: {error}"),
                )
                .await;
                // Keep the broker fill retryable. Marking a cancelled/rejected
                // partial order as processed here would permanently lose the
                // fill if the trade transaction failed.
                if plan.terminal_state.is_none() && cancellation_acknowledged {
                    sqlx::query("UPDATE strategy_orders SET status='cancelling',broker_status='Fill processing will retry; broker remainder cancellation is still pending.',state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status IN ('submitted','partially_filled')")
                        .bind(order.id).execute(&state.db).await?;
                }
                continue;
            }
        }

        if let Some(terminal_state) = plan.terminal_state {
            sqlx::query("UPDATE strategy_orders SET status=$2,processed_quantity=GREATEST(processed_quantity,$3),state_version=state_version+1,updated_at=NOW() WHERE id=$1")
                .bind(order.id).bind(terminal_state).bind(filled).execute(&state.db).await?;
        } else if filled > 0 && cancellation_acknowledged {
            sqlx::query("UPDATE strategy_orders SET status='cancelling',broker_status='Unfilled broker remainder cancellation is pending reconciliation.',state_version=state_version+1,updated_at=NOW() WHERE id=$1")
                .bind(order.id).execute(&state.db).await?;
        } else if filled > 0 {
            sqlx::query("UPDATE strategy_orders SET status='partially_filled',broker_status='Partial fill processed; broker remainder cancellation will retry.',state_version=state_version+1,updated_at=NOW() WHERE id=$1")
                .bind(order.id).execute(&state.db).await?;
        }
    }
    let final_credential_revision: i64 =
        sqlx::query_scalar("SELECT broker_credential_revision FROM user_profiles WHERE user_id=$1")
            .bind(user_id)
            .fetch_one(&state.db)
            .await?;
    if final_credential_revision != broker_credential_revision {
        let detail = "Broker credentials changed during reconciliation; broker results were not applied to positions.";
        let _ = risk::set_reconciliation_health(state, user_id, false, detail).await;
        return Err(AppError::BadRequest(detail.into()));
    }
    reconcile_broker_positions(
        state,
        user_id,
        broker_credential_revision,
        &positions,
        trade_book.as_ref(),
        &values,
    )
    .await?;
    let unresolved_incidents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM broker_position_incidents WHERE user_id=$1 AND status IN ('open','operator_required')")
        .bind(user_id).fetch_one(&state.db).await?;
    if unresolved_incidents > 0 {
        risk::set_reconciliation_health(
            state,
            user_id,
            false,
            "Broker/local position reconciliation has unresolved incidents.",
        )
        .await?;
        if full_readiness {
            return Err(AppError::BadRequest(
                "Full broker reconciliation found unresolved position exposure.".into(),
            ));
        }
    }
    if full_readiness {
        let trade_book = trade_book.as_ref().ok_or_else(|| {
            AppError::BadRequest(
                "Full broker readiness requires an authoritative trade book.".into(),
            )
        })?;
        let (rulenix_owned_exposure, manual_external_exposure, ambiguous_exposure) =
            replace_broker_exposure_observations(
                state,
                user_id,
                broker_credential_revision,
                &broker_positions,
                &values,
                trade_book,
                conditional_rules.as_deref().unwrap_or_default(),
            )
            .await?;
        let ambiguous_local_orders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM strategy_orders WHERE user_id=$1 AND execution_mode='live' AND status IN ('submitting','ambiguous')")
            .bind(user_id).fetch_one(&state.db).await?;
        let broker_mutation_blockers = ambiguous_exposure;
        if broker_mutation_blockers > 0 {
            let blocker_detail = format!(
                "Full broker reconciliation found Rulenix-owned exposure={rulenix_owned_exposure}, manual/external exposure={manual_external_exposure}, ambiguous exposure={ambiguous_exposure}."
            );
            sqlx::query("INSERT INTO broker_reconciliation_blockers(user_id,status,external_active_orders,structurally_unknown_orders,active_conditional_rules,rulenix_owned_exposure,ambiguous_exposure,manual_external_exposure,detail,first_detected_at,last_checked_at,resolved_at) VALUES($1,'open',0,$2,0,$3,$2,$4,$5,NOW(),NOW(),NULL) ON CONFLICT(user_id) DO UPDATE SET status='open',external_active_orders=0,structurally_unknown_orders=EXCLUDED.structurally_unknown_orders,active_conditional_rules=0,rulenix_owned_exposure=EXCLUDED.rulenix_owned_exposure,ambiguous_exposure=EXCLUDED.ambiguous_exposure,manual_external_exposure=EXCLUDED.manual_external_exposure,detail=EXCLUDED.detail,last_checked_at=NOW(),resolved_at=NULL")
                .bind(user_id)
                .bind(ambiguous_exposure)
                .bind(rulenix_owned_exposure)
                .bind(manual_external_exposure)
                .bind(&blocker_detail)
                .execute(&state.db)
                .await?;
        } else {
            sqlx::query("INSERT INTO broker_reconciliation_blockers(user_id,status,external_active_orders,structurally_unknown_orders,active_conditional_rules,rulenix_owned_exposure,ambiguous_exposure,manual_external_exposure,detail,first_detected_at,last_checked_at,resolved_at) VALUES($1,'resolved',0,0,0,$2,0,$3,'Authoritative full broker reconciliation found no ambiguous broker mutations.',NOW(),NOW(),NOW()) ON CONFLICT(user_id) DO UPDATE SET status='resolved',external_active_orders=0,structurally_unknown_orders=0,active_conditional_rules=0,rulenix_owned_exposure=EXCLUDED.rulenix_owned_exposure,ambiguous_exposure=0,manual_external_exposure=EXCLUDED.manual_external_exposure,detail=EXCLUDED.detail,last_checked_at=NOW(),resolved_at=NOW()")
                .bind(user_id)
                .bind(rulenix_owned_exposure)
                .bind(manual_external_exposure)
                .execute(&state.db)
                .await?;
        }
        if unresolved_incidents > 0 || broker_mutation_blockers > 0 || ambiguous_local_orders > 0 {
            let detail = format!(
                "Full broker reconciliation is unsafe: incidents={unresolved_incidents}, ambiguous_exposure={ambiguous_exposure}, ambiguous_local_orders={ambiguous_local_orders}."
            );
            risk::set_reconciliation_health(state, user_id, false, &detail).await?;
            return Err(AppError::BadRequest(detail));
        }
        risk::set_reconciliation_health(
            state,
            user_id,
            true,
            &format!("Full Angel state reconciled: Rulenix-owned exposure={rulenix_owned_exposure}, manual/external exposure={manual_external_exposure}, ambiguous exposure=0."),
        )
        .await?;
        if !risk::reconciliation_ready(state, user_id).await? {
            return Err(AppError::BadRequest(
                "Broker reconciliation completed against a stale credential revision.".into(),
            ));
        }
    }
    Ok(())
}

async fn reconcile_live_user(state: &AppState, user_id: Uuid) -> AppResult<()> {
    reconcile_live_user_with_scope(state, user_id, false).await
}

async fn reconcile_live_user_readiness(state: &AppState, user_id: Uuid) -> AppResult<()> {
    reconcile_live_user_with_scope(state, user_id, true).await
}

type ProtectionRecoveryRow = (
    Uuid,
    Uuid,
    Uuid,
    String,
    String,
    String,
    String,
    i32,
    i32,
    i32,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    String,
    Option<DateTime<Utc>>,
    i32,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtectionRecoveryDecision {
    ConfirmProtected,
    WaitForReconciliation,
    SubmitStop,
    EmergencyClose,
}

fn protection_recovery_decision(
    protected_quantity: i64,
    required_quantity: i32,
    active_stop: bool,
    deadline_elapsed: bool,
    attempts: i32,
    max_attempts: i32,
) -> ProtectionRecoveryDecision {
    if protected_quantity >= i64::from(required_quantity.max(0)) {
        ProtectionRecoveryDecision::ConfirmProtected
    } else if deadline_elapsed || attempts >= max_attempts {
        ProtectionRecoveryDecision::EmergencyClose
    } else if active_stop {
        ProtectionRecoveryDecision::WaitForReconciliation
    } else {
        ProtectionRecoveryDecision::SubmitStop
    }
}

fn protection_session_key(strategy_key: &str, entry_session: &str) -> String {
    if strategy_key == SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY {
        supertrend_protection_session_key(entry_session)
    } else {
        entry_session.to_owned()
    }
}

fn emergency_close_session(trade_id: Uuid) -> String {
    format!("ec-{}", &trade_id.simple().to_string()[..16])
}

fn manual_close_session(trade_id: Uuid) -> String {
    format!("mc-{}", &trade_id.simple().to_string()[..16])
}

async fn terminal_retry_session(
    state: &AppState,
    trade_id: Uuid,
    role: &str,
    base_session: &str,
) -> AppResult<String> {
    let terminal_attempts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM strategy_orders
         WHERE trade_id=$1 AND role=$2 AND status IN ('failed','rejected','cancelled')",
    )
    .bind(trade_id)
    .bind(role)
    .fetch_one(&state.db)
    .await?;
    if terminal_attempts == 0 {
        Ok(session_with_suffix(base_session, ""))
    } else {
        Ok(session_with_suffix(
            base_session,
            &format!("a{}", terminal_attempts + 1),
        ))
    }
}

async fn protection_runner(
    state: &AppState,
    user_id: Uuid,
    strategy_key: &str,
    snapshot: &Snapshot,
) -> AppResult<Runner> {
    let instrument = if strategy_key == SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY {
        supertrend_snapshot_underlying(&snapshot.instrument).ok_or_else(|| {
            AppError::BadRequest("SuperTrend protection has no underlying instrument.".into())
        })?
    } else {
        snapshot.instrument.as_str()
    };
    // Risk-reducing exits remain available even after LIVE-entry permission or
    // strategy activation is removed.
    let mut runner: Runner = sqlx::query_as(
        "SELECT c.user_id,u.username,c.instrument,c.lots,c.run_day_session,c.run_evening_session,p.trading_mode
         FROM user_strategy_configs c
         JOIN users u ON u.id=c.user_id
         JOIN user_profiles p ON p.user_id=c.user_id
         WHERE c.user_id=$1 AND c.strategy_key=$2 AND c.instrument=$3",
    )
    .bind(user_id)
    .bind(strategy_key)
    .bind(instrument)
    .fetch_one(&state.db)
    .await?;
    runner.trading_mode = "live".into();
    Ok(runner)
}

async fn ensure_target_after_protection(
    state: &AppState,
    trade: &ProtectionRecoveryRow,
    snapshot: &Snapshot,
    entry_session: &str,
) -> AppResult<()> {
    let (
        trade_id,
        user_id,
        _,
        strategy_key,
        _,
        direction,
        _,
        quantity,
        remaining_lots,
        total_lots,
        target,
        _,
        _,
        _,
        _,
        _,
    ) = trade;
    let target = required_exit_level(*target, "target")?;
    let target_already_filled: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=$1 AND role='TARGET' AND processed_quantity>0)")
        .bind(trade_id).fetch_one(&state.db).await?;
    if target_already_filled {
        return Ok(());
    }
    let runner = protection_runner(state, *user_id, strategy_key, snapshot).await?;
    let (desired_lots, desired_quantity) = if strategy_key == STRATEGY_KEY {
        let lots = target_exit_lots(*total_lots).min(*remaining_lots).max(1);
        (
            lots,
            (lots * snapshot.lot_size.unwrap_or(1).max(1)).min(*quantity),
        )
    } else {
        ((*remaining_lots).max(1), (*quantity).max(1))
    };
    let active_target_quantity: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(GREATEST(quantity-processed_quantity,0)),0) FROM strategy_orders WHERE trade_id=$1 AND role='TARGET' AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')")
        .bind(trade_id).fetch_one(&state.db).await?;
    let target_quantity = (i64::from(desired_quantity) - active_target_quantity).max(0) as i32;
    if target_quantity == 0 {
        return Ok(());
    }
    let lot_size = snapshot.lot_size.unwrap_or(1).max(1);
    let lots = ((target_quantity + lot_size - 1) / lot_size)
        .min(desired_lots)
        .max(1);
    let target_session = format!(
        "{}:tpq:{}",
        protection_session_key(strategy_key, entry_session),
        desired_quantity
    );
    if let Err(error) = place_strategy_order(
        state,
        &runner,
        snapshot,
        &target_session,
        NewOrder {
            role: "TARGET",
            side: if direction == "BUY" { "SELL" } else { "BUY" },
            order_type: "LIMIT",
            lots,
            price: target,
            trigger: None,
            trade_id: Some(*trade_id),
            quantity: Some(target_quantity),
        },
    )
    .await
    {
        operational_alert_for(state,strategy_key,Some(*user_id),&snapshot.instrument,"target_submission_failed","error",&format!("Stop is confirmed, but target submission failed and will not displace protection: {error}")).await;
    }
    Ok(())
}

async fn begin_emergency_close(
    state: &AppState,
    trade: &ProtectionRecoveryRow,
    snapshot: &Snapshot,
) -> AppResult<()> {
    let (
        trade_id,
        user_id,
        _,
        strategy_key,
        instrument,
        direction,
        _,
        quantity,
        remaining_lots,
        _,
        _,
        _,
        _,
        _,
        _,
        _,
    ) = trade;
    let manual_close_status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM manual_trade_close_intents WHERE trade_id=$1 AND status<>'completed'",
    )
    .bind(trade_id)
    .fetch_optional(&state.db)
    .await?;
    let manual_close_requested = manual_close_status.is_some();
    if manual_close_status.as_deref() == Some("failed") {
        return Ok(());
    }
    sqlx::query("UPDATE trades SET safety_status=CASE WHEN $2 THEN 'CLOSING' ELSE 'EMERGENCY_CLOSING' END,updated_at=NOW() WHERE id=$1 AND status='open'")
        .bind(trade_id).bind(manual_close_requested).execute(&state.db).await?;
    if manual_close_requested {
        sqlx::query("UPDATE manual_trade_close_intents SET status='cancelling_protection',last_error='',updated_at=NOW() WHERE trade_id=$1 AND status IN ('requested','cancelling_protection','reconciliation_required')")
            .bind(trade_id).execute(&state.db).await?;
    }
    cancel_active_exits(state, *user_id, *trade_id).await?;
    let active_exit: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=$1 AND role IN ('TARGET','SL1','SL2') AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))")
        .bind(trade_id).fetch_one(&state.db).await?;
    if active_exit {
        return Ok(());
    }
    if manual_close_requested {
        let credentials = state.credentials.load(*user_id).await?;
        angel::order_book(
            state,
            *user_id,
            &credentials.api_key,
            &credentials.jwt_token,
        )
        .await?;
        let positions = angel::positions(
            state,
            *user_id,
            &credentials.api_key,
            &credentials.jwt_token,
        )
        .await?;
        let token = snapshot.contract_token.as_deref().unwrap_or("");
        let exchange = snapshot.exchange_segment.to_uppercase();
        let broker_quantity = parse_broker_positions(&positions)
            .into_iter()
            .find(|position| position.exchange == exchange && position.token == token)
            .map_or(0, |position| position.net_quantity);
        let expected_quantity = if direction == "BUY" {
            *quantity
        } else {
            -*quantity
        };
        let attributable_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM trades t
             JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
             WHERE t.user_id=$1 AND t.execution_mode='live' AND t.status='open'
               AND UPPER(s.exchange_segment)=$2 AND s.contract_token=$3",
        )
        .bind(user_id)
        .bind(&exchange)
        .bind(token)
        .fetch_one(&state.db)
        .await?;
        if attributable_rows != 1 || broker_quantity != expected_quantity {
            let detail = format!(
                "Manual close paused before submission: broker quantity {broker_quantity}, attributable local quantity {expected_quantity}, matching local trades {attributable_rows}."
            );
            sqlx::query("UPDATE manual_trade_close_intents SET status='reconciliation_required',last_error=$2,updated_at=NOW() WHERE trade_id=$1")
                .bind(trade_id).bind(&detail).execute(&state.db).await?;
            sqlx::query("UPDATE trades SET safety_status='RECONCILIATION_REQUIRED',broker_net_quantity=$2,last_position_reconciled_at=NOW(),last_protection_error=$3,updated_at=NOW() WHERE id=$1 AND status='open'")
                .bind(trade_id).bind(broker_quantity).bind(detail).execute(&state.db).await?;
            return Ok(());
        }
    }
    let already_closing: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=$1 AND role='EMERGENCY_CLOSE' AND status NOT IN ('failed','rejected','cancelled'))")
        .bind(trade_id).fetch_one(&state.db).await?;
    if already_closing {
        return Ok(());
    }
    let runner = protection_runner(state, *user_id, strategy_key, snapshot).await?;
    let price = if snapshot.strategy_key == SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY {
        option_execution_ltp(state, snapshot).await?
    } else {
        let tick: Option<f64> = sqlx::query_scalar("SELECT price FROM market_price_ticks WHERE exchange_segment=$1 AND contract_token=$2 AND received_at>NOW()-INTERVAL '60 seconds'")
            .bind(&snapshot.exchange_segment).bind(snapshot.contract_token.as_deref().unwrap_or(""))
            .fetch_optional(&state.db).await?;
        tick.ok_or_else(|| {
            AppError::BadRequest("Emergency close has no fresh market price.".into())
        })?
    };
    operational_alert_for(state,strategy_key,Some(*user_id),instrument,"emergency_close","critical",&format!("Stop protection could not be confirmed; submitting an idempotent MARKET close for trade {trade_id}.")).await;
    let base_session = if manual_close_requested {
        manual_close_session(*trade_id)
    } else {
        emergency_close_session(*trade_id)
    };
    let session =
        terminal_retry_session(state, *trade_id, "EMERGENCY_CLOSE", &base_session).await?;
    let result = place_strategy_order(
        state,
        &runner,
        snapshot,
        &session,
        NewOrder {
            role: "EMERGENCY_CLOSE",
            side: if direction == "BUY" { "SELL" } else { "BUY" },
            order_type: "MARKET",
            lots: (*remaining_lots).max(1),
            price,
            trigger: None,
            trade_id: Some(*trade_id),
            quantity: Some((*quantity).max(1)),
        },
    )
    .await;
    if manual_close_requested {
        let close_order: Option<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id,status,broker_status FROM strategy_orders WHERE trade_id=$1 AND role='EMERGENCY_CLOSE' AND session_key LIKE 'mc-%' ORDER BY created_at DESC LIMIT 1",
        )
        .bind(trade_id)
        .fetch_optional(&state.db)
        .await?;
        if let Some((order_id, status, diagnostic)) = close_order {
            let intent_status = match status.as_str() {
                "submitted" | "processing" | "cancelling" => "submitted",
                "partially_filled" => "partially_filled",
                "ambiguous" | "submitting" => "ambiguous",
                "filled" => "completed",
                "failed" | "rejected" | "cancelled" => "failed",
                _ => "requested",
            };
            sqlx::query("UPDATE manual_trade_close_intents SET status=$2,strategy_order_id=$3,last_error=CASE WHEN $2 IN ('failed','ambiguous') THEN $4 ELSE '' END,completed_at=CASE WHEN $2='completed' THEN NOW() ELSE completed_at END,updated_at=NOW() WHERE trade_id=$1")
                .bind(trade_id).bind(intent_status).bind(order_id).bind(diagnostic).execute(&state.db).await?;
        } else if let Err(error) = &result {
            sqlx::query("UPDATE manual_trade_close_intents SET status='failed',last_error=$2,updated_at=NOW() WHERE trade_id=$1 AND status NOT IN ('submitted','ambiguous','completed')")
                .bind(trade_id).bind(error.to_string()).execute(&state.db).await?;
        }
    }
    result
}

async fn recover_unprotected_trades(state: &AppState) -> AppResult<()> {
    let trades: Vec<ProtectionRecoveryRow> = sqlx::query_as("SELECT id,user_id,strategy_snapshot_id,strategy_key,instrument_label,direction,execution_mode,quantity,remaining_lots,total_lots,target_price::float8,sl1_price::float8,sl2_price::float8,safety_status,protection_deadline_at,protection_attempts FROM trades WHERE execution_mode='live' AND status='open' AND strategy_snapshot_id IS NOT NULL AND safety_status IN ('PROTECTION_REQUIRED','PROTECTION_SUBMITTING','PROTECTION_UNCERTAIN','PROTECTION_FAILED','CLOSING','EMERGENCY_CLOSING') ORDER BY entry_datetime LIMIT 100")
        .fetch_all(&state.db).await?;
    for trade in trades {
        let (
            trade_id,
            user_id,
            snapshot_id,
            strategy_key,
            instrument,
            direction,
            _,
            quantity,
            remaining_lots,
            _,
            _,
            sl1,
            sl2,
            safety_status,
            deadline,
            attempts,
        ) = &trade;
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await?;
        if safety_status == "RECONCILIATION_REQUIRED" {
            continue;
        }
        if matches!(safety_status.as_str(), "CLOSING" | "EMERGENCY_CLOSING") {
            if let Err(error) = begin_emergency_close(state, &trade, &snapshot).await {
                operational_alert_for(
                    state,
                    strategy_key,
                    Some(*user_id),
                    instrument,
                    "emergency_close_failed",
                    "critical",
                    &error.to_string(),
                )
                .await;
            }
            continue;
        }
        let protected_quantity: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(GREATEST(quantity-processed_quantity,0)),0) FROM strategy_orders WHERE trade_id=$1 AND role IN ('SL1','SL2') AND status IN ('submitted','partially_filled') AND broker_order_id<>'' AND last_reconciled_at IS NOT NULL")
            .bind(trade_id).fetch_one(&state.db).await?;
        let deadline_elapsed = deadline.is_some_and(|value| value <= Utc::now());
        let active_stop: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=$1 AND role IN ('SL1','SL2') AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))")
            .bind(trade_id).fetch_one(&state.db).await?;
        match protection_recovery_decision(
            protected_quantity,
            *quantity,
            active_stop,
            deadline_elapsed,
            *attempts,
            state.config.protection_max_attempts,
        ) {
            ProtectionRecoveryDecision::ConfirmProtected => {
                sqlx::query("UPDATE trades SET safety_status='PROTECTED',last_protection_error='',updated_at=NOW() WHERE id=$1 AND status='open'")
                    .bind(trade_id).execute(&state.db).await?;
                let entry_session:String=sqlx::query_scalar("SELECT session_key FROM strategy_orders WHERE trade_id=$1 AND role IN ('BUY_ENTRY','SELL_ENTRY') ORDER BY created_at LIMIT 1")
                    .bind(trade_id).fetch_one(&state.db).await?;
                ensure_target_after_protection(state, &trade, &snapshot, &entry_session).await?;
                continue;
            }
            ProtectionRecoveryDecision::EmergencyClose => {
                if let Err(error) = begin_emergency_close(state, &trade, &snapshot).await {
                    operational_alert_for(
                        state,
                        strategy_key,
                        Some(*user_id),
                        instrument,
                        "emergency_close_failed",
                        "critical",
                        &error.to_string(),
                    )
                    .await;
                }
                continue;
            }
            ProtectionRecoveryDecision::WaitForReconciliation => continue,
            ProtectionRecoveryDecision::SubmitStop => {}
        }
        let target_done:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=$1 AND role='TARGET' AND processed_quantity>0)")
            .bind(trade_id).fetch_one(&state.db).await?;
        let (role, stop) = if target_done {
            ("SL2", *sl2)
        } else {
            ("SL1", *sl1)
        };
        let stop = required_exit_level(stop, "recovery stop loss")?;
        let entry_session:String=sqlx::query_scalar("SELECT session_key FROM strategy_orders WHERE trade_id=$1 AND role IN ('BUY_ENTRY','SELL_ENTRY') ORDER BY created_at LIMIT 1")
            .bind(trade_id).fetch_one(&state.db).await?;
        sqlx::query("UPDATE trades SET safety_status='PROTECTION_SUBMITTING',protection_attempts=protection_attempts+1,last_protection_error='',updated_at=NOW() WHERE id=$1")
            .bind(trade_id).execute(&state.db).await?;
        let runner = protection_runner(state, *user_id, strategy_key, &snapshot).await?;
        let stop_session = if *attempts > 0 {
            format!(
                "{}:pr{}",
                protection_session_key(strategy_key, &entry_session),
                attempts + 1
            )
        } else {
            protection_session_key(strategy_key, &entry_session)
        };
        if let Err(error) = place_strategy_order(
            state,
            &runner,
            &snapshot,
            &stop_session,
            NewOrder {
                role,
                side: if direction == "BUY" { "SELL" } else { "BUY" },
                order_type: "STOPLOSS_MARKET",
                lots: (*remaining_lots).max(1),
                price: stop,
                trigger: Some(stop),
                trade_id: Some(*trade_id),
                quantity: Some((*quantity).max(1)),
            },
        )
        .await
        {
            mark_protection_submission_failure(state, *trade_id, &error.to_string()).await?;
            operational_alert_for(
                state,
                strategy_key,
                Some(*user_id),
                instrument,
                "protection_submission_failure",
                "critical",
                &error.to_string(),
            )
            .await;
        }
    }
    Ok(())
}

async fn close_demo_trade(
    state: &AppState,
    user: &AuthUser,
    trade_id: Uuid,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
) -> AppResult<Json<Value>> {
    type DemoCloseRow = (
        String,
        String,
        String,
        i32,
        i32,
        i32,
        f64,
        f64,
        String,
        String,
        String,
        String,
        String,
        Option<i32>,
    );

    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtext('rulenix:risk:global'))")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::uuid::text,0))")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    let trade: Option<DemoCloseRow> = sqlx::query_as(
        "SELECT t.status,t.execution_mode,t.direction,t.quantity,t.total_lots,t.remaining_lots,
                t.entry_price::float8,t.pnl::float8,t.strategy_key,t.instrument_label,
                COALESCE(t.contract_symbol,''),UPPER(s.exchange_segment),s.contract_token,s.lot_size
         FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
         WHERE t.id=$1 AND t.user_id=$2 FOR UPDATE OF t",
    )
    .bind(trade_id)
    .bind(user.id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((
        status,
        mode,
        direction,
        quantity,
        total_lots,
        _,
        entry_price,
        stored_pnl,
        strategy_key,
        instrument,
        symbol,
        exchange,
        token,
        lot_size,
    )) = trade
    else {
        return Err(AppError::NotFound("Trade was not found.".into()));
    };
    if status == "closed" {
        tx.commit().await?;
        return Ok(Json(json!({
            "trade_id":trade_id,
            "status":"completed",
            "execution_mode":"demo",
            "message":"Trade is already closed."
        })));
    }
    if mode != "demo" || status != "open" || quantity <= 0 {
        return Err(AppError::BadRequest(
            "Only an eligible running DEMO trade can be closed locally.".into(),
        ));
    }

    let active_orders: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id,status FROM strategy_orders
         WHERE trade_id=$1 AND execution_mode='demo'
           AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
         FOR UPDATE",
    )
    .bind(trade_id)
    .fetch_all(&mut *tx)
    .await?;
    if active_orders
        .iter()
        .any(|(_, status)| status == "processing")
    {
        return Err(AppError::BadRequest(
            "A DEMO exit fill is already being processed; refresh the trade before closing it."
                .into(),
        ));
    }

    let max_price_age_seconds: i32 = sqlx::query_scalar(
        "SELECT COALESCE(u.max_price_age_seconds,g.max_price_age_seconds)::int4
         FROM risk_limits g LEFT JOIN risk_limits u ON u.user_id=$1
         WHERE g.user_id IS NULL",
    )
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    let exit_price: Option<f64> = sqlx::query_scalar(
        "SELECT price::float8 FROM market_price_ticks
         WHERE exchange_segment=$1 AND contract_token=$2
           AND received_at>NOW()-($3::text || ' seconds')::interval
           AND price>0
         ORDER BY received_at DESC LIMIT 1",
    )
    .bind(&exchange)
    .bind(&token)
    .bind(max_price_age_seconds.max(1))
    .fetch_optional(&mut *tx)
    .await?;
    let exit_price = exit_price
        .filter(|price| price.is_finite() && *price > 0.0)
        .ok_or_else(|| {
            AppError::BadRequest(
                "DEMO close stopped because no fresh valid market price is available.".into(),
            )
        })?;
    let realized = trade_pnl(
        &direction,
        entry_price,
        exit_price,
        runtime_pnl_units(&instrument, quantity, lot_size),
    );
    let pnl = stored_pnl + realized;
    let reporting_quantity = if strategy_key == STRATEGY_KEY {
        total_lots.saturating_mul(lot_size.unwrap_or(1).max(1))
    } else {
        quantity
    };

    sqlx::query(
        "UPDATE strategy_orders
         SET status='cancelled',broker_status='DEMO order terminalized by user close',
             state_version=state_version+1,updated_at=NOW()
         WHERE trade_id=$1 AND execution_mode='demo'
           AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','cancelling')",
    )
    .bind(trade_id)
    .execute(&mut *tx)
    .await?;
    let changed = sqlx::query(
        "UPDATE trades
         SET status='closed',safety_status='CLOSED',quantity=$3,remaining_lots=0,
             exit_price=($4::float8)::numeric,last_price=($4::float8)::numeric,
             pnl=($5::float8)::numeric,exit_datetime=NOW(),exit_reason='MANUAL_RULENIX_CLOSE',
             notes=CONCAT(notes,'; running DEMO trade closed locally by user'),updated_at=NOW()
         WHERE id=$1 AND user_id=$2 AND execution_mode='demo' AND status='open'",
    )
    .bind(trade_id)
    .bind(user.id)
    .bind(reporting_quantity)
    .bind(exit_price)
    .bind(pnl)
    .execute(&mut *tx)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(AppError::BadRequest(
            "DEMO trade state changed while the close was being processed; refresh and try again."
                .into(),
        ));
    }
    tx.commit().await?;

    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(
        state,
        crate::audit::AuditEvent {
            context: request_context.as_ref(),
            headers: Some(&headers),
            event_type: "manual_demo_trade_closed",
            actor_user_id: Some(user.id),
            target_user_id: Some(user.id),
            summary: "User closed an attributable running DEMO trade locally",
            metadata: json!({"trade_id":trade_id,"exit_price":exit_price,"quantity":quantity}),
        },
    )
    .await
    {
        tracing::warn!(%error,%trade_id,"could not write DEMO close audit event");
    }
    emit_for(
        state,
        &strategy_key,
        Some(user.id),
        &instrument,
        "demo_trade_manually_closed",
        json!({"trade_id":trade_id,"contract_symbol":symbol,"exit_price":exit_price,"pnl":pnl}),
    )
    .await;
    Ok(Json(json!({
        "trade_id":trade_id,
        "status":"completed",
        "execution_mode":"demo",
        "exit_price":exit_price,
        "pnl":pnl,
        "message":"DEMO trade closed locally at the latest authoritative simulated price."
    })))
}

pub async fn manual_close_trade(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(trade_id): Path<Uuid>,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
) -> AppResult<Json<Value>> {
    type ManualCloseLookup = (String, String, String, i32, String, String, String);
    let lookup: Option<ManualCloseLookup> = sqlx::query_as(
        "SELECT t.status,t.execution_mode,t.direction,t.quantity,
                UPPER(s.exchange_segment),s.contract_token,COALESCE(t.contract_symbol,'')
         FROM trades t
         JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
         WHERE t.id=$1 AND t.user_id=$2",
    )
    .bind(trade_id)
    .bind(user.id)
    .fetch_optional(&state.db)
    .await?;
    let Some((status, mode, _, _, _, _, _)) = lookup else {
        return Err(AppError::NotFound("Trade was not found.".into()));
    };
    if status == "closed" {
        return Ok(Json(json!({
            "trade_id":trade_id,
            "status":"completed",
            "message":"Trade is already closed."
        })));
    }
    if mode == "demo" {
        return close_demo_trade(&state, &user, trade_id, headers, context).await;
    }
    if mode != "live" {
        return Err(AppError::BadRequest(
            "Close Trade is available only for running DEMO or LIVE trades.".into(),
        ));
    }

    // Reconcile known fills/cancellations first, then independently require
    // successful fresh order and position reads for this user action.
    reconcile_live_user(&state, user.id).await?;
    let credentials = state.credentials.load(user.id).await?;
    let order_book = angel::order_book(
        &state,
        user.id,
        &credentials.api_key,
        &credentials.jwt_token,
    )
    .await?;
    broker_book_items(&order_book, "order-book")?;
    let positions = angel::positions(
        &state,
        user.id,
        &credentials.api_key,
        &credentials.jwt_token,
    )
    .await?;
    let authoritative_positions = parse_authoritative_broker_positions(&positions)?;

    let refreshed: Option<ManualCloseLookup> = sqlx::query_as(
        "SELECT t.status,t.execution_mode,t.direction,t.quantity,
                UPPER(s.exchange_segment),s.contract_token,COALESCE(t.contract_symbol,'')
         FROM trades t
         JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
         WHERE t.id=$1 AND t.user_id=$2",
    )
    .bind(trade_id)
    .bind(user.id)
    .fetch_optional(&state.db)
    .await?;
    let Some((status, _, direction, quantity, exchange, token, symbol)) = refreshed else {
        return Err(AppError::NotFound("Trade was not found.".into()));
    };
    if status == "closed" {
        return Ok(Json(json!({
            "trade_id":trade_id,
            "status":"completed",
            "message":"Broker reconciliation confirmed that the trade is already closed."
        })));
    }
    let attributable_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM trades t
         JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
         WHERE t.user_id=$1 AND t.execution_mode='live' AND t.status='open'
           AND UPPER(s.exchange_segment)=$2 AND s.contract_token=$3",
    )
    .bind(user.id)
    .bind(&exchange)
    .bind(&token)
    .fetch_one(&state.db)
    .await?;
    if attributable_rows != 1 {
        return Err(AppError::BadRequest(format!(
            "Close Trade requires exactly one attributable open Rulenix trade for {exchange}/{token}; found {attributable_rows}."
        )));
    }
    let broker = authoritative_positions
        .into_iter()
        .find(|position| position.exchange == exchange && position.token == token);
    let expected_signed = if direction == "BUY" {
        quantity
    } else {
        -quantity
    };
    let broker_quantity = broker.as_ref().map_or(0, |position| position.net_quantity);
    let symbol_matches = broker.as_ref().is_none_or(|position| {
        symbol.is_empty()
            || position.symbol.is_empty()
            || position.symbol.eq_ignore_ascii_case(&symbol)
    });
    if broker_quantity != expected_signed || !symbol_matches {
        sqlx::query("UPDATE trades SET safety_status='RECONCILIATION_REQUIRED',broker_net_quantity=$2,last_position_reconciled_at=NOW(),updated_at=NOW() WHERE id=$1 AND status='open'")
            .bind(trade_id).bind(broker_quantity).execute(&state.db).await?;
        return Err(AppError::BadRequest(format!(
            "Close Trade stopped because fresh Angel exposure ({broker_quantity}) does not exactly match the attributable Rulenix exposure ({expected_signed}). Reconciliation is required."
        )));
    }
    let close_side = if direction == "BUY" { "SELL" } else { "BUY" };
    let mut tx = state.db.begin().await?;
    let lock_key = format!("manual-close:{trade_id}");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(lock_key)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO manual_trade_close_intents(trade_id,user_id,requested_quantity,close_side)
         VALUES($1,$2,$3,$4)
         ON CONFLICT(trade_id) DO UPDATE
         SET status=CASE
               WHEN manual_trade_close_intents.status IN ('failed','partially_filled','reconciliation_required')
                 THEN 'requested'
               ELSE manual_trade_close_intents.status
             END,
             requested_quantity=CASE
               WHEN manual_trade_close_intents.status IN ('failed','partially_filled','reconciliation_required')
                 THEN EXCLUDED.requested_quantity
               ELSE manual_trade_close_intents.requested_quantity
             END,
             close_side=EXCLUDED.close_side,
             last_error='',updated_at=NOW()",
    )
    .bind(trade_id)
    .bind(user.id)
    .bind(quantity)
    .bind(close_side)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE trades SET safety_status='CLOSING',updated_at=NOW() WHERE id=$1 AND user_id=$2 AND status='open'")
        .bind(trade_id).bind(user.id).execute(&mut *tx).await?;
    tx.commit().await?;
    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: request_context.as_ref(),
            headers: Some(&headers),
            event_type: "manual_live_trade_close_requested",
            actor_user_id: Some(user.id),
            target_user_id: Some(user.id),
            summary: "User requested an attributable risk-reducing LIVE trade close",
            metadata: json!({"trade_id":trade_id,"close_side":close_side,"quantity":quantity}),
        },
    )
    .await
    {
        tracing::warn!(%error,%trade_id,"could not write manual close audit event");
    }

    let trade: ProtectionRecoveryRow = sqlx::query_as("SELECT id,user_id,strategy_snapshot_id,strategy_key,instrument_label,direction,execution_mode,quantity,remaining_lots,total_lots,target_price::float8,sl1_price::float8,sl2_price::float8,safety_status,protection_deadline_at,protection_attempts FROM trades WHERE id=$1 AND user_id=$2 AND status='open'")
        .bind(trade_id).bind(user.id).fetch_one(&state.db).await?;
    let query = format!("{} WHERE id=$1", snapshot_select());
    let snapshot: Snapshot = sqlx::query_as(&query)
        .bind(trade.2)
        .fetch_one(&state.db)
        .await?;
    let submission_error = begin_emergency_close(&state, &trade, &snapshot)
        .await
        .err()
        .map(|error| error.to_string());
    let intent: (String, String) = sqlx::query_as(
        "SELECT status,last_error FROM manual_trade_close_intents WHERE trade_id=$1",
    )
    .bind(trade_id)
    .fetch_one(&state.db)
    .await?;
    if let Some(error) = submission_error {
        sqlx::query("UPDATE manual_trade_close_intents SET last_error=$2,updated_at=NOW() WHERE trade_id=$1 AND status<>'completed'")
            .bind(trade_id).bind(&error).execute(&state.db).await?;
        return Err(AppError::BadRequest(error));
    }
    Ok(Json(json!({
        "trade_id":trade_id,
        "status":intent.0,
        "message":if intent.0=="submitted" {
            "The broker close was submitted and is awaiting authoritative fill reconciliation."
        } else {
            "The close request is durable and is waiting for protective-order cancellation reconciliation."
        },
        "detail":intent.1
    })))
}

pub(crate) async fn cancel_active_exits(
    state: &AppState,
    user_id: Uuid,
    trade_id: Uuid,
) -> AppResult<()> {
    let orders:Vec<(Uuid,String,String,String)>=sqlx::query_as("SELECT id,broker_order_id,execution_mode,order_type FROM strategy_orders WHERE trade_id=$1 AND role IN ('TARGET','SL1','SL2') AND status IN ('submitted','partially_filled')").bind(trade_id).fetch_all(&state.db).await?;
    cancel_exit_orders(state, user_id, orders).await
}

async fn cancel_exit_orders(
    state: &AppState,
    user_id: Uuid,
    orders: Vec<(Uuid, String, String, String)>,
) -> AppResult<()> {
    let credentials = if orders
        .iter()
        .any(|(_, _, execution_mode, _)| execution_mode == "live")
    {
        Some(state.credentials.load(user_id).await?)
    } else {
        None
    };
    for (id, broker_id, execution_mode, order_type) in orders {
        if execution_mode == "live" {
            let credentials = credentials
                .as_ref()
                .filter(|credentials| {
                    !credentials.api_key.is_empty() && !credentials.jwt_token.is_empty()
                })
                .ok_or_else(|| {
                    AppError::Unauthorized(
                        "Cannot cancel the live protective order until Angel One is reconnected."
                            .into(),
                    )
                })?;
            if broker_id.is_empty() {
                let message =
                    "Cannot cancel a submitted live protective order without its broker order ID."
                        .to_string();
                sqlx::query("UPDATE strategy_orders SET broker_status=$2,updated_at=NOW() WHERE id=$1 AND status IN ('submitted','partially_filled')")
                    .bind(id)
                    .bind(&message)
                    .execute(&state.db)
                    .await?;
                operational_alert(
                    state,
                    Some(user_id),
                    "",
                    "protective_cancel_missing_broker_id",
                    "error",
                    &message,
                )
                .await;
                return Err(AppError::BadRequest(message));
            }
            let variety = if order_type.starts_with("STOPLOSS") {
                "STOPLOSS"
            } else {
                "NORMAL"
            };
            if let Err(error) = angel::cancel_order(
                state,
                user_id,
                &credentials.api_key,
                &credentials.jwt_token,
                &broker_id,
                variety,
            )
            .await
            {
                let message = format!("Protective order cancellation was not confirmed: {error}");
                sqlx::query("UPDATE strategy_orders SET broker_status=$2,updated_at=NOW() WHERE id=$1 AND status IN ('submitted','partially_filled')")
                    .bind(id)
                    .bind(&message)
                    .execute(&state.db)
                    .await?;
                operational_alert(
                    state,
                    Some(user_id),
                    "",
                    "protective_cancel_failed",
                    "error",
                    &message,
                )
                .await;
                return Err(AppError::BadRequest(message));
            }
            sqlx::query("UPDATE strategy_orders SET status='cancelling',broker_status='Protective order cancellation requested; awaiting broker reconciliation.',state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status IN ('submitted','partially_filled')")
                .bind(id)
                .execute(&state.db)
                .await?;
            continue;
        }
        sqlx::query("UPDATE strategy_orders SET status='cancelled',updated_at=NOW() WHERE id=$1 AND status IN ('submitted','partially_filled')").bind(id).execute(&state.db).await?;
    }
    Ok(())
}

fn target_exit_lots(lots: i32) -> i32 {
    if lots <= 1 {
        lots.max(0)
    } else {
        (lots + 1) / 2
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sl2ReversalPlan {
    direction: &'static str,
    entry_role: &'static str,
    entry_side: &'static str,
    lots: i32,
}

fn sl2_reversal_plan(source_direction: &str, original_lots: i32) -> Option<Sl2ReversalPlan> {
    let (direction, entry_role, entry_side) = match source_direction {
        "BUY" => ("SELL", "SELL_ENTRY", "SELL"),
        "SELL" => ("BUY", "BUY_ENTRY", "BUY"),
        _ => return None,
    };
    (original_lots > 0).then_some(Sl2ReversalPlan {
        direction,
        entry_role,
        entry_side,
        lots: original_lots,
    })
}

fn sl2_reversal_session(source_trade_id: Uuid) -> String {
    format!("r-{}", &source_trade_id.simple().to_string()[..30])
}

#[derive(Debug, Clone, FromRow)]
struct Sl2ReversalIntent {
    source_trade_id: Uuid,
    user_id: Uuid,
    snapshot_id: Uuid,
    instrument: String,
    source_direction: String,
    reversal_direction: String,
    lots: i32,
    entry_price: f64,
    order_session_key: String,
    attempts: i32,
    created_at: DateTime<Utc>,
}

enum Sl2ReversalOutcome {
    Waiting(String),
    Submitted,
    Completed,
    Cancelled(String),
}

fn trade_pnl(direction: &str, entry: f64, exit: f64, units: f64) -> f64 {
    let movement = if direction == "BUY" {
        exit - entry
    } else {
        entry - exit
    };
    movement * units
}

fn runtime_pnl_units(instrument: &str, quantity: i32, lot_size: Option<i32>) -> f64 {
    futures_pnl_units(instrument, quantity, lot_size)
}

fn supertrend_protection_session_key(entry_session_key: &str) -> String {
    format!("{}:p", entry_session_key)
}

fn session_with_suffix(session: &str, suffix: &str) -> String {
    let suffix = suffix.trim_matches(':');
    if suffix.is_empty() {
        return session.chars().take(32).collect();
    }
    let max_base = 32_usize.saturating_sub(suffix.len() + 1);
    let base: String = session.chars().take(max_base).collect();
    format!("{base}:{suffix}")
}

fn required_exit_level(value: Option<f64>, label: &str) -> AppResult<f64> {
    value
        .filter(|level| level.is_finite() && *level > 0.0)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "Futures Breakout snapshot has no valid {label}; the position was not opened."
            ))
        })
}

fn snapshot_exit_levels(
    snapshot: &Snapshot,
    direction: &str,
    entry_price: f64,
    rebase_to_entry: bool,
) -> AppResult<FuturesExitLevels> {
    if rebase_to_entry {
        let hh2 = required_exit_level(snapshot.hh2, "HH2")?;
        let ll2 = required_exit_level(snapshot.ll2, "LL2")?;
        let hh4 = required_exit_level(snapshot.hh4, "HH4")?;
        let ll4 = required_exit_level(snapshot.ll4, "LL4")?;
        return futures_exit_levels_for_entry(direction, entry_price, hh2, ll2, hh4, ll4)
            .ok_or_else(|| {
                AppError::BadRequest(
                    "Futures Breakout could not calculate valid reversal exit levels.".into(),
                )
            });
    }
    let (target, sl1, sl2) = match direction {
        "BUY" => (snapshot.buy_target, snapshot.buy_sl1, snapshot.buy_sl2),
        "SELL" => (snapshot.sell_target, snapshot.sell_sl1, snapshot.sell_sl2),
        _ => {
            return Err(AppError::BadRequest(
                "Futures Breakout trade direction is invalid.".into(),
            ));
        }
    };
    Ok(FuturesExitLevels {
        target: required_exit_level(target, "target")?,
        sl1: required_exit_level(sl1, "initial stop loss")?,
        sl2: required_exit_level(sl2, "continuation stop loss")?,
    })
}

fn snapshot_order_exit_levels(
    snapshot: &Snapshot,
    direction: &str,
    entry_price: f64,
    _reversal: bool,
) -> AppResult<FuturesExitLevels> {
    // A stop entry can be filled away from the planned trigger, especially in
    // demo mode where the fill uses the triggering tick LTP. TP1 must stay
    // anchored to the actual fill price, not to the planned breakout level;
    // otherwise a favorable gap can make TP1 only a few ticks away.
    snapshot_exit_levels(snapshot, direction, entry_price, true)
}

async fn append_user_log(state: &AppState, user_id: Uuid, message: &str) {
    let username: Result<Option<String>, sqlx::Error> =
        sqlx::query_scalar("SELECT username FROM users WHERE id=$1")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await;
    if let Ok(Some(username)) = username {
        crate::logs::append(&username, message).await;
    }
}

fn contract_log_label(instrument: &str, contract_symbol: Option<&str>) -> String {
    let symbol = contract_symbol.unwrap_or("").trim();
    if symbol.is_empty() || symbol.eq_ignore_ascii_case(instrument) {
        instrument.to_string()
    } else {
        format!("{instrument} ({symbol})")
    }
}

async fn clear_entry_orders_for_sl2_reversal(
    state: &AppState,
    intent: &Sl2ReversalIntent,
) -> AppResult<bool> {
    let orders: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
        "SELECT o.id,o.broker_order_id,o.execution_mode,o.order_type,o.status
         FROM strategy_orders o
         JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
         WHERE o.user_id=$1
           AND s.strategy_key=$2
           AND s.instrument=$3
           AND o.session_key<>$4
           AND o.role IN ('BUY_ENTRY','SELL_ENTRY')
           AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
         ORDER BY o.created_at",
    )
    .bind(intent.user_id)
    .bind(STRATEGY_KEY)
    .bind(&intent.instrument)
    .bind(&intent.order_session_key)
    .fetch_all(&state.db)
    .await?;

    for (id, broker_id, mode, order_type, status) in orders {
        if mode == "demo" || (status == "pending" && broker_id.is_empty()) {
            sqlx::query(
                "UPDATE strategy_orders
                 SET status='cancelled',
                     broker_status='Cancelled before full-lot SL2 reversal',
                     state_version=state_version+1,
                     updated_at=NOW()
                 WHERE id=$1 AND status IN ('pending','submitted','partially_filled')",
            )
            .bind(id)
            .execute(&state.db)
            .await?;
            continue;
        }
        if !matches!(status.as_str(), "submitted" | "partially_filled") {
            continue;
        }
        if broker_id.is_empty() {
            continue;
        }
        let credentials = state.credentials.load(intent.user_id).await?;
        angel::cancel_order(
            state,
            intent.user_id,
            &credentials.api_key,
            &credentials.jwt_token,
            &broker_id,
            if order_type.starts_with("STOPLOSS") {
                "STOPLOSS"
            } else {
                "NORMAL"
            },
        )
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
        sqlx::query(
            "UPDATE strategy_orders
             SET status='cancelling',
                 broker_status='SL2 reversal cancellation requested; awaiting broker reconciliation.',
                 state_version=state_version+1,
                 updated_at=NOW()
             WHERE id=$1 AND status IN ('submitted','partially_filled')",
        )
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1
            FROM strategy_orders o
            JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
            WHERE o.user_id=$1
              AND s.strategy_key=$2
              AND s.instrument=$3
              AND o.session_key<>$4
              AND o.role IN ('BUY_ENTRY','SELL_ENTRY')
              AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
        )",
    )
    .bind(intent.user_id)
    .bind(STRATEGY_KEY)
    .bind(&intent.instrument)
    .bind(&intent.order_session_key)
    .fetch_one(&state.db)
    .await?;
    Ok(!active)
}

async fn cancel_active_breakout_entry_orders(
    state: &AppState,
    user_id: Uuid,
    instrument: &str,
    exclude_order_id: Uuid,
    reason: &str,
) -> AppResult<()> {
    let orders: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
        "SELECT o.id,o.broker_order_id,o.execution_mode,o.order_type,o.status
         FROM strategy_orders o
         JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
         WHERE o.user_id=$1
           AND s.strategy_key=$2
           AND s.instrument=$3
           AND o.id<>$4
           AND o.role IN ('BUY_ENTRY','SELL_ENTRY')
           AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
         ORDER BY o.created_at",
    )
    .bind(user_id)
    .bind(STRATEGY_KEY)
    .bind(instrument)
    .bind(exclude_order_id)
    .fetch_all(&state.db)
    .await?;

    if orders.is_empty() {
        return Ok(());
    }

    let credentials = if orders.iter().any(|(_, _, execution_mode, _, status)| {
        execution_mode == "live" && matches!(status.as_str(), "submitted" | "partially_filled")
    }) {
        Some(state.credentials.load(user_id).await?)
    } else {
        None
    };

    for (id, broker_id, execution_mode, order_type, status) in orders {
        if execution_mode == "demo" || (status == "pending" && broker_id.is_empty()) {
            sqlx::query(
                "UPDATE strategy_orders
                 SET status='cancelled',
                     broker_status=$2,
                     state_version=state_version+1,
                     updated_at=NOW()
                 WHERE id=$1
                   AND status IN ('pending','submitted','partially_filled','submitting','ambiguous','processing','cancelling')",
            )
            .bind(id)
            .bind(reason)
            .execute(&state.db)
            .await?;
            continue;
        }
        if !matches!(status.as_str(), "submitted" | "partially_filled") || broker_id.is_empty() {
            continue;
        }
        if execution_mode == "live" {
            let Some(credentials) = credentials.as_ref() else {
                continue;
            };
            angel::cancel_order(
                state,
                user_id,
                &credentials.api_key,
                &credentials.jwt_token,
                &broker_id,
                if order_type.starts_with("STOPLOSS") {
                    "STOPLOSS"
                } else {
                    "NORMAL"
                },
            )
            .await
            .map_err(|error| AppError::BadRequest(error.to_string()))?;
            sqlx::query(
                "UPDATE strategy_orders
                 SET status='cancelling',
                     broker_status=$2,
                     state_version=state_version+1,
                     updated_at=NOW()
                 WHERE id=$1
                   AND status IN ('submitted','partially_filled')",
            )
            .bind(id)
            .bind(reason)
            .execute(&state.db)
            .await?;
        }
    }
    Ok(())
}

async fn attempt_claimed_sl2_reversal(
    state: &AppState,
    intent: &Sl2ReversalIntent,
) -> AppResult<Sl2ReversalOutcome> {
    let Some(plan) = sl2_reversal_plan(&intent.source_direction, intent.lots) else {
        return Ok(Sl2ReversalOutcome::Cancelled(
            "The source trade has no valid SL2 reversal direction or lot size.".into(),
        ));
    };
    if plan.direction != intent.reversal_direction {
        return Ok(Sl2ReversalOutcome::Cancelled(
            "The stored SL2 reversal direction does not match the source trade.".into(),
        ));
    }
    let source_confirmed_flat: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM trades t
            JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
            WHERE t.id=$1 AND t.status='closed' AND t.exit_reason='SL2'
              AND (
                  t.execution_mode='demo'
                  OR (
                      t.execution_mode='live'
                      AND t.broker_net_quantity=0
                      AND t.last_position_reconciled_at IS NOT NULL
                      AND t.last_position_reconciled_at>=t.exit_datetime
                      AND NOT EXISTS(
                          SELECT 1 FROM broker_position_incidents i
                          WHERE i.user_id=t.user_id AND i.exchange_segment=s.exchange_segment
                            AND i.contract_token=s.contract_token AND i.status IN ('open','operator_required')
                      )
                  )
              )
        )",
    )
    .bind(intent.source_trade_id)
    .fetch_one(&state.db)
    .await?;
    if !source_confirmed_flat {
        return Ok(Sl2ReversalOutcome::Waiting(
            "Waiting for broker net-position reconciliation to confirm the source SL2 position is flat.".into(),
        ));
    }
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1
            FROM user_strategy_configs c
            JOIN user_strategy_activations a
              ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key
            JOIN users u ON u.id=c.user_id
            WHERE c.user_id=$1
              AND c.strategy_key=$2
              AND c.instrument=$3
              AND c.enabled=TRUE
              AND a.is_active=TRUE
              AND u.is_active=TRUE
        )",
    )
    .bind(intent.user_id)
    .bind(STRATEGY_KEY)
    .bind(&intent.instrument)
    .fetch_one(&state.db)
    .await?;
    if !active {
        return Ok(Sl2ReversalOutcome::Cancelled(
            "The strategy or instrument was deactivated before the reversal could be submitted."
                .into(),
        ));
    }
    if !clear_entry_orders_for_sl2_reversal(state, intent).await? {
        return Ok(Sl2ReversalOutcome::Waiting(
            "Waiting for earlier breakout entry orders to finish cancelling at the broker.".into(),
        ));
    }

    let open_trade: Option<(String, i32)> = sqlx::query_as(
        "SELECT direction,total_lots
         FROM trades
         WHERE user_id=$1
           AND strategy_key=$2
           AND instrument_label=$3
           AND status='open'
         ORDER BY entry_datetime DESC
         LIMIT 1",
    )
    .bind(intent.user_id)
    .bind(STRATEGY_KEY)
    .bind(&intent.instrument)
    .fetch_optional(&state.db)
    .await?;
    let lots_to_place = match open_trade {
        Some((direction, open_lots)) if direction == plan.direction => {
            if open_lots >= plan.lots {
                return Ok(Sl2ReversalOutcome::Completed);
            }
            plan.lots - open_lots.max(0)
        }
        Some((direction, _)) => {
            return Ok(Sl2ReversalOutcome::Waiting(format!(
                "A {direction} position is still open; the {} SL2 reversal is paused.",
                plan.direction
            )));
        }
        None => plan.lots,
    };

    let query = format!("{} WHERE id=$1", snapshot_select());
    let mut snapshot: Snapshot = sqlx::query_as(&query)
        .bind(intent.snapshot_id)
        .fetch_one(&state.db)
        .await?;
    if snapshot.strategy_key != STRATEGY_KEY {
        return Ok(Sl2ReversalOutcome::Cancelled(
            "The reversal snapshot does not belong to Futures Breakout v3.".into(),
        ));
    }
    if !has_valid_contract_metadata(&snapshot, snapshot.trade_date)
        && let Some(refreshed) =
            force_refresh_futures_contract_snapshot(state, &intent.instrument, snapshot.trade_date)
                .await?
    {
        snapshot = refreshed;
    }
    let runner = runner_for(state, intent.user_id, &intent.instrument).await?;
    place_strategy_order(
        state,
        &runner,
        &snapshot,
        &intent.order_session_key,
        NewOrder {
            role: plan.entry_role,
            side: plan.entry_side,
            order_type: "MARKET",
            lots: lots_to_place,
            price: intent.entry_price,
            trigger: None,
            trade_id: Some(intent.source_trade_id),
            quantity: None,
        },
    )
    .await?;

    let order_status: Option<String> = sqlx::query_scalar(
        "SELECT status
         FROM strategy_orders
         WHERE user_id=$1
           AND snapshot_id=$2
           AND session_key LIKE $3 || '%'
           AND role=$4
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(intent.user_id)
    .bind(intent.snapshot_id)
    .bind(&intent.order_session_key)
    .bind(plan.entry_role)
    .fetch_optional(&state.db)
    .await?;
    match order_status.as_deref() {
        Some("filled") => Ok(Sl2ReversalOutcome::Completed),
        Some(
            "pending" | "submitting" | "ambiguous" | "submitted" | "partially_filled"
            | "processing",
        ) => Ok(Sl2ReversalOutcome::Submitted),
        Some("rejected" | "cancelled") => Ok(Sl2ReversalOutcome::Cancelled(
            "The broker rejected or cancelled the SL2 reversal entry.".into(),
        )),
        Some("failed") => Err(AppError::BadRequest(
            "The SL2 reversal entry failed before broker acknowledgement and will retry.".into(),
        )),
        Some(status) => Err(AppError::BadRequest(format!(
            "The SL2 reversal entry reached an unexpected order state: {status}."
        ))),
        None => Err(AppError::BadRequest(
            "The SL2 reversal entry was not reserved and will retry.".into(),
        )),
    }
}

async fn process_sl2_reversal_intent(state: &AppState, source_trade_id: Uuid) -> AppResult<()> {
    let now = ist_now();
    let (market_open, market_reason) = futures_runtime_is_open(state, now).await?;
    if !market_open {
        sqlx::query(
            "UPDATE strategy_reversal_intents
             SET status='waiting',
                 next_attempt_at=NOW()+INTERVAL '15 minutes',
                 last_error=$2,
                 updated_at=NOW()
             WHERE source_trade_id=$1
               AND status IN ('pending','waiting','failed')
               AND next_attempt_at<=NOW()",
        )
        .bind(source_trade_id)
        .bind(format!(
            "SL2 reversal is paused until the Futures Breakout market session opens: {market_reason}"
        ))
        .execute(&state.db)
        .await?;
        return Ok(());
    }
    let intent: Option<Sl2ReversalIntent> = sqlx::query_as(
        "UPDATE strategy_reversal_intents
         SET status='processing',
             attempts=attempts+1,
             updated_at=NOW()
         WHERE source_trade_id=$1
           AND status IN ('pending','waiting','failed')
           AND next_attempt_at<=NOW()
         RETURNING source_trade_id,user_id,snapshot_id,instrument,source_direction,reversal_direction,lots,entry_price,order_session_key,attempts,created_at",
    )
    .bind(source_trade_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(intent) = intent else {
        return Ok(());
    };

    let symbol: String = sqlx::query_scalar(
        "SELECT COALESCE(contract_symbol,'')
         FROM strategy_market_snapshots
         WHERE id=$1",
    )
    .bind(intent.snapshot_id)
    .fetch_one(&state.db)
    .await?;
    let contract_label = contract_log_label(&intent.instrument, Some(&symbol));
    let intent_date = intent.created_at.with_timezone(now.offset()).date_naive();
    if intent_date < now.date_naive() {
        let message = format!(
            "The SL2 reversal for {contract_label} was not submitted on {intent_date}; it has been cancelled instead of placing a stale next-day reversal."
        );
        sqlx::query(
            "UPDATE strategy_reversal_intents
             SET status='cancelled',
                 last_error=$2,
                 updated_at=NOW()
             WHERE source_trade_id=$1 AND status='processing'",
        )
        .bind(intent.source_trade_id)
        .bind(&message)
        .execute(&state.db)
        .await?;
        operational_alert(
            state,
            Some(intent.user_id),
            &intent.instrument,
            "sl2_reversal_stale_cancelled",
            "warning",
            &message,
        )
        .await;
        append_user_log(state, intent.user_id, &message).await;
        return Ok(());
    }
    match attempt_claimed_sl2_reversal(state, &intent).await {
        Ok(Sl2ReversalOutcome::Waiting(message)) => {
            sqlx::query(
                "UPDATE strategy_reversal_intents
                 SET status='waiting',
                     next_attempt_at=NOW()+INTERVAL '5 seconds',
                     last_error=$2,
                     updated_at=NOW()
                 WHERE source_trade_id=$1 AND status='processing'",
            )
            .bind(intent.source_trade_id)
            .bind(message)
            .execute(&state.db)
            .await?;
            Ok(())
        }
        Ok(Sl2ReversalOutcome::Submitted) => {
            let changed = sqlx::query(
                "UPDATE strategy_reversal_intents
                 SET status='submitted',last_error='',updated_at=NOW()
                 WHERE source_trade_id=$1 AND status='processing'",
            )
            .bind(intent.source_trade_id)
            .execute(&state.db)
            .await?;
            if changed.rows_affected() > 0 {
                emit(
                    state,
                    Some(intent.user_id),
                    &intent.instrument,
                    "sl2_reversal_submitted",
                    json!({
                        "source_trade_id":intent.source_trade_id,
                        "source_direction":&intent.source_direction,
                        "reversal_direction":&intent.reversal_direction,
                        "lots":intent.lots,
                        "entry_price":intent.entry_price
                    }),
                )
                .await;
                append_user_log(
                    state,
                    intent.user_id,
                    &format!(
                        "STRATEGY SL2 REVERSAL SUBMITTED {} {} {} lots @ MARKET ({:.2} reference)",
                        contract_label, intent.reversal_direction, intent.lots, intent.entry_price
                    ),
                )
                .await;
            }
            Ok(())
        }
        Ok(Sl2ReversalOutcome::Completed) => {
            let changed = sqlx::query(
                "UPDATE strategy_reversal_intents
                 SET status='completed',last_error='',updated_at=NOW()
                 WHERE source_trade_id=$1 AND status='processing'",
            )
            .bind(intent.source_trade_id)
            .execute(&state.db)
            .await?;
            if changed.rows_affected() > 0 {
                append_user_log(
                    state,
                    intent.user_id,
                    &format!(
                        "STRATEGY SL2 REVERSAL COMPLETED {} {} {} lots",
                        contract_label, intent.reversal_direction, intent.lots
                    ),
                )
                .await;
            }
            Ok(())
        }
        Ok(Sl2ReversalOutcome::Cancelled(message)) => {
            let changed = sqlx::query(
                "UPDATE strategy_reversal_intents
                 SET status='cancelled',last_error=$2,updated_at=NOW()
                 WHERE source_trade_id=$1 AND status='processing'",
            )
            .bind(intent.source_trade_id)
            .bind(&message)
            .execute(&state.db)
            .await?;
            if changed.rows_affected() > 0 {
                append_user_log(
                    state,
                    intent.user_id,
                    &format!(
                        "STRATEGY SL2 REVERSAL CANCELLED {}: {}",
                        contract_label, message
                    ),
                )
                .await;
                operational_alert(
                    state,
                    Some(intent.user_id),
                    &intent.instrument,
                    "sl2_reversal_cancelled",
                    "warning",
                    &message,
                )
                .await;
            }
            Ok(())
        }
        Err(error) => {
            let message = error.to_string();
            let delay_seconds = recoverable_retry_delay_seconds(&message, intent.attempts);
            let severity = retry_alert_severity(&message);
            let changed = sqlx::query(
                "UPDATE strategy_reversal_intents
                 SET status='failed',
                     next_attempt_at=NOW()+($3::int * INTERVAL '1 second'),
                     last_error=$2,
                     updated_at=NOW()
                 WHERE source_trade_id=$1 AND status='processing'",
            )
            .bind(intent.source_trade_id)
            .bind(&message)
            .bind(delay_seconds)
            .execute(&state.db)
            .await?;
            if changed.rows_affected() > 0 {
                operational_alert(
                    state,
                    Some(intent.user_id),
                    &intent.instrument,
                    "sl2_reversal_retry",
                    severity,
                    &format!(
                        "The full-lot SL2 reversal will retry automatically in about {delay_seconds} seconds: {message}"
                    ),
                )
                .await;
                return Err(error);
            }
            Ok(())
        }
    }
}

async fn recover_sl2_reversal_intents(state: &AppState) -> AppResult<()> {
    sqlx::query(
        "UPDATE strategy_reversal_intents
         SET status='pending',
             next_attempt_at=NOW(),
             last_error='Backend restarted while the reversal was being processed.',
             updated_at=NOW()
         WHERE status='processing' AND updated_at<NOW()-INTERVAL '30 seconds'",
    )
    .execute(&state.db)
    .await?;
    sqlx::query(
        "UPDATE strategy_reversal_intents i
         SET status='failed',
             next_attempt_at=NOW(),
             last_error='The reversal order failed before broker acknowledgement.',
             updated_at=NOW()
         WHERE i.status='submitted'
           AND EXISTS(
               SELECT 1
               FROM strategy_orders o
               WHERE o.user_id=i.user_id
                 AND o.snapshot_id=i.snapshot_id
                 AND o.session_key=i.order_session_key
                 AND o.status='failed'
                 AND o.broker_order_id=''
           )",
    )
    .execute(&state.db)
    .await?;
    let terminal: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "UPDATE strategy_reversal_intents i
         SET status='cancelled',
             last_error='The broker rejected or cancelled the submitted SL2 reversal order.',
             updated_at=NOW()
         WHERE i.status='submitted'
           AND EXISTS(
               SELECT 1
               FROM strategy_orders o
               WHERE o.user_id=i.user_id
                 AND o.snapshot_id=i.snapshot_id
                 AND o.session_key=i.order_session_key
                 AND o.status IN ('rejected','cancelled')
           )
         RETURNING i.source_trade_id,i.user_id,i.instrument",
    )
    .fetch_all(&state.db)
    .await?;
    for (source_trade_id, user_id, instrument) in terminal {
        operational_alert(
            state,
            Some(user_id),
            &instrument,
            "sl2_reversal_cancelled",
            "error",
            &format!(
                "The broker rejected or cancelled the full-lot SL2 reversal for trade {source_trade_id}."
            ),
        )
        .await;
    }
    let stale: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "UPDATE strategy_reversal_intents i
         SET status='cancelled',
             last_error='The SL2 reversal was not submitted on the source trade day; stale next-day reversals are not safe.',
             updated_at=NOW()
         WHERE i.status IN ('pending','waiting','failed')
           AND i.created_at < (
               date_trunc('day', NOW() AT TIME ZONE 'Asia/Kolkata')
               AT TIME ZONE 'Asia/Kolkata'
           )
         RETURNING i.source_trade_id,i.user_id,i.instrument",
    )
    .fetch_all(&state.db)
    .await?;
    for (source_trade_id, user_id, instrument) in stale {
        operational_alert(
            state,
            Some(user_id),
            &instrument,
            "sl2_reversal_stale_cancelled",
            "warning",
            &format!(
                "The full-lot SL2 reversal for trade {source_trade_id} was cancelled because it became stale before a safe same-day submission."
            ),
        )
        .await;
    }
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT source_trade_id
         FROM strategy_reversal_intents
         WHERE status IN ('pending','waiting','failed')
           AND next_attempt_at<=NOW()
         ORDER BY created_at
         LIMIT 100",
    )
    .fetch_all(&state.db)
    .await?;
    for source_trade_id in ids {
        if let Err(error) = process_sl2_reversal_intent(state, source_trade_id).await {
            tracing::warn!(%source_trade_id, %error, "SL2 reversal recovery failed");
        }
    }
    Ok(())
}

pub(crate) async fn complete_order(
    state: &AppState,
    order: StoredOrder,
    fill: f64,
) -> AppResult<()> {
    let cumulative_fill = order.cumulative_fill_quantity();
    let claimed=sqlx::query("UPDATE strategy_orders SET status='processing',filled_price=$2,average_fill_price=CASE WHEN execution_mode='live' THEN average_fill_price ELSE $2 END,filled_quantity=GREATEST(filled_quantity,$3),filled_at=NOW(),state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status IN ('submitted','partially_filled') AND processed_quantity<$3")
        .bind(order.id).bind(fill).bind(cumulative_fill).execute(&state.db).await?;
    if claimed.rows_affected() == 0 {
        let processed: i32 =
            sqlx::query_scalar("SELECT processed_quantity FROM strategy_orders WHERE id=$1")
                .bind(order.id)
                .fetch_one(&state.db)
                .await?;
        if processed >= cumulative_fill {
            return Ok(());
        }
        return Err(AppError::BadRequest(
            "The fill claim changed state concurrently; broker reconciliation will retry it."
                .into(),
        ));
    }
    let order_id = order.id;
    let result = complete_claimed_order(state, order, fill).await;
    if result.is_ok() {
        sqlx::query("UPDATE strategy_orders SET status=CASE WHEN $2<quantity THEN 'partially_filled' ELSE 'filled' END,processed_quantity=GREATEST(processed_quantity,$2),filled_quantity=GREATEST(filled_quantity,$2),state_version=state_version+1,updated_at=NOW() WHERE id=$1")
            .bind(order_id)
            .bind(cumulative_fill)
            .execute(&state.db)
            .await?;
        return Ok(());
    }
    if let Err(error) = &result {
        // Some handlers commit the position mutation before performing a
        // recoverable protective-order side effect. A cumulative ledger write
        // in that transaction proves the fill itself was already accounted.
        let processed: i32 =
            sqlx::query_scalar("SELECT processed_quantity FROM strategy_orders WHERE id=$1")
                .bind(order_id)
                .fetch_one(&state.db)
                .await?;
        if processed >= cumulative_fill {
            sqlx::query("UPDATE strategy_orders SET status=CASE WHEN $2<quantity THEN 'partially_filled' ELSE 'filled' END,filled_quantity=GREATEST(filled_quantity,$2),broker_status=CONCAT('Fill committed; post-fill recovery required: ',$3),state_version=state_version+1,updated_at=NOW() WHERE id=$1")
                .bind(order_id)
                .bind(cumulative_fill)
                .bind(error.to_string())
                .execute(&state.db)
                .await?;
            tracing::warn!(%order_id, %error, "fill committed but a post-fill side effect requires recovery");
            return Ok(());
        }
        let recovered = sqlx::query("UPDATE strategy_orders SET status=CASE WHEN processed_quantity>0 AND processed_quantity<filled_quantity THEN 'partially_filled' ELSE 'submitted' END,broker_status='Fill processing failed before commit; queued for reconciliation.',state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status='processing'")
            .bind(order_id)
            .execute(&state.db)
            .await;
        match recovered {
            Ok(result) if result.rows_affected() > 0 => {
                tracing::warn!(%order_id, %error, "fill processing failed; order returned to reconciliation queue");
            }
            Ok(_) => {}
            Err(recovery_error) => {
                tracing::error!(%order_id, %error, %recovery_error, "fill processing failed and the processing claim could not be recovered");
            }
        }
    }
    result
}

async fn complete_supertrend_entry_order(
    state: &AppState,
    order: &StoredOrder,
    snapshot: &Snapshot,
    fill: f64,
    cumulative_fill: i32,
) -> AppResult<bool> {
    if !matches!(order.role.as_str(), "BUY_ENTRY" | "SELL_ENTRY") {
        return Ok(false);
    }
    let (target, stop, exit_side) = if snapshot.strategy_key
        == SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
    {
        let (target_points, stop_points) = supertrend_config_points(snapshot)
            .ok_or_else(|| AppError::BadRequest("SuperTrend TP/SL points are missing.".into()))?;
        let side = supertrend_snapshot_side(&snapshot.instrument)
            .ok_or_else(|| AppError::BadRequest("SuperTrend option side is missing.".into()))?;
        (
            fill + target_points,
            (fill - stop_points).max(0.05),
            side.exit_side(),
        )
    } else {
        return Ok(false);
    };
    let trade_id = Uuid::new_v4();
    let mut fill_tx = state.db.begin().await?;
    sqlx::query("INSERT INTO trades (id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,external_entry_id,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,safety_status,protection_deadline_at) SELECT $1,$2,execution_mode,'open',$3,$4,($5::float8)::numeric,($5::float8)::numeric,0,NOW(),$6,$7,broker_order_id,'SuperTrend Index Options v1',$8,$9,$10,$10,$11,$12,CASE WHEN execution_mode='live' THEN 'PROTECTION_REQUIRED' ELSE 'DEMO' END,CASE WHEN execution_mode='live' THEN NOW()+($14::text || ' seconds')::interval END FROM strategy_orders WHERE id=$13")
        .bind(trade_id)
        .bind(order.user_id)
        .bind(&order.side)
        .bind(order.quantity)
        .bind(fill)
        .bind(&snapshot.instrument)
        .bind(snapshot.contract_symbol.as_deref().unwrap_or(""))
        .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
        .bind(snapshot.id)
        .bind(order.lots.max(1))
        .bind(target)
        .bind(stop)
        .bind(order.id)
        .bind(state.config.protection_ack_timeout_seconds)
        .execute(&mut *fill_tx)
        .await?;
    sqlx::query("UPDATE strategy_orders SET status=CASE WHEN $3<quantity THEN 'partially_filled' ELSE 'filled' END,trade_id=$2,processed_quantity=GREATEST(processed_quantity,$3),filled_quantity=GREATEST(filled_quantity,$3),updated_at=NOW() WHERE id=$1")
        .bind(order.id)
        .bind(trade_id)
        .bind(cumulative_fill)
        .execute(&mut *fill_tx)
        .await?;
    fill_tx.commit().await?;
    trip_execution_failpoint("after_fill_commit_before_protection").await?;
    crate::notifications::notify_trade_opened(state.clone(), trade_id);
    emit_for(
        state,
        SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
        Some(order.user_id),
        &snapshot.instrument,
        "supertrend_position_opened",
        json!({"trade_id":trade_id,"side":order.side,"fill_price":fill,"target":target,"stop_loss":stop,"lots":order.lots}),
    )
    .await;
    let contract_label =
        contract_log_label(&snapshot.instrument, snapshot.contract_symbol.as_deref());
    append_user_log(
        state,
        order.user_id,
        &format!(
            "OPTION POSITION OPENED {} {} {} lots @ {:.2} TARGET {:.2} SL {:.2} [{}]",
            contract_label,
            order.side,
            order.lots,
            fill,
            target,
            stop,
            order.execution_mode.to_uppercase()
        ),
    )
    .await;
    let underlying = supertrend_snapshot_underlying(&snapshot.instrument).ok_or_else(|| {
        AppError::BadRequest("SuperTrend underlying instrument is missing.".into())
    })?;
    let runner = if order.execution_mode == "live" {
        protection_runner(
            state,
            order.user_id,
            SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
            snapshot,
        )
        .await?
    } else {
        runner_for_strategy(
            state,
            order.user_id,
            SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
            underlying,
        )
        .await?
    };
    let protection_session = supertrend_protection_session_key(&order.session_key);
    sqlx::query("UPDATE trades SET safety_status=CASE WHEN execution_mode='live' THEN 'PROTECTION_SUBMITTING' ELSE 'PROTECTED' END,protection_attempts=protection_attempts+1,last_protection_error='',updated_at=NOW() WHERE id=$1")
        .bind(trade_id).execute(&state.db).await?;
    let stop_result = place_strategy_order(
        state,
        &runner,
        snapshot,
        &protection_session,
        NewOrder {
            role: "SL1",
            side: exit_side,
            order_type: "STOPLOSS_MARKET",
            lots: order.lots.max(1),
            price: stop,
            trigger: Some(stop),
            trade_id: Some(trade_id),
            quantity: Some(order.quantity.max(1)),
        },
    )
    .await;
    if let Err(error) = stop_result {
        mark_protection_submission_failure(state, trade_id, &error.to_string()).await?;
        operational_alert_for(state,SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,Some(order.user_id),&snapshot.instrument,"unprotected_live_position","critical",&format!("Entry filled but stop protection was not confirmed; recovery or emergency close is required: {error}")).await;
        return Err(error);
    }
    if order.execution_mode == "live" {
        return Ok(true);
    }
    place_strategy_order(
        state,
        &runner,
        snapshot,
        &protection_session,
        NewOrder {
            role: "TARGET",
            side: exit_side,
            order_type: "LIMIT",
            lots: order.lots.max(1),
            price: target,
            trigger: None,
            trade_id: Some(trade_id),
            quantity: Some(order.quantity.max(1)),
        },
    )
    .await?;
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OppositeFillAccounting {
    offset_quantity: i32,
    residual_existing: i32,
    residual_incoming: i32,
}

fn account_opposite_fill(existing_quantity: i32, incoming_quantity: i32) -> OppositeFillAccounting {
    let existing_quantity = existing_quantity.max(0);
    let incoming_quantity = incoming_quantity.max(0);
    let offset_quantity = existing_quantity.min(incoming_quantity);
    OppositeFillAccounting {
        offset_quantity,
        residual_existing: existing_quantity - offset_quantity,
        residual_incoming: incoming_quantity - offset_quantity,
    }
}

#[cfg(test)]
fn futures_fill_overlap_barrier() -> &'static tokio::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>
{
    static BARRIER: std::sync::OnceLock<tokio::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>> =
        std::sync::OnceLock::new();
    BARRIER.get_or_init(|| tokio::sync::Mutex::new(None))
}

#[cfg(test)]
async fn wait_at_futures_fill_overlap_failpoint() {
    let barrier = futures_fill_overlap_barrier().lock().await.clone();
    if let Some(barrier) = barrier {
        barrier.wait().await;
    }
}

#[cfg(not(test))]
async fn wait_at_futures_fill_overlap_failpoint() {}

#[cfg(test)]
fn execution_failpoints() -> &'static tokio::sync::Mutex<HashSet<&'static str>> {
    static FAILPOINTS: std::sync::OnceLock<tokio::sync::Mutex<HashSet<&'static str>>> =
        std::sync::OnceLock::new();
    FAILPOINTS.get_or_init(|| tokio::sync::Mutex::new(HashSet::new()))
}

#[cfg(test)]
async fn trip_execution_failpoint(name: &'static str) -> AppResult<()> {
    if execution_failpoints().lock().await.remove(name) {
        return Err(AppError::BadRequest(format!(
            "test-only execution failpoint triggered: {name}"
        )));
    }
    Ok(())
}

#[cfg(not(test))]
async fn trip_execution_failpoint(_name: &'static str) -> AppResult<()> {
    Ok(())
}

async fn complete_claimed_order(
    state: &AppState,
    mut order: StoredOrder,
    fill: f64,
) -> AppResult<()> {
    let cumulative_fill = order.cumulative_fill_quantity();
    let query = format!("{} WHERE id=$1", snapshot_select());
    let snapshot: Snapshot = sqlx::query_as(&query)
        .bind(order.snapshot_id)
        .fetch_one(&state.db)
        .await?;
    if complete_supertrend_entry_order(state, &order, &snapshot, fill, cumulative_fill).await? {
        return Ok(());
    }
    let instrument = snapshot.instrument.clone();
    let snapshot_contract_label =
        contract_log_label(&instrument, snapshot.contract_symbol.as_deref());
    match order.role.as_str() {
        "BUY_ENTRY" | "SELL_ENTRY" => {
            let direction = if order.role == "BUY_ENTRY" {
                "BUY"
            } else {
                "SELL"
            };
            let reversal_source_trade_id: Option<Uuid> = sqlx::query_scalar(
                "SELECT source_trade_id
                 FROM strategy_reversal_intents
                 WHERE user_id=$1 AND order_session_key=$2
                 LIMIT 1",
            )
            .bind(order.user_id)
            .bind(&order.session_key)
            .fetch_optional(&state.db)
            .await?;
            let exit_levels = snapshot_order_exit_levels(
                &snapshot,
                direction,
                fill,
                reversal_source_trade_id.is_some(),
            )?;
            let target = exit_levels.target;
            let sl1 = exit_levels.sl1;
            let sl2 = exit_levels.sl2;
            let mut unexpected_residual = false;
            let exposure_lock_key = format!(
                "strategy-exposure:{}:{}:{}",
                order.user_id, STRATEGY_KEY, instrument
            );
            wait_at_futures_fill_overlap_failpoint().await;
            let mut exposure_tx = state.db.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,0))")
                .bind(&exposure_lock_key)
                .execute(&mut *exposure_tx)
                .await?;
            let locked_order: (String, i32, i32) = sqlx::query_as(
                "SELECT status,filled_quantity,processed_quantity
                 FROM strategy_orders
                 WHERE id=$1
                 FOR UPDATE",
            )
            .bind(order.id)
            .fetch_one(&mut *exposure_tx)
            .await?;
            if locked_order.2 >= cumulative_fill {
                exposure_tx.commit().await?;
                return Ok(());
            }
            if locked_order.0 != "processing" || locked_order.1 < cumulative_fill {
                return Err(AppError::BadRequest(format!(
                    "Futures fill {} lost its processing claim or cumulative broker watermark before exposure accounting.",
                    order.id
                )));
            }
            let expected_delta = cumulative_fill.saturating_sub(locked_order.2);
            if expected_delta != order.quantity {
                return Err(AppError::BadRequest(format!(
                    "Futures fill {} delta changed from {} to {} before exposure accounting; reconciliation must retry from the durable watermark.",
                    order.id, order.quantity, expected_delta
                )));
            }
            let mut cancel_existing_trade_id = None;
            if let Some(existing)=sqlx::query_as::<_,(Uuid,String,i32,f64,i32,i32,String,Option<f64>)>("SELECT id,direction,quantity,entry_price::float8,total_lots,remaining_lots,COALESCE(contract_symbol,''),target_price::float8 FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 AND status='open' ORDER BY entry_datetime DESC LIMIT 1 FOR UPDATE")
                .bind(order.user_id).bind(STRATEGY_KEY).bind(&instrument).fetch_optional(&mut *exposure_tx).await? {
                if existing.1!=direction {
                    cancel_existing_trade_id = Some(existing.0);
                    let accounting = account_opposite_fill(existing.2, order.quantity);
                    let offset_quantity = accounting.offset_quantity;
                    let pnl=trade_pnl(&existing.1,existing.3,fill,runtime_pnl_units(&instrument, offset_quantity, snapshot.lot_size));
                    let residual_existing = accounting.residual_existing;
                    let residual_incoming = accounting.residual_incoming;
                    if residual_existing==0 {
                        sqlx::query("UPDATE trades SET status='closed',safety_status='CLOSED',quantity=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=($3::float8)::numeric,exit_datetime=NOW(),remaining_lots=0,exit_reason=$4,notes=CONCAT(notes,'; opposite entry fill accounted'),updated_at=NOW() WHERE id=$1")
                            .bind(existing.0).bind(fill).bind(pnl).bind(if reversal_source_trade_id.is_some(){"SAR_REVERSAL"}else{"LATE_OPPOSITE_ENTRY_FILL"}).execute(&mut *exposure_tx).await?;
                    } else {
                        let lot_size = snapshot.lot_size.unwrap_or(1).max(1);
                        let residual_lots = ((residual_existing + lot_size - 1) / lot_size).max(1);
                        sqlx::query("UPDATE trades SET quantity=$2,remaining_lots=$3,last_price=($4::float8)::numeric,pnl=(pnl::float8+$5)::numeric,safety_status=CASE WHEN execution_mode='live' THEN 'PROTECTION_REQUIRED' ELSE safety_status END,protection_deadline_at=CASE WHEN execution_mode='live' THEN NOW()+($6::text || ' seconds')::interval ELSE protection_deadline_at END,updated_at=NOW() WHERE id=$1")
                            .bind(existing.0).bind(residual_existing).bind(residual_lots).bind(fill).bind(pnl).bind(state.config.protection_ack_timeout_seconds).execute(&mut *exposure_tx).await?;
                    }
                    if residual_incoming==0 {
                        sqlx::query("UPDATE strategy_orders SET status=CASE WHEN $3<quantity THEN 'partially_filled' ELSE 'filled' END,trade_id=$2,broker_status='Opposite entry fill was accounted against actual local exposure.',processed_quantity=GREATEST(processed_quantity,$3),filled_quantity=GREATEST(filled_quantity,$3),updated_at=NOW() WHERE id=$1")
                            .bind(order.id).bind(existing.0).bind(cumulative_fill).execute(&mut *exposure_tx).await?;
                        exposure_tx.commit().await?;
                        if let Err(error) = cancel_active_exits(state,order.user_id,existing.0).await {
                            operational_alert_for(state,STRATEGY_KEY,Some(order.user_id),&instrument,"opposite_fill_exit_cancel_failed","critical",&format!("Opposite fill {} was durably accounted, but existing exits for trade {} could not yet be cancelled: {error}",order.id,existing.0)).await;
                        }
                        let contract_label = contract_log_label(&instrument, Some(&existing.6));
                        if reversal_source_trade_id.is_none() {
                            operational_alert_for(state,STRATEGY_KEY,Some(order.user_id),&instrument,"late_opposite_entry_fill","critical",&format!("A sibling/opposite entry filled after another position existed. Order {}; trade {}; offset quantity {offset_quantity}; incoming residual {residual_incoming}; local exposure was updated and broker reconciliation is required.",order.id,existing.0)).await;
                        }
                        append_user_log(state, order.user_id, &format!("STRATEGY OPPOSITE FILL ACCOUNTED {} @ {:.2} P&L {:+.2}", contract_label, fill, pnl)).await;
                        return Ok(());
                    }
                    order.quantity=residual_incoming;
                    let lot_size = snapshot.lot_size.unwrap_or(1).max(1);
                    order.lots = ((residual_incoming + lot_size - 1) / lot_size).max(1);
                    unexpected_residual=reversal_source_trade_id.is_none();
                } else {
                    let fixed_target = required_exit_level(existing.7, "fixed target")?;
                    let old_target_lots = target_exit_lots(existing.4);
                    let new_total_lots = existing.4.saturating_add(order.lots.max(0));
                    let new_target_lots = target_exit_lots(new_total_lots);
                    let added_target_lots = (new_target_lots - old_target_lots).max(0);
                    let new_quantity = existing.2.saturating_add(order.quantity);
                    let weighted_entry = (existing.3 * existing.2 as f64
                        + fill * order.quantity as f64)
                        / new_quantity.max(1) as f64;
                    sqlx::query("UPDATE trades SET quantity=$2,entry_price=($3::float8)::numeric,last_price=($4::float8)::numeric,total_lots=$5,remaining_lots=remaining_lots+$6,safety_status=CASE WHEN execution_mode='live' THEN 'PROTECTION_REQUIRED' ELSE safety_status END,protection_deadline_at=CASE WHEN execution_mode='live' THEN NOW()+($7::text || ' seconds')::interval ELSE protection_deadline_at END,updated_at=NOW() WHERE id=$1 AND status='open'")
                        .bind(existing.0).bind(new_quantity).bind(weighted_entry).bind(fill)
                        .bind(new_total_lots).bind(order.lots.max(0))
                        .bind(state.config.protection_ack_timeout_seconds)
                        .execute(&mut *exposure_tx).await?;
                    sqlx::query("UPDATE strategy_orders SET status=CASE WHEN $3<quantity THEN 'partially_filled' ELSE 'filled' END,trade_id=$2,processed_quantity=GREATEST(processed_quantity,$3),filled_quantity=GREATEST(filled_quantity,$3),updated_at=NOW() WHERE id=$1")
                        .bind(order.id).bind(existing.0).bind(cumulative_fill).execute(&mut *exposure_tx).await?;
                    if let Some(source_trade_id) = reversal_source_trade_id {
                        sqlx::query("UPDATE strategy_reversal_intents SET status='completed',last_error='',updated_at=NOW() WHERE source_trade_id=$1")
                            .bind(source_trade_id)
                            .execute(&mut *exposure_tx)
                            .await?;
                    }
                    exposure_tx.commit().await?;

                    let runner = if order.execution_mode == "live" {
                        protection_runner(state, order.user_id, STRATEGY_KEY, &snapshot).await?
                    } else {
                        runner_for(state, order.user_id, &instrument).await?
                    };
                    let exit_side = if direction == "BUY" { "SELL" } else { "BUY" };
                    let tranche_session = format!("{}:fill:{}", order.session_key, cumulative_fill);
                    sqlx::query("UPDATE trades SET safety_status=CASE WHEN execution_mode='live' THEN 'PROTECTION_SUBMITTING' ELSE 'PROTECTED' END,protection_attempts=protection_attempts+1,last_protection_error='',updated_at=NOW() WHERE id=$1")
                        .bind(existing.0).execute(&state.db).await?;
                    let stop_result = place_strategy_order(
                        state,
                        &runner,
                        &snapshot,
                        &tranche_session,
                        NewOrder {
                            role: "SL1",
                            side: exit_side,
                            order_type: "STOPLOSS_MARKET",
                            lots: order.lots.max(1),
                            price: sl1,
                            trigger: Some(sl1),
                            trade_id: Some(existing.0),
                            quantity: Some(order.quantity),
                        },
                    )
                    .await;
                    if let Err(error) = stop_result {
                        mark_protection_submission_failure(
                            state,
                            existing.0,
                            &error.to_string(),
                        )
                        .await?;
                        operational_alert_for(state,STRATEGY_KEY,Some(order.user_id),&instrument,"unprotected_live_position","critical",&format!("An additional real fill increased exposure, but its stop could not be confirmed: {error}")).await;
                        return Err(error);
                    }
                    if order.execution_mode == "demo" && added_target_lots > 0 {
                        place_strategy_order(
                            state,
                            &runner,
                            &snapshot,
                            &tranche_session,
                            NewOrder {
                                role: "TARGET",
                                side: exit_side,
                                order_type: "LIMIT",
                                lots: added_target_lots,
                                price: fixed_target,
                                trigger: None,
                                trade_id: Some(existing.0),
                                quantity: Some(
                                    (added_target_lots
                                        * snapshot.lot_size.unwrap_or(1).max(1))
                                        .min(order.quantity),
                                ),
                            },
                        )
                        .await?;
                    }
                    emit(state,Some(order.user_id),&instrument,"position_increased",json!({"trade_id":existing.0,"direction":direction,"fill_price":fill,"fill_delta":order.quantity,"cumulative_broker_fill":cumulative_fill,"quantity":new_quantity})).await;
                    append_user_log(
                        state,
                        order.user_id,
                        &format!(
                            "STRATEGY POSITION INCREASED {} {} +{} lots @ {:.2} [{}]",
                            snapshot_contract_label,
                            direction,
                            order.lots,
                            fill,
                            order.execution_mode.to_uppercase()
                        ),
                    )
                    .await;
                    return Ok(());
                }
            }
            let trade_id = Uuid::new_v4();
            sqlx::query("INSERT INTO trades (id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,external_entry_id,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,sl2_price,reversal_of_trade_id,safety_status,protection_deadline_at) SELECT $1,$2,execution_mode,'open',$3,$4,($5::float8)::numeric,($5::float8)::numeric,0,NOW(),$6,$7,broker_order_id,'Futures Breakout v3',$8,$9,$10,$10,$11,$12,$13,$14,CASE WHEN execution_mode='live' AND $16 THEN 'EMERGENCY_CLOSING' WHEN execution_mode='live' THEN 'PROTECTION_REQUIRED' ELSE 'DEMO' END,CASE WHEN execution_mode='live' THEN NOW()+($17::text || ' seconds')::interval END FROM strategy_orders WHERE id=$15")
                .bind(trade_id).bind(order.user_id).bind(direction).bind(order.quantity).bind(fill).bind(&instrument).bind(snapshot.contract_symbol.as_deref().unwrap_or(""))
                .bind(STRATEGY_KEY).bind(snapshot.id).bind(order.lots.max(1)).bind(target).bind(sl1).bind(sl2).bind(reversal_source_trade_id).bind(order.id).bind(unexpected_residual).bind(state.config.protection_ack_timeout_seconds).execute(&mut *exposure_tx).await?;
            sqlx::query("UPDATE strategy_orders SET status=CASE WHEN $3<quantity THEN 'partially_filled' ELSE 'filled' END,trade_id=$2,broker_status=CASE WHEN $4 THEN 'Opposite entry fill offset existing exposure and left an unintended residual.' ELSE broker_status END,processed_quantity=GREATEST(processed_quantity,$3),filled_quantity=GREATEST(filled_quantity,$3),updated_at=NOW() WHERE id=$1").bind(order.id).bind(trade_id).bind(cumulative_fill).bind(unexpected_residual).execute(&mut *exposure_tx).await?;
            if let Some(source_trade_id) = reversal_source_trade_id {
                sqlx::query("UPDATE strategy_reversal_intents SET status='completed',last_error='',updated_at=NOW() WHERE source_trade_id=$1")
                    .bind(source_trade_id)
                    .execute(&mut *exposure_tx)
                    .await?;
            }
            exposure_tx.commit().await?;
            trip_execution_failpoint("after_fill_commit_before_protection").await?;
            if let Some(existing_trade_id) = cancel_existing_trade_id
                && let Err(error) =
                    cancel_active_exits(state, order.user_id, existing_trade_id).await
            {
                operational_alert_for(state,STRATEGY_KEY,Some(order.user_id),&instrument,"opposite_fill_exit_cancel_failed","critical",&format!("Opposite fill {} was durably accounted, but existing exits for trade {existing_trade_id} could not yet be cancelled: {error}",order.id)).await;
            }
            crate::notifications::notify_trade_opened(state.clone(), trade_id);
            if let Err(error) = cancel_active_breakout_entry_orders(
                state,
                order.user_id,
                &instrument,
                order.id,
                "Cancelled because a Futures Breakout position is already open for this instrument.",
            )
            .await
            {
                operational_alert_for(
                    state,
                    STRATEGY_KEY,
                    Some(order.user_id),
                    &instrument,
                    "entry_cleanup_failed",
                    "error",
                    &format!(
                        "A breakout position opened, but leftover entry-order cancellation failed: {error}"
                    ),
                )
                .await;
            }
            if unexpected_residual {
                operational_alert_for(state,STRATEGY_KEY,Some(order.user_id),&instrument,"unintended_reverse_exposure","critical",&format!("Opposite breakout fills crossed and left an unintended residual of {} units. Trade {trade_id} is durably marked EMERGENCY_CLOSING.", order.quantity)).await;
                return Ok(());
            }
            let runner = if order.execution_mode == "live" {
                protection_runner(state, order.user_id, STRATEGY_KEY, &snapshot).await?
            } else {
                runner_for(state, order.user_id, &instrument).await?
            };
            let close_lots = target_exit_lots(order.lots);
            let exit_side = if direction == "BUY" { "SELL" } else { "BUY" };
            sqlx::query("UPDATE trades SET safety_status=CASE WHEN execution_mode='live' THEN 'PROTECTION_SUBMITTING' ELSE 'PROTECTED' END,protection_attempts=protection_attempts+1,last_protection_error='',updated_at=NOW() WHERE id=$1")
                .bind(trade_id).execute(&state.db).await?;
            let stop_result = place_strategy_order(
                state,
                &runner,
                &snapshot,
                &order.session_key,
                NewOrder {
                    role: "SL1",
                    side: exit_side,
                    order_type: "STOPLOSS_MARKET",
                    lots: order.lots,
                    price: sl1,
                    trigger: Some(sl1),
                    trade_id: Some(trade_id),
                    quantity: Some(order.quantity),
                },
            )
            .await;
            if let Err(error) = stop_result {
                mark_protection_submission_failure(state, trade_id, &error.to_string()).await?;
                operational_alert_for(state,STRATEGY_KEY,Some(order.user_id),&instrument,"unprotected_live_position","critical",&format!("Entry filled but stop protection was not confirmed; recovery or emergency close is required: {error}")).await;
                return Err(error);
            }
            if order.execution_mode == "demo" {
                place_strategy_order(
                    state,
                    &runner,
                    &snapshot,
                    &order.session_key,
                    NewOrder {
                        role: "TARGET",
                        side: exit_side,
                        order_type: "LIMIT",
                        lots: close_lots.min(order.lots),
                        price: target,
                        trigger: None,
                        trade_id: Some(trade_id),
                        quantity: Some(
                            (close_lots.min(order.lots) * snapshot.lot_size.unwrap_or(1))
                                .min(order.quantity),
                        ),
                    },
                )
                .await?;
            }
            emit(state,Some(order.user_id),&instrument,"position_opened",json!({
                "trade_id":trade_id,
                "direction":direction,
                "fill_price":fill,
                "lots":order.lots,
                "gap_direction":snapshot.gap_direction.as_deref(),
                "entry_source":if reversal_source_trade_id.is_some(){"SL2_REVERSAL"}else{snapshot.entry_source.as_deref().unwrap_or("STANDARD")},
                "previous_close":snapshot.previous_close,
                "market_open":snapshot.market_open,
                "opening_range_high":snapshot.opening_range_high,
                "opening_range_low":snapshot.opening_range_low,
                "planned_entry":snapshot.planned_entry,
            })).await;
            append_user_log(
                state,
                order.user_id,
                &format!(
                    "STRATEGY POSITION OPENED {} {} {} lots @ {:.2} {} [{}]",
                    snapshot_contract_label,
                    direction,
                    order.lots,
                    fill,
                    if reversal_source_trade_id.is_some() {
                        "SL2_REVERSAL"
                    } else {
                        snapshot.entry_source.as_deref().unwrap_or("STANDARD")
                    },
                    runner.trading_mode.to_uppercase()
                ),
            )
            .await;
        }
        "TARGET" => {
            if let Some(trade_id) = order.trade_id {
                let trade:(String,i32,i32,i32,f64,Option<f64>,String,String)=sqlx::query_as("SELECT direction,total_lots,remaining_lots,quantity,entry_price::float8,sl2_price::float8,COALESCE(contract_symbol,''),strategy_key FROM trades WHERE id=$1").bind(trade_id).fetch_one(&state.db).await?;
                cancel_active_exits(state, order.user_id, trade_id).await?;
                let closed = order.lots.min(trade.2);
                let remaining = (trade.2 - closed).max(0);
                let closed_quantity = order.quantity.min(trade.3).max(0);
                let remaining_quantity = (trade.3 - closed_quantity).max(0);
                let realized = trade_pnl(
                    &trade.0,
                    trade.4,
                    fill,
                    runtime_pnl_units(&instrument, closed_quantity, snapshot.lot_size),
                );
                let reporting_quantity = if trade.7 == STRATEGY_KEY {
                    trade
                        .1
                        .saturating_mul(snapshot.lot_size.unwrap_or(1).max(1))
                } else {
                    trade.3
                };
                let mut fill_tx = state.db.begin().await?;
                if remaining_quantity == 0 {
                    sqlx::query("UPDATE trades SET status='closed',safety_status='CLOSED',quantity=$4,remaining_lots=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=(pnl::float8+$3)::numeric,exit_datetime=NOW(),exit_reason=CASE WHEN strategy_key='futures_breakout_v3' THEN 'TP1' ELSE 'TP' END,tp1_exit_price=CASE WHEN strategy_key='futures_breakout_v3' THEN ($2::float8)::numeric ELSE tp1_exit_price END,tp1_exit_datetime=CASE WHEN strategy_key='futures_breakout_v3' THEN NOW() ELSE tp1_exit_datetime END,tp1_exit_quantity=CASE WHEN strategy_key='futures_breakout_v3' THEN $5 ELSE tp1_exit_quantity END,updated_at=NOW() WHERE id=$1").bind(trade_id).bind(fill).bind(realized).bind(reporting_quantity).bind(closed_quantity).execute(&mut *fill_tx).await?;
                } else {
                    sqlx::query("UPDATE trades SET safety_status=CASE WHEN execution_mode='live' THEN 'PROTECTION_REQUIRED' ELSE safety_status END,protection_deadline_at=CASE WHEN execution_mode='live' THEN NOW()+($7::text || ' seconds')::interval ELSE protection_deadline_at END,remaining_lots=$2,quantity=$3,last_price=($4::float8)::numeric,pnl=(pnl::float8+$5)::numeric,tp1_exit_price=($4::float8)::numeric,tp1_exit_datetime=NOW(),tp1_exit_quantity=tp1_exit_quantity+$6,updated_at=NOW() WHERE id=$1").bind(trade_id).bind(remaining).bind(remaining_quantity).bind(fill).bind(realized).bind(closed_quantity).bind(state.config.protection_ack_timeout_seconds).execute(&mut *fill_tx).await?;
                }
                sqlx::query("UPDATE strategy_orders SET status=CASE WHEN $2<quantity THEN 'partially_filled' ELSE 'filled' END,processed_quantity=GREATEST(processed_quantity,$2),filled_quantity=GREATEST(filled_quantity,$2),updated_at=NOW() WHERE id=$1").bind(order.id).bind(cumulative_fill).execute(&mut *fill_tx).await?;
                fill_tx.commit().await?;
                // A live SL1 remains capable of filling until Angel confirms
                // its cancellation. The reconciliation loop creates SL2 only
                // after every earlier protective order is terminal, avoiding
                // overlapping stops at different daily levels.
                if remaining_quantity > 0 && order.execution_mode == "demo" {
                    let runner = runner_for(state, order.user_id, &instrument).await?;
                    let sl2 = required_exit_level(trade.5, "continuation stop loss")?;
                    let side = if trade.0 == "BUY" { "SELL" } else { "BUY" };
                    place_strategy_order(
                        state,
                        &runner,
                        &snapshot,
                        &order.session_key,
                        NewOrder {
                            role: "SL2",
                            side,
                            order_type: "STOPLOSS_LIMIT",
                            lots: remaining,
                            price: sl2,
                            trigger: Some(sl2),
                            trade_id: Some(trade_id),
                            quantity: Some(remaining_quantity),
                        },
                    )
                    .await?;
                }
                emit(state,Some(order.user_id),&instrument,"target_filled",json!({"trade_id":trade_id,"fill_price":fill,"closed_lots":closed,"remaining_lots":remaining})).await;
                let contract_label = contract_log_label(&instrument, Some(&trade.6));
                append_user_log(state, order.user_id, &format!("STRATEGY TARGET FILLED {} {} lots @ {:.2} REALIZED P&L {:+.2}; {} lots remain", contract_label, closed, fill, realized, remaining)).await;
            }
        }
        "SL1" | "SL2" | "EMERGENCY_CLOSE" => {
            if let Some(trade_id) = order.trade_id {
                let trade: ExitFillTradeRow = sqlx::query_as("SELECT direction,quantity,remaining_lots,total_lots,entry_price::float8,pnl::float8,sl1_price::float8,sl2_price::float8,COALESCE(contract_symbol,''),strategy_key FROM trades WHERE id=$1").bind(trade_id).fetch_one(&state.db).await?;
                cancel_active_exits(state, order.user_id, trade_id).await?;
                let closed_quantity = order.quantity.min(trade.1);
                let remaining_quantity = trade.1 - closed_quantity;
                let closed_lots = order.lots.min(trade.2);
                let remaining_lots = (trade.2 - closed_lots).max(0);
                let closing_pnl = trade_pnl(
                    &trade.0,
                    trade.4,
                    fill,
                    runtime_pnl_units(&instrument, closed_quantity, snapshot.lot_size),
                );
                let pnl = trade.5 + closing_pnl;
                let reversal =
                    if order.role == "SL2" && remaining_quantity == 0 && trade.9 == STRATEGY_KEY {
                        sl2_reversal_plan(&trade.0, trade.3)
                    } else {
                        None
                    };
                let exit_reason = recorded_exit_reason(&trade.9, &order.role, &order.session_key);
                let reporting_quantity = if trade.9 == STRATEGY_KEY {
                    trade
                        .3
                        .saturating_mul(snapshot.lot_size.unwrap_or(1).max(1))
                } else {
                    trade.1
                };
                let mut fill_tx = state.db.begin().await?;
                if remaining_quantity == 0 {
                    sqlx::query("UPDATE trades SET status='closed',safety_status='CLOSED',quantity=$4,remaining_lots=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=($3::float8)::numeric,exit_datetime=NOW(),exit_reason=$5,updated_at=NOW() WHERE id=$1").bind(trade_id).bind(fill).bind(pnl).bind(reporting_quantity).bind(exit_reason).execute(&mut *fill_tx).await?;
                } else {
                    sqlx::query("UPDATE trades SET safety_status=CASE WHEN $6='EMERGENCY_CLOSE' THEN 'EMERGENCY_CLOSING' WHEN execution_mode='live' THEN 'PROTECTION_REQUIRED' ELSE safety_status END,protection_deadline_at=CASE WHEN execution_mode='live' THEN NOW()+($7::text || ' seconds')::interval ELSE protection_deadline_at END,quantity=$2,remaining_lots=$3,last_price=($4::float8)::numeric,pnl=($5::float8)::numeric,updated_at=NOW() WHERE id=$1").bind(trade_id).bind(remaining_quantity).bind(remaining_lots).bind(fill).bind(pnl).bind(&order.role).bind(state.config.protection_ack_timeout_seconds).execute(&mut *fill_tx).await?;
                }
                sqlx::query("UPDATE strategy_orders SET status=CASE WHEN $2<quantity THEN 'partially_filled' ELSE 'filled' END,processed_quantity=GREATEST(processed_quantity,$2),filled_quantity=GREATEST(filled_quantity,$2),updated_at=NOW() WHERE id=$1").bind(order.id).bind(cumulative_fill).execute(&mut *fill_tx).await?;
                if order.role == "EMERGENCY_CLOSE" && order.session_key.starts_with("mc-") {
                    sqlx::query("UPDATE manual_trade_close_intents SET status=CASE WHEN $2=0 THEN 'completed' ELSE 'partially_filled' END,strategy_order_id=$3,last_error='',completed_at=CASE WHEN $2=0 THEN NOW() ELSE completed_at END,updated_at=NOW() WHERE trade_id=$1")
                        .bind(trade_id).bind(remaining_quantity).bind(order.id).execute(&mut *fill_tx).await?;
                }
                if let Some(plan) = reversal {
                    sqlx::query(
                        "INSERT INTO strategy_reversal_intents
                         (source_trade_id,user_id,snapshot_id,instrument,source_direction,reversal_direction,lots,entry_price,order_session_key)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
                         ON CONFLICT (source_trade_id) DO NOTHING",
                    )
                    .bind(trade_id)
                    .bind(order.user_id)
                    .bind(snapshot.id)
                    .bind(&instrument)
                    .bind(&trade.0)
                    .bind(plan.direction)
                    .bind(plan.lots)
                    .bind(fill)
                    .bind(sl2_reversal_session(trade_id))
                    .execute(&mut *fill_tx)
                    .await?;
                }
                fill_tx.commit().await?;
                emit(
                    state,
                    Some(order.user_id),
                    &instrument,
                    "stop_loss_filled",
                    json!({"trade_id":trade_id,"role":order.role,"fill_price":fill,"filled_quantity":closed_quantity,"remaining_quantity":remaining_quantity,"pnl":pnl}),
                )
                .await;
                append_user_log(
                    state,
                    order.user_id,
                    &format!(
                        "STRATEGY {} FILLED {} @ {:.2} TOTAL P&L {:+.2}",
                        order.role,
                        contract_log_label(&instrument, Some(&trade.8)),
                        fill,
                        pnl
                    ),
                )
                .await;
                if let Some(plan) = reversal {
                    emit(
                        state,
                        Some(order.user_id),
                        &instrument,
                        "sl2_reversal_queued",
                        json!({
                            "source_trade_id":trade_id,
                            "source_direction":&trade.0,
                            "reversal_direction":plan.direction,
                            "lots":plan.lots,
                            "entry_price":fill
                        }),
                    )
                    .await;
                    append_user_log(
                        state,
                        order.user_id,
                        &format!(
                            "STRATEGY SL2 REVERSAL QUEUED {} {} -> {} {} lots @ MARKET",
                            contract_log_label(&instrument, Some(&trade.8)),
                            trade.0,
                            plan.direction,
                            plan.lots
                        ),
                    )
                    .await;
                    if let Err(error) = process_sl2_reversal_intent(state, trade_id).await {
                        tracing::warn!(%trade_id, %error, "immediate SL2 reversal submission failed");
                    }
                }
                if remaining_quantity > 0 && order.execution_mode == "demo" {
                    let runner = runner_for(state, order.user_id, &instrument).await?;
                    let (next_role, stop) = if order.role == "SL1" {
                        ("SL1", trade.6)
                    } else {
                        ("SL2", trade.7)
                    };
                    let stop = required_exit_level(stop, "remaining-position stop loss")?;
                    place_strategy_order(
                        state,
                        &runner,
                        &snapshot,
                        &order.session_key,
                        NewOrder {
                            role: next_role,
                            side: if trade.0 == "BUY" { "SELL" } else { "BUY" },
                            order_type: "STOPLOSS_LIMIT",
                            lots: remaining_lots.max(1),
                            price: stop,
                            trigger: Some(stop),
                            trade_id: Some(trade_id),
                            quantity: Some(remaining_quantity),
                        },
                    )
                    .await?;
                }
            }
            sqlx::query("UPDATE strategy_orders SET status=CASE WHEN $2<quantity THEN 'partially_filled' ELSE 'filled' END,updated_at=NOW() WHERE id=$1")
                .bind(order.id)
                .bind(cumulative_fill)
                .execute(&state.db)
                .await?;
        }
        _ => {}
    }
    sqlx::query("UPDATE strategy_orders SET processed_quantity=GREATEST(processed_quantity,$2),filled_quantity=GREATEST(filled_quantity,$2),state_version=state_version+1,updated_at=NOW() WHERE id=$1")
        .bind(order.id).bind(cumulative_fill).execute(&state.db).await?;
    Ok(())
}

pub async fn process_tick(
    state: &AppState,
    user_id: Uuid,
    exchange_segment: &str,
    token: &str,
    ltp: f64,
) -> AppResult<()> {
    if implausible_option_tick(state, exchange_segment, token, ltp).await? {
        return Ok(());
    }
    risk::record_tick(state, exchange_segment, token, ltp).await?;
    process_demo_tick(state, user_id, exchange_segment, token, ltp).await
}

async fn implausible_option_tick(
    state: &AppState,
    exchange_segment: &str,
    token: &str,
    ltp: f64,
) -> AppResult<bool> {
    let implausible: bool = sqlx::query_scalar(
        "WITH token_context AS (
            SELECT
                BOOL_OR(
                    s.contract_symbol ILIKE '%CE'
                    OR s.contract_symbol ILIKE '%PE'
                    OR s.instrument ILIKE '%\\_CE' ESCAPE '\\'
                    OR s.instrument ILIKE '%\\_PE' ESCAPE '\\'
                ) AS is_option,
                COALESCE(
                    MAX(GREATEST(COALESCE(t.entry_price::float8,0),COALESCE(o.price,0))*20.0)
                        FILTER (WHERE t.id IS NOT NULL OR o.id IS NOT NULL),
                    20000.0
                ) AS max_plausible_price
            FROM strategy_market_snapshots s
            LEFT JOIN trades t
                ON t.strategy_snapshot_id=s.id
               AND t.status='open'
            LEFT JOIN strategy_orders o
                ON o.snapshot_id=s.id
               AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
            WHERE s.exchange_segment=$1
              AND s.contract_token=$2
        )
        SELECT COALESCE(is_option,FALSE) AND $3::float8>max_plausible_price
        FROM token_context",
    )
    .bind(exchange_segment)
    .bind(token)
    .bind(ltp)
    .fetch_one(&state.db)
    .await?;
    if implausible {
        tracing::warn!(
            exchange_segment,
            token,
            ltp,
            "ignored implausible option market tick"
        );
    }
    Ok(implausible)
}

async fn process_demo_tick(
    state: &AppState,
    user_id: Uuid,
    exchange_segment: &str,
    token: &str,
    ltp: f64,
) -> AppResult<()> {
    sqlx::query("UPDATE trades t SET last_price=($4::float8)::numeric,updated_at=NOW() FROM strategy_market_snapshots s WHERE t.strategy_snapshot_id=s.id AND t.user_id=$1 AND t.status='open' AND s.exchange_segment=$2 AND s.contract_token=$3")
        .bind(user_id).bind(exchange_segment).bind(token).bind(ltp).execute(&state.db).await?;
    let orders:Vec<StoredOrder>=sqlx::query_as("SELECT o.id,o.user_id,o.snapshot_id,o.trade_id,o.session_key,o.role,o.side,o.order_type,o.execution_mode,o.lots,o.quantity,o.price,o.broker_order_id,o.client_order_id,o.status,o.filled_quantity,o.processed_quantity,o.average_fill_price::float8 FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.user_id=$1 AND o.execution_mode='demo' AND o.status='submitted' AND s.exchange_segment=$2 AND s.contract_token=$3 ORDER BY CASE WHEN o.role IN ('TARGET','SL1','SL2') THEN 0 ELSE 1 END,o.created_at")
        .bind(user_id).bind(exchange_segment).bind(token).fetch_all(&state.db).await?;
    for order in orders {
        let triggered = match (order.role.as_str(), order.side.as_str()) {
            ("BUY_ENTRY", _) => ltp >= order.price,
            ("SELL_ENTRY", _) => ltp <= order.price,
            ("TARGET", "SELL") => ltp >= order.price,
            ("TARGET", "BUY") => ltp <= order.price,
            ("SL1" | "SL2", "SELL") => ltp <= order.price,
            ("SL1" | "SL2", "BUY") => ltp >= order.price,
            _ => false,
        };
        if triggered {
            let still_submitted: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE id=$1 AND status='submitted')",
            )
            .bind(order.id)
            .fetch_one(&state.db)
            .await?;
            if still_submitted {
                complete_order(state, order, ltp).await?;
            }
        }
    }
    Ok(())
}

fn market_tick_is_newer(previous: Option<(i64, i64)>, timestamp_ms: i64, sequence: i64) -> bool {
    previous.is_none_or(|(previous_timestamp, previous_sequence)| {
        timestamp_ms > previous_timestamp
            || (timestamp_ms == previous_timestamp && sequence > previous_sequence)
    })
}

pub async fn process_tick_shared(
    state: &AppState,
    exchange_segment: &str,
    token: &str,
    ltp: f64,
    tick_at: DateTime<Utc>,
    sequence: i64,
) -> AppResult<()> {
    let tick_key = (exchange_segment.to_uppercase(), token.to_owned());
    let timestamp_ms = tick_at.timestamp_millis();
    {
        let mut sequences = state.strategy_tick_sequences.lock().await;
        if !market_tick_is_newer(sequences.get(&tick_key).copied(), timestamp_ms, sequence) {
            return Ok(());
        }
        sequences.insert(tick_key, (timestamp_ms, sequence));
    }
    if implausible_option_tick(state, exchange_segment, token, ltp).await? {
        return Ok(());
    }
    record_supertrend_index_tick(state, exchange_segment, token, ltp, tick_at).await;
    risk::record_tick(state, exchange_segment, token, ltp).await?;
    sqlx::query("UPDATE trades t SET last_price=($3::float8)::numeric,updated_at=NOW() FROM strategy_market_snapshots s WHERE t.strategy_snapshot_id=s.id AND t.status='open' AND s.exchange_segment=$1 AND s.contract_token=$2")
        .bind(exchange_segment)
        .bind(token)
        .bind(ltp)
        .execute(&state.db)
        .await?;
    let users: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT o.user_id FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.execution_mode='demo' AND o.status='submitted' AND s.exchange_segment=$1 AND s.contract_token=$2")
        .bind(exchange_segment).bind(token).fetch_all(&state.db).await?;
    for user in users {
        process_demo_tick(state, user, exchange_segment, token, ltp).await?;
    }
    Ok(())
}

pub async fn finish_kill_cancellations(
    state: &AppState,
    orders: Vec<(Uuid, Uuid, String, String, String, String)>,
) -> AppResult<()> {
    for (id, user_id, mode, broker_id, _role, order_type) in orders {
        if mode == "live" && !broker_id.is_empty() {
            let credentials = state.credentials.load(user_id).await?;
            if let Err(error) = angel::cancel_order(
                state,
                user_id,
                &credentials.api_key,
                &credentials.jwt_token,
                &broker_id,
                if order_type.starts_with("STOPLOSS") {
                    "STOPLOSS"
                } else {
                    "NORMAL"
                },
            )
            .await
            {
                sqlx::query("UPDATE strategy_orders SET status='submitted',broker_status=$2,updated_at=NOW() WHERE id=$1 AND status='cancelling'")
                    .bind(id).bind(format!("Kill-switch cancellation failed: {error}")).execute(&state.db).await?;
                operational_alert(state,Some(user_id),"","kill_switch_cancel_failed","error","An emergency entry cancellation was not confirmed by the broker; retry or review immediately.").await;
                continue;
            }
            sqlx::query("UPDATE strategy_orders SET broker_status='Kill-switch cancellation requested; awaiting broker reconciliation.',updated_at=NOW() WHERE id=$1 AND status='cancelling'")
                .bind(id)
                .execute(&state.db)
                .await?;
            continue;
        }
        sqlx::query("UPDATE strategy_orders SET status='cancelled',updated_at=NOW() WHERE id=$1 AND status='cancelling'")
            .bind(id).execute(&state.db).await?;
    }
    Ok(())
}

async fn enforce_entry_shutdowns(state: &AppState) -> AppResult<()> {
    let global_kill: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT enabled FROM risk_kill_switches WHERE user_id IS NULL),FALSE)",
    )
    .fetch_one(&state.db)
    .await?;
    if global_kill {
        let orders =
            risk::cancel_pending_entries(state, None, "Global kill switch is enabled").await?;
        finish_kill_cancellations(state, orders).await?;
    } else {
        let killed_users: Vec<Uuid> = sqlx::query_scalar(
            "SELECT user_id FROM risk_kill_switches WHERE user_id IS NOT NULL AND enabled=TRUE",
        )
        .fetch_all(&state.db)
        .await?;
        for user_id in killed_users {
            let orders =
                risk::cancel_pending_entries(state, Some(user_id), "User kill switch is enabled")
                    .await?;
            finish_kill_cancellations(state, orders).await?;
        }
    }
    let disabled: Vec<(Uuid, String)> = sqlx::query_as("SELECT DISTINCT o.user_id,s.strategy_key FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id LEFT JOIN user_strategy_activations a ON a.user_id=o.user_id AND a.strategy_key=s.strategy_key LEFT JOIN user_strategy_configs c ON c.user_id=o.user_id AND c.strategy_key=s.strategy_key AND c.instrument=CASE WHEN s.strategy_key=$1 THEN split_part(s.instrument,'_',1) ELSE s.instrument END WHERE o.role IN ('BUY_ENTRY','SELL_ENTRY') AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND (COALESCE(a.is_active,FALSE)=FALSE OR COALESCE(c.enabled,FALSE)=FALSE)")
        .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY).fetch_all(&state.db).await?;
    for (user_id, strategy_key) in disabled {
        cancel_pending_entries(state, user_id, &strategy_key).await?;
    }
    Ok(())
}

pub(crate) async fn emit_for(
    state: &AppState,
    strategy_key: &str,
    user_id: Option<Uuid>,
    instrument: &str,
    event_type: &str,
    payload: Value,
) {
    let envelope = json!({"type":event_type,"user_id":user_id,"strategy_key":strategy_key,"instrument":instrument,"payload":payload,"created_at":Utc::now()});
    if let Err(error)=sqlx::query("INSERT INTO strategy_events (user_id,strategy_key,instrument,event_type,payload) VALUES ($1,$2,$3,$4,$5)").bind(user_id).bind(strategy_key).bind(instrument).bind(event_type).bind(&payload).execute(&state.db).await { tracing::warn!(%error,"could not persist strategy event"); }
    let _ = state.strategy_events.send(envelope);
}

async fn emit(
    state: &AppState,
    user_id: Option<Uuid>,
    instrument: &str,
    event_type: &str,
    payload: Value,
) {
    emit_for(
        state,
        STRATEGY_KEY,
        user_id,
        instrument,
        event_type,
        payload,
    )
    .await;
}

pub async fn operational_alert(
    state: &AppState,
    user_id: Option<Uuid>,
    instrument: &str,
    code: &str,
    severity: &str,
    message: &str,
) {
    operational_alert_for(
        state,
        STRATEGY_KEY,
        user_id,
        instrument,
        code,
        severity,
        message,
    )
    .await;
}

async fn safety_alert_correlation(
    state: &AppState,
    strategy_key: &str,
    user_id: Option<Uuid>,
    instrument: &str,
) -> Value {
    let Some(user_id) = user_id else {
        return json!({});
    };
    let order: Option<(Uuid, Option<Uuid>, String, String, String)> = sqlx::query_as(
        "SELECT o.id,o.trade_id,o.client_order_id,o.broker_order_id,COALESCE(s.contract_token,'')
         FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
         WHERE o.user_id=$1 AND s.strategy_key=$2
           AND ($3='' OR s.instrument=$3 OR split_part(s.instrument,'_',1)=$3)
         ORDER BY o.updated_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(strategy_key)
    .bind(instrument)
    .fetch_optional(&state.db)
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, %user_id, %strategy_key, "could not load order correlation for alert");
        None
    });
    let intent: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT i.id,i.signal_id FROM strategy_execution_intents i
         WHERE i.user_id=$1 AND i.strategy_key=$2
           AND ($3='' OR i.instrument=$3)
         ORDER BY i.updated_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(strategy_key)
    .bind(instrument)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    let incident_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM broker_position_incidents
         WHERE user_id=$1 AND strategy_key=$2
           AND ($3='' OR instrument=$3 OR split_part(instrument,'_',1)=$3)
         ORDER BY last_detected_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(strategy_key)
    .bind(instrument)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    json!({
        "trade_id":order.as_ref().and_then(|value| value.1),
        "local_order_id":order.as_ref().map(|value| value.0),
        "client_order_id":order.as_ref().map(|value| value.2.as_str()).filter(|value| !value.is_empty()),
        "broker_order_id":order.as_ref().map(|value| value.3.as_str()).filter(|value| !value.is_empty()),
        "contract_token":order.as_ref().map(|value| value.4.as_str()).filter(|value| !value.is_empty()),
        "intent_id":intent.map(|value| value.0),
        "signal_id":intent.map(|value| value.1),
        "reconciliation_incident_id":incident_id
    })
}

pub async fn operational_alert_for(
    state: &AppState,
    strategy_key: &str,
    user_id: Option<Uuid>,
    instrument: &str,
    code: &str,
    severity: &str,
    message: &str,
) {
    let correlation = safety_alert_correlation(state, strategy_key, user_id, instrument).await;
    let payload =
        json!({"code":code,"severity":severity,"message":message,"correlation":correlation});
    match severity {
        "critical" | "error" => tracing::error!(
            %code,
            %severity,
            ?user_id,
            %strategy_key,
            %instrument,
            correlation = %correlation,
            "strategy operational alert"
        ),
        _ => tracing::warn!(
            %code,
            %severity,
            ?user_id,
            %strategy_key,
            %instrument,
            correlation = %correlation,
            "strategy operational alert"
        ),
    }
    let inserted: Result<Option<i64>, sqlx::Error> = sqlx::query_scalar("INSERT INTO strategy_events (user_id,strategy_key,instrument,event_type,payload) SELECT $1,$2,$3,'operational_alert',$4 WHERE NOT EXISTS (SELECT 1 FROM strategy_events WHERE user_id IS NOT DISTINCT FROM $1 AND strategy_key=$2 AND instrument=$3 AND event_type='operational_alert' AND payload->>'code'=$5 AND created_at>NOW()-INTERVAL '5 minutes') RETURNING id")
        .bind(user_id).bind(strategy_key).bind(instrument).bind(&payload).bind(code)
        .fetch_optional(&state.db).await;
    match inserted {
        Ok(Some(_)) => {
            let envelope = json!({"type":"operational_alert","user_id":user_id,"strategy_key":strategy_key,"instrument":instrument,"payload":payload,"created_at":Utc::now()});
            let _ = state.strategy_events.send(envelope);
            if let Err(error) = crate::alerts::deliver(
                state,
                code,
                severity,
                json!({"user_id":user_id,"strategy_key":strategy_key,"instrument":instrument,"message":message,"correlation":correlation}),
            )
            .await
            {
                tracing::warn!(%error, %code, "could not deliver operational alert");
            }
        }
        Ok(None) => {}
        Err(error) => tracing::warn!(%error, %code, "could not persist operational alert"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyQuery {
    pub instrument: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyUpdate {
    pub strategy_key: Option<String>,
    pub instrument: Option<String>,
    pub enabled: bool,
    pub lots: i32,
    pub run_day_session: Option<bool>,
    pub run_evening_session: Option<bool>,
    pub target_points: Option<f64>,
    pub stop_loss_points: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationUpdate {
    pub active: bool,
}

async fn activation_state(state: &AppState, user: Uuid) -> AppResult<bool> {
    activation_state_for(state, user, STRATEGY_KEY).await
}

pub(crate) async fn activation_state_for(
    state: &AppState,
    user: Uuid,
    strategy_key: &str,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT is_active FROM user_strategy_activations WHERE user_id=$1 AND strategy_key=$2",
    )
    .bind(user)
    .bind(strategy_key)
    .fetch_optional(&state.db)
    .await?
    .unwrap_or(false))
}

pub async fn catalog(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> AppResult<Json<Value>> {
    let user = user.id;
    let today = ist_now().date_naive();
    let active = activation_state(&state, user).await?;
    let supertrend_active =
        activation_state_for(&state, user, SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY).await?;
    let configs: Vec<(String, bool, i32, bool, bool)> = sqlx::query_as("SELECT instrument,enabled,lots,run_day_session,run_evening_session FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2")
        .bind(user).bind(STRATEGY_KEY).fetch_all(&state.db).await?;
    let supertrend_configs: Vec<(String, bool, i32, bool, bool, f64, f64)> = sqlx::query_as("SELECT instrument,enabled,lots,run_day_session,run_evening_session,target_points,stop_loss_points FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2")
        .bind(user).bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY).fetch_all(&state.db).await?;
    let snapshots = ensure_supported_contract_metadata(&state, today).await?;
    let option_contracts = load_contract_master(&state).await.ok();
    let shared_sessions = shared_market_session_count(&state).await.unwrap_or(0);
    let option_market_data = if shared_sessions > 0 {
        json!({
            "status":"connected",
            "connected_sessions":shared_sessions,
            "message":"Angel One market-data session is connected."
        })
    } else {
        json!({
            "status":"disconnected",
            "connected_sessions":0,
            "message":"Angel One market-data session is disconnected. Reconnect Angel One before SuperTrend can fetch index candles/options LTP or place live/demo entries."
        })
    };
    // The strategy card is a current-status surface, not an incident log. Keep the
    // complete event history in strategy_events/logs and return only the newest
    // recent alert here so resolved retries do not clutter the trading controls.
    let alerts: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'instrument',instrument,'severity',payload->>'severity','code',payload->>'code','message',payload->>'message','created_at',created_at) FROM strategy_events WHERE strategy_key=$1 AND event_type='operational_alert' AND (user_id=$2 OR user_id IS NULL) AND created_at>NOW()-INTERVAL '10 minutes' ORDER BY created_at DESC LIMIT 10")
        .bind(STRATEGY_KEY).bind(user).fetch_all(&state.db).await?;
    let alerts: Vec<Value> = alerts
        .into_iter()
        .filter(|alert| {
            alert
                .get("instrument")
                .and_then(Value::as_str)
                .is_none_or(|instrument| {
                    instrument.is_empty() || is_futures_breakout_instrument(instrument)
                })
        })
        .take(1)
        .collect();
    let runs: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('instrument',instrument,'session',session_key,'action',action,'status',status,'attempts',attempts,'scheduled_for',scheduled_for,'last_error',last_error,'updated_at',updated_at) FROM strategy_scheduler_runs WHERE strategy_key=$1 AND trade_date=$2 ORDER BY scheduled_for,action")
        .bind(STRATEGY_KEY).bind(ist_now().date_naive()).fetch_all(&state.db).await?;
    let runs: Vec<Value> = runs
        .into_iter()
        .filter(|run| {
            run.get("instrument")
                .and_then(Value::as_str)
                .is_some_and(is_futures_breakout_instrument)
        })
        .collect();
    let breakout_instruments: Vec<Value> = FUTURES_BREAKOUT_INSTRUMENTS
        .iter()
        .map(|instrument| {
            let config = configs
                .iter()
                .find(|config| config.0 == *instrument)
                .map(|config| (config.1, config.2, config.3, config.4))
                .unwrap_or((false, 1, true, true));
            json!({
                "instrument":instrument,
                "label":futures_breakout_label(instrument),
                "enabled":config.0,
                "lots":config.1,
                "run_day_session":config.2,
                "run_evening_session":config.3,
                "snapshot":snapshots.get(*instrument)
            })
        })
        .collect();
    let breakout = json!({
        "key":STRATEGY_KEY,
        "name":"Futures Breakout v3",
        "description":"Four-day MCX futures breakout with stop-and-reverse trade management.",
        "active":active,
        "operational_alerts":alerts,
        "scheduler_runs":runs,
        "instruments":breakout_instruments
    });
    let supertrend_alerts: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'instrument',instrument,'severity',payload->>'severity','code',payload->>'code','message',payload->>'message','created_at',created_at) FROM strategy_events WHERE strategy_key=$1 AND event_type='operational_alert' AND (user_id=$2 OR user_id IS NULL) AND created_at>NOW()-INTERVAL '10 minutes' ORDER BY created_at DESC LIMIT 1")
        .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY).bind(user).fetch_all(&state.db).await?;
    let supertrend_instruments: Vec<Value> = ["SENSEX", "NIFTY"]
        .into_iter()
        .filter_map(|instrument| {
            let config = index_option_config(instrument)?;
            let user_config = supertrend_configs
                .iter()
                .find(|item| item.0 == instrument)
                .map(|item| (item.1, item.2, item.3, item.4, item.5, item.6))
                .unwrap_or((
                    false,
                    1,
                    true,
                    false,
                    config.default_target_points,
                    config.default_stop_loss_points,
                ));
            let preview = option_contracts
                .as_ref()
                .and_then(|contracts| supertrend_option_expiry_preview(contracts, config, today));
            Some(json!({
                "instrument":instrument,
                "label":config.label,
                "enabled":user_config.0,
                "lots":user_config.1,
                "run_day_session":user_config.2,
                "run_evening_session":user_config.3,
                "target_points": if user_config.4 > 0.0 { user_config.4 } else { config.default_target_points },
                "stop_loss_points": if user_config.5 > 0.0 { user_config.5 } else { config.default_stop_loss_points },
                "parameters":{
                    "target_points": if user_config.4 > 0.0 { user_config.4 } else { config.default_target_points },
                    "stop_loss_points": if user_config.5 > 0.0 { user_config.5 } else { config.default_stop_loss_points },
                    "atr_period":SUPERTREND_ATR_PERIOD,
                    "factor":SUPERTREND_FACTOR,
                    "interval":OPTION_INTERVAL,
                    "contract_selection":"ATM"
                },
                "snapshot":{
                    "strategy_key":SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                    "instrument":instrument,
                    "status":"ready",
                    "execution_key":"catalog-preview",
                    "exchange_segment":config.option_exchange,
                    "product_type":OPTION_PRODUCT_TYPE,
                    "underlying_token":config.index_token,
                    "contract_expiry":preview.map(|value| value.0),
                    "lot_size":preview.map(|value| value.1),
                    "market_data":option_market_data
                }
            }))
        })
        .collect();
    let supertrend_strategy = json!({
        "key":SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
        "name":"SuperTrend Index Options v1",
        "description":"Intraday 5-minute SuperTrend flips on SENSEX/NIFTY closed candles, buying ATM CE/PE with user-defined TP and SL points.",
        "active":supertrend_active,
        "operational_alerts":supertrend_alerts,
        "scheduler_runs":[],
        "instruments":supertrend_instruments
    });
    Ok(Json(json!({"strategies":[breakout,supertrend_strategy]})))
}

async fn cancel_pending_entries(state: &AppState, user: Uuid, strategy_key: &str) -> AppResult<()> {
    let orders: Vec<(Uuid, String, String, String, String)> = sqlx::query_as("SELECT o.id,o.broker_order_id,o.execution_mode,o.status,o.order_type FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.user_id=$1 AND s.strategy_key=$2 AND o.role IN ('BUY_ENTRY','SELL_ENTRY') AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')")
        .bind(user).bind(strategy_key).fetch_all(&state.db).await?;
    let needs_credentials = orders.iter().any(|(_, broker_id, mode, status, _)| {
        mode == "live"
            && !broker_id.is_empty()
            && matches!(status.as_str(), "submitted" | "partially_filled")
    });
    let credentials = if needs_credentials {
        Some(state.credentials.load(user).await?)
    } else {
        None
    };
    for (id, broker_id, mode, status, order_type) in orders {
        if matches!(
            status.as_str(),
            "submitting" | "ambiguous" | "processing" | "cancelling"
        ) {
            sqlx::query("UPDATE strategy_orders SET broker_status='Strategy deactivated; this in-flight entry remains reconciliation-only and will not be retried.',updated_at=NOW() WHERE id=$1")
                .bind(id).execute(&state.db).await?;
            continue;
        }
        if mode == "live" && !broker_id.is_empty() {
            let credentials = credentials.as_ref().ok_or_else(|| {
                AppError::BadRequest(
                    "Broker credentials are required to cancel a live entry.".into(),
                )
            })?;
            if let Err(error) = angel::cancel_order(
                state,
                user,
                &credentials.api_key,
                &credentials.jwt_token,
                &broker_id,
                if order_type.starts_with("STOPLOSS") {
                    "STOPLOSS"
                } else {
                    "NORMAL"
                },
            )
            .await
            {
                tracing::warn!(%error, %broker_id, "could not cancel strategy entry while deactivating");
                continue;
            }
            sqlx::query("UPDATE strategy_orders SET status='cancelling',broker_status='Strategy deactivation cancellation requested',updated_at=NOW() WHERE id=$1 AND status IN ('submitted','partially_filled')")
                .bind(id).execute(&state.db).await?;
            continue;
        }
        sqlx::query("UPDATE strategy_orders SET status='cancelled',broker_status='Strategy deactivated',updated_at=NOW() WHERE id=$1 AND status IN ('pending','submitted','partially_filled')")
            .bind(id).execute(&state.db).await?;
    }
    Ok(())
}

pub async fn update_activation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Path(strategy_key): Path<String>,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
    Json(input): Json<ActivationUpdate>,
) -> AppResult<Json<Value>> {
    if !matches!(
        strategy_key.as_str(),
        STRATEGY_KEY | SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
    ) {
        return Err(AppError::NotFound("Strategy not found.".into()));
    }
    let user = auth.id;
    if !input.active {
        cancel_pending_entries(&state, user, &strategy_key).await?;
    }
    sqlx::query("INSERT INTO user_strategy_activations (user_id,strategy_key,is_active,activated_at,deactivated_at) VALUES ($1,$2,$3,CASE WHEN $3 THEN NOW() END,CASE WHEN $3 THEN NULL ELSE NOW() END) ON CONFLICT (user_id,strategy_key) DO UPDATE SET is_active=EXCLUDED.is_active,activated_at=CASE WHEN EXCLUDED.is_active THEN COALESCE(user_strategy_activations.activated_at,NOW()) ELSE user_strategy_activations.activated_at END,deactivated_at=CASE WHEN EXCLUDED.is_active THEN NULL ELSE NOW() END,updated_at=NOW()")
        .bind(user).bind(&strategy_key).bind(input.active).execute(&state.db).await?;
    emit_for(
        &state,
        &strategy_key,
        Some(user),
        "",
        if input.active {
            "strategy_activated"
        } else {
            "strategy_deactivated"
        },
        json!({"active":input.active}),
    )
    .await;
    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: request_context.as_ref(),
            headers: Some(&headers),
            event_type: "strategy_activation_changed",
            actor_user_id: Some(user),
            target_user_id: Some(user),
            summary: "User changed strategy activation",
            metadata: json!({"strategy_key":&strategy_key,"active":input.active}),
        },
    )
    .await
    {
        tracing::warn!(%error, "could not write strategy activation audit event");
    }
    catalog(State(state), Extension(auth)).await
}

pub async fn update(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
    Json(input): Json<StrategyUpdate>,
) -> AppResult<Json<Value>> {
    if input.lots <= 0 {
        return Err(AppError::BadRequest(
            "Lots must be a positive integer.".into(),
        ));
    }
    let user = auth.id;
    let strategy_key = input
        .strategy_key
        .unwrap_or_else(|| STRATEGY_KEY.into())
        .trim()
        .to_string();
    if !matches!(
        strategy_key.as_str(),
        STRATEGY_KEY | SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
    ) {
        return Err(AppError::NotFound("Strategy not found.".into()));
    }
    let instrument = input
        .instrument
        .unwrap_or_else(|| {
            if strategy_key == SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY {
                "SENSEX".into()
            } else {
                "GOLDTEN".into()
            }
        })
        .trim()
        .to_uppercase();
    if strategy_key == STRATEGY_KEY && !is_futures_breakout_instrument(&instrument) {
        return Err(AppError::BadRequest(format!(
            "Futures Breakout supports {}.",
            FUTURES_BREAKOUT_INSTRUMENTS.join(", ")
        )));
    }
    if strategy_key == SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
        && !is_supertrend_index_option_instrument(&instrument)
    {
        return Err(AppError::BadRequest(
            "SuperTrend Index Options supports SENSEX and NIFTY.".into(),
        ));
    }
    let default_points = index_option_config(&instrument);
    let target_points = input
        .target_points
        .or_else(|| default_points.map(|config| config.default_target_points))
        .unwrap_or(0.0);
    let stop_loss_points = input
        .stop_loss_points
        .or_else(|| default_points.map(|config| config.default_stop_loss_points))
        .unwrap_or(0.0);
    if strategy_key == SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
        && (!target_points.is_finite()
            || target_points <= 0.0
            || !stop_loss_points.is_finite()
            || stop_loss_points <= 0.0)
    {
        return Err(AppError::BadRequest(
            "TP and SL points must be positive numbers.".into(),
        ));
    }
    if input.enabled && !activation_state_for(&state, user, &strategy_key).await? {
        return Err(AppError::BadRequest(
            "Activate the strategy before enabling an instrument.".into(),
        ));
    }
    sqlx::query("INSERT INTO user_strategy_configs (user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session,target_points,stop_loss_points) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (user_id,strategy_key,instrument) DO UPDATE SET enabled=EXCLUDED.enabled,lots=EXCLUDED.lots,run_day_session=EXCLUDED.run_day_session,run_evening_session=EXCLUDED.run_evening_session,target_points=EXCLUDED.target_points,stop_loss_points=EXCLUDED.stop_loss_points,updated_at=NOW()")
        .bind(user).bind(&strategy_key).bind(&instrument).bind(input.enabled).bind(input.lots).bind(input.run_day_session.unwrap_or(true)).bind(input.run_evening_session.unwrap_or(strategy_key != SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)).bind(target_points).bind(stop_loss_points).execute(&state.db).await?;
    emit_for(
        &state,
        &strategy_key,
        Some(user),
        &instrument,
        "configuration_updated",
        json!({"enabled":input.enabled,"lots":input.lots,"target_points":target_points,"stop_loss_points":stop_loss_points}),
    )
    .await;
    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: request_context.as_ref(),
            headers: Some(&headers),
            event_type: "strategy_configuration_changed",
            actor_user_id: Some(user),
            target_user_id: Some(user),
            summary: "User changed strategy configuration",
            metadata: json!({"strategy_key":&strategy_key,"instrument":&instrument,"enabled":input.enabled,"lots":input.lots,"target_points":target_points,"stop_loss_points":stop_loss_points}),
        },
    )
    .await
    {
        tracing::warn!(%error, "could not write strategy configuration audit event");
    }
    catalog(State(state), Extension(auth)).await
}

pub async fn status(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<StrategyQuery>,
) -> AppResult<Json<Value>> {
    let user = user.id;
    let instrument = query
        .instrument
        .unwrap_or_else(|| "GOLDTEN".into())
        .to_uppercase();
    if !is_futures_breakout_instrument(&instrument) {
        return Err(AppError::BadRequest(format!(
            "Futures Breakout supports {}.",
            FUTURES_BREAKOUT_INSTRUMENTS.join(", ")
        )));
    }
    let config:Option<(bool,i32,bool,bool)>=sqlx::query_as("SELECT enabled,lots,run_day_session,run_evening_session FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2 AND instrument=$3").bind(user).bind(STRATEGY_KEY).bind(&instrument).fetch_optional(&state.db).await?;
    let strategy_active = activation_state(&state, user).await?;
    let snapshot = load_snapshot(&state, &instrument, ist_now().date_naive()).await?;
    let orders:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'role',role,'side',side,'status',status,'lots',lots,'quantity',quantity,'price',price,'trigger_price',trigger_price,'client_order_id',client_order_id,'broker_order_id',broker_order_id,'filled_quantity',filled_quantity,'average_fill_price',average_fill_price,'broker_error_class',broker_error_class,'broker_error_code',broker_error_code,'broker_http_status',broker_http_status,'last_reconciled_at',last_reconciled_at,'created_at',created_at) FROM strategy_orders WHERE user_id=$1 ORDER BY created_at DESC LIMIT 100").bind(user).fetch_all(&state.db).await?;
    let trades:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'status',status,'direction',direction,'lots',total_lots,'remaining_lots',remaining_lots,'quantity',quantity,'entry_price',entry_price,'exit_price',exit_price,'pnl',pnl,'trigger_time',entry_datetime,'exit_time',exit_datetime,'contract_symbol',contract_symbol,'target',target_price,'sl1',sl1_price,'sl2',sl2_price,'reversal_of_trade_id',reversal_of_trade_id) FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 ORDER BY created_at DESC LIMIT 100").bind(user).bind(STRATEGY_KEY).bind(&instrument).fetch_all(&state.db).await?;
    let alerts:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'instrument',instrument,'severity',payload->>'severity','code',payload->>'code','message',payload->>'message','created_at',created_at) FROM strategy_events WHERE strategy_key=$1 AND event_type='operational_alert' AND (user_id=$2 OR user_id IS NULL) AND created_at>NOW()-INTERVAL '24 hours' ORDER BY created_at DESC LIMIT 20").bind(STRATEGY_KEY).bind(user).fetch_all(&state.db).await?;
    Ok(Json(
        json!({"strategy_key":STRATEGY_KEY,"strategy_active":strategy_active,"instrument":instrument,"configuration":config.map(|v|json!({"enabled":v.0,"lots":v.1,"run_day_session":v.2,"run_evening_session":v.3})),"snapshot":snapshot,"orders":orders,"trades":trades,"operational_alerts":alerts}),
    ))
}

pub async fn events_upgrade(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    ws: WebSocketUpgrade,
) -> AppResult<Response> {
    Ok(ws.on_upgrade(move |socket| events_socket(socket, state, user.id)))
}
async fn events_socket(mut socket: WebSocket, state: AppState, user_id: Uuid) {
    let mut receiver = state.strategy_events.subscribe();
    let user_key = user_id.to_string();
    loop {
        tokio::select! {
            event=receiver.recv()=>match event {
                Ok(value)=>{
                    let target=value.get("user_id").and_then(Value::as_str);
                    if (target.is_none()||target==Some(user_key.as_str()))
                        && socket.send(Message::Text(value.to_string().into())).await.is_err() { break; }
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>continue,
                Err(_)=>break
            },
            incoming=socket.recv()=>match incoming {Some(Ok(Message::Ping(value)))=>{if socket.send(Message::Pong(value)).await.is_err(){break;}},Some(Ok(Message::Close(_)))|None=>break,_=>{}}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use std::{collections::VecDeque, path::Path, time::Duration as StdDuration};

    #[derive(Debug, Deserialize)]
    struct HistoricalReplayEvent {
        at: String,
        strategy: String,
        instrument: String,
        signal: String,
        price: f64,
        demo_users: usize,
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct HistoricalReplayCounts {
        futures_dispatched: usize,
        futures_evaluated: usize,
        futures_demo_path: usize,
        supertrend_dispatched: usize,
        supertrend_evaluated: usize,
        supertrend_demo_path: usize,
        duplicate_dispatches: usize,
    }

    fn historical_replay_events() -> Vec<HistoricalReplayEvent> {
        serde_json::from_str(include_str!("../tests/fixtures/sep_14_18_demo_replay.json"))
            .expect("the audited Sep 14-18 replay fixture must parse")
    }

    fn replay_scheduler_delivery(events: &[HistoricalReplayEvent]) -> HistoricalReplayCounts {
        let mut counts = HistoricalReplayCounts::default();
        let mut dispatched = HashSet::new();
        for _ in 0..2 {
            for event in events {
                let key = format!(
                    "{}:{}:{}:{}",
                    event.at, event.strategy, event.instrument, event.signal
                );
                if !dispatched.insert(key) {
                    // A repeated scheduler observation is suppressed before
                    // evaluation; it is not counted as a duplicate dispatch.
                    continue;
                }
                let target = if event.strategy == STRATEGY_KEY {
                    (
                        &mut counts.futures_dispatched,
                        &mut counts.futures_evaluated,
                        &mut counts.futures_demo_path,
                    )
                } else {
                    assert_eq!(event.strategy, SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY);
                    (
                        &mut counts.supertrend_dispatched,
                        &mut counts.supertrend_evaluated,
                        &mut counts.supertrend_demo_path,
                    )
                };
                *target.0 += event.demo_users;
                *target.1 += event.demo_users;
                *target.2 += event.demo_users;
            }
        }
        counts
    }

    fn isolated_test_database_url() -> String {
        let raw = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must point to an explicitly disposable local database");
        let parsed =
            url::Url::parse(&raw).expect("TEST_DATABASE_URL must be a valid PostgreSQL URL");
        assert!(
            matches!(parsed.scheme(), "postgres" | "postgresql"),
            "TEST_DATABASE_URL must use PostgreSQL"
        );
        assert!(
            matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1")),
            "stateful safety tests refuse non-loopback databases"
        );
        let database = parsed.path().trim_start_matches('/');
        assert!(
            database.starts_with("rulenix_test_") && database.len() > "rulenix_test_".len(),
            "stateful safety tests require a database named rulenix_test_*"
        );
        raw
    }

    fn database_url_for_name(base: &str, database: &str) -> String {
        assert!(
            database == "postgres"
                || (database.starts_with("rulenix_test_")
                    && database.chars().all(|value| value.is_ascii_lowercase()
                        || value.is_ascii_digit()
                        || value == '_')),
            "generated database name must stay in the disposable namespace"
        );
        let mut parsed = url::Url::parse(base).expect("base test database URL must parse");
        parsed.set_path(&format!("/{database}"));
        parsed.to_string()
    }

    async fn apply_raw_migrations_through(
        db: &sqlx::PgPool,
        inclusive_file_name: &str,
    ) -> Result<(), sqlx::Error> {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut paths = std::fs::read_dir(directory)
            .expect("migration directory must be readable")
            .map(|entry| entry.expect("migration entry must be readable").path())
            .filter(|path| path.extension().is_some_and(|value| value == "sql"))
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let file_name = path
                .file_name()
                .and_then(|value| value.to_str())
                .expect("migration file name must be UTF-8");
            if file_name > inclusive_file_name {
                break;
            }
            let sql = std::fs::read_to_string(&path).expect("migration SQL must be readable");
            let mut transaction = db.begin().await?;
            sqlx::raw_sql(&sql).execute(&mut *transaction).await?;
            transaction.commit().await?;
        }
        Ok(())
    }

    async fn apply_raw_migration_file(
        db: &sqlx::PgPool,
        file_name: &str,
    ) -> Result<(), sqlx::Error> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("migrations")
            .join(file_name);
        let sql = std::fs::read_to_string(path).expect("migration SQL must be readable");
        let mut transaction = db.begin().await?;
        sqlx::raw_sql(&sql).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn isolated_test_state() -> AppState {
        isolated_test_state_with_broker("https://127.0.0.1:9").await
    }

    async fn isolated_test_state_with_broker(angel_api_base: &str) -> AppState {
        let database_url = isolated_test_database_url();
        let db = PgPoolOptions::new()
            .max_connections(16)
            .connect(&database_url)
            .await
            .expect("disposable PostgreSQL must be reachable");
        let migrations = sqlx::migrate::Migrator::new(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations"
        )))
        .await
        .expect("migration directory must load");
        migrations
            .run(&db)
            .await
            .expect("all migrations must apply to the disposable database");
        sqlx::query(
            "DO $$
             DECLARE tables TEXT;
             BEGIN
               SELECT string_agg(format('%I',tablename), ', ')
               INTO tables
               FROM pg_tables
               WHERE schemaname='public' AND tablename<>'_sqlx_migrations';
               IF tables IS NOT NULL THEN
                 EXECUTE 'TRUNCATE TABLE ' || tables || ' RESTART IDENTITY CASCADE';
               END IF;
             END $$",
        )
        .execute(&db)
        .await
        .expect("disposable test data reset must succeed");
        sqlx::query("INSERT INTO risk_limits(user_id,max_lots,max_quantity,max_notional,max_open_positions,max_trades_per_day,max_daily_realized_loss,max_daily_unrealized_loss,max_price_age_seconds) VALUES(NULL,20,10000,100000000,20,100,1000000,1000000,30)")
            .execute(&db)
            .await
            .expect("global test risk limits must be restored");
        sqlx::query("INSERT INTO risk_kill_switches(user_id,enabled,reason) VALUES(NULL,FALSE,'')")
            .execute(&db)
            .await
            .expect("global test kill switch must be restored");
        sqlx::query("INSERT INTO live_mutation_authority(singleton,holder,epoch,lease_owner,lease_expires_at,updated_by) VALUES(TRUE,'rust',1,'267961f9-6037-580b-906f-152939952a73'::uuid,'infinity'::timestamptz,'isolated Rust test authority')")
            .execute(&db)
            .await
            .expect("isolated Rust test mutation authority must be restored");
        let (strategy_events, _) = tokio::sync::broadcast::channel(64);
        AppState {
            http: reqwest::Client::builder()
                .timeout(StdDuration::from_millis(250))
                .build()
                .expect("test HTTP client must build"),
            config: crate::config::Config::for_isolated_test(&database_url, angel_api_base),
            strategy_events,
            strategy_feeds: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            strategy_feed_tokens: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            live_index_candles: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            strategy_tick_sequences: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            session_checks: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            angel_api_cooldowns: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            angel_request_history: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            shared_historical_cooldowns: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            shared_market_cursor: Arc::new(tokio::sync::Mutex::new(0)),
            scheduler_health: Default::default(),
            strategy_execution_permits: Arc::new(tokio::sync::Semaphore::new(8)),
            credentials: crate::credentials::CredentialStore::for_isolated_test(db.clone()),
            abuse_prevention: crate::security::AbusePrevention::default(),
            db,
        }
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn sep_14_18_replay_persists_66_demo_intents_exactly_once() {
        let state = isolated_test_state().await;
        let users = [Uuid::new_v4(), Uuid::new_v4()];
        for (index, user_id) in users.iter().copied().enumerate() {
            sqlx::query(
                "INSERT INTO users(id,username,email,password_hash) VALUES($1,$2,$3,'test-only')",
            )
            .bind(user_id)
            .bind(format!("historical-demo-{index}"))
            .bind(format!("historical-demo-{index}@example.test"))
            .execute(&state.db)
            .await
            .unwrap();
            sqlx::query("INSERT INTO user_profiles(user_id,trading_mode) VALUES($1,'demo')")
                .bind(user_id)
                .execute(&state.db)
                .await
                .unwrap();
        }

        let events = historical_replay_events();
        let mut snapshots = HashMap::new();
        for (index, event) in events.iter().enumerate() {
            let at = DateTime::parse_from_rfc3339(&event.at).unwrap();
            let trade_date = at.date_naive();
            let snapshot_key = (event.strategy.clone(), event.instrument.clone(), trade_date);
            let snapshot_id = if let Some(id) = snapshots.get(&snapshot_key) {
                *id
            } else {
                let id = Uuid::new_v4();
                sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,contract_token,contract_symbol,lot_size,exchange_segment,product_type,execution_key) VALUES($1,$2,$3,$4,'ready',$5,$3,1,'TEST','INTRADAY',$6)")
                    .bind(id)
                    .bind(&event.strategy)
                    .bind(&event.instrument)
                    .bind(trade_date)
                    .bind(format!("replay-token-{index}"))
                    .bind(format!("historical-replay-{index}"))
                    .execute(&state.db)
                    .await
                    .unwrap();
                snapshots.insert(snapshot_key, id);
                id
            };
            let session_key = format!("replay-{index}-{}", at.format("%Y%m%d%H%M"));
            let side: &'static str = if event.signal == "SELL" {
                "SELL"
            } else {
                "BUY"
            };
            let role: &'static str = if side == "SELL" {
                "SELL_ENTRY"
            } else {
                "BUY_ENTRY"
            };
            let order_type: &'static str = if event.strategy == STRATEGY_KEY {
                "STOPLOSS_LIMIT"
            } else {
                "MARKET"
            };
            let intents = users
                .iter()
                .take(event.demo_users)
                .map(|user_id| PreparedExecutionIntent {
                    user_id: *user_id,
                    snapshot_id,
                    strategy_key: event.strategy.clone(),
                    instrument: event.instrument.clone(),
                    session_key: session_key.clone(),
                    action: "ENTRY",
                    role,
                    side,
                    order_type,
                    lots: 1,
                    quantity: None,
                    price: event.price,
                    trigger_price: (order_type == "STOPLOSS_LIMIT").then_some(event.price),
                    trade_id: None,
                    expires_at: None,
                })
                .collect::<Vec<_>>();
            let (_, inserted) = materialize_signal_intents(
                &state,
                &event.strategy,
                &event.instrument,
                &session_key,
                role,
                at.with_timezone(&Utc),
                Some(snapshot_id),
                json!({"historical_replay":true,"source_at":event.at}),
                &intents,
            )
            .await
            .unwrap();
            assert!(inserted);
            let (_, duplicate_inserted) = materialize_signal_intents(
                &state,
                &event.strategy,
                &event.instrument,
                &session_key,
                role,
                at.with_timezone(&Utc),
                Some(snapshot_id),
                json!({"historical_replay":true,"source_at":event.at}),
                &intents,
            )
            .await
            .unwrap();
            assert!(!duplicate_inserted);
        }

        let counts: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT s.strategy_key,COUNT(DISTINCT s.id),COUNT(i.id)
             FROM strategy_signals s JOIN strategy_execution_intents i ON i.signal_id=s.id
             GROUP BY s.strategy_key ORDER BY s.strategy_key",
        )
        .fetch_all(&state.db)
        .await
        .unwrap();
        assert_eq!(
            counts,
            vec![
                (STRATEGY_KEY.into(), 6, 8),
                (SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY.into(), 29, 58),
            ]
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn scheduler_advisory_leadership_is_single_and_reacquired_after_connection_loss() {
        let state = isolated_test_state().await;
        let mut first = state.db.acquire().await.unwrap();
        first.close_on_drop();
        let first_acquired: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_lock(hashtext('rulenix:test:strategy_scheduler'))",
        )
        .fetch_one(&mut *first)
        .await
        .unwrap();
        assert!(first_acquired);

        let mut second = state.db.acquire().await.unwrap();
        second.close_on_drop();
        let duplicate_acquired: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_lock(hashtext('rulenix:test:strategy_scheduler'))",
        )
        .fetch_one(&mut *second)
        .await
        .unwrap();
        assert!(!duplicate_acquired, "a second instance must remain standby");

        first.close().await.unwrap();
        let reacquired: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_lock(hashtext('rulenix:test:strategy_scheduler'))",
        )
        .fetch_one(&mut *second)
        .await
        .unwrap();
        assert!(
            reacquired,
            "standby must acquire leadership after leader loss"
        );
        second.close().await.unwrap();
    }

    #[derive(Clone, Default)]
    struct DeterministicFakeBroker {
        placed_orders: Arc<tokio::sync::Mutex<Vec<Value>>>,
        order_book: Arc<tokio::sync::Mutex<Vec<Value>>>,
        trade_book: Arc<tokio::sync::Mutex<Vec<Value>>>,
        conditional_rules: Arc<tokio::sync::Mutex<Vec<Value>>>,
        positions: Arc<tokio::sync::Mutex<Vec<Value>>>,
        cancelled_orders: Arc<tokio::sync::Mutex<Vec<String>>>,
        place_modes: Arc<tokio::sync::Mutex<VecDeque<FakePlaceMode>>>,
        cancel_modes: Arc<tokio::sync::Mutex<VecDeque<FakeCancelMode>>>,
        quote_ltps: Arc<tokio::sync::Mutex<HashMap<String, f64>>>,
        quote_unavailable: Arc<tokio::sync::Mutex<bool>>,
        conditional_unavailable: Arc<tokio::sync::Mutex<bool>>,
        order_book_unavailable: Arc<tokio::sync::Mutex<bool>>,
        order_book_timeout: Arc<tokio::sync::Mutex<bool>>,
        positions_unavailable: Arc<tokio::sync::Mutex<bool>>,
        trade_book_unavailable: Arc<tokio::sync::Mutex<bool>>,
    }

    #[derive(Clone, Copy)]
    #[allow(dead_code)]
    enum FakePlaceMode {
        Accept,
        AcceptWithoutOrderId,
        Reject,
        TimeoutBeforeAcceptance,
        TimeoutAfterAcceptance,
        AuthenticationFailure,
        RateLimit,
        BrokerUnavailable,
    }

    #[derive(Clone, Copy)]
    #[allow(dead_code)]
    enum FakeCancelMode {
        Accept,
        Reject,
        ResponseLost,
    }

    async fn fake_place_order(
        State(fake): State<DeterministicFakeBroker>,
        Json(body): Json<Value>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        let mode = fake
            .place_modes
            .lock()
            .await
            .pop_front()
            .unwrap_or(FakePlaceMode::Accept);
        if matches!(mode, FakePlaceMode::TimeoutBeforeAcceptance) {
            tokio::time::sleep(StdDuration::from_millis(600)).await;
            return (
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                Json(
                    json!({"status":false,"message":"test timeout before acceptance","data":null}),
                ),
            );
        }
        if matches!(mode, FakePlaceMode::Reject) {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(
                    json!({"status":false,"message":"test rejection","errorcode":"TEST_REJECT","data":null}),
                ),
            );
        }
        if matches!(mode, FakePlaceMode::AuthenticationFailure) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(
                    json!({"status":false,"message":"Invalid Token","errorcode":"AG8001","data":null}),
                ),
            );
        }
        if matches!(mode, FakePlaceMode::RateLimit) {
            return (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                Json(
                    json!({"status":false,"message":"Too many requests","errorcode":"RATE_LIMIT","data":null}),
                ),
            );
        }
        if matches!(mode, FakePlaceMode::BrokerUnavailable) {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    json!({"status":false,"message":"Broker unavailable","errorcode":"SERVICE_UNAVAILABLE","data":null}),
                ),
            );
        }
        let mut placed = fake.placed_orders.lock().await;
        placed.push(body.clone());
        let order_id = format!("FAKE-{}", placed.len());
        fake.order_book.lock().await.push(json!({
            "orderid":order_id,
            "ordertag":body.get("ordertag").cloned().unwrap_or(Value::Null),
            "status":"open",
            "filledshares":"0",
            "averageprice":"0"
        }));
        if matches!(mode, FakePlaceMode::TimeoutAfterAcceptance) {
            tokio::time::sleep(StdDuration::from_millis(600)).await;
        }
        let data = if matches!(mode, FakePlaceMode::AcceptWithoutOrderId) {
            json!({})
        } else {
            json!({"orderid":order_id})
        };
        (
            axum::http::StatusCode::OK,
            Json(json!({"status":true,"message":"SUCCESS","data":data})),
        )
    }

    async fn fake_order_book(
        State(fake): State<DeterministicFakeBroker>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        if *fake.order_book_timeout.lock().await {
            tokio::time::sleep(StdDuration::from_millis(600)).await;
        }
        if *fake.order_book_unavailable.lock().await {
            return (
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                Json(json!({"status":false,"message":"order book unavailable","data":null})),
            );
        }
        (
            axum::http::StatusCode::OK,
            Json(json!({
                "status":true,
                "message":"SUCCESS",
                "data":fake.order_book.lock().await.clone()
            })),
        )
    }

    async fn fake_positions(
        State(fake): State<DeterministicFakeBroker>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        if *fake.positions_unavailable.lock().await {
            return (
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                Json(json!({"status":false,"message":"positions unavailable","data":null})),
            );
        }
        (
            axum::http::StatusCode::OK,
            Json(json!({
                "status":true,
                "message":"SUCCESS",
                "data":fake.positions.lock().await.clone()
            })),
        )
    }

    async fn fake_trade_book(
        State(fake): State<DeterministicFakeBroker>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        if *fake.trade_book_unavailable.lock().await {
            return (
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                Json(json!({"status":false,"message":"trade book unavailable","data":null})),
            );
        }
        (
            axum::http::StatusCode::OK,
            Json(json!({
                "status":true,
                "message":"SUCCESS",
                "data":fake.trade_book.lock().await.clone()
            })),
        )
    }

    async fn fake_conditional_rules(
        State(fake): State<DeterministicFakeBroker>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        if *fake.conditional_unavailable.lock().await {
            tokio::time::sleep(StdDuration::from_millis(600)).await;
            return (
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                Json(json!({"status":false,"message":"conditional read timed out","data":null})),
            );
        }
        (
            axum::http::StatusCode::OK,
            Json(json!({
                "status":true,
                "message":"SUCCESS",
                "data":fake.conditional_rules.lock().await.clone()
            })),
        )
    }

    async fn fake_rms_limits() -> Json<Value> {
        Json(json!({
            "status":true,"message":"SUCCESS",
            "data":{"availablecash":"10000000","net":"10000000","availablelimitmargin":"0"}
        }))
    }

    async fn fake_margin_required() -> Json<Value> {
        Json(json!({
            "status":true,"message":"SUCCESS","data":{"totalMarginRequired":"1000"}
        }))
    }

    async fn fake_market_quote(
        State(fake): State<DeterministicFakeBroker>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        if *fake.quote_unavailable.lock().await {
            return Json(json!({"status":true,"message":"SUCCESS","data":{"fetched":[]}}));
        }
        let requested: Vec<String> = body
            .get("exchangeTokens")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|map| map.values())
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let configured = fake.quote_ltps.lock().await;
        let fetched: Vec<Value> = requested
            .iter()
            .map(|token| {
                json!({
                    "symbolToken":token,
                    "ltp":configured.get(token).copied().unwrap_or(100.0),
                    "lowerCircuit":0.05,
                    "upperCircuit":1_000_000.0
                })
            })
            .collect();
        Json(json!({"status":true,"message":"SUCCESS","data":{"fetched":fetched}}))
    }

    async fn fake_cancel_order(
        State(fake): State<DeterministicFakeBroker>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mode = fake
            .cancel_modes
            .lock()
            .await
            .pop_front()
            .unwrap_or(FakeCancelMode::Accept);
        if matches!(mode, FakeCancelMode::ResponseLost) {
            tokio::time::sleep(StdDuration::from_millis(600)).await;
        }
        if matches!(mode, FakeCancelMode::Reject) {
            return Json(
                json!({"status":false,"message":"test cancel rejection","errorcode":"TEST_CANCEL_REJECT","data":null}),
            );
        }
        if let Some(order_id) = body.get("orderid").and_then(Value::as_str) {
            fake.cancelled_orders.lock().await.push(order_id.to_owned());
        }
        Json(json!({"status":true,"message":"SUCCESS","data":{"orderid":body.get("orderid")}}))
    }

    async fn spawn_deterministic_fake_broker()
    -> (String, DeterministicFakeBroker, tokio::task::JoinHandle<()>) {
        use axum::routing::{get, post};
        let fake = DeterministicFakeBroker::default();
        let app = axum::Router::new()
            .route(
                "/rest/secure/angelbroking/order/v1/placeOrder",
                post(fake_place_order),
            )
            .route(
                "/rest/secure/angelbroking/order/v1/getOrderBook",
                get(fake_order_book),
            )
            .route(
                "/rest/secure/angelbroking/order/v1/getPosition",
                get(fake_positions),
            )
            .route(
                "/rest/secure/angelbroking/order/v1/getTradeBook",
                get(fake_trade_book),
            )
            .route(
                "/rest/secure/angelbroking/gtt/v1/ruleList",
                post(fake_conditional_rules),
            )
            .route(
                "/rest/secure/angelbroking/order/v1/cancelOrder",
                post(fake_cancel_order),
            )
            .route(
                "/rest/secure/angelbroking/market/v1/quote",
                post(fake_market_quote),
            )
            .route(
                "/rest/secure/angelbroking/user/v1/getRMS",
                get(fake_rms_limits),
            )
            .route(
                "/rest/secure/angelbroking/margin/v1/batch",
                post(fake_margin_required),
            )
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake broker listener must bind");
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("fake broker server must run");
        });
        (format!("http://{address}"), fake, task)
    }

    async fn seed_live_futures_protection_fixture(
        state: &AppState,
        prefix: &str,
    ) -> (Uuid, Uuid, Uuid, String, String) {
        let user_id = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        let trade_id = Uuid::new_v4();
        let token = format!("{prefix}-token");
        let symbol = format!("GOLDTEN-{prefix}-FUT");
        sqlx::query("INSERT INTO users(id,username,email,password_hash,can_live_trade) VALUES($1,$2,$3,'test-only',TRUE)")
            .bind(user_id)
            .bind(format!("{prefix}-user"))
            .bind(format!("{prefix}@example.test"))
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO user_profiles(user_id,trading_mode,last_token_status) VALUES($1,'live','success')")
            .bind(user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session) VALUES($1,$2,'GOLDTEN',TRUE,5,TRUE,TRUE)")
            .bind(user_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        state
            .credentials
            .put(
                user_id,
                &[("api_key", "fake-api"), ("jwt_token", "fake-jwt")],
            )
            .await
            .unwrap();
        sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,candle_dates,highs,lows,hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2,previous_close) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready','',$3,$4,CURRENT_DATE+30,10,'MCX','CARRYFORWARD',$5,'',ARRAY[]::date[],ARRAY[110.0,112.0,118.0,120.0],ARRAY[80.0,82.0,88.0,90.0],120.0,80.0,120.0,80.0,120.144,121.94616,98.5,98.0,79.904,78.70544,101.5,102.0,100.0)")
            .bind(snapshot_id).bind(STRATEGY_KEY).bind(&token).bind(&symbol).bind(prefix)
            .execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,sl2_price,safety_status,protection_deadline_at,broker_net_quantity,broker_average_price,last_position_reconciled_at) VALUES($1,$2,'live','open','BUY',50,100,100,0,NOW(),'GOLDTEN',$3,$4,$5,$6,5,5,101.5,98.5,98.0,'PROTECTION_REQUIRED',NOW()+INTERVAL '5 minutes',50,100,NOW())")
            .bind(trade_id).bind(user_id).bind(&symbol).bind(format!("test {prefix} protection")).bind(STRATEGY_KEY).bind(snapshot_id)
            .execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,status,broker_order_id,broker_status,idempotency_key,client_order_id,filled_quantity,processed_quantity,average_fill_price) VALUES($1,$2,$3,$4,$5,'BUY_ENTRY','BUY','MARKET','live',5,50,100,'filled','ENTRY-1','',$6,$7,50,50,100)")
            .bind(Uuid::new_v4()).bind(user_id).bind(snapshot_id).bind(trade_id)
            .bind(format!("{prefix}-entry")).bind(format!("{prefix}-entry-{trade_id}"))
            .bind(format!("{prefix}-entry-client")).execute(&state.db).await.unwrap();
        crate::contract_master::set_isolated_test_cache(vec![MasterContract {
            token: token.clone(),
            symbol: symbol.clone(),
            name: "GOLDTEN".into(),
            expiry: "30SEP2026".into(),
            strike: "0".into(),
            lotsize: "10".into(),
            tick_size: "5.000000".into(),
            instrumenttype: "FUTCOM".into(),
            exch_seg: "MCX".into(),
        }])
        .await;
        (user_id, snapshot_id, trade_id, token, symbol)
    }

    async fn seed_flat_linked_live_account(state: &AppState, prefix: &str) -> Uuid {
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,username,email,password_hash,can_live_trade) VALUES($1,$2,$3,'test-only',TRUE)")
            .bind(user_id)
            .bind(format!("{prefix}-user"))
            .bind(format!("{prefix}@example.test"))
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO user_profiles(user_id,trading_mode,last_token_status,token_state,broker_credential_revision) VALUES($1,'live','success','connected',7)")
            .bind(user_id)
            .execute(&state.db)
            .await
            .unwrap();
        state
            .credentials
            .put(
                user_id,
                &[("api_key", "fake-api"), ("jwt_token", "fake-jwt")],
            )
            .await
            .unwrap();
        user_id
    }

    async fn seed_live_supertrend_square_off_fixture(
        state: &AppState,
    ) -> (Uuid, Uuid, Uuid, Uuid, String, String) {
        let user_id = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        let trade_id = Uuid::new_v4();
        let stop_id = Uuid::new_v4();
        let token = "squareoff-token".to_string();
        let symbol = "NIFTY30SEP26ATMCE".to_string();
        sqlx::query("INSERT INTO users(id,username,email,password_hash,can_live_trade) VALUES($1,'squareoff-user','squareoff@example.test','test-only',TRUE)")
            .bind(user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_profiles(user_id,trading_mode,last_token_status) VALUES($1,'live','success')")
            .bind(user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session,target_points,stop_loss_points) VALUES($1,$2,'NIFTY',TRUE,1,TRUE,FALSE,20,10)")
            .bind(user_id).bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY).execute(&state.db).await.unwrap();
        state
            .credentials
            .put(
                user_id,
                &[("api_key", "fake-api"), ("jwt_token", "fake-jwt")],
            )
            .await
            .unwrap();
        sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,buy_target,buy_sl1) VALUES($1,$2,'NIFTY_CE',CURRENT_DATE,'ready','',$3,$4,CURRENT_DATE+30,25,'NFO','INTRADAY','squareoff-execution','nifty-index-token',20,10)")
            .bind(snapshot_id).bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY).bind(&token).bind(&symbol)
            .execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,safety_status,broker_net_quantity,broker_average_price,last_position_reconciled_at) VALUES($1,$2,'live','open','BUY',25,100,100,0,NOW(),'NIFTY_CE',$3,'test square-off',$4,$5,1,1,120,90,'PROTECTED',25,100,NOW())")
            .bind(trade_id).bind(user_id).bind(&symbol).bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY).bind(snapshot_id)
            .execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,trigger_price,status,broker_order_id,broker_status,idempotency_key,client_order_id,filled_quantity,processed_quantity,average_fill_price,last_reconciled_at) VALUES($1,$2,$3,$4,'squareoff-protection','SL1','SELL','STOPLOSS_MARKET','live',1,25,90,90,'submitted','SL-BROKER-1','',$5,'SL-CLIENT-1',0,0,NULL,NOW())")
            .bind(stop_id).bind(user_id).bind(snapshot_id).bind(trade_id).bind(format!("squareoff-stop-{trade_id}"))
            .execute(&state.db).await.unwrap();
        crate::contract_master::set_isolated_test_cache(vec![MasterContract {
            token: token.clone(),
            symbol: symbol.clone(),
            name: "NIFTY".into(),
            expiry: "30SEP2026".into(),
            strike: "2500000".into(),
            lotsize: "25".into(),
            tick_size: "5.000000".into(),
            instrumenttype: "OPTIDX".into(),
            exch_seg: "NFO".into(),
        }])
        .await;
        (user_id, snapshot_id, trade_id, stop_id, token, symbol)
    }

    async fn seed_concurrent_futures_fill(
        state: &AppState,
        iteration: usize,
    ) -> (StoredOrder, StoredOrder) {
        let user_id = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id,username,email,password_hash)
             VALUES ($1,$2,$3,'test-only')",
        )
        .bind(user_id)
        .bind(format!("race-user-{iteration}"))
        .bind(format!("race-{iteration}@example.test"))
        .execute(&state.db)
        .await
        .expect("test user insert must succeed");
        sqlx::query(
            "INSERT INTO strategy_market_snapshots
             (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,
              contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,
              candle_dates,highs,lows,hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,
              sell_entry,sell_target,sell_sl1,sell_sl2,previous_close)
             VALUES
             ($1,$2,'GOLDTEN',CURRENT_DATE,'ready','', 'race-token','GOLDTEN-RACE',
              CURRENT_DATE+30,10,'MCX','CARRYFORWARD',$3,'',
              ARRAY[]::date[],ARRAY[110.0,112.0,118.0,120.0],ARRAY[80.0,82.0,88.0,90.0],
              120.0,80.0,120.0,80.0,120.144,121.94616,118.34184,118.34184,
              79.904,78.70544,81.10256,81.10256,100.0)",
        )
        .bind(snapshot_id)
        .bind(STRATEGY_KEY)
        .bind(format!("race-{iteration}"))
        .execute(&state.db)
        .await
        .expect("test snapshot insert must succeed");

        let build_order = |role: &str, side: &str, price: f64| StoredOrder {
            id: Uuid::new_v4(),
            user_id,
            snapshot_id,
            trade_id: None,
            session_key: format!("race-{iteration}-{role}"),
            role: role.into(),
            side: side.into(),
            order_type: "STOPLOSS_LIMIT".into(),
            execution_mode: "live".into(),
            lots: 1,
            quantity: 10,
            price,
            broker_order_id: format!("broker-{iteration}-{role}"),
            client_order_id: format!("client-{iteration}-{role}"),
            status: "processing".into(),
            filled_quantity: 10,
            processed_quantity: 0,
            average_fill_price: Some(price),
        };
        let buy = build_order("BUY_ENTRY", "BUY", 121.0);
        let sell = build_order("SELL_ENTRY", "SELL", 79.0);
        for order in [&buy, &sell] {
            sqlx::query(
                "INSERT INTO strategy_orders
                 (id,user_id,snapshot_id,session_key,role,side,order_type,execution_mode,lots,
                  quantity,price,trigger_price,status,broker_order_id,broker_status,idempotency_key,
                  client_order_id,filled_quantity,processed_quantity,average_fill_price)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$11,'processing',$12,'',
                         $13,$14,10,0,$11)",
            )
            .bind(order.id)
            .bind(order.user_id)
            .bind(order.snapshot_id)
            .bind(&order.session_key)
            .bind(&order.role)
            .bind(&order.side)
            .bind(&order.order_type)
            .bind(&order.execution_mode)
            .bind(order.lots)
            .bind(order.quantity)
            .bind(order.price)
            .bind(&order.broker_order_id)
            .bind(format!("idempotency-{iteration}-{}", order.role))
            .bind(&order.client_order_id)
            .execute(&state.db)
            .await
            .expect("test order insert must succeed");
        }
        (buy, sell)
    }
    fn contract_for(instrument: &str, expiry: &str, lot_size: i32) -> MasterContract {
        MasterContract {
            token: "1".into(),
            symbol: format!("{instrument}{expiry}FUT"),
            name: instrument.into(),
            expiry: expiry.into(),
            strike: "0.000000".into(),
            lotsize: lot_size.to_string(),
            tick_size: "5.000000".into(),
            instrumenttype: "FUTCOM".into(),
            exch_seg: "MCX".into(),
        }
    }

    fn contract(expiry: &str) -> MasterContract {
        contract_for("GOLDTEN", expiry, 10)
    }

    fn option_master(
        exchange: &str,
        name: &str,
        expiry: &str,
        strike: f64,
        option_type: &str,
        lot_size: i32,
        token: &str,
    ) -> MasterContract {
        MasterContract {
            token: token.into(),
            symbol: format!("{name}{expiry}{}{}", strike as i32, option_type),
            name: name.into(),
            expiry: expiry.into(),
            strike: format!("{}", (strike * 100.0) as i64),
            lotsize: lot_size.to_string(),
            tick_size: "5.000000".into(),
            instrumenttype: "OPTIDX".into(),
            exch_seg: exchange.into(),
        }
    }

    fn st_candle(index: i64, close: f64) -> IntradayCandle {
        let at = NaiveDate::from_ymd_opt(2026, 8, 3)
            .unwrap()
            .and_hms_opt(9, 15, 0)
            .unwrap()
            + Duration::minutes(index * 5);
        IntradayCandle {
            at,
            open: close,
            high: close + 2.0,
            low: close - 2.0,
            close,
        }
    }

    #[test]
    fn supertrend_uses_wilder_rma_seed_and_update() {
        let values = rma(&[1.0, 2.0, 3.0, 4.0], 3);
        assert_eq!(values[0], None);
        assert_eq!(values[1], None);
        assert!((values[2].unwrap() - 2.0).abs() < 1e-12);
        assert!((values[3].unwrap() - (2.0 * 2.0 + 4.0) / 3.0).abs() < 1e-12);
    }

    #[test]
    fn supertrend_entry_window_is_0915_through_1509_ist() {
        let offset = FixedOffset::east_opt(19_800).unwrap();
        let at = |hour, minute| {
            offset
                .with_ymd_and_hms(2026, 8, 19, hour, minute, 0)
                .single()
                .unwrap()
        };
        assert!(!supertrend_entry_allowed(at(9, 14)));
        assert!(supertrend_entry_allowed(at(9, 15)));
        assert!(supertrend_entry_allowed(at(15, 9)));
        assert!(!option_square_off_due(at(15, 9)));
        assert!(!supertrend_entry_allowed(at(15, 10)));
        assert!(option_square_off_due(at(15, 10)));
        assert!(option_square_off_due(at(15, 11)));
    }

    #[test]
    fn supertrend_signal_detects_closed_candle_flip_to_call() {
        let closes = [
            100.0, 99.0, 98.0, 97.0, 96.0, 95.0, 94.0, 93.0, 92.0, 91.0, 90.0, 91.0, 92.0, 93.0,
            108.0,
        ];
        let candles: Vec<_> = closes
            .iter()
            .enumerate()
            .map(|(index, close)| st_candle(index as i64, *close))
            .collect();
        let points = supertrend_points(&candles, 3, 1.0);
        let signal = supertrend_signal(&points).unwrap();
        assert_eq!(signal.side, IndexOptionSide::Call);
        assert_eq!(signal.direction, SuperTrendDirection::Up);
        assert_eq!(signal.signal_at, candles.last().unwrap().at);
    }

    #[test]
    fn supertrend_current_signal_uses_just_closed_flip_candle() {
        let base = NaiveDate::from_ymd_opt(2026, 8, 17)
            .unwrap()
            .and_hms_opt(11, 35, 0)
            .unwrap();
        let candle = |minutes: i64, close: f64| IntradayCandle {
            at: base + Duration::minutes(minutes),
            open: close,
            high: close + 2.0,
            low: close - 2.0,
            close,
        };
        let points = vec![
            SuperTrendPoint {
                candle: candle(0, 100.0),
                value: 104.0,
                direction: SuperTrendDirection::Down,
            },
            SuperTrendPoint {
                candle: candle(5, 101.0),
                value: 103.0,
                direction: SuperTrendDirection::Down,
            },
            SuperTrendPoint {
                candle: candle(10, 110.0),
                value: 102.0,
                direction: SuperTrendDirection::Up,
            },
            SuperTrendPoint {
                candle: candle(15, 111.0),
                value: 103.0,
                direction: SuperTrendDirection::Up,
            },
        ];
        let now = FixedOffset::east_opt(19_800)
            .unwrap()
            .from_local_datetime(&(base + Duration::minutes(15)))
            .single()
            .unwrap();
        let signal = current_supertrend_signal(&points, now).unwrap();
        assert_eq!(signal.side, IndexOptionSide::Call);
        assert_eq!(signal.signal_at, candle(10, 110.0).at);
    }

    #[test]
    fn supertrend_does_not_replay_stale_flip_candle() {
        let base = NaiveDate::from_ymd_opt(2026, 8, 17)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        let candle = |minutes: i64, close: f64| IntradayCandle {
            at: base + Duration::minutes(minutes),
            open: close,
            high: close + 2.0,
            low: close - 2.0,
            close,
        };
        let points = vec![
            SuperTrendPoint {
                candle: candle(0, 100.0),
                value: 104.0,
                direction: SuperTrendDirection::Down,
            },
            SuperTrendPoint {
                candle: candle(5, 110.0),
                value: 102.0,
                direction: SuperTrendDirection::Up,
            },
        ];
        let now = FixedOffset::east_opt(19_800)
            .unwrap()
            .from_local_datetime(&(base + Duration::minutes(45)))
            .single()
            .unwrap();
        assert!(current_supertrend_signal(&points, now).is_none());
    }

    #[test]
    fn supertrend_first_candle_continues_previous_trading_day_direction() {
        let previous_at = NaiveDate::from_ymd_opt(2026, 8, 21)
            .unwrap()
            .and_hms_opt(15, 25, 0)
            .unwrap();
        let first_at = NaiveDate::from_ymd_opt(2026, 8, 24)
            .unwrap()
            .and_hms_opt(9, 15, 0)
            .unwrap();
        let candle = |at, close| IntradayCandle {
            at,
            open: close,
            high: close + 2.0,
            low: close - 2.0,
            close,
        };
        let points = vec![
            SuperTrendPoint {
                candle: candle(previous_at, 100.0),
                value: 104.0,
                direction: SuperTrendDirection::Down,
            },
            SuperTrendPoint {
                candle: candle(first_at, 110.0),
                value: 102.0,
                direction: SuperTrendDirection::Up,
            },
        ];
        let now = FixedOffset::east_opt(19_800)
            .unwrap()
            .from_local_datetime(&(first_at + Duration::minutes(5)))
            .single()
            .unwrap();
        let signal = current_supertrend_signal(&points, now).unwrap();
        assert_eq!(signal.side, IndexOptionSide::Call);
        assert_eq!(signal.signal_at, first_at);
    }

    #[test]
    fn supertrend_window_uses_latest_prior_session_across_weekend() {
        let friday = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        let monday = NaiveDate::from_ymd_opt(2026, 8, 24).unwrap();
        let mut candles = Vec::new();
        for index in 0..(SUPERTREND_ATR_PERIOD + 2) {
            let at = friday.and_hms_opt(14, 40, 0).unwrap() + Duration::minutes(index as i64 * 5);
            candles.push(IntradayCandle {
                at,
                open: 100.0,
                high: 102.0,
                low: 98.0,
                close: 99.0,
            });
        }
        let first = monday.and_hms_opt(9, 15, 0).unwrap();
        candles.push(IntradayCandle {
            at: first,
            open: 110.0,
            high: 112.0,
            low: 109.0,
            close: 111.0,
        });
        let (window, previous_session) =
            supertrend_session_candles(candles, monday, first).unwrap();
        assert_eq!(previous_session, friday);
        assert_eq!(window.last().unwrap().at, first);
    }

    #[test]
    fn live_index_candle_requires_ticks_near_both_bucket_edges() {
        let complete = LiveIndexCandle {
            bucket_epoch: 1_000,
            first_tick_epoch_ms: 1_005_000,
            last_tick_epoch_ms: 1_275_000,
            open: 100.0,
            high: 105.0,
            low: 99.0,
            close: 104.0,
        };
        assert!(live_index_candle_is_complete(complete));
        assert!(!live_index_candle_is_complete(LiveIndexCandle {
            first_tick_epoch_ms: 1_040_000,
            ..complete
        }));
        assert!(!live_index_candle_is_complete(LiveIndexCandle {
            last_tick_epoch_ms: 1_250_000,
            ..complete
        }));
    }

    #[test]
    fn supertrend_selects_nearest_expiry_atm_option() {
        let config = index_option_config("NIFTY").unwrap();
        let contracts = vec![
            option_master("NFO", "NIFTY", "27AUG2026", 25000.0, "CE", 75, "far"),
            option_master("NFO", "NIFTY", "20AUG2026", 24950.0, "CE", 75, "low"),
            option_master("NFO", "NIFTY", "20AUG2026", 25050.0, "CE", 75, "atm"),
            option_master("NFO", "NIFTY", "20AUG2026", 25150.0, "PE", 75, "put"),
        ];
        let candidates = supertrend_option_candidates(
            &contracts,
            config,
            NaiveDate::from_ymd_opt(2026, 8, 9).unwrap(),
            IndexOptionSide::Call,
        );
        let selected = choose_atm_contract(&candidates, 25060.0).unwrap();
        assert_eq!(selected.token, "atm");
        assert_eq!(
            selected.expiry,
            NaiveDate::from_ymd_opt(2026, 8, 20).unwrap()
        );
    }

    #[test]
    fn supertrend_defaults_and_entries_are_long_options_only() {
        assert_eq!(SUPERTREND_ATR_PERIOD, 7);
        assert!((SUPERTREND_FACTOR - 2.0).abs() < f64::EPSILON);
        assert_eq!(IndexOptionSide::Call.entry_role(), "BUY_ENTRY");
        assert_eq!(IndexOptionSide::Put.entry_role(), "BUY_ENTRY");
        assert_eq!(IndexOptionSide::Call.entry_side(), "BUY");
        assert_eq!(IndexOptionSide::Put.entry_side(), "BUY");
        assert_eq!(IndexOptionSide::Call.exit_side(), "SELL");
        assert_eq!(IndexOptionSide::Put.exit_side(), "SELL");
    }

    #[test]
    fn supertrend_protection_session_key_fits_order_column() {
        let key = supertrend_protection_session_key("st-SENSEX-20260810-0935-PE");
        assert_eq!(key, "st-SENSEX-20260810-0935-PE:p");
        assert!(key.len() <= 32);
    }

    #[test]
    fn formulas_match_v3() {
        let v = calculate(&[100.0, 110.0, 105.0, 108.0], &[90.0, 92.0, 94.0, 93.0]).unwrap();
        assert_eq!(v.hh4, 110.0);
        assert_eq!(v.ll2, 93.0);
        assert!((v.buy_entry - 110.132).abs() < 1e-9);
        assert!((v.buy_target - v.buy_entry * 1.015).abs() < 1e-9);
        assert!((v.buy_sl1 - (v.buy_entry * 0.985).max(v.ll2 * 0.9988)).abs() < 1e-9);
        assert!((v.buy_sl2 - (v.buy_entry * 0.985).max(v.ll4 * 0.9988)).abs() < 1e-9);
        assert!((v.sell_entry - 89.892).abs() < 1e-9);
        assert!((v.sell_target - v.sell_entry * 0.985).abs() < 1e-9);
        assert!((v.sell_sl1 - (v.sell_entry * 1.015).min(v.hh2 * 1.0012)).abs() < 1e-9);
        assert!((v.sell_sl2 - (v.sell_entry * 1.015).min(v.hh4 * 1.0012)).abs() < 1e-9);
    }

    #[test]
    fn futures_stops_always_apply_the_authoritative_max_min_cap() {
        let v = calculate(
            &[151_128.0, 152_100.0, 154_074.0, 154_450.0],
            &[147_979.0, 150_001.0, 152_011.0, 152_971.0],
        )
        .unwrap();
        assert!((v.buy_entry - 154_635.34).abs() < 1e-9);
        assert!((v.buy_sl1 - (v.buy_entry * 0.985).max(v.ll2 * 0.9988)).abs() < 1e-9);
        assert!((v.buy_sl2 - (v.buy_entry * 0.985).max(v.ll4 * 0.9988)).abs() < 1e-9);
        assert!((v.sell_sl1 - (v.sell_entry * 1.015).min(v.hh2 * 1.0012)).abs() < 1e-9);
        assert!((v.sell_sl2 - (v.sell_entry * 1.015).min(v.hh4 * 1.0012)).abs() < 1e-9);
    }

    #[test]
    fn option_ltp_lookup_uses_requested_contract_token() {
        let quote = json!({
            "data": {
                "fetched": [
                    {"symbolToken": SENSEX_INDEX_TOKEN, "ltp": 77928.15},
                    {"symbolToken": "1145633", "ltp": 272.0}
                ]
            }
        });

        assert_eq!(quote_ltp_for_token(&quote, "1145633"), Some(272.0));
        assert_eq!(quote_ltp_for_token(&quote, "missing"), None);
    }

    #[test]
    fn rolls_inside_ten_weekdays() {
        let items = vec![contract("10JUL2026"), contract("31JUL2026")];
        let selected = select_contract(
            &items,
            "GOLDTEN",
            NaiveDate::from_ymd_opt(2026, 7, 2).unwrap(),
        )
        .unwrap();
        assert_eq!(selected.1, NaiveDate::from_ymd_opt(2026, 7, 31).unwrap());
    }

    #[test]
    fn selects_each_supported_futures_contract_independently() {
        let items = vec![
            contract_for("GOLDM", "31AUG2026", 100),
            contract_for("GOLDTEN", "31AUG2026", 10),
            contract_for("SILVERM", "31AUG2026", 5),
            contract_for("SILVERMIC", "31AUG2026", 1),
            contract_for("NATGASMINI", "31AUG2026", 250),
        ];
        let date = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        for (instrument, lot_size) in [
            ("GOLDTEN", 10),
            ("GOLDM", 100),
            ("SILVERM", 5),
            ("SILVERMIC", 1),
            ("NATGASMINI", 250),
        ] {
            let selected = select_contract(&items, instrument, date).unwrap();
            assert_eq!(selected.0.name, instrument);
            assert_eq!(parse_lot_size(&selected.0.lotsize), Some(lot_size));
        }
    }

    #[test]
    fn target_lot_split() {
        for (lots, closed) in [(1, 1), (2, 1), (3, 2), (4, 2), (5, 3), (6, 3)] {
            assert_eq!(target_exit_lots(lots), closed);
        }
    }

    #[test]
    fn carry_orders_keep_target_and_advance_stop_after_tp1() {
        assert_eq!(carry_exit_role("TARGET", false), Some("TARGET"));
        assert_eq!(carry_exit_role("TARGET", true), None);
        assert_eq!(carry_exit_role("STOP", false), Some("SL1"));
        assert_eq!(carry_exit_role("STOP", true), Some("SL2"));
        assert!(!may_submit_exit_replacement(true));
        assert!(may_submit_exit_replacement(false));
    }

    #[test]
    fn exit_reasons_distinguish_protective_and_scheduled_closures() {
        assert_eq!(recorded_exit_reason(STRATEGY_KEY, "TARGET", "day"), "TP1");
        assert_eq!(recorded_exit_reason(STRATEGY_KEY, "SL1", "day"), "SL1");
        assert_eq!(recorded_exit_reason(STRATEGY_KEY, "SL2", "day"), "SL2");
        assert_eq!(
            recorded_exit_reason(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY, "TARGET", "st"),
            "TP"
        );
        assert_eq!(
            recorded_exit_reason(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY, "SL1", "st"),
            "SL"
        );
        assert_eq!(
            recorded_exit_reason(
                SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                "SL1",
                "stsq-20260812-1510"
            ),
            "MARKET_CLOSED"
        );
        assert_eq!(
            recorded_exit_reason(
                SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                "SL1",
                "strev-SENSEX-20260812-1000-CE"
            ),
            "SIGNAL_REVERSAL"
        );
    }

    #[test]
    fn sl2_reversal_uses_opposite_side_and_original_lots() {
        let sell = sl2_reversal_plan("BUY", 2).unwrap();
        assert_eq!(sell.direction, "SELL");
        assert_eq!(sell.entry_role, "SELL_ENTRY");
        assert_eq!(sell.entry_side, "SELL");
        assert_eq!(sell.lots, 2);

        let buy = sl2_reversal_plan("SELL", 4).unwrap();
        assert_eq!(buy.direction, "BUY");
        assert_eq!(buy.entry_role, "BUY_ENTRY");
        assert_eq!(buy.entry_side, "BUY");
        assert_eq!(buy.lots, 4);
        assert!(sl2_reversal_plan("BUY", 0).is_none());
    }

    #[test]
    fn missed_entry_logic_evaluates_buy_and_sell_independently() {
        let normal = futures_missed_entry_plan(100.0, 110.0, 90.0).unwrap();
        assert_eq!(
            normal,
            FuturesMissedEntryPlan {
                buy_missed: false,
                sell_missed: false
            }
        );

        let buy = futures_missed_entry_plan(110.01, 110.0, 90.0).unwrap();
        assert_eq!(
            buy,
            FuturesMissedEntryPlan {
                buy_missed: true,
                sell_missed: false
            }
        );
        let (buy_entry, sell_entry) = futures_opening_range_entries(buy, 112.0, 108.0).unwrap();
        assert!((buy_entry.unwrap() - 112.1344).abs() < 1e-9);
        assert!(sell_entry.is_none());

        let sell = futures_missed_entry_plan(89.99, 110.0, 90.0).unwrap();
        assert_eq!(
            sell,
            FuturesMissedEntryPlan {
                buy_missed: false,
                sell_missed: true
            }
        );
        let (buy_entry, sell_entry) = futures_opening_range_entries(sell, 92.0, 88.0).unwrap();
        assert!(buy_entry.is_none());
        assert!((sell_entry.unwrap() - 87.8944).abs() < 1e-9);
    }

    #[test]
    fn persisted_futures_gap_labels_fit_the_migrated_columns() {
        let plans = [
            FuturesMissedEntryPlan {
                buy_missed: false,
                sell_missed: false,
            },
            FuturesMissedEntryPlan {
                buy_missed: true,
                sell_missed: false,
            },
            FuturesMissedEntryPlan {
                buy_missed: false,
                sell_missed: true,
            },
            FuturesMissedEntryPlan {
                buy_missed: true,
                sell_missed: true,
            },
        ];
        assert!(plans.into_iter().all(|plan| plan.as_str().len() <= 16));
        assert!(
            ["BUY", "SELL", "BOTH"]
                .into_iter()
                .all(|direction| direction.len() <= 16)
        );
    }

    #[test]
    fn missed_entry_boundaries_are_inclusive_and_ignore_previous_close() {
        assert!(
            futures_missed_entry_plan(110.0, 110.0, 90.0)
                .unwrap()
                .buy_missed
        );
        assert!(
            futures_missed_entry_plan(90.0, 110.0, 90.0)
                .unwrap()
                .sell_missed
        );

        // These are the mandatory regressions: where yesterday closed is irrelevant.
        // An open above any hypothetical previous close but below BUY_ENTRY remains normal.
        let above_previous_close = futures_missed_entry_plan(105.0, 110.0, 90.0).unwrap();
        assert!(!above_previous_close.buy_missed);
        assert!(!above_previous_close.sell_missed);
        // An open below any hypothetical previous close but above SELL_ENTRY remains normal.
        let below_previous_close = futures_missed_entry_plan(95.0, 110.0, 90.0).unwrap();
        assert!(!below_previous_close.buy_missed);
        assert!(!below_previous_close.sell_missed);
    }

    #[test]
    fn reversal_exit_levels_are_anchored_to_the_new_entry() {
        let buy = futures_exit_levels_for_entry("BUY", 100.0, 110.0, 90.0, 120.0, 80.0).unwrap();
        assert!((buy.target - 101.5).abs() < 1e-9);
        assert!(buy.sl1 < 100.0);
        assert!(buy.sl2 < 100.0);

        let sell = futures_exit_levels_for_entry("SELL", 100.0, 110.0, 90.0, 120.0, 80.0).unwrap();
        assert!((sell.target - 98.5).abs() < 1e-9);
        assert!(sell.sl1 > 100.0);
        assert!(sell.sl2 > 100.0);

        let crossed_buy =
            futures_exit_levels_for_entry("BUY", 100.0, 120.0, 110.0, 130.0, 105.0).unwrap();
        assert!((crossed_buy.sl1 - (100.0_f64 * 0.985).max(110.0 * 0.9988)).abs() < 1e-9);
        assert!((crossed_buy.sl2 - (100.0_f64 * 0.985).max(105.0 * 0.9988)).abs() < 1e-9);

        let crossed_sell =
            futures_exit_levels_for_entry("SELL", 100.0, 90.0, 70.0, 95.0, 60.0).unwrap();
        assert!((crossed_sell.sl1 - (100.0_f64 * 1.015).min(90.0 * 1.0012)).abs() < 1e-9);
        assert!((crossed_sell.sl2 - (100.0_f64 * 1.015).min(95.0 * 1.0012)).abs() < 1e-9);
    }

    #[test]
    fn initial_futures_target_is_anchored_to_actual_fill_price() {
        let snapshot = Snapshot {
            id: Uuid::new_v4(),
            strategy_key: STRATEGY_KEY.into(),
            instrument: "NATGASMINI".into(),
            trade_date: NaiveDate::from_ymd_opt(2026, 8, 17).unwrap(),
            status: "ready".into(),
            error: None,
            contract_token: Some("token".into()),
            contract_symbol: Some("NATGASMINI25SEP26FUT".into()),
            contract_expiry: Some(NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()),
            lot_size: Some(250),
            exchange_segment: "MCX".into(),
            product_type: "CARRYFORWARD".into(),
            execution_key: "default".into(),
            underlying_token: String::new(),
            candle_dates: Vec::new(),
            highs: Vec::new(),
            lows: Vec::new(),
            hh2: Some(274.5),
            ll2: Some(266.6),
            hh4: Some(276.4),
            ll4: Some(266.6),
            buy_entry: Some(276.73168),
            buy_target: Some(280.8826552),
            buy_sl1: Some(266.28008),
            buy_sl2: Some(266.28008),
            sell_entry: Some(266.28008),
            sell_target: Some(262.2858788),
            sell_sl1: Some(274.8294),
            sell_sl2: Some(276.73168),
            previous_close: Some(269.7),
            market_open: Some(267.3),
            gap_direction: None,
            entry_direction: None,
            entry_source: Some("STANDARD".into()),
            gap_plan_status: None,
            opening_range_high: None,
            opening_range_low: None,
            planned_entry: None,
            planned_target: None,
            planned_sl1: None,
            planned_sl2: None,
            gap_planned_at: None,
            fetched_at: Utc::now(),
        };

        let levels = snapshot_order_exit_levels(&snapshot, "SELL", 262.60, false).unwrap();
        assert!((levels.target - 258.661).abs() < 1e-9);
        assert!(levels.target < 262.20);
        assert!((levels.sl1 - (262.60_f64 * 1.015).min(274.5 * 1.0012)).abs() < 1e-9);
        assert!((levels.sl2 - (262.60_f64 * 1.015).min(276.4 * 1.0012)).abs() < 1e-9);
    }

    #[test]
    fn sl2_reversal_session_is_stable_and_fits_order_storage() {
        let trade_id = Uuid::parse_str("630e1867-1bb3-4f77-a753-d663f5efc1fe").unwrap();
        let session = sl2_reversal_session(trade_id);
        assert_eq!(session, sl2_reversal_session(trade_id));
        assert_eq!(session.len(), 32);
    }

    #[test]
    fn contract_log_label_includes_selected_contract_symbol() {
        assert_eq!(
            contract_log_label("GOLDTEN", Some("GOLDTEN05AUG26FUT")),
            "GOLDTEN (GOLDTEN05AUG26FUT)"
        );
        assert_eq!(contract_log_label("GOLDTEN", Some("goldten")), "GOLDTEN");
        assert_eq!(contract_log_label("SENSEX_CE", None), "SENSEX_CE");
    }

    #[test]
    fn pnl_supports_long_and_short_positions() {
        assert_eq!(trade_pnl("BUY", 100.0, 112.5, 4.0), 50.0);
        assert_eq!(trade_pnl("SELL", 100.0, 87.5, 4.0), 50.0);
        assert_eq!(trade_pnl("BUY", 100.0, 87.5, 4.0), -50.0);
        assert_eq!(trade_pnl("BUY", 100.0, 101.0, 50.0), 50.0);
        assert_eq!(
            trade_pnl("BUY", 143_398.71, 145_549.70, 1.0).round(),
            2151.0
        );
    }

    #[test]
    fn futures_runtime_pnl_uses_each_contract_point_value() {
        assert_eq!(runtime_pnl_units("GOLDM", 400, Some(100)), 40.0);
        assert_eq!(runtime_pnl_units("GOLDTEN", 40, Some(10)), 4.0);
        assert_eq!(runtime_pnl_units("SILVERM", 20, Some(5)), 20.0);
        assert_eq!(runtime_pnl_units("SILVERMIC", 4, Some(1)), 4.0);
        assert_eq!(runtime_pnl_units("NATGASMINI", 1_000, Some(250)), 1_000.0);
        assert_eq!(
            trade_pnl(
                "BUY",
                100.0,
                1100.0,
                runtime_pnl_units("GOLDTEN", 40, Some(10))
            ),
            4000.0
        );
        assert_eq!(runtime_pnl_units("OTHER", 40, Some(10)), 40.0);
    }

    #[test]
    fn protective_levels_must_be_positive_and_finite() {
        assert_eq!(required_exit_level(Some(123.45), "target").unwrap(), 123.45);
        assert!(required_exit_level(None, "target").is_err());
        assert!(required_exit_level(Some(0.0), "stop loss").is_err());
        assert!(required_exit_level(Some(f64::NAN), "stop loss").is_err());
    }
    #[test]
    fn catchup_window_is_bounded() {
        assert!(!within_catchup_window(9 * 60 + 9, 9 * 60 + 10));
        assert!(within_catchup_window(9 * 60 + 10, 9 * 60 + 10));
        assert!(within_catchup_window(9 * 60 + 25, 9 * 60 + 10));
        assert!(!within_catchup_window(9 * 60 + 26, 9 * 60 + 10));
    }
    #[test]
    fn durable_order_state_machine_blocks_terminal_regressions() {
        assert!(valid_order_transition("pending", "submitting"));
        assert!(valid_order_transition("submitting", "ambiguous"));
        assert!(valid_order_transition("ambiguous", "submitted"));
        assert!(valid_order_transition("submitted", "partially_filled"));
        assert!(valid_order_transition("partially_filled", "cancelling"));
        assert!(valid_order_transition("cancelling", "filled"));
        assert!(valid_order_transition("cancelling", "rejected"));
        assert!(valid_order_transition("processing", "filled"));
        assert!(!valid_order_transition("filled", "submitting"));
        assert!(!valid_order_transition("cancelled", "processing"));
        assert!(!valid_order_transition("rejected", "pending"));
    }
    #[test]
    fn reconciliation_maps_partial_and_terminal_broker_states() {
        assert_eq!(reconciled_state("open", 0), "submitted");
        assert_eq!(reconciled_state("open", 2), "partially_filled");
        assert_eq!(reconciled_state("complete", 2), "filled");
        assert_eq!(reconciled_state("rejected", 0), "rejected");
        assert_eq!(reconciled_state("canceled", 1), "cancelled");
    }

    #[test]
    fn aggregate_position_mismatch_types_supersede_each_other() {
        assert!(is_aggregate_position_mismatch("LOCAL_POSITION_BROKER_FLAT"));
        assert!(is_aggregate_position_mismatch(
            "QUANTITY_OR_DIRECTION_MISMATCH"
        ));
        assert!(is_aggregate_position_mismatch("AVERAGE_ENTRY_MISMATCH"));
        assert!(!is_aggregate_position_mismatch("ORPHAN_POSITION"));
    }

    #[test]
    fn background_lease_prevents_overlapping_reconciliation_and_releases_on_drop() {
        let active = Arc::new(AtomicBool::new(false));
        let lease = BackgroundLease::try_acquire(&active).expect("first lease must be acquired");
        assert!(BackgroundLease::try_acquire(&active).is_none());
        drop(lease);
        assert!(BackgroundLease::try_acquire(&active).is_some());
    }

    #[tokio::test]
    async fn scheduler_job_lease_recovers_after_panic_timeout_and_shutdown() {
        let leases = SchedulerLeaseRegistry::default();
        let lease = leases.try_acquire("worker").expect("worker starts");
        let panic = tokio::spawn(async move {
            let _lease = lease;
            panic!("isolated worker panic");
        })
        .await;
        assert!(panic.is_err());
        assert!(leases.try_acquire("worker").is_some());

        let lease = leases.try_acquire("timeout").expect("worker starts");
        let timed_out = tokio::time::timeout(std::time::Duration::from_millis(5), async move {
            let _lease = lease;
            std::future::pending::<()>().await;
        })
        .await;
        assert!(timed_out.is_err());
        assert!(leases.try_acquire("timeout").is_some());

        let lease = leases.try_acquire("shutdown").expect("worker starts");
        let task = tokio::spawn(async move {
            let _lease = lease;
            std::future::pending::<()>().await;
        });
        task.abort();
        let _ = task.await;
        assert!(leases.try_acquire("shutdown").is_some());
    }

    #[test]
    fn completed_scheduler_dispatches_are_exactly_once_but_failures_can_retry() {
        let dispatches = SchedulerDispatchTracker::default();
        assert!(!dispatches.completed("2026-09-18:event"));
        dispatches.mark_completed("2026-09-18:event".into());
        assert!(dispatches.completed("2026-09-18:event"));
        assert!(!dispatches.completed("2026-09-18:failed-event"));
        dispatches.retain_date(NaiveDate::from_ymd_opt(2026, 9, 19).unwrap());
        assert!(!dispatches.completed("2026-09-18:event"));
    }

    #[test]
    fn sep_14_18_replay_delivers_all_66_demo_entries_once() {
        let events = historical_replay_events();
        assert_eq!(events.len(), 35);
        assert!(events.iter().all(|event| event.price > 0.0));
        let counts = replay_scheduler_delivery(&events);
        assert_eq!(counts.futures_dispatched, 8);
        assert_eq!(counts.futures_evaluated, 8);
        assert_eq!(counts.futures_demo_path, 8);
        assert_eq!(counts.supertrend_dispatched, 58);
        assert_eq!(counts.supertrend_evaluated, 58);
        assert_eq!(counts.supertrend_demo_path, 58);
        assert_eq!(counts.duplicate_dispatches, 0);
    }

    #[test]
    fn cancelling_orders_process_each_new_fill_delta_before_terminal_state() {
        let first_partial = reconciliation_plan("cancelling", "open", 10, 0);
        assert_eq!(first_partial.prepare_state, "submitted");
        assert!(first_partial.process_delta);
        assert!(first_partial.cancellation_in_flight);
        assert!(!first_partial.request_cancel);

        let later_partial = reconciliation_plan("cancelling", "open", 20, 10);
        assert!(later_partial.process_delta);

        let terminal_partial = reconciliation_plan("cancelling", "cancelled", 20, 10);
        assert_eq!(terminal_partial.prepare_state, "submitted");
        assert_eq!(terminal_partial.terminal_state, Some("cancelled"));
        assert!(terminal_partial.process_delta);

        let terminal_without_delta = reconciliation_plan("cancelling", "rejected", 20, 20);
        assert_eq!(terminal_without_delta.prepare_state, "rejected");
        assert!(!terminal_without_delta.process_delta);
    }

    #[test]
    fn a_new_partial_fill_requests_cancel_but_an_existing_cancel_does_not_repeat() {
        let detected = reconciliation_plan("submitted", "open", 5, 0);
        assert!(detected.request_cancel);
        assert!(!detected.cancellation_in_flight);

        let pending = reconciliation_plan("cancelling", "open", 5, 5);
        assert!(!pending.request_cancel);
        assert!(pending.cancellation_in_flight);
        assert_eq!(pending.prepare_state, "cancelling");
    }

    #[test]
    fn broker_fill_watermarks_and_delta_prices_are_monotonic() {
        assert_eq!(broker_fill_watermark(5, 10, 8, 20), 10);
        assert_eq!(broker_fill_watermark(25, 10, 8, 20), 20);
        assert_eq!(incremental_fill_price(10, Some(100.0), 20, 105.0), 110.0);
        assert_eq!(incremental_fill_price(0, None, 10, 101.5), 101.5);
    }

    #[test]
    fn protection_recovery_never_treats_submission_as_confirmation() {
        assert_eq!(
            protection_recovery_decision(10, 10, true, false, 1, 3),
            ProtectionRecoveryDecision::ConfirmProtected
        );
        assert_eq!(
            protection_recovery_decision(0, 10, true, false, 1, 3),
            ProtectionRecoveryDecision::WaitForReconciliation
        );
        assert_eq!(
            protection_recovery_decision(0, 10, false, false, 1, 3),
            ProtectionRecoveryDecision::SubmitStop
        );
    }

    #[test]
    fn protection_timeout_or_retry_exhaustion_requires_emergency_close() {
        assert_eq!(
            protection_recovery_decision(0, 10, true, true, 1, 3),
            ProtectionRecoveryDecision::EmergencyClose
        );
        assert_eq!(
            protection_recovery_decision(0, 10, false, false, 3, 3),
            ProtectionRecoveryDecision::EmergencyClose
        );
    }

    #[test]
    fn broker_position_parser_accepts_official_string_fields() {
        let raw = json!({
            "exchange":"nfo",
            "symboltoken":"12345",
            "tradingsymbol":"NIFTY26AUG25000CE",
            "producttype":"INTRADAY",
            "netqty":"-75",
            "avgnetprice":"101.25"
        });
        let positions = parse_broker_positions(&json!([raw.clone()]));
        assert_eq!(
            positions,
            vec![BrokerNetPosition {
                exchange: "NFO".into(),
                token: "12345".into(),
                symbol: "NIFTY26AUG25000CE".into(),
                product: "INTRADAY".into(),
                net_quantity: -75,
                average_price: 101.25,
                raw,
            }]
        );
    }

    #[test]
    fn position_mismatch_policy_detects_flat_quantity_and_direction() {
        assert_eq!(position_mismatch_type(10, 10), None);
        assert_eq!(
            position_mismatch_type(0, 10),
            Some("LOCAL_POSITION_BROKER_FLAT")
        );
        assert_eq!(
            position_mismatch_type(-10, 10),
            Some("QUANTITY_OR_DIRECTION_MISMATCH")
        );
        assert_eq!(
            position_mismatch_type(15, 10),
            Some("QUANTITY_OR_DIRECTION_MISMATCH")
        );
    }

    #[test]
    fn angel_master_tick_size_and_directional_rounding_are_deterministic() {
        let tick = parse_tick_size("5.000000").unwrap();
        assert!((tick - 0.05).abs() < 1e-12);
        assert_eq!(normalize_to_tick(100.021, tick, "BUY"), Some(100.05));
        assert_eq!(normalize_to_tick(100.021, tick, "SELL"), Some(100.0));
        assert_eq!(normalize_to_tick(0.001, tick, "SELL"), None);
        assert_eq!(normalize_to_tick(f64::NAN, tick, "BUY"), None);
    }

    #[test]
    fn duplicate_and_invalid_intraday_candles_are_filtered_by_identity() {
        let candles = parse_intraday_candles(&json!([
            ["2026-08-21T09:15:00", 100.0, 102.0, 99.0, 101.0],
            ["2026-08-21T09:15:00", 101.0, 103.0, 100.0, 102.0],
            ["2026-08-21T09:20:00", 100.0, 99.0, 98.0, 101.0]
        ]));
        assert_eq!(candles.len(), 1);
        assert_eq!(
            candles[0].at.time(),
            NaiveTime::from_hms_opt(9, 15, 0).unwrap()
        );
        assert_eq!(candles[0].open, 101.0);
        assert_eq!(candles[0].close, 102.0);
    }

    #[test]
    fn duplicate_and_out_of_order_tick_sequences_are_rejected() {
        assert!(market_tick_is_newer(None, 1_000, 10));
        assert!(market_tick_is_newer(Some((1_000, 10)), 1_000, 11));
        assert!(!market_tick_is_newer(Some((1_000, 10)), 1_000, 10));
        assert!(!market_tick_is_newer(Some((1_000, 10)), 999, 99));
        // A reconnect may reset sequence numbers; a genuinely later exchange
        // timestamp remains acceptable.
        assert!(market_tick_is_newer(Some((1_000, 10)), 1_001, 1));
    }

    #[test]
    fn contract_quantities_must_be_positive_whole_lots() {
        assert!(valid_contract_quantity(75, 25, 3));
        assert!(!valid_contract_quantity(74, 25, 3));
        assert!(!valid_contract_quantity(75, 0, 3));
        assert!(!valid_contract_quantity(75, 25, 0));
    }

    #[test]
    fn entry_exit_and_broker_residual_quantity_policies_are_distinct() {
        assert!(quantity_matches_policy(
            75,
            OrderQuantityPolicy::NormalEntry {
                lot_size: 25,
                lots: 3
            }
        ));
        assert!(!quantity_matches_policy(
            74,
            OrderQuantityPolicy::NormalEntry {
                lot_size: 25,
                lots: 3
            }
        ));
        assert!(quantity_matches_policy(
            25,
            OrderQuantityPolicy::NormalExit {
                remaining_quantity: 50
            }
        ));
        assert!(!quantity_matches_policy(
            51,
            OrderQuantityPolicy::NormalExit {
                remaining_quantity: 50
            }
        ));
        assert!(quantity_matches_policy(
            17,
            OrderQuantityPolicy::BrokerResidual {
                broker_quantity: 17
            }
        ));
        assert!(!quantity_matches_policy(
            20,
            OrderQuantityPolicy::BrokerResidual {
                broker_quantity: 17
            }
        ));
    }

    #[test]
    fn available_funds_parser_is_numeric_and_conservative() {
        assert_eq!(
            broker_available_funds(&json!({
                "availablecash":"1000.50",
                "availablelimitmargin":"900.00",
                "net":"950.00"
            })),
            Some(1000.5)
        );
        let typed = parse_angel_rms_funds(&json!({
            "availablecash":"1000.50",
            "availablelimitmargin":"-50.00",
            "net":"950.00"
        }))
        .unwrap();
        assert_eq!(typed.available_cash, 1000.5);
        assert_eq!(typed.net, Some(950.0));
        assert_eq!(typed.available_limit_margin, Some(-50.0));
        assert_eq!(
            broker_available_funds(&json!({"availablecash":"bad","net":"99999"})),
            None,
            "undocumented RMS fields must not become a permissive fallback"
        );
        assert_eq!(broker_available_funds(&json!({"net":"NaN"})), None);
    }

    #[test]
    fn full_quote_circuit_limits_are_parsed_for_the_requested_token() {
        let quote = json!({"data":{"fetched":[
            {"symbolToken":"other","lowerCircuit":1.0,"upperCircuit":2.0},
            {"symbolToken":"wanted","lowerCircuit":"90.5","upperCircuit":"110.5"}
        ]}});
        assert_eq!(
            quote_price_band_for_token(&quote, "wanted"),
            Some((90.5, 110.5))
        );
        assert_eq!(quote_price_band_for_token(&quote, "missing"), None);
    }

    #[test]
    fn simultaneous_opposite_fills_are_fully_accounted() {
        assert_eq!(
            account_opposite_fill(10, 10),
            OppositeFillAccounting {
                offset_quantity: 10,
                residual_existing: 0,
                residual_incoming: 0,
            }
        );
        assert_eq!(
            account_opposite_fill(10, 16),
            OppositeFillAccounting {
                offset_quantity: 10,
                residual_existing: 0,
                residual_incoming: 6,
            }
        );
        assert_eq!(
            account_opposite_fill(16, 10),
            OppositeFillAccounting {
                offset_quantity: 10,
                residual_existing: 6,
                residual_incoming: 0,
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn concurrent_signal_status_refreshes_do_not_deadlock() {
        let state = isolated_test_state().await;
        let user_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users(id,username,email,password_hash) VALUES($1,'signal-lock-test','signal-lock-test@example.test','test-only')",
        )
        .bind(user_id)
        .execute(&state.db)
        .await
        .unwrap();
        for index in 0..64 {
            let signal_id = Uuid::new_v4();
            let action = if index % 2 == 0 {
                "ENTRY"
            } else {
                "SQUARE_OFF"
            };
            let signal_type = if action == "ENTRY" {
                "ENTRY"
            } else {
                "SQUARE_OFF"
            };
            sqlx::query(
                "INSERT INTO strategy_signals(id,strategy_key,instrument,session_key,signal_at,signal_type,status,expected_users)
                 VALUES($1,$2,'LOCK_TEST',$3,NOW(),$4,'dispatching',1)",
            )
            .bind(signal_id)
            .bind(if action == "ENTRY" {
                STRATEGY_KEY
            } else {
                SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
            })
            .bind(format!("signal-lock-{index}"))
            .bind(signal_type)
            .execute(&state.db)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO strategy_execution_intents(
                   id,signal_id,user_id,strategy_key,instrument,session_key,action,role,side,
                   order_type,lots,quantity,price,status,completed_at)
                 VALUES(gen_random_uuid(),$1,$2,$3,'LOCK_TEST',$4,$5,'EMERGENCY_CLOSE','SELL',
                        'MARKET',1,1,1,'completed',NOW())",
            )
            .bind(signal_id)
            .bind(user_id)
            .bind(if action == "ENTRY" {
                STRATEGY_KEY
            } else {
                SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
            })
            .bind(format!("signal-lock-{index}"))
            .bind(action)
            .execute(&state.db)
            .await
            .unwrap();
        }

        let mut tasks = tokio::task::JoinSet::new();
        for worker in 0..16 {
            let task_state = state.clone();
            tasks.spawn(async move {
                for _ in 0..20 {
                    if worker % 2 == 0 {
                        process_execution_intents(&task_state, None).await?;
                    } else {
                        reconcile_square_off_intents(&task_state).await?;
                    }
                }
                AppResult::Ok(())
            });
        }
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while let Some(result) = tasks.join_next().await {
                result.expect("status refresh task must not panic").unwrap();
            }
        })
        .await
        .expect("deterministically ordered status refreshes must not stall");

        let completed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM strategy_signals
             WHERE instrument='LOCK_TEST' AND status='completed'",
        )
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(completed, 64);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn concurrent_opposite_futures_fill_handlers_serialize_in_postgres() {
        let state = isolated_test_state().await;
        for iteration in 0..20 {
            let (buy, sell) = seed_concurrent_futures_fill(&state, iteration).await;
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            *futures_fill_overlap_barrier().lock().await = Some(barrier);
            let buy_state = state.clone();
            let sell_state = state.clone();
            let buy_task =
                tokio::spawn(async move { complete_claimed_order(&buy_state, buy, 121.0).await });
            let sell_task =
                tokio::spawn(async move { complete_claimed_order(&sell_state, sell, 79.0).await });
            let (buy_result, sell_result) = tokio::join!(buy_task, sell_task);
            *futures_fill_overlap_barrier().lock().await = None;
            let _ = buy_result.expect("BUY fill task must not panic");
            let _ = sell_result.expect("SELL fill task must not panic");

            let user_id: Uuid = sqlx::query_scalar(
                "SELECT user_id FROM strategy_orders WHERE session_key=$1 LIMIT 1",
            )
            .bind(format!("race-{iteration}-BUY_ENTRY"))
            .fetch_one(&state.db)
            .await
            .expect("test user must remain queryable");
            let watermarks: Vec<(i32, i32)> = sqlx::query_as(
                "SELECT filled_quantity,processed_quantity
                 FROM strategy_orders
                 WHERE user_id=$1 AND role IN ('BUY_ENTRY','SELL_ENTRY')
                 ORDER BY role",
            )
            .bind(user_id)
            .fetch_all(&state.db)
            .await
            .expect("fill watermarks must be queryable");
            assert_eq!(watermarks, vec![(10, 10), (10, 10)]);
            let (trade_rows, open_rows, signed_open_quantity): (i64, i64, i64) = sqlx::query_as(
                "SELECT COUNT(*),
                            COUNT(*) FILTER (WHERE status='open'),
                            COALESCE(SUM(CASE direction WHEN 'BUY' THEN quantity ELSE -quantity END)
                                FILTER (WHERE status='open'),0)
                     FROM trades
                     WHERE user_id=$1 AND strategy_key=$2 AND instrument_label='GOLDTEN'",
            )
            .bind(user_id)
            .bind(STRATEGY_KEY)
            .fetch_one(&state.db)
            .await
            .expect("resulting exposure must be queryable");
            assert_eq!(trade_rows, 1, "one serialized exposure row is expected");
            assert_eq!(open_rows, 0, "equal opposite fills must leave no open row");
            assert_eq!(signed_open_quantity, 0, "local net exposure must be flat");
            let active_protection: i64 = sqlx::query_scalar(
                "SELECT COUNT(*)
                 FROM strategy_orders
                 WHERE user_id=$1
                   AND role IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE')
                   AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')",
            )
            .bind(user_id)
            .fetch_one(&state.db)
            .await
            .expect("protection intents must be queryable");
            assert!(
                active_protection <= 1,
                "serialized fills must not create duplicate protection intent"
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires CREATE DATABASE on an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn removed_legacy_features_preserve_active_and_inactive_history() {
        let base = isolated_test_database_url();
        let admin_url = database_url_for_name(&base, "postgres");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .expect("isolated PostgreSQL admin database must be reachable");
        let suffix = &Uuid::new_v4().simple().to_string()[..12];
        let database_name = format!("rulenix_test_history_{suffix}");
        sqlx::query(&format!("CREATE DATABASE \"{database_name}\""))
            .execute(&admin)
            .await
            .expect("disposable history database must be creatable");

        let database_url = database_url_for_name(&base, &database_name);
        let db = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await
            .expect("disposable history database must be reachable");
        apply_raw_migrations_through(&db, "20260819000000_durable_signal_fanout.sql")
            .await
            .expect("schema immediately before the retirement migration must apply");

        let active_user = Uuid::new_v4();
        let inactive_user = Uuid::new_v4();
        let active_snapshot = Uuid::new_v4();
        let inactive_snapshot = Uuid::new_v4();
        let active_trade = Uuid::new_v4();
        let inactive_trade = Uuid::new_v4();
        let active_order = Uuid::new_v4();
        let inactive_order = Uuid::new_v4();
        let active_decision = Uuid::new_v4();
        let inactive_decision = Uuid::new_v4();
        let active_signal = Uuid::new_v4();
        let inactive_signal = Uuid::new_v4();
        let backtest_run = Uuid::new_v4();

        for (id, username, email) in [
            (active_user, "legacy-active", "legacy-active@example.test"),
            (
                inactive_user,
                "legacy-inactive",
                "legacy-inactive@example.test",
            ),
        ] {
            sqlx::query(
                "INSERT INTO users(id,username,email,password_hash) VALUES($1,$2,$3,'test-only')",
            )
            .bind(id)
            .bind(username)
            .bind(email)
            .execute(&db)
            .await
            .unwrap();
            sqlx::query("INSERT INTO user_profiles(user_id,demo_balance) VALUES($1,$2)")
                .bind(id)
                .bind(if id == active_user {
                    187654.25
                } else {
                    204321.75
                })
                .execute(&db)
                .await
                .unwrap();
        }

        for (user_id, active) in [(active_user, true), (inactive_user, false)] {
            sqlx::query("INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots) VALUES($1,'option_entry_v1','NIFTY',$2,1)")
                .bind(user_id).bind(active).execute(&db).await.unwrap();
            sqlx::query("INSERT INTO user_strategy_activations(user_id,strategy_key,is_active,activated_at,deactivated_at) VALUES($1,'option_entry_v1',$2,NOW()-INTERVAL '30 days',CASE WHEN $2 THEN NULL ELSE NOW()-INTERVAL '2 days' END)")
                .bind(user_id).bind(active).execute(&db).await.unwrap();
        }

        for (snapshot_id, trade_date, token, symbol) in [
            (active_snapshot, "2026-08-20", "26001", "NIFTY26AUG25000CE"),
            (
                inactive_snapshot,
                "2026-07-20",
                "25001",
                "NIFTY26JUL24500PE",
            ),
        ] {
            sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token) VALUES($1,'option_entry_v1','NIFTY',$2::date,'ready','',$3,$4,$2::date+7,50,'NFO','INTRADAY',$5,'99926000')")
                .bind(snapshot_id).bind(trade_date).bind(token).bind(symbol)
                .bind(format!("legacy-{token}"))
                .execute(&db).await.unwrap();
        }

        for (trade_id, user_id, snapshot_id, status, external_entry, margin, pnl) in [
            (
                active_trade,
                active_user,
                active_snapshot,
                "open",
                "BROKER-ACTIVE-ENTRY",
                32145.50,
                125.75,
            ),
            (
                inactive_trade,
                inactive_user,
                inactive_snapshot,
                "closed",
                "BROKER-HISTORIC-ENTRY",
                29876.25,
                -84.50,
            ),
        ] {
            sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,exit_price,last_price,pnl,entry_datetime,exit_datetime,instrument_label,contract_symbol,external_entry_id,external_exit_id,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,margin_required) VALUES($1,$2,'live',$4,'BUY',50,100.25,CASE WHEN $4='closed' THEN 98.56 ELSE NULL END,102.75,$7,NOW()-INTERVAL '3 days',CASE WHEN $4='closed' THEN NOW()-INTERVAL '2 days' ELSE NULL END,'NIFTY',$5,$6,CASE WHEN $4='closed' THEN 'BROKER-HISTORIC-EXIT' ELSE '' END,'preservation fixture','option_entry_v1',$3,1,CASE WHEN $4='open' THEN 1 ELSE 0 END,$8)")
                .bind(trade_id).bind(user_id).bind(snapshot_id).bind(status).bind(if status == "open" { "NIFTY26AUG25000CE" } else { "NIFTY26JUL24500PE" }).bind(external_entry).bind(pnl).bind(margin)
                .execute(&db).await.unwrap();
        }

        for (decision_id, user_id, margin) in [
            (active_decision, active_user, 32145.50),
            (inactive_decision, inactive_user, 29876.25),
        ] {
            sqlx::query("INSERT INTO risk_decisions(id,user_id,order_id,execution_mode,order_role,allowed,reason_code,message,values) VALUES($1,$2,NULL,'live','BUY_ENTRY',TRUE,'ALLOWED','legacy decision',jsonb_build_object('order',jsonb_build_object('margin_required',$3),'health',jsonb_build_object('margin_available',50000)))")
                .bind(decision_id).bind(user_id).bind(margin)
                .execute(&db).await.unwrap();
        }

        for (order_id, user_id, snapshot_id, trade_id, decision_id, broker_id, margin) in [
            (
                active_order,
                active_user,
                active_snapshot,
                active_trade,
                active_decision,
                "ANGEL-ACTIVE-ORDER",
                32145.50,
            ),
            (
                inactive_order,
                inactive_user,
                inactive_snapshot,
                inactive_trade,
                inactive_decision,
                "ANGEL-HISTORIC-ORDER",
                29876.25,
            ),
        ] {
            sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,execution_mode,lots,quantity,price,status,broker_order_id,broker_status,idempotency_key,client_order_id,filled_quantity,processed_quantity,average_fill_price,filled_price,filled_at,risk_decision_id,margin_required) VALUES($1,$2,$3,$4,$5,'BUY_ENTRY','BUY','live',1,50,100.25,'filled',$7,'complete',$6,$8,50,50,100.25,100.25,NOW()-INTERVAL '3 days',$9,$10)")
                .bind(order_id).bind(user_id).bind(snapshot_id).bind(trade_id)
                .bind(format!("legacy-{}", &order_id.simple().to_string()[..8]))
                .bind(format!("legacy-idempotency-{order_id}"))
                .bind(broker_id)
                .bind(format!("RX{}", &order_id.simple().to_string()[..18]).to_uppercase())
                .bind(decision_id).bind(margin)
                .execute(&db).await.unwrap();
            sqlx::query("UPDATE risk_decisions SET order_id=$1 WHERE id=$2")
                .bind(order_id)
                .bind(decision_id)
                .execute(&db)
                .await
                .unwrap();
            sqlx::query("INSERT INTO broker_order_events(order_id,user_id,from_state,to_state,event_type,broker_order_id,diagnostic,broker_payload) VALUES($1,$2,'submitted','filled','reconciled_fill',$3,'historical fill retained',jsonb_build_object('filledshares','50','averageprice','100.25'))")
                .bind(order_id).bind(user_id).bind(broker_id).execute(&db).await.unwrap();
            sqlx::query("INSERT INTO strategy_events(user_id,strategy_key,instrument,event_type,payload) VALUES($1,'option_entry_v1','NIFTY','legacy_fill',jsonb_build_object('order_id',$2::text,'broker_order_id',$3))")
                .bind(user_id).bind(order_id).bind(broker_id).execute(&db).await.unwrap();
        }

        for (signal_id, user_id, snapshot_id, trade_id, order_id, session) in [
            (
                active_signal,
                active_user,
                active_snapshot,
                active_trade,
                active_order,
                "legacy-active-signal",
            ),
            (
                inactive_signal,
                inactive_user,
                inactive_snapshot,
                inactive_trade,
                inactive_order,
                "legacy-inactive-signal",
            ),
        ] {
            sqlx::query("INSERT INTO strategy_signals(id,strategy_key,instrument,session_key,signal_at,snapshot_id,signal_type,status,expected_users,payload) VALUES($1,'option_entry_v1','NIFTY',$2,NOW()-INTERVAL '3 days',$3,'OPTION_ENTRY','completed',1,'{}')")
                .bind(signal_id).bind(session).bind(snapshot_id).execute(&db).await.unwrap();
            sqlx::query("INSERT INTO strategy_execution_intents(id,signal_id,user_id,snapshot_id,trade_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,status,strategy_order_id,completed_at) VALUES($1,$2,$3,$4,$5,'option_entry_v1','NIFTY',$6,'ENTRY','BUY_ENTRY','BUY','MARKET',1,50,100.25,'completed',$7,NOW()-INTERVAL '3 days')")
                .bind(Uuid::new_v4()).bind(signal_id).bind(user_id).bind(snapshot_id).bind(trade_id).bind(session).bind(order_id)
                .execute(&db).await.unwrap();
        }

        sqlx::query("INSERT INTO strategy_reversal_intents(source_trade_id,user_id,snapshot_id,instrument,source_direction,reversal_direction,lots,entry_price,order_session_key,status) VALUES($1,$2,$3,'NIFTY','BUY','SELL',1,98.50,'legacy-reversal','completed')")
            .bind(inactive_trade).bind(inactive_user).bind(inactive_snapshot).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO broker_margin_estimates(id,exchange,symbol_token,trading_symbol,product_type,order_type,trade_type,lot_size,margin_per_lot,raw_response,fetched_by) VALUES($1,'NFO','26001','NIFTY26AUG25000CE','INTRADAY','MARKET','BUY',50,32145.50,jsonb_build_object('legacy',TRUE),$2)")
            .bind(Uuid::new_v4()).bind(active_user).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO backtest_option_contracts(id,snapshot_date,instrument,side,exchange,symbol_token,trading_symbol,expiry_date,strike_price,lot_size) VALUES($1,'2026-07-20','NIFTY','CE','NFO','25001','NIFTY26JUL24500CE','2026-07-30',24500,50)")
            .bind(Uuid::new_v4()).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO backtest_runs(id,user_id,strategy_key,instrument,trading_symbol,symbol_token,interval_key,lookback_months,from_time,to_time,lots,lot_size,status,summary,data_points) VALUES($1,$2,'option_entry_v1','NIFTY','NIFTY-I','99926000','FIVE_MINUTE',1,NOW()-INTERVAL '30 days',NOW(),1,50,'completed',jsonb_build_object('initial_margin',29876.25,'max_margin_used',32145.50),100)")
            .bind(backtest_run).bind(inactive_user).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO backtest_trades(id,run_id,trade_date,direction,entry_time,entry_price,exit_time,exit_price,lots,quantity,realized_pnl,exit_reason,levels) VALUES($1,$2,'2026-07-20','BUY',NOW()-INTERVAL '4 days',100.25,NOW()-INTERVAL '3 days',98.56,1,50,-84.50,'STOP',jsonb_build_object('broker_reference','BACKTEST-HISTORY'))")
            .bind(Uuid::new_v4()).bind(backtest_run).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO audit_events(event_type,actor_user_id,target_user_id,summary,metadata) VALUES('legacy_option_entry',$1,$2,'historical audit retained',jsonb_build_object('strategy_key','option_entry_v1'))")
            .bind(active_user).bind(inactive_user).execute(&db).await.unwrap();

        let counts_sql = "SELECT jsonb_build_object(
            'users',(SELECT COUNT(*) FROM users WHERE id IN ($1,$2)),
            'profiles',(SELECT COUNT(*) FROM user_profiles WHERE user_id IN ($1,$2)),
            'configs',(SELECT COUNT(*) FROM user_strategy_configs WHERE strategy_key='option_entry_v1'),
            'activations',(SELECT COUNT(*) FROM user_strategy_activations WHERE strategy_key='option_entry_v1'),
            'snapshots',(SELECT COUNT(*) FROM strategy_market_snapshots WHERE strategy_key='option_entry_v1'),
            'trades',(SELECT COUNT(*) FROM trades WHERE strategy_key='option_entry_v1'),
            'orders',(SELECT COUNT(*) FROM strategy_orders WHERE snapshot_id IN ($3,$4)),
            'fills',(SELECT COUNT(*) FROM broker_order_events WHERE order_id IN ($5,$6)),
            'events',(SELECT COUNT(*) FROM strategy_events WHERE strategy_key='option_entry_v1'),
            'signals',(SELECT COUNT(*) FROM strategy_signals WHERE strategy_key='option_entry_v1'),
            'intents',(SELECT COUNT(*) FROM strategy_execution_intents WHERE strategy_key='option_entry_v1'),
            'reversals',(SELECT COUNT(*) FROM strategy_reversal_intents WHERE snapshot_id IN ($3,$4)),
            'backtest_runs',(SELECT COUNT(*) FROM backtest_runs WHERE strategy_key='option_entry_v1'),
            'backtest_trades',(SELECT COUNT(*) FROM backtest_trades WHERE run_id=$7),
            'contracts',(SELECT COUNT(*) FROM backtest_option_contracts),
            'margin_estimates',(SELECT COUNT(*) FROM broker_margin_estimates),
            'audit_events',(SELECT COUNT(*) FROM audit_events WHERE event_type='legacy_option_entry'))";
        let before: Value = sqlx::query_scalar(counts_sql)
            .bind(active_user)
            .bind(inactive_user)
            .bind(active_snapshot)
            .bind(inactive_snapshot)
            .bind(active_order)
            .bind(inactive_order)
            .bind(backtest_run)
            .fetch_one(&db)
            .await
            .unwrap();

        apply_raw_migration_file(&db, "20260823000000_remove_margin_and_option_entry.sql")
            .await
            .expect("history-preserving retirement migration must apply");

        let after: Value = sqlx::query_scalar(counts_sql)
            .bind(active_user)
            .bind(inactive_user)
            .bind(active_snapshot)
            .bind(inactive_snapshot)
            .bind(active_order)
            .bind(inactive_order)
            .bind(backtest_run)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(
            after, before,
            "retirement migration must preserve every historical category"
        );
        assert_eq!(after["trades"], 2);
        assert_eq!(after["orders"], 2);
        assert_eq!(after["snapshots"], 2);
        assert_eq!(after["events"], 2);

        let relationships: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT COUNT(*) FROM strategy_orders o JOIN trades t ON t.id=o.trade_id JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE s.strategy_key='option_entry_v1'),
                (SELECT COUNT(*) FROM broker_order_events e JOIN strategy_orders o ON o.id=e.order_id WHERE o.id IN ($1,$2)),
                (SELECT COUNT(*) FROM strategy_execution_intents i JOIN strategy_signals s ON s.id=i.signal_id JOIN strategy_orders o ON o.id=i.strategy_order_id WHERE i.strategy_key='option_entry_v1'),
                (SELECT COUNT(*) FROM risk_decisions d JOIN strategy_orders o ON o.risk_decision_id=d.id WHERE o.id IN ($1,$2))",
        )
        .bind(active_order).bind(inactive_order).fetch_one(&db).await.unwrap();
        assert_eq!(relationships, (2, 2, 2, 2));

        let preserved_values: (String, String, String, String, f64, f64) = sqlx::query_as(
            "SELECT
                (SELECT external_entry_id FROM trades WHERE id=$1),
                (SELECT external_exit_id FROM trades WHERE id=$2),
                (SELECT broker_order_id FROM strategy_orders WHERE id=$3),
                (SELECT broker_order_id FROM strategy_orders WHERE id=$4),
                (SELECT margin_required FROM trades WHERE id=$1),
                (SELECT pnl::double precision FROM trades WHERE id=$2)",
        )
        .bind(active_trade)
        .bind(inactive_trade)
        .bind(active_order)
        .bind(inactive_order)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(preserved_values.0, "BROKER-ACTIVE-ENTRY");
        assert_eq!(preserved_values.1, "BROKER-HISTORIC-EXIT");
        assert_eq!(preserved_values.2, "ANGEL-ACTIVE-ORDER");
        assert_eq!(preserved_values.3, "ANGEL-HISTORIC-ORDER");
        assert!((preserved_values.4 - 32145.50).abs() < 1e-9);
        assert!((preserved_values.5 - (-84.50)).abs() < 1e-9);

        apply_raw_migration_file(&db, "20260823010000_execution_safety_lifecycle.sql")
            .await
            .unwrap();
        apply_raw_migration_file(&db, "20260823020000_p0_execution_safety.sql")
            .await
            .unwrap();
        let application_history_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM trades t JOIN strategy_orders o ON o.trade_id=t.id JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id WHERE t.user_id IN ($1,$2) AND t.strategy_key='option_entry_v1' AND s.strategy_key='option_entry_v1'",
        )
        .bind(active_user).bind(inactive_user).fetch_one(&db).await.unwrap();
        assert_eq!(
            application_history_rows, 2,
            "historical application query must work after all pending migrations"
        );

        db.close().await;
        sqlx::query(&format!("DROP DATABASE \"{database_name}\" WITH (FORCE)"))
            .execute(&admin)
            .await
            .expect("disposable history database cleanup must succeed");
        admin.close().await;
    }

    #[tokio::test]
    #[ignore = "requires CREATE DATABASE on an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn migrations_pass_clean_and_require_explicit_legacy_exit_reconciliation() {
        let base = isolated_test_database_url();
        let admin_url = database_url_for_name(&base, "postgres");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .expect("isolated PostgreSQL admin database must be reachable");
        let suffix = &Uuid::new_v4().simple().to_string()[..12];
        let clean_name = format!("rulenix_test_clean_{suffix}");
        let legacy_name = format!("rulenix_test_legacy_{suffix}");
        for name in [&clean_name, &legacy_name] {
            sqlx::query(&format!("CREATE DATABASE \"{name}\""))
                .execute(&admin)
                .await
                .expect("disposable child database must be creatable");
        }

        let clean_url = database_url_for_name(&base, &clean_name);
        let clean = PgPoolOptions::new()
            .max_connections(2)
            .connect(&clean_url)
            .await
            .unwrap();
        let migrations = sqlx::migrate::Migrator::new(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations"
        )))
        .await
        .unwrap();
        migrations
            .run(&clean)
            .await
            .expect("all migrations must apply to a clean disposable database");
        let safety_columns: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.columns WHERE table_schema='public' AND ((table_name='trades' AND column_name IN ('safety_status','exposure_origin')) OR (table_name='strategy_orders' AND column_name IN ('order_type','exchange_segment','product_type')) OR (table_name='broker_position_incidents' AND column_name IN ('raw_broker_position','ownership_status','product_type')))")
            .fetch_one(&clean).await.unwrap();
        assert_eq!(safety_columns, 8);
        let gap_column_widths: Vec<(String, i32)> = sqlx::query_as(
            "SELECT column_name,character_maximum_length::INTEGER
             FROM information_schema.columns
             WHERE table_schema='public'
               AND table_name='strategy_market_snapshots'
               AND column_name IN ('gap_direction','entry_direction')
             ORDER BY column_name",
        )
        .fetch_all(&clean)
        .await
        .unwrap();
        assert_eq!(
            gap_column_widths,
            vec![
                ("entry_direction".to_string(), 16),
                ("gap_direction".to_string(), 16),
            ]
        );
        let snapshot_id = Uuid::new_v4();
        sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready','','test-token','TESTFUT',CURRENT_DATE+30,10,'MCX','CARRYFORWARD','gap-width-regression','')")
            .bind(snapshot_id)
            .bind(STRATEGY_KEY)
            .execute(&clean)
            .await
            .unwrap();
        for gap_direction in ["NONE_MISSED", "BUY_MISSED", "SELL_MISSED", "BOTH_MISSED"] {
            sqlx::query("UPDATE strategy_market_snapshots SET gap_direction=$2,entry_direction='BOTH' WHERE id=$1")
                .bind(snapshot_id)
                .bind(gap_direction)
                .execute(&clean)
                .await
                .expect("every executable Futures gap label must persist after migration");
        }
        clean.close().await;

        let legacy_url = database_url_for_name(&base, &legacy_name);
        let legacy = PgPoolOptions::new()
            .max_connections(4)
            .connect(&legacy_url)
            .await
            .unwrap();
        apply_raw_migrations_through(&legacy, "20260823000000_remove_margin_and_option_entry.sql")
            .await
            .expect("pre-safety migrations must apply");
        let user_id = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        let trade_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,username,email,password_hash) VALUES($1,'legacy-user','legacy@example.test','test-only')")
            .bind(user_id).execute(&legacy).await.unwrap();
        sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready','','legacy-token','LEGACYFUT',CURRENT_DATE+30,10,'MCX','CARRYFORWARD','legacy','')")
            .bind(snapshot_id).bind(STRATEGY_KEY).execute(&legacy).await.unwrap();
        sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots) VALUES($1,$2,'live','open','BUY',50,100,100,0,NOW(),'GOLDTEN','LEGACYFUT','representative legacy active trade',$3,$4,5,5)")
            .bind(trade_id).bind(user_id).bind(STRATEGY_KEY).bind(snapshot_id).execute(&legacy).await.unwrap();
        let duplicate_a = Uuid::new_v4();
        let duplicate_b = Uuid::new_v4();
        let legacy_orders = [
            (Uuid::new_v4(), "BUY_ENTRY", "BUY", "pending"),
            (Uuid::new_v4(), "SL1", "SELL", "failed"),
            (Uuid::new_v4(), "SELL_ENTRY", "SELL", "ambiguous"),
            (duplicate_a, "SL1", "SELL", "submitted"),
            (duplicate_b, "SL2", "SELL", "submitted"),
        ];
        for (index, (order_id, role, side, status)) in legacy_orders.into_iter().enumerate() {
            sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,execution_mode,lots,quantity,price,status,broker_order_id,broker_status,idempotency_key,client_order_id,filled_quantity,processed_quantity) VALUES($1,$2,$3,$4,$5,$6,$7,'live',5,50,100,$8,$9,'',$10,$11,0,0)")
                .bind(order_id).bind(user_id).bind(snapshot_id).bind(trade_id)
                .bind(format!("legacy-{index}"))
                .bind(role).bind(side).bind(status)
                .bind(if status == "submitted" { format!("LEGACY-{index}") } else { String::new() })
                .bind(format!("legacy-idempotency-{index}"))
                .bind(format!("LEGACYCLIENT{index}"))
                .execute(&legacy).await.unwrap();
        }
        let unsafe_duplicates: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM (SELECT trade_id,CASE WHEN role IN ('SL1','SL2') THEN 'STOP' ELSE role END family FROM strategy_orders WHERE trade_id IS NOT NULL AND role IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE') AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') GROUP BY trade_id,family HAVING COUNT(*)>1) duplicates")
            .fetch_one(&legacy).await.unwrap();
        assert_eq!(
            unsafe_duplicates, 1,
            "preflight must expose unsafe active duplicates"
        );

        let lifecycle_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("migrations/20260823010000_execution_safety_lifecycle.sql");
        let lifecycle_sql = std::fs::read_to_string(lifecycle_path).unwrap();
        let mut unsafe_transaction = legacy.begin().await.unwrap();
        let unsafe_result = sqlx::raw_sql(&lifecycle_sql)
            .execute(&mut *unsafe_transaction)
            .await;
        assert!(
            unsafe_result.is_err(),
            "unsafe duplicate exits must stop migration"
        );
        unsafe_transaction.rollback().await.unwrap();

        sqlx::query("UPDATE strategy_orders SET status='cancelled',broker_status='Test preflight reconciled this duplicate as terminal; row retained.' WHERE id=$1 AND status='submitted'")
            .bind(duplicate_b).execute(&legacy).await.unwrap();
        let mut repaired_transaction = legacy.begin().await.unwrap();
        sqlx::raw_sql(&lifecycle_sql)
            .execute(&mut *repaired_transaction)
            .await
            .expect("migration must pass after broker-informed terminal reconciliation");
        repaired_transaction.commit().await.unwrap();
        let p0_sql = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("migrations/20260823020000_p0_execution_safety.sql"),
        )
        .unwrap();
        let mut p0_transaction = legacy.begin().await.unwrap();
        sqlx::raw_sql(&p0_sql)
            .execute(&mut *p0_transaction)
            .await
            .unwrap();
        p0_transaction.commit().await.unwrap();
        let legacy_state: (String, String, i64) = sqlx::query_as("SELECT t.safety_status,(SELECT status FROM strategy_orders WHERE id=$2),(SELECT COUNT(*) FROM strategy_orders WHERE trade_id=t.id) FROM trades t WHERE t.id=$1")
            .bind(trade_id).bind(duplicate_b).fetch_one(&legacy).await.unwrap();
        assert_eq!(legacy_state.0, "PROTECTION_REQUIRED");
        assert_eq!(legacy_state.1, "cancelled");
        assert_eq!(legacy_state.2, 5, "legacy order history must be retained");
        legacy.close().await;

        for name in [&clean_name, &legacy_name] {
            sqlx::query(&format!("DROP DATABASE \"{name}\" WITH (FORCE)"))
                .execute(&admin)
                .await
                .expect("disposable child database cleanup must succeed");
        }
        admin.close().await;
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn futures_gap_labels_persist_in_migrated_database() {
        let state = isolated_test_state().await;
        let snapshot_id = Uuid::new_v4();
        sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready','','test-token','TESTFUT',CURRENT_DATE+30,10,'MCX','CARRYFORWARD','gap-width-stateful-regression','')")
            .bind(snapshot_id)
            .bind(STRATEGY_KEY)
            .execute(&state.db)
            .await
            .unwrap();

        for gap_direction in ["NONE_MISSED", "BUY_MISSED", "SELL_MISSED", "BOTH_MISSED"] {
            sqlx::query("UPDATE strategy_market_snapshots SET gap_direction=$2,entry_direction='BOTH' WHERE id=$1")
                .bind(snapshot_id)
                .bind(gap_direction)
                .execute(&state.db)
                .await
                .expect("every executable Futures gap label must persist after migration");
        }

        let widths: Vec<(String, i32)> = sqlx::query_as(
            "SELECT column_name,character_maximum_length::INTEGER
             FROM information_schema.columns
             WHERE table_schema='public'
               AND table_name='strategy_market_snapshots'
               AND column_name IN ('gap_direction','entry_direction')
             ORDER BY column_name",
        )
        .fetch_all(&state.db)
        .await
        .unwrap();
        assert_eq!(
            widths,
            vec![
                ("entry_direction".to_string(), 16),
                ("gap_direction".to_string(), 16),
            ]
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn position_reconciliation_includes_zero_order_accounts_and_persists_unmapped_exposure() {
        let state = isolated_test_state().await;
        let user_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id,username,email,password_hash,can_live_trade)
             VALUES ($1,'orphan-user','orphan@example.test','test-only',TRUE)",
        )
        .bind(user_id)
        .execute(&state.db)
        .await
        .expect("test user insert must succeed");
        sqlx::query(
            "INSERT INTO user_profiles (user_id,trading_mode,last_token_status)
             VALUES ($1,'live','success')",
        )
        .bind(user_id)
        .execute(&state.db)
        .await
        .expect("test live profile insert must succeed");
        for kind in ["api_key", "jwt_token"] {
            sqlx::query(
                "INSERT INTO broker_secrets
                 (user_id,secret_kind,key_version,nonce,ciphertext)
                 VALUES ($1,$2,1,$3,$4)",
            )
            .bind(user_id)
            .bind(kind)
            .bind(vec![0_u8; 12])
            .bind(vec![1_u8; 16])
            .execute(&state.db)
            .await
            .expect("test broker-secret marker insert must succeed");
        }
        let audience = reconciliation_audience(&state)
            .await
            .expect("reconciliation audience query must succeed");
        assert!(audience.connected.is_empty());
        assert_eq!(audience.needs_full_readiness, vec![user_id]);
        assert!(audience.disconnected.is_empty());
        let active_orders: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM strategy_orders WHERE user_id=$1")
                .bind(user_id)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(
            active_orders, 0,
            "audience membership must not require orders"
        );

        let raw_position = json!({
            "exchange":"MCX",
            "symboltoken":"never-seen-token",
            "tradingsymbol":"MANUALUNKNOWN",
            "producttype":"CARRYFORWARD",
            "netqty":"7",
            "avgnetprice":"4321.25",
            "brokerExtra":"retained"
        });
        reconcile_broker_positions(
            &state,
            user_id,
            0,
            &json!([raw_position.clone()]),
            None,
            &json!([]),
        )
        .await
        .expect("unknown broker position reconciliation must succeed");
        let incident: (String, String, String, String, i32, Option<f64>, Value) = sqlx::query_as(
            "SELECT incident_type,status,ownership_status,product_type,
                        broker_quantity,broker_average_price,raw_broker_position
                 FROM broker_position_incidents
                 WHERE user_id=$1 AND contract_token='never-seen-token'",
        )
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .expect("unmapped broker exposure must be persisted");
        assert_eq!(incident.0, "UNMAPPED_BROKER_POSITION");
        assert_eq!(incident.1, "operator_required");
        assert_eq!(incident.2, "ambiguous");
        assert_eq!(incident.3, "CARRYFORWARD");
        assert_eq!(incident.4, 7);
        assert_eq!(incident.5, Some(4321.25));
        assert_eq!(incident.6, raw_position);
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn broker_position_matrix_reconciles_orphans_mismatches_and_aggregates() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,username,email,password_hash,can_live_trade) VALUES($1,'matrix-user','matrix@example.test','test-only',TRUE)")
            .bind(user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_profiles(user_id,trading_mode,last_token_status) VALUES($1,'live','success')")
            .bind(user_id).execute(&state.db).await.unwrap();
        state
            .credentials
            .put(
                user_id,
                &[("api_key", "matrix-api"), ("jwt_token", "matrix-jwt")],
            )
            .await
            .unwrap();

        let cases = [
            ("a", "ORPHAN", Vec::<(i32, &str, f64)>::new()),
            ("b", "BROKER_FLAT", vec![(10, "BUY", 100.0)]),
            ("c", "BROKER_GREATER", vec![(10, "BUY", 100.0)]),
            ("d", "BROKER_LOWER", vec![(10, "BUY", 100.0)]),
            ("e", "DIRECTION", vec![(10, "BUY", 100.0)]),
            ("f", "AVERAGE", vec![(10, "BUY", 100.0)]),
            ("h", "AGGREGATE", vec![(5, "BUY", 100.0), (5, "BUY", 100.0)]),
        ];
        let mut snapshots = HashMap::new();
        for (case, instrument, locals) in cases {
            let snapshot_id = Uuid::new_v4();
            let token = format!("matrix-{case}");
            let symbol = format!("MATRIX{case}FUT");
            sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token) VALUES($1,$2,$3,CURRENT_DATE,'ready','',$4,$5,CURRENT_DATE+30,5,'MCX','CARRYFORWARD',$6,'')")
                .bind(snapshot_id).bind(STRATEGY_KEY).bind(instrument).bind(&token).bind(&symbol).bind(format!("matrix-{case}"))
                .execute(&state.db).await.unwrap();
            for (quantity, direction, average) in locals {
                sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status) VALUES($1,$2,'live','open',$3,$4,($5::float8)::numeric,($5::float8)::numeric,0,NOW(),$6,$7,'position matrix',$8,$9,1,1,'PROTECTED')")
                    .bind(Uuid::new_v4()).bind(user_id).bind(direction).bind(quantity).bind(average).bind(instrument).bind(&symbol).bind(STRATEGY_KEY).bind(snapshot_id)
                    .execute(&state.db).await.unwrap();
            }
            snapshots.insert(case.to_owned(), (snapshot_id, token, symbol));
        }
        let position = |case: &str, quantity: i32, average: f64| {
            let (_, token, symbol) = snapshots.get(case).unwrap();
            json!({
                "exchange":"MCX","symboltoken":token,"tradingsymbol":symbol,
                "producttype":"CARRYFORWARD","netqty":quantity.to_string(),
                "avgnetprice":average.to_string()
            })
        };
        *fake.positions.lock().await = vec![
            position("a", 10, 100.0),
            position("c", 15, 100.0),
            position("d", 5, 100.0),
            position("e", -10, 100.0),
            position("f", 10, 101.0),
            position("h", 10, 100.0),
        ];
        reconcile_live_user(&state, user_id).await.unwrap();

        let incident_for = |case: &str| snapshots.get(case).unwrap().1.clone();
        let a: (String, String) = sqlx::query_as("SELECT incident_type,status FROM broker_position_incidents WHERE user_id=$1 AND contract_token=$2")
            .bind(user_id).bind(incident_for("a")).fetch_one(&state.db).await.unwrap();
        assert_eq!(a, ("ORPHAN_POSITION".into(), "operator_required".into()));
        let b: String = sqlx::query_scalar("SELECT incident_type FROM broker_position_incidents WHERE user_id=$1 AND contract_token=$2")
            .bind(user_id).bind(incident_for("b")).fetch_one(&state.db).await.unwrap();
        assert_eq!(b, "LOCAL_POSITION_BROKER_FLAT");
        for case in ["c", "d", "e"] {
            let incident: String = sqlx::query_scalar("SELECT incident_type FROM broker_position_incidents WHERE user_id=$1 AND contract_token=$2")
                .bind(user_id).bind(incident_for(case)).fetch_one(&state.db).await.unwrap();
            assert_eq!(incident, "QUANTITY_OR_DIRECTION_MISMATCH");
        }
        let f: String = sqlx::query_scalar("SELECT incident_type FROM broker_position_incidents WHERE user_id=$1 AND contract_token=$2")
            .bind(user_id).bind(incident_for("f")).fetch_one(&state.db).await.unwrap();
        assert_eq!(f, "AVERAGE_ENTRY_MISMATCH");
        let h_incidents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM broker_position_incidents WHERE user_id=$1 AND contract_token=$2 AND status IN ('open','operator_required')")
            .bind(user_id).bind(incident_for("h")).fetch_one(&state.db).await.unwrap();
        assert_eq!(h_incidents, 0);
        let h_rows: Vec<(i32, Option<i32>)> = sqlx::query_as("SELECT t.quantity,t.broker_net_quantity FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id WHERE t.user_id=$1 AND s.contract_token=$2 ORDER BY t.id")
            .bind(user_id).bind(incident_for("h")).fetch_all(&state.db).await.unwrap();
        assert_eq!(h_rows, vec![(5, Some(10)), (5, Some(10))]);
        broker_task.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn tp_sl_double_fills_create_one_exact_emergency_close_and_converge_flat() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,username,email,password_hash,can_live_trade) VALUES($1,'overclose-user','overclose@example.test','test-only',TRUE)")
            .bind(user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_profiles(user_id,trading_mode,last_token_status) VALUES($1,'live','success')")
            .bind(user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session) VALUES($1,$2,'GOLDTEN',TRUE,5,TRUE,TRUE)")
            .bind(user_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        state
            .credentials
            .put(
                user_id,
                &[("api_key", "fake-api"), ("jwt_token", "fake-jwt")],
            )
            .await
            .unwrap();

        let cases = [
            ("tp-then-sl", 50, 50, 50),
            ("sl-then-tp", 50, 50, 50),
            ("partial-tp-full-sl", 50, 25, 50),
            ("partial-sl-full-tp", 50, 50, 25),
        ];
        let mut broker_positions = Vec::new();
        let mut broker_trade_fills = Vec::new();
        let mut contracts = Vec::new();
        let mut expected = HashMap::new();
        for (case_index, (case_name, entry_quantity, target_fill, stop_fill)) in
            cases.into_iter().enumerate()
        {
            let snapshot_id = Uuid::new_v4();
            let source_trade_id = Uuid::new_v4();
            let token = format!("overclose-token-{case_index}");
            let symbol = format!("GOLDTEN30SEP26FUT{case_index}");
            sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,candle_dates,highs,lows,hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2,previous_close) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready','',$3,$4,CURRENT_DATE+30,10,'MCX','CARRYFORWARD',$5,'',ARRAY[]::date[],ARRAY[110.0,112.0,118.0,120.0],ARRAY[80.0,82.0,88.0,90.0],120.0,80.0,120.0,80.0,120.144,121.94616,118.34184,118.34184,79.904,78.70544,81.10256,81.10256,100.0)")
                .bind(snapshot_id).bind(STRATEGY_KEY).bind(&token).bind(&symbol).bind(case_name)
                .execute(&state.db).await.unwrap();
            sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,exit_datetime,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,sl2_price,safety_status,exit_reason) VALUES($1,$2,'live','closed','BUY',$3,100,100,0,NOW(),NOW(),'GOLDTEN',$4,$5,$6,$7,5,0,101.5,98.5,98.0,'CLOSED','TP_SL_DOUBLE_FILL_SOURCE')")
                .bind(source_trade_id).bind(user_id).bind(entry_quantity).bind(&symbol).bind(case_name).bind(STRATEGY_KEY).bind(snapshot_id)
                .execute(&state.db).await.unwrap();
            let order_specs = [
                ("BUY_ENTRY", "BUY", entry_quantity, entry_quantity, "filled"),
                (
                    "TARGET",
                    "SELL",
                    entry_quantity,
                    target_fill,
                    if target_fill == entry_quantity {
                        "filled"
                    } else {
                        "cancelled"
                    },
                ),
                (
                    "SL1",
                    "SELL",
                    entry_quantity,
                    stop_fill,
                    if stop_fill == entry_quantity {
                        "filled"
                    } else {
                        "cancelled"
                    },
                ),
            ];
            for (order_index, (role, side, requested, processed, status)) in
                order_specs.into_iter().enumerate()
            {
                let order_id = Uuid::new_v4();
                sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,status,broker_order_id,broker_status,idempotency_key,client_order_id,filled_quantity,processed_quantity,average_fill_price) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'live',5,$9,100,$10,$11,'',$12,$13,$14,$14,100)")
                    .bind(order_id).bind(user_id).bind(snapshot_id).bind(source_trade_id)
                    .bind(format!("{case_index}-{order_index}"))
                    .bind(role).bind(side)
                    .bind(if role == "SL1" { "STOPLOSS_MARKET" } else if role == "TARGET" { "LIMIT" } else { "MARKET" })
                    .bind(requested).bind(status)
                    .bind(format!("SRC-{case_index}-{order_index}"))
                    .bind(format!("source-{case_index}-{order_index}"))
                    .bind(format!("source-client-{case_index}-{order_index}"))
                    .bind(processed)
                    .execute(&state.db).await.unwrap();
                if processed > 0 {
                    broker_trade_fills.push(json!({
                        "orderid":format!("SRC-{case_index}-{order_index}"),
                        "ordertag":format!("source-client-{case_index}-{order_index}"),
                        "exchange":"MCX", "symboltoken":token,
                        "tradingsymbol":symbol, "transactiontype":side,
                        "fillsize":processed.to_string(), "fillprice":"100",
                        "filltime":Utc::now().to_rfc3339()
                    }));
                }
            }
            let residual = target_fill + stop_fill - entry_quantity;
            assert!(residual > 0);
            broker_positions.push(json!({
                "exchange":"MCX",
                "symboltoken":token,
                "tradingsymbol":symbol,
                "producttype":"CARRYFORWARD",
                "netqty":(-residual).to_string(),
                "avgnetprice":"99.50"
            }));
            expected.insert(token.clone(), residual);
            sqlx::query("INSERT INTO market_price_ticks(exchange_segment,contract_token,price,received_at) VALUES('MCX',$1,99.5,NOW())")
                .bind(&token).execute(&state.db).await.unwrap();
            contracts.push(MasterContract {
                token,
                symbol,
                name: "GOLDTEN".into(),
                expiry: "30SEP2026".into(),
                strike: "0".into(),
                lotsize: "10".into(),
                tick_size: "5.000000".into(),
                instrumenttype: "FUTCOM".into(),
                exch_seg: "MCX".into(),
            });
        }
        crate::contract_master::set_isolated_test_cache(contracts).await;

        let positions = json!(broker_positions);
        let trade_book = json!(broker_trade_fills);
        let first_state = state.clone();
        let first_positions = positions.clone();
        let first_trade_book = trade_book.clone();
        let second_state = state.clone();
        let second_positions = positions.clone();
        let second_trade_book = trade_book.clone();
        let (first, second) = tokio::join!(
            tokio::spawn(async move {
                reconcile_broker_positions(
                    &first_state,
                    user_id,
                    0,
                    &first_positions,
                    Some(&first_trade_book),
                    &json!([]),
                )
                .await
            }),
            tokio::spawn(async move {
                reconcile_broker_positions(
                    &second_state,
                    user_id,
                    0,
                    &second_positions,
                    Some(&second_trade_book),
                    &json!([]),
                )
                .await
            })
        );
        first.unwrap().unwrap();
        second.unwrap().unwrap();

        let residuals: Vec<(Uuid, String, String, i32)> = sqlx::query_as(
            "SELECT t.id,s.contract_token,t.direction,t.quantity
             FROM trades t
             JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
             WHERE t.user_id=$1 AND t.status='open' AND t.exposure_origin='broker_over_close'
             ORDER BY s.contract_token",
        )
        .bind(user_id)
        .fetch_all(&state.db)
        .await
        .unwrap();
        assert_eq!(residuals.len(), 4, "one residual row per broker token");
        for (_, token, direction, quantity) in &residuals {
            assert_eq!(direction, "SELL");
            assert_eq!(Some(quantity), expected.get(token));
        }
        let incidents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM broker_position_incidents WHERE user_id=$1 AND incident_type='OVER_CLOSE_POSITION' AND status='operator_required'")
            .bind(user_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(incidents, 4);

        recover_unprotected_trades(&state).await.unwrap();
        recover_unprotected_trades(&state).await.unwrap();
        let emergency_orders: Vec<(Uuid, String, i32, String)> = sqlx::query_as(
            "SELECT o.trade_id,o.side,o.quantity,o.status
             FROM strategy_orders o
             JOIN trades t ON t.id=o.trade_id
             WHERE t.user_id=$1 AND o.role='EMERGENCY_CLOSE'
             ORDER BY o.created_at",
        )
        .bind(user_id)
        .fetch_all(&state.db)
        .await
        .unwrap();
        assert_eq!(emergency_orders.len(), 4, "one close intent per residual");
        for (trade_id, side, quantity, status) in &emergency_orders {
            let expected_quantity = residuals
                .iter()
                .find(|(id, _, _, _)| id == trade_id)
                .map(|(_, _, _, value)| *value)
                .unwrap();
            assert_eq!(side, "BUY");
            assert_eq!(*quantity, expected_quantity);
            assert_eq!(status, "submitted");
        }
        let placed = fake.placed_orders.lock().await.clone();
        assert_eq!(placed.len(), 4);
        assert!(placed.iter().all(|body| {
            body.get("transactiontype").and_then(Value::as_str) == Some("BUY")
                && body
                    .get("quantity")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<i32>().ok())
                    .is_some_and(|quantity| quantity == 25 || quantity == 50)
        }));

        sqlx::query("UPDATE strategy_orders SET status='filled',filled_quantity=quantity,processed_quantity=quantity,average_fill_price=price,last_reconciled_at=NOW() WHERE user_id=$1 AND role='EMERGENCY_CLOSE'")
            .bind(user_id).execute(&state.db).await.unwrap();
        reconcile_broker_positions(&state, user_id, 0, &json!([]), None, &json!([]))
            .await
            .unwrap();
        let still_open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trades WHERE user_id=$1 AND status='open' AND exposure_origin='broker_over_close'")
            .bind(user_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(
            still_open, 0,
            "broker-confirmed flat residuals must converge closed"
        );
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn ambiguous_stop_response_is_reconciled_without_blind_retry_before_target() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        fake.place_modes
            .lock()
            .await
            .extend([FakePlaceMode::AcceptWithoutOrderId, FakePlaceMode::Accept]);
        let state = isolated_test_state_with_broker(&broker_url).await;
        let user_id = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        let trade_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,username,email,password_hash,can_live_trade) VALUES($1,'uncertain-user','uncertain@example.test','test-only',TRUE)")
            .bind(user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_profiles(user_id,trading_mode,last_token_status) VALUES($1,'live','success')")
            .bind(user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session) VALUES($1,$2,'GOLDTEN',TRUE,5,TRUE,TRUE)")
            .bind(user_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        state
            .credentials
            .put(
                user_id,
                &[("api_key", "fake-api"), ("jwt_token", "fake-jwt")],
            )
            .await
            .unwrap();
        sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,candle_dates,highs,lows,hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2,previous_close) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready','','uncertain-token','GOLDTEN30SEP26FUT',CURRENT_DATE+30,10,'MCX','CARRYFORWARD','uncertain-protection','',ARRAY[]::date[],ARRAY[110.0,112.0,118.0,120.0],ARRAY[80.0,82.0,88.0,90.0],120.0,80.0,120.0,80.0,120.144,121.94616,98.5,98.0,79.904,78.70544,101.5,102.0,100.0)")
            .bind(snapshot_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,sl2_price,safety_status,protection_deadline_at,broker_net_quantity,broker_average_price,last_position_reconciled_at) VALUES($1,$2,'live','open','BUY',50,100,100,0,NOW(),'GOLDTEN','GOLDTEN30SEP26FUT','test ambiguous protection',$3,$4,5,5,101.5,98.5,98.0,'PROTECTION_REQUIRED',NOW()+INTERVAL '5 minutes',50,100,NOW())")
            .bind(trade_id).bind(user_id).bind(STRATEGY_KEY).bind(snapshot_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,status,broker_order_id,broker_status,idempotency_key,client_order_id,filled_quantity,processed_quantity,average_fill_price) VALUES($1,$2,$3,$4,'uncertain-entry','BUY_ENTRY','BUY','MARKET','live',5,50,100,'filled','ENTRY-1','','$5','ENTRY-CLIENT-1',50,50,100)")
            .bind(Uuid::new_v4()).bind(user_id).bind(snapshot_id).bind(trade_id).bind(format!("uncertain-entry-{trade_id}"))
            .execute(&state.db).await.unwrap();
        crate::contract_master::set_isolated_test_cache(vec![MasterContract {
            token: "uncertain-token".into(),
            symbol: "GOLDTEN30SEP26FUT".into(),
            name: "GOLDTEN".into(),
            expiry: "30SEP2026".into(),
            strike: "0".into(),
            lotsize: "10".into(),
            tick_size: "5.000000".into(),
            instrumenttype: "FUTCOM".into(),
            exch_seg: "MCX".into(),
        }])
        .await;
        *fake.positions.lock().await = vec![json!({
            "exchange":"MCX","symboltoken":"uncertain-token",
            "tradingsymbol":"GOLDTEN30SEP26FUT","producttype":"CARRYFORWARD",
            "netqty":"50","avgnetprice":"100"
        })];

        recover_unprotected_trades(&state).await.unwrap();
        recover_unprotected_trades(&state).await.unwrap();
        assert_eq!(
            fake.placed_orders.lock().await.len(),
            1,
            "ambiguous stop must not be blindly retried"
        );
        let uncertain: (String, String, i64) = sqlx::query_as("SELECT t.safety_status,o.status,(SELECT COUNT(*) FROM strategy_orders target WHERE target.trade_id=t.id AND target.role='TARGET') FROM trades t JOIN strategy_orders o ON o.trade_id=t.id AND o.role='SL1' WHERE t.id=$1")
            .bind(trade_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(uncertain.0, "PROTECTION_UNCERTAIN");
        assert_eq!(uncertain.1, "ambiguous");
        assert_eq!(
            uncertain.2, 0,
            "target remains blocked while stop is uncertain"
        );
        let incident_status: String = sqlx::query_scalar("SELECT status FROM broker_position_incidents WHERE trade_id=$1 AND incident_type='AMBIGUOUS_PROTECTION'")
            .bind(trade_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(incident_status, "operator_required");

        reconcile_live_user(&state, user_id).await.unwrap();
        recover_unprotected_trades(&state).await.unwrap();
        let recovered: (String, String, i64, i64) = sqlx::query_as("SELECT t.safety_status,stop.status,(SELECT COUNT(*) FROM strategy_orders target WHERE target.trade_id=t.id AND target.role='TARGET'),(SELECT COUNT(*) FROM broker_position_incidents i WHERE i.trade_id=t.id AND i.incident_type='AMBIGUOUS_PROTECTION' AND i.status='resolved') FROM trades t JOIN strategy_orders stop ON stop.trade_id=t.id AND stop.role='SL1' WHERE t.id=$1")
            .bind(trade_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(recovered.0, "PROTECTED");
        assert_eq!(recovered.1, "submitted");
        assert_eq!(
            recovered.2, 1,
            "target is created only after broker stop acknowledgement"
        );
        assert_eq!(recovered.3, 1);
        assert_eq!(
            fake.placed_orders.lock().await.len(),
            2,
            "one stop and one target only"
        );
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn rejected_stop_retries_deterministically_then_protects() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        fake.place_modes.lock().await.extend([
            FakePlaceMode::Reject,
            FakePlaceMode::Accept,
            FakePlaceMode::Accept,
        ]);
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (user_id, _, trade_id, token, symbol) =
            seed_live_futures_protection_fixture(&state, "sl-reject").await;
        *fake.positions.lock().await = vec![json!({
            "exchange":"MCX","symboltoken":token,"tradingsymbol":symbol,
            "producttype":"CARRYFORWARD","netqty":"50","avgnetprice":"100"
        })];
        sqlx::query("INSERT INTO market_price_ticks(exchange_segment,contract_token,price,received_at) VALUES('MCX',$1,100,NOW())")
            .bind(&token).execute(&state.db).await.unwrap();

        recover_unprotected_trades(&state).await.unwrap();
        let first: (String, String) = sqlx::query_as(
            "SELECT t.safety_status,o.status FROM trades t JOIN strategy_orders o
             ON o.trade_id=t.id AND o.role='SL1' WHERE t.id=$1",
        )
        .bind(trade_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(first, ("PROTECTION_FAILED".into(), "rejected".into()));

        recover_unprotected_trades(&state).await.unwrap();
        reconcile_live_user(&state, user_id).await.unwrap();
        recover_unprotected_trades(&state).await.unwrap();
        let final_state: (String, i64, i64) = sqlx::query_as(
            "SELECT t.safety_status,
                    (SELECT COUNT(*) FROM strategy_orders o WHERE o.trade_id=t.id AND o.role='SL1'),
                    (SELECT COUNT(*) FROM strategy_orders o WHERE o.trade_id=t.id AND o.role='TARGET')
             FROM trades t WHERE t.id=$1",
        )
        .bind(trade_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(final_state, ("PROTECTED".into(), 2, 1));
        let stop_sessions: Vec<String> = sqlx::query_scalar(
            "SELECT session_key FROM strategy_orders WHERE trade_id=$1 AND role='SL1' ORDER BY created_at",
        )
        .bind(trade_id)
        .fetch_all(&state.db)
        .await
        .unwrap();
        assert_ne!(stop_sessions[0], stop_sessions[1]);
        assert_eq!(fake.placed_orders.lock().await.len(), 2);
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn protection_restart_boundaries_converge_without_duplicate_stop() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;

        let (user_a, _, trade_a, token_a, symbol_a) =
            seed_live_futures_protection_fixture(&state, "restart-intent").await;
        *fake.positions.lock().await = vec![json!({
            "exchange":"MCX","symboltoken":token_a,"tradingsymbol":symbol_a,
            "producttype":"CARRYFORWARD","netqty":"50","avgnetprice":"100"
        })];
        execution_failpoints()
            .lock()
            .await
            .insert("after_protection_intent_before_submission");
        recover_unprotected_trades(&state).await.unwrap();
        let pending: String = sqlx::query_scalar(
            "SELECT status FROM strategy_orders WHERE trade_id=$1 AND role='SL1'",
        )
        .bind(trade_a)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(pending, "pending");
        sqlx::query("UPDATE strategy_orders SET updated_at=NOW()-INTERVAL '1 minute' WHERE trade_id=$1 AND role='SL1'")
            .bind(trade_a).execute(&state.db).await.unwrap();
        reconcile_live(&state).await.unwrap();
        recover_unprotected_trades(&state).await.unwrap();
        reconcile_live_user(&state, user_a).await.unwrap();
        recover_unprotected_trades(&state).await.unwrap();
        let intent_boundary: (String, i64, i64) = sqlx::query_as(
            "SELECT safety_status,
                    (SELECT COUNT(*) FROM strategy_orders WHERE trade_id=t.id AND role='SL1'),
                    (SELECT COUNT(*) FROM strategy_orders WHERE trade_id=t.id AND role='SL1' AND status='submitted')
             FROM trades t WHERE id=$1",
        )
        .bind(trade_a).fetch_one(&state.db).await.unwrap();
        assert_eq!(intent_boundary, ("PROTECTED".into(), 2, 1));

        let (user_b, _, trade_b, token_b, symbol_b) =
            seed_live_futures_protection_fixture(&state, "restart-accepted").await;
        fake.positions.lock().await.push(json!({
            "exchange":"MCX","symboltoken":token_b,"tradingsymbol":symbol_b,
            "producttype":"CARRYFORWARD","netqty":"50","avgnetprice":"100"
        }));
        execution_failpoints()
            .lock()
            .await
            .insert("after_broker_accept_before_local_ack");
        recover_unprotected_trades(&state).await.unwrap();
        let submitting: String = sqlx::query_scalar(
            "SELECT status FROM strategy_orders WHERE trade_id=$1 AND role='SL1'",
        )
        .bind(trade_b)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(submitting, "submitting");
        sqlx::query("UPDATE strategy_orders SET updated_at=NOW()-INTERVAL '1 minute' WHERE trade_id=$1 AND role='SL1'")
            .bind(trade_b).execute(&state.db).await.unwrap();
        reconcile_live(&state).await.unwrap();
        recover_unprotected_trades(&state).await.unwrap();
        let accepted_boundary: (String, i64, i64) = sqlx::query_as(
            "SELECT safety_status,
                    (SELECT COUNT(*) FROM strategy_orders WHERE trade_id=t.id AND role='SL1'),
                    (SELECT COUNT(*) FROM strategy_orders WHERE trade_id=t.id AND role='TARGET')
             FROM trades t WHERE id=$1",
        )
        .bind(trade_b)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(accepted_boundary, ("PROTECTED".into(), 1, 1));
        let broker_stops = fake
            .placed_orders
            .lock()
            .await
            .iter()
            .filter(|body| body.get("ordertype").and_then(Value::as_str) == Some("STOPLOSS_MARKET"))
            .count();
        assert_eq!(broker_stops, 2, "one broker stop per restart scenario");
        let _ = user_b;
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn contract_roll_retry_requires_terminal_nonfill_and_flat_position() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (user_id, snapshot_id, _, token, symbol) =
            seed_live_futures_protection_fixture(&state, "contract-roll").await;
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        let credentials = state.credentials.load(user_id).await.unwrap();
        *fake.positions.lock().await = vec![json!({
            "exchange":"MCX","symboltoken":token,"tradingsymbol":symbol,
            "producttype":"CARRYFORWARD","netqty":"10","avgnetprice":"100"
        })];
        assert!(
            !confirm_original_terminal_nonfill(
                &state,
                user_id,
                &credentials,
                &snapshot,
                "missing-client-tag"
            )
            .await
            .unwrap()
        );
        fake.positions.lock().await.clear();
        assert!(
            confirm_original_terminal_nonfill(
                &state,
                user_id,
                &credentials,
                &snapshot,
                "missing-client-tag"
            )
            .await
            .unwrap()
        );
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn stop_timeout_before_acceptance_escalates_without_conflicting_exit() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        fake.place_modes
            .lock()
            .await
            .push_back(FakePlaceMode::TimeoutBeforeAcceptance);
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (user_id, _, trade_id, token, symbol) =
            seed_live_futures_protection_fixture(&state, "sl-timeout").await;
        *fake.positions.lock().await = vec![json!({
            "exchange":"MCX","symboltoken":token,"tradingsymbol":symbol,
            "producttype":"CARRYFORWARD","netqty":"50","avgnetprice":"100"
        })];

        recover_unprotected_trades(&state).await.unwrap();
        recover_unprotected_trades(&state).await.unwrap();
        assert_eq!(fake.placed_orders.lock().await.len(), 0);
        let initial: (String, String, i64) = sqlx::query_as(
            "SELECT t.safety_status,o.status,
                    (SELECT COUNT(*) FROM strategy_orders x WHERE x.trade_id=t.id AND x.role IN ('TARGET','EMERGENCY_CLOSE'))
             FROM trades t JOIN strategy_orders o ON o.trade_id=t.id AND o.role='SL1'
             WHERE t.id=$1",
        )
        .bind(trade_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(
            initial,
            ("PROTECTION_UNCERTAIN".into(), "ambiguous".into(), 0)
        );
        sqlx::query("UPDATE strategy_orders SET uncertain_since_at=NOW()-INTERVAL '1 day' WHERE trade_id=$1 AND role='SL1'")
            .bind(trade_id).execute(&state.db).await.unwrap();
        reconcile_live_user(&state, user_id).await.unwrap();
        let escalated: (String, i64, i64) = sqlx::query_as(
            "SELECT safety_status,
                    (SELECT COUNT(*) FROM broker_position_incidents WHERE trade_id=t.id AND status='operator_required'),
                    (SELECT COUNT(*) FROM strategy_orders WHERE trade_id=t.id AND role IN ('TARGET','EMERGENCY_CLOSE'))
             FROM trades t WHERE id=$1",
        )
        .bind(trade_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(escalated.0, "PROTECTION_UNCERTAIN");
        assert!(escalated.1 >= 1);
        assert_eq!(escalated.2, 0, "no speculative conflicting exit is allowed");
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn square_off_failures_retain_history_and_restart_with_new_attempt() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (_, _, trade_id, stop_id, token, _) =
            seed_live_supertrend_square_off_fixture(&state).await;
        *fake.quote_unavailable.lock().await = true;
        let offset = FixedOffset::east_opt(19_800).unwrap();
        let local = ist_now().date_naive().and_hms_opt(15, 10, 0).unwrap();
        let now = offset.from_local_datetime(&local).single().unwrap();

        let quote_error = process_supertrend_square_off(&state, now)
            .await
            .expect_err("missing quote must fail closed");
        assert!(quote_error.to_string().contains("quote"));
        let unchanged: (String, String, String) = sqlx::query_as(
            "SELECT t.safety_status,o.status,i.status
             FROM trades t JOIN strategy_orders o ON o.id=$2
             JOIN strategy_execution_intents i ON i.trade_id=t.id AND i.action='SQUARE_OFF'
             WHERE t.id=$1",
        )
        .bind(trade_id)
        .bind(stop_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(
            unchanged,
            ("PROTECTED".into(), "submitted".into(), "retry_wait".into())
        );

        *fake.quote_unavailable.lock().await = false;
        fake.quote_ltps.lock().await.insert(token, 99.5);
        process_supertrend_square_off(&state, now).await.unwrap();
        let cancelling: String =
            sqlx::query_scalar("SELECT status FROM strategy_orders WHERE id=$1")
                .bind(stop_id)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(cancelling, "cancelling");
        sqlx::query("UPDATE strategy_orders SET status='cancelled',updated_at=NOW() WHERE id=$1")
            .bind(stop_id)
            .execute(&state.db)
            .await
            .unwrap();

        fake.place_modes
            .lock()
            .await
            .extend([FakePlaceMode::Reject, FakePlaceMode::Accept]);
        process_supertrend_square_off(&state, now)
            .await
            .expect_err("first market close rejection must remain retryable");
        process_supertrend_square_off(&state, now).await.unwrap();
        process_supertrend_square_off(&state, now).await.unwrap();
        let result: (String, String, i64, i64, String) = sqlx::query_as(
            "SELECT t.safety_status,o.status,
                    (SELECT COUNT(*) FROM strategy_orders x WHERE x.trade_id=t.id AND x.role='EMERGENCY_CLOSE'),
                    (SELECT COUNT(*) FROM strategy_orders x WHERE x.trade_id=t.id AND x.role='EMERGENCY_CLOSE' AND x.status='submitted'),
                    i.status
             FROM trades t JOIN strategy_orders o ON o.id=$2
             JOIN strategy_execution_intents i ON i.trade_id=t.id AND i.action='SQUARE_OFF'
             WHERE t.id=$1",
        )
        .bind(trade_id).bind(stop_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(result.0, "EMERGENCY_CLOSING");
        assert_eq!(result.1, "cancelled");
        assert_eq!(result.2, 2, "rejected attempt is retained beside retry");
        assert_eq!(result.3, 1, "exactly one active market close is allowed");
        assert_eq!(result.4, "submitted");
        assert_eq!(fake.placed_orders.lock().await.len(), 1);
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn supertrend_1510_demo_square_off_is_kill_safe_idempotent_and_broker_free() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (user_id, _, trade_id, _stop_id, token, _) =
            seed_live_supertrend_square_off_fixture(&state).await;
        sqlx::query("UPDATE user_profiles SET trading_mode='demo' WHERE user_id=$1")
            .bind(user_id)
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("UPDATE trades SET execution_mode='demo',safety_status='DEMO',broker_net_quantity=NULL,last_position_reconciled_at=NULL WHERE id=$1")
            .bind(trade_id).execute(&state.db).await.unwrap();
        sqlx::query(
            "UPDATE strategy_orders SET execution_mode='demo',broker_order_id='' WHERE trade_id=$1",
        )
        .bind(trade_id)
        .execute(&state.db)
        .await
        .unwrap();
        sqlx::query("UPDATE risk_kill_switches SET enabled=TRUE,reason='15:10 demo exit test'")
            .execute(&state.db)
            .await
            .unwrap();
        fake.quote_ltps.lock().await.insert(token, 101.0);
        let offset = FixedOffset::east_opt(19_800).unwrap();
        let local = ist_now().date_naive().and_hms_opt(15, 10, 0).unwrap();
        let now = offset.from_local_datetime(&local).single().unwrap();

        process_supertrend_square_off(&state, now).await.unwrap();
        process_supertrend_square_off(&state, now).await.unwrap();
        let result: (String, String, i64) = sqlx::query_as(
            "SELECT status,exit_reason,(SELECT COUNT(*) FROM strategy_orders WHERE trade_id=t.id AND role='EMERGENCY_CLOSE') FROM trades t WHERE id=$1",
        )
        .bind(trade_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(result, ("closed".into(), "MARKET_CLOSED".into(), 1));
        assert!(fake.placed_orders.lock().await.is_empty());
        assert!(fake.cancelled_orders.lock().await.is_empty());
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn multiple_entry_partial_fills_receive_exact_nonduplicated_stop_slices() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (mut buy, _) = seed_concurrent_futures_fill(&state, 9_999).await;
        sqlx::query("UPDATE users SET can_live_trade=TRUE WHERE id=$1")
            .bind(buy.user_id)
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO user_profiles(user_id,trading_mode,last_token_status) VALUES($1,'live','success')")
            .bind(buy.user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session) VALUES($1,$2,'GOLDTEN',TRUE,5,TRUE,TRUE)")
            .bind(buy.user_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        state
            .credentials
            .put(
                buy.user_id,
                &[("api_key", "fake-api"), ("jwt_token", "fake-jwt")],
            )
            .await
            .unwrap();
        crate::contract_master::set_isolated_test_cache(vec![MasterContract {
            token: "race-token".into(),
            symbol: "GOLDTEN-RACE".into(),
            name: "GOLDTEN".into(),
            expiry: "30SEP2026".into(),
            strike: "0".into(),
            lotsize: "10".into(),
            tick_size: "5.000000".into(),
            instrumenttype: "FUTCOM".into(),
            exch_seg: "MCX".into(),
        }])
        .await;

        sqlx::query("UPDATE strategy_orders SET quantity=50,filled_quantity=20,processed_quantity=0,status='processing' WHERE id=$1")
            .bind(buy.id).execute(&state.db).await.unwrap();
        buy.quantity = 20;
        buy.lots = 2;
        buy.filled_quantity = 20;
        complete_claimed_order(&state, buy.clone(), 121.0)
            .await
            .unwrap();

        sqlx::query("UPDATE strategy_orders SET filled_quantity=30,processed_quantity=20,status='processing' WHERE id=$1")
            .bind(buy.id).execute(&state.db).await.unwrap();
        buy.quantity = 10;
        buy.lots = 1;
        buy.filled_quantity = 30;
        buy.processed_quantity = 20;
        complete_claimed_order(&state, buy.clone(), 122.0)
            .await
            .unwrap();

        let trade: (Uuid, i32, i64, i64) = sqlx::query_as(
            "SELECT t.id,t.quantity,
                    (SELECT COALESCE(SUM(quantity),0) FROM strategy_orders WHERE trade_id=t.id AND role='SL1'),
                    (SELECT COUNT(*) FROM strategy_orders WHERE trade_id=t.id AND role='SL1')
             FROM trades t WHERE t.user_id=$1 AND t.status='open'",
        )
        .bind(buy.user_id).fetch_one(&state.db).await.unwrap();
        assert_eq!((trade.1, trade.2, trade.3), (30, 30, 2));
        let stop_quantities: Vec<i32> = fake
            .placed_orders
            .lock()
            .await
            .iter()
            .filter(|body| body.get("ordertype").and_then(Value::as_str) == Some("STOPLOSS_MARKET"))
            .filter_map(|body| body.get("quantity")?.as_str()?.parse().ok())
            .collect();
        assert_eq!(stop_quantities, vec![20, 10]);

        let overcoverage = sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,trigger_price,status,broker_order_id,broker_status,idempotency_key,client_order_id) VALUES($1,$2,$3,$4,'unsafe-extra-stop','SL1','SELL','STOPLOSS_MARKET','live',1,10,98,98,'pending','','',$5,'unsafe-client')")
            .bind(Uuid::new_v4()).bind(buy.user_id).bind(buy.snapshot_id).bind(trade.0)
            .bind(format!("unsafe-extra-stop-{}",trade.0)).execute(&state.db).await;
        assert!(
            overcoverage.is_err(),
            "database must reject stop coverage above local exposure"
        );
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn state_transition_guard_rejects_stale_worker_regressions() {
        let state = isolated_test_state().await;
        let (buy, sell) = seed_concurrent_futures_fill(&state, 10_001).await;
        sqlx::query("UPDATE strategy_orders SET status='filled' WHERE id=$1")
            .bind(buy.id)
            .execute(&state.db)
            .await
            .unwrap();
        let stale_fill_worker =
            sqlx::query("UPDATE strategy_orders SET status='submitted' WHERE id=$1")
                .bind(buy.id)
                .execute(&state.db)
                .await;
        assert!(stale_fill_worker.is_err());
        sqlx::query("UPDATE strategy_orders SET status='cancelled' WHERE id=$1")
            .bind(sell.id)
            .execute(&state.db)
            .await
            .unwrap();
        let stale_cancel_worker =
            sqlx::query("UPDATE strategy_orders SET status='processing' WHERE id=$1")
                .bind(sell.id)
                .execute(&state.db)
                .await;
        assert!(stale_cancel_worker.is_err());
        let statuses: Vec<String> = sqlx::query_scalar(
            "SELECT status FROM strategy_orders WHERE id IN ($1,$2) ORDER BY role",
        )
        .bind(buy.id)
        .bind(sell.id)
        .fetch_all(&state.db)
        .await
        .unwrap();
        assert_eq!(statuses, vec!["filled", "cancelled"]);

        let trade_id = Uuid::new_v4();
        sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,exit_datetime,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status) VALUES($1,$2,'live','closed','BUY',0,100,100,0,NOW(),NOW(),'GOLDTEN','GOLDTEN-RACE','terminal safety test',$3,$4,1,0,'CLOSED')")
            .bind(trade_id).bind(buy.user_id).bind(STRATEGY_KEY).bind(buy.snapshot_id)
            .execute(&state.db).await.unwrap();
        let stale_trade_worker =
            sqlx::query("UPDATE trades SET safety_status='PROTECTED' WHERE id=$1")
                .bind(trade_id)
                .execute(&state.db)
                .await;
        assert!(stale_trade_worker.is_err());
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn entry_response_loss_reconciles_by_client_tag_without_resubmission() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        fake.place_modes
            .lock()
            .await
            .push_back(FakePlaceMode::AcceptWithoutOrderId);
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (buy, _) = seed_concurrent_futures_fill(&state, 10_002).await;
        sqlx::query("DELETE FROM strategy_orders WHERE user_id=$1")
            .bind(buy.user_id)
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("UPDATE strategy_market_snapshots SET trade_date=$2 WHERE id=$1")
            .bind(buy.snapshot_id)
            .bind(ist_now().date_naive())
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET can_live_trade=TRUE WHERE id=$1")
            .bind(buy.user_id)
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO user_profiles(user_id,trading_mode,last_token_status) VALUES($1,'live','success')")
            .bind(buy.user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session) VALUES($1,$2,'GOLDTEN',TRUE,1,TRUE,TRUE)")
            .bind(buy.user_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_strategy_activations(user_id,strategy_key,is_active) VALUES($1,$2,TRUE)")
            .bind(buy.user_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        state
            .credentials
            .put(
                buy.user_id,
                &[("api_key", "fake-api"), ("jwt_token", "fake-jwt")],
            )
            .await
            .unwrap();
        let expiry: NaiveDate =
            sqlx::query_scalar("SELECT contract_expiry FROM strategy_market_snapshots WHERE id=$1")
                .bind(buy.snapshot_id)
                .fetch_one(&state.db)
                .await
                .unwrap();
        crate::contract_master::set_isolated_test_cache(vec![MasterContract {
            token: "race-token".into(),
            symbol: "GOLDTEN-RACE".into(),
            name: "GOLDTEN".into(),
            expiry: expiry.format("%d%b%Y").to_string().to_uppercase(),
            strike: "0".into(),
            lotsize: "10".into(),
            tick_size: "5.000000".into(),
            instrumenttype: "FUTCOM".into(),
            exch_seg: "MCX".into(),
        }])
        .await;
        sqlx::query("INSERT INTO market_price_ticks(exchange_segment,contract_token,price,received_at) VALUES('MCX','race-token',121,NOW())")
            .execute(&state.db).await.unwrap();
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(buy.snapshot_id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        let runner = runner_for(&state, buy.user_id, "GOLDTEN").await.unwrap();
        let result = place_strategy_order(
            &state,
            &runner,
            &snapshot,
            "entry-response-loss",
            NewOrder {
                role: "BUY_ENTRY",
                side: "BUY",
                order_type: "STOPLOSS_LIMIT",
                lots: 1,
                price: 121.0,
                trigger: Some(121.0),
                trade_id: None,
                quantity: Some(10),
            },
        )
        .await;
        let placement_error = result.expect_err("missing acknowledgement must be ambiguous");
        let ambiguous: String =
            sqlx::query_scalar("SELECT status FROM strategy_orders WHERE user_id=$1")
                .bind(buy.user_id)
                .fetch_optional(&state.db)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("entry failed before reservation: {placement_error}"));
        assert_eq!(ambiguous, "ambiguous");
        reconcile_live_user(&state, buy.user_id).await.unwrap();
        let reconciled: (String, String) =
            sqlx::query_as("SELECT status,broker_order_id FROM strategy_orders WHERE user_id=$1")
                .bind(buy.user_id)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(reconciled.0, "submitted");
        assert!(!reconciled.1.is_empty());
        assert_eq!(
            fake.placed_orders.lock().await.len(),
            1,
            "accepted entry is never resent"
        );
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn fill_commit_crash_recovers_protection_on_restart() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (buy, sell) = seed_concurrent_futures_fill(&state, 10_003).await;
        sqlx::query("DELETE FROM strategy_orders WHERE id=$1")
            .bind(sell.id)
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET can_live_trade=TRUE WHERE id=$1")
            .bind(buy.user_id)
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO user_profiles(user_id,trading_mode,last_token_status) VALUES($1,'live','success')")
            .bind(buy.user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session) VALUES($1,$2,'GOLDTEN',TRUE,1,TRUE,TRUE)")
            .bind(buy.user_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        state
            .credentials
            .put(
                buy.user_id,
                &[("api_key", "fake-api"), ("jwt_token", "fake-jwt")],
            )
            .await
            .unwrap();
        crate::contract_master::set_isolated_test_cache(vec![MasterContract {
            token: "race-token".into(),
            symbol: "GOLDTEN-RACE".into(),
            name: "GOLDTEN".into(),
            expiry: "30SEP2026".into(),
            strike: "0".into(),
            lotsize: "10".into(),
            tick_size: "5.000000".into(),
            instrumenttype: "FUTCOM".into(),
            exch_seg: "MCX".into(),
        }])
        .await;
        execution_failpoints()
            .lock()
            .await
            .insert("after_fill_commit_before_protection");
        let crash = complete_claimed_order(&state, buy.clone(), 121.0).await;
        assert!(crash.is_err());
        let committed: (Uuid, String, i64) = sqlx::query_as(
            "SELECT t.id,t.safety_status,(SELECT COUNT(*) FROM strategy_orders o WHERE o.trade_id=t.id AND o.role='SL1')
             FROM trades t WHERE t.user_id=$1 AND t.status='open'",
        )
        .bind(buy.user_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(
            (committed.1.as_str(), committed.2),
            ("PROTECTION_REQUIRED", 0)
        );
        *fake.positions.lock().await = vec![json!({
            "exchange":"MCX","symboltoken":"race-token","tradingsymbol":"GOLDTEN-RACE",
            "producttype":"CARRYFORWARD","netqty":"10","avgnetprice":"121"
        })];
        recover_unprotected_trades(&state).await.unwrap();
        reconcile_live_user(&state, buy.user_id).await.unwrap();
        recover_unprotected_trades(&state).await.unwrap();
        let restarted: (String, i64, i64) = sqlx::query_as(
            "SELECT safety_status,
                    (SELECT COUNT(*) FROM strategy_orders WHERE trade_id=t.id AND role='SL1'),
                    (SELECT COUNT(*) FROM strategy_orders WHERE trade_id=t.id AND role='TARGET')
             FROM trades t WHERE id=$1",
        )
        .bind(committed.0)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(restarted, ("PROTECTED".into(), 1, 1));
        assert_eq!(fake.placed_orders.lock().await.len(), 2);
        broker_task.abort();
    }

    #[test]
    fn live_submission_guard_rechecks_kills_account_mode_and_session() {
        let safe = Some((true, true, "live", "success"));
        assert_eq!(
            live_submission_rejection(false, false, false, safe, true, true),
            None
        );
        assert_eq!(
            live_submission_rejection(false, true, false, safe, true, true).map(|value| value.0),
            Some("global_kill_switch")
        );
        assert_eq!(
            live_submission_rejection(
                false,
                false,
                false,
                Some((true, false, "live", "success")),
                true,
                true,
            )
            .map(|value| value.0),
            Some("live_permission_revoked")
        );
        assert_eq!(
            live_submission_rejection(
                false,
                false,
                false,
                Some((true, true, "demo", "success")),
                true,
                true,
            )
            .map(|value| value.0),
            Some("trading_mode_changed")
        );
        assert_eq!(
            live_submission_rejection(false, false, false, safe, false, true).map(|value| value.0),
            Some("broker_session_missing")
        );
        assert_eq!(
            live_submission_rejection(false, false, false, safe, true, false).map(|value| value.0),
            Some("broker_reconciliation")
        );
    }

    #[test]
    fn calendar_lookup_failure_is_not_reclassified_as_normal_market_close() {
        assert_eq!(
            scheduler_session_flags(Ok(((false, "Holiday".into()), (false, "Holiday".into()))))
                .unwrap(),
            (false, false)
        );
        let error = scheduler_session_flags(Err(AppError::BadRequest(
            "calendar database unavailable".into(),
        )))
        .expect_err("calendar errors must remain distinguishable and fail closed");
        assert!(error.to_string().contains("calendar database unavailable"));
    }

    #[test]
    fn durable_entry_intents_retry_only_transient_failures() {
        assert!(entry_intent_retryable("Angel One API rate limit is active"));
        assert!(entry_intent_retryable(
            "no fresh valid market price is available"
        ));
        assert!(entry_intent_retryable(
            "selected contract is unavailable at Angel One"
        ));
        assert!(!entry_intent_retryable(
            "Order rejected by the user kill switch"
        ));
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn admin_global_kill_switch_is_authorized_idempotent_persistent_and_audited() {
        let state = isolated_test_state().await;
        let admin_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,username,email,password_hash,can_administer) VALUES($1,'kill-admin','kill-admin@example.test','test-only',TRUE)")
            .bind(admin_id).execute(&state.db).await.unwrap();
        let principal = crate::auth::AuthUser {
            id: admin_id,
            username: "kill-admin".into(),
            can_administer: true,
            can_live_trade: false,
            can_backtest: false,
            can_backtest_on_trading_days: false,
            trading_mode: "demo".into(),
            session_id: Uuid::new_v4(),
        };
        let normal = crate::auth::AuthUser {
            can_administer: false,
            ..principal.clone()
        };
        let headers = axum::http::HeaderMap::new();

        let forbidden = risk::update_kill(
            &state,
            &normal,
            &headers,
            None,
            None,
            &risk::KillUpdate {
                enabled: true,
                reason: None,
            },
        )
        .await;
        assert!(matches!(forbidden, Err(AppError::Forbidden(_))));

        for enabled in [true, true, false, false] {
            risk::update_kill(
                &state,
                &principal,
                &headers,
                None,
                None,
                &risk::KillUpdate {
                    enabled,
                    reason: Some(format!("set {enabled}")),
                },
            )
            .await
            .unwrap();
            let stored: bool =
                sqlx::query_scalar("SELECT enabled FROM risk_kill_switches WHERE user_id IS NULL")
                    .fetch_one(&state.db)
                    .await
                    .unwrap();
            assert_eq!(stored, enabled);
        }
        let audit: Vec<(bool, bool, bool, bool)> = sqlx::query_as(
            "SELECT (metadata->>'previous_state')::bool,(metadata->>'requested_state')::bool,(metadata->>'resulting_state')::bool,(metadata->>'state_changed')::bool FROM audit_events WHERE event_type='admin_global_kill_switch_set' ORDER BY created_at,id",
        )
        .fetch_all(&state.db)
        .await
        .unwrap();
        assert_eq!(
            audit,
            vec![
                (false, true, true, true),
                (true, true, true, false),
                (true, false, false, true),
                (false, false, false, false),
            ]
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn admin_clear_trades_removes_running_demo_graph_and_fences_stale_execution() {
        let state = isolated_test_state().await;
        let user_a = Uuid::new_v4();
        let user_b = Uuid::new_v4();
        let snapshot_a = Uuid::new_v4();
        let snapshot_b = Uuid::new_v4();
        for (user_id, username) in [(user_a, "clear-a"), (user_b, "clear-b")] {
            sqlx::query(
                "INSERT INTO users(id,username,email,password_hash) VALUES($1,$2,$3,'test-only')",
            )
            .bind(user_id)
            .bind(username)
            .bind(format!("{username}@example.test"))
            .execute(&state.db)
            .await
            .unwrap();
            sqlx::query("INSERT INTO user_profiles(user_id,trading_mode) VALUES($1,'demo')")
                .bind(user_id)
                .execute(&state.db)
                .await
                .unwrap();
            sqlx::query("INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots) VALUES($1,$2,'GOLDTEN',TRUE,1)")
                .bind(user_id)
                .bind(STRATEGY_KEY)
                .execute(&state.db)
                .await
                .unwrap();
            sqlx::query("INSERT INTO user_strategy_activations(user_id,strategy_key,is_active,activated_at) VALUES($1,$2,TRUE,NOW())")
                .bind(user_id)
                .bind(STRATEGY_KEY)
                .execute(&state.db)
                .await
                .unwrap();
        }
        for (snapshot_id, token, key) in [
            (snapshot_a, "clear-token-a", "clear-a"),
            (snapshot_b, "clear-token-b", "clear-b"),
        ] {
            sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready',$3,$4,CURRENT_DATE+30,10,'MCX','CARRYFORWARD',$5,101,102,99,98,89,88,91,92)")
                .bind(snapshot_id)
                .bind(STRATEGY_KEY)
                .bind(token)
                .bind(format!("GOLDTEN-{key}"))
                .bind(key)
                .execute(&state.db)
                .await
                .unwrap();
        }
        let buy_trade = Uuid::new_v4();
        let sell_trade = Uuid::new_v4();
        let closed_trade = Uuid::new_v4();
        let live_open_trade = Uuid::new_v4();
        let live_closed_trade = Uuid::new_v4();
        for (trade_id, mode, status, direction, snapshot_id, user_id) in [
            (buy_trade, "demo", "open", "BUY", snapshot_a, user_a),
            (sell_trade, "demo", "open", "SELL", snapshot_a, user_a),
            (closed_trade, "demo", "closed", "BUY", snapshot_a, user_a),
            (live_open_trade, "live", "open", "BUY", snapshot_b, user_a),
            (
                live_closed_trade,
                "live",
                "closed",
                "SELL",
                snapshot_b,
                user_a,
            ),
            (Uuid::new_v4(), "demo", "open", "SELL", snapshot_b, user_b),
        ] {
            sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,exit_datetime,instrument_label,contract_symbol,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,sl2_price,safety_status) VALUES($1,$2,$3,$4,$5,10,100,100,0,NOW(),CASE WHEN $4='closed' THEN NOW() END,'GOLDTEN','GOLDTEN-TEST',$6,$7,1,CASE WHEN $4='open' THEN 1 ELSE 0 END,102,99,98,CASE WHEN $3='demo' THEN 'DEMO' WHEN $4='closed' THEN 'CLOSED' ELSE 'PROTECTED' END)")
                .bind(trade_id)
                .bind(user_id)
                .bind(mode)
                .bind(status)
                .bind(direction)
                .bind(STRATEGY_KEY)
                .bind(snapshot_id)
                .execute(&state.db)
                .await
                .unwrap();
        }
        let demo_orders = [
            (
                Uuid::new_v4(),
                Some(buy_trade),
                "BUY_ENTRY",
                "BUY",
                "partially_filled",
                5,
                0,
            ),
            (
                Uuid::new_v4(),
                Some(buy_trade),
                "SL1",
                "SELL",
                "submitted",
                10,
                0,
            ),
            (
                Uuid::new_v4(),
                Some(buy_trade),
                "TARGET",
                "SELL",
                "submitted",
                10,
                0,
            ),
            (
                Uuid::new_v4(),
                Some(sell_trade),
                "SELL_ENTRY",
                "SELL",
                "pending",
                10,
                0,
            ),
            (
                Uuid::new_v4(),
                Some(closed_trade),
                "BUY_ENTRY",
                "BUY",
                "filled",
                10,
                10,
            ),
        ];
        for (index, (order_id, trade_id, role, side, status, filled, processed)) in
            demo_orders.iter().enumerate()
        {
            sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,trigger_price,status,broker_order_id,idempotency_key,client_order_id,filled_quantity,processed_quantity) VALUES($1,$2,$3,$4,$5,$6,$7,'STOPLOSS_LIMIT','demo',1,10,100,100,$8,$9,$10,$11,$12,$13)")
                .bind(order_id).bind(user_a).bind(snapshot_a).bind(trade_id)
                .bind(format!("clear-session-{index}")).bind(role).bind(side).bind(status)
                .bind(format!("DEMO-{order_id}")).bind(format!("clear-key-{index}"))
                .bind(format!("CLEAR{index}")).bind(filled).bind(processed)
                .execute(&state.db).await.unwrap();
        }
        let user_b_order = Uuid::new_v4();
        sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,status,broker_order_id,idempotency_key,client_order_id) VALUES($1,$2,$3,'user-b','SELL_ENTRY','SELL','STOPLOSS_LIMIT','demo',1,10,90,'submitted',$4,$5,'USERB')")
            .bind(user_b_order).bind(user_b).bind(snapshot_b).bind(format!("DEMO-{user_b_order}")).bind(format!("user-b-{user_b_order}"))
            .execute(&state.db).await.unwrap();
        let live_order = Uuid::new_v4();
        sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,status,broker_order_id,idempotency_key,client_order_id,filled_quantity,processed_quantity) VALUES($1,$2,$3,$4,'live-history','BUY_ENTRY','BUY','MARKET','live',1,10,100,'filled','LIVE-HISTORY','live-history-key','LIVEHISTORY',10,10)")
            .bind(live_order).bind(user_a).bind(snapshot_b).bind(live_closed_trade)
            .execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO broker_secrets(user_id,secret_kind,key_version,nonce,ciphertext) VALUES($1,'api_key',1,decode('000000000000000000000000','hex'),decode('00','hex'))")
            .bind(user_a).execute(&state.db).await.unwrap();
        let egress_id: Uuid = sqlx::query_scalar("INSERT INTO broker_egress_ips(ip_address,configuration_status,verification_status) VALUES('192.0.2.10','CONFIGURED','VERIFIED') RETURNING id")
            .fetch_one(&state.db).await.unwrap();
        sqlx::query("UPDATE user_profiles SET broker_egress_ip_id=$2 WHERE user_id=$1")
            .bind(user_a)
            .bind(egress_id)
            .execute(&state.db)
            .await
            .unwrap();
        let old_signal = Uuid::new_v4();
        let old_signal_at = Utc::now() - Duration::minutes(5);
        sqlx::query("INSERT INTO strategy_signals(id,strategy_key,instrument,session_key,signal_at,snapshot_id,signal_type,status,expected_users) VALUES($1,$2,'GOLDTEN','clear-old',$3,$4,'ENTRY','dispatching',1)")
            .bind(old_signal).bind(STRATEGY_KEY).bind(old_signal_at).bind(snapshot_a)
            .execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO strategy_execution_intents(id,signal_id,user_id,snapshot_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,price,status) VALUES($1,$2,$3,$4,$5,'GOLDTEN','clear-old','ENTRY','BUY_ENTRY','BUY','STOPLOSS_LIMIT',1,101,'claimed')")
            .bind(Uuid::new_v4()).bind(old_signal).bind(user_a).bind(snapshot_a).bind(STRATEGY_KEY)
            .execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO strategy_events(user_id,strategy_key,instrument,event_type,payload) VALUES($1,$2,'GOLDTEN','position_opened',jsonb_build_object('trade_id',$3::text,'mode','demo'))")
            .bind(user_a).bind(STRATEGY_KEY).bind(buy_trade).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO risk_decisions(id,user_id,execution_mode,order_role,allowed,reason_code,message) VALUES($1,$2,'demo','BUY_ENTRY',TRUE,'allowed','demo fixture')")
            .bind(Uuid::new_v4()).bind(user_a).execute(&state.db).await.unwrap();
        sqlx::query("UPDATE risk_kill_switches SET enabled=TRUE,reason='clear test'")
            .execute(&state.db)
            .await
            .unwrap();

        let mut tx = state.db.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtext('rulenix:risk:global'))")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::uuid::text,0))")
            .bind(user_a)
            .execute(&mut *tx)
            .await
            .unwrap();
        let race_state = state.clone();
        let stale_race = tokio::spawn(async move {
            risk::assess_and_reserve(
                &race_state,
                &risk::OrderRisk {
                    user_id: user_a,
                    snapshot_id: snapshot_b,
                    trade_id: None,
                    session: "stale-racing",
                    role: "BUY_ENTRY",
                    side: "BUY",
                    mode: "demo",
                    lots: 1,
                    quantity: 10,
                    price: 101.0,
                    trigger_price: Some(101.0),
                    idempotency_key: "stale-racing",
                    snapshot_ready: true,
                    snapshot_current: true,
                    exchange_segment: "MCX",
                    contract_token: "clear-token-b",
                    live_reconciled: true,
                    originated_at: Some(old_signal_at),
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        let cleared = crate::auth::clear_user_demo_trade_state(&mut tx, user_a)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let stale_race_error = stale_race
            .await
            .unwrap()
            .expect_err("an execution waiting at the user lock must be fenced by the reset");
        assert!(
            stale_race_error
                .to_string()
                .contains("predates the latest Admin Clear Trades reset")
        );
        assert_eq!(cleared.deleted_demo_trades, 3);
        assert_eq!(cleared.deleted_demo_orders, 5);
        assert_eq!(cleared.deleted_demo_intents, 0);
        assert_eq!(cleared.skipped_ambiguous_intents, 1);
        assert_eq!(cleared.deleted_demo_events, 1);
        assert_eq!(cleared.deleted_demo_risk_decisions, 1);
        assert_eq!(cleared.deleted_orphan_signals, 0);
        assert_eq!(cleared.deleted_orphan_snapshots, 0);
        let remaining_demo: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM trades WHERE user_id=$1 AND execution_mode='demo'),(SELECT COUNT(*) FROM strategy_orders WHERE user_id=$1 AND execution_mode='demo'),(SELECT COUNT(*) FROM strategy_execution_intents WHERE user_id=$1 AND status IN ('pending','claimed','retry_wait','submitted')),(SELECT COUNT(*) FROM risk_decisions WHERE user_id=$1 AND execution_mode='demo')")
            .bind(user_a).fetch_one(&state.db).await.unwrap();
        assert_eq!(remaining_demo, (0, 0, 0, 0));
        let ambiguous_intent: (i64, i64) = sqlx::query_as("SELECT COUNT(*),COUNT(*) FILTER (WHERE status='skipped' AND last_error LIKE 'Preserved and terminalized%') FROM strategy_execution_intents WHERE user_id=$1")
            .bind(user_a).fetch_one(&state.db).await.unwrap();
        assert_eq!(ambiguous_intent, (1, 1));
        let live_preserved: (i64, i64) = sqlx::query_as("SELECT COUNT(*) FILTER (WHERE status='open'),COUNT(*) FILTER (WHERE status='closed') FROM trades WHERE user_id=$1 AND execution_mode='live'")
            .bind(user_a).fetch_one(&state.db).await.unwrap();
        assert_eq!(live_preserved, (1, 1));
        let protected_records: (i64, i64, Option<Uuid>) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM strategy_orders WHERE user_id=$1 AND execution_mode='live'),(SELECT COUNT(*) FROM broker_secrets WHERE user_id=$1),broker_egress_ip_id FROM user_profiles WHERE user_id=$1")
            .bind(user_a).fetch_one(&state.db).await.unwrap();
        assert_eq!(protected_records, (1, 1, Some(egress_id)));
        let user_b_state: (i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM trades WHERE user_id=$1 AND execution_mode='demo'),(SELECT COUNT(*) FROM strategy_orders WHERE user_id=$1 AND execution_mode='demo')")
            .bind(user_b).fetch_one(&state.db).await.unwrap();
        assert_eq!(user_b_state, (1, 1));
        let preserved_configuration: (String, bool, bool) = sqlx::query_as("SELECT p.trading_mode,c.enabled,a.is_active FROM user_profiles p JOIN user_strategy_configs c ON c.user_id=p.user_id JOIN user_strategy_activations a ON a.user_id=p.user_id AND a.strategy_key=c.strategy_key WHERE p.user_id=$1")
            .bind(user_a).fetch_one(&state.db).await.unwrap();
        assert_eq!(preserved_configuration, ("demo".into(), true, true));

        reconcile_live(&state).await.unwrap();
        let after_reconcile: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM trades WHERE user_id=$1 AND execution_mode='demo'",
        )
        .bind(user_a)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(after_reconcile, 0);

        sqlx::query("UPDATE risk_kill_switches SET enabled=FALSE")
            .execute(&state.db)
            .await
            .unwrap();
        let stale = risk::assess_and_reserve(
            &state,
            &risk::OrderRisk {
                user_id: user_a,
                snapshot_id: snapshot_b,
                trade_id: None,
                session: "stale-old",
                role: "BUY_ENTRY",
                side: "BUY",
                mode: "demo",
                lots: 1,
                quantity: 10,
                price: 101.0,
                trigger_price: Some(101.0),
                idempotency_key: "stale-old",
                snapshot_ready: true,
                snapshot_current: true,
                exchange_segment: "MCX",
                contract_token: "clear-token-b",
                live_reconciled: true,
                originated_at: Some(old_signal_at),
            },
        )
        .await
        .expect_err("pre-reset signal must remain fenced after the kill switch is later disabled");
        assert!(
            stale
                .to_string()
                .contains("predates the latest Admin Clear Trades reset")
        );
        let reset_at: chrono::DateTime<Utc> =
            sqlx::query_scalar("SELECT demo_state_reset_at FROM user_profiles WHERE user_id=$1")
                .bind(user_a)
                .fetch_one(&state.db)
                .await
                .unwrap();
        sqlx::query("INSERT INTO market_price_ticks(exchange_segment,contract_token,price,received_at) VALUES('MCX','clear-token-b',101,NOW()) ON CONFLICT(exchange_segment,contract_token) DO UPDATE SET price=EXCLUDED.price,received_at=NOW()")
            .execute(&state.db).await.unwrap();
        let fresh = risk::assess_and_reserve(
            &state,
            &risk::OrderRisk {
                user_id: user_a,
                snapshot_id: snapshot_b,
                trade_id: None,
                session: "fresh-new",
                role: "BUY_ENTRY",
                side: "BUY",
                mode: "demo",
                lots: 1,
                quantity: 10,
                price: 101.0,
                trigger_price: Some(101.0),
                idempotency_key: "fresh-new",
                snapshot_ready: true,
                snapshot_current: true,
                exchange_segment: "MCX",
                contract_token: "clear-token-b",
                live_reconciled: true,
                originated_at: Some(reset_at + Duration::milliseconds(1)),
            },
        )
        .await
        .unwrap();
        assert!(
            fresh.is_some(),
            "a genuinely new post-reset signal remains eligible"
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn admin_clear_trades_without_broker_verification_clears_demo_and_preserves_live() {
        let state = isolated_test_state().await;
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,username,email,password_hash) VALUES($1,'clear-unverified','clear-unverified@example.test','test-only')")
            .bind(user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_profiles(user_id,trading_mode) VALUES($1,'demo')")
            .bind(user_id)
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,strategy_key,total_lots,remaining_lots,safety_status) VALUES($1,$2,'demo','open','BUY',10,100,100,0,NOW(),'GOLDTEN',$3,1,1,'DEMO'),($4,$2,'live','closed','BUY',10,100,101,10,NOW()-INTERVAL '2 days','NIFTY','option_entry_v1',1,0,'CLOSED')")
            .bind(Uuid::new_v4()).bind(user_id).bind(STRATEGY_KEY).bind(Uuid::new_v4())
            .execute(&state.db).await.unwrap();
        let active_before: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trades WHERE user_id=$1 AND status='open') OR EXISTS(SELECT 1 FROM strategy_orders WHERE user_id=$1 AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')) OR EXISTS(SELECT 1 FROM strategy_execution_intents WHERE user_id=$1 AND action='ENTRY' AND status IN ('pending','claimed','retry_wait','submitted'))")
            .bind(user_id).fetch_one(&state.db).await.unwrap();
        assert!(active_before, "running DEMO state must block a mode change");

        let mut tx = state.db.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtext('rulenix:risk:global'))")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::uuid::text,0))")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        let cleared = crate::auth::clear_user_demo_trade_state(&mut tx, user_id)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(cleared.deleted_demo_trades, 1);
        let remaining: (i64, i64) = sqlx::query_as("SELECT COUNT(*) FILTER (WHERE execution_mode='demo'),COUNT(*) FILTER (WHERE execution_mode='live' AND status='closed') FROM trades WHERE user_id=$1")
            .bind(user_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(remaining, (0, 1));
        let active_after: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trades WHERE user_id=$1 AND status='open') OR EXISTS(SELECT 1 FROM strategy_orders WHERE user_id=$1 AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')) OR EXISTS(SELECT 1 FROM strategy_execution_intents WHERE user_id=$1 AND action='ENTRY' AND status IN ('pending','claimed','retry_wait','submitted'))")
            .bind(user_id).fetch_one(&state.db).await.unwrap();
        assert!(
            !active_after,
            "legitimate demo cleanup must remove only the active-state blocker"
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn admin_clear_live_and_all_are_scoped_idempotent_and_preserve_account_configuration() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let admin_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        for (id, username, admin) in [
            (admin_id, "clear-scope-admin", true),
            (user_id, "clear-scope-user", false),
            (other_id, "clear-scope-other", false),
        ] {
            sqlx::query("INSERT INTO users(id,username,email,password_hash,can_administer) VALUES($1,$2,$3,'test-only',$4)")
                .bind(id).bind(username).bind(format!("{username}@example.test")).bind(admin)
                .execute(&state.db).await.unwrap();
            sqlx::query("INSERT INTO user_profiles(user_id,trading_mode) VALUES($1,'demo')")
                .bind(id)
                .execute(&state.db)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready','','clear-live-token','CLEARLIVEFUT',CURRENT_DATE+30,10,'MCX','CARRYFORWARD','clear-live-safe')")
            .bind(snapshot_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        let live_trade = Uuid::new_v4();
        let demo_trade = Uuid::new_v4();
        let other_trade = Uuid::new_v4();
        for (trade_id, owner, mode) in [
            (live_trade, user_id, "live"),
            (demo_trade, user_id, "demo"),
            (other_trade, other_id, "live"),
        ] {
            sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,exit_datetime,instrument_label,contract_symbol,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status,broker_net_quantity,last_position_reconciled_at) VALUES($1,$2,$3,'closed','BUY',10,100,101,10,NOW()-INTERVAL '1 day',NOW(),'GOLDTEN','CLEARLIVEFUT',$4,$5,1,0,CASE WHEN $3='live' THEN 'CLOSED' ELSE 'DEMO' END,CASE WHEN $3='live' THEN 0 END,CASE WHEN $3='live' THEN NOW() END)")
                .bind(trade_id).bind(owner).bind(mode).bind(STRATEGY_KEY).bind(snapshot_id)
                .execute(&state.db).await.unwrap();
        }
        let live_order = Uuid::new_v4();
        sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,status,broker_order_id,idempotency_key,client_order_id,filled_quantity,processed_quantity) VALUES($1,$2,$3,$4,'clear-live-terminal','BUY_ENTRY','BUY','MARKET','live',1,10,100,'filled','CLEAR-LIVE-BROKER','clear-live-key','CLEARLIVETAG',10,10)")
            .bind(live_order).bind(user_id).bind(snapshot_id).bind(live_trade)
            .execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_strategy_configs(user_id,strategy_key,instrument,enabled,lots) VALUES($1,$2,'GOLDTEN',TRUE,2)")
            .bind(user_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        let egress_id: Uuid = sqlx::query_scalar("INSERT INTO broker_egress_ips(ip_address,configuration_status,verification_status) VALUES('192.0.2.44','CONFIGURED','VERIFIED') RETURNING id")
            .fetch_one(&state.db).await.unwrap();
        state
            .credentials
            .put(
                user_id,
                &[("api_key", "clear-api"), ("jwt_token", "clear-jwt")],
            )
            .await
            .unwrap();
        sqlx::query("UPDATE risk_kill_switches SET enabled=TRUE,reason='clear scope test'")
            .execute(&state.db)
            .await
            .unwrap();
        let admin = AuthUser {
            id: admin_id,
            username: "clear-scope-admin".into(),
            can_administer: true,
            can_live_trade: false,
            can_backtest: false,
            can_backtest_on_trading_days: false,
            trading_mode: "demo".into(),
            session_id: Uuid::new_v4(),
        };

        sqlx::query("UPDATE risk_kill_switches SET enabled=FALSE")
            .execute(&state.db)
            .await
            .unwrap();
        for scope in [
            crate::auth::ClearTradeScope::Demo,
            crate::auth::ClearTradeScope::Live,
            crate::auth::ClearTradeScope::All,
        ] {
            let error = crate::auth::clear_user_trade_logs(
                State(state.clone()),
                Extension(admin.clone()),
                HeaderMap::new(),
                None,
                Json(crate::auth::AdminClearTradesMutation {
                    username: "clear-scope-user".into(),
                    scope,
                }),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("global kill switch"));
        }
        sqlx::query("UPDATE risk_kill_switches SET enabled=TRUE")
            .execute(&state.db)
            .await
            .unwrap();
        let forbidden = crate::auth::clear_user_trade_logs(
            State(state.clone()),
            Extension(AuthUser {
                can_administer: false,
                ..admin.clone()
            }),
            HeaderMap::new(),
            None,
            Json(crate::auth::AdminClearTradesMutation {
                username: "clear-scope-user".into(),
                scope: crate::auth::ClearTradeScope::Demo,
            }),
        )
        .await;
        assert!(matches!(forbidden, Err(AppError::Forbidden(_))));

        for expected_deleted in [1_u64, 0] {
            let response = crate::auth::clear_user_trade_logs(
                State(state.clone()),
                Extension(admin.clone()),
                HeaderMap::new(),
                None,
                Json(crate::auth::AdminClearTradesMutation {
                    username: "clear-scope-user".into(),
                    scope: crate::auth::ClearTradeScope::Live,
                }),
            )
            .await
            .unwrap();
            assert_eq!(response.0["deleted_live_trades"], expected_deleted);
        }
        let scoped: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM trades WHERE user_id=$1 AND execution_mode='live'),(SELECT COUNT(*) FROM trades WHERE user_id=$1 AND execution_mode='demo'),(SELECT COUNT(*) FROM trades WHERE user_id=$2)")
            .bind(user_id).bind(other_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(scoped, (0, 1, 1));
        let preserved: (i64, Option<Uuid>, i64, i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM broker_secrets WHERE user_id=$1),p.broker_egress_ip_id,(SELECT COUNT(*) FROM user_strategy_configs WHERE user_id=$1),(SELECT COUNT(*) FROM users WHERE id=$1),(SELECT COUNT(*) FROM broker_egress_ips WHERE id=$2) FROM user_profiles p WHERE p.user_id=$1")
            .bind(user_id).bind(egress_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(preserved, (2, None, 1, 1, 1));

        let all = crate::auth::clear_user_trade_logs(
            State(state.clone()),
            Extension(admin),
            HeaderMap::new(),
            None,
            Json(crate::auth::AdminClearTradesMutation {
                username: "clear-scope-user".into(),
                scope: crate::auth::ClearTradeScope::All,
            }),
        )
        .await
        .unwrap();
        assert_eq!(all.0["deleted_demo_trades"], 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM trades WHERE user_id=$1")
                .bind(user_id)
                .fetch_one(&state.db)
                .await
                .unwrap(),
            0
        );
        assert!(fake.placed_orders.lock().await.is_empty());
        assert!(fake.cancelled_orders.lock().await.is_empty());
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn admin_clear_live_fails_closed_for_broker_or_durable_exposure_without_mutations() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let user_id = seed_flat_linked_live_account(&state, "clear-live-blocked").await;
        sqlx::query("UPDATE risk_kill_switches SET enabled=TRUE,reason='clear blocked test'")
            .execute(&state.db)
            .await
            .unwrap();
        let invoke = || {
            crate::auth::clear_user_trade_logs(
                State(state.clone()),
                Extension(AuthUser {
                    id: Uuid::new_v4(),
                    username: "test-admin".into(),
                    can_administer: true,
                    can_live_trade: false,
                    can_backtest: false,
                    can_backtest_on_trading_days: false,
                    trading_mode: "demo".into(),
                    session_id: Uuid::new_v4(),
                }),
                HeaderMap::new(),
                None,
                Json(crate::auth::AdminClearTradesMutation {
                    username: "clear-live-blocked-user".into(),
                    scope: crate::auth::ClearTradeScope::Live,
                }),
            )
        };

        *fake.positions.lock().await = vec![
            json!({"exchange":"MCX","symboltoken":"1","tradingsymbol":"OPEN","producttype":"CARRYFORWARD","netqty":"10","avgnetprice":"100"}),
        ];
        assert!(
            invoke()
                .await
                .unwrap_err()
                .to_string()
                .contains("open broker position")
        );
        fake.positions.lock().await.clear();
        *fake.order_book.lock().await = vec![json!({"orderid":"ACTIVE","status":"open"})];
        assert!(
            invoke()
                .await
                .unwrap_err()
                .to_string()
                .contains("broker order")
        );
        fake.order_book.lock().await.clear();
        *fake.conditional_rules.lock().await = vec![json!({"id":"GTT-1","status":"ACTIVE"})];
        assert!(
            invoke()
                .await
                .unwrap_err()
                .to_string()
                .contains("conditional/GTT")
        );
        fake.conditional_rules.lock().await.clear();
        *fake.conditional_unavailable.lock().await = true;
        assert!(
            invoke()
                .await
                .unwrap_err()
                .to_string()
                .contains("unreadable")
        );
        *fake.conditional_unavailable.lock().await = false;
        sqlx::query("INSERT INTO broker_reconciliation_blockers(user_id,status,external_active_orders,detail) VALUES($1,'open',1,'test unresolved mutation')")
            .bind(user_id).execute(&state.db).await.unwrap();
        assert!(
            invoke()
                .await
                .unwrap_err()
                .to_string()
                .contains("durable LIVE exposure")
        );
        assert!(fake.placed_orders.lock().await.is_empty());
        assert!(fake.cancelled_orders.lock().await.is_empty());
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn admin_clear_trades_rolls_back_atomically() {
        let state = isolated_test_state().await;
        let user_id = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        let trade_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,username,email,password_hash) VALUES($1,'clear-rollback','clear-rollback@example.test','test-only')")
            .bind(user_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_profiles(user_id,trading_mode) VALUES($1,'demo')")
            .bind(user_id)
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,contract_token,contract_symbol,lot_size) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready','rollback-token','ROLLBACK',10)")
            .bind(snapshot_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status) VALUES($1,$2,'demo','open','BUY',10,100,100,0,NOW(),'GOLDTEN',$3,$4,1,1,'DEMO')")
            .bind(trade_id).bind(user_id).bind(STRATEGY_KEY).bind(snapshot_id).execute(&state.db).await.unwrap();
        let mut tx = state.db.begin().await.unwrap();
        crate::auth::clear_user_demo_trade_state(&mut tx, user_id)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let state_after_rollback: (i64, Option<chrono::DateTime<Utc>>) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM trades WHERE user_id=$1 AND execution_mode='demo'),demo_state_reset_at FROM user_profiles WHERE user_id=$1")
            .bind(user_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(state_after_rollback, (1, None));
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn claimed_sl2_reversals_submit_once_in_both_directions() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        for (index, source_direction, reversal_direction, expected_side) in [
            ("buy", "BUY", "SELL", "SELL"),
            ("sell", "SELL", "BUY", "BUY"),
        ] {
            // Each iteration represents a distinct Angel account. The shared fake
            // server must not leak the prior account's broker book into this one.
            fake.order_book.lock().await.clear();
            fake.trade_book.lock().await.clear();
            fake.positions.lock().await.clear();
            let (user_id, snapshot_id, trade_id, token, symbol) =
                seed_live_futures_protection_fixture(&state, &format!("reversal-{index}")).await;
            let contract_expiry = ist_now().date_naive() + chrono::Duration::days(30);
            contract_master::set_isolated_test_cache(vec![MasterContract {
                token: token.clone(),
                symbol: symbol.clone(),
                name: "GOLDTEN".into(),
                expiry: contract_expiry.format("%d%b%Y").to_string().to_uppercase(),
                strike: "0".into(),
                lotsize: "10".into(),
                tick_size: "100.000000".into(),
                instrumenttype: "FUTCOM".into(),
                exch_seg: "MCX".into(),
            }])
            .await;
            sqlx::query(
                "UPDATE strategy_market_snapshots SET trade_date=$2,contract_expiry=$3 WHERE id=$1",
            )
            .bind(snapshot_id)
            .bind(ist_now().date_naive())
            .bind(contract_expiry)
            .execute(&state.db)
            .await
            .unwrap();
            sqlx::query("INSERT INTO user_strategy_activations(user_id,strategy_key,is_active) VALUES($1,$2,TRUE) ON CONFLICT(user_id,strategy_key) DO UPDATE SET is_active=TRUE")
                .bind(user_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
            sqlx::query("UPDATE trades SET status='closed',direction=$2,exit_reason='SL2',exit_datetime=NOW()-INTERVAL '1 second',remaining_lots=0,safety_status='CLOSED',broker_net_quantity=0,last_position_reconciled_at=NOW() WHERE id=$1")
                .bind(trade_id).bind(source_direction).execute(&state.db).await.unwrap();
            sqlx::query("INSERT INTO market_price_ticks(exchange_segment,contract_token,price,received_at) VALUES('MCX',$1,100,NOW()) ON CONFLICT(exchange_segment,contract_token) DO UPDATE SET price=100,received_at=NOW()")
                .bind(&token).execute(&state.db).await.unwrap();
            let session = sl2_reversal_session(trade_id);
            sqlx::query("INSERT INTO strategy_reversal_intents(source_trade_id,user_id,snapshot_id,instrument,source_direction,reversal_direction,lots,entry_price,order_session_key,status,attempts) VALUES($1,$2,$3,'GOLDTEN',$4,$5,5,98,$6,'processing',1)")
                .bind(trade_id).bind(user_id).bind(snapshot_id).bind(source_direction).bind(reversal_direction).bind(&session).execute(&state.db).await.unwrap();
            let intent = Sl2ReversalIntent {
                source_trade_id: trade_id,
                user_id,
                snapshot_id,
                instrument: "GOLDTEN".into(),
                source_direction: source_direction.into(),
                reversal_direction: reversal_direction.into(),
                lots: 5,
                entry_price: 98.0,
                order_session_key: session,
                attempts: 1,
                created_at: Utc::now(),
            };
            assert!(matches!(
                attempt_claimed_sl2_reversal(&state, &intent).await.unwrap(),
                Sl2ReversalOutcome::Submitted
            ));
            assert!(matches!(
                attempt_claimed_sl2_reversal(&state, &intent).await.unwrap(),
                Sl2ReversalOutcome::Submitted
            ));
            let placed = fake.placed_orders.lock().await;
            let latest = placed.last().expect("reversal must reach fake broker");
            assert_eq!(latest["transactiontype"], expected_side);
            assert_eq!(latest["quantity"], "50");
            drop(placed);
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM strategy_orders WHERE trade_id=$1 AND session_key=$2 AND role IN ('BUY_ENTRY','SELL_ENTRY')")
                .bind(trade_id).bind(&intent.order_session_key).fetch_one(&state.db).await.unwrap();
            assert_eq!(
                count, 1,
                "repeated observation must reuse one durable order"
            );
        }
        assert_eq!(fake.placed_orders.lock().await.len(), 2);
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn demo_sl2_reversal_is_simulated_without_angel_mutation() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (user_id, snapshot_id, trade_id, token, symbol) =
            seed_live_futures_protection_fixture(&state, "demo-reversal").await;
        let contract_expiry = ist_now().date_naive() + chrono::Duration::days(30);
        contract_master::set_isolated_test_cache(vec![MasterContract {
            token: token.clone(),
            symbol,
            name: "GOLDTEN".into(),
            expiry: contract_expiry.format("%d%b%Y").to_string().to_uppercase(),
            strike: "0".into(),
            lotsize: "10".into(),
            tick_size: "100.000000".into(),
            instrumenttype: "FUTCOM".into(),
            exch_seg: "MCX".into(),
        }])
        .await;
        sqlx::query("UPDATE user_profiles SET trading_mode='demo' WHERE user_id=$1")
            .bind(user_id)
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE strategy_market_snapshots SET trade_date=$2,contract_expiry=$3 WHERE id=$1",
        )
        .bind(snapshot_id)
        .bind(ist_now().date_naive())
        .bind(contract_expiry)
        .execute(&state.db)
        .await
        .unwrap();
        sqlx::query("UPDATE trades SET execution_mode='demo',status='closed',exit_reason='SL2',exit_datetime=NOW(),remaining_lots=0,safety_status='CLOSED',broker_net_quantity=NULL,last_position_reconciled_at=NULL WHERE id=$1")
            .bind(trade_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO user_strategy_activations(user_id,strategy_key,is_active) VALUES($1,$2,TRUE)")
            .bind(user_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO market_price_ticks(exchange_segment,contract_token,price,received_at) VALUES('MCX',$1,98,NOW())")
            .bind(&token).execute(&state.db).await.unwrap();
        let session = sl2_reversal_session(trade_id);
        sqlx::query("INSERT INTO strategy_reversal_intents(source_trade_id,user_id,snapshot_id,instrument,source_direction,reversal_direction,lots,entry_price,order_session_key,status,attempts) VALUES($1,$2,$3,'GOLDTEN','BUY','SELL',5,98,$4,'processing',1)")
            .bind(trade_id).bind(user_id).bind(snapshot_id).bind(&session).execute(&state.db).await.unwrap();
        let intent = Sl2ReversalIntent {
            source_trade_id: trade_id,
            user_id,
            snapshot_id,
            instrument: "GOLDTEN".into(),
            source_direction: "BUY".into(),
            reversal_direction: "SELL".into(),
            lots: 5,
            entry_price: 98.0,
            order_session_key: session,
            attempts: 1,
            created_at: Utc::now(),
        };
        assert!(matches!(
            attempt_claimed_sl2_reversal(&state, &intent).await.unwrap(),
            Sl2ReversalOutcome::Completed
        ));
        let reversal: (String, String, Option<Uuid>) = sqlx::query_as("SELECT execution_mode,direction,reversal_of_trade_id FROM trades WHERE reversal_of_trade_id=$1 AND status='open'")
            .bind(trade_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(reversal, ("demo".into(), "SELL".into(), Some(trade_id)));
        assert!(fake.placed_orders.lock().await.is_empty());
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn manual_live_close_is_owned_idempotent_and_allowed_during_global_kill() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (user_id, _, trade_id, token, symbol) =
            seed_live_futures_protection_fixture(&state, "manual-close").await;
        *fake.positions.lock().await = vec![json!({
            "exchange":"MCX","symboltoken":token,"tradingsymbol":symbol,
            "producttype":"CARRYFORWARD","netqty":"50","avgnetprice":"100"
        })];
        sqlx::query("INSERT INTO market_price_ticks(exchange_segment,contract_token,price,received_at) VALUES('MCX',$1,100,NOW())")
            .bind(&token).execute(&state.db).await.unwrap();
        sqlx::query("UPDATE risk_kill_switches SET enabled=TRUE,reason='test global kill'")
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET can_live_trade=FALSE WHERE id=$1")
            .bind(user_id)
            .execute(&state.db)
            .await
            .unwrap();
        let auth = AuthUser {
            id: user_id,
            username: "manual-close-user".into(),
            can_administer: false,
            can_live_trade: false,
            can_backtest: false,
            can_backtest_on_trading_days: false,
            trading_mode: "live".into(),
            session_id: Uuid::new_v4(),
        };
        for _ in 0..2 {
            let _ = manual_close_trade(
                State(state.clone()),
                Extension(auth.clone()),
                Path(trade_id),
                HeaderMap::new(),
                None,
            )
            .await
            .unwrap();
        }
        let intent: (String, i32, String) = sqlx::query_as("SELECT status,requested_quantity,close_side FROM manual_trade_close_intents WHERE trade_id=$1")
            .bind(trade_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(intent, ("submitted".into(), 50, "SELL".into()));
        let placed = fake.placed_orders.lock().await;
        assert_eq!(
            placed.len(),
            1,
            "double request cannot submit a second close"
        );
        assert_eq!(placed[0]["transactiontype"], "SELL");
        assert_eq!(placed[0]["quantity"], "50");
        drop(placed);
        let local_status: String = sqlx::query_scalar("SELECT status FROM trades WHERE id=$1")
            .bind(trade_id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(local_status, "open", "submission is not broker fill truth");

        let other = AuthUser {
            id: Uuid::new_v4(),
            ..auth
        };
        assert!(matches!(
            manual_close_trade(
                State(state.clone()),
                Extension(other),
                Path(trade_id),
                HeaderMap::new(),
                None,
            )
            .await,
            Err(AppError::NotFound(_))
        ));
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn broker_manual_flat_requires_trade_fill_and_records_actual_price() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (user_id, _, trade_id, _token, _symbol) =
            seed_live_futures_protection_fixture(&state, "broker-manual").await;
        *fake.positions.lock().await = vec![];
        *fake.trade_book.lock().await = vec![json!({
            "orderid":"ANGEL-MANUAL-1","exchange":"MCX",
            "symboltoken":"broker-manual-token","tradingsymbol":"GOLDTEN-broker-manual-FUT",
            "transactiontype":"SELL","fillsize":"50","fillprice":"102.5",
            "filltime":Utc::now().to_rfc3339()
        })];
        reconcile_live_user(&state, user_id).await.unwrap();
        let closed: (String, String, f64, f64) = sqlx::query_as(
            "SELECT status,exit_reason,exit_price::float8,pnl::float8 FROM trades WHERE id=$1",
        )
        .bind(trade_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(closed.0, "closed");
        assert_eq!(closed.1, "MANUAL_BROKER_CLOSE");
        assert_eq!(closed.2, 102.5);
        assert_eq!(closed.3, 12.5, "GOLDTEN P&L uses actual weighted fill");
        reconcile_live_user(&state, user_id).await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM trades WHERE id=$1 AND status='closed'")
                .bind(trade_id)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(count, 1);
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn aggregate_position_incident_transition_is_atomic_and_fail_closed() {
        let state = isolated_test_state().await;
        let (user_id, _snapshot_id, trade_id, token, symbol) =
            seed_live_futures_protection_fixture(&state, "incident-transition").await;
        sqlx::query("INSERT INTO broker_position_incidents(id,user_id,strategy_key,instrument,exchange_segment,contract_token,contract_symbol,incident_type,status,broker_quantity,local_quantity,detail) VALUES($1,$2,$3,'GOLDTEN','MCX',$4,$5,'QUANTITY_OR_DIRECTION_MISMATCH','open',-100,50,'old aggregate state')")
            .bind(Uuid::new_v4()).bind(user_id).bind(STRATEGY_KEY).bind(&token).bind(&symbol)
            .execute(&state.db).await.unwrap();

        record_position_incident(
            &state,
            user_id,
            STRATEGY_KEY,
            "GOLDTEN",
            "MCX",
            &token,
            &symbol,
            "LOCAL_POSITION_BROKER_FLAT",
            0,
            50,
            None,
            Some(trade_id),
            "current aggregate state",
            "CARRYFORWARD",
            None,
        )
        .await
        .unwrap();

        let incidents: Vec<(String, String)> = sqlx::query_as(
            "SELECT incident_type,status FROM broker_position_incidents
             WHERE user_id=$1 AND contract_token=$2 ORDER BY incident_type",
        )
        .bind(user_id)
        .bind(&token)
        .fetch_all(&state.db)
        .await
        .unwrap();
        assert_eq!(
            incidents,
            vec![
                ("LOCAL_POSITION_BROKER_FLAT".into(), "open".into()),
                ("QUANTITY_OR_DIRECTION_MISMATCH".into(), "resolved".into()),
            ]
        );
        let trade: (String, Option<i32>) =
            sqlx::query_as("SELECT safety_status,broker_net_quantity FROM trades WHERE id=$1")
                .bind(trade_id)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(trade, ("RECONCILIATION_REQUIRED".into(), Some(0)));
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn broker_manual_close_evidence_survives_protection_cleanup_and_trade_book_expiry() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (user_id, snapshot_id, trade_id, token, symbol) =
            seed_live_futures_protection_fixture(&state, "durable-manual").await;
        sqlx::query(
            "UPDATE trades SET last_exact_broker_exposure_at=NOW()-INTERVAL '1 minute' WHERE id=$1",
        )
        .bind(trade_id)
        .execute(&state.db)
        .await
        .unwrap();
        let stop_id = Uuid::new_v4();
        sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,trigger_price,status,broker_order_id,idempotency_key,client_order_id) VALUES($1,$2,$3,$4,'durable-manual-stop','SL1','SELL','STOPLOSS_MARKET','live',5,50,98,98,'submitted','DURABLE-STOP','durable-manual-stop-key','DURABLE-STOP-TAG')")
            .bind(stop_id).bind(user_id).bind(snapshot_id).bind(trade_id).execute(&state.db).await.unwrap();
        fake.positions.lock().await.clear();
        let fill_at = Utc::now();
        *fake.trade_book.lock().await = vec![
            json!({
                "orderid":"EXTERNAL-1","exchange":"MCX","symboltoken":token,
                "tradingsymbol":symbol,"transactiontype":"SELL","fillsize":"10",
                "fillprice":"102","filltime":fill_at.to_rfc3339()
            }),
            json!({
                "orderid":"EXTERNAL-2","exchange":"MCX","symboltoken":token,
                "tradingsymbol":symbol,"transactiontype":"SELL","fillsize":"40",
                "fillprice":"104","filltime":fill_at.to_rfc3339()
            }),
        ];

        reconcile_live_user(&state, user_id).await.unwrap();
        let first: (String, String, i64, f64, i64) = sqlx::query_as(
            "SELECT t.status,o.status,
                    (SELECT COUNT(*) FROM manual_broker_close_evidence e WHERE e.trade_id=t.id),
                    (SELECT weighted_fill_price::float8 FROM manual_broker_close_evidence e WHERE e.trade_id=t.id),
                    (SELECT broker_credential_revision FROM manual_broker_close_evidence e WHERE e.trade_id=t.id)
             FROM trades t JOIN strategy_orders o ON o.id=$2 WHERE t.id=$1",
        )
        .bind(trade_id)
        .bind(stop_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(
            first.0, "open",
            "executable protection blocks terminalization"
        );
        assert_eq!(first.1, "cancelling");
        assert_eq!(
            first.2, 1,
            "exact external evidence must be durable before cleanup"
        );
        assert_eq!(first.3, 103.6);
        assert_eq!(
            first.4, 0,
            "evidence must be scoped to the broker account revision"
        );
        assert_eq!(
            fake.cancelled_orders.lock().await.as_slice(),
            ["DURABLE-STOP"]
        );
        assert!(fake.placed_orders.lock().await.is_empty());

        fake.trade_book.lock().await.clear();
        *fake.trade_book_unavailable.lock().await = true;
        *fake.order_book.lock().await = vec![json!({
            "orderid":"DURABLE-STOP","ordertag":"DURABLE-STOP-TAG","status":"cancelled",
            "filledshares":"0","averageprice":"0"
        })];
        sqlx::query("UPDATE user_profiles SET broker_credential_revision=1 WHERE user_id=$1")
            .bind(user_id)
            .execute(&state.db)
            .await
            .unwrap();
        assert!(
            reconcile_live_user(&state, user_id).await.is_err(),
            "evidence from another broker credential revision must not be reused"
        );
        let stale_revision_status: String =
            sqlx::query_scalar("SELECT status FROM trades WHERE id=$1")
                .bind(trade_id)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(stale_revision_status, "open");
        sqlx::query("UPDATE user_profiles SET broker_credential_revision=0 WHERE user_id=$1")
            .bind(user_id)
            .execute(&state.db)
            .await
            .unwrap();
        reconcile_live_user(&state, user_id).await.unwrap();
        let closed: (String, String, f64, f64, bool, DateTime<Utc>) = sqlx::query_as(
            "SELECT status,exit_reason,exit_price::float8,pnl::float8,
                    EXISTS(SELECT 1 FROM manual_broker_close_evidence e WHERE e.trade_id=$1 AND e.consumed_at IS NOT NULL)
                    ,exit_datetime
             FROM trades WHERE id=$1",
        )
        .bind(trade_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(closed.0, "closed");
        assert_eq!(closed.1, "MANUAL_BROKER_CLOSE");
        assert_eq!(closed.2, 103.6);
        assert_eq!(closed.3, 18.0);
        assert!(closed.4);
        assert!(
            (closed.5 - fill_at).num_milliseconds().abs() < 1,
            "the local exit timestamp must be the attributable broker fill time"
        );
        assert_eq!(fake.cancelled_orders.lock().await.len(), 1);
        assert!(
            fake.placed_orders.lock().await.is_empty(),
            "an already external close cannot submit another broker close"
        );

        reconcile_live_user(&state, user_id).await.unwrap();
        assert_eq!(fake.cancelled_orders.lock().await.len(), 1);
        assert!(fake.placed_orders.lock().await.is_empty());
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn broker_manual_flat_fails_closed_for_partial_ambiguous_or_missing_evidence() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let (user_id, snapshot_id, trade_id, token, symbol) =
            seed_live_futures_protection_fixture(&state, "manual-fail-closed").await;
        sqlx::query(
            "UPDATE trades SET last_exact_broker_exposure_at=NOW()-INTERVAL '1 minute' WHERE id=$1",
        )
        .bind(trade_id)
        .execute(&state.db)
        .await
        .unwrap();
        fake.positions.lock().await.clear();

        *fake.order_book_unavailable.lock().await = true;
        assert!(reconcile_live_user(&state, user_id).await.is_err());
        *fake.order_book_unavailable.lock().await = false;
        *fake.order_book_timeout.lock().await = true;
        assert!(reconcile_live_user(&state, user_id).await.is_err());
        *fake.order_book_timeout.lock().await = false;
        *fake.positions_unavailable.lock().await = true;
        assert!(reconcile_live_user(&state, user_id).await.is_err());
        *fake.positions_unavailable.lock().await = false;

        fake.trade_book.lock().await.clear();
        reconcile_live_user(&state, user_id).await.unwrap();
        let missing: (String, i64) = sqlx::query_as(
            "SELECT status,(SELECT COUNT(*) FROM manual_broker_close_evidence WHERE trade_id=$1) FROM trades WHERE id=$1",
        )
        .bind(trade_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(missing, ("open".into(), 0));

        *fake.trade_book.lock().await = vec![json!({
            "orderid":"PARTIAL","exchange":"MCX","symboltoken":token,
            "tradingsymbol":symbol,"transactiontype":"SELL","fillsize":"25",
            "fillprice":"102.5","filltime":Utc::now().to_rfc3339()
        })];
        reconcile_live_user(&state, user_id).await.unwrap();
        let partial: (String, String, i64) = sqlx::query_as(
            "SELECT status,safety_status,(SELECT COUNT(*) FROM manual_broker_close_evidence WHERE trade_id=$1) FROM trades WHERE id=$1",
        )
        .bind(trade_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(
            partial,
            ("open".into(), "RECONCILIATION_REQUIRED".into(), 0)
        );

        *fake.trade_book_unavailable.lock().await = true;
        assert!(reconcile_live_user(&state, user_id).await.is_err());
        let after_failure: String = sqlx::query_scalar("SELECT status FROM trades WHERE id=$1")
            .bind(trade_id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(after_failure, "open");
        *fake.trade_book_unavailable.lock().await = false;

        let second_trade = Uuid::new_v4();
        sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status,last_exact_broker_exposure_at) VALUES($1,$2,'live','open','BUY',50,100,100,0,NOW()-INTERVAL '2 minutes','GOLDTEN',$3,'ambiguous second local trade',$4,$5,5,5,'RECONCILIATION_REQUIRED',NOW()-INTERVAL '1 minute')")
            .bind(second_trade).bind(user_id).bind(&symbol).bind(STRATEGY_KEY).bind(snapshot_id).execute(&state.db).await.unwrap();
        *fake.trade_book.lock().await = vec![json!({
            "orderid":"EXACT-BUT-AMBIGUOUS","exchange":"MCX","symboltoken":token,
            "tradingsymbol":symbol,"transactiontype":"SELL","fillsize":"50",
            "fillprice":"103","filltime":Utc::now().to_rfc3339()
        })];
        reconcile_live_user(&state, user_id).await.unwrap();
        let ambiguous_closed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM trades WHERE id IN ($1,$2) AND status='closed'",
        )
        .bind(trade_id)
        .bind(second_trade)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(
            ambiguous_closed, 0,
            "multiple local trades prevent attribution"
        );
        assert!(fake.placed_orders.lock().await.is_empty());
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn demo_manual_close_is_owned_idempotent_and_never_calls_angel() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let user_id = Uuid::new_v4();
        let other_user_id = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        let trade_id = Uuid::new_v4();
        let stop_id = Uuid::new_v4();
        for (id, username) in [
            (user_id, "demo-close-owner"),
            (other_user_id, "demo-close-other"),
        ] {
            sqlx::query(
                "INSERT INTO users(id,username,email,password_hash) VALUES($1,$2,$3,'test-only')",
            )
            .bind(id)
            .bind(username)
            .bind(format!("{username}@example.test"))
            .execute(&state.db)
            .await
            .unwrap();
            sqlx::query("INSERT INTO user_profiles(user_id,trading_mode) VALUES($1,'demo')")
                .bind(id)
                .execute(&state.db)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,contract_token,contract_symbol,lot_size,exchange_segment,product_type,execution_key) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready','demo-close-token','GOLDTEN-DEMO-FUT',10,'MCX','CARRYFORWARD','demo-close')")
            .bind(snapshot_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status) VALUES($1,$2,'demo','open','SELL',20,100,100,5,NOW(),'GOLDTEN','GOLDTEN-DEMO-FUT',$3,$4,2,2,'PROTECTED')")
            .bind(trade_id).bind(user_id).bind(STRATEGY_KEY).bind(snapshot_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,trigger_price,status,broker_order_id,idempotency_key,client_order_id) VALUES($1,$2,$3,$4,'demo-close-stop','SL1','BUY','STOPLOSS_MARKET','demo',2,20,105,105,'submitted','DEMO-STOP','demo-close-stop-key','DEMO-STOP-TAG')")
            .bind(stop_id).bind(user_id).bind(snapshot_id).bind(trade_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO market_price_ticks(exchange_segment,contract_token,price,received_at) VALUES('MCX','demo-close-token',98,NOW())")
            .execute(&state.db).await.unwrap();
        let auth = |id, username: &str| AuthUser {
            id,
            username: username.into(),
            can_administer: false,
            can_live_trade: false,
            can_backtest: false,
            can_backtest_on_trading_days: false,
            trading_mode: "demo".into(),
            session_id: Uuid::new_v4(),
        };
        assert!(matches!(
            manual_close_trade(
                State(state.clone()),
                Extension(auth(other_user_id, "demo-close-other")),
                Path(trade_id),
                HeaderMap::new(),
                None,
            )
            .await,
            Err(AppError::NotFound(_))
        ));
        let first = manual_close_trade(
            State(state.clone()),
            Extension(auth(user_id, "demo-close-owner")),
            Path(trade_id),
            HeaderMap::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(first.0["status"], "completed");
        assert_eq!(first.0["execution_mode"], "demo");
        let closed: (String, String, f64, f64, i32, String) = sqlx::query_as(
            "SELECT t.status,t.exit_reason,t.exit_price::float8,t.pnl::float8,t.remaining_lots,o.status FROM trades t JOIN strategy_orders o ON o.id=$2 WHERE t.id=$1",
        )
        .bind(trade_id).bind(stop_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(
            closed,
            (
                "closed".into(),
                "MANUAL_RULENIX_CLOSE".into(),
                98.0,
                9.0,
                0,
                "cancelled".into()
            )
        );
        let duplicate = manual_close_trade(
            State(state.clone()),
            Extension(auth(user_id, "demo-close-owner")),
            Path(trade_id),
            HeaderMap::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(duplicate.0["status"], "completed");
        let live_side_effects: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM manual_trade_close_intents WHERE trade_id=$1),
                    (SELECT COUNT(*) FROM strategy_orders WHERE trade_id=$1 AND execution_mode='live')",
        )
        .bind(trade_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(live_side_effects, (0, 0));
        assert!(fake.placed_orders.lock().await.is_empty());
        assert!(fake.cancelled_orders.lock().await.is_empty());
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn deployment_safety_inventory_covers_all_durable_live_exposure() {
        let state = isolated_test_state().await;
        let user_id = seed_flat_linked_live_account(&state, "deployment-inventory").await;
        let snapshot_id = Uuid::new_v4();
        sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready','','deployment-token','DEPLOYMENTFUT',CURRENT_DATE+30,10,'MCX','CARRYFORWARD','deployment-inventory','')")
            .bind(snapshot_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();

        async fn inventory(
            state: &AppState,
            user_id: Uuid,
        ) -> (i64, i64, i64, i64, i64, i64, i64, i64) {
            sqlx::query_as("SELECT open_live_trades,unresolved_closed_live_trades,unresolved_live_orders,unresolved_live_execution_intents,unresolved_live_reversals,unresolved_live_manual_closes,unresolved_broker_incidents,unresolved_broker_mutations FROM broker_deployment_account_safety WHERE user_id=$1")
                .bind(user_id)
                .fetch_one(&state.db)
                .await
                .unwrap()
        }
        assert_eq!(inventory(&state, user_id).await, (0, 0, 0, 0, 0, 0, 0, 0));

        let trade_id = Uuid::new_v4();
        sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status) VALUES($1,$2,'live','open','BUY',10,100,100,0,NOW(),'GOLDTEN','DEPLOYMENTFUT','deployment inventory',$3,$4,1,1,'PROTECTION_REQUIRED')")
            .bind(trade_id).bind(user_id).bind(STRATEGY_KEY).bind(snapshot_id).execute(&state.db).await.unwrap();
        assert_eq!(inventory(&state, user_id).await.0, 1);

        let order_id = Uuid::new_v4();
        sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,status,broker_order_id,broker_status,idempotency_key,client_order_id) VALUES($1,$2,$3,'deployment-pending','BUY_ENTRY','BUY','MARKET','live',1,10,100,'pending','','','deployment-pending-key','deployment-pending-tag')")
            .bind(order_id).bind(user_id).bind(snapshot_id).execute(&state.db).await.unwrap();
        assert_eq!(inventory(&state, user_id).await.2, 1);

        let signal_id = Uuid::new_v4();
        sqlx::query("INSERT INTO strategy_signals(id,strategy_key,instrument,session_key,signal_at,signal_type) VALUES($1,$2,'GOLDTEN','deployment-signal',NOW(),'BUY_ENTRY')")
            .bind(signal_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO strategy_execution_intents(id,signal_id,user_id,snapshot_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,status) VALUES($1,$2,$3,$4,$5,'GOLDTEN','deployment-intent','ENTRY','BUY_ENTRY','BUY','MARKET',1,10,100,'claimed')")
            .bind(Uuid::new_v4()).bind(signal_id).bind(user_id).bind(snapshot_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        assert_eq!(inventory(&state, user_id).await.3, 1);

        sqlx::query("UPDATE trades SET status='closed',safety_status='CLOSED',remaining_lots=0,exit_reason='SL2',exit_price=98,exit_datetime=NOW(),broker_net_quantity=0 WHERE id=$1")
            .bind(trade_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO strategy_reversal_intents(source_trade_id,user_id,snapshot_id,instrument,source_direction,reversal_direction,lots,entry_price,order_session_key,status) VALUES($1,$2,$3,'GOLDTEN','BUY','SELL',1,98,$4,'waiting')")
            .bind(trade_id).bind(user_id).bind(snapshot_id).bind(sl2_reversal_session(trade_id)).execute(&state.db).await.unwrap();
        assert_eq!(inventory(&state, user_id).await.4, 1);

        let manual_trade_id = Uuid::new_v4();
        sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,safety_status) VALUES($1,$2,'live','open','SELL',10,100,100,0,NOW(),'GOLDTEN','DEPLOYMENTFUT','manual deployment inventory',$3,$4,1,1,'PROTECTED')")
            .bind(manual_trade_id).bind(user_id).bind(STRATEGY_KEY).bind(snapshot_id).execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO manual_trade_close_intents(trade_id,user_id,requested_quantity,close_side,status) VALUES($1,$2,10,'BUY','ambiguous')")
            .bind(manual_trade_id).bind(user_id).execute(&state.db).await.unwrap();
        assert_eq!(inventory(&state, user_id).await.5, 1);

        sqlx::query("INSERT INTO broker_position_incidents(id,user_id,strategy_key,instrument,exchange_segment,contract_token,contract_symbol,incident_type,status,broker_quantity,local_quantity,detail) VALUES($1,$2,$3,'GOLDTEN','MCX','deployment-token','DEPLOYMENTFUT','QUANTITY_OR_DIRECTION_MISMATCH','operator_required',0,10,'deployment inventory')")
            .bind(Uuid::new_v4()).bind(user_id).bind(STRATEGY_KEY).execute(&state.db).await.unwrap();
        assert_eq!(inventory(&state, user_id).await.6, 1);

        sqlx::query("INSERT INTO broker_reconciliation_blockers(user_id,status,external_active_orders,detail) VALUES($1,'open',1,'deployment inventory')")
            .bind(user_id).execute(&state.db).await.unwrap();
        assert_eq!(inventory(&state, user_id).await.7, 1);

        sqlx::query("UPDATE trades SET safety_status='CLOSED',broker_net_quantity=5 WHERE id=$1")
            .bind(trade_id)
            .execute(&state.db)
            .await
            .unwrap();
        assert_eq!(inventory(&state, user_id).await.1, 1);
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn full_reconciliation_controls_revision_bound_live_readiness() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let user_id = seed_flat_linked_live_account(&state, "readiness").await;
        risk::set_reconciliation_health(&state, user_id, false, "offline")
            .await
            .unwrap();
        assert!(!risk::reconciliation_ready(&state, user_id).await.unwrap());

        reconcile_live_user_readiness(&state, user_id)
            .await
            .unwrap();
        assert!(risk::reconciliation_ready(&state, user_id).await.unwrap());

        *fake.positions.lock().await = vec![json!({
            "exchange":"MCX", "symboltoken":"external-token",
            "tradingsymbol":"EXTERNALFUT", "producttype":"CARRYFORWARD",
            "netqty":"10", "avgnetprice":"100"
        })];
        assert!(
            reconcile_live_user_readiness(&state, user_id)
                .await
                .is_err()
        );
        assert!(!risk::reconciliation_ready(&state, user_id).await.unwrap());
        let incident: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM broker_position_incidents WHERE user_id=$1 AND status IN ('open','operator_required')")
            .bind(user_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(incident, 1);

        *fake.positions.lock().await = Vec::new();
        sqlx::query("UPDATE broker_position_incidents SET status='resolved',resolved_at=NOW() WHERE user_id=$1")
            .bind(user_id).execute(&state.db).await.unwrap();
        *fake.order_book.lock().await = vec![json!({
            "orderid":"EXTERNAL-ACTIVE-1", "status":"open",
            "transactiontype":"BUY", "quantity":"10",
            "tradingsymbol":"EXTERNALFUT", "symboltoken":"external-token"
        })];
        assert!(
            reconcile_live_user_readiness(&state, user_id)
                .await
                .is_err()
        );
        let durable_blocker: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM broker_reconciliation_blockers WHERE user_id=$1 AND status='open'")
            .bind(user_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(durable_blocker, 1);

        fake.order_book.lock().await.clear();
        *fake.conditional_unavailable.lock().await = true;
        assert!(
            reconcile_live_user_readiness(&state, user_id)
                .await
                .is_err()
        );
        assert!(!risk::reconciliation_ready(&state, user_id).await.unwrap());
        let blocker_survives_read_failure: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM broker_reconciliation_blockers WHERE user_id=$1 AND status='open'")
            .bind(user_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(blocker_survives_read_failure, 1);
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn manual_broker_exposure_is_reported_narrow_and_never_mutated() {
        let (broker_url, fake, broker_task) = spawn_deterministic_fake_broker().await;
        let state = isolated_test_state_with_broker(&broker_url).await;
        let user_id = seed_flat_linked_live_account(&state, "manual-narrow").await;
        let manual_order = json!({
            "orderid":"MANUAL-1", "ordertag":"", "status":"open", "exchange":"MCX",
            "symboltoken":"manual-token", "tradingsymbol":"MANUALFUT",
            "transactiontype":"BUY", "quantity":"10", "filledshares":"10"
        });
        *fake.order_book.lock().await = vec![manual_order];
        *fake.positions.lock().await = vec![json!({
            "exchange":"MCX", "symboltoken":"manual-token", "tradingsymbol":"MANUALFUT",
            "producttype":"CARRYFORWARD", "netqty":"10", "avgnetprice":"100"
        })];
        *fake.trade_book.lock().await = vec![json!({
            "orderid":"MANUAL-1", "ordertag":"", "exchange":"MCX",
            "symboltoken":"manual-token", "tradingsymbol":"MANUALFUT",
            "transactiontype":"BUY", "fillsize":"10", "fillprice":"100",
            "filltime":Utc::now().to_rfc3339()
        })];
        reconcile_live_user_readiness(&state, user_id)
            .await
            .unwrap();
        assert!(risk::reconciliation_ready(&state, user_id).await.unwrap());
        let ownership: Vec<(String, String)> = sqlx::query_as(
            "SELECT exposure_kind,ownership_status FROM broker_exposure_observations
             WHERE user_id=$1 ORDER BY exposure_kind",
        )
        .bind(user_id)
        .fetch_all(&state.db)
        .await
        .unwrap();
        assert_eq!(
            ownership,
            vec![
                ("order".into(), "manual_external".into()),
                ("position".into(), "manual_external".into()),
            ]
        );
        reconcile_live_user_readiness(&state, user_id)
            .await
            .unwrap();
        let observation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM broker_exposure_observations WHERE user_id=$1",
        )
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(
            observation_count, 2,
            "restart/reconciliation must only refresh manual evidence"
        );
        assert!(fake.placed_orders.lock().await.is_empty());
        assert!(fake.cancelled_orders.lock().await.is_empty());

        let mut snapshots = HashMap::new();
        for (token, key) in [
            ("unrelated-token", "manual-narrow-unrelated"),
            ("manual-token", "manual-narrow-exact"),
        ] {
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,contract_token,contract_symbol,lot_size,exchange_segment,product_type,execution_key) VALUES($1,$2,'GOLDTEN',CURRENT_DATE,'ready',$3,$3,10,'MCX','CARRYFORWARD',$4)")
                .bind(id).bind(STRATEGY_KEY).bind(token).bind(key).execute(&state.db).await.unwrap();
            sqlx::query("INSERT INTO market_price_ticks(exchange_segment,contract_token,price,received_at) VALUES('MCX',$1,100,NOW())")
                .bind(token).execute(&state.db).await.unwrap();
            snapshots.insert(token, id);
        }
        let unrelated_snapshot = snapshots["unrelated-token"];
        let manual_snapshot = snapshots["manual-token"];
        let risk_order =
            |snapshot_id, token: &'static str, mode: &'static str, key: &'static str| {
                risk::OrderRisk {
                    user_id,
                    snapshot_id,
                    trade_id: None,
                    session: key,
                    role: "BUY_ENTRY",
                    side: "BUY",
                    mode,
                    lots: 1,
                    quantity: 10,
                    price: 100.0,
                    trigger_price: None,
                    idempotency_key: key,
                    snapshot_ready: true,
                    snapshot_current: true,
                    exchange_segment: "MCX",
                    contract_token: token,
                    live_reconciled: true,
                    originated_at: None,
                }
            };
        assert!(
            risk::assess_and_reserve(
                &state,
                &risk_order(
                    unrelated_snapshot,
                    "unrelated-token",
                    "live",
                    "manual-unrelated-live"
                ),
            )
            .await
            .unwrap()
            .is_some()
        );
        let collision = risk::assess_and_reserve(
            &state,
            &risk_order(manual_snapshot, "manual-token", "live", "manual-exact-live"),
        )
        .await
        .expect_err("exact manual contract must block only this LIVE mutation");
        assert!(collision.to_string().contains("exact LIVE contract"));

        sqlx::query("UPDATE user_profiles SET trading_mode='demo' WHERE user_id=$1")
            .bind(user_id)
            .execute(&state.db)
            .await
            .unwrap();
        assert!(
            risk::assess_and_reserve(
                &state,
                &risk_order(manual_snapshot, "manual-token", "demo", "manual-exact-demo"),
            )
            .await
            .unwrap()
            .is_some()
        );
        assert!(fake.placed_orders.lock().await.is_empty());
        assert!(fake.cancelled_orders.lock().await.is_empty());
        broker_task.abort();
    }

    #[tokio::test]
    #[ignore = "requires an isolated loopback TEST_DATABASE_URL named rulenix_test_*"]
    async fn disconnected_flat_account_is_live_blocked_without_platform_exposure() {
        let state = isolated_test_state().await;
        let user_id = seed_flat_linked_live_account(&state, "offline-flat").await;
        state
            .credentials
            .put(user_id, &[("jwt_token", "")])
            .await
            .unwrap();
        sqlx::query("UPDATE user_profiles SET last_token_status='invalid',token_state='invalid' WHERE user_id=$1")
            .bind(user_id).execute(&state.db).await.unwrap();
        let audience = reconciliation_audience(&state).await.unwrap();
        assert_eq!(audience.disconnected, vec![user_id]);
        let local_total: i64 = sqlx::query_scalar("SELECT open_live_trades+unresolved_closed_live_trades+unresolved_live_orders+unresolved_live_execution_intents+unresolved_live_reversals+unresolved_live_manual_closes+unresolved_broker_incidents+unresolved_broker_mutations FROM broker_deployment_account_safety WHERE user_id=$1")
            .bind(user_id).fetch_one(&state.db).await.unwrap();
        assert_eq!(local_total, 0);
        assert!(
            reconcile_live_user_readiness(&state, user_id)
                .await
                .is_err()
        );
        assert!(!risk::reconciliation_ready(&state, user_id).await.unwrap());
    }

    #[test]
    fn manual_flat_attribution_requires_exact_unknown_opposite_fills() {
        let entry_at = Utc::now() - Duration::minutes(5);
        let known = HashSet::from(["RULENIX-SL".to_string()]);
        let fill = |order_id: &str, side: &str, quantity: i32, seconds: i64| BrokerTradeFill {
            order_id: order_id.into(),
            order_tag: String::new(),
            exchange: "MCX".into(),
            token: "123".into(),
            symbol: "GOLDTEN30SEP26FUT".into(),
            side: side.into(),
            quantity,
            price: 101.5,
            filled_at: entry_at + Duration::seconds(seconds),
        };
        let expected = ManualFillExpectation {
            exchange: "MCX",
            token: "123",
            symbol: "GOLDTEN30SEP26FUT",
            direction: "BUY",
            quantity: 20,
            entry_at,
            evidence_since: entry_at,
            known_order_ids: &known,
        };
        assert_eq!(
            attributable_manual_flat_fill(&[fill("MANUAL-1", "SELL", 20, 1)], &expected,)
                .map(|evidence| evidence.weighted_price),
            Some(101.5)
        );
        assert!(
            attributable_manual_flat_fill(&[fill("RULENIX-SL", "SELL", 20, 1)], &expected,)
                .is_none(),
            "a Rulenix-owned protective fill is not a broker-side manual close"
        );
        assert!(
            attributable_manual_flat_fill(&[fill("MANUAL-1", "SELL", 10, 1)], &expected,).is_none(),
            "partial or ambiguous attribution must fail closed"
        );
        assert!(
            attributable_manual_flat_fill(
                &[fill("MANUAL-BEFORE-ENTRY", "SELL", 20, -1)],
                &expected,
            )
            .is_none(),
            "a fill before the local entry cannot close the trade"
        );
        let weighted = attributable_manual_flat_fill(
            &[
                BrokerTradeFill {
                    price: 100.0,
                    ..fill("MANUAL-1", "SELL", 5, 1)
                },
                BrokerTradeFill {
                    price: 104.0,
                    ..fill("MANUAL-2", "SELL", 15, 2)
                },
            ],
            &expected,
        )
        .expect("multiple exact external fills must be attributable");
        assert_eq!(weighted.quantity, 20);
        assert_eq!(weighted.weighted_price, 103.0);
        assert_eq!(weighted.order_ids, ["MANUAL-1", "MANUAL-2"]);
        assert!(
            attributable_manual_flat_fill(&[fill("MANUAL-1", "SELL", 21, 1)], &expected,).is_none(),
            "an external over-close cannot be treated as an exact close"
        );
        assert!(
            attributable_manual_flat_fill(&[fill("MANUAL-1", "BUY", 20, 1)], &expected,).is_none(),
            "same-side or unrelated fills cannot close the trade"
        );
        assert!(
            attributable_manual_flat_fill(
                &[
                    fill("MANUAL-1", "SELL", 20, 1),
                    fill("UNRELATED", "BUY", 1, 2),
                ],
                &expected,
            )
            .is_none(),
            "conflicting external activity on the same contract must fail closed"
        );
        assert!(
            attributable_manual_flat_fill(
                &[BrokerTradeFill {
                    symbol: "WRONG".into(),
                    ..fill("MANUAL-1", "SELL", 20, 1)
                }],
                &expected,
            )
            .is_none(),
            "a wrong-symbol fill cannot be attributed even when a token matches"
        );
        assert!(
            attributable_manual_flat_fill(
                &[BrokerTradeFill {
                    token: "wrong-token".into(),
                    ..fill("MANUAL-1", "SELL", 20, 1)
                }],
                &expected,
            )
            .is_none(),
            "a wrong-token fill cannot be attributed"
        );
        let stale_expected = ManualFillExpectation {
            evidence_since: entry_at + Duration::seconds(3),
            ..expected
        };
        assert!(
            attributable_manual_flat_fill(
                &[fill("MANUAL-BEFORE-MATCH", "SELL", 20, 2)],
                &stale_expected,
            )
            .is_none(),
            "a fill before the last exact broker/local exposure match is stale evidence"
        );
        let short_expected = ManualFillExpectation {
            direction: "SELL",
            ..expected
        };
        assert_eq!(
            attributable_manual_flat_fill(&[fill("MANUAL-SHORT", "BUY", 20, 1)], &short_expected,)
                .map(|evidence| evidence.weighted_price),
            Some(101.5)
        );
    }

    #[test]
    fn broker_exposure_ownership_requires_durable_or_complete_evidence() {
        let manual = json!({
            "orderid":"MANUAL", "exchange":"MCX", "symboltoken":"123",
            "transactiontype":"BUY", "quantity":"10", "ordertag":""
        });
        assert_eq!(
            broker_order_ownership(&manual, &HashSet::new(), &HashSet::new()),
            BrokerExposureOwnership::ManualExternal
        );
        assert_eq!(
            broker_order_ownership(
                &json!({"orderid":"ORPHAN", "exchange":"MCX", "symboltoken":"123", "transactiontype":"BUY", "quantity":"10", "ordertag":"RX0123456789ABCDEF01"}),
                &HashSet::new(),
                &HashSet::new(),
            ),
            BrokerExposureOwnership::Ambiguous
        );
        assert_eq!(
            broker_order_ownership(&manual, &HashSet::from(["MANUAL".into()]), &HashSet::new(),),
            BrokerExposureOwnership::RulenixOwned
        );
    }

    #[test]
    fn broker_trade_parser_accepts_angel_exchange_time_and_fails_closed_on_malformed_rows() {
        let parsed = parse_broker_trade_fills(&json!([{
            "orderid":"MANUAL-1","exchange":"MCX","symboltoken":"123",
            "tradingsymbol":"GOLDTEN30SEP26FUT","transactiontype":"SELL",
            "quantity":"20","price":"101.25","exchtime":"11-Sep-2026 12:15:01"
        }]))
        .expect("the observed Angel trade-book aliases must parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].price, 101.25);
        assert!(
            parse_broker_trade_fills(&json!([{
                "orderid":"MANUAL-1","exchange":"MCX","symboltoken":"123"
            }]))
            .is_err()
        );
        assert!(parse_broker_trade_fills(&json!({"unexpected":[]})).is_err());
    }

    #[test]
    fn manual_close_sessions_are_stable_and_have_distinct_exit_reason() {
        let trade_id = Uuid::new_v4();
        assert_eq!(
            manual_close_session(trade_id),
            manual_close_session(trade_id)
        );
        assert_ne!(
            manual_close_session(trade_id),
            emergency_close_session(trade_id)
        );
        assert_eq!(
            recorded_exit_reason(
                STRATEGY_KEY,
                "EMERGENCY_CLOSE",
                &manual_close_session(trade_id)
            ),
            "MANUAL_RULENIX_CLOSE"
        );
    }
}
