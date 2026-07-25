use crate::{
    angel,
    auth::AuthUser,
    error::{AppError, AppResult},
    state::AppState,
    strategy::{OPTION_ENTRY_STRATEGY_KEY, STRATEGY_KEY},
};
use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{Response, header},
};
use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Timelike, Utc, Weekday,
};
use rust_xlsxwriter::Workbook;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

const MASTER_URL: &str =
    "https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json";
const SUPPORTED_INSTRUMENT: &str = "GOLDTEN";
const OPTION_SUPPORTED_INSTRUMENT: &str = "SENSEX";
const SENSEX_INDEX_TOKEN: &str = "99919000";
const OPTION_INTERVAL: &str = "FIVE_MINUTE";
const OPTION_MIN_PREMIUM: f64 = 220.0;
const OPTION_MAX_PREMIUM: f64 = 300.0;
const OPTION_TARGET_PREMIUM: f64 = 260.0;
const OPTION_BACKTEST_MAX_CONTRACTS_PER_SIDE: usize = 24;
const OPTION_BACKTEST_STRIKE_WINDOW: f64 = 5_000.0;
const KELTNER_EMA_PERIOD: usize = 20;
const KELTNER_ATR_PERIOD: usize = 10;
const KELTNER_MULTIPLIER: f64 = 2.0;
const TSI_LONG_PERIOD: usize = 25;
const TSI_SHORT_PERIOD: usize = 13;
const TRADING_DAY_BLOCK_MESSAGE: &str = "Backtesting is disabled for the entire Indian trading day to reserve Angel One API capacity for live market data and order execution. Try again on a weekend or full market holiday.";

#[derive(Debug, Clone, Deserialize)]
struct MasterContract {
    #[serde(deserialize_with = "string_from_any")]
    token: String,
    #[serde(deserialize_with = "string_from_any")]
    symbol: String,
    #[serde(deserialize_with = "string_from_any")]
    name: String,
    #[serde(deserialize_with = "string_from_any")]
    expiry: String,
    #[serde(default, deserialize_with = "string_from_any")]
    strike: String,
    #[serde(deserialize_with = "string_from_any")]
    lotsize: String,
    #[serde(deserialize_with = "string_from_any")]
    instrumenttype: String,
    #[serde(deserialize_with = "string_from_any")]
    exch_seg: String,
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

#[derive(Debug, Clone, FromRow)]
pub(crate) struct Candle {
    pub candle_time: DateTime<Utc>,
    pub open_price: f64,
    pub high_price: f64,
    pub low_price: f64,
    pub close_price: f64,
    pub volume: f64,
}

#[derive(Debug, Clone)]
struct ParsedCandle {
    candle_time: DateTime<Utc>,
    open_price: f64,
    high_price: f64,
    low_price: f64,
    close_price: f64,
    volume: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct ContractSelection {
    pub exchange: String,
    pub token: String,
    pub symbol: String,
    pub lot_size: i32,
    pub buy_margin_per_lot: Option<f64>,
    pub sell_margin_per_lot: Option<f64>,
}

#[derive(Debug)]
pub(crate) struct CacheStats {
    pub data_points: i64,
    pub reused_points: i64,
    pub fetched_points: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestRequest {
    pub strategy_key: Option<String>,
    pub instrument: Option<String>,
    pub interval: Option<String>,
    pub lookback_months: i32,
    pub lots: i32,
}

#[derive(Debug, Serialize)]
struct TradeResult {
    id: Uuid,
    trade_date: NaiveDate,
    direction: String,
    entry_time: DateTime<Utc>,
    entry_price: f64,
    exit_time: DateTime<Utc>,
    exit_price: f64,
    lots: i32,
    quantity: i32,
    margin_per_lot: f64,
    margin_used: f64,
    realized_pnl: f64,
    exit_reason: String,
    levels: Value,
}

#[derive(Debug)]
struct Position {
    trade_date: NaiveDate,
    direction: String,
    entry_time: DateTime<Utc>,
    entry_price: f64,
    lots: i32,
    lot_size: i32,
    remaining_lots: i32,
    pnl_multiplier_per_lot: f64,
    margin_per_lot: f64,
    margin_used: f64,
    realized_pnl: f64,
    target_done: bool,
    levels: Levels,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum OptionBacktestSide {
    Call,
    Put,
}

impl OptionBacktestSide {
    fn option_type(self) -> &'static str {
        match self {
            Self::Call => "CE",
            Self::Put => "PE",
        }
    }

    fn direction(self) -> &'static str {
        match self {
            Self::Call => "BUY",
            Self::Put => "SELL",
        }
    }
}

#[derive(Debug, Clone)]
struct OptionBacktestContract {
    side: OptionBacktestSide,
    exchange: String,
    token: String,
    symbol: String,
    expiry: NaiveDate,
    strike: f64,
    lot_size: i32,
}

#[derive(Debug, Clone)]
struct OptionIndicator {
    candle: Candle,
    middle: f64,
    upper: f64,
    lower: f64,
    tsi: f64,
}

#[derive(Debug, Clone)]
struct OptionBacktestSignal {
    entry_price: f64,
    stop_loss: f64,
    target_band: f64,
    confirmation_at: DateTime<Utc>,
    signal_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct OptionChainSelection {
    token: String,
    symbol: String,
    premium: f64,
    strike: f64,
    underlying: f64,
}

#[derive(Debug, Clone)]
enum OptionSetup {
    Idle,
    AwaitRetrace,
    AwaitConfirmation,
    AwaitBreak {
        high: f64,
        low: f64,
        at: DateTime<Utc>,
    },
}

#[derive(Debug, FromRow)]
struct ExportRun {
    id: Uuid,
    strategy_key: String,
    instrument: String,
    trading_symbol: String,
    interval_key: String,
    from_time: DateTime<Utc>,
    to_time: DateTime<Utc>,
    lots: i32,
    lot_size: i32,
    summary: Value,
    created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct ExportTrade {
    trade_date: NaiveDate,
    direction: String,
    entry_time: DateTime<Utc>,
    entry_price: f64,
    exit_time: DateTime<Utc>,
    exit_price: f64,
    lots: i32,
    quantity: i32,
    realized_pnl: f64,
    exit_reason: String,
    levels: Value,
}

pub fn require_backtest_permission(user: &AuthUser) -> AppResult<()> {
    if user.can_backtest {
        Ok(())
    } else {
        Err(AppError::Forbidden("Backtesting access required.".into()))
    }
}

fn backtesting_allowed_on_date(date: NaiveDate, calendar_sessions: Option<(bool, bool)>) -> bool {
    let market_open = calendar_sessions
        .map(|(morning_open, evening_open)| morning_open || evening_open)
        .unwrap_or_else(|| !matches!(date.weekday(), Weekday::Sat | Weekday::Sun));
    !market_open
}

async fn backtesting_availability(state: &AppState) -> AppResult<Value> {
    let trade_date = Utc::now().with_timezone(&ist_offset()).date_naive();
    let calendar: Option<(bool, bool, String)> = sqlx::query_as(
        "SELECT morning_open,evening_open,reason FROM market_calendar WHERE trade_date=$1",
    )
    .bind(trade_date)
    .fetch_optional(&state.db)
    .await?;
    let allowed = backtesting_allowed_on_date(
        trade_date,
        calendar
            .as_ref()
            .map(|(morning, evening, _)| (*morning, *evening)),
    );
    let calendar_reason = calendar
        .as_ref()
        .map(|(_, _, reason)| reason.as_str())
        .filter(|reason| !reason.is_empty());
    Ok(json!({
        "allowed": allowed,
        "trade_date": trade_date,
        "reason": if allowed {
            calendar_reason.unwrap_or("Non-trading day")
        } else {
            TRADING_DAY_BLOCK_MESSAGE
        }
    }))
}

async fn require_non_trading_day(state: &AppState) -> AppResult<()> {
    let availability = backtesting_availability(state).await?;
    if availability["allowed"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(AppError::BadRequest(TRADING_DAY_BLOCK_MESSAGE.into()))
    }
}

fn ist_offset() -> FixedOffset {
    FixedOffset::east_opt(19_800).expect("valid IST offset")
}

fn string_from_any<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::String(text) => text,
        Value::Number(number) => number.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

fn normalize_interval(value: Option<String>) -> AppResult<String> {
    let interval = value
        .unwrap_or_else(|| "FIFTEEN_MINUTE".into())
        .trim()
        .to_uppercase();
    let allowed = [
        "ONE_MINUTE",
        "FIVE_MINUTE",
        "FIFTEEN_MINUTE",
        "THIRTY_MINUTE",
        "ONE_HOUR",
    ];
    if allowed.contains(&interval.as_str()) {
        Ok(interval)
    } else {
        Err(AppError::BadRequest(
            "Choose a supported interval: ONE_MINUTE, FIVE_MINUTE, FIFTEEN_MINUTE, THIRTY_MINUTE, or ONE_HOUR.".into(),
        ))
    }
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
) -> Option<ContractSelection> {
    contracts
        .iter()
        .filter(|item| {
            item.exch_seg == "MCX"
                && item.name.eq_ignore_ascii_case(instrument)
                && item.instrumenttype == "FUTCOM"
        })
        .filter_map(|item| parse_expiry(&item.expiry).map(|expiry| (item, expiry)))
        .filter(|(_, expiry)| *expiry >= date && weekdays_until(date, *expiry) >= 10)
        .min_by_key(|(_, expiry)| *expiry)
        .and_then(|(contract, _)| {
            let lot_size = contract
                .lotsize
                .parse::<i32>()
                .ok()
                .or_else(|| {
                    contract
                        .lotsize
                        .parse::<f64>()
                        .ok()
                        .map(|value| value as i32)
                })
                .filter(|value| *value > 0)?;
            Some(ContractSelection {
                exchange: "MCX".into(),
                token: contract.token.clone(),
                symbol: contract.symbol.clone(),
                lot_size,
                buy_margin_per_lot: None,
                sell_margin_per_lot: None,
            })
        })
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

fn option_backtest_candidates(
    contracts: &[MasterContract],
    side: OptionBacktestSide,
    date: NaiveDate,
    min_underlying: f64,
    max_underlying: f64,
    reference_underlying: f64,
) -> Vec<OptionBacktestContract> {
    let option_type = side.option_type();
    let mut candidates: Vec<OptionBacktestContract> = contracts
        .iter()
        .filter(|item| {
            item.exch_seg == "BFO"
                && item.name == OPTION_SUPPORTED_INSTRUMENT
                && item.instrumenttype == "OPTIDX"
                && item.symbol.ends_with(option_type)
        })
        .filter_map(|item| {
            let expiry = parse_expiry(&item.expiry)?;
            let lot_size = parse_lot_size(&item.lotsize)?;
            let strike = parse_option_strike(&item.strike)?;
            (expiry >= date
                && strike >= min_underlying - OPTION_BACKTEST_STRIKE_WINDOW
                && strike <= max_underlying + OPTION_BACKTEST_STRIKE_WINDOW)
                .then_some(OptionBacktestContract {
                    side,
                    exchange: "BFO".into(),
                    token: item.token.clone(),
                    symbol: item.symbol.clone(),
                    expiry,
                    strike,
                    lot_size,
                })
        })
        .collect();
    let Some(nearest_expiry) = candidates.iter().map(|contract| contract.expiry).min() else {
        return Vec::new();
    };
    candidates.retain(|contract| contract.expiry == nearest_expiry);
    candidates.sort_by(|left, right| {
        let left_otm_rank = match side {
            OptionBacktestSide::Call => (left.strike < reference_underlying) as i32,
            OptionBacktestSide::Put => (left.strike > reference_underlying) as i32,
        };
        let right_otm_rank = match side {
            OptionBacktestSide::Call => (right.strike < reference_underlying) as i32,
            OptionBacktestSide::Put => (right.strike > reference_underlying) as i32,
        };
        left_otm_rank
            .cmp(&right_otm_rank)
            .then_with(|| {
                (left.strike - reference_underlying)
                    .abs()
                    .total_cmp(&(right.strike - reference_underlying).abs())
            })
            .then_with(|| left.strike.total_cmp(&right.strike))
    });
    candidates.truncate(OPTION_BACKTEST_MAX_CONTRACTS_PER_SIDE);
    candidates
}

async fn load_contract_master(state: &AppState) -> AppResult<Vec<MasterContract>> {
    state
        .http
        .get(MASTER_URL)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!(
                "Unable to download Angel One contract master: {error}"
            ))
        })?
        .error_for_status()
        .map_err(|error| {
            AppError::BadRequest(format!("Angel One contract master failed: {error}"))
        })?
        .json()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Invalid Angel One contract master: {error}"))
        })
}

pub(crate) async fn current_contract(
    state: &AppState,
    instrument: &str,
) -> AppResult<ContractSelection> {
    let cached = cached_contract(state, instrument).await?;
    let contracts = match load_contract_master(state).await {
        Ok(contracts) => contracts,
        Err(error) => {
            return cached.ok_or_else(|| AppError::BadRequest(error.to_string()));
        }
    };
    select_contract(&contracts, instrument, Utc::now().date_naive())
        .or(cached)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "No eligible MCX {instrument} FUTCOM contract is at least 10 trading days from expiry."
            ))
        })
}

async fn cached_contract(
    state: &AppState,
    instrument: &str,
) -> AppResult<Option<ContractSelection>> {
    if let Some((token, symbol, lot_size)) = sqlx::query_as::<_, (String, String, i32)>(
        "SELECT symbol_token,trading_symbol,lot_size FROM backtest_runs WHERE instrument=$1 AND symbol_token<>'' AND trading_symbol<>'' AND lot_size>0 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(instrument)
    .fetch_optional(&state.db)
    .await?
    {
        return Ok(Some(ContractSelection {
            exchange: "MCX".into(),
            token,
            symbol,
            lot_size,
            buy_margin_per_lot: None,
            sell_margin_per_lot: None,
        }));
    }
    let snapshot = sqlx::query_as::<_, (String, String, i32)>(
        "SELECT contract_token,contract_symbol,lot_size FROM strategy_market_snapshots WHERE strategy_key=$1 AND instrument=$2 AND contract_token IS NOT NULL AND contract_symbol IS NOT NULL AND lot_size IS NOT NULL ORDER BY trade_date DESC,fetched_at DESC LIMIT 1",
    )
    .bind(STRATEGY_KEY)
    .bind(instrument)
    .fetch_optional(&state.db)
    .await?;
    Ok(snapshot.map(|(token, symbol, lot_size)| ContractSelection {
        exchange: "MCX".into(),
        token,
        symbol,
        lot_size,
        buy_margin_per_lot: None,
        sell_margin_per_lot: None,
    }))
}

fn numeric(value: Option<&Value>) -> Option<f64> {
    value.and_then(|item| {
        item.as_f64()
            .or_else(|| item.as_str().and_then(|text| text.parse::<f64>().ok()))
    })
}

fn parse_candle_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .or_else(|_| DateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%z"))
        .map(|value| value.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d")
                .ok()
                .and_then(|date| {
                    ist_offset()
                        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
                        .single()
                })
                .map(|value| value.with_timezone(&Utc))
        })
}

fn parse_candles(value: Value) -> Vec<ParsedCandle> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let values = row.as_array()?;
            let candle_time = parse_candle_time(values.first()?.as_str()?)?;
            let open_price = numeric(values.get(1))?;
            let high_price = numeric(values.get(2))?;
            let low_price = numeric(values.get(3))?;
            let close_price = numeric(values.get(4))?;
            let volume = numeric(values.get(5)).unwrap_or(0.0);
            (open_price.is_finite()
                && high_price.is_finite()
                && low_price.is_finite()
                && close_price.is_finite())
            .then_some(ParsedCandle {
                candle_time,
                open_price,
                high_price,
                low_price,
                close_price,
                volume,
            })
        })
        .collect()
}

async fn cached_count(
    state: &AppState,
    exchange: &str,
    token: &str,
    interval: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> AppResult<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM backtest_market_candles WHERE exchange=$1 AND symbol_token=$2 AND interval_key=$3 AND candle_time BETWEEN $4 AND $5")
        .bind(exchange).bind(token).bind(interval).bind(from).bind(to).fetch_one(&state.db).await?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_and_cache(
    state: &AppState,
    user_id: Uuid,
    credentials: &crate::credentials::BrokerCredentials,
    contract: &ContractSelection,
    instrument: &str,
    interval: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> AppResult<i64> {
    let chunk_days = match interval {
        // Keep one-minute requests below Angel One's 8,000-record cap.
        "ONE_MINUTE" => 20,
        "FIVE_MINUTE" => 90,
        "FIFTEEN_MINUTE" | "THIRTY_MINUTE" => 180,
        "ONE_HOUR" => 365,
        "ONE_DAY" => 1_900,
        _ => 90,
    };
    let mut cursor = from;
    let mut total = 0_i64;
    while cursor <= to {
        let chunk_to = (cursor + Duration::days(chunk_days)).min(to);
        let raw = angel::get_candles_with_exchange_interval(
            state,
            &credentials.api_key,
            &credentials.jwt_token,
            &contract.exchange,
            &contract.token,
            interval,
            &cursor
                .with_timezone(&ist_offset())
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            &chunk_to
                .with_timezone(&ist_offset())
                .format("%Y-%m-%d %H:%M")
                .to_string(),
        )
        .await;
        let raw = match raw {
            Ok(value) => value,
            Err(error) => {
                if angel::is_invalid_api_key_error(&error.to_string()) {
                    crate::home::mark_invalid(
                        state,
                        user_id,
                        "Angel One API token is invalid. Please establish the broker connection again.",
                    )
                    .await?;
                }
                return Err(error);
            }
        };
        // Some broker responses include the current forming candle even when
        // the requested upper bound points at the last completed interval.
        // Never let an out-of-range row contaminate the shared historical
        // cache; live evaluators can then safely reuse this table.
        let candles: Vec<ParsedCandle> = parse_candles(raw)
            .into_iter()
            .filter(|candle| candle.candle_time >= cursor && candle.candle_time <= chunk_to)
            .collect();
        total += candles.len() as i64;
        for candle in &candles {
            sqlx::query("INSERT INTO backtest_market_candles (id,exchange,instrument,symbol_token,trading_symbol,interval_key,candle_time,open_price,high_price,low_price,close_price,volume,fetched_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) ON CONFLICT (exchange,symbol_token,interval_key,candle_time) DO UPDATE SET instrument=EXCLUDED.instrument,trading_symbol=EXCLUDED.trading_symbol,open_price=EXCLUDED.open_price,high_price=EXCLUDED.high_price,low_price=EXCLUDED.low_price,close_price=EXCLUDED.close_price,volume=EXCLUDED.volume,fetched_by=EXCLUDED.fetched_by,fetched_at=NOW()")
                .bind(Uuid::new_v4()).bind(&contract.exchange).bind(instrument).bind(&contract.token).bind(&contract.symbol).bind(interval)
                .bind(candle.candle_time).bind(candle.open_price).bind(candle.high_price).bind(candle.low_price).bind(candle.close_price).bind(candle.volume).bind(user_id)
                .execute(&state.db).await?;
        }
        if chunk_to >= to {
            break;
        }
        cursor = chunk_to + Duration::minutes(1);
    }
    Ok(total)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn ensure_candles(
    state: &AppState,
    user_id: Uuid,
    credentials: &crate::credentials::BrokerCredentials,
    contract: &ContractSelection,
    instrument: &str,
    interval: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> AppResult<CacheStats> {
    let before = cached_count(
        state,
        &contract.exchange,
        &contract.token,
        interval,
        from,
        to,
    )
    .await?;
    let bounds: (Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as("SELECT MIN(candle_time),MAX(candle_time) FROM backtest_market_candles WHERE exchange=$1 AND symbol_token=$2 AND interval_key=$3 AND candle_time BETWEEN $4 AND $5")
        .bind(&contract.exchange).bind(&contract.token).bind(interval).bind(from).bind(to).fetch_one(&state.db).await?;
    let mut fetched_points = 0;
    match bounds {
        (Some(min_time), Some(max_time))
            if min_time <= from && max_time >= to - Duration::minutes(90) => {}
        (Some(min_time), Some(max_time)) => {
            if min_time > from {
                fetched_points += fetch_and_cache(
                    state,
                    user_id,
                    credentials,
                    contract,
                    instrument,
                    interval,
                    from,
                    min_time - Duration::minutes(1),
                )
                .await?;
            }
            if max_time < to - Duration::minutes(90) {
                fetched_points += fetch_and_cache(
                    state,
                    user_id,
                    credentials,
                    contract,
                    instrument,
                    interval,
                    max_time + Duration::minutes(1),
                    to,
                )
                .await?;
            }
        }
        _ => {
            fetched_points = fetch_and_cache(
                state,
                user_id,
                credentials,
                contract,
                instrument,
                interval,
                from,
                to,
            )
            .await?;
        }
    }
    let data_points = cached_count(
        state,
        &contract.exchange,
        &contract.token,
        interval,
        from,
        to,
    )
    .await?;
    Ok(CacheStats {
        data_points,
        reused_points: before,
        fetched_points,
    })
}

pub(crate) async fn load_candles(
    state: &AppState,
    exchange: &str,
    token: &str,
    interval: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> AppResult<Vec<Candle>> {
    Ok(sqlx::query_as("SELECT candle_time,open_price,high_price,low_price,close_price,volume FROM backtest_market_candles WHERE exchange=$1 AND symbol_token=$2 AND interval_key=$3 AND candle_time BETWEEN $4 AND $5 ORDER BY candle_time")
        .bind(exchange).bind(token).bind(interval).bind(from).bind(to).fetch_all(&state.db).await?)
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
    Some(Levels {
        hh2,
        ll2,
        hh4,
        ll4,
        buy_entry,
        buy_target: buy_entry * (1.0 + 0.015),
        buy_sl1: (buy_entry * (1.0 - 0.015)).max(ll2 * (1.0 - 0.0012)),
        buy_sl2: (buy_entry * (1.0 - 0.015)).max(ll4 * (1.0 - 0.0012)),
        sell_entry,
        sell_target: sell_entry * (1.0 - 0.015),
        sell_sl1: (sell_entry * (1.0 + 0.015)).min(hh2 * (1.0 + 0.0012)),
        sell_sl2: (sell_entry * (1.0 + 0.015)).min(hh4 * (1.0 + 0.0012)),
    })
}

fn levels_json(levels: Levels) -> Value {
    json!({
        "hh2":levels.hh2,"ll2":levels.ll2,"hh4":levels.hh4,"ll4":levels.ll4,
        "buy_entry":levels.buy_entry,"buy_target":levels.buy_target,"buy_sl1":levels.buy_sl1,"buy_sl2":levels.buy_sl2,
        "sell_entry":levels.sell_entry,"sell_target":levels.sell_target,"sell_sl1":levels.sell_sl1,"sell_sl2":levels.sell_sl2
    })
}

fn trade_pnl(direction: &str, entry: f64, exit: f64, units: f64) -> f64 {
    if direction == "BUY" {
        (exit - entry) * units
    } else {
        (entry - exit) * units
    }
}

fn pnl_multiplier_per_lot(instrument: &str, lot_size: i32) -> f64 {
    if instrument == SUPPORTED_INSTRUMENT {
        1.0
    } else {
        lot_size.max(1) as f64
    }
}

fn margin_per_lot(entry_price: f64, lot_size: i32, margin_requirement_percent: f64) -> f64 {
    entry_price * lot_size as f64 * margin_requirement_percent / 100.0
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

fn option_true_ranges(candles: &[Candle]) -> Vec<f64> {
    candles
        .iter()
        .enumerate()
        .map(|(index, candle)| {
            if index == 0 {
                candle.high_price - candle.low_price
            } else {
                let previous_close = candles[index - 1].close_price;
                (candle.high_price - candle.low_price)
                    .max((candle.high_price - previous_close).abs())
                    .max((candle.low_price - previous_close).abs())
            }
        })
        .collect()
}

fn option_tsi_values(candles: &[Candle]) -> Vec<Option<f64>> {
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
                candle.close_price - candles[index - 1].close_price
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

fn option_indicators(candles: &[Candle]) -> Vec<OptionIndicator> {
    let closes: Vec<f64> = candles.iter().map(|candle| candle.close_price).collect();
    let middle = ema(&closes, KELTNER_EMA_PERIOD);
    let atr = ema(&option_true_ranges(candles), KELTNER_ATR_PERIOD);
    let tsi = option_tsi_values(candles);
    candles
        .iter()
        .enumerate()
        .filter_map(|(index, candle)| {
            let middle = middle[index]?;
            let atr = atr[index]?;
            let tsi = tsi[index]?;
            Some(OptionIndicator {
                candle: candle.clone(),
                middle,
                upper: middle + KELTNER_MULTIPLIER * atr,
                lower: middle - KELTNER_MULTIPLIER * atr,
                tsi,
            })
        })
        .collect()
}

fn option_backtest_exit(
    item: &OptionIndicator,
    side: OptionBacktestSide,
    stop_loss: f64,
) -> Option<(&'static str, f64)> {
    match side {
        OptionBacktestSide::Call => {
            if item.candle.low_price <= stop_loss && item.candle.close_price < stop_loss {
                Some(("SL1", item.candle.close_price))
            } else if item.candle.high_price >= item.upper {
                Some(("TARGET", item.candle.close_price))
            } else {
                None
            }
        }
        OptionBacktestSide::Put => {
            if item.candle.high_price >= stop_loss && item.candle.close_price > stop_loss {
                Some(("SL1", item.candle.close_price))
            } else if item.candle.low_price <= item.lower {
                Some(("TARGET", item.candle.close_price))
            } else {
                None
            }
        }
    }
}

fn underlying_at(index_candles: &[Candle], at: DateTime<Utc>) -> Option<f64> {
    let index = index_candles.partition_point(|candle| candle.candle_time <= at);
    index
        .checked_sub(1)
        .and_then(|position| index_candles.get(position))
        .map(|candle| candle.close_price)
}

fn option_chain_selection_is_better(
    candidate: &OptionChainSelection,
    existing: &OptionChainSelection,
) -> bool {
    (candidate.premium - OPTION_TARGET_PREMIUM)
        .abs()
        .total_cmp(&(existing.premium - OPTION_TARGET_PREMIUM).abs())
        .then_with(|| {
            (candidate.strike - candidate.underlying)
                .abs()
                .total_cmp(&(existing.strike - existing.underlying).abs())
        })
        .then_with(|| candidate.strike.total_cmp(&existing.strike))
        .is_lt()
}

fn option_chain_selections(
    indicator_sets: &[(OptionBacktestContract, Vec<OptionIndicator>)],
    index_candles: &[Candle],
) -> HashMap<(OptionBacktestSide, DateTime<Utc>), OptionChainSelection> {
    let mut selected = HashMap::new();
    for (contract, indicators) in indicator_sets {
        for item in indicators {
            if !option_market_minutes(item.candle.candle_time)
                || !(OPTION_MIN_PREMIUM..=OPTION_MAX_PREMIUM).contains(&item.candle.close_price)
            {
                continue;
            }
            let Some(underlying) = underlying_at(index_candles, item.candle.candle_time) else {
                continue;
            };
            let candidate = OptionChainSelection {
                token: contract.token.clone(),
                symbol: contract.symbol.clone(),
                premium: item.candle.close_price,
                strike: contract.strike,
                underlying,
            };
            let key = (contract.side, item.candle.candle_time);
            match selected.get(&key) {
                Some(existing) if !option_chain_selection_is_better(&candidate, existing) => {}
                _ => {
                    selected.insert(key, candidate);
                }
            }
        }
    }
    selected
}

async fn effective_margin_requirement(state: &AppState, user_id: Uuid) -> AppResult<f64> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(u.margin_requirement_percent,g.margin_requirement_percent,10.0)::float8 FROM risk_limits g LEFT JOIN risk_limits u ON u.user_id=$1 WHERE g.user_id IS NULL",
    )
    .bind(user_id)
    .fetch_one(&state.db)
    .await?)
}

fn candle_date(candle_time: DateTime<Utc>) -> NaiveDate {
    candle_time.with_timezone(&ist_offset()).date_naive()
}

fn entry_session(candle_time: DateTime<Utc>) -> Option<(NaiveDate, &'static str)> {
    let local = candle_time.with_timezone(&ist_offset());
    let minute = local.hour() * 60 + local.minute();
    let day_entry = 9 * 60 + 10;
    let evening_entry = 17 * 60 + 10;
    if minute >= evening_entry {
        Some((local.date_naive(), "evening"))
    } else if minute >= day_entry {
        Some((local.date_naive(), "day"))
    } else {
        None
    }
}

fn build_daily_levels(daily: &[Candle]) -> std::collections::HashMap<NaiveDate, Levels> {
    let mut levels = std::collections::HashMap::new();
    for index in 4..daily.len() {
        let date = candle_date(daily[index].candle_time);
        let previous = &daily[index - 4..index];
        let highs: Vec<f64> = previous.iter().map(|row| row.high_price).collect();
        let lows: Vec<f64> = previous.iter().map(|row| row.low_price).collect();
        if let Some(value) = calculate(&highs, &lows) {
            levels.insert(date, value);
        }
    }
    levels
}

fn target_exit_lots(lots: i32) -> i32 {
    if lots <= 1 {
        lots.max(0)
    } else {
        (lots + 1) / 2
    }
}

fn close_position(
    position: Position,
    candle: &Candle,
    exit_price: f64,
    reason: &str,
) -> TradeResult {
    let pnl_units = position.remaining_lots as f64 * position.pnl_multiplier_per_lot;
    let final_leg_pnl = trade_pnl(
        &position.direction,
        position.entry_price,
        exit_price,
        pnl_units,
    );
    let pnl = position.realized_pnl + final_leg_pnl;
    let quantity = position.lots.saturating_mul(position.lot_size);
    let mut audit_levels = levels_json(position.levels);
    if let Some(levels) = audit_levels.as_object_mut() {
        levels.insert("contract_lot_size".into(), json!(position.lot_size));
        levels.insert("configured_lots".into(), json!(position.lots));
        levels.insert("quantity".into(), json!(quantity));
        levels.insert(
            "partial_exit_lots".into(),
            json!(position.lots.saturating_sub(position.remaining_lots)),
        );
        levels.insert(
            "partial_exit_quantity".into(),
            json!(
                position
                    .lots
                    .saturating_sub(position.remaining_lots)
                    .saturating_mul(position.lot_size)
            ),
        );
        levels.insert("partial_realized_pnl".into(), json!(position.realized_pnl));
        levels.insert("final_leg_lots".into(), json!(position.remaining_lots));
        levels.insert(
            "final_leg_quantity".into(),
            json!(position.remaining_lots.saturating_mul(position.lot_size)),
        );
        levels.insert("final_leg_pnl".into(), json!(final_leg_pnl));
        levels.insert("calculated_pnl".into(), json!(pnl));
    }
    TradeResult {
        id: Uuid::new_v4(),
        trade_date: position.trade_date,
        direction: position.direction,
        entry_time: position.entry_time,
        entry_price: position.entry_price,
        exit_time: candle.candle_time,
        exit_price,
        lots: position.lots,
        quantity,
        margin_per_lot: position.margin_per_lot,
        margin_used: position.margin_used,
        realized_pnl: pnl,
        exit_reason: reason.into(),
        levels: audit_levels,
    }
}

fn refresh_position_levels(
    position: &mut Position,
    levels_by_date: &HashMap<NaiveDate, Levels>,
    date: NaiveDate,
) {
    if let Some(levels) = levels_by_date.get(&date).copied() {
        position.levels = levels;
    }
}

fn process_exit(
    position: &mut Option<Position>,
    candle: &Candle,
    levels_by_date: &HashMap<NaiveDate, Levels>,
) -> Option<TradeResult> {
    let mut current = position.take()?;
    refresh_position_levels(
        &mut current,
        levels_by_date,
        candle_date(candle.candle_time),
    );
    let (target, stop) = if current.direction == "BUY" {
        (
            candle.high_price >= current.levels.buy_target,
            candle.low_price
                <= if current.target_done {
                    current.levels.buy_sl2
                } else {
                    current.levels.buy_sl1
                },
        )
    } else {
        (
            candle.low_price <= current.levels.sell_target,
            candle.high_price
                >= if current.target_done {
                    current.levels.sell_sl2
                } else {
                    current.levels.sell_sl1
                },
        )
    };
    if stop {
        let price = if current.direction == "BUY" {
            if current.target_done {
                current.levels.buy_sl2
            } else {
                current.levels.buy_sl1
            }
        } else if current.target_done {
            current.levels.sell_sl2
        } else {
            current.levels.sell_sl1
        };
        let reason = if current.target_done { "SL2" } else { "SL1" };
        return Some(close_position(current, candle, price, reason));
    }
    if target && !current.target_done {
        let close_lots = target_exit_lots(current.lots).min(current.remaining_lots);
        let price = if current.direction == "BUY" {
            current.levels.buy_target
        } else {
            current.levels.sell_target
        };
        current.realized_pnl += trade_pnl(
            &current.direction,
            current.entry_price,
            price,
            close_lots as f64 * current.pnl_multiplier_per_lot,
        );
        current.remaining_lots -= close_lots;
        current.target_done = true;
        if current.remaining_lots <= 0 {
            return Some(close_position(current, candle, price, "TARGET"));
        }
    }
    *position = Some(current);
    None
}

fn simulate(
    intraday: &[Candle],
    daily: &[Candle],
    lot_size: i32,
    lots: i32,
    margin_requirement_percent: f64,
    buy_margin_per_lot: Option<f64>,
    sell_margin_per_lot: Option<f64>,
) -> (Vec<TradeResult>, Value) {
    let levels_by_date = build_daily_levels(daily);
    let pnl_multiplier = pnl_multiplier_per_lot(SUPPORTED_INSTRUMENT, lot_size);
    let mut position: Option<Position> = None;
    let mut trades = Vec::new();
    let mut equity: f64 = 0.0;
    let mut peak: f64 = 0.0;
    let mut max_drawdown: f64 = 0.0;
    let mut entered_sessions: HashSet<(NaiveDate, &'static str)> = HashSet::new();

    for candle in intraday {
        if let Some(trade) = process_exit(&mut position, candle, &levels_by_date) {
            equity += trade.realized_pnl;
            peak = f64::max(peak, equity);
            max_drawdown = f64::max(max_drawdown, peak - equity);
            trades.push(trade);
            continue;
        }
        if position.is_some() {
            continue;
        }
        let Some(session_key) = entry_session(candle.candle_time) else {
            continue;
        };
        if entered_sessions.contains(&session_key) {
            continue;
        }
        let date = candle_date(candle.candle_time);
        let Some(levels) = levels_by_date.get(&date).copied() else {
            continue;
        };
        let buy = candle.high_price >= levels.buy_entry;
        let sell = candle.low_price <= levels.sell_entry;
        if !buy && !sell {
            continue;
        }
        let direction = if buy && sell {
            if candle.close_price >= candle.open_price {
                "BUY"
            } else {
                "SELL"
            }
        } else if buy {
            "BUY"
        } else {
            "SELL"
        };
        let entry_price = if direction == "BUY" {
            levels.buy_entry
        } else {
            levels.sell_entry
        };
        let margin_per_lot = if direction == "BUY" {
            buy_margin_per_lot
        } else {
            sell_margin_per_lot
        }
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or_else(|| margin_per_lot(entry_price, lot_size, margin_requirement_percent));
        entered_sessions.insert(session_key);
        position = Some(Position {
            trade_date: date,
            direction: direction.into(),
            entry_time: candle.candle_time,
            entry_price,
            lots,
            lot_size,
            remaining_lots: lots,
            pnl_multiplier_per_lot: pnl_multiplier,
            margin_per_lot,
            margin_used: margin_per_lot * lots as f64,
            realized_pnl: 0.0,
            target_done: false,
            levels,
        });
    }

    if let (Some(open), Some(last)) = (position, intraday.last()) {
        let trade = close_position(open, last, last.close_price, "END_OF_TEST");
        equity += trade.realized_pnl;
        peak = f64::max(peak, equity);
        max_drawdown = f64::max(max_drawdown, peak - equity);
        trades.push(trade);
    }

    let wins = trades
        .iter()
        .filter(|trade| trade.realized_pnl > 0.0)
        .count();
    let losses = trades
        .iter()
        .filter(|trade| trade.realized_pnl < 0.0)
        .count();
    let gross_profit: f64 = trades
        .iter()
        .filter(|trade| trade.realized_pnl > 0.0)
        .map(|trade| trade.realized_pnl)
        .sum();
    let gross_loss: f64 = trades
        .iter()
        .filter(|trade| trade.realized_pnl < 0.0)
        .map(|trade| trade.realized_pnl)
        .sum();
    let average_pnl = if trades.is_empty() {
        0.0
    } else {
        equity / trades.len() as f64
    };
    let average_win = if wins == 0 {
        0.0
    } else {
        gross_profit / wins as f64
    };
    let average_loss = if losses == 0 {
        0.0
    } else {
        gross_loss / losses as f64
    };
    let initial_margin_per_lot = trades
        .first()
        .map(|trade| trade.margin_per_lot)
        .unwrap_or(0.0);
    let initial_margin = trades.first().map(|trade| trade.margin_used).unwrap_or(0.0);
    let max_margin_per_lot = trades
        .iter()
        .map(|trade| trade.margin_per_lot)
        .reduce(f64::max)
        .unwrap_or(0.0);
    let max_margin_used = trades
        .iter()
        .map(|trade| trade.margin_used)
        .reduce(f64::max)
        .unwrap_or(0.0);
    let summary = json!({
        "strategy_key": STRATEGY_KEY,
        "strategy_name": "Futures Breakout v3",
        "trades":trades.len(),
        "wins":wins,
        "losses":losses,
        "win_rate": if trades.is_empty() { 0.0 } else { wins as f64 * 100.0 / trades.len() as f64 },
        "net_pnl": equity,
        "gross_profit": gross_profit,
        "gross_loss": gross_loss,
        "average_pnl": average_pnl,
        "average_win": average_win,
        "average_loss": average_loss,
        "profit_factor": (gross_loss.abs() > 0.0).then_some(gross_profit / gross_loss.abs()),
        "max_drawdown": max_drawdown,
        "lot_size": lot_size,
        "target_exit_lots": target_exit_lots(lots),
        "pnl_multiplier_per_lot": pnl_multiplier,
        "pnl_model": "goldten_points_x_lots",
        "entry_frequency": "one_per_session",
        "margin_requirement_percent": margin_requirement_percent,
        "initial_margin_per_lot": initial_margin_per_lot,
        "initial_margin": initial_margin,
        "max_margin_per_lot": max_margin_per_lot,
        "max_margin_used": max_margin_used,
        "buy_trades": trades.iter().filter(|trade| trade.direction == "BUY").count(),
        "sell_trades": trades.iter().filter(|trade| trade.direction == "SELL").count(),
    });
    (trades, summary)
}

fn option_signal_step(
    setup: &mut OptionSetup,
    item: &OptionIndicator,
    side: OptionBacktestSide,
) -> Option<OptionBacktestSignal> {
    let candle = &item.candle;
    match side {
        OptionBacktestSide::Call => match setup.clone() {
            OptionSetup::Idle => {
                if candle.high_price > item.upper && candle.close_price > item.upper {
                    *setup = OptionSetup::AwaitRetrace;
                }
            }
            OptionSetup::AwaitRetrace => {
                if candle.low_price <= item.middle {
                    *setup = OptionSetup::AwaitConfirmation;
                }
            }
            OptionSetup::AwaitConfirmation => {
                if candle.close_price > candle.open_price
                    && candle.high_price > item.middle
                    && candle.close_price > item.middle
                {
                    *setup = OptionSetup::AwaitBreak {
                        high: candle.high_price,
                        low: candle.low_price,
                        at: candle.candle_time,
                    };
                }
            }
            OptionSetup::AwaitBreak { high, low, at } => {
                if candle.close_price > high && item.tsi > 0.0 {
                    *setup = OptionSetup::AwaitRetrace;
                    return Some(OptionBacktestSignal {
                        entry_price: candle.close_price,
                        stop_loss: low,
                        target_band: item.upper,
                        confirmation_at: at,
                        signal_at: candle.candle_time,
                    });
                } else if candle.low_price < item.middle {
                    *setup = OptionSetup::AwaitConfirmation;
                }
            }
        },
        OptionBacktestSide::Put => match setup.clone() {
            OptionSetup::Idle => {
                if candle.low_price < item.lower && candle.close_price < item.lower {
                    *setup = OptionSetup::AwaitRetrace;
                }
            }
            OptionSetup::AwaitRetrace => {
                if candle.high_price >= item.middle {
                    *setup = OptionSetup::AwaitConfirmation;
                }
            }
            OptionSetup::AwaitConfirmation => {
                if candle.close_price < candle.open_price
                    && candle.low_price < item.middle
                    && candle.close_price < item.middle
                {
                    *setup = OptionSetup::AwaitBreak {
                        high: candle.high_price,
                        low: candle.low_price,
                        at: candle.candle_time,
                    };
                }
            }
            OptionSetup::AwaitBreak { high, low, at } => {
                if candle.close_price < low && item.tsi < 0.0 {
                    *setup = OptionSetup::AwaitRetrace;
                    return Some(OptionBacktestSignal {
                        entry_price: candle.close_price,
                        stop_loss: high,
                        target_band: item.lower,
                        confirmation_at: at,
                        signal_at: candle.candle_time,
                    });
                } else if candle.high_price > item.middle {
                    *setup = OptionSetup::AwaitConfirmation;
                }
            }
        },
    }
    None
}

fn option_market_minutes(candle_time: DateTime<Utc>) -> bool {
    let local = candle_time.with_timezone(&ist_offset());
    let minute = local.hour() * 60 + local.minute();
    minute >= 9 * 60 + 20 && minute <= 15 * 60 + 30
}

fn close_option_backtest_trade(
    contract: &OptionBacktestContract,
    signal: OptionBacktestSignal,
    exit: &OptionIndicator,
    exit_price: f64,
    reason: &str,
    lots: i32,
    margin_requirement_percent: f64,
) -> TradeResult {
    let quantity = lots.saturating_mul(contract.lot_size);
    let direction = contract.side.direction();
    let gross_pnl = trade_pnl(direction, signal.entry_price, exit_price, quantity as f64);
    let margin_per_lot = margin_per_lot(
        signal.entry_price,
        contract.lot_size,
        margin_requirement_percent,
    );
    let levels = json!({
        "strategy_key": OPTION_ENTRY_STRATEGY_KEY,
        "option_type": contract.side.option_type(),
        "contract_symbol": contract.symbol,
        "symbol_token": contract.token,
        "exchange": contract.exchange,
        "expiry": contract.expiry,
        "strike": contract.strike,
        "entry_premium": signal.entry_price,
        "premium_min": OPTION_MIN_PREMIUM,
        "premium_max": OPTION_MAX_PREMIUM,
        "premium_distance": (signal.entry_price - OPTION_TARGET_PREMIUM).abs(),
        "confirmation_at": signal.confirmation_at,
        "signal_at": signal.signal_at,
        "stop_loss": signal.stop_loss,
        "target_band_at_entry": signal.target_band,
        "target_band_at_exit": if contract.side == OptionBacktestSide::Call { exit.upper } else { exit.lower },
        "contract_lot_size": contract.lot_size,
        "configured_lots": lots,
        "quantity": quantity,
        "gross_pnl": gross_pnl,
        "costs": 0.0,
        "calculated_pnl": gross_pnl,
        "data_basis": "historical_option_candles",
    });
    TradeResult {
        id: Uuid::new_v4(),
        trade_date: candle_date(signal.signal_at),
        direction: direction.into(),
        entry_time: signal.signal_at,
        entry_price: signal.entry_price,
        exit_time: exit.candle.candle_time,
        exit_price,
        lots,
        quantity,
        margin_per_lot,
        margin_used: margin_per_lot * lots as f64,
        realized_pnl: gross_pnl,
        exit_reason: reason.into(),
        levels,
    }
}

fn simulate_option_indicators(
    contract: &OptionBacktestContract,
    indicators: &[OptionIndicator],
    lots: i32,
    margin_requirement_percent: f64,
) -> Vec<TradeResult> {
    let mut setup = OptionSetup::Idle;
    let mut open_signal: Option<OptionBacktestSignal> = None;
    let mut trades = Vec::new();
    for item in indicators {
        if let Some(signal) = open_signal.clone() {
            if let Some((reason, exit_price)) =
                option_backtest_exit(item, contract.side, signal.stop_loss)
            {
                trades.push(close_option_backtest_trade(
                    contract,
                    signal,
                    item,
                    exit_price,
                    reason,
                    lots,
                    margin_requirement_percent,
                ));
                open_signal = None;
            }
            continue;
        }
        if !option_market_minutes(item.candle.candle_time) {
            continue;
        }
        let Some(signal) = option_signal_step(&mut setup, item, contract.side) else {
            continue;
        };
        if (OPTION_MIN_PREMIUM..=OPTION_MAX_PREMIUM).contains(&signal.entry_price) {
            open_signal = Some(signal);
        }
    }
    if let (Some(signal), Some(last)) = (open_signal, indicators.last()) {
        trades.push(close_option_backtest_trade(
            contract,
            signal,
            last,
            last.candle.close_price,
            "END_OF_TEST",
            lots,
            margin_requirement_percent,
        ));
    }
    trades
}

fn side_key(trade: &TradeResult) -> &str {
    trade
        .levels
        .get("option_type")
        .and_then(Value::as_str)
        .unwrap_or(&trade.direction)
}

fn option_side_from_trade(trade: &TradeResult) -> Option<OptionBacktestSide> {
    match trade.levels.get("option_type").and_then(Value::as_str) {
        Some("CE") => Some(OptionBacktestSide::Call),
        Some("PE") => Some(OptionBacktestSide::Put),
        _ => match trade.direction.as_str() {
            "BUY" => Some(OptionBacktestSide::Call),
            "SELL" => Some(OptionBacktestSide::Put),
            _ => None,
        },
    }
}

fn apply_option_chain_selection(
    mut trade: TradeResult,
    selections: &HashMap<(OptionBacktestSide, DateTime<Utc>), OptionChainSelection>,
) -> Option<TradeResult> {
    let side = option_side_from_trade(&trade)?;
    let token = trade.levels.get("symbol_token").and_then(Value::as_str)?;
    let selection = selections.get(&(side, trade.entry_time))?;
    if selection.token != token {
        return None;
    }
    if let Some(levels) = trade.levels.as_object_mut() {
        levels.insert("option_chain_selected_at_entry".into(), json!(true));
        levels.insert("selected_contract_symbol".into(), json!(selection.symbol));
        levels.insert("selected_contract_token".into(), json!(selection.token));
        levels.insert("selected_entry_premium".into(), json!(selection.premium));
        levels.insert("underlying_at_entry".into(), json!(selection.underlying));
        levels.insert(
            "selection_basis".into(),
            json!("premium_220_300_closest_to_260"),
        );
    }
    Some(trade)
}

fn option_backtest_summary(
    trades: &[TradeResult],
    lot_size: i32,
    lots: i32,
    contract_count: usize,
    interval_candles: usize,
    chain_selection_points: usize,
    potential_signals: usize,
    selected_signals: usize,
) -> Value {
    let equity: f64 = trades.iter().map(|trade| trade.realized_pnl).sum();
    let wins = trades
        .iter()
        .filter(|trade| trade.realized_pnl > 0.0)
        .count();
    let losses = trades
        .iter()
        .filter(|trade| trade.realized_pnl < 0.0)
        .count();
    let gross_profit: f64 = trades
        .iter()
        .filter(|trade| trade.realized_pnl > 0.0)
        .map(|trade| trade.realized_pnl)
        .sum();
    let gross_loss: f64 = trades
        .iter()
        .filter(|trade| trade.realized_pnl < 0.0)
        .map(|trade| trade.realized_pnl)
        .sum();
    let mut running = 0.0_f64;
    let mut peak = 0.0_f64;
    let mut max_drawdown = 0.0_f64;
    for trade in trades {
        running += trade.realized_pnl;
        peak = peak.max(running);
        max_drawdown = max_drawdown.max(peak - running);
    }
    json!({
        "strategy_key": OPTION_ENTRY_STRATEGY_KEY,
        "strategy_name": "Option Entry Strategy V1.0",
        "trades": trades.len(),
        "wins": wins,
        "losses": losses,
        "win_rate": if trades.is_empty() { 0.0 } else { wins as f64 * 100.0 / trades.len() as f64 },
        "net_pnl": equity,
        "gross_profit": gross_profit,
        "gross_loss": gross_loss,
        "average_pnl": if trades.is_empty() { 0.0 } else { equity / trades.len() as f64 },
        "average_win": if wins == 0 { 0.0 } else { gross_profit / wins as f64 },
        "average_loss": if losses == 0 { 0.0 } else { gross_loss / losses as f64 },
        "profit_factor": (gross_loss.abs() > 0.0).then_some(gross_profit / gross_loss.abs()),
        "max_drawdown": max_drawdown,
        "lot_size": lot_size,
        "pnl_multiplier_per_lot": lot_size,
        "configured_lots": lots,
        "entry_frequency": "one_open_trade_per_option_side",
        "data_basis": "historical_option_candles_for_current_master_contracts",
        "pnl_model": "premium_points_x_quantity",
        "selection_basis": "entry_time_option_chain_premium_220_300_closest_to_260",
        "parameters": {
            "interval": OPTION_INTERVAL,
            "keltner_ema_period": KELTNER_EMA_PERIOD,
            "keltner_atr_period": KELTNER_ATR_PERIOD,
            "keltner_multiplier": KELTNER_MULTIPLIER,
            "tsi_long_period": TSI_LONG_PERIOD,
            "tsi_short_period": TSI_SHORT_PERIOD,
            "premium_min": OPTION_MIN_PREMIUM,
            "premium_max": OPTION_MAX_PREMIUM,
            "max_contracts_per_side": OPTION_BACKTEST_MAX_CONTRACTS_PER_SIDE,
        },
        "contracts_scanned": contract_count,
        "chain_selection_points": chain_selection_points,
        "potential_signals": potential_signals,
        "selected_signals": selected_signals,
        "interval_candles": interval_candles,
        "buy_trades": trades.iter().filter(|trade| trade.direction == "BUY").count(),
        "sell_trades": trades.iter().filter(|trade| trade.direction == "SELL").count(),
    })
}

fn option_contract_start(from_time: DateTime<Utc>, expiry: NaiveDate) -> DateTime<Utc> {
    let listed_window = expiry - Duration::days(45);
    let local_start = ist_offset()
        .from_local_datetime(
            &listed_window
                .and_hms_opt(9, 15, 0)
                .expect("valid market open time"),
        )
        .single()
        .expect("IST has no ambiguous local times")
        .with_timezone(&Utc);
    from_time.max(local_start)
}

async fn run_option_backtest(
    state: AppState,
    user: AuthUser,
    input: BacktestRequest,
) -> AppResult<Json<Value>> {
    let credentials = state.credentials.load(user.id).await?;
    if credentials.api_key.is_empty() || credentials.jwt_token.is_empty() {
        return Err(AppError::BadRequest(
            "Connect Angel One before running a backtest so historical market data can be fetched."
                .into(),
        ));
    }
    let to_time = Utc::now();
    let from_time = to_time - Duration::days(i64::from(input.lookback_months) * 31);
    let margin_requirement_percent = effective_margin_requirement(&state, user.id).await?;
    let index_contract = ContractSelection {
        exchange: "BSE".into(),
        token: SENSEX_INDEX_TOKEN.into(),
        symbol: OPTION_SUPPORTED_INSTRUMENT.into(),
        lot_size: 1,
        buy_margin_per_lot: None,
        sell_margin_per_lot: None,
    };
    let index_stats = ensure_candles(
        &state,
        user.id,
        &credentials,
        &index_contract,
        OPTION_SUPPORTED_INSTRUMENT,
        OPTION_INTERVAL,
        from_time,
        to_time,
    )
    .await?;
    let index_candles = load_candles(
        &state,
        &index_contract.exchange,
        &index_contract.token,
        OPTION_INTERVAL,
        from_time,
        to_time,
    )
    .await?;
    if index_candles.is_empty() {
        return Err(AppError::BadRequest(
            "Not enough SENSEX index candles to choose option backtest candidates.".into(),
        ));
    }
    let min_underlying = index_candles
        .iter()
        .map(|candle| candle.low_price)
        .reduce(f64::min)
        .unwrap_or(0.0);
    let max_underlying = index_candles
        .iter()
        .map(|candle| candle.high_price)
        .reduce(f64::max)
        .unwrap_or(0.0);
    let reference_underlying = index_candles
        .last()
        .map(|candle| candle.close_price)
        .unwrap_or(max_underlying);
    let contracts = load_contract_master(&state).await?;
    let mut option_contracts = Vec::new();
    for side in [OptionBacktestSide::Call, OptionBacktestSide::Put] {
        option_contracts.extend(option_backtest_candidates(
            &contracts,
            side,
            Utc::now().date_naive(),
            min_underlying,
            max_underlying,
            reference_underlying,
        ));
    }
    if option_contracts.is_empty() {
        return Err(AppError::BadRequest(
            "No current SENSEX option contracts are available for backtesting.".into(),
        ));
    }

    let mut potential_trades = Vec::new();
    let mut option_data_points = 0_i64;
    let mut option_reused_points = 0_i64;
    let mut option_fetched_points = 0_i64;
    let mut interval_candles = index_candles.len();
    let mut representative_symbol = String::new();
    let mut representative_token = String::new();
    let mut representative_lot_size = 1;
    let mut indicator_sets: Vec<(OptionBacktestContract, Vec<OptionIndicator>)> = Vec::new();

    for contract in &option_contracts {
        let contract_selection = ContractSelection {
            exchange: contract.exchange.clone(),
            token: contract.token.clone(),
            symbol: contract.symbol.clone(),
            lot_size: contract.lot_size,
            buy_margin_per_lot: None,
            sell_margin_per_lot: None,
        };
        let contract_from = option_contract_start(from_time, contract.expiry);
        if contract_from > to_time {
            continue;
        }
        let stats = ensure_candles(
            &state,
            user.id,
            &credentials,
            &contract_selection,
            OPTION_SUPPORTED_INSTRUMENT,
            OPTION_INTERVAL,
            contract_from,
            to_time,
        )
        .await?;
        option_data_points += stats.data_points;
        option_reused_points += stats.reused_points;
        option_fetched_points += stats.fetched_points;
        let candles = load_candles(
            &state,
            &contract.exchange,
            &contract.token,
            OPTION_INTERVAL,
            contract_from,
            to_time,
        )
        .await?;
        if candles.is_empty() {
            continue;
        }
        let indicators = option_indicators(&candles);
        if indicators.is_empty() {
            continue;
        }
        if representative_symbol.is_empty() {
            representative_symbol = contract.symbol.clone();
            representative_token = contract.token.clone();
            representative_lot_size = contract.lot_size;
        }
        interval_candles += candles.len();
        potential_trades.extend(simulate_option_indicators(
            contract,
            &indicators,
            input.lots,
            margin_requirement_percent,
        ));
        indicator_sets.push((contract.clone(), indicators));
    }

    let chain_selections = option_chain_selections(&indicator_sets, &index_candles);
    let potential_signal_count = potential_trades.len();
    let mut selected_signal_count = 0_usize;
    potential_trades = potential_trades
        .into_iter()
        .filter_map(|trade| {
            let selected = apply_option_chain_selection(trade, &chain_selections);
            if selected.is_some() {
                selected_signal_count += 1;
            }
            selected
        })
        .collect();

    potential_trades.sort_by(|left, right| {
        left.entry_time.cmp(&right.entry_time).then_with(|| {
            let left_distance = left
                .levels
                .get("premium_distance")
                .and_then(Value::as_f64)
                .unwrap_or(f64::MAX);
            let right_distance = right
                .levels
                .get("premium_distance")
                .and_then(Value::as_f64)
                .unwrap_or(f64::MAX);
            left_distance.total_cmp(&right_distance)
        })
    });
    let mut side_available_at: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut trades = Vec::new();
    for trade in potential_trades {
        let side = side_key(&trade).to_string();
        if side_available_at
            .get(&side)
            .is_some_and(|available_at| trade.entry_time < *available_at)
        {
            continue;
        }
        side_available_at.insert(side, trade.exit_time);
        trades.push(trade);
    }
    trades.sort_by_key(|trade| trade.entry_time);

    let mut summary = option_backtest_summary(
        &trades,
        representative_lot_size,
        input.lots,
        option_contracts.len(),
        interval_candles,
        chain_selections.len(),
        potential_signal_count,
        selected_signal_count,
    );
    summary["index_candles"] = json!(index_candles.len());
    summary["option_candles"] = json!(interval_candles.saturating_sub(index_candles.len()));
    summary["api_limit_note"] = json!(
        "Backtest scans a capped nearest-expiry SENSEX option candidate set and reuses cached candles before calling Angel One."
    );
    if representative_symbol.is_empty() {
        representative_symbol = "SENSEX_OPTIONS".into();
        representative_token = SENSEX_INDEX_TOKEN.into();
    }

    let run_id = Uuid::new_v4();
    let data_points = index_stats.data_points + option_data_points;
    let reused_points = index_stats.reused_points + option_reused_points;
    let fetched_points = index_stats.fetched_points + option_fetched_points;
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO backtest_runs (id,user_id,strategy_key,instrument,trading_symbol,symbol_token,interval_key,lookback_months,from_time,to_time,lots,lot_size,status,summary,data_points,reused_points,fetched_points) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'completed',$13,$14,$15,$16)")
        .bind(run_id).bind(user.id).bind(OPTION_ENTRY_STRATEGY_KEY).bind(OPTION_SUPPORTED_INSTRUMENT).bind(&representative_symbol).bind(&representative_token).bind(OPTION_INTERVAL)
        .bind(input.lookback_months).bind(from_time).bind(to_time).bind(input.lots).bind(representative_lot_size)
        .bind(&summary).bind(data_points as i32).bind(reused_points as i32).bind(fetched_points as i32)
        .execute(&mut *tx).await?;
    for trade in &trades {
        sqlx::query("INSERT INTO backtest_trades (id,run_id,trade_date,direction,entry_time,entry_price,exit_time,exit_price,lots,quantity,realized_pnl,exit_reason,levels) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)")
            .bind(trade.id).bind(run_id).bind(trade.trade_date).bind(&trade.direction).bind(trade.entry_time).bind(trade.entry_price).bind(trade.exit_time).bind(trade.exit_price)
            .bind(trade.lots).bind(trade.quantity).bind(trade.realized_pnl).bind(&trade.exit_reason).bind(&trade.levels)
            .execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({
        "run":{
            "id":run_id,
            "strategy_key":OPTION_ENTRY_STRATEGY_KEY,
            "instrument":OPTION_SUPPORTED_INSTRUMENT,
            "trading_symbol":representative_symbol,
            "symbol_token":representative_token,
            "interval":OPTION_INTERVAL,
            "lookback_months":input.lookback_months,
            "from_time":from_time,
            "to_time":to_time,
            "lots":input.lots,
            "lot_size":representative_lot_size,
            "summary":summary,
            "data_points":data_points,
            "reused_points":reused_points,
            "fetched_points":fetched_points,
            "created_at":Utc::now()
        },
        "trades":trades
    })))
}

pub async fn run(
    State(state): State<AppState>,
    axum::extract::Extension(user): axum::extract::Extension<AuthUser>,
    Json(input): Json<BacktestRequest>,
) -> AppResult<Json<Value>> {
    require_backtest_permission(&user)?;
    require_non_trading_day(&state).await?;
    let strategy_key = input
        .strategy_key
        .as_deref()
        .unwrap_or(STRATEGY_KEY)
        .trim()
        .to_lowercase();
    let instrument = input
        .instrument
        .clone()
        .unwrap_or_else(|| SUPPORTED_INSTRUMENT.into())
        .trim()
        .to_uppercase();
    if !matches!(input.lookback_months, 1 | 3 | 6) {
        return Err(AppError::BadRequest(
            "Backtest lookback must be 1, 3, or 6 months.".into(),
        ));
    }
    if input.lots <= 0 {
        return Err(AppError::BadRequest(
            "Lots must be a positive integer.".into(),
        ));
    }
    let interval = normalize_interval(input.interval.clone())?;
    if strategy_key == OPTION_ENTRY_STRATEGY_KEY {
        if instrument != OPTION_SUPPORTED_INSTRUMENT {
            return Err(AppError::BadRequest(
                "Option Entry Strategy V1.0 backtesting supports only SENSEX.".into(),
            ));
        }
        if interval != OPTION_INTERVAL {
            return Err(AppError::BadRequest(
                "Option Entry Strategy V1.0 backtesting uses the FIVE_MINUTE interval.".into(),
            ));
        }
        return run_option_backtest(state, user, input).await;
    }
    let credentials = state.credentials.load(user.id).await?;
    if credentials.api_key.is_empty() || credentials.jwt_token.is_empty() {
        return Err(AppError::BadRequest(
            "Connect Angel One before running a backtest so historical market data can be fetched."
                .into(),
        ));
    }
    if strategy_key != STRATEGY_KEY {
        return Err(AppError::BadRequest(
            "Unknown backtesting strategy key.".into(),
        ));
    }
    if instrument != SUPPORTED_INSTRUMENT {
        return Err(AppError::BadRequest(
            "Futures Breakout v3 backtesting supports only GOLDTEN.".into(),
        ));
    }
    let contract = current_contract(&state, &instrument).await?;
    let buy_margin = crate::margin::estimate(
        &state,
        user.id,
        &credentials.api_key,
        &credentials.jwt_token,
        &contract.exchange,
        "CARRYFORWARD",
        &contract.token,
        &contract.symbol,
        "STOPLOSS_LIMIT",
        "BUY",
        contract.lot_size,
        input.lots,
    )
    .await?;
    let sell_margin = crate::margin::estimate(
        &state,
        user.id,
        &credentials.api_key,
        &credentials.jwt_token,
        &contract.exchange,
        "CARRYFORWARD",
        &contract.token,
        &contract.symbol,
        "STOPLOSS_LIMIT",
        "SELL",
        contract.lot_size,
        input.lots,
    )
    .await?;
    let contract = ContractSelection {
        buy_margin_per_lot: Some(buy_margin.margin_per_lot),
        sell_margin_per_lot: Some(sell_margin.margin_per_lot),
        ..contract
    };
    let to_time = Utc::now();
    let from_time = to_time - Duration::days(i64::from(input.lookback_months) * 31);
    let warmup_from = from_time - Duration::days(20);
    let daily_stats = ensure_candles(
        &state,
        user.id,
        &credentials,
        &contract,
        &instrument,
        "ONE_DAY",
        warmup_from,
        to_time,
    )
    .await?;
    let interval_stats = ensure_candles(
        &state,
        user.id,
        &credentials,
        &contract,
        &instrument,
        &interval,
        from_time,
        to_time,
    )
    .await?;
    let daily = load_candles(
        &state,
        &contract.exchange,
        &contract.token,
        "ONE_DAY",
        warmup_from,
        to_time,
    )
    .await?;
    let intraday = load_candles(
        &state,
        &contract.exchange,
        &contract.token,
        &interval,
        from_time,
        to_time,
    )
    .await?;
    if daily.len() < 5 || intraday.is_empty() {
        return Err(AppError::BadRequest(
            "Not enough cached or broker-returned candles to run this backtest.".into(),
        ));
    }
    let margin_requirement_percent = effective_margin_requirement(&state, user.id).await?;
    let (trades, mut summary) = simulate(
        &intraday,
        &daily,
        contract.lot_size,
        input.lots,
        margin_requirement_percent,
        contract.buy_margin_per_lot,
        contract.sell_margin_per_lot,
    );
    summary["daily_candles"] = json!(daily.len());
    summary["interval_candles"] = json!(intraday.len());
    summary["buy_margin_per_lot"] = json!(buy_margin.margin_per_lot);
    summary["sell_margin_per_lot"] = json!(sell_margin.margin_per_lot);
    summary["calculator_margin_per_lot"] =
        json!(buy_margin.margin_per_lot.max(sell_margin.margin_per_lot));
    if summary
        .get("initial_margin_per_lot")
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        <= 0.0
    {
        summary["initial_margin_per_lot"] =
            json!(buy_margin.margin_per_lot.max(sell_margin.margin_per_lot));
        summary["initial_margin"] =
            json!(buy_margin.margin_per_lot.max(sell_margin.margin_per_lot) * input.lots as f64);
    }
    let run_id = Uuid::new_v4();
    let data_points = daily_stats.data_points + interval_stats.data_points;
    let reused_points = daily_stats.reused_points + interval_stats.reused_points;
    let fetched_points = daily_stats.fetched_points + interval_stats.fetched_points;
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO backtest_runs (id,user_id,strategy_key,instrument,trading_symbol,symbol_token,interval_key,lookback_months,from_time,to_time,lots,lot_size,status,summary,data_points,reused_points,fetched_points) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'completed',$13,$14,$15,$16)")
        .bind(run_id).bind(user.id).bind(STRATEGY_KEY).bind(&instrument).bind(&contract.symbol).bind(&contract.token).bind(&interval)
        .bind(input.lookback_months).bind(from_time).bind(to_time).bind(input.lots).bind(contract.lot_size)
        .bind(&summary).bind(data_points as i32).bind(reused_points as i32).bind(fetched_points as i32)
        .execute(&mut *tx).await?;
    for trade in &trades {
        sqlx::query("INSERT INTO backtest_trades (id,run_id,trade_date,direction,entry_time,entry_price,exit_time,exit_price,lots,quantity,realized_pnl,exit_reason,levels) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)")
            .bind(trade.id).bind(run_id).bind(trade.trade_date).bind(&trade.direction).bind(trade.entry_time).bind(trade.entry_price).bind(trade.exit_time).bind(trade.exit_price)
            .bind(trade.lots).bind(trade.quantity).bind(trade.realized_pnl).bind(&trade.exit_reason).bind(&trade.levels)
            .execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({
        "run":{
            "id":run_id,
            "strategy_key":STRATEGY_KEY,
            "instrument":instrument,
            "trading_symbol":contract.symbol,
            "symbol_token":contract.token,
            "interval":interval,
            "lookback_months":input.lookback_months,
            "from_time":from_time,
            "to_time":to_time,
            "lots":input.lots,
            "lot_size":contract.lot_size,
            "summary":summary,
            "data_points":data_points,
            "reused_points":reused_points,
            "fetched_points":fetched_points,
            "created_at":Utc::now()
        },
        "trades":trades
    })))
}

pub async fn history(
    State(state): State<AppState>,
    axum::extract::Extension(user): axum::extract::Extension<AuthUser>,
) -> AppResult<Json<Value>> {
    require_backtest_permission(&user)?;
    let availability = backtesting_availability(&state).await?;
    let runs: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'strategy_key',strategy_key,'instrument',instrument,'trading_symbol',trading_symbol,'interval',interval_key,'lookback_months',lookback_months,'from_time',from_time,'to_time',to_time,'lots',lots,'lot_size',lot_size,'status',status,'summary',summary,'error',error,'data_points',data_points,'reused_points',reused_points,'fetched_points',fetched_points,'created_at',created_at) FROM backtest_runs WHERE user_id=$1 ORDER BY created_at DESC LIMIT 20")
        .bind(user.id).fetch_all(&state.db).await?;
    Ok(Json(json!({"runs":runs,"availability":availability})))
}

pub async fn export(
    State(state): State<AppState>,
    axum::extract::Extension(user): axum::extract::Extension<AuthUser>,
    Path(run_id): Path<Uuid>,
) -> AppResult<Response<Body>> {
    require_backtest_permission(&user)?;
    let run: ExportRun = sqlx::query_as(
        "SELECT id,strategy_key,instrument,trading_symbol,interval_key,from_time,to_time,lots,lot_size,summary,created_at FROM backtest_runs WHERE id=$1 AND user_id=$2",
    )
    .bind(run_id)
    .bind(user.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Backtest run not found.".into()))?;
    let trades: Vec<ExportTrade> = sqlx::query_as(
        "SELECT trade_date,direction,entry_time,entry_price,exit_time,exit_price,lots,quantity,realized_pnl,exit_reason,levels FROM backtest_trades WHERE run_id=$1 ORDER BY entry_time,id",
    )
    .bind(run.id)
    .fetch_all(&state.db)
    .await?;

    let mut workbook = Workbook::new();
    let summary = workbook.add_worksheet();
    summary
        .set_name("Summary")
        .map_err(|error| AppError::Internal(error.into()))?;
    let summary_rows = vec![
        ("Run ID", run.id.to_string()),
        ("Strategy", run.strategy_key.clone()),
        (
            "Strategy name",
            run.summary
                .get("strategy_name")
                .and_then(Value::as_str)
                .unwrap_or(&run.strategy_key)
                .to_string(),
        ),
        ("Instrument", run.instrument.clone()),
        ("Trading symbol", run.trading_symbol.clone()),
        ("Interval", run.interval_key.clone()),
        ("From", run.from_time.to_rfc3339()),
        ("To", run.to_time.to_rfc3339()),
        ("Configured lots", run.lots.to_string()),
        ("Contract lot size", run.lot_size.to_string()),
        (
            "Contract quantity",
            run.lots.saturating_mul(run.lot_size).to_string(),
        ),
        ("Trades", trades.len().to_string()),
        (
            "Net P&L",
            run.summary
                .get("net_pnl")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .to_string(),
        ),
        (
            "Data basis",
            run.summary
                .get("data_basis")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ),
        (
            "P&L model",
            run.summary
                .get("pnl_model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ),
        (
            "Parameters",
            run.summary
                .get("parameters")
                .map(Value::to_string)
                .unwrap_or_default(),
        ),
        ("Created", run.created_at.to_rfc3339()),
    ];
    for (row, (label, value)) in summary_rows.iter().enumerate() {
        summary
            .write_string(row as u32, 0, *label)
            .map_err(|error| AppError::Internal(error.into()))?;
        summary
            .write_string(row as u32, 1, value)
            .map_err(|error| AppError::Internal(error.into()))?;
    }
    summary
        .set_column_width(0, 24)
        .map_err(|error| AppError::Internal(error.into()))?;
    summary
        .set_column_width(1, 44)
        .map_err(|error| AppError::Internal(error.into()))?;

    let sheet = workbook.add_worksheet();
    sheet
        .set_name("Trades")
        .map_err(|error| AppError::Internal(error.into()))?;
    let headings = [
        "#",
        "Trade date",
        "Side",
        "Entry time",
        "Entry price",
        "Exit time",
        "Exit price",
        "Price movement",
        "Configured lots",
        "Contract lot size",
        "Contract quantity",
        "Partial exit lots",
        "Partial exit quantity",
        "Partial P&L",
        "Final leg lots",
        "Final leg quantity",
        "Final leg P&L",
        "Gross P&L",
        "Costs",
        "Realized P&L",
        "Exit reason",
        "Calculation check",
    ];
    for (column, heading) in headings.iter().enumerate() {
        sheet
            .write_string(0, column as u16, *heading)
            .map_err(|error| AppError::Internal(error.into()))?;
    }
    for (index, trade) in trades.iter().enumerate() {
        let row = (index + 1) as u32;
        let movement = if trade.direction == "BUY" {
            trade.exit_price - trade.entry_price
        } else {
            trade.entry_price - trade.exit_price
        };
        let partial_pnl = trade
            .levels
            .get("partial_realized_pnl")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let partial_exit_lots = trade
            .levels
            .get("partial_exit_lots")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let partial_exit_quantity = trade
            .levels
            .get("partial_exit_quantity")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let final_leg_lots = trade
            .levels
            .get("final_leg_lots")
            .and_then(Value::as_i64)
            .unwrap_or(trade.lots as i64);
        let final_leg_quantity = trade
            .levels
            .get("final_leg_quantity")
            .and_then(Value::as_i64)
            .unwrap_or(trade.quantity as i64);
        let final_leg_pnl = trade
            .levels
            .get("final_leg_pnl")
            .and_then(Value::as_f64)
            .unwrap_or(movement * trade.quantity as f64);
        let gross_pnl = trade
            .levels
            .get("gross_pnl")
            .and_then(Value::as_f64)
            .unwrap_or(trade.realized_pnl);
        let costs = trade
            .levels
            .get("costs")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let calculation = if run.strategy_key == STRATEGY_KEY {
            format!(
                "{partial_pnl:.4} + {final_leg_pnl:.4} = {:.4}",
                trade.realized_pnl
            )
        } else {
            format!("{gross_pnl:.4} - {costs:.4} = {:.4}", trade.realized_pnl)
        };
        sheet
            .write_number(row, 0, (index + 1) as f64)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_string(row, 1, trade.trade_date.to_string())
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_string(row, 2, &trade.direction)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_string(row, 3, trade.entry_time.to_rfc3339())
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 4, trade.entry_price)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_string(row, 5, trade.exit_time.to_rfc3339())
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 6, trade.exit_price)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 7, movement)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 8, trade.lots as f64)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 9, run.lot_size as f64)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 10, trade.quantity as f64)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 11, partial_exit_lots as f64)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 12, partial_exit_quantity as f64)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 13, partial_pnl)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 14, final_leg_lots as f64)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 15, final_leg_quantity as f64)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 16, final_leg_pnl)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 17, gross_pnl)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 18, costs)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_number(row, 19, trade.realized_pnl)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_string(row, 20, &trade.exit_reason)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_string(row, 21, calculation)
            .map_err(|error| AppError::Internal(error.into()))?;
    }
    for (column, width) in [(3, 24), (5, 24), (20, 18), (21, 34)] {
        sheet
            .set_column_width(column, width)
            .map_err(|error| AppError::Internal(error.into()))?;
    }

    let bytes = workbook
        .save_to_buffer()
        .map_err(|error| AppError::Internal(error.into()))?;
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        )
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=Rulenix-Backtest-{}.xlsx", run.id),
        )
        .body(Body::from(bytes))
        .map_err(|error| AppError::Internal(error.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backtesting_is_reserved_for_non_trading_dates() {
        let weekday = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let weekend = NaiveDate::from_ymd_opt(2026, 7, 18).unwrap();
        assert!(!backtesting_allowed_on_date(weekday, None));
        assert!(backtesting_allowed_on_date(weekend, None));
        assert!(backtesting_allowed_on_date(weekday, Some((false, false))));
        assert!(!backtesting_allowed_on_date(weekday, Some((false, true))));
        assert!(!backtesting_allowed_on_date(weekend, Some((true, false))));
    }

    fn candle(day: u32, open: f64, high: f64, low: f64, close: f64) -> Candle {
        Candle {
            candle_time: Utc
                .with_ymd_and_hms(2026, 1, day, 9, 15, 0)
                .single()
                .unwrap(),
            open_price: open,
            high_price: high,
            low_price: low,
            close_price: close,
            volume: 100.0,
        }
    }

    #[test]
    fn backtest_formulas_match_live_strategy() {
        let v = calculate(&[100.0, 110.0, 105.0, 108.0], &[90.0, 92.0, 94.0, 93.0]).unwrap();
        assert_eq!(v.hh4, 110.0);
        assert_eq!(v.ll2, 93.0);
        assert!((v.buy_entry - 110.132).abs() < 1e-9);
        assert!((v.sell_entry - 89.892).abs() < 1e-9);
    }

    #[test]
    fn gold_target_lot_split_matches_live_strategy() {
        for (lots, closed) in [(1, 1), (2, 1), (3, 2), (4, 2), (5, 3), (6, 3)] {
            assert_eq!(target_exit_lots(lots), closed);
        }
    }

    fn sensex_option(token: &str, expiry: &str, strike: i32, option_type: &str) -> MasterContract {
        MasterContract {
            token: token.into(),
            symbol: format!("SENSEX26JUL{strike}{option_type}"),
            name: "SENSEX".into(),
            expiry: expiry.into(),
            strike: format!("{:.6}", strike as f64 * 100.0),
            lotsize: "20".into(),
            instrumenttype: "OPTIDX".into(),
            exch_seg: "BFO".into(),
        }
    }

    fn option_contract(
        token: &str,
        side: OptionBacktestSide,
        strike: f64,
    ) -> OptionBacktestContract {
        OptionBacktestContract {
            side,
            exchange: "BFO".into(),
            token: token.into(),
            symbol: format!("SENSEX26JUL{}{}", strike as i32, side.option_type()),
            expiry: NaiveDate::from_ymd_opt(2026, 8, 2).unwrap(),
            strike,
            lot_size: 20,
        }
    }

    fn option_indicator(at: DateTime<Utc>, premium: f64) -> OptionIndicator {
        OptionIndicator {
            candle: Candle {
                candle_time: at,
                open_price: premium,
                high_price: premium + 5.0,
                low_price: premium - 5.0,
                close_price: premium,
                volume: 100.0,
            },
            middle: premium,
            upper: premium + 10.0,
            lower: premium - 10.0,
            tsi: 1.0,
        }
    }

    #[test]
    fn option_backtest_candidates_use_nearest_expiry_and_api_cap() {
        let mut contracts = vec![sensex_option("old", "26JUL2026", 76000, "CE")];
        for index in 0..40 {
            contracts.push(sensex_option(
                &format!("near{index}"),
                "02AUG2026",
                75000 + index * 100,
                "CE",
            ));
        }

        let selected = option_backtest_candidates(
            &contracts,
            OptionBacktestSide::Call,
            NaiveDate::from_ymd_opt(2026, 7, 27).unwrap(),
            73000.0,
            79000.0,
            77000.0,
        );

        assert_eq!(selected.len(), OPTION_BACKTEST_MAX_CONTRACTS_PER_SIDE);
        assert!(selected.iter().all(|contract| {
            contract.expiry == NaiveDate::from_ymd_opt(2026, 8, 2).unwrap()
                && contract.side == OptionBacktestSide::Call
        }));
    }

    #[test]
    fn option_chain_selection_picks_entry_premium_closest_to_target() {
        let at = Utc.with_ymd_and_hms(2026, 1, 5, 5, 0, 0).single().unwrap();
        let index_candles = vec![Candle {
            candle_time: at,
            open_price: 76_000.0,
            high_price: 76_100.0,
            low_price: 75_900.0,
            close_price: 76_025.0,
            volume: 100.0,
        }];
        let sets = vec![
            (
                option_contract("wide", OptionBacktestSide::Call, 76_000.0),
                vec![option_indicator(at, 225.0)],
            ),
            (
                option_contract("best", OptionBacktestSide::Call, 76_100.0),
                vec![option_indicator(at, 258.0)],
            ),
            (
                option_contract("outside", OptionBacktestSide::Call, 76_200.0),
                vec![option_indicator(at, 301.0)],
            ),
        ];

        let selections = option_chain_selections(&sets, &index_candles);
        let selected = selections.get(&(OptionBacktestSide::Call, at)).unwrap();

        assert_eq!(selected.token, "best");
        assert_eq!(selected.premium, 258.0);
    }

    #[test]
    fn option_chain_selection_rejects_non_selected_signal() {
        let at = Utc.with_ymd_and_hms(2026, 1, 5, 5, 0, 0).single().unwrap();
        let mut selections = HashMap::new();
        selections.insert(
            (OptionBacktestSide::Call, at),
            OptionChainSelection {
                token: "selected".into(),
                symbol: "SENSEX26JUL76100CE".into(),
                premium: 258.0,
                strike: 76_100.0,
                underlying: 76_025.0,
            },
        );
        let trade = |token: &str| TradeResult {
            id: Uuid::new_v4(),
            trade_date: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            direction: "BUY".into(),
            entry_time: at,
            entry_price: 258.0,
            exit_time: at + Duration::minutes(5),
            exit_price: 270.0,
            lots: 1,
            quantity: 20,
            margin_per_lot: 516.0,
            margin_used: 516.0,
            realized_pnl: 240.0,
            exit_reason: "TARGET".into(),
            levels: json!({"option_type":"CE","symbol_token":token}),
        };

        assert!(apply_option_chain_selection(trade("other"), &selections).is_none());
        let accepted = apply_option_chain_selection(trade("selected"), &selections).unwrap();
        assert_eq!(accepted.levels["selected_entry_premium"], 258.0);
        assert_eq!(
            accepted.levels["selection_basis"],
            "premium_220_300_closest_to_260"
        );
    }

    #[test]
    fn simulator_records_target_then_stop_carry() {
        let daily = vec![
            candle(1, 95.0, 100.0, 90.0, 96.0),
            candle(2, 96.0, 101.0, 91.0, 97.0),
            candle(3, 97.0, 102.0, 92.0, 98.0),
            candle(4, 98.0, 103.0, 93.0, 99.0),
            candle(5, 99.0, 104.0, 94.0, 100.0),
        ];
        let levels = calculate(&[100.0, 101.0, 102.0, 103.0], &[90.0, 91.0, 92.0, 93.0]).unwrap();
        let intraday = vec![
            candle(5, 100.0, levels.buy_entry + 0.1, 99.0, levels.buy_entry),
            candle(
                5,
                levels.buy_entry,
                levels.buy_target + 0.1,
                levels.buy_sl2 + 1.0,
                levels.buy_target,
            ),
            candle(
                5,
                levels.buy_target,
                levels.buy_target,
                levels.buy_sl2 - 0.1,
                levels.buy_sl2,
            ),
        ];
        let (trades, summary) = simulate(&intraday, &daily, 1, 3, 10.0, Some(12.0), Some(12.0));
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].exit_reason, "SL2");
        assert_eq!(summary["trades"], 1);
        assert_eq!(summary["initial_margin_per_lot"], 12.0);
    }

    #[test]
    fn simulator_closes_partial_target_and_refreshes_next_day_stop() {
        let daily = vec![
            candle(1, 95.0, 100.0, 90.0, 96.0),
            candle(2, 96.0, 101.0, 91.0, 97.0),
            candle(3, 97.0, 102.0, 92.0, 98.0),
            candle(4, 98.0, 103.0, 93.0, 99.0),
            candle(5, 99.0, 120.0, 94.0, 100.0),
            candle(6, 100.0, 121.0, 95.0, 101.0),
        ];
        let entry_day =
            calculate(&[100.0, 101.0, 102.0, 103.0], &[90.0, 91.0, 92.0, 93.0]).unwrap();
        let next_day = calculate(&[101.0, 102.0, 103.0, 120.0], &[91.0, 92.0, 93.0, 94.0]).unwrap();
        assert!(next_day.buy_sl2 > entry_day.buy_sl2);
        let intraday = vec![
            candle(
                5,
                entry_day.buy_entry,
                entry_day.buy_entry + 0.1,
                entry_day.buy_entry - 1.0,
                entry_day.buy_entry,
            ),
            candle(
                5,
                entry_day.buy_entry,
                entry_day.buy_target + 0.1,
                entry_day.buy_entry,
                entry_day.buy_target,
            ),
            candle(
                6,
                next_day.buy_sl2 + 0.5,
                next_day.buy_sl2 + 0.5,
                next_day.buy_sl2 - 0.1,
                next_day.buy_sl2,
            ),
        ];

        let (trades, summary) = simulate(&intraday, &daily, 10, 2, 10.0, Some(12.0), Some(12.0));

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].exit_reason, "SL2");
        assert_eq!(trades[0].levels["partial_exit_lots"], 1);
        assert_eq!(trades[0].levels["final_leg_lots"], 1);
        assert_eq!(trades[0].levels["partial_exit_quantity"], 10);
        assert_eq!(trades[0].levels["final_leg_quantity"], 10);
        assert!((trades[0].exit_price - next_day.buy_sl2).abs() < 1e-9);
        let expected =
            (entry_day.buy_target - entry_day.buy_entry) + (next_day.buy_sl2 - entry_day.buy_entry);
        assert!((trades[0].realized_pnl - expected).abs() < 1e-9);
        assert_eq!(summary["target_exit_lots"], 1);
    }

    #[test]
    fn simulator_uses_gold_lots_for_pnl_not_contract_quantity() {
        let daily = vec![
            candle(1, 95.0, 100.0, 90.0, 96.0),
            candle(2, 96.0, 101.0, 91.0, 97.0),
            candle(3, 97.0, 102.0, 92.0, 98.0),
            candle(4, 98.0, 103.0, 93.0, 99.0),
            candle(5, 99.0, 104.0, 94.0, 100.0),
        ];
        let levels = calculate(&[100.0, 101.0, 102.0, 103.0], &[90.0, 91.0, 92.0, 93.0]).unwrap();
        let intraday = vec![
            candle(5, 100.0, levels.buy_entry + 0.1, 99.0, levels.buy_entry),
            candle(
                5,
                levels.buy_entry,
                levels.buy_target + 0.1,
                levels.buy_entry,
                levels.buy_target,
            ),
        ];
        let (trades, summary) = simulate(
            &intraday,
            &daily,
            10,
            1,
            10.0,
            Some(13_218.0),
            Some(13_218.0),
        );
        let expected = levels.buy_target - levels.buy_entry;
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].exit_reason, "TARGET");
        assert_eq!(trades[0].quantity, 10);
        assert!((trades[0].realized_pnl - expected).abs() < 1e-9);
        assert_eq!(summary["pnl_multiplier_per_lot"], 1.0);
        assert_eq!(summary["pnl_model"], "goldten_points_x_lots");
        assert_eq!(trades[0].levels["contract_lot_size"], 10);
        assert_eq!(trades[0].levels["partial_exit_quantity"], 10);
        assert_eq!(trades[0].levels["final_leg_quantity"], 0);
    }

    #[test]
    fn simulator_limits_entries_to_one_per_session() {
        let daily = vec![
            candle(1, 95.0, 100.0, 90.0, 96.0),
            candle(2, 96.0, 101.0, 91.0, 97.0),
            candle(3, 97.0, 102.0, 92.0, 98.0),
            candle(4, 98.0, 103.0, 93.0, 99.0),
            candle(5, 99.0, 104.0, 94.0, 100.0),
        ];
        let levels = calculate(&[100.0, 101.0, 102.0, 103.0], &[90.0, 91.0, 92.0, 93.0]).unwrap();
        let intraday = vec![
            Candle {
                candle_time: Utc.with_ymd_and_hms(2026, 1, 5, 3, 45, 0).single().unwrap(),
                open_price: 100.0,
                high_price: levels.buy_entry + 0.1,
                low_price: 99.0,
                close_price: levels.buy_entry,
                volume: 100.0,
            },
            Candle {
                candle_time: Utc.with_ymd_and_hms(2026, 1, 5, 3, 50, 0).single().unwrap(),
                open_price: levels.buy_entry,
                high_price: levels.buy_target + 0.1,
                low_price: levels.buy_entry,
                close_price: levels.buy_target,
                volume: 100.0,
            },
            Candle {
                candle_time: Utc.with_ymd_and_hms(2026, 1, 5, 3, 55, 0).single().unwrap(),
                open_price: levels.buy_entry,
                high_price: levels.buy_entry + 0.1,
                low_price: 99.0,
                close_price: levels.buy_entry,
                volume: 100.0,
            },
        ];
        let (trades, summary) = simulate(
            &intraday,
            &daily,
            10,
            1,
            10.0,
            Some(13_218.0),
            Some(13_218.0),
        );
        assert_eq!(trades.len(), 1);
        assert_eq!(summary["entry_frequency"], "one_per_session");
    }
}
