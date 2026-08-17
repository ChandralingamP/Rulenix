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
    state::AppState,
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
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::time::{MissedTickBehavior, interval};
use uuid::Uuid;

pub const STRATEGY_KEY: &str = "futures_breakout_v3";
pub const OPTION_ENTRY_STRATEGY_KEY: &str = "option_entry_v1";
pub const SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY: &str = "supertrend_index_options_v1";
const SENSEX_INDEX_TOKEN: &str = "99919000";
const NIFTY_INDEX_TOKEN: &str = "99926000";
const OPTION_INTERVAL: &str = "FIVE_MINUTE";
const OPTION_MIN_PREMIUM: f64 = 220.0;
const OPTION_MAX_PREMIUM: f64 = 290.0;
const OPTION_TARGET_PREMIUM: f64 = 260.0;
const OPTION_PRODUCT_TYPE: &str = "INTRADAY";
const OPTION_ENTRY_START_MINUTE: u32 = 9 * 60 + 20;
const OPTION_SQUARE_OFF_MINUTE: u32 = 15 * 60 + 20;
const OPTION_SCHEDULER_END_MINUTE: u32 = 15 * 60 + 30;
const FUTURES_EXPIRY_SQUARE_OFF_MINUTE: u32 = 15 * 60 + 20;
const SHARED_MARKET_CREDENTIAL_LIMIT: i64 = 8;
const KELTNER_EMA_PERIOD: usize = 20;
const KELTNER_ATR_PERIOD: usize = 10;
const KELTNER_MULTIPLIER: f64 = 2.0;
const TSI_LONG_PERIOD: usize = 25;
const TSI_SHORT_PERIOD: usize = 13;
const OPTION_TSI_ENTRY_THRESHOLD: f64 = 0.5;
const SUPERTREND_ATR_PERIOD: usize = 7;
const SUPERTREND_FACTOR: f64 = 2.0;
const SUPERTREND_LOOKBACK_DAYS: i64 = 14;
const SUPERTREND_SIGNAL_CATCHUP_MINUTES: i64 = 30;
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

#[derive(Debug, Clone, Copy)]
struct IntradayCandle {
    at: NaiveDateTime,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
}

#[derive(Debug, Clone, Copy)]
struct IndicatorCandle {
    candle: IntradayCandle,
    middle: f64,
    upper: f64,
    lower: f64,
    tsi: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionSide {
    Call,
    Put,
}

impl OptionSide {
    fn instrument(self) -> &'static str {
        match self {
            Self::Call => "SENSEX_CE",
            Self::Put => "SENSEX_PE",
        }
    }

    fn option_type(self) -> &'static str {
        match self {
            Self::Call => "CE",
            Self::Put => "PE",
        }
    }

    fn entry_role(self) -> &'static str {
        match self {
            Self::Call => "BUY_ENTRY",
            Self::Put => "SELL_ENTRY",
        }
    }

    fn entry_side(self) -> &'static str {
        "BUY"
    }

    fn exit_side(self) -> &'static str {
        "SELL"
    }

    fn from_instrument(instrument: &str) -> Option<Self> {
        match instrument {
            "SENSEX_CE" => Some(Self::Call),
            "SENSEX_PE" => Some(Self::Put),
            _ => None,
        }
    }
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
enum SuperTrendDirection {
    Up,
    Down,
}

impl SuperTrendDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Up => "UP",
            Self::Down => "DOWN",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SuperTrendPoint {
    candle: IntradayCandle,
    value: f64,
    direction: SuperTrendDirection,
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

#[derive(Debug, Clone, Copy)]
struct OptionSignal {
    side: OptionSide,
    entry_price: f64,
    stop_loss: f64,
    target_band: f64,
    entry_tsi: f64,
    confirmation_at: NaiveDateTime,
    signal_at: NaiveDateTime,
}

type OpenOptionTradeRow = (
    Uuid,
    Uuid,
    String,
    i32,
    i32,
    Option<Uuid>,
    f64,
    Option<DateTime<Utc>>,
);
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
    f64,
    Option<f64>,
    Option<f64>,
    String,
    String,
);

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
struct Levels {
    hh2: f64,
    ll2: f64,
    hh4: f64,
    ll4: f64,
    buy_entry: f64,
    buy_target: f64,
    buy_sl1: f64,
    buy_sl2: f64,
    sell_entry: f64,
    sell_target: f64,
    sell_sl1: f64,
    sell_sl2: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FuturesExitLevels {
    pub target: f64,
    pub sl1: f64,
    pub sl2: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FuturesGapDirection {
    Up,
    Down,
    Flat,
}

impl FuturesGapDirection {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Up => "UP",
            Self::Down => "DOWN",
            Self::Flat => "FLAT",
        }
    }

    pub(crate) fn entry_direction(self) -> &'static str {
        match self {
            Self::Up => "BUY",
            Self::Down => "SELL",
            Self::Flat => "BOTH",
        }
    }
}

pub(crate) fn futures_gap_direction(
    previous_close: f64,
    market_open: f64,
) -> Option<FuturesGapDirection> {
    if !previous_close.is_finite()
        || previous_close <= 0.0
        || !market_open.is_finite()
        || market_open <= 0.0
    {
        return None;
    }
    Some(if market_open > previous_close {
        FuturesGapDirection::Up
    } else if market_open < previous_close {
        FuturesGapDirection::Down
    } else {
        FuturesGapDirection::Flat
    })
}

pub(crate) fn futures_gap_entry_was_jumped(
    gap: FuturesGapDirection,
    market_open: f64,
    buy_entry: f64,
    sell_entry: f64,
) -> Option<bool> {
    if [market_open, buy_entry, sell_entry]
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return None;
    }
    Some(match gap {
        FuturesGapDirection::Up => market_open >= buy_entry,
        FuturesGapDirection::Down => market_open <= sell_entry,
        FuturesGapDirection::Flat => false,
    })
}

pub(crate) fn futures_opening_range_entry(
    gap: FuturesGapDirection,
    opening_range_high: f64,
    opening_range_low: f64,
) -> Option<f64> {
    if [opening_range_high, opening_range_low]
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
        || opening_range_high < opening_range_low
    {
        return None;
    }
    match gap {
        FuturesGapDirection::Up => Some(opening_range_high * (1.0 + 0.0012)),
        FuturesGapDirection::Down => Some(opening_range_low * (1.0 - 0.0012)),
        FuturesGapDirection::Flat => None,
    }
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
            let fallback_stop = entry * (1.0 - 0.015);
            let stop = |technical: f64| {
                if technical < entry {
                    technical
                } else {
                    fallback_stop
                }
            };
            (
                entry * (1.0 + 0.015),
                stop(ll2 * (1.0 - 0.0012)),
                stop(ll4 * (1.0 - 0.0012)),
            )
        }
        "SELL" => {
            let fallback_stop = entry * (1.0 + 0.015);
            let stop = |technical: f64| {
                if technical > entry {
                    technical
                } else {
                    fallback_stop
                }
            };
            (
                entry * (1.0 - 0.015),
                stop(hh2 * (1.0 + 0.0012)),
                stop(hh4 * (1.0 + 0.0012)),
            )
        }
        _ => return None,
    };
    [target, sl1, sl2]
        .iter()
        .all(|value| value.is_finite() && *value > 0.0)
        .then_some(FuturesExitLevels { target, sl1, sl2 })
}

fn calculate(highs: &[f64], lows: &[f64]) -> Option<Levels> {
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

fn ema(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut result = vec![None; values.len()];
    if period == 0 || values.len() < period {
        return result;
    }
    let seed = values[..period].iter().sum::<f64>() / period as f64;
    result[period - 1] = Some(seed);
    let alpha = 2.0 / (period as f64 + 1.0);
    let mut previous = seed;
    for (index, value) in values.iter().enumerate().skip(period) {
        previous = *value * alpha + previous * (1.0 - alpha);
        result[index] = Some(previous);
    }
    result
}

fn ema_from_options(values: &[Option<f64>], period: usize) -> Vec<Option<f64>> {
    let mut result = vec![None; values.len()];
    let alpha = 2.0 / (period as f64 + 1.0);
    let mut window = Vec::new();
    let mut previous = None;
    for (index, value) in values.iter().enumerate() {
        let Some(value) = value else {
            continue;
        };
        if let Some(prev) = previous {
            let next = *value * alpha + prev * (1.0 - alpha);
            result[index] = Some(next);
            previous = Some(next);
        } else {
            window.push(*value);
            if window.len() == period {
                let seed = window.iter().sum::<f64>() / period as f64;
                result[index] = Some(seed);
                previous = Some(seed);
            }
        }
    }
    result
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

fn supertrend_points(
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
            let direction = if candle.close >= hl2 {
                SuperTrendDirection::Up
            } else {
                SuperTrendDirection::Down
            };
            (basic_upper, basic_lower, direction)
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

fn recent_supertrend_signal(
    points: &[SuperTrendPoint],
    now: DateTime<FixedOffset>,
) -> Option<SuperTrendSignal> {
    let today = now.date_naive();
    let latest_allowed = option_latest_completed_candle_time(now);
    let catchup_from = latest_allowed - Duration::minutes(SUPERTREND_SIGNAL_CATCHUP_MINUTES);
    points.windows(2).rev().find_map(|pair| {
        let previous = pair.first()?;
        let latest = pair.get(1)?;
        if latest.candle.at.date() != today
            || latest.candle.at > latest_allowed
            || latest.candle.at < catchup_from
        {
            return None;
        }
        supertrend_signal_from_transition(previous, latest)
    })
}

fn tsi_values(candles: &[IntradayCandle]) -> Vec<Option<f64>> {
    if candles.len() < TSI_LONG_PERIOD + TSI_SHORT_PERIOD {
        return vec![None; candles.len()];
    }
    let momentum: Vec<f64> = candles
        .iter()
        .enumerate()
        .map(|(index, candle)| {
            if index == 0 {
                0.0
            } else {
                candle.close - candles[index - 1].close
            }
        })
        .collect();
    let abs_momentum: Vec<f64> = momentum.iter().map(|value| value.abs()).collect();
    let ema_momentum = ema(&momentum, TSI_LONG_PERIOD);
    let ema_abs = ema(&abs_momentum, TSI_LONG_PERIOD);
    let double_momentum = ema_from_options(&ema_momentum, TSI_SHORT_PERIOD);
    let double_abs = ema_from_options(&ema_abs, TSI_SHORT_PERIOD);
    double_momentum
        .into_iter()
        .zip(double_abs)
        .map(|(num, den)| match (num, den) {
            (Some(num), Some(den)) if den.abs() > f64::EPSILON => Some(100.0 * num / den),
            _ => None,
        })
        .collect()
}

fn indicator_candles(candles: &[IntradayCandle]) -> Vec<IndicatorCandle> {
    let closes: Vec<f64> = candles.iter().map(|candle| candle.close).collect();
    let middle = ema(&closes, KELTNER_EMA_PERIOD);
    let atr = ema(&true_ranges(candles), KELTNER_ATR_PERIOD);
    let tsi = tsi_values(candles);
    candles
        .iter()
        .enumerate()
        .filter_map(|(index, candle)| {
            let middle = middle[index]?;
            let atr = atr[index]?;
            let tsi = tsi[index]?;
            Some(IndicatorCandle {
                candle: *candle,
                middle,
                upper: middle + KELTNER_MULTIPLIER * atr,
                lower: middle - KELTNER_MULTIPLIER * atr,
                tsi,
            })
        })
        .collect()
}

fn option_signal(candles: &[IndicatorCandle], side: OptionSide) -> Option<OptionSignal> {
    enum Setup {
        Idle,
        AwaitRetrace,
        AwaitConfirmation,
        AwaitBreak {
            high: f64,
            low: f64,
            target_band: f64,
            confirmation_at: NaiveDateTime,
        },
    }
    let mut setup = Setup::Idle;
    let mut active_date = None;
    let mut signal = None;
    for item in candles {
        let candle = item.candle;
        if active_date != Some(candle.at.date()) {
            active_date = Some(candle.at.date());
            setup = Setup::Idle;
        }
        match side {
            OptionSide::Call => match setup {
                Setup::Idle => {
                    if candle.high > item.upper && candle.close > item.upper {
                        setup = Setup::AwaitRetrace;
                    }
                }
                Setup::AwaitRetrace => {
                    if candle.low <= item.middle {
                        setup = Setup::AwaitConfirmation;
                    }
                }
                Setup::AwaitConfirmation => {
                    if candle.close > candle.open
                        && candle.high > item.middle
                        && candle.close > item.middle
                    {
                        setup = Setup::AwaitBreak {
                            high: candle.high,
                            low: candle.low,
                            target_band: item.upper,
                            confirmation_at: candle.at,
                        };
                    }
                }
                Setup::AwaitBreak {
                    high,
                    low,
                    target_band,
                    confirmation_at,
                } => {
                    if candle.close > high {
                        setup = Setup::AwaitRetrace;
                        if item.tsi > OPTION_TSI_ENTRY_THRESHOLD
                            && option_signal_has_min_rr(side, candle.close, low, target_band)
                        {
                            signal = Some(OptionSignal {
                                side,
                                entry_price: candle.close,
                                stop_loss: low,
                                target_band,
                                entry_tsi: item.tsi,
                                confirmation_at,
                                signal_at: candle.at,
                            });
                        }
                    } else if candle.close < item.middle {
                        setup = Setup::AwaitConfirmation;
                    }
                }
            },
            OptionSide::Put => match setup {
                Setup::Idle => {
                    if candle.low < item.lower && candle.close < item.lower {
                        setup = Setup::AwaitRetrace;
                    }
                }
                Setup::AwaitRetrace => {
                    if candle.high >= item.middle {
                        setup = Setup::AwaitConfirmation;
                    }
                }
                Setup::AwaitConfirmation => {
                    if candle.close < candle.open
                        && candle.low < item.middle
                        && candle.close < item.middle
                    {
                        setup = Setup::AwaitBreak {
                            high: candle.high,
                            low: candle.low,
                            target_band: item.lower,
                            confirmation_at: candle.at,
                        };
                    }
                }
                Setup::AwaitBreak {
                    high,
                    low,
                    target_band,
                    confirmation_at,
                } => {
                    if candle.close < low {
                        setup = Setup::AwaitRetrace;
                        if item.tsi < -OPTION_TSI_ENTRY_THRESHOLD
                            && option_signal_has_min_rr(side, candle.close, high, target_band)
                        {
                            signal = Some(OptionSignal {
                                side,
                                entry_price: candle.close,
                                stop_loss: high,
                                target_band,
                                entry_tsi: item.tsi,
                                confirmation_at,
                                signal_at: candle.at,
                            });
                        }
                    } else if candle.close > item.middle {
                        setup = Setup::AwaitConfirmation;
                    }
                }
            },
        }
    }
    signal
}

fn option_signal_risk_reward(
    side: OptionSide,
    entry: f64,
    stop: f64,
    target: f64,
) -> Option<(f64, f64, f64)> {
    if [entry, stop, target]
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return None;
    }
    let (risk, reward) = match side {
        OptionSide::Call => (entry - stop, target - entry),
        OptionSide::Put => (stop - entry, entry - target),
    };
    (risk > 0.0 && reward > 0.0).then_some((risk, reward, reward / risk))
}

fn option_signal_has_min_rr(side: OptionSide, entry: f64, stop: f64, target: f64) -> bool {
    option_signal_risk_reward(side, entry, stop, target)
        .is_some_and(|(_risk, _reward, ratio)| ratio >= 1.0)
}

fn option_levels(snapshot: &Snapshot, side: OptionSide) -> Option<(f64, f64)> {
    match side {
        OptionSide::Call => Some((snapshot.buy_target?, snapshot.buy_sl1?)),
        OptionSide::Put => Some((snapshot.sell_target?, snapshot.sell_sl1?)),
    }
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

fn option_entry_allowed(now: DateTime<FixedOffset>) -> bool {
    let minute = option_minute_of_day(now);
    (OPTION_ENTRY_START_MINUTE..OPTION_SQUARE_OFF_MINUTE).contains(&minute)
}

fn option_square_off_due(now: DateTime<FixedOffset>) -> bool {
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

fn option_exit(
    item: IndicatorCandle,
    side: OptionSide,
    stop_loss: f64,
) -> Option<(&'static str, f64)> {
    match side {
        OptionSide::Call => {
            if item.candle.low <= stop_loss && item.candle.close < stop_loss {
                Some(("SL1", item.candle.close))
            } else if item.candle.high >= item.upper {
                Some(("TARGET", item.candle.close))
            } else {
                None
            }
        }
        OptionSide::Put => {
            if item.candle.high >= stop_loss && item.candle.close > stop_loss {
                Some(("SL1", item.candle.close))
            } else if item.candle.low <= item.lower {
                Some(("TARGET", item.candle.close))
            } else {
                None
            }
        }
    }
}

fn option_exit_since(
    indicators: &[IndicatorCandle],
    side: OptionSide,
    stop_loss: f64,
    entry_time: Option<DateTime<Utc>>,
) -> Option<(&'static str, f64, NaiveDateTime)> {
    let offset = FixedOffset::east_opt(19_800).expect("valid IST offset");
    let entry_at = entry_time.map(|value| value.with_timezone(&offset).naive_local());
    indicators.iter().find_map(|item| {
        if entry_at.is_some_and(|entry_at| item.candle.at <= entry_at) {
            return None;
        }
        option_exit(*item, side, stop_loss)
            .map(|(role, index_price)| (role, index_price, item.candle.at))
    })
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

fn parse_option_strike(value: &str) -> Option<f64> {
    value.parse::<f64>().ok().map(|value| value / 100.0)
}

fn sensex_option_candidates(
    contracts: &[MasterContract],
    date: NaiveDate,
    option_type: &'static str,
) -> Vec<OptionContract> {
    let mut candidates: Vec<OptionContract> = contracts
        .iter()
        .filter(|item| {
            item.exch_seg == "BFO"
                && item.name == "SENSEX"
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

fn sensex_option_expiry_preview(
    contracts: &[MasterContract],
    date: NaiveDate,
) -> Option<(NaiveDate, i32)> {
    ["CE", "PE"]
        .into_iter()
        .flat_map(|option_type| sensex_option_candidates(contracts, date, option_type))
        .min_by_key(|contract| contract.expiry)
        .map(|contract| (contract.expiry, contract.lot_size))
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

fn choose_premium_contract(
    candidates: &[OptionContract],
    premiums: &HashMap<String, f64>,
    underlying_ltp: f64,
) -> Option<OptionContract> {
    candidates
        .iter()
        .filter_map(|contract| {
            let premium = premiums.get(&contract.token).copied()?;
            (OPTION_MIN_PREMIUM..=OPTION_MAX_PREMIUM)
                .contains(&premium)
                .then(|| {
                    let mut selected = contract.clone();
                    selected.premium = premium;
                    selected
                })
        })
        .min_by(|left, right| {
            (left.premium - OPTION_TARGET_PREMIUM)
                .abs()
                .total_cmp(&(right.premium - OPTION_TARGET_PREMIUM).abs())
                .then_with(|| {
                    (left.strike - underlying_ltp)
                        .abs()
                        .total_cmp(&(right.strike - underlying_ltp).abs())
                })
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

async fn quote_option_premiums(
    state: &AppState,
    exchange: &str,
    candidates: &[OptionContract],
) -> AppResult<HashMap<String, f64>> {
    let mut premiums = HashMap::new();
    for chunk in candidates.chunks(50) {
        let tokens: Vec<String> = chunk
            .iter()
            .map(|contract| contract.token.clone())
            .collect();
        match shared_market_quote(state, "LTP", json!({exchange:tokens})).await {
            Ok(quote) => premiums.extend(extract_quote_ltps(&quote)),
            Err(error)
                if tokens.len() > 1 && angel::is_contract_unavailable_error(&error.to_string()) =>
            {
                for token in tokens {
                    match shared_market_quote(state, "LTP", json!({exchange:[token.clone()]})).await
                    {
                        Ok(quote) => premiums.extend(extract_quote_ltps(&quote)),
                        Err(error) if angel::is_contract_unavailable_error(&error.to_string()) => {
                            tracing::warn!(
                                exchange,
                                token,
                                error = %error,
                                "skipping unavailable option contract token"
                            );
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            Err(error) if angel::is_contract_unavailable_error(&error.to_string()) => {
                for token in tokens {
                    tracing::warn!(
                        exchange,
                        token,
                        error = %error,
                        "skipping unavailable option contract token"
                    );
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(premiums)
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

async fn select_sensex_option_contract(
    state: &AppState,
    contracts: &[MasterContract],
    date: NaiveDate,
    option_type: &'static str,
    underlying_ltp: f64,
    excluded_tokens: &HashSet<String>,
) -> AppResult<Option<OptionContract>> {
    let mut candidates = sensex_option_candidates(contracts, date, option_type);
    candidates.retain(|contract| !excluded_tokens.contains(&contract.token));
    candidates.sort_by(|left, right| {
        left.expiry.cmp(&right.expiry).then_with(|| {
            (left.strike - underlying_ltp)
                .abs()
                .total_cmp(&(right.strike - underlying_ltp).abs())
        })
    });
    if candidates.is_empty() {
        return Ok(None);
    }

    let mut expiries: Vec<NaiveDate> = candidates.iter().map(|contract| contract.expiry).collect();
    expiries.sort();
    expiries.dedup();
    for expiry in expiries {
        let bucket: Vec<OptionContract> = candidates
            .iter()
            .filter(|contract| contract.expiry == expiry)
            .cloned()
            .collect();
        let premiums = quote_option_premiums(state, "BFO", &bucket).await?;
        if let Some(contract) = choose_premium_contract(&bucket, &premiums, underlying_ltp) {
            return Ok(Some(contract));
        }
    }

    Ok(None)
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
    let mut candles: Vec<IntradayCandle> = raw
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
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
            Some(IntradayCandle {
                at: timestamp,
                open: parse(1)?,
                high: parse(2)?,
                low: parse(3)?,
                close: parse(4)?,
            })
        })
        .collect();
    candles.sort_by_key(|candle| candle.at);
    candles.dedup_by_key(|candle| candle.at);
    candles
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
                && snapshot
                    .previous_close
                    .is_some_and(|value| value.is_finite() && value > 0.0)
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
            let previous_close = required_exit_level(snapshot.previous_close, "previous close")?;
            let buy_entry = required_exit_level(snapshot.buy_entry, "buy entry")?;
            let sell_entry = required_exit_level(snapshot.sell_entry, "sell entry")?;
            let gap = futures_gap_direction(previous_close, market_open).ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Could not calculate a valid gap direction for {}.",
                    snapshot.instrument
                ))
            })?;
            let jumped = futures_gap_entry_was_jumped(gap, market_open, buy_entry, sell_entry)
                .ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "Could not validate the entry jump for {}.",
                        snapshot.instrument
                    ))
                })?;
            let direction = gap.entry_direction();
            let (source, status, entry, exits) = if jumped {
                ("OPENING_RANGE", "WAITING_RANGE", None, None)
            } else if gap == FuturesGapDirection::Flat {
                ("STANDARD", "READY", None, None)
            } else {
                let entry = if gap == FuturesGapDirection::Up {
                    buy_entry
                } else {
                    sell_entry
                };
                let exits = snapshot_exit_levels(&snapshot, direction, entry, false)?;
                ("STANDARD", "READY", Some(entry), Some(exits))
            };
            Ok::<_, AppError>((
                market_open,
                previous_close,
                gap,
                direction,
                source,
                status,
                entry,
                exits,
            ))
        };
        let (market_open, previous_close, gap, direction, source, status, entry, exits) =
            match plan.await {
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
                 planned_entry=$7,planned_target=$8,planned_sl1=$9,planned_sl2=$10,
                 gap_planned_at=NOW()
             WHERE id=$1",
        )
        .bind(snapshot.id)
        .bind(market_open)
        .bind(gap.as_str())
        .bind(direction)
        .bind(source)
        .bind(status)
        .bind(entry)
        .bind(exits.map(|value| value.target))
        .bind(exits.map(|value| value.sl1))
        .bind(exits.map(|value| value.sl2))
        .execute(&mut *tx)
        .await?;
        planned.push((
            snapshot.instrument,
            json!({
                "previous_close": previous_close,
                "market_open": market_open,
                "gap_direction": gap.as_str(),
                "entry_direction": direction,
                "entry_source": source,
                "status": status,
                "standard_entry": entry,
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

fn snapshot_gap_direction(snapshot: &Snapshot) -> AppResult<FuturesGapDirection> {
    match snapshot.gap_direction.as_deref() {
        Some("UP") => Ok(FuturesGapDirection::Up),
        Some("DOWN") => Ok(FuturesGapDirection::Down),
        Some("FLAT") => Ok(FuturesGapDirection::Flat),
        _ => Err(AppError::BadRequest(format!(
            "{} has no valid futures gap direction.",
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
    let gap = snapshot_gap_direction(snapshot)?;
    let direction = gap.entry_direction();
    let entry = futures_opening_range_entry(gap, opening_range_high, opening_range_low)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "Could not calculate an opening-range entry for {}.",
                snapshot.instrument
            ))
        })?;
    let hh2 = required_exit_level(snapshot.hh2, "HH2")?;
    let ll2 = required_exit_level(snapshot.ll2, "LL2")?;
    let hh4 = required_exit_level(snapshot.hh4, "HH4")?;
    let ll4 = required_exit_level(snapshot.ll4, "LL4")?;
    let exits =
        futures_exit_levels_for_entry(direction, entry, hh2, ll2, hh4, ll4).ok_or_else(|| {
            AppError::BadRequest(format!(
                "Could not calculate opening-range exit levels for {}.",
                snapshot.instrument
            ))
        })?;
    sqlx::query(
        "UPDATE strategy_market_snapshots
         SET opening_range_high=$2,opening_range_low=$3,planned_entry=$4,
             planned_target=$5,planned_sl1=$6,planned_sl2=$7,
             gap_plan_status='READY',gap_planned_at=NOW()
         WHERE id=$1 AND gap_plan_status='WAITING_RANGE'",
    )
    .bind(snapshot.id)
    .bind(opening_range_high)
    .bind(opening_range_low)
    .bind(entry)
    .bind(exits.target)
    .bind(exits.sl1)
    .bind(exits.sl2)
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
            "gap_direction": gap.as_str(),
            "entry_direction": direction,
            "entry_source": "OPENING_RANGE",
            "opening_range_high": opening_range_high,
            "opening_range_low": opening_range_low,
            "entry": entry,
            "target": exits.target,
            "sl1": exits.sl1,
            "sl2": exits.sl2,
        }),
    )
    .await;
    Ok(resolved)
}

async fn load_contract_master(state: &AppState) -> AppResult<Arc<Vec<MasterContract>>> {
    contract_master::load(state).await
}

async fn ensure_sensex_option_contract_metadata(
    state: &AppState,
    date: NaiveDate,
) -> AppResult<()> {
    let contracts = load_contract_master(state).await?;
    let mut preview = sensex_option_expiry_preview(&contracts, date);
    if preview.is_none() {
        contract_master::invalidate_cache().await;
        let refreshed = load_contract_master(state).await?;
        preview = sensex_option_expiry_preview(&refreshed, date);
    }
    let Some((expiry, lot_size)) = preview else {
        return Err(AppError::BadRequest(format!(
            "No current BFO SENSEX option expiry is available in the refreshed Angel One contract master for {date}."
        )));
    };
    tracing::debug!(
        %date,
        %expiry,
        lot_size,
        "SENSEX option contract metadata warmed"
    );
    Ok(())
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

async fn sensex_ltp(state: &AppState) -> AppResult<f64> {
    let quote = shared_market_quote(state, "LTP", json!({"BSE":[SENSEX_INDEX_TOKEN]})).await?;
    find_quote_ltp(&quote)
        .ok_or_else(|| AppError::BadRequest("Angel One SENSEX quote did not include LTP.".into()))
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
) -> AppResult<(Snapshot, f64, f64)> {
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
        .bind(underlying)
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
    emit_for(
        state,
        SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
        None,
        config.instrument,
        "supertrend_atm_contract_selected",
        json!({"symbol":contract.symbol,"token":contract.token,"expiry":contract.expiry,"strike":contract.strike,"option_type":contract.option_type,"premium":contract.premium,"underlying_ltp":underlying,"signal_at":signal_at}),
    )
    .await;
    Ok((snapshot, contract.premium, underlying))
}

async fn option_snapshot_for_signal(
    state: &AppState,
    side: OptionSide,
    date: NaiveDate,
) -> AppResult<(Snapshot, f64)> {
    let underlying = sensex_ltp(state).await?;
    let excluded_tokens = HashSet::new();
    let contracts = load_contract_master(state).await?;
    let mut contract = select_sensex_option_contract(
        state,
        &contracts,
        date,
        side.option_type(),
        underlying,
        &excluded_tokens,
    )
    .await?;
    if contract.is_none() {
        contract_master::invalidate_cache().await;
        let refreshed = load_contract_master(state).await?;
        contract = select_sensex_option_contract(
            state,
            &refreshed,
            date,
            side.option_type(),
            underlying,
            &excluded_tokens,
        )
        .await?;
    }
    let contract = contract.ok_or_else(|| {
        AppError::BadRequest(format!(
            "No BFO SENSEX {} option contract with premium between Rs. {OPTION_MIN_PREMIUM:.0} and Rs. {OPTION_MAX_PREMIUM:.0} is available for {date}; Rulenix refreshed the Angel One contract master and could not find a quoteable contract.",
            side.option_type(),
        ))
    })?;
    risk::record_tick(state, "BFO", &contract.token, contract.premium).await?;
    let id = Uuid::new_v4();
    let instrument = side.instrument();
    let now = Utc::now();
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,fetched_at) VALUES ($1,$2,$3,$4,'ready','',$5,$6,$7,$8,'BFO',$11,$9,$10,NOW()) ON CONFLICT (strategy_key,instrument,trade_date,execution_key) DO UPDATE SET status='ready',error='',contract_token=EXCLUDED.contract_token,contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,exchange_segment='BFO',product_type=EXCLUDED.product_type,underlying_token=EXCLUDED.underlying_token,fetched_at=NOW()")
        .bind(id)
        .bind(OPTION_ENTRY_STRATEGY_KEY)
        .bind(instrument)
        .bind(date)
        .bind(&contract.token)
        .bind(&contract.symbol)
        .bind(contract.expiry)
        .bind(contract.lot_size)
        .bind(&contract.symbol)
        .bind(SENSEX_INDEX_TOKEN)
        .bind(OPTION_PRODUCT_TYPE)
        .execute(&state.db)
        .await?;
    let query = format!(
        "{} WHERE strategy_key=$1 AND instrument=$2 AND trade_date=$3 AND execution_key=$4",
        snapshot_select()
    );
    let mut snapshot: Snapshot = sqlx::query_as(&query)
        .bind(OPTION_ENTRY_STRATEGY_KEY)
        .bind(instrument)
        .bind(date)
        .bind(&contract.symbol)
        .fetch_one(&state.db)
        .await?;
    snapshot.fetched_at = now;
    emit_for(
        state,
        OPTION_ENTRY_STRATEGY_KEY,
        None,
        instrument,
        "option_contract_selected",
        json!({"symbol":contract.symbol,"token":contract.token,"expiry":contract.expiry,"strike":contract.strike,"option_type":contract.option_type,"premium":contract.premium,"premium_min":OPTION_MIN_PREMIUM,"premium_max":OPTION_MAX_PREMIUM,"underlying_ltp":underlying}),
    )
    .await;
    Ok((snapshot, contract.premium))
}

async fn option_entry_retry_snapshot(
    state: &AppState,
    original: &Snapshot,
    user_id: Uuid,
) -> AppResult<Option<(Snapshot, f64)>> {
    let Some(side) = OptionSide::from_instrument(&original.instrument) else {
        return Ok(None);
    };
    let old_token = original.contract_token.clone().unwrap_or_default();
    let mut excluded_tokens = HashSet::new();
    if !old_token.trim().is_empty() {
        excluded_tokens.insert(old_token);
    }
    contract_master::invalidate_cache().await;
    let underlying = sensex_ltp(state).await?;
    let contracts = load_contract_master(state).await?;
    let Some(contract) = select_sensex_option_contract(
        state,
        &contracts,
        original.trade_date,
        side.option_type(),
        underlying,
        &excluded_tokens,
    )
    .await?
    else {
        return Ok(None);
    };
    risk::record_tick(state, "BFO", &contract.token, contract.premium).await?;
    let execution_key = format!(
        "{}-retry-{}",
        contract.symbol,
        &user_id.simple().to_string()[..8]
    );
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,buy_entry,buy_target,buy_sl1,sell_entry,sell_target,sell_sl1,previous_close,fetched_at) VALUES ($1,$2,$3,$4,'ready','',$5,$6,$7,$8,'BFO',$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,NOW()) ON CONFLICT (strategy_key,instrument,trade_date,execution_key) DO UPDATE SET status='ready',error='',contract_token=EXCLUDED.contract_token,contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,exchange_segment='BFO',product_type=EXCLUDED.product_type,underlying_token=EXCLUDED.underlying_token,buy_entry=EXCLUDED.buy_entry,buy_target=EXCLUDED.buy_target,buy_sl1=EXCLUDED.buy_sl1,sell_entry=EXCLUDED.sell_entry,sell_target=EXCLUDED.sell_target,sell_sl1=EXCLUDED.sell_sl1,previous_close=EXCLUDED.previous_close,fetched_at=NOW()")
        .bind(id)
        .bind(OPTION_ENTRY_STRATEGY_KEY)
        .bind(&original.instrument)
        .bind(original.trade_date)
        .bind(&contract.token)
        .bind(&contract.symbol)
        .bind(contract.expiry)
        .bind(contract.lot_size)
        .bind(OPTION_PRODUCT_TYPE)
        .bind(&execution_key)
        .bind(SENSEX_INDEX_TOKEN)
        .bind(original.buy_entry)
        .bind(original.buy_target)
        .bind(original.buy_sl1)
        .bind(original.sell_entry)
        .bind(original.sell_target)
        .bind(original.sell_sl1)
        .bind(underlying)
        .execute(&state.db)
        .await?;
    let query = format!(
        "{} WHERE strategy_key=$1 AND instrument=$2 AND trade_date=$3 AND execution_key=$4",
        snapshot_select()
    );
    let snapshot = sqlx::query_as(&query)
        .bind(OPTION_ENTRY_STRATEGY_KEY)
        .bind(&original.instrument)
        .bind(original.trade_date)
        .bind(&execution_key)
        .fetch_one(&state.db)
        .await?;
    Ok(Some((snapshot, contract.premium)))
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

async fn option_entry_retry_contract_snapshot(
    state: &AppState,
    snapshot: &Snapshot,
    user_id: Uuid,
) -> AppResult<Option<(Snapshot, f64)>> {
    match snapshot.strategy_key.as_str() {
        OPTION_ENTRY_STRATEGY_KEY => option_entry_retry_snapshot(state, snapshot, user_id).await,
        SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY => {
            supertrend_retry_snapshot(state, snapshot, user_id).await
        }
        _ => Ok(None),
    }
}

async fn sensex_index_candles(
    state: &AppState,
    lookback: Duration,
    to: DateTime<FixedOffset>,
) -> AppResult<Vec<IntradayCandle>> {
    let to_candle = option_latest_completed_candle_time(to);
    let from_candle = to_candle - lookback;
    cached_sensex_index_candles(state, from_candle, to_candle).await
}

async fn index_candles(
    state: &AppState,
    config: IndexOptionConfig,
    lookback: Duration,
    to: DateTime<FixedOffset>,
) -> AppResult<Vec<IntradayCandle>> {
    let to_candle = option_latest_completed_candle_time(to);
    let from_candle = to_candle - lookback;
    cached_index_candles(state, config, from_candle, to_candle).await
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
    if let Some(error) = fetch_error {
        if let Some(latest) = candles.last() {
            tracing::warn!(
                instrument = config.instrument,
                latest_candle = %latest.at,
                requested_to = %to_candle,
                %error,
                "using cached index candles after Angel historical fetch failed"
            );
        }
    }
    Ok(candles)
}

async fn load_cached_sensex_index_candles(
    state: &AppState,
    from_utc: DateTime<Utc>,
    to_utc: DateTime<Utc>,
) -> AppResult<Vec<IntradayCandle>> {
    let rows: Vec<(DateTime<Utc>, f64, f64, f64, f64)> = sqlx::query_as(
        "SELECT candle_time,open_price,high_price,low_price,close_price
         FROM backtest_market_candles
         WHERE exchange='BSE' AND symbol_token=$1 AND interval_key=$2
           AND candle_time BETWEEN $3 AND $4
         ORDER BY candle_time",
    )
    .bind(SENSEX_INDEX_TOKEN)
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

async fn cache_sensex_index_candles(state: &AppState, candles: &[IntradayCandle]) -> AppResult<()> {
    for candle in candles {
        let candle_time = ist_naive_to_utc(candle.at)?;
        sqlx::query(
            "INSERT INTO backtest_market_candles
             (id,exchange,instrument,symbol_token,trading_symbol,interval_key,candle_time,open_price,high_price,low_price,close_price,volume)
             VALUES ($1,'BSE','SENSEX',$2,'SENSEX',$3,$4,$5,$6,$7,$8,0)
             ON CONFLICT (exchange,symbol_token,interval_key,candle_time)
             DO UPDATE SET instrument=EXCLUDED.instrument,trading_symbol=EXCLUDED.trading_symbol,
                open_price=EXCLUDED.open_price,high_price=EXCLUDED.high_price,
                low_price=EXCLUDED.low_price,close_price=EXCLUDED.close_price,fetched_at=NOW()",
        )
        .bind(Uuid::new_v4())
        .bind(SENSEX_INDEX_TOKEN)
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

async fn cached_sensex_index_candles(
    state: &AppState,
    from_candle: NaiveDateTime,
    to_candle: NaiveDateTime,
) -> AppResult<Vec<IntradayCandle>> {
    let from_utc = ist_naive_to_utc(from_candle)?;
    let to_utc = ist_naive_to_utc(to_candle)?;
    let max_cached: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT MAX(candle_time)
         FROM backtest_market_candles
         WHERE exchange='BSE' AND symbol_token=$1 AND interval_key=$2
           AND candle_time BETWEEN $3 AND $4",
    )
    .bind(SENSEX_INDEX_TOKEN)
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

    if fetch_from <= to_candle
        && !shared_historical_cooldown_active(state, "BSE", SENSEX_INDEX_TOKEN, OPTION_INTERVAL)
            .await
    {
        let fetched = shared_market_candles(
            state,
            "BSE",
            SENSEX_INDEX_TOKEN,
            OPTION_INTERVAL,
            &format!("{}", fetch_from.format("%Y-%m-%d %H:%M")),
            &format!("{}", to_candle.format("%Y-%m-%d %H:%M")),
        )
        .await;
        match fetched {
            Ok(raw) => {
                let candles = parse_intraday_candles(&raw);
                cache_sensex_index_candles(state, &candles).await?;
            }
            Err(error) => {
                if angel::is_rate_limit_error(&error.to_string()) {
                    activate_shared_historical_cooldown(
                        state,
                        "BSE",
                        SENSEX_INDEX_TOKEN,
                        OPTION_INTERVAL,
                    )
                    .await;
                }
                let fresh_enough =
                    max_cached.is_some_and(|cached| cached >= to_utc - Duration::minutes(10));
                if !fresh_enough {
                    return Err(error);
                }
                tracing::warn!(
                    %error,
                    "using recent cached SENSEX candles after Angel historical fetch failed"
                );
            }
        }
    }

    let candles = load_cached_sensex_index_candles(state, from_utc, to_utc).await?;
    if candles.is_empty() {
        return Err(AppError::BadRequest(
            "No cached or broker-returned SENSEX candles are available for Option Entry.".into(),
        ));
    }
    Ok(candles)
}

async fn ensure_option_indicators(
    state: &AppState,
    now: DateTime<FixedOffset>,
    indicators: &mut Option<Vec<IndicatorCandle>>,
) -> AppResult<()> {
    if indicators.is_none() {
        let candles = sensex_index_candles(state, Duration::days(7), now).await?;
        *indicators = Some(indicator_candles(&candles));
    }
    Ok(())
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

#[derive(Debug, FromRow)]
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

fn live_submission_rejection(
    force_demo: bool,
    global_kill: bool,
    user_kill: bool,
    account: Option<(bool, bool, &str, &str)>,
    broker_credentials_present: bool,
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
            Some(_) => None,
        }
    }
}

type ProtectiveRetryRow = (
    Uuid,
    Uuid,
    Option<Uuid>,
    String,
    String,
    String,
    String,
    String,
    i32,
    i32,
    f64,
    Option<f64>,
);

pub(crate) async fn place_strategy_order(
    state: &AppState,
    runner: &Runner,
    snapshot: &Snapshot,
    session: &str,
    order: NewOrder,
) -> AppResult<()> {
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
    let lot_size = snapshot
        .lot_size
        .ok_or_else(|| AppError::BadRequest("Snapshot has no contract lot size.".into()))?;
    let token = snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Snapshot has no contract token.".into()))?;
    let symbol = snapshot
        .contract_symbol
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Snapshot has no contract symbol.".into()))?;
    let quantity = order.quantity.unwrap_or(
        lot_size
            .checked_mul(order.lots)
            .ok_or_else(|| AppError::BadRequest("Order quantity overflow.".into()))?,
    );
    if snapshot.strategy_key == STRATEGY_KEY
        && matches!(order.role, "BUY_ENTRY" | "SELL_ENTRY")
        && order.trade_id.is_none()
    {
        let expected_quantity = lot_size
            .checked_mul(order.lots)
            .ok_or_else(|| AppError::BadRequest("Order quantity overflow.".into()))?;
        if quantity != expected_quantity {
            return Err(AppError::BadRequest(format!(
                "Futures Breakout quantity mismatch: {order_lots} lots × contract lot {lot_size} must be {expected_quantity}, got {quantity}.",
                order_lots = order.lots
            )));
        }
        if user_has_breakout_open_position(state, runner.user_id, &snapshot.instrument).await? {
            return Err(AppError::BadRequest(format!(
                "{} entry skipped because an open Futures Breakout position already exists for {}.",
                order.role, snapshot.instrument
            )));
        }
    }
    let key = format!(
        "{}:{}:{}:{}:{}",
        runner.user_id,
        snapshot.id,
        session,
        order.role,
        order.trade_id.map(|v| v.to_string()).unwrap_or_default()
    );
    let protective = matches!(order.role, "TARGET" | "SL1" | "SL2");
    let mut live_margin = None;
    let mut live_reconciled = runner.trading_mode != "live" || protective;
    let mut entry_credentials = if !protective && runner.trading_mode == "live" {
        Some(state.credentials.load(runner.user_id).await?)
    } else {
        None
    };
    let margin_required = if protective {
        0.0
    } else if runner.trading_mode == "demo" {
        demo_margin_required(state, runner.user_id, snapshot, order.price, quantity).await?
    } else {
        let credentials = entry_credentials
            .as_ref()
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("entry credentials missing")))?;
        match crate::margin::estimate(
            state,
            runner.user_id,
            &credentials.api_key,
            &credentials.jwt_token,
            &snapshot.exchange_segment,
            &snapshot.product_type,
            token,
            symbol,
            order.order_type,
            order.side,
            lot_size,
            order.lots,
        )
        .await
        {
            Ok(estimate) => estimate.margin_required,
            Err(error)
                if !protective
                    && matches!(
                        snapshot.strategy_key.as_str(),
                        STRATEGY_KEY
                            | OPTION_ENTRY_STRATEGY_KEY
                            | SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
                    )
                    && !session.contains(":oroll")
                    && angel::is_contract_unavailable_error(&error.to_string()) =>
            {
                if snapshot.strategy_key != STRATEGY_KEY
                    && matches!(order.role, "BUY_ENTRY" | "SELL_ENTRY")
                    && let Some((refreshed_snapshot, refreshed_price)) =
                        option_entry_retry_contract_snapshot(state, snapshot, runner.user_id)
                            .await?
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
                            "{} was unavailable for broker margin calculation, so Rulenix refreshed the contract master and is retrying with {}.",
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
                    return Err(error);
                }
                if let Some(refreshed_snapshot) = force_refresh_futures_contract_snapshot(
                    state,
                    &snapshot.instrument,
                    snapshot.trade_date,
                )
                .await?
                {
                    let retry_session = session_with_suffix(session, "croll");
                    operational_alert_for(
                        state,
                        &snapshot.strategy_key,
                        Some(runner.user_id),
                        &runner.instrument,
                        "contract_rolled_forward",
                        "warning",
                        &format!(
                            "{} was unavailable for broker margin calculation, so Rulenix refreshed the contract master and is retrying with {}.",
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
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    };
    if !protective && runner.trading_mode == "live" {
        // Margin estimation may refresh an expired Angel session. Reload the
        // encrypted credentials before reconciliation and order submission.
        entry_credentials = Some(state.credentials.load(runner.user_id).await?);
    }
    if runner.trading_mode == "live" && !protective {
        let credentials = entry_credentials
            .as_ref()
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("entry credentials missing")))?;
        match angel::order_book(state, &credentials.api_key, &credentials.jwt_token).await {
            Ok(_) => {
                live_reconciled = true;
                risk::set_reconciliation_health(
                    state,
                    runner.user_id,
                    true,
                    "Broker order book reconciled before entry",
                )
                .await?;
            }
            Err(error) => {
                risk::set_reconciliation_health(state, runner.user_id, false, &error.to_string())
                    .await?;
            }
        }
        if let Ok(value) =
            angel::get_margin(state, &credentials.api_key, &credentials.jwt_token).await
        {
            live_margin = value.get("available_balance").and_then(Value::as_f64);
        }
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
            margin_required: Some(margin_required),
            idempotency_key: &key,
            snapshot_ready: snapshot.status == "ready",
            snapshot_current: snapshot.trade_date == ist_now().date_naive()
                && Utc::now() - snapshot.fetched_at < Duration::hours(26),
            exchange_segment: &snapshot.exchange_segment,
            contract_token: token,
            live_margin_available: live_margin,
            live_reconciled,
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
    let result = if runner.trading_mode == "live" {
        let claimed = if protective {
            sqlx::query("UPDATE strategy_orders SET status='submitting',submission_attempts=submission_attempts+1,state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status='pending'")
                .bind(id).execute(&state.db).await?.rows_affected() > 0
        } else {
            let mut tx = state.db.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtext('rulenix:risk:global'))")
                .execute(&mut *tx)
                .await?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,0))")
                .bind(runner.user_id)
                .execute(&mut *tx)
                .await?;
            let kills: (bool, bool) = sqlx::query_as("SELECT COALESCE((SELECT enabled FROM risk_kill_switches WHERE user_id IS NULL),FALSE),COALESCE((SELECT enabled FROM risk_kill_switches WHERE user_id=$1),FALSE)")
                .bind(runner.user_id)
                .fetch_one(&mut *tx)
                .await?;
            let account: Option<(bool, bool, String, String)> = sqlx::query_as("SELECT u.is_active,u.can_live_trade,COALESCE(p.trading_mode,'demo'),COALESCE(p.last_token_status,'') FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=$1")
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
        let credentials = state.credentials.load(runner.user_id).await?;
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
                let stored: StoredOrder = sqlx::query_as("SELECT id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,margin_required,broker_order_id,client_order_id,status,filled_quantity,processed_quantity,average_fill_price::float8 FROM strategy_orders WHERE id=$1")
                    .bind(id).fetch_one(&state.db).await?;
                Box::pin(complete_order(state, stored, order.price)).await?;
            }
            Ok(())
        }
        Err(error) => {
            let contract_unavailable = !protective
                && matches!(
                    snapshot.strategy_key.as_str(),
                    STRATEGY_KEY
                        | OPTION_ENTRY_STRATEGY_KEY
                        | SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
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
            sqlx::query("UPDATE strategy_orders SET status=$2,broker_status=$3,broker_error_class=$4,broker_error_code=$5,broker_http_status=$6,state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status='submitting'")
                .bind(id).bind(status).bind(&diagnostic).bind(error.class.as_str()).bind(&error.code).bind(error.status.map(i32::from)).execute(&state.db).await?;
            sqlx::query("INSERT INTO broker_order_events(order_id,user_id,from_state,to_state,event_type,error_class,error_code,http_status,diagnostic) VALUES($1,$2,'submitting',$3,'submission_failed',$4,$5,$6,$7)")
                .bind(id).bind(runner.user_id).bind(status).bind(error.class.as_str()).bind(&error.code).bind(error.status.map(i32::from)).bind(&diagnostic).execute(&state.db).await?;
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
            if contract_unavailable && !session.contains(":oroll") {
                if snapshot.strategy_key != STRATEGY_KEY
                    && matches!(order.role, "BUY_ENTRY" | "SELL_ENTRY")
                    && let Some((refreshed_snapshot, refreshed_price)) =
                        option_entry_retry_contract_snapshot(state, snapshot, runner.user_id)
                            .await?
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

async fn place_entries(
    state: &AppState,
    runner: &Runner,
    snapshot: &Snapshot,
    session: &str,
) -> AppResult<()> {
    if snapshot.status != "ready" {
        return Err(AppError::BadRequest(
            snapshot
                .error
                .clone()
                .unwrap_or_else(|| "Market snapshot is not ready.".into()),
        ));
    }
    if snapshot.gap_plan_status.as_deref() != Some("READY") {
        return Err(AppError::BadRequest(format!(
            "{} gap entry plan is not ready.",
            snapshot.instrument
        )));
    }
    if user_has_breakout_open_position(state, runner.user_id, &snapshot.instrument).await? {
        emit(
            state,
            Some(runner.user_id),
            &snapshot.instrument,
            "entry_skipped",
            json!({
                "reason":"OPEN_POSITION_EXISTS",
                "message":"Futures Breakout entry skipped because an open position already exists for this instrument."
            }),
        )
        .await;
        return Ok(());
    }
    if user_has_active_breakout_entry_order(state, runner.user_id, &snapshot.instrument).await? {
        emit(
            state,
            Some(runner.user_id),
            &snapshot.instrument,
            "entry_skipped",
            json!({
                "reason":"ACTIVE_ENTRY_ORDER_EXISTS",
                "message":"Futures Breakout entry skipped because an entry order is already active for this instrument."
            }),
        )
        .await;
        return Ok(());
    }
    let orders = match snapshot.entry_direction.as_deref() {
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
    };
    if let Some(token) = snapshot.contract_token.clone() {
        crate::market_ws::ensure_strategy_feed(
            state.clone(),
            snapshot.exchange_segment.clone(),
            token,
        )
        .await;
    }
    for (role, side, price) in orders {
        place_strategy_order(
            state,
            runner,
            snapshot,
            session,
            NewOrder {
                role,
                side,
                order_type: "STOPLOSS_LIMIT",
                lots: runner.lots,
                price,
                trigger: Some(price),
                trade_id: None,
                quantity: None,
            },
        )
        .await?;
    }
    Ok(())
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

async fn user_has_active_breakout_entry_order(
    state: &AppState,
    user_id: Uuid,
    instrument: &str,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1
            FROM strategy_orders o
            JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
            WHERE o.user_id=$1
              AND s.strategy_key=$2
              AND s.instrument=$3
              AND o.role IN ('BUY_ENTRY','SELL_ENTRY')
              AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
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
    let mut tasks = tokio::task::JoinSet::new();
    for runner in runners {
        let state = state.clone();
        let snapshot = snapshot.clone();
        tasks.spawn(async move {
            let result = place_entries(&state, &runner, &snapshot, session).await;
            if let Err(error) = &result {
                tracing::warn!(user=%runner.username,%error,"entry placement failed");
            }
            result
        });
    }
    let mut errors = Vec::new();
    let mut successes = 0_usize;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(())) => successes += 1,
            Ok(Err(error)) => errors.push(error.to_string()),
            Err(error) => errors.push(error.to_string()),
        }
    }
    // A batch is shared by all configured users. One user's margin or broker
    // rejection must not make already successful users retry their orders or
    // age the whole action into a misleading "skipped" state.
    if errors.is_empty() || successes > 0 {
        if !errors.is_empty() {
            tracing::warn!(
                instrument = %instrument,
                session,
                successes,
                failures = errors.len(),
                errors = ?errors,
                "entry batch completed with per-user failures"
            );
        }
        Ok(())
    } else {
        Err(AppError::BadRequest(errors.join("; ")))
    }
}

async fn option_runners(state: &AppState, instrument: &str) -> AppResult<Vec<Runner>> {
    Ok(sqlx::query_as("SELECT c.user_id,u.username,c.instrument,c.lots,c.run_day_session,c.run_evening_session,p.trading_mode FROM user_strategy_configs c JOIN user_strategy_activations a ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key JOIN users u ON u.id=c.user_id JOIN user_profiles p ON p.user_id=c.user_id WHERE c.enabled=TRUE AND a.is_active=TRUE AND c.strategy_key=$1 AND c.instrument=$2 AND u.is_active=TRUE AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))")
        .bind(OPTION_ENTRY_STRATEGY_KEY)
        .bind(instrument)
        .fetch_all(&state.db)
        .await?)
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
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id WHERE t.user_id=$1 AND t.strategy_key=$2 AND t.instrument_label=$3 AND t.status='open' AND t.remaining_lots>0 AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE)) OR EXISTS(SELECT 1 FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.user_id=$1 AND s.strategy_key=$2 AND s.instrument=$3 AND o.role IN ('BUY_ENTRY','SELL_ENTRY') AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE))")
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
    let trades: Vec<(Uuid, String, i32, i32, Option<Uuid>)> = sqlx::query_as(
        "SELECT id,instrument_label,quantity,remaining_lots,strategy_snapshot_id
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
    for (trade_id, instrument, quantity, remaining_lots, snapshot_id) in trades {
        let Some(snapshot_id) = snapshot_id else {
            continue;
        };
        let active_exit_order_types = active_option_exit_order_types(state, trade_id).await?;
        if active_exit_order_types
            .iter()
            .any(|order_type| order_type == "MARKET")
        {
            continue;
        }
        if !active_exit_order_types.is_empty() {
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
            tracing::info!(
                %trade_id,
                user_id=%runner.user_id,
                instrument=%instrument,
                contract=?snapshot.contract_symbol,
                expiry=?snapshot.contract_expiry,
                "deferring expired SuperTrend reversal square-off to expiry checkpoint"
            );
            continue;
        }
        let price = match option_execution_ltp(state, &snapshot).await {
            Ok(price) => price,
            Err(error) => {
                errors.push(format!("{instrument} {trade_id}: {error}"));
                continue;
            }
        };
        let session = format!(
            "strev-{}-{}-{}-{}",
            config.instrument,
            now.format("%Y%m%d"),
            now.format("%H%M"),
            side.option_type()
        );
        if let Err(error) = place_strategy_order(
            state,
            &base_runner,
            &snapshot,
            &session,
            NewOrder {
                role: "SL1",
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

async fn user_has_any_option_exposure(state: &AppState, user_id: Uuid) -> AppResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id WHERE t.user_id=$1 AND t.strategy_key=$2 AND t.status='open' AND t.remaining_lots>0 AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE)) OR EXISTS(SELECT 1 FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.user_id=$1 AND o.role IN ('BUY_ENTRY','SELL_ENTRY') AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND s.strategy_key=$2 AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE))")
        .bind(user_id)
        .bind(OPTION_ENTRY_STRATEGY_KEY)
        .fetch_one(&state.db)
        .await?)
}

async fn has_any_active_option_exit(state: &AppState, trade_id: Uuid) -> AppResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=$1 AND role IN ('TARGET','SL1','SL2') AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))")
        .bind(trade_id)
        .fetch_one(&state.db)
        .await?)
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
              AND t.strategy_key IN ($1,$2,$3)
              AND s.contract_expiry IS NOT NULL
              AND (
                    s.contract_expiry<CURRENT_DATE
                 OR (s.contract_expiry=CURRENT_DATE AND $4::boolean)
              )
        ),
        changed AS (
            UPDATE trades t
            SET status='closed',
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
                exit_datetime=$5,
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
    .bind(OPTION_ENTRY_STRATEGY_KEY)
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
           AND role IN ('TARGET','SL1','SL2')
           AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')",
    )
    .bind(trade_id)
    .fetch_all(&state.db)
    .await?)
}

async fn update_option_snapshot_levels(
    state: &AppState,
    snapshot: &mut Snapshot,
    signal: OptionSignal,
) -> AppResult<()> {
    match signal.side {
        OptionSide::Call => {
            snapshot.buy_entry = Some(signal.entry_price);
            snapshot.buy_target = Some(signal.target_band);
            snapshot.buy_sl1 = Some(signal.stop_loss);
            sqlx::query("UPDATE strategy_market_snapshots SET buy_entry=$2,buy_target=$3,buy_sl1=$4,fetched_at=NOW() WHERE id=$1")
                .bind(snapshot.id)
                .bind(signal.entry_price)
                .bind(signal.target_band)
                .bind(signal.stop_loss)
                .execute(&state.db)
                .await?;
        }
        OptionSide::Put => {
            snapshot.sell_entry = Some(signal.entry_price);
            snapshot.sell_target = Some(signal.target_band);
            snapshot.sell_sl1 = Some(signal.stop_loss);
            sqlx::query("UPDATE strategy_market_snapshots SET sell_entry=$2,sell_target=$3,sell_sl1=$4,fetched_at=NOW() WHERE id=$1")
                .bind(snapshot.id)
                .bind(signal.entry_price)
                .bind(signal.target_band)
                .bind(signal.stop_loss)
                .execute(&state.db)
                .await?;
        }
    }
    Ok(())
}

async fn place_option_entries_for_signal(
    state: &AppState,
    runners: &[Runner],
    mut snapshot: Snapshot,
    signal: OptionSignal,
    execution_price: f64,
) -> AppResult<()> {
    update_option_snapshot_levels(state, &mut snapshot, signal).await?;
    if let Some(token) = snapshot.contract_token.clone() {
        crate::market_ws::ensure_strategy_feed(
            state.clone(),
            snapshot.exchange_segment.clone(),
            token,
        )
        .await;
    }
    let session = format!(
        "opt-{}-{}-{}",
        signal.signal_at.format("%Y%m%d"),
        signal.signal_at.format("%H%M"),
        signal.side.option_type()
    );
    let rr = option_signal_risk_reward(
        signal.side,
        signal.entry_price,
        signal.stop_loss,
        signal.target_band,
    );
    emit_for(
        state,
        OPTION_ENTRY_STRATEGY_KEY,
        None,
        signal.side.instrument(),
        "option_entry_signal",
        json!({
            "side":signal.side.option_type(),
            "signal_at":signal.signal_at,
            "confirmation_at":signal.confirmation_at,
            "index_entry_price":signal.entry_price,
            "option_execution_price":execution_price,
            "stop_loss":signal.stop_loss,
            "target_band":signal.target_band,
            "entry_tsi":signal.entry_tsi,
            "minimum_abs_tsi_required":OPTION_TSI_ENTRY_THRESHOLD,
            "risk_points":rr.map(|value| value.0),
            "reward_points":rr.map(|value| value.1),
            "reward_risk_ratio":rr.map(|value| value.2),
            "minimum_reward_risk_required":1.0
        }),
    )
    .await;
    let mut errors = Vec::new();
    for runner in runners {
        if user_has_any_option_exposure(state, runner.user_id).await? {
            continue;
        }
        if let Err(error) = place_strategy_order(
            state,
            runner,
            &snapshot,
            &session,
            NewOrder {
                role: signal.side.entry_role(),
                side: signal.side.entry_side(),
                order_type: "MARKET",
                lots: runner.lots,
                price: execution_price,
                trigger: None,
                trade_id: None,
                quantity: None,
            },
        )
        .await
        {
            errors.push(format!("{}: {error}", runner.username));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::BadRequest(errors.join("; ")))
    }
}

async fn process_option_entry_side(
    state: &AppState,
    side: OptionSide,
    now: DateTime<FixedOffset>,
    indicators: &[IndicatorCandle],
) -> AppResult<()> {
    let runners = option_runners(state, "SENSEX").await?;
    if runners.is_empty() {
        return Ok(());
    }
    let date = now.date_naive();
    if let Some(signal) = option_signal(indicators, side)
        && signal.signal_at.date() == date
        && indicators
            .last()
            .is_some_and(|latest| latest.candle.at == signal.signal_at)
    {
        let mut eligible = Vec::new();
        for runner in runners {
            if !user_has_any_option_exposure(state, runner.user_id).await? {
                eligible.push(runner);
            }
        }
        if eligible.is_empty() {
            return Ok(());
        }
        let (snapshot, execution_price) = option_snapshot_for_signal(state, side, date).await?;
        place_option_entries_for_signal(state, &eligible, snapshot, signal, execution_price)
            .await?;
    }
    Ok(())
}

async fn process_option_exits(
    state: &AppState,
    now: DateTime<FixedOffset>,
    indicators: &mut Option<Vec<IndicatorCandle>>,
) -> AppResult<()> {
    let trades: Vec<OpenOptionTradeRow> = sqlx::query_as("SELECT id,user_id,instrument_label,quantity,remaining_lots,strategy_snapshot_id,sl1_price,entry_datetime FROM trades WHERE strategy_key=$1 AND status='open' AND remaining_lots>0 AND strategy_snapshot_id IS NOT NULL")
        .bind(OPTION_ENTRY_STRATEGY_KEY)
        .fetch_all(&state.db)
        .await?;
    let mut errors = Vec::new();
    let mut eligible = Vec::new();
    for (
        trade_id,
        user_id,
        instrument,
        quantity,
        remaining_lots,
        snapshot_id,
        stop_loss,
        entry_time,
    ) in trades
    {
        let Some(snapshot_id) = snapshot_id else {
            continue;
        };
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await?;
        if snapshot
            .contract_expiry
            .is_some_and(|expiry| expiry < now.date_naive())
        {
            tracing::warn!(
                %trade_id,
                user_id=%user_id,
                instrument=%instrument,
                contract=?snapshot.contract_symbol,
                expiry=?snapshot.contract_expiry,
                "skipping expired option entry trade during exit scan"
            );
            continue;
        }
        let Some(side) = OptionSide::from_instrument(&instrument) else {
            continue;
        };
        eligible.push((
            trade_id,
            user_id,
            instrument,
            quantity,
            remaining_lots,
            snapshot,
            side,
            stop_loss,
            entry_time,
        ));
    }
    if eligible.is_empty() {
        return Ok(());
    }
    ensure_option_indicators(state, now, indicators).await?;
    let indicators = indicators.as_deref().unwrap_or(&[]);
    for (
        trade_id,
        user_id,
        instrument,
        quantity,
        remaining_lots,
        snapshot,
        side,
        stop_loss,
        entry_time,
    ) in eligible
    {
        let Some((role, index_price, trigger_at)) =
            option_exit_since(indicators, side, stop_loss, entry_time)
        else {
            continue;
        };
        if has_any_active_option_exit(state, trade_id).await? {
            continue;
        }
        let price = match option_execution_ltp(state, &snapshot).await {
            Ok(price) => price,
            Err(error) => {
                errors.push(format!("{instrument} {trade_id}: {error}"));
                continue;
            }
        };
        let runner =
            match runner_for_strategy(state, user_id, OPTION_ENTRY_STRATEGY_KEY, "SENSEX").await {
                Ok(runner) => runner,
                Err(error) => {
                    errors.push(format!("{instrument} {trade_id}: {error}"));
                    continue;
                }
            };
        let session = format!(
            "optx-{}-{}-{}",
            trigger_at.format("%Y%m%d"),
            trigger_at.format("%H%M"),
            role
        );
        if let Err(error) = place_strategy_order(
            state,
            &runner,
            &snapshot,
            &session,
            NewOrder {
                role,
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
            errors.push(error.to_string());
        } else {
            emit_for(
                state,
                OPTION_ENTRY_STRATEGY_KEY,
                Some(user_id),
                &instrument,
                "option_exit_signal",
                json!({"trade_id":trade_id,"role":role,"trigger_at":trigger_at,"index_trigger_price":index_price,"option_execution_price":price}),
            )
            .await;
        }
    }
    if !errors.is_empty() {
        let message = format!(
            "Option Entry exit scan had {} non-fatal error(s): {}",
            errors.len(),
            errors.join("; ")
        );
        tracing::warn!(error=%message, "option entry exit scan continued after non-fatal errors");
        operational_alert_for(
            state,
            OPTION_ENTRY_STRATEGY_KEY,
            None,
            "SENSEX",
            "option_exit_scan_warning",
            "warning",
            &message,
        )
        .await;
    }
    Ok(())
}

async fn process_option_square_off(state: &AppState, now: DateTime<FixedOffset>) -> AppResult<()> {
    let trades: Vec<(Uuid, Uuid, String, i32, i32, Option<Uuid>)> = sqlx::query_as("SELECT id,user_id,instrument_label,quantity,remaining_lots,strategy_snapshot_id FROM trades WHERE strategy_key=$1 AND status='open' AND remaining_lots>0 AND strategy_snapshot_id IS NOT NULL")
        .bind(OPTION_ENTRY_STRATEGY_KEY)
        .fetch_all(&state.db)
        .await?;
    let mut errors = Vec::new();
    for (trade_id, user_id, instrument, quantity, remaining_lots, snapshot_id) in trades {
        let Some(snapshot_id) = snapshot_id else {
            continue;
        };
        if has_any_active_option_exit(state, trade_id).await? {
            continue;
        }
        let Some(side) = OptionSide::from_instrument(&instrument) else {
            continue;
        };
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await?;
        if snapshot
            .contract_expiry
            .is_some_and(|expiry| option_expiry_checkpoint_due(expiry, now))
        {
            tracing::info!(
                %trade_id,
                user_id=%user_id,
                instrument=%instrument,
                contract=?snapshot.contract_symbol,
                expiry=?snapshot.contract_expiry,
                "deferring expired option entry trade to expiry checkpoint"
            );
            continue;
        }
        let price = match option_execution_ltp(state, &snapshot).await {
            Ok(price) => price,
            Err(error) => {
                errors.push(format!("{instrument} {trade_id}: {error}"));
                continue;
            }
        };
        let runner =
            match runner_for_strategy(state, user_id, OPTION_ENTRY_STRATEGY_KEY, "SENSEX").await {
                Ok(runner) => runner,
                Err(error) => {
                    errors.push(format!("{instrument} {trade_id}: {error}"));
                    continue;
                }
            };
        let session = format!("optsq-{}-{}", now.format("%Y%m%d"), now.format("%H%M"));
        if let Err(error) = place_strategy_order(
            state,
            &runner,
            &snapshot,
            &session,
            NewOrder {
                role: "SL1",
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
            errors.push(error.to_string());
        } else {
            emit_for(
                state,
                OPTION_ENTRY_STRATEGY_KEY,
                Some(user_id),
                &instrument,
                "option_intraday_square_off",
                json!({"trade_id":trade_id,"square_off_at":now,"option_execution_price":price}),
            )
            .await;
            append_user_log(
                state,
                user_id,
                &format!(
                    "OPTION INTRADAY SQUARE-OFF {} @ {:.2} [15:20 IST]",
                    contract_log_label(&instrument, snapshot.contract_symbol.as_deref()),
                    price
                ),
            )
            .await;
        }
    }
    if !errors.is_empty() {
        let message = format!(
            "Option Entry square-off scan had {} non-fatal error(s): {}",
            errors.len(),
            errors.join("; ")
        );
        tracing::warn!(error=%message, "option entry square-off continued after non-fatal errors");
        operational_alert_for(
            state,
            OPTION_ENTRY_STRATEGY_KEY,
            None,
            "SENSEX",
            "option_square_off_warning",
            "warning",
            &message,
        )
        .await;
    }
    Ok(())
}

async fn place_supertrend_entries_for_signal(
    state: &AppState,
    config: IndexOptionConfig,
    runners: &[SuperTrendRunner],
    signal: SuperTrendSignal,
    now: DateTime<FixedOffset>,
) -> AppResult<()> {
    let mut errors = Vec::new();
    for runner in runners {
        if runner.target_points <= 0.0 || runner.stop_loss_points <= 0.0 {
            errors.push(format!(
                "{}: TP and SL points must be positive.",
                runner.username
            ));
            continue;
        }
        if user_has_supertrend_side_exposure(state, runner.user_id, config.instrument, signal.side)
            .await?
        {
            continue;
        }
        let opposite = signal.side.opposite();
        if let Err(error) = cancel_supertrend_active_entries_for_side(
            state,
            runner.user_id,
            config.instrument,
            opposite,
            "SuperTrend reversal confirmed; cancelling stale opposite entry.",
        )
        .await
        {
            errors.push(format!("{}: {error}", runner.username));
            continue;
        }
        if let Err(error) = close_supertrend_open_trades_for_side(
            state,
            runner,
            config,
            opposite,
            now,
            "SuperTrend reversal confirmed.",
        )
        .await
        {
            errors.push(format!("{}: {error}", runner.username));
            continue;
        }
        let (snapshot, execution_price, underlying_ltp) =
            match supertrend_option_snapshot_for_signal(
                state,
                config,
                signal.side,
                signal.signal_at.date(),
                signal.signal_at,
                runner.user_id,
                runner.target_points,
                runner.stop_loss_points,
            )
            .await
            {
                Ok(value) => value,
                Err(error) => {
                    errors.push(format!("{}: {error}", runner.username));
                    continue;
                }
            };
        if let Some(token) = snapshot.contract_token.clone() {
            crate::market_ws::ensure_strategy_feed(
                state.clone(),
                snapshot.exchange_segment.clone(),
                token,
            )
            .await;
        }
        let session = format!(
            "st-{}-{}-{}-{}",
            config.instrument,
            signal.signal_at.format("%Y%m%d"),
            signal.signal_at.format("%H%M"),
            signal.side.option_type()
        );
        emit_for(
            state,
            SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
            Some(runner.user_id),
            config.instrument,
            "supertrend_signal",
            json!({
                "side":signal.side.option_type(),
                "signal_at":signal.signal_at,
                "index_close":signal.index_close,
                "index_ltp":underlying_ltp,
                "supertrend":signal.supertrend,
                "previous_direction":signal.previous_direction.as_str(),
                "direction":signal.direction.as_str(),
                "option_execution_price":execution_price,
                "target_points":runner.target_points,
                "stop_loss_points":runner.stop_loss_points,
                "atr_period":SUPERTREND_ATR_PERIOD,
                "factor":SUPERTREND_FACTOR,
            }),
        )
        .await;
        let base_runner = Runner::from(runner.clone());
        if let Err(error) = place_strategy_order(
            state,
            &base_runner,
            &snapshot,
            &session,
            NewOrder {
                role: signal.side.entry_role(),
                side: signal.side.entry_side(),
                order_type: "MARKET",
                lots: runner.lots,
                price: execution_price,
                trigger: None,
                trade_id: None,
                quantity: None,
            },
        )
        .await
        {
            errors.push(format!("{}: {error}", runner.username));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::BadRequest(errors.join("; ")))
    }
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
    let candles =
        index_candles(state, config, Duration::days(SUPERTREND_LOOKBACK_DAYS), now).await?;
    let points = supertrend_points(&candles, SUPERTREND_ATR_PERIOD, SUPERTREND_FACTOR);
    let Some(signal) = recent_supertrend_signal(&points, now) else {
        return Ok(());
    };
    let date = now.date_naive();
    if signal.signal_at.date() != date {
        return Ok(());
    }
    place_supertrend_entries_for_signal(state, config, &runners, signal, now).await
}

async fn process_supertrend_square_off(
    state: &AppState,
    now: DateTime<FixedOffset>,
) -> AppResult<()> {
    let trades: Vec<(Uuid, Uuid, String, i32, i32, Option<Uuid>)> = sqlx::query_as("SELECT id,user_id,instrument_label,quantity,remaining_lots,strategy_snapshot_id FROM trades WHERE strategy_key=$1 AND status='open' AND remaining_lots>0 AND strategy_snapshot_id IS NOT NULL")
        .bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
        .fetch_all(&state.db)
        .await?;
    let mut errors = Vec::new();
    for (trade_id, user_id, instrument, quantity, remaining_lots, snapshot_id) in trades {
        let Some(snapshot_id) = snapshot_id else {
            continue;
        };
        let Some(underlying) = supertrend_snapshot_underlying(&instrument) else {
            continue;
        };
        let active_exit_order_types = active_option_exit_order_types(state, trade_id).await?;
        if active_exit_order_types
            .iter()
            .any(|order_type| order_type == "MARKET")
        {
            continue;
        }
        if !active_exit_order_types.is_empty() {
            if let Err(error) = cancel_active_exits(state, user_id, trade_id).await {
                errors.push(format!("{instrument} {trade_id}: {error}"));
                continue;
            }
            if !active_option_exit_order_types(state, trade_id)
                .await?
                .is_empty()
            {
                continue;
            }
        }
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await?;
        if snapshot
            .contract_expiry
            .is_some_and(|expiry| option_expiry_checkpoint_due(expiry, now))
        {
            tracing::info!(
                %trade_id,
                user_id=%user_id,
                instrument=%instrument,
                contract=?snapshot.contract_symbol,
                expiry=?snapshot.contract_expiry,
                "deferring expired SuperTrend trade to expiry checkpoint"
            );
            continue;
        }
        let price = match option_execution_ltp(state, &snapshot).await {
            Ok(price) => price,
            Err(error) => {
                errors.push(format!("{instrument} {trade_id}: {error}"));
                continue;
            }
        };
        let runner = match runner_for_strategy(
            state,
            user_id,
            SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
            underlying,
        )
        .await
        {
            Ok(runner) => runner,
            Err(error) => {
                errors.push(format!("{instrument} {trade_id}: {error}"));
                continue;
            }
        };
        let session = format!(
            "stsq-{}-{}-{}",
            underlying,
            now.format("%Y%m%d"),
            now.format("%H%M")
        );
        if let Err(error) = place_strategy_order(
            state,
            &runner,
            &snapshot,
            &session,
            NewOrder {
                role: "SL1",
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
            errors.push(error.to_string());
        } else {
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
        process_supertrend_square_off(state, now).await?;
        close_lingering_expired_contract_trades(state, now).await?;
        return Ok(());
    }
    if !option_entry_allowed(now) {
        return Ok(());
    }
    let mut errors = Vec::new();
    for instrument in ["SENSEX", "NIFTY"] {
        let Some(config) = index_option_config(instrument) else {
            continue;
        };
        if let Err(error) = process_supertrend_instrument(state, config, now).await {
            tracing::warn!(
                instrument,
                %error,
                "SuperTrend instrument cycle failed; continuing with remaining instruments"
            );
            errors.push(format!("{instrument}: {error}"));
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

async fn run_option_entry_cycle(state: &AppState, now: DateTime<FixedOffset>) -> AppResult<()> {
    let (open, reason) = session_is_open(state, now.date_naive(), "day").await?;
    if !open {
        operational_alert_for(
            state,
            OPTION_ENTRY_STRATEGY_KEY,
            None,
            "SENSEX",
            "session_skipped",
            "warning",
            &format!("Option strategy skipped: {reason}"),
        )
        .await;
        return Ok(());
    }
    if option_square_off_due(now) {
        process_option_square_off(state, now).await?;
        close_lingering_expired_contract_trades(state, now).await?;
        return Ok(());
    }
    let mut indicators = None;
    process_option_exits(state, now, &mut indicators).await?;
    if !option_entry_allowed(now) {
        return Ok(());
    }
    ensure_option_indicators(state, now, &mut indicators).await?;
    let indicators = indicators.as_deref().unwrap_or(&[]);
    process_option_entry_side(state, OptionSide::Call, now, indicators).await?;
    process_option_entry_side(state, OptionSide::Put, now, indicators).await
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
    if session_key.starts_with("optsq-") || session_key.starts_with("stsq-") {
        "MARKET_CLOSED"
    } else if session_key.starts_with("strev-") {
        "SIGNAL_REVERSAL"
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
                        "STOPLOSS_LIMIT"
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
    if changed.rows_affected() > 0 {
        if reason != "Weekend" {
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
            if is_terminal_scheduler_error(&message) {
                sqlx::query("UPDATE strategy_scheduler_runs SET status='skipped',completed_at=NOW(),last_error=$2,updated_at=NOW() WHERE id=$1")
                    .bind(run_id)
                    .bind(&message)
                    .execute(&state.db)
                    .await?;
                operational_alert(
                    state,
                    None,
                    instrument,
                    "session_skipped",
                    "warning",
                    &format!("{session} {action} skipped: {message}"),
                )
                .await;
            } else {
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

fn is_terminal_scheduler_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower
        .split("; ")
        .all(|part| part.contains("demo balance is insufficient for the required margin"))
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

pub fn start(state: AppState) {
    tokio::spawn(async move {
        let _leader_connection = loop {
            match state.db.acquire().await {
                Ok(mut connection) => {
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
        if let Err(error) = sqlx::query("UPDATE strategy_scheduler_runs SET status='failed',next_attempt_at=NOW(),last_error='Backend restarted while this action was running',updated_at=NOW() WHERE status='running'")
            .execute(&state.db).await {
            tracing::warn!(%error, "could not recover interrupted scheduler runs");
        }
        let startup_date = ist_now().date_naive();
        if let Err(error) = ensure_supported_contract_metadata(&state, startup_date).await {
            for instrument in FUTURES_BREAKOUT_INSTRUMENTS {
                tracing::warn!(%instrument, %error, "startup contract selection failed");
                operational_alert(
                    &state,
                    None,
                    instrument,
                    "contract_selection_failed",
                    "error",
                    &format!("Contract selection failed and will retry: {error}"),
                )
                .await;
            }
        }
        if let Err(error) = ensure_sensex_option_contract_metadata(&state, startup_date).await {
            tracing::warn!(%error, "startup SENSEX option contract metadata failed");
            operational_alert_for(
                &state,
                OPTION_ENTRY_STRATEGY_KEY,
                None,
                "SENSEX",
                "option_contract_metadata_failed",
                "error",
                &format!("SENSEX option contract metadata refresh failed and will retry: {error}"),
            )
            .await;
        }
        if let Err(error) = ensure_supertrend_option_contract_metadata(&state, startup_date).await {
            tracing::warn!(%error, "startup SuperTrend option contract metadata failed");
            operational_alert_for(
                &state,
                SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                None,
                "",
                "supertrend_contract_metadata_failed",
                "error",
                &format!(
                    "SuperTrend option contract metadata refresh failed and will retry: {error}"
                ),
            )
            .await;
        }
        let mut timer = interval(std::time::Duration::from_secs(5));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut dispatched = HashSet::new();
        loop {
            timer.tick().await;
            let now = ist_now();
            let date = now.date_naive();
            dispatched.retain(|key: &String| key.starts_with(&date.to_string()));
            if dispatched.insert(format!("{date}:expire"))
                && let Err(error) = sqlx::query("UPDATE strategy_orders o SET status='cancelled',broker_status='Demo DAY order expired',updated_at=NOW() FROM strategy_market_snapshots s WHERE s.id=o.snapshot_id AND s.trade_date<$1 AND o.execution_mode='demo' AND o.status IN ('pending','submitted')")
                    .bind(date).execute(&state.db).await {
                tracing::warn!(%error, "could not expire prior-day strategy orders");
            }
            if dispatched.insert(format!(
                "{date}:contract-expiry-checkpoint:{}",
                now.format("%H%M")
            )) && let Err(error) = close_lingering_expired_contract_trades(&state, now).await
            {
                tracing::warn!(%error, "could not close lingering expired contract trades");
            }
            let mut contracts_ready = true;
            for instrument in FUTURES_BREAKOUT_INSTRUMENTS {
                let ready = load_snapshot(&state, instrument, date)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|snapshot| has_valid_contract_metadata(&snapshot, date));
                contracts_ready &= ready;
            }
            if !contracts_ready
                && dispatched.insert(format!(
                    "{date}:contracts:{}:{}",
                    now.hour(),
                    now.minute() / 5
                ))
            {
                let cloned = state.clone();
                tokio::spawn(async move {
                    if let Err(error) = ensure_supported_contract_metadata(&cloned, date).await {
                        for instrument in FUTURES_BREAKOUT_INSTRUMENTS {
                            tracing::warn!(%instrument, %error, "daily contract selection failed");
                            operational_alert(
                                &cloned,
                                None,
                                instrument,
                                "contract_selection_failed",
                                "error",
                                &format!("Contract selection failed and will retry: {error}"),
                            )
                            .await;
                        }
                    }
                });
            }
            if dispatched.insert(format!(
                "{date}:sensex-option-contracts:{}:{}",
                now.hour(),
                now.minute() / 5
            )) {
                let cloned = state.clone();
                tokio::spawn(async move {
                    if let Err(error) = ensure_sensex_option_contract_metadata(&cloned, date).await
                    {
                        tracing::warn!(%error, "daily SENSEX option contract metadata failed");
                        operational_alert_for(
                            &cloned,
                            OPTION_ENTRY_STRATEGY_KEY,
                            None,
                            "SENSEX",
                            "option_contract_metadata_failed",
                            "error",
                            &format!(
                                "SENSEX option contract metadata refresh failed and will retry: {error}"
                            ),
                        )
                        .await;
                    }
                });
            }
            if dispatched.insert(format!(
                "{date}:supertrend-option-contracts:{}:{}",
                now.hour(),
                now.minute() / 5
            )) {
                let cloned = state.clone();
                tokio::spawn(async move {
                    if let Err(error) =
                        ensure_supertrend_option_contract_metadata(&cloned, date).await
                    {
                        tracing::warn!(%error, "daily SuperTrend option contract metadata failed");
                        operational_alert_for(
                            &cloned,
                            SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                            None,
                            "",
                            "supertrend_contract_metadata_failed",
                            "error",
                            &format!(
                                "SuperTrend option contract metadata refresh failed and will retry: {error}"
                            ),
                        )
                        .await;
                    }
                });
            }
            let day_open = session_is_open(&state, date, "day")
                .await
                .unwrap_or((false, String::new()))
                .0;
            let evening_open = session_is_open(&state, date, "evening")
                .await
                .unwrap_or((false, String::new()))
                .0;
            if (now.hour(), now.minute()) >= (8, 30) && (day_open || evening_open) {
                for instrument in FUTURES_BREAKOUT_INSTRUMENTS {
                    let snapshot = load_snapshot(&state, instrument, date).await.ok().flatten();
                    let metadata_ready = snapshot
                        .as_ref()
                        .is_some_and(|snapshot| has_valid_contract_metadata(snapshot, date));
                    let levels_ready = snapshot.is_some_and(|snapshot| {
                        snapshot.status == "ready"
                            && snapshot
                                .previous_close
                                .is_some_and(|value| value.is_finite() && value > 0.0)
                    });
                    if metadata_ready
                        && !levels_ready
                        && dispatched.insert(format!(
                            "{date}:levels:{instrument}:{}:{}",
                            now.hour(),
                            now.minute()
                        ))
                    {
                        let cloned = state.clone();
                        tokio::spawn(async move {
                            if let Err(error) = create_snapshot(&cloned, instrument, date).await {
                                record_snapshot_failure(
                                    &cloned,
                                    instrument,
                                    date,
                                    &error.to_string(),
                                )
                                .await;
                                tracing::warn!(%instrument, %error, "daily market snapshot failed");
                                operational_alert(
                                    &cloned,
                                    None,
                                    instrument,
                                    "snapshot_refresh_failed",
                                    "error",
                                    "Market data is temporarily unavailable. No trades will be placed until it recovers",
                                )
                                .await;
                            }
                        });
                    }
                }
            }
            for instrument in FUTURES_BREAKOUT_INSTRUMENTS {
                if let Err(error) = schedule_session(&state, now, instrument, "day", 9).await {
                    tracing::warn!(%instrument, %error, "day scheduler failed");
                }
                if let Err(error) = schedule_session(&state, now, instrument, "evening", 17).await {
                    tracing::warn!(%instrument, %error, "evening scheduler failed");
                }
            }
            let minute_of_day = now.hour() * 60 + now.minute();
            if (OPTION_ENTRY_START_MINUTE..=OPTION_SCHEDULER_END_MINUTE).contains(&minute_of_day)
                && minute_of_day.is_multiple_of(5)
                && dispatched.insert(format!("{date}:option-entry:{}", now.format("%H%M")))
            {
                let cloned = state.clone();
                tokio::spawn(async move {
                    if let Err(error) = run_option_entry_cycle(&cloned, now).await {
                        tracing::warn!(%error, "option entry strategy cycle failed");
                        operational_alert_for(
                            &cloned,
                            OPTION_ENTRY_STRATEGY_KEY,
                            None,
                            "SENSEX",
                            "option_cycle_failed",
                            "error",
                            &format!("Option Entry Strategy V1.0 cycle failed: {error}"),
                        )
                        .await;
                    }
                });
            }
            if (OPTION_ENTRY_START_MINUTE..=OPTION_SCHEDULER_END_MINUTE).contains(&minute_of_day)
                && minute_of_day % 5 == 1
                && dispatched.insert(format!("{date}:supertrend-options:{}", now.format("%H%M")))
            {
                let cloned = state.clone();
                tokio::spawn(async move {
                    if let Err(error) = run_supertrend_cycle(&cloned, now).await {
                        tracing::warn!(%error, "supertrend index options cycle failed");
                        operational_alert_for(
                            &cloned,
                            SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
                            None,
                            "",
                            "supertrend_cycle_failed",
                            "error",
                            &format!("SuperTrend Index Options v1 cycle failed: {error}"),
                        )
                        .await;
                    }
                });
            }
            let active_tokens: Vec<(String, String)> = sqlx::query_as("SELECT DISTINCT s.exchange_segment,s.contract_token FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND s.contract_token IS NOT NULL AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE) UNION SELECT DISTINCT s.exchange_segment,s.contract_token FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id WHERE t.status='open' AND s.contract_token IS NOT NULL AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE)")
                .fetch_all(&state.db).await.unwrap_or_default();
            for (exchange, token) in active_tokens {
                crate::market_ws::ensure_strategy_feed(state.clone(), exchange, token).await;
            }
            if let Err(error) = retry_failed_protective_orders(&state).await {
                tracing::warn!(%error, "protective order recovery failed");
            }
            if let Err(error) = reconcile_live(&state).await {
                tracing::warn!(%error,"strategy order reconciliation failed");
            }
            if let Err(error) = recover_sl2_reversal_intents(&state).await {
                tracing::warn!(%error, "SL2 reversal recovery failed");
            }
        }
    });
}

pub fn refresh_after_broker_connect(state: AppState) {
    tokio::spawn(async move {
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
    if let Err(error) = ensure_sensex_option_contract_metadata(state, date).await {
        snapshot_errors.push(format!("SENSEX option metadata: {error}"));
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
    Ok(json!({
        "detail":"Strategy reload completed.",
        "date":date,
        "retried_scheduler_runs":retried_runs,
        "snapshot_errors":snapshot_errors,
        "retry_errors":retry_errors
    }))
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
    pub margin_required: f64,
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

async fn reconcile_live(state: &AppState) -> AppResult<()> {
    // `pending` is only the short pre-submission reservation state. If the
    // process dies before it atomically claims `submitting`, no broker request
    // was made and the order is safe to retry through its durable strategy
    // action/idempotency key.
    sqlx::query("UPDATE strategy_orders SET status='failed',broker_error_class='retryable',broker_error_code='interrupted_before_submission',broker_status='Backend restarted before broker submission began; safe retry is allowed.',state_version=state_version+1,updated_at=NOW() WHERE status='pending' AND updated_at<NOW()-INTERVAL '30 seconds'")
        .execute(&state.db)
        .await?;
    sqlx::query("UPDATE strategy_orders SET status='ambiguous',broker_error_class='ambiguous',broker_error_code='crash_during_submission',broker_status='Backend restarted while submission was in progress; reconciling without retry.',state_version=state_version+1,updated_at=NOW() WHERE execution_mode='live' AND status='submitting' AND updated_at<NOW()-INTERVAL '30 seconds'")
        .execute(&state.db).await?;
    sqlx::query("UPDATE strategy_orders SET status=CASE WHEN filled_quantity>0 AND filled_quantity<quantity THEN 'partially_filled' ELSE 'submitted' END,broker_status='Recovered after interruption during fill processing',state_version=state_version+1,updated_at=NOW() WHERE execution_mode='live' AND status='processing' AND processed_quantity<filled_quantity AND updated_at<NOW()-INTERVAL '30 seconds'")
        .execute(&state.db).await?;
    let disconnected: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT o.user_id FROM strategy_orders o WHERE o.execution_mode='live' AND o.status IN ('submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND NOT EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=o.user_id AND s.secret_kind='jwt_token')")
        .fetch_all(&state.db).await?;
    for user_id in disconnected {
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
    let profiles: Vec<Uuid>=sqlx::query_scalar("SELECT DISTINCT o.user_id FROM strategy_orders o WHERE o.execution_mode='live' AND o.status IN ('submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=o.user_id AND s.secret_kind='jwt_token')")
        .fetch_all(&state.db).await?;
    let mut tasks = tokio::task::JoinSet::new();
    for user_id in profiles {
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
                order_type: "STOPLOSS_LIMIT",
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

fn valid_order_transition(from: &str, to: &str) -> bool {
    from == to
        || matches!(
            (from, to),
            ("pending", "submitting" | "submitted" | "cancelled")
                | (
                    "submitting",
                    "submitted" | "ambiguous" | "failed" | "rejected"
                )
                | (
                    "ambiguous",
                    "submitted" | "partially_filled" | "rejected" | "cancelled" | "cancelling"
                )
                | (
                    "submitted",
                    "partially_filled"
                        | "processing"
                        | "filled"
                        | "rejected"
                        | "cancelled"
                        | "cancelling"
                )
                | (
                    "partially_filled",
                    "submitted" | "processing" | "filled" | "rejected" | "cancelled" | "cancelling"
                )
                | (
                    "processing",
                    "submitted" | "partially_filled" | "filled" | "cancelled" | "rejected"
                )
                | (
                    "cancelling",
                    "submitted"
                        | "partially_filled"
                        | "filled"
                        | "rejected"
                        | "cancelled"
                        | "processing"
                )
                | ("failed", "pending")
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

async fn reconcile_live_user(state: &AppState, user_id: Uuid) -> AppResult<()> {
    let credentials = state.credentials.load(user_id).await?;
    let values = match angel::order_book(state, &credentials.api_key, &credentials.jwt_token).await
    {
        Ok(values) => values,
        Err(error) => {
            let _ =
                risk::set_reconciliation_health(state, user_id, false, &error.to_string()).await;
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
            return Ok(());
        }
    };
    risk::set_reconciliation_health(state, user_id, true, "Broker order book reconciled").await?;
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
    let orders: Vec<StoredOrder>=sqlx::query_as("SELECT id,user_id,snapshot_id,trade_id,session_key,role,side,order_type,execution_mode,lots,quantity,price,margin_required,broker_order_id,client_order_id,status,filled_quantity,processed_quantity,average_fill_price::float8 FROM strategy_orders WHERE user_id=$1 AND execution_mode='live' AND status IN ('submitting','ambiguous','submitted','partially_filled','processing','cancelling') ORDER BY CASE WHEN role IN ('TARGET','SL1','SL2') THEN 0 ELSE 1 END,created_at")
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
            }
            continue;
        };
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
            filled_order.margin_required =
                order.margin_required * fill_delta as f64 / order.quantity.max(1) as f64;
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
    Ok(())
}

async fn retry_failed_protective_orders(state: &AppState) -> AppResult<()> {
    let orders: Vec<ProtectiveRetryRow> = sqlx::query_as("SELECT o.user_id,o.snapshot_id,o.trade_id,o.session_key,o.role,o.side,o.execution_mode,o.order_type,o.lots,o.quantity,o.price,o.trigger_price FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.execution_mode='live' AND o.status='failed' AND o.role IN ('TARGET','SL1','SL2') AND o.broker_order_id='' AND o.broker_error_class IN ('authentication','retryable') ORDER BY o.created_at LIMIT 100")
        .fetch_all(&state.db).await?;
    for (
        user_id,
        snapshot_id,
        trade_id,
        session,
        role,
        side,
        execution_mode,
        order_type,
        lots,
        quantity,
        price,
        trigger,
    ) in orders
    {
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await?;
        let mut runner =
            runner_for_strategy(state, user_id, &snapshot.strategy_key, &snapshot.instrument)
                .await?;
        runner.trading_mode = execution_mode;
        if state.credentials.load(user_id).await?.jwt_token.is_empty() {
            continue;
        }
        let role = match role.as_str() {
            "TARGET" => "TARGET",
            "SL1" => "SL1",
            "SL2" => "SL2",
            _ => continue,
        };
        let side = if side == "BUY" { "BUY" } else { "SELL" };
        let order_type = match order_type.as_str() {
            "MARKET" => "MARKET",
            "STOPLOSS_LIMIT" => "STOPLOSS_LIMIT",
            "STOPLOSS_MARKET" => "STOPLOSS_MARKET",
            _ => "LIMIT",
        };
        if let Err(error) = place_strategy_order(
            state,
            &runner,
            &snapshot,
            &session,
            NewOrder {
                role,
                side,
                order_type,
                lots,
                price,
                trigger,
                trade_id,
                quantity: Some(quantity),
            },
        )
        .await
        {
            operational_alert_for(
                state,
                &snapshot.strategy_key,
                Some(user_id),
                &snapshot.instrument,
                "protective_order_retry_failed",
                "error",
                &format!("{role} retry failed: {error}"),
            )
            .await;
        }
    }
    Ok(())
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

fn demo_margin_amount(quantity: i32, price: f64, margin_requirement_percent: f64) -> f64 {
    quantity as f64 * price * margin_requirement_percent / 100.0
}

async fn demo_margin_required(
    state: &AppState,
    user_id: Uuid,
    snapshot: &Snapshot,
    price: f64,
    quantity: i32,
) -> AppResult<f64> {
    let margin_percent: f64 = sqlx::query_scalar(
        "SELECT COALESCE(u.margin_requirement_percent,g.margin_requirement_percent)::float8
         FROM risk_limits g
         LEFT JOIN risk_limits u ON u.user_id=$1
         WHERE g.user_id IS NULL",
    )
    .bind(user_id)
    .fetch_one(&state.db)
    .await?;
    let units = runtime_pnl_units(&snapshot.instrument, quantity, snapshot.lot_size);
    Ok((units * price * margin_percent / 100.0).max(0.0))
}

#[cfg(test)]
fn demo_margin_release(
    total_quantity: i32,
    price: f64,
    margin_requirement_percent: f64,
    closed_quantity: i32,
) -> f64 {
    if total_quantity <= 0 || closed_quantity <= 0 {
        return 0.0;
    }
    let full_margin = demo_margin_amount(total_quantity, price, margin_requirement_percent);
    full_margin * (closed_quantity.min(total_quantity) as f64 / total_quantity as f64)
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

async fn complete_option_entry_order(
    state: &AppState,
    order: &StoredOrder,
    snapshot: &Snapshot,
    fill: f64,
    cumulative_fill: i32,
) -> AppResult<bool> {
    if !matches!(order.role.as_str(), "BUY_ENTRY" | "SELL_ENTRY") {
        return Ok(false);
    }
    let (strategy_key, notes, target, stop, exit_side, should_place_protection) = if snapshot
        .strategy_key
        == OPTION_ENTRY_STRATEGY_KEY
    {
        let side = OptionSide::from_instrument(&snapshot.instrument)
            .ok_or_else(|| AppError::BadRequest("Option side is missing.".into()))?;
        let (target, stop) = option_levels(snapshot, side)
            .ok_or_else(|| AppError::BadRequest("Option exit levels are missing.".into()))?;
        (
            OPTION_ENTRY_STRATEGY_KEY,
            "Option Entry Strategy V1.0",
            target,
            stop,
            side.exit_side(),
            false,
        )
    } else if snapshot.strategy_key == SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY {
        let (target_points, stop_points) = supertrend_config_points(snapshot)
            .ok_or_else(|| AppError::BadRequest("SuperTrend TP/SL points are missing.".into()))?;
        let side = supertrend_snapshot_side(&snapshot.instrument)
            .ok_or_else(|| AppError::BadRequest("SuperTrend option side is missing.".into()))?;
        (
            SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
            "SuperTrend Index Options v1",
            fill + target_points,
            (fill - stop_points).max(0.05),
            side.exit_side(),
            true,
        )
    } else {
        return Ok(false);
    };
    let trade_id = Uuid::new_v4();
    let mut fill_tx = state.db.begin().await?;
    if order.execution_mode == "demo" {
        sqlx::query("UPDATE user_profiles SET demo_balance=(GREATEST((demo_balance::float8 - $2),0::numeric))::numeric,updated_at=NOW() WHERE user_id=$1")
            .bind(order.user_id)
            .bind(order.margin_required)
            .execute(&mut *fill_tx)
            .await?;
    }
    sqlx::query("INSERT INTO trades (id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,external_entry_id,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,margin_required) SELECT $1,$2,execution_mode,'open',$3,$4,($5::float8)::numeric,($5::float8)::numeric,0,NOW(),$6,$7,broker_order_id,$8,$9,$10,$11,$11,$12,$13,$15 FROM strategy_orders WHERE id=$14")
        .bind(trade_id)
        .bind(order.user_id)
        .bind(&order.side)
        .bind(order.quantity)
        .bind(fill)
        .bind(&snapshot.instrument)
        .bind(snapshot.contract_symbol.as_deref().unwrap_or(""))
        .bind(notes)
        .bind(strategy_key)
        .bind(snapshot.id)
        .bind(order.lots.max(1))
        .bind(target)
        .bind(stop)
        .bind(order.id)
        .bind(order.margin_required)
        .execute(&mut *fill_tx)
        .await?;
    sqlx::query("UPDATE strategy_orders SET status='filled',trade_id=$2,processed_quantity=GREATEST(processed_quantity,$3),filled_quantity=GREATEST(filled_quantity,$3),updated_at=NOW() WHERE id=$1")
        .bind(order.id)
        .bind(trade_id)
        .bind(cumulative_fill)
        .execute(&mut *fill_tx)
        .await?;
    fill_tx.commit().await?;
    crate::notifications::notify_trade_opened(state.clone(), trade_id);
    emit_for(
        state,
        strategy_key,
        Some(order.user_id),
        &snapshot.instrument,
        if should_place_protection {
            "supertrend_position_opened"
        } else {
            "option_position_opened"
        },
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
    if should_place_protection {
        let underlying = supertrend_snapshot_underlying(&snapshot.instrument).ok_or_else(|| {
            AppError::BadRequest("SuperTrend underlying instrument is missing.".into())
        })?;
        let runner = runner_for_strategy(
            state,
            order.user_id,
            SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY,
            underlying,
        )
        .await?;
        let protection_session = supertrend_protection_session_key(&order.session_key);
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
        place_strategy_order(
            state,
            &runner,
            snapshot,
            &protection_session,
            NewOrder {
                role: "SL1",
                side: exit_side,
                order_type: "STOPLOSS_LIMIT",
                lots: order.lots.max(1),
                price: stop,
                trigger: Some(stop),
                trade_id: Some(trade_id),
                quantity: Some(order.quantity.max(1)),
            },
        )
        .await?;
    }
    Ok(true)
}

async fn complete_claimed_order(state: &AppState, order: StoredOrder, fill: f64) -> AppResult<()> {
    let cumulative_fill = order.cumulative_fill_quantity();
    let query = format!("{} WHERE id=$1", snapshot_select());
    let snapshot: Snapshot = sqlx::query_as(&query)
        .bind(order.snapshot_id)
        .fetch_one(&state.db)
        .await?;
    if complete_option_entry_order(state, &order, &snapshot, fill, cumulative_fill).await? {
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
            if let Some(existing)=sqlx::query_as::<_,(Uuid,String,i32,f64,i32,i32,f64,String,Option<f64>)>("SELECT id,direction,quantity,entry_price::float8,total_lots,remaining_lots,margin_required,COALESCE(contract_symbol,''),target_price::float8 FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 AND status='open' ORDER BY entry_datetime DESC LIMIT 1")
                .bind(order.user_id).bind(STRATEGY_KEY).bind(&instrument).fetch_optional(&state.db).await? {
                if reversal_source_trade_id.is_none() {
                    let message = format!(
                        "{} {} fill ignored because an open Futures Breakout {} position already exists for {}.",
                        order.role, direction, existing.1, instrument
                    );
                    sqlx::query("UPDATE strategy_orders SET status=CASE WHEN execution_mode='live' THEN 'filled' ELSE 'cancelled' END,broker_status=$2,processed_quantity=GREATEST(processed_quantity,$3),filled_quantity=GREATEST(filled_quantity,$3),updated_at=NOW() WHERE id=$1")
                        .bind(order.id)
                        .bind(&message)
                        .bind(cumulative_fill)
                        .execute(&state.db)
                        .await?;
                    operational_alert_for(
                        state,
                        STRATEGY_KEY,
                        Some(order.user_id),
                        &instrument,
                        "entry_blocked_open_position",
                        if order.execution_mode == "live" { "error" } else { "warning" },
                        &message,
                    )
                    .await;
                    append_user_log(state, order.user_id, &message).await;
                    return Ok(());
                }
                if existing.1!=direction {
                    cancel_active_exits(state,order.user_id,existing.0).await?;
                    let pnl=trade_pnl(&existing.1,existing.3,fill,runtime_pnl_units(&instrument, existing.2, snapshot.lot_size));
                    let release_margin = existing.6;
                    sqlx::query("WITH closed AS (UPDATE trades SET status='closed',exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=($3::float8)::numeric,exit_datetime=NOW(),remaining_lots=0,exit_reason='SAR_REVERSAL',notes=CONCAT(notes,'; SAR reversal'),updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$3+$4)::numeric,updated_at=NOW() FROM closed WHERE p.user_id=closed.user_id AND closed.execution_mode='demo'")
                        .bind(existing.0).bind(fill).bind(pnl).bind(release_margin).execute(&state.db).await?;
                    let contract_label = contract_log_label(&instrument, Some(&existing.7));
                    append_user_log(state, order.user_id, &format!("STRATEGY POSITION CLOSED {} SAR @ {:.2} P&L {:+.2}", contract_label, fill, pnl)).await;
                } else {
                    let fixed_target = required_exit_level(existing.8, "fixed target")?;
                    let old_target_lots = target_exit_lots(existing.4);
                    let new_total_lots = existing.4.saturating_add(order.lots.max(0));
                    let new_target_lots = target_exit_lots(new_total_lots);
                    let added_target_lots = (new_target_lots - old_target_lots).max(0);
                    let new_quantity = existing.2.saturating_add(order.quantity);
                    let weighted_entry = (existing.3 * existing.2 as f64
                        + fill * order.quantity as f64)
                        / new_quantity.max(1) as f64;
                    let mut fill_tx = state.db.begin().await?;
                    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,0))")
                        .bind(order.user_id).execute(&mut *fill_tx).await?;
                    if order.execution_mode == "demo" {
                        sqlx::query("UPDATE user_profiles SET demo_balance=(GREATEST((demo_balance::float8-$2),0::numeric))::numeric,updated_at=NOW() WHERE user_id=$1")
                            .bind(order.user_id).bind(order.margin_required).execute(&mut *fill_tx).await?;
                    }
                    sqlx::query("UPDATE trades SET quantity=$2,entry_price=($3::float8)::numeric,last_price=($4::float8)::numeric,total_lots=$5,remaining_lots=remaining_lots+$6,margin_required=margin_required+$7,updated_at=NOW() WHERE id=$1 AND status='open'")
                        .bind(existing.0).bind(new_quantity).bind(weighted_entry).bind(fill)
                        .bind(new_total_lots).bind(order.lots.max(0)).bind(order.margin_required)
                        .execute(&mut *fill_tx).await?;
                    sqlx::query("UPDATE strategy_orders SET status='filled',trade_id=$2,processed_quantity=GREATEST(processed_quantity,$3),filled_quantity=GREATEST(filled_quantity,$3),updated_at=NOW() WHERE id=$1")
                        .bind(order.id).bind(existing.0).bind(cumulative_fill).execute(&mut *fill_tx).await?;
                    if let Some(source_trade_id) = reversal_source_trade_id {
                        sqlx::query("UPDATE strategy_reversal_intents SET status='completed',last_error='',updated_at=NOW() WHERE source_trade_id=$1")
                            .bind(source_trade_id)
                            .execute(&mut *fill_tx)
                            .await?;
                    }
                    fill_tx.commit().await?;

                    let runner = runner_for(state, order.user_id, &instrument).await?;
                    let exit_side = if direction == "BUY" { "SELL" } else { "BUY" };
                    let tranche_session = format!("{}:fill:{}", order.session_key, cumulative_fill);
                    place_strategy_order(
                        state,
                        &runner,
                        &snapshot,
                        &tranche_session,
                        NewOrder {
                            role: "SL1",
                            side: exit_side,
                            order_type: "STOPLOSS_LIMIT",
                            lots: order.lots.max(1),
                            price: sl1,
                            trigger: Some(sl1),
                            trade_id: Some(existing.0),
                            quantity: Some(order.quantity),
                        },
                    )
                    .await?;
                    if added_target_lots > 0 {
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
            let reserved_margin = order.margin_required;
            let mut fill_tx = state.db.begin().await?;
            if order.execution_mode == "demo" {
                sqlx::query("UPDATE user_profiles SET demo_balance=(GREATEST((demo_balance::float8 - $2),0::numeric))::numeric,updated_at=NOW() WHERE user_id=$1")
                    .bind(order.user_id).bind(reserved_margin).execute(&mut *fill_tx).await?;
            }
            sqlx::query("INSERT INTO trades (id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,external_entry_id,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,sl2_price,reversal_of_trade_id,margin_required) SELECT $1,$2,execution_mode,'open',$3,$4,($5::float8)::numeric,($5::float8)::numeric,0,NOW(),$6,$7,broker_order_id,'Futures Breakout v3',$8,$9,$10,$10,$11,$12,$13,$14,$16 FROM strategy_orders WHERE id=$15")
                .bind(trade_id).bind(order.user_id).bind(direction).bind(order.quantity).bind(fill).bind(&instrument).bind(snapshot.contract_symbol.as_deref().unwrap_or(""))
                .bind(STRATEGY_KEY).bind(snapshot.id).bind(order.lots.max(1)).bind(target).bind(sl1).bind(sl2).bind(reversal_source_trade_id).bind(order.id).bind(order.margin_required).execute(&mut *fill_tx).await?;
            sqlx::query("UPDATE strategy_orders SET status='filled',trade_id=$2,processed_quantity=GREATEST(processed_quantity,$3),filled_quantity=GREATEST(filled_quantity,$3),updated_at=NOW() WHERE id=$1").bind(order.id).bind(trade_id).bind(cumulative_fill).execute(&mut *fill_tx).await?;
            if let Some(source_trade_id) = reversal_source_trade_id {
                sqlx::query("UPDATE strategy_reversal_intents SET status='completed',last_error='',updated_at=NOW() WHERE source_trade_id=$1")
                    .bind(source_trade_id)
                    .execute(&mut *fill_tx)
                    .await?;
            }
            fill_tx.commit().await?;
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
            let runner = runner_for(state, order.user_id, &instrument).await?;
            let close_lots = target_exit_lots(order.lots);
            let exit_side = if direction == "BUY" { "SELL" } else { "BUY" };
            place_strategy_order(
                state,
                &runner,
                &snapshot,
                &order.session_key,
                NewOrder {
                    role: "SL1",
                    side: exit_side,
                    order_type: "STOPLOSS_LIMIT",
                    lots: order.lots,
                    price: sl1,
                    trigger: Some(sl1),
                    trade_id: Some(trade_id),
                    quantity: Some(order.quantity),
                },
            )
            .await?;
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
                    "STRATEGY POSITION OPENED {} {} {} lots @ {:.2} {} MARGIN {:.2} [{}]",
                    snapshot_contract_label,
                    direction,
                    order.lots,
                    fill,
                    if reversal_source_trade_id.is_some() {
                        "SL2_REVERSAL"
                    } else {
                        snapshot.entry_source.as_deref().unwrap_or("STANDARD")
                    },
                    order.margin_required,
                    runner.trading_mode.to_uppercase()
                ),
            )
            .await;
        }
        "TARGET" => {
            if let Some(trade_id) = order.trade_id {
                let trade:(String,i32,i32,i32,f64,f64,Option<f64>,String,String)=sqlx::query_as("SELECT direction,total_lots,remaining_lots,quantity,entry_price::float8,margin_required,sl2_price::float8,COALESCE(contract_symbol,''),strategy_key FROM trades WHERE id=$1").bind(trade_id).fetch_one(&state.db).await?;
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
                let release_margin = if trade.3 > 0 {
                    trade.5 * closed_quantity as f64 / trade.3 as f64
                } else {
                    0.0
                };
                let remaining_margin = (trade.5 - release_margin).max(0.0);
                let reporting_quantity = if trade.8 == STRATEGY_KEY {
                    trade
                        .1
                        .saturating_mul(snapshot.lot_size.unwrap_or(1).max(1))
                } else {
                    trade.3
                };
                let mut fill_tx = state.db.begin().await?;
                if remaining_quantity == 0 {
                    sqlx::query("WITH closed AS (UPDATE trades SET status='closed',quantity=$5,remaining_lots=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=(pnl::float8+$3)::numeric,exit_datetime=NOW(),exit_reason=CASE WHEN strategy_key='futures_breakout_v3' THEN 'TP1' ELSE 'TP' END,tp1_exit_price=CASE WHEN strategy_key='futures_breakout_v3' THEN ($2::float8)::numeric ELSE tp1_exit_price END,tp1_exit_datetime=CASE WHEN strategy_key='futures_breakout_v3' THEN NOW() ELSE tp1_exit_datetime END,tp1_exit_quantity=CASE WHEN strategy_key='futures_breakout_v3' THEN $6 ELSE tp1_exit_quantity END,updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$3+$4)::numeric,updated_at=NOW() FROM closed WHERE p.user_id=closed.user_id AND closed.execution_mode='demo'").bind(trade_id).bind(fill).bind(realized).bind(release_margin).bind(reporting_quantity).bind(closed_quantity).execute(&mut *fill_tx).await?;
                } else {
                    sqlx::query("WITH reduced AS (UPDATE trades SET remaining_lots=$2,quantity=$3,last_price=($4::float8)::numeric,pnl=(pnl::float8+$5)::numeric,margin_required=$7,tp1_exit_price=($4::float8)::numeric,tp1_exit_datetime=NOW(),tp1_exit_quantity=tp1_exit_quantity+$8,updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$5+$6)::numeric,updated_at=NOW() FROM reduced WHERE p.user_id=reduced.user_id AND reduced.execution_mode='demo'").bind(trade_id).bind(remaining).bind(remaining_quantity).bind(fill).bind(realized).bind(release_margin).bind(remaining_margin).bind(closed_quantity).execute(&mut *fill_tx).await?;
                }
                sqlx::query("UPDATE strategy_orders SET status='filled',processed_quantity=GREATEST(processed_quantity,$2),filled_quantity=GREATEST(filled_quantity,$2),updated_at=NOW() WHERE id=$1").bind(order.id).bind(cumulative_fill).execute(&mut *fill_tx).await?;
                fill_tx.commit().await?;
                // A live SL1 remains capable of filling until Angel confirms
                // its cancellation. The reconciliation loop creates SL2 only
                // after every earlier protective order is terminal, avoiding
                // overlapping stops at different daily levels.
                if remaining_quantity > 0 && order.execution_mode == "demo" {
                    let runner = runner_for(state, order.user_id, &instrument).await?;
                    let sl2 = required_exit_level(trade.6, "continuation stop loss")?;
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
                let contract_label = contract_log_label(&instrument, Some(&trade.7));
                append_user_log(state, order.user_id, &format!("STRATEGY TARGET FILLED {} {} lots @ {:.2} REALIZED P&L {:+.2}; {} lots remain", contract_label, closed, fill, realized, remaining)).await;
            }
        }
        "SL1" | "SL2" => {
            if let Some(trade_id) = order.trade_id {
                let trade: ExitFillTradeRow = sqlx::query_as("SELECT direction,quantity,remaining_lots,total_lots,entry_price::float8,pnl::float8,margin_required,sl1_price::float8,sl2_price::float8,COALESCE(contract_symbol,''),strategy_key FROM trades WHERE id=$1").bind(trade_id).fetch_one(&state.db).await?;
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
                let release_margin = if trade.1 > 0 {
                    trade.6 * closed_quantity as f64 / trade.1 as f64
                } else {
                    0.0
                };
                let remaining_margin = (trade.6 - release_margin).max(0.0);
                let reversal =
                    if order.role == "SL2" && remaining_quantity == 0 && trade.10 == STRATEGY_KEY {
                        sl2_reversal_plan(&trade.0, trade.3)
                    } else {
                        None
                    };
                let exit_reason = recorded_exit_reason(&trade.10, &order.role, &order.session_key);
                let reporting_quantity = if trade.10 == STRATEGY_KEY {
                    trade
                        .3
                        .saturating_mul(snapshot.lot_size.unwrap_or(1).max(1))
                } else {
                    trade.1
                };
                let mut fill_tx = state.db.begin().await?;
                if remaining_quantity == 0 {
                    sqlx::query("WITH changed AS (UPDATE trades SET status='closed',quantity=$6,remaining_lots=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=($3::float8)::numeric,exit_datetime=NOW(),exit_reason=$7,updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$4+$5)::numeric,updated_at=NOW() FROM changed WHERE p.user_id=changed.user_id AND changed.execution_mode='demo'").bind(trade_id).bind(fill).bind(pnl).bind(closing_pnl).bind(release_margin).bind(reporting_quantity).bind(exit_reason).execute(&mut *fill_tx).await?;
                } else {
                    sqlx::query("WITH changed AS (UPDATE trades SET quantity=$2,remaining_lots=$3,last_price=($4::float8)::numeric,pnl=($5::float8)::numeric,margin_required=$8,updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$6+$7)::numeric,updated_at=NOW() FROM changed WHERE p.user_id=changed.user_id AND changed.execution_mode='demo'").bind(trade_id).bind(remaining_quantity).bind(remaining_lots).bind(fill).bind(pnl).bind(closing_pnl).bind(release_margin).bind(remaining_margin).execute(&mut *fill_tx).await?;
                }
                sqlx::query("UPDATE strategy_orders SET status='filled',processed_quantity=GREATEST(processed_quantity,$2),filled_quantity=GREATEST(filled_quantity,$2),updated_at=NOW() WHERE id=$1").bind(order.id).bind(cumulative_fill).execute(&mut *fill_tx).await?;
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
                        contract_log_label(&instrument, Some(&trade.9)),
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
                            contract_log_label(&instrument, Some(&trade.9)),
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
                        ("SL1", trade.7)
                    } else {
                        ("SL2", trade.8)
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
            sqlx::query("UPDATE strategy_orders SET status='filled',updated_at=NOW() WHERE id=$1")
                .bind(order.id)
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
    let orders:Vec<StoredOrder>=sqlx::query_as("SELECT o.id,o.user_id,o.snapshot_id,o.trade_id,o.session_key,o.role,o.side,o.order_type,o.execution_mode,o.lots,o.quantity,o.price,o.margin_required,o.broker_order_id,o.client_order_id,o.status,o.filled_quantity,o.processed_quantity,o.average_fill_price::float8 FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.user_id=$1 AND o.execution_mode='demo' AND o.status='submitted' AND s.exchange_segment=$2 AND s.contract_token=$3 ORDER BY CASE WHEN o.role IN ('TARGET','SL1','SL2') THEN 0 ELSE 1 END,o.created_at")
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

pub async fn process_tick_shared(
    state: &AppState,
    exchange_segment: &str,
    token: &str,
    ltp: f64,
) -> AppResult<()> {
    if implausible_option_tick(state, exchange_segment, token, ltp).await? {
        return Ok(());
    }
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

pub async fn operational_alert_for(
    state: &AppState,
    strategy_key: &str,
    user_id: Option<Uuid>,
    instrument: &str,
    code: &str,
    severity: &str,
    message: &str,
) {
    let payload = json!({"code":code,"severity":severity,"message":message});
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
                json!({"user_id":user_id,"strategy_key":strategy_key,"instrument":instrument,"message":message}),
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
    let option_active = activation_state_for(&state, user, OPTION_ENTRY_STRATEGY_KEY).await?;
    let supertrend_active =
        activation_state_for(&state, user, SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY).await?;
    let configs: Vec<(String, bool, i32, bool, bool)> = sqlx::query_as("SELECT instrument,enabled,lots,run_day_session,run_evening_session FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2")
        .bind(user).bind(STRATEGY_KEY).fetch_all(&state.db).await?;
    let option_config: Option<(bool, i32, bool, bool)> = sqlx::query_as("SELECT enabled,lots,run_day_session,run_evening_session FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2 AND instrument='SENSEX'")
        .bind(user).bind(OPTION_ENTRY_STRATEGY_KEY).fetch_optional(&state.db).await?;
    let supertrend_configs: Vec<(String, bool, i32, bool, bool, f64, f64)> = sqlx::query_as("SELECT instrument,enabled,lots,run_day_session,run_evening_session,target_points,stop_loss_points FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2")
        .bind(user).bind(SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY).fetch_all(&state.db).await?;
    let snapshots = ensure_supported_contract_metadata(&state, today).await?;
    let option_contracts = load_contract_master(&state).await.ok();
    let option_expiry_preview = option_contracts
        .as_ref()
        .and_then(|contracts| sensex_option_expiry_preview(contracts, today));
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
            "message":"Angel One market-data session is disconnected. Reconnect Angel One before Option Entry can fetch SENSEX candles/options LTP or place live/demo entries."
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
    let option_instrument = option_config.unwrap_or((false, 1, true, false));
    let breakout = json!({
        "key":STRATEGY_KEY,
        "name":"Futures Breakout v3",
        "description":"Four-day MCX futures breakout with stop-and-reverse trade management.",
        "active":active,
        "operational_alerts":alerts,
        "scheduler_runs":runs,
        "instruments":breakout_instruments
    });
    let option_alerts: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'instrument',instrument,'severity',payload->>'severity','code',payload->>'code','message',payload->>'message','created_at',created_at) FROM strategy_events WHERE strategy_key=$1 AND event_type='operational_alert' AND (user_id=$2 OR user_id IS NULL) AND created_at>NOW()-INTERVAL '10 minutes' ORDER BY created_at DESC LIMIT 1")
        .bind(OPTION_ENTRY_STRATEGY_KEY).bind(user).fetch_all(&state.db).await?;
    let option_strategy = json!({
        "key":OPTION_ENTRY_STRATEGY_KEY,
        "name":"Option Entry Strategy V1.0",
        "description":"Intraday 5-minute SENSEX index signals using Keltner Channel retracement confirmation, TSI zero-line filter, and Rs. 220-290 current-week option premium selection.",
        "active":option_active,
        "operational_alerts":option_alerts,
        "scheduler_runs":[],
        "instruments":[{
            "instrument":"SENSEX",
            "label":"SENSEX Options",
            "enabled":option_instrument.0,
            "lots":option_instrument.1,
            "run_day_session":option_instrument.2,
            "run_evening_session":option_instrument.3,
            "snapshot":{
                "strategy_key":OPTION_ENTRY_STRATEGY_KEY,
                "instrument":"SENSEX",
                "status":"ready",
                "execution_key":"catalog-preview",
                "exchange_segment":"BFO",
                "product_type":OPTION_PRODUCT_TYPE,
                "underlying_token":SENSEX_INDEX_TOKEN,
                "contract_expiry":option_expiry_preview.map(|preview| preview.0),
                "lot_size":option_expiry_preview.map(|preview| preview.1),
                "market_data":option_market_data
            }
        }]
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
    Ok(Json(
        json!({"strategies":[breakout,option_strategy,supertrend_strategy]}),
    ))
}

async fn cancel_pending_entries(state: &AppState, user: Uuid, strategy_key: &str) -> AppResult<()> {
    let orders: Vec<(Uuid, String, String)> = sqlx::query_as("SELECT o.id,o.broker_order_id,o.execution_mode FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.user_id=$1 AND s.strategy_key=$2 AND o.role IN ('BUY_ENTRY','SELL_ENTRY') AND o.status='submitted'")
        .bind(user).bind(strategy_key).fetch_all(&state.db).await?;
    let credentials = state.credentials.load(user).await?;
    for (id, broker_id, mode) in orders {
        if mode == "live" && !broker_id.is_empty() {
            if let Err(error) = angel::cancel_order(
                state,
                &credentials.api_key,
                &credentials.jwt_token,
                &broker_id,
                "STOPLOSS",
            )
            .await
            {
                tracing::warn!(%error, %broker_id, "could not cancel strategy entry while deactivating");
                continue;
            }
            sqlx::query("UPDATE strategy_orders SET status='cancelling',broker_status='Strategy deactivation cancellation requested',updated_at=NOW() WHERE id=$1 AND status='submitted'")
                .bind(id).execute(&state.db).await?;
            continue;
        }
        sqlx::query("UPDATE strategy_orders SET status='cancelled',broker_status='Strategy deactivated',updated_at=NOW() WHERE id=$1 AND status='submitted'")
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
        STRATEGY_KEY | OPTION_ENTRY_STRATEGY_KEY | SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
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
        STRATEGY_KEY | OPTION_ENTRY_STRATEGY_KEY | SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
    ) {
        return Err(AppError::NotFound("Strategy not found.".into()));
    }
    let instrument = input
        .instrument
        .unwrap_or_else(|| {
            if matches!(
                strategy_key.as_str(),
                OPTION_ENTRY_STRATEGY_KEY | SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY
            ) {
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
    if strategy_key == OPTION_ENTRY_STRATEGY_KEY && instrument != "SENSEX" {
        return Err(AppError::BadRequest(
            "Option Entry Strategy V1.0 supports only SENSEX.".into(),
        ));
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
        .bind(user).bind(&strategy_key).bind(&instrument).bind(input.enabled).bind(input.lots).bind(input.run_day_session.unwrap_or(true)).bind(input.run_evening_session.unwrap_or(strategy_key != OPTION_ENTRY_STRATEGY_KEY && strategy_key != SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)).bind(target_points).bind(stop_loss_points).execute(&state.db).await?;
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
    let orders:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'role',role,'side',side,'status',status,'lots',lots,'quantity',quantity,'price',price,'trigger_price',trigger_price,'margin_required',margin_required,'client_order_id',client_order_id,'broker_order_id',broker_order_id,'filled_quantity',filled_quantity,'average_fill_price',average_fill_price,'broker_error_class',broker_error_class,'broker_error_code',broker_error_code,'broker_http_status',broker_http_status,'last_reconciled_at',last_reconciled_at,'created_at',created_at) FROM strategy_orders WHERE user_id=$1 ORDER BY created_at DESC LIMIT 100").bind(user).fetch_all(&state.db).await?;
    let trades:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'status',status,'direction',direction,'lots',total_lots,'remaining_lots',remaining_lots,'quantity',quantity,'entry_price',entry_price,'exit_price',exit_price,'pnl',pnl,'margin_required',margin_required,'trigger_time',entry_datetime,'exit_time',exit_datetime,'contract_symbol',contract_symbol,'target',target_price,'sl1',sl1_price,'sl2',sl2_price,'reversal_of_trade_id',reversal_of_trade_id) FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 ORDER BY created_at DESC LIMIT 100").bind(user).bind(STRATEGY_KEY).bind(&instrument).fetch_all(&state.db).await?;
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
    fn contract_for(instrument: &str, expiry: &str, lot_size: i32) -> MasterContract {
        MasterContract {
            token: "1".into(),
            symbol: format!("{instrument}{expiry}FUT"),
            name: instrument.into(),
            expiry: expiry.into(),
            strike: "0.000000".into(),
            lotsize: lot_size.to_string(),
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
    fn supertrend_recent_signal_catches_missed_flip_candle() {
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
            .from_local_datetime(&(base + Duration::minutes(21)))
            .single()
            .unwrap();
        let signal = recent_supertrend_signal(&points, now).unwrap();
        assert_eq!(signal.side, IndexOptionSide::Call);
        assert_eq!(signal.signal_at, candle(10, 110.0).at);
    }

    #[test]
    fn supertrend_recent_signal_ignores_stale_flip_candle() {
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
        assert!(recent_supertrend_signal(&points, now).is_none());
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
        assert!((v.sell_entry - 89.892).abs() < 1e-9);
    }

    #[test]
    fn buy_sl2_tracks_opposite_four_day_breakout_level() {
        let v = calculate(
            &[151_128.0, 152_100.0, 154_074.0, 154_450.0],
            &[147_979.0, 150_001.0, 152_011.0, 152_971.0],
        )
        .unwrap();
        assert!((v.buy_entry - 154_635.34).abs() < 1e-9);
        assert!((v.buy_sl1 - 151_828.5868).abs() < 1e-9);
        assert!((v.buy_sl2 - 147_801.4252).abs() < 1e-9);
        assert!((v.buy_sl2 - v.sell_entry).abs() < 1e-9);
    }

    #[allow(clippy::too_many_arguments)]
    fn ic(
        minute: u32,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        middle: f64,
        upper: f64,
        lower: f64,
        tsi: f64,
    ) -> IndicatorCandle {
        IndicatorCandle {
            candle: IntradayCandle {
                at: NaiveDate::from_ymd_opt(2026, 7, 24)
                    .unwrap()
                    .and_hms_opt(9, minute, 0)
                    .unwrap(),
                open,
                high,
                low,
                close,
            },
            middle,
            upper,
            lower,
            tsi,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ic_at(
        day: u32,
        hour: u32,
        minute: u32,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        middle: f64,
        upper: f64,
        lower: f64,
        tsi: f64,
    ) -> IndicatorCandle {
        IndicatorCandle {
            candle: IntradayCandle {
                at: NaiveDate::from_ymd_opt(2026, 8, day)
                    .unwrap()
                    .and_hms_opt(hour, minute, 0)
                    .unwrap(),
                open,
                high,
                low,
                close,
            },
            middle,
            upper,
            lower,
            tsi,
        }
    }

    #[test]
    fn tsi_values_use_tradingview_percent_scale() {
        let start = NaiveDate::from_ymd_opt(2026, 7, 24)
            .unwrap()
            .and_hms_opt(9, 15, 0)
            .unwrap();
        let candles: Vec<IntradayCandle> = (0..80)
            .map(|index| {
                let close = 100.0 + index as f64;
                IntradayCandle {
                    at: start + Duration::minutes(index),
                    open: close - 0.5,
                    high: close + 1.0,
                    low: close - 1.0,
                    close,
                }
            })
            .collect();

        let last = tsi_values(&candles)
            .into_iter()
            .flatten()
            .last()
            .expect("TSI should be available after warmup");
        assert!(last <= 100.0);
        assert!(last > 95.0);
    }

    #[test]
    fn option_call_signal_enters_after_confirmation_high_break_when_rr_is_valid() {
        let candles = vec![
            ic(15, 100.0, 113.0, 99.0, 112.0, 100.0, 110.0, 90.0, 8.0),
            ic(20, 112.0, 113.0, 99.0, 101.0, 100.0, 110.0, 90.0, 7.0),
            ic(25, 99.0, 106.0, 103.0, 104.0, 100.0, 115.0, 90.0, 0.4),
            ic(30, 104.0, 106.5, 103.5, 105.0, 100.0, 115.0, 90.0, 0.4),
            ic(35, 105.0, 108.0, 104.0, 107.0, 100.0, 115.0, 90.0, 0.6),
        ];
        let signal = option_signal(&candles, OptionSide::Call).unwrap();
        assert_eq!(signal.side, OptionSide::Call);
        assert_eq!(signal.confirmation_at, candles[2].candle.at);
        assert_eq!(signal.signal_at, candles[4].candle.at);
        assert_eq!(signal.stop_loss, 103.0);
        assert_eq!(signal.entry_price, 107.0);
    }

    #[test]
    fn option_put_signal_enters_after_confirmation_low_break_when_rr_is_valid() {
        let candles = vec![
            ic(15, 100.0, 101.0, 87.0, 88.0, 100.0, 110.0, 90.0, -8.0),
            ic(20, 88.0, 101.0, 87.0, 99.0, 100.0, 110.0, 90.0, -7.0),
            ic(25, 101.0, 102.0, 94.0, 96.0, 100.0, 110.0, 80.0, -0.4),
            ic(30, 96.0, 97.0, 94.5, 95.0, 100.0, 110.0, 80.0, -0.4),
            ic(35, 95.0, 96.0, 92.0, 93.0, 100.0, 110.0, 80.0, -0.6),
        ];
        let signal = option_signal(&candles, OptionSide::Put).unwrap();
        assert_eq!(signal.side, OptionSide::Put);
        assert_eq!(signal.confirmation_at, candles[2].candle.at);
        assert_eq!(signal.signal_at, candles[4].candle.at);
        assert_eq!(signal.stop_loss, 102.0);
        assert_eq!(signal.entry_price, 93.0);
    }

    #[test]
    fn option_signal_rejects_entry_near_target_when_rr_is_below_one() {
        let candles = vec![
            ic(15, 100.0, 113.0, 99.0, 112.0, 100.0, 110.0, 90.0, 8.0),
            ic(20, 112.0, 113.0, 99.0, 101.0, 100.0, 110.0, 90.0, 7.0),
            ic(25, 101.0, 106.0, 100.0, 104.0, 100.0, 110.0, 90.0, 6.0),
            ic(30, 104.0, 109.5, 103.0, 109.0, 100.0, 110.0, 90.0, 6.0),
        ];
        assert!(option_signal(&candles, OptionSide::Call).is_none());
    }

    #[test]
    fn option_signal_requires_tsi_to_clear_half_point_threshold_at_entry() {
        let candles = vec![
            ic(15, 100.0, 113.0, 99.0, 112.0, 100.0, 110.0, 90.0, 8.0),
            ic(20, 112.0, 113.0, 99.0, 101.0, 100.0, 110.0, 90.0, 7.0),
            ic(25, 99.0, 106.0, 103.0, 104.0, 100.0, 115.0, 90.0, 8.0),
            ic(30, 104.0, 108.0, 104.0, 107.0, 100.0, 115.0, 90.0, 0.5),
        ];
        assert!(option_signal(&candles, OptionSide::Call).is_none());
    }

    #[test]
    fn option_signal_does_not_carry_setup_across_trading_days() {
        let candles = vec![
            ic_at(
                6, 15, 20, 100.0, 113.0, 99.0, 112.0, 100.0, 110.0, 90.0, 8.0,
            ),
            ic_at(
                6, 15, 25, 112.0, 113.0, 99.0, 101.0, 100.0, 110.0, 90.0, 7.0,
            ),
            ic_at(
                6, 15, 30, 99.0, 106.0, 103.0, 104.0, 100.0, 115.0, 90.0, 8.0,
            ),
            ic_at(
                7, 9, 15, 104.0, 108.0, 104.0, 107.0, 100.0, 115.0, 90.0, 8.0,
            ),
        ];
        assert!(option_signal(&candles, OptionSide::Call).is_none());
    }

    #[test]
    fn option_put_entries_are_long_pe_trades() {
        assert_eq!(OptionSide::Put.entry_role(), "SELL_ENTRY");
        assert_eq!(OptionSide::Put.entry_side(), "BUY");
        assert_eq!(OptionSide::Put.exit_side(), "SELL");
    }

    #[test]
    fn option_entries_stop_before_intraday_square_off() {
        let offset = FixedOffset::east_opt(19_800).unwrap();
        let at = |hour, minute| {
            offset
                .with_ymd_and_hms(2026, 7, 24, hour, minute, 0)
                .single()
                .unwrap()
        };
        assert!(!option_entry_allowed(at(9, 19)));
        assert!(option_entry_allowed(at(9, 20)));
        assert!(option_entry_allowed(at(15, 15)));
        assert!(!option_entry_allowed(at(15, 20)));
        assert!(!option_square_off_due(at(15, 19)));
        assert!(option_square_off_due(at(15, 20)));
    }

    #[test]
    fn option_expiry_checkpoint_runs_on_expiry_square_off_or_later() {
        let offset = FixedOffset::east_opt(19_800).unwrap();
        let at = |day, hour, minute| {
            offset
                .with_ymd_and_hms(2026, 8, day, hour, minute, 0)
                .single()
                .unwrap()
        };
        let expiry = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap();

        assert!(!option_expiry_checkpoint_due(expiry, at(12, 15, 19)));
        assert!(option_expiry_checkpoint_due(expiry, at(12, 15, 20)));
        assert!(option_expiry_checkpoint_due(expiry, at(13, 9, 15)));
        assert!(!option_expiry_checkpoint_due(
            NaiveDate::from_ymd_opt(2026, 8, 13).unwrap(),
            at(12, 15, 20)
        ));
    }

    #[test]
    fn option_exit_scan_ignores_pre_entry_candles_and_catches_later_exit() {
        let offset = FixedOffset::east_opt(19_800).unwrap();
        let entry_time = offset
            .with_ymd_and_hms(2026, 8, 5, 9, 25, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let candles = vec![
            ic_at(5, 9, 20, 100.0, 112.0, 98.0, 111.0, 100.0, 110.0, 90.0, 5.0),
            ic_at(
                5, 9, 30, 111.0, 112.0, 105.0, 106.0, 100.0, 115.0, 90.0, 5.0,
            ),
            ic_at(
                5, 9, 35, 106.0, 116.0, 105.0, 115.5, 100.0, 115.0, 90.0, 5.0,
            ),
        ];

        let exit = option_exit_since(&candles, OptionSide::Call, 99.0, Some(entry_time)).unwrap();

        assert_eq!(exit.0, "TARGET");
        assert_eq!(exit.2, candles[2].candle.at);
    }

    fn option_contract(token: &str, strike: f64) -> OptionContract {
        OptionContract {
            token: token.into(),
            symbol: format!("SENSEX26JUL{}CE", strike as i32),
            expiry: NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
            lot_size: 20,
            strike,
            option_type: "CE",
            premium: 0.0,
        }
    }

    #[test]
    fn option_selection_requires_premium_inside_entry_band() {
        let candidates = vec![
            option_contract("outside_low", 76000.0),
            option_contract("inside_best", 76100.0),
            option_contract("inside_farther", 76200.0),
            option_contract("outside_high", 76300.0),
        ];
        let premiums = HashMap::from([
            ("outside_low".to_string(), 219.95),
            ("inside_best".to_string(), 258.0),
            ("inside_farther".to_string(), 289.0),
            ("outside_high".to_string(), 290.05),
        ]);

        let selected = choose_premium_contract(&candidates, &premiums, 76125.0).unwrap();

        assert_eq!(selected.token, "inside_best");
        assert_eq!(selected.premium, 258.0);
    }

    #[test]
    fn option_selection_skips_when_no_premium_is_in_entry_band() {
        let candidates = vec![
            option_contract("below", 76000.0),
            option_contract("above", 76100.0),
        ];
        let premiums = HashMap::from([("below".to_string(), 219.0), ("above".to_string(), 291.0)]);

        assert!(choose_premium_contract(&candidates, &premiums, 76100.0).is_none());
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
            recorded_exit_reason(OPTION_ENTRY_STRATEGY_KEY, "TARGET", "opt"),
            "TP"
        );
        assert_eq!(
            recorded_exit_reason(OPTION_ENTRY_STRATEGY_KEY, "SL1", "opt"),
            "SL"
        );
        assert_eq!(
            recorded_exit_reason(OPTION_ENTRY_STRATEGY_KEY, "SL1", "optsq-20260812-1520"),
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
    fn gap_entry_helpers_select_one_side_and_replace_jumped_entries() {
        let up = futures_gap_direction(100.0, 105.0).unwrap();
        assert_eq!(up, FuturesGapDirection::Up);
        assert_eq!(up.entry_direction(), "BUY");
        assert_eq!(
            futures_gap_entry_was_jumped(up, 105.0, 104.0, 90.0),
            Some(true)
        );
        assert_eq!(
            futures_gap_entry_was_jumped(up, 101.0, 104.0, 90.0),
            Some(false)
        );
        assert!((futures_opening_range_entry(up, 106.0, 101.0).unwrap() - 106.1272).abs() < 1e-9);

        let down = futures_gap_direction(100.0, 95.0).unwrap();
        assert_eq!(down, FuturesGapDirection::Down);
        assert_eq!(down.entry_direction(), "SELL");
        assert_eq!(
            futures_gap_entry_was_jumped(down, 95.0, 110.0, 96.0),
            Some(true)
        );
        assert_eq!(
            futures_gap_entry_was_jumped(down, 99.0, 110.0, 96.0),
            Some(false)
        );
        assert!((futures_opening_range_entry(down, 99.0, 94.0).unwrap() - 93.8872).abs() < 1e-9);

        let flat = futures_gap_direction(100.0, 100.0).unwrap();
        assert_eq!(flat, FuturesGapDirection::Flat);
        assert_eq!(flat.entry_direction(), "BOTH");
        assert_eq!(
            futures_gap_entry_was_jumped(flat, 100.0, 101.0, 99.0),
            Some(false)
        );
        assert!(futures_opening_range_entry(flat, 101.0, 99.0).is_none());
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
        assert!((crossed_buy.sl1 - 98.5).abs() < 1e-9);
        assert!((crossed_buy.sl2 - 98.5).abs() < 1e-9);

        let crossed_sell =
            futures_exit_levels_for_entry("SELL", 100.0, 90.0, 70.0, 95.0, 60.0).unwrap();
        assert!((crossed_sell.sl1 - 101.5).abs() < 1e-9);
        assert!((crossed_sell.sl2 - 101.5).abs() < 1e-9);
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
        assert!((levels.sl1 - 274.8294).abs() < 1e-9);
        assert!((levels.sl2 - 276.73168).abs() < 1e-9);
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
    fn demo_margin_is_calculated_from_quantity_and_price() {
        assert_eq!(demo_margin_amount(100, 200.0, 10.0), 2000.0);
        assert_eq!(demo_margin_release(100, 200.0, 10.0, 25), 500.0);
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
    fn deterministic_demo_margin_failures_are_terminal() {
        assert!(is_terminal_scheduler_error(
            "Order rejected: demo balance is insufficient for the required margin."
        ));
        assert!(is_terminal_scheduler_error(
            "Order rejected: demo balance is insufficient for the required margin.; Order rejected: demo balance is insufficient for the required margin."
        ));
        assert!(!is_terminal_scheduler_error(
            "Angel One API rate limit is active."
        ));
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
    fn live_submission_guard_rechecks_kills_account_mode_and_session() {
        let safe = Some((true, true, "live", "success"));
        assert_eq!(
            live_submission_rejection(false, false, false, safe, true),
            None
        );
        assert_eq!(
            live_submission_rejection(false, true, false, safe, true).map(|value| value.0),
            Some("global_kill_switch")
        );
        assert_eq!(
            live_submission_rejection(
                false,
                false,
                false,
                Some((true, false, "live", "success")),
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
            )
            .map(|value| value.0),
            Some("trading_mode_changed")
        );
        assert_eq!(
            live_submission_rejection(false, false, false, safe, false).map(|value| value.0),
            Some("broker_session_missing")
        );
    }
}
