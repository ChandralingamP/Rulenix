use crate::{
    angel,
    auth::AuthUser,
    contract_master::{self, MasterContract},
    error::{AppError, AppResult},
    instruments::{
        FUTURES_BREAKOUT_INSTRUMENTS, futures_pnl_multiplier_per_lot,
        is_futures_breakout_instrument,
    },
    state::AppState,
    strategy::{
        FuturesGapDirection, OPTION_ENTRY_STRATEGY_KEY, STRATEGY_KEY,
        SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY, futures_exit_levels_for_entry,
        futures_gap_direction, futures_gap_entry_was_jumped, futures_opening_range_entry,
    },
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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use uuid::Uuid;

const TRADING_DAY_BLOCK_MESSAGE: &str = "Backtesting is disabled for the entire Indian trading day to reserve Angel One API capacity for live market data and order execution. Try again on a weekend or full market holiday.";
const TRADING_DAY_OVERRIDE_MESSAGE: &str =
    "Trading-day backtesting is enabled by an administrator for this account.";

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
    #[allow(dead_code)]
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

#[derive(Debug, Clone, Serialize)]
struct ExitEvent {
    event: String,
    at: DateTime<Utc>,
    price: f64,
    lots: i32,
    quantity: i32,
    realized_pnl: f64,
    remaining_lots: i32,
    remaining_quantity: i32,
    position_closed: bool,
}

#[derive(Debug)]
struct Position {
    id: Uuid,
    trade_date: NaiveDate,
    direction: String,
    entry_time: DateTime<Utc>,
    entry_price: f64,
    entry_reason: String,
    reversal_of_trade_id: Option<Uuid>,
    lots: i32,
    lot_size: i32,
    remaining_lots: i32,
    pnl_multiplier_per_lot: f64,
    margin_per_lot: f64,
    margin_used: f64,
    realized_pnl: f64,
    target_done: bool,
    levels: Levels,
    entry_audit: Option<EntryAudit>,
    exit_events: Vec<ExitEvent>,
}

#[derive(Debug, Clone, Copy)]
struct OpeningRange {
    market_open: f64,
    high: f64,
    low: f64,
}

#[derive(Debug, Clone)]
struct EntryAudit {
    gap_direction: &'static str,
    entry_direction: String,
    entry_source: &'static str,
    previous_close: f64,
    market_open: f64,
    opening_range_high: f64,
    opening_range_low: f64,
    original_entry: f64,
    effective_entry: f64,
}

#[derive(Debug, Clone)]
struct EntryPlan {
    gap: FuturesGapDirection,
    direction: &'static str,
    source: &'static str,
    previous_close: f64,
    opening: OpeningRange,
    levels: Levels,
    available_minute: u32,
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

fn backtesting_allowed_on_date(
    date: NaiveDate,
    calendar_sessions: Option<(bool, bool)>,
    allow_trading_day: bool,
) -> bool {
    let market_open = calendar_sessions
        .map(|(morning_open, evening_open)| morning_open || evening_open)
        .unwrap_or_else(|| !matches!(date.weekday(), Weekday::Sat | Weekday::Sun));
    !market_open || allow_trading_day
}

async fn backtesting_availability(state: &AppState, user: &AuthUser) -> AppResult<Value> {
    let trade_date = Utc::now().with_timezone(&ist_offset()).date_naive();
    let calendar: Option<(bool, bool, String)> = sqlx::query_as(
        "SELECT morning_open,evening_open,reason FROM market_calendar WHERE trade_date=$1",
    )
    .bind(trade_date)
    .fetch_optional(&state.db)
    .await?;
    let normally_allowed = backtesting_allowed_on_date(
        trade_date,
        calendar
            .as_ref()
            .map(|(morning, evening, _)| (*morning, *evening)),
        false,
    );
    let override_active = !normally_allowed && user.can_backtest_on_trading_days;
    let allowed = normally_allowed || override_active;
    let calendar_reason = calendar
        .as_ref()
        .map(|(_, _, reason)| reason.as_str())
        .filter(|reason| !reason.is_empty());
    Ok(json!({
        "allowed": allowed,
        "trading_day_override": override_active,
        "trade_date": trade_date,
        "reason": if override_active {
            TRADING_DAY_OVERRIDE_MESSAGE
        } else if allowed {
            calendar_reason.unwrap_or("Non-trading day")
        } else {
            TRADING_DAY_BLOCK_MESSAGE
        }
    }))
}

async fn require_backtesting_available(state: &AppState, user: &AuthUser) -> AppResult<()> {
    let availability = backtesting_availability(state, user).await?;
    if availability["allowed"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(AppError::BadRequest(TRADING_DAY_BLOCK_MESSAGE.into()))
    }
}

fn ist_offset() -> FixedOffset {
    FixedOffset::east_opt(19_800).expect("valid IST offset")
}

fn market_close_time(date: NaiveDate) -> DateTime<Utc> {
    ist_offset()
        .from_local_datetime(
            &date
                .and_hms_opt(15, 30, 0)
                .expect("valid Indian market close"),
        )
        .single()
        .expect("IST has no ambiguous local times")
        .with_timezone(&Utc)
}

fn market_candle_time(date: NaiveDate, minute_of_day: u32) -> DateTime<Utc> {
    let hour = minute_of_day / 60;
    let minute = minute_of_day % 60;
    ist_offset()
        .from_local_datetime(
            &date
                .and_hms_opt(hour, minute, 0)
                .expect("valid Indian market candle time"),
        )
        .single()
        .expect("IST has no ambiguous local times")
        .with_timezone(&Utc)
}

fn previous_weekday(mut date: NaiveDate) -> NaiveDate {
    loop {
        date -= Duration::days(1);
        if !matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
            return date;
        }
    }
}

fn latest_completed_backtest_time(now: DateTime<Utc>) -> DateTime<Utc> {
    let local = now.with_timezone(&ist_offset());
    let date = local.date_naive();
    if matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
        return market_close_time(previous_weekday(date));
    }

    let minute = local.hour() * 60 + local.minute();
    let market_open_complete_minute = 9 * 60 + 20;
    let market_close_minute = 15 * 60 + 30;
    if minute < market_open_complete_minute {
        return market_close_time(previous_weekday(date));
    }
    if minute >= market_close_minute {
        return market_close_time(date);
    }

    let rounded = minute - (minute % 5);
    market_candle_time(date, rounded.saturating_sub(5))
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

async fn load_contract_master(state: &AppState) -> AppResult<Arc<Vec<MasterContract>>> {
    contract_master::load(state).await
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

fn pnl_multiplier_per_lot(instrument: &str) -> f64 {
    futures_pnl_multiplier_per_lot(instrument)
}

fn futures_margin_per_lot(
    entry_price: f64,
    instrument: &str,
    margin_requirement_percent: f64,
) -> f64 {
    entry_price * pnl_multiplier_per_lot(instrument) * margin_requirement_percent / 100.0
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

fn build_previous_closes(daily: &[Candle]) -> HashMap<NaiveDate, f64> {
    let mut closes = HashMap::new();
    for index in 1..daily.len() {
        let close = daily[index - 1].close_price;
        if close.is_finite() && close > 0.0 {
            closes.insert(candle_date(daily[index].candle_time), close);
        }
    }
    closes
}

fn build_opening_ranges(candles: &[Candle]) -> HashMap<NaiveDate, OpeningRange> {
    let mut ranges: HashMap<NaiveDate, OpeningRange> = HashMap::new();
    for candle in candles {
        let local = candle.candle_time.with_timezone(&ist_offset());
        let minute = local.hour() * 60 + local.minute();
        if !(9 * 60..9 * 60 + 15).contains(&minute) {
            continue;
        }
        if !ranges.contains_key(&local.date_naive()) && minute != 9 * 60 {
            continue;
        }
        ranges
            .entry(local.date_naive())
            .and_modify(|range| {
                range.high = range.high.max(candle.high_price);
                range.low = range.low.min(candle.low_price);
            })
            .or_insert(OpeningRange {
                market_open: candle.open_price,
                high: candle.high_price,
                low: candle.low_price,
            });
    }
    ranges
}

fn target_exit_lots(lots: i32) -> i32 {
    if lots <= 1 {
        lots.max(0)
    } else {
        (lots + 1) / 2
    }
}

fn opposite_direction(direction: &str) -> Option<&'static str> {
    match direction {
        "BUY" => Some("SELL"),
        "SELL" => Some("BUY"),
        _ => None,
    }
}

fn price_key(price: f64) -> i64 {
    (price * 100.0).round() as i64
}

fn has_open_direction(positions: &[Position], direction: &str) -> bool {
    positions
        .iter()
        .any(|position| position.direction == direction && position.remaining_lots > 0)
}

#[allow(clippy::too_many_arguments)]
fn open_position(
    candle: &Candle,
    direction: &str,
    entry_price: f64,
    entry_reason: &str,
    reversal_of_trade_id: Option<Uuid>,
    lots: i32,
    lot_size: i32,
    pnl_multiplier_per_lot: f64,
    margin_per_lot: f64,
    levels: Levels,
) -> Position {
    Position {
        id: Uuid::new_v4(),
        trade_date: candle_date(candle.candle_time),
        direction: direction.into(),
        entry_time: candle.candle_time,
        entry_price,
        entry_reason: entry_reason.into(),
        reversal_of_trade_id,
        lots,
        lot_size,
        remaining_lots: lots,
        pnl_multiplier_per_lot,
        margin_per_lot,
        margin_used: margin_per_lot * lots as f64,
        realized_pnl: 0.0,
        target_done: false,
        levels,
        entry_audit: None,
        exit_events: Vec::new(),
    }
}

fn levels_for_entry_price(mut levels: Levels, direction: &str, entry_price: f64) -> Option<Levels> {
    let exits = futures_exit_levels_for_entry(
        direction,
        entry_price,
        levels.hh2,
        levels.ll2,
        levels.hh4,
        levels.ll4,
    )?;
    match direction {
        "BUY" => {
            levels.buy_entry = entry_price;
            levels.buy_target = exits.target;
            levels.buy_sl1 = exits.sl1;
            levels.buy_sl2 = exits.sl2;
        }
        "SELL" => {
            levels.sell_entry = entry_price;
            levels.sell_target = exits.target;
            levels.sell_sl1 = exits.sl1;
            levels.sell_sl2 = exits.sl2;
        }
        _ => return None,
    }
    Some(levels)
}

fn build_entry_plan(
    levels: Levels,
    previous_close: f64,
    opening: OpeningRange,
) -> Option<EntryPlan> {
    let gap = futures_gap_direction(previous_close, opening.market_open)?;
    let jumped = futures_gap_entry_was_jumped(
        gap,
        opening.market_open,
        levels.buy_entry,
        levels.sell_entry,
    )?;
    let direction = gap.entry_direction();
    if jumped {
        let entry = futures_opening_range_entry(gap, opening.high, opening.low)?;
        let levels = levels_for_entry_price(levels, direction, entry)?;
        Some(EntryPlan {
            gap,
            direction,
            source: "OPENING_RANGE",
            previous_close,
            opening,
            levels,
            available_minute: 9 * 60 + 15,
        })
    } else {
        Some(EntryPlan {
            gap,
            direction,
            source: "STANDARD",
            previous_close,
            opening,
            levels,
            available_minute: 9 * 60 + 10,
        })
    }
}

fn close_position(
    mut position: Position,
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
    if position.remaining_lots > 0 {
        position.exit_events.push(ExitEvent {
            event: reason.into(),
            at: candle.candle_time,
            price: exit_price,
            lots: position.remaining_lots,
            quantity: position.remaining_lots.saturating_mul(position.lot_size),
            realized_pnl: final_leg_pnl,
            remaining_lots: 0,
            remaining_quantity: 0,
            position_closed: true,
        });
    }
    let quantity = position.lots.saturating_mul(position.lot_size);
    let mut audit_levels = levels_json(position.levels);
    if let Some(levels) = audit_levels.as_object_mut() {
        levels.insert("entry_reason".into(), json!(position.entry_reason));
        levels.insert(
            "reversal_of_trade_id".into(),
            json!(position.reversal_of_trade_id),
        );
        if let Some(audit) = position.entry_audit {
            levels.insert("gap_direction".into(), json!(audit.gap_direction));
            levels.insert("entry_direction".into(), json!(audit.entry_direction));
            levels.insert("entry_source".into(), json!(audit.entry_source));
            levels.insert("previous_close".into(), json!(audit.previous_close));
            levels.insert("market_open".into(), json!(audit.market_open));
            levels.insert("opening_range_high".into(), json!(audit.opening_range_high));
            levels.insert("opening_range_low".into(), json!(audit.opening_range_low));
            levels.insert("original_entry".into(), json!(audit.original_entry));
            levels.insert("effective_entry".into(), json!(audit.effective_entry));
        }
        levels.insert("exit_events".into(), json!(position.exit_events));
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
        id: position.id,
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

fn refresh_stop_levels(current: &mut Levels, latest: Levels) {
    current.buy_sl1 = latest.buy_sl1;
    current.buy_sl2 = latest.buy_sl2;
    current.sell_sl1 = latest.sell_sl1;
    current.sell_sl2 = latest.sell_sl2;
}

fn refresh_position_levels(
    position: &mut Position,
    levels_by_date: &HashMap<NaiveDate, Levels>,
    date: NaiveDate,
) {
    if date <= position.trade_date {
        return;
    }
    let Some(latest) = levels_by_date.get(&date).copied() else {
        return;
    };
    if position.reversal_of_trade_id.is_none() {
        refresh_stop_levels(&mut position.levels, latest);
        return;
    }
    let Some(exits) = futures_exit_levels_for_entry(
        &position.direction,
        position.entry_price,
        latest.hh2,
        latest.ll2,
        latest.hh4,
        latest.ll4,
    ) else {
        return;
    };
    if position.direction == "BUY" {
        position.levels.buy_sl1 = exits.sl1;
        position.levels.buy_sl2 = exits.sl2;
    } else {
        position.levels.sell_sl1 = exits.sl1;
        position.levels.sell_sl2 = exits.sl2;
    }
}

fn process_exit(
    mut current: Position,
    candle: &Candle,
    levels_by_date: &HashMap<NaiveDate, Levels>,
) -> Result<Position, TradeResult> {
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
        return Err(close_position(current, candle, price, reason));
    }
    if target && !current.target_done {
        let close_lots = target_exit_lots(current.lots).min(current.remaining_lots);
        let price = if current.direction == "BUY" {
            current.levels.buy_target
        } else {
            current.levels.sell_target
        };
        let target_pnl = trade_pnl(
            &current.direction,
            current.entry_price,
            price,
            close_lots as f64 * current.pnl_multiplier_per_lot,
        );
        current.realized_pnl += target_pnl;
        current.remaining_lots -= close_lots;
        current.target_done = true;
        current.exit_events.push(ExitEvent {
            event: "TP1".into(),
            at: candle.candle_time,
            price,
            lots: close_lots,
            quantity: close_lots.saturating_mul(current.lot_size),
            realized_pnl: target_pnl,
            remaining_lots: current.remaining_lots,
            remaining_quantity: current.remaining_lots.saturating_mul(current.lot_size),
            position_closed: current.remaining_lots <= 0,
        });
        if current.remaining_lots <= 0 {
            return Err(close_position(current, candle, price, "TARGET"));
        }
    }
    Ok(current)
}

#[allow(clippy::too_many_arguments)]
fn simulate(
    intraday: &[Candle],
    daily: &[Candle],
    opening_ranges: &HashMap<NaiveDate, OpeningRange>,
    instrument: &str,
    lot_size: i32,
    lots: i32,
    margin_requirement_percent: f64,
    buy_margin_per_lot: Option<f64>,
    sell_margin_per_lot: Option<f64>,
) -> (Vec<TradeResult>, Value) {
    let levels_by_date = build_daily_levels(daily);
    let previous_closes = build_previous_closes(daily);
    let pnl_multiplier = pnl_multiplier_per_lot(instrument);
    let mut positions: Vec<Position> = Vec::new();
    let mut trades = Vec::new();
    let mut equity: f64 = 0.0;
    let mut peak: f64 = 0.0;
    let mut max_drawdown: f64 = 0.0;
    let mut max_open_margin_used: f64 = 0.0;
    let mut breakout_entry_days: HashSet<NaiveDate> = HashSet::new();
    let mut reversal_entry_keys: HashSet<(NaiveDate, &'static str, i64)> = HashSet::new();

    for candle in intraday {
        let mut still_open = Vec::with_capacity(positions.len());
        let mut reversals = Vec::new();
        for position in positions.drain(..) {
            let closing_levels = position.levels;
            match process_exit(position, candle, &levels_by_date) {
                Ok(open) => still_open.push(open),
                Err(trade) => {
                    if trade.exit_reason == "SL2" {
                        let entry_day = candle_date(candle.candle_time);
                        let reversal_direction = opposite_direction(&trade.direction);
                        let levels = levels_by_date
                            .get(&candle_date(candle.candle_time))
                            .copied()
                            .or(Some(closing_levels));
                        if let Some(reversal) =
                            reversal_direction
                                .zip(levels)
                                .and_then(|(direction, levels)| {
                                    let key = (entry_day, direction, price_key(trade.exit_price));
                                    if reversal_entry_keys.contains(&key) {
                                        return None;
                                    }
                                    if has_open_direction(&still_open, direction)
                                        || has_open_direction(&reversals, direction)
                                    {
                                        return None;
                                    }
                                    let levels = levels_for_entry_price(
                                        levels,
                                        direction,
                                        trade.exit_price,
                                    )?;
                                    let margin_per_lot = if direction == "BUY" {
                                        buy_margin_per_lot
                                    } else {
                                        sell_margin_per_lot
                                    }
                                    .filter(|value| value.is_finite() && *value > 0.0)
                                    .unwrap_or_else(|| {
                                        futures_margin_per_lot(
                                            trade.exit_price,
                                            instrument,
                                            margin_requirement_percent,
                                        )
                                    });
                                    reversal_entry_keys.insert(key);
                                    Some(open_position(
                                        candle,
                                        direction,
                                        trade.exit_price,
                                        "SL2_REVERSAL",
                                        Some(trade.id),
                                        trade.lots,
                                        lot_size,
                                        pnl_multiplier,
                                        margin_per_lot,
                                        levels,
                                    ))
                                })
                        {
                            reversals.push(reversal);
                        }
                    }
                    equity += trade.realized_pnl;
                    peak = f64::max(peak, equity);
                    max_drawdown = f64::max(max_drawdown, peak - equity);
                    trades.push(trade);
                }
            }
        }
        still_open.extend(reversals);
        positions = still_open;
        max_open_margin_used = max_open_margin_used.max(
            positions
                .iter()
                .map(|position| position.margin_used)
                .sum::<f64>(),
        );

        let Some(session_key) = entry_session(candle.candle_time) else {
            continue;
        };
        let entry_day = session_key.0;
        if breakout_entry_days.contains(&entry_day) {
            continue;
        }
        let date = candle_date(candle.candle_time);
        let Some(levels) = levels_by_date.get(&date).copied() else {
            continue;
        };
        let Some(previous_close) = previous_closes.get(&date).copied() else {
            continue;
        };
        let Some(opening) = opening_ranges.get(&date).copied() else {
            continue;
        };
        let Some(plan) = build_entry_plan(levels, previous_close, opening) else {
            continue;
        };
        let local = candle.candle_time.with_timezone(&ist_offset());
        let minute = local.hour() * 60 + local.minute();
        if session_key.1 == "day" && minute < plan.available_minute {
            continue;
        }
        let buy = plan.direction != "SELL" && candle.high_price >= plan.levels.buy_entry;
        let sell = plan.direction != "BUY" && candle.low_price <= plan.levels.sell_entry;
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
            plan.levels.buy_entry
        } else {
            plan.levels.sell_entry
        };
        if has_open_direction(&positions, direction) {
            breakout_entry_days.insert(entry_day);
            continue;
        }
        let margin_per_lot = if direction == "BUY" {
            buy_margin_per_lot
        } else {
            sell_margin_per_lot
        }
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or_else(|| {
            futures_margin_per_lot(entry_price, instrument, margin_requirement_percent)
        });
        breakout_entry_days.insert(entry_day);
        let original_entry = if direction == "BUY" {
            levels.buy_entry
        } else {
            levels.sell_entry
        };
        let mut opened = open_position(
            candle,
            direction,
            entry_price,
            "BREAKOUT",
            None,
            lots,
            lot_size,
            pnl_multiplier,
            margin_per_lot,
            plan.levels,
        );
        opened.entry_audit = Some(EntryAudit {
            gap_direction: plan.gap.as_str(),
            entry_direction: direction.into(),
            entry_source: plan.source,
            previous_close: plan.previous_close,
            market_open: plan.opening.market_open,
            opening_range_high: plan.opening.high,
            opening_range_low: plan.opening.low,
            original_entry,
            effective_entry: entry_price,
        });
        positions.push(opened);
        max_open_margin_used = max_open_margin_used.max(
            positions
                .iter()
                .map(|position| position.margin_used)
                .sum::<f64>(),
        );
    }

    if let Some(last) = intraday.last() {
        for open in positions {
            let trade = close_position(open, last, last.close_price, "END_OF_TEST");
            equity += trade.realized_pnl;
            peak = f64::max(peak, equity);
            max_drawdown = f64::max(max_drawdown, peak - equity);
            trades.push(trade);
        }
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
        "instrument": instrument,
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
        "tp1_refresh_rule": "fixed_from_entry",
        "stop_refresh_rule": "sl1_and_sl2_daily",
        "sl2_reversal_lots": lots,
        "sl2_reversal_rule": "opposite_direction_full_original_lots",
        "sl2_reversal_management": "fresh_tp1_and_sl1_then_sl2_after_tp1",
        "open_trade_model": "multiple_concurrent_trades_without_same_side_duplicates",
        "sl2_reversals": trades.iter().filter(|trade| {
            trade.levels.get("entry_reason").and_then(Value::as_str) == Some("SL2_REVERSAL")
        }).count(),
        "pnl_multiplier_per_lot": pnl_multiplier,
        "pnl_model": "futures_price_points_x_contract_value_x_lots",
        "entry_frequency": "one_breakout_entry_per_trading_day_plus_sl2_reversals",
        "gap_entry_rule": "gap_direction_only; jumped_entries_use_completed_09:00_09:15_range_with_0.12_percent_buffer",
        "margin_requirement_percent": margin_requirement_percent,
        "initial_margin_per_lot": initial_margin_per_lot,
        "initial_margin": initial_margin,
        "max_margin_per_lot": max_margin_per_lot,
        "max_single_trade_margin_used": max_margin_used,
        "max_margin_used": max_open_margin_used,
        "buy_trades": trades.iter().filter(|trade| trade.direction == "BUY").count(),
        "sell_trades": trades.iter().filter(|trade| trade.direction == "SELL").count(),
    });
    (trades, summary)
}

pub async fn run(
    State(state): State<AppState>,
    axum::extract::Extension(user): axum::extract::Extension<AuthUser>,
    Json(input): Json<BacktestRequest>,
) -> AppResult<Json<Value>> {
    require_backtest_permission(&user)?;
    require_backtesting_available(&state, &user).await?;
    let strategy_key = input
        .strategy_key
        .as_deref()
        .unwrap_or(STRATEGY_KEY)
        .trim()
        .to_lowercase();
    let instrument = input
        .instrument
        .clone()
        .unwrap_or_else(|| "GOLDTEN".into())
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
    if strategy_key == OPTION_ENTRY_STRATEGY_KEY {
        return Err(AppError::BadRequest(
            "Option Entry Strategy V1.0 backtesting has been removed. Use live/demo strategy monitoring for Option Entry; backtesting is available only for Futures Breakout v3."
                .into(),
        ));
    }
    if strategy_key == SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY {
        return Err(AppError::BadRequest(
            "SuperTrend Index Options v1 is a live/demo option strategy. Use runtime events and trade history for validation; backtesting is available only for Futures Breakout v3."
                .into(),
        ));
    }
    let interval = normalize_interval(input.interval.clone())?;
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
    if !is_futures_breakout_instrument(&instrument) {
        return Err(AppError::BadRequest(format!(
            "Futures Breakout v3 backtesting supports {}.",
            FUTURES_BREAKOUT_INSTRUMENTS.join(", ")
        )));
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
    let to_time = latest_completed_backtest_time(Utc::now());
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
    let opening_interval_stats = if matches!(interval.as_str(), "THIRTY_MINUTE" | "ONE_HOUR") {
        Some(
            ensure_candles(
                &state,
                user.id,
                &credentials,
                &contract,
                &instrument,
                "FIFTEEN_MINUTE",
                from_time,
                to_time,
            )
            .await?,
        )
    } else {
        None
    };
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
    let opening_candles = if opening_interval_stats.is_some() {
        Some(
            load_candles(
                &state,
                &contract.exchange,
                &contract.token,
                "FIFTEEN_MINUTE",
                from_time,
                to_time,
            )
            .await?,
        )
    } else {
        None
    };
    if daily.len() < 5 || intraday.is_empty() {
        return Err(AppError::BadRequest(
            "Not enough cached or broker-returned candles to run this backtest.".into(),
        ));
    }
    let opening_ranges =
        build_opening_ranges(opening_candles.as_deref().unwrap_or(intraday.as_slice()));
    let margin_requirement_percent = effective_margin_requirement(&state, user.id).await?;
    let (trades, mut summary) = simulate(
        &intraday,
        &daily,
        &opening_ranges,
        &instrument,
        contract.lot_size,
        input.lots,
        margin_requirement_percent,
        contract.buy_margin_per_lot,
        contract.sell_margin_per_lot,
    );
    summary["daily_candles"] = json!(daily.len());
    summary["interval_candles"] = json!(intraday.len());
    summary["opening_range_candles"] =
        json!(opening_candles.as_ref().map_or(intraday.len(), Vec::len));
    summary["opening_range_days"] = json!(opening_ranges.len());
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
    let data_points = daily_stats.data_points
        + interval_stats.data_points
        + opening_interval_stats
            .as_ref()
            .map_or(0, |stats| stats.data_points);
    let reused_points = daily_stats.reused_points
        + interval_stats.reused_points
        + opening_interval_stats
            .as_ref()
            .map_or(0, |stats| stats.reused_points);
    let fetched_points = daily_stats.fetched_points
        + interval_stats.fetched_points
        + opening_interval_stats
            .as_ref()
            .map_or(0, |stats| stats.fetched_points);
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
    let availability = backtesting_availability(&state, &user).await?;
    let runs: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'strategy_key',strategy_key,'instrument',instrument,'trading_symbol',trading_symbol,'interval',interval_key,'lookback_months',lookback_months,'from_time',from_time,'to_time',to_time,'lots',lots,'lot_size',lot_size,'status',status,'summary',summary,'error',error,'data_points',data_points,'reused_points',reused_points,'fetched_points',fetched_points,'created_at',created_at) FROM backtest_runs WHERE user_id=$1 ORDER BY created_at DESC LIMIT 20")
        .bind(user.id).fetch_all(&state.db).await?;
    Ok(Json(json!({"runs":runs,"availability":availability})))
}

fn exit_events_text(levels: &Value) -> String {
    levels
        .get("exit_events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|event| {
            let label = event
                .get("event")
                .and_then(Value::as_str)
                .unwrap_or("EXIT");
            let at = event.get("at").and_then(Value::as_str).unwrap_or("-");
            let price = event.get("price").and_then(Value::as_f64).unwrap_or(0.0);
            let lots = event.get("lots").and_then(Value::as_i64).unwrap_or(0);
            let quantity = event
                .get("quantity")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let pnl = event
                .get("realized_pnl")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let remaining = event
                .get("remaining_lots")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            format!(
                "{label} @ {price:.2} on {at}: {lots} lots / {quantity} qty, P&L {pnl:+.2}, {remaining} lots remain"
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
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
        "Entry reason",
        "Exit events",
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
        let entry_reason = trade
            .levels
            .get("entry_reason")
            .and_then(Value::as_str)
            .unwrap_or("SIGNAL");
        let exit_events = exit_events_text(&trade.levels);
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
            .write_string(row, 21, entry_reason)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_string(row, 22, exit_events)
            .map_err(|error| AppError::Internal(error.into()))?;
        sheet
            .write_string(row, 23, calculation)
            .map_err(|error| AppError::Internal(error.into()))?;
    }
    for (column, width) in [(3, 24), (5, 24), (20, 18), (21, 22), (22, 72), (23, 34)] {
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
    fn trading_day_backtesting_requires_an_explicit_override() {
        let weekday = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let weekend = NaiveDate::from_ymd_opt(2026, 7, 18).unwrap();
        assert!(!backtesting_allowed_on_date(weekday, None, false));
        assert!(backtesting_allowed_on_date(weekend, None, false));
        assert!(backtesting_allowed_on_date(
            weekday,
            Some((false, false)),
            false
        ));
        assert!(!backtesting_allowed_on_date(
            weekday,
            Some((false, true)),
            false
        ));
        assert!(!backtesting_allowed_on_date(
            weekend,
            Some((true, false)),
            false
        ));
        assert!(backtesting_allowed_on_date(weekday, None, true));
        assert!(backtesting_allowed_on_date(
            weekday,
            Some((false, true)),
            true
        ));
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

    fn flat_opening_ranges(daily: &[Candle]) -> HashMap<NaiveDate, OpeningRange> {
        build_previous_closes(daily)
            .into_iter()
            .map(|(date, previous_close)| {
                (
                    date,
                    OpeningRange {
                        market_open: previous_close,
                        high: previous_close,
                        low: previous_close,
                    },
                )
            })
            .collect()
    }

    fn timed_candle(
        day: u32,
        utc_hour: u32,
        utc_minute: u32,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
    ) -> Candle {
        Candle {
            candle_time: Utc
                .with_ymd_and_hms(2026, 1, day, utc_hour, utc_minute, 0)
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
    fn simulator_waits_for_replacement_buy_entry_after_gap_jump() {
        let daily = vec![
            candle(1, 95.0, 100.0, 90.0, 96.0),
            candle(2, 96.0, 101.0, 91.0, 97.0),
            candle(3, 97.0, 102.0, 92.0, 98.0),
            candle(4, 98.0, 103.0, 93.0, 99.0),
            candle(5, 105.0, 110.0, 104.0, 106.0),
        ];
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let opening = OpeningRange {
            market_open: 105.0,
            high: 106.0,
            low: 104.0,
        };
        let replacement =
            futures_opening_range_entry(FuturesGapDirection::Up, opening.high, opening.low)
                .unwrap();
        let intraday = vec![
            timed_candle(5, 3, 40, 105.0, 106.0, 104.0, 105.5),
            timed_candle(
                5,
                3,
                45,
                replacement - 0.2,
                replacement + 0.1,
                replacement - 0.3,
                replacement,
            ),
        ];

        let (trades, _) = simulate(
            &intraday,
            &daily,
            &HashMap::from([(date, opening)]),
            "GOLDTEN",
            10,
            1,
            10.0,
            Some(12.0),
            Some(12.0),
        );

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].direction, "BUY");
        assert_eq!(
            trades[0].entry_time,
            Utc.with_ymd_and_hms(2026, 1, 5, 3, 45, 0).single().unwrap()
        );
        assert!((trades[0].entry_price - replacement).abs() < 1e-9);
        assert_eq!(trades[0].levels["gap_direction"], "UP");
        assert_eq!(trades[0].levels["entry_source"], "OPENING_RANGE");
        assert_eq!(trades[0].levels["previous_close"], 99.0);
        assert_eq!(trades[0].levels["market_open"], 105.0);
    }

    #[test]
    fn simulator_waits_for_replacement_sell_entry_after_gap_jump() {
        let daily = vec![
            candle(1, 95.0, 100.0, 90.0, 96.0),
            candle(2, 96.0, 101.0, 91.0, 97.0),
            candle(3, 97.0, 102.0, 92.0, 98.0),
            candle(4, 98.0, 103.0, 93.0, 99.0),
            candle(5, 85.0, 86.0, 83.0, 84.0),
        ];
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let opening = OpeningRange {
            market_open: 85.0,
            high: 86.0,
            low: 84.0,
        };
        let replacement =
            futures_opening_range_entry(FuturesGapDirection::Down, opening.high, opening.low)
                .unwrap();
        let intraday = vec![
            timed_candle(5, 3, 40, 85.0, 86.0, 84.0, 84.5),
            timed_candle(
                5,
                3,
                45,
                replacement + 0.2,
                replacement + 0.3,
                replacement - 0.1,
                replacement,
            ),
        ];

        let (trades, _) = simulate(
            &intraday,
            &daily,
            &HashMap::from([(date, opening)]),
            "GOLDTEN",
            10,
            1,
            10.0,
            Some(12.0),
            Some(12.0),
        );

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].direction, "SELL");
        assert_eq!(
            trades[0].entry_time,
            Utc.with_ymd_and_hms(2026, 1, 5, 3, 45, 0).single().unwrap()
        );
        assert!((trades[0].entry_price - replacement).abs() < 1e-9);
        assert_eq!(trades[0].levels["gap_direction"], "DOWN");
        assert_eq!(trades[0].levels["entry_source"], "OPENING_RANGE");
    }

    #[test]
    fn backtest_target_lot_split_matches_strategy() {
        for (lots, closed) in [(1, 1), (2, 1), (3, 2), (4, 2), (5, 3), (6, 3)] {
            assert_eq!(target_exit_lots(lots), closed);
        }
    }

    #[test]
    fn open_same_side_runner_blocks_duplicate_trade_row() {
        let daily = vec![
            candle(1, 95.0, 100.0, 90.0, 96.0),
            candle(2, 96.0, 101.0, 91.0, 97.0),
            candle(3, 97.0, 102.0, 92.0, 98.0),
            candle(4, 98.0, 103.0, 93.0, 99.0),
            candle(5, 99.0, 104.0, 94.0, 100.0),
            candle(6, 100.0, 105.0, 95.0, 101.0),
        ];
        let day5 = calculate(&[100.0, 101.0, 102.0, 103.0], &[90.0, 91.0, 92.0, 93.0]).unwrap();
        let day6 = calculate(&[101.0, 102.0, 103.0, 104.0], &[91.0, 92.0, 93.0, 94.0]).unwrap();
        let intraday = vec![
            candle(
                5,
                day5.buy_entry,
                day5.buy_entry + 0.1,
                day5.buy_entry,
                day5.buy_entry,
            ),
            candle(
                5,
                day5.buy_entry,
                day5.buy_target + 0.1,
                day5.buy_entry,
                day5.buy_target,
            ),
            candle(
                6,
                day6.buy_entry,
                day6.buy_entry + 0.1,
                day6.buy_entry,
                day6.buy_entry,
            ),
            candle(
                6,
                day6.buy_entry,
                day6.buy_target + 0.1,
                day6.buy_entry,
                day6.buy_target,
            ),
        ];

        let run = |lots| {
            simulate(
                &intraday,
                &daily,
                &flat_opening_ranges(&daily),
                "GOLDTEN",
                10,
                lots,
                10.0,
                Some(12.0),
                Some(12.0),
            )
        };

        let (two_lot_trades, two_lot_summary) = run(2);
        let (four_lot_trades, four_lot_summary) = run(4);
        let two_lot_entries: Vec<_> = two_lot_trades
            .iter()
            .map(|trade| trade.entry_time)
            .collect();
        let four_lot_entries: Vec<_> = four_lot_trades
            .iter()
            .map(|trade| trade.entry_time)
            .collect();

        assert_eq!(two_lot_trades.len(), 1);
        assert_eq!(four_lot_trades.len(), 1);
        assert_eq!(two_lot_entries, four_lot_entries);
        assert!(
            four_lot_trades
                .iter()
                .all(|trade| trade.exit_reason != "NEXT_BREAKOUT")
        );
        assert_eq!(four_lot_trades[0].exit_reason, "END_OF_TEST");
        assert_eq!(
            four_lot_trades[0].levels["exit_events"][1]["event"],
            "END_OF_TEST"
        );
        assert_eq!(two_lot_summary["trades"], four_lot_summary["trades"]);
        assert_eq!(
            four_lot_summary["open_trade_model"],
            "multiple_concurrent_trades_without_same_side_duplicates"
        );
    }

    #[test]
    fn sl2_reversal_direction_is_always_opposite() {
        assert_eq!(opposite_direction("BUY"), Some("SELL"));
        assert_eq!(opposite_direction("SELL"), Some("BUY"));
        assert_eq!(opposite_direction(""), None);
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
        let (trades, summary) = simulate(
            &intraday,
            &daily,
            &flat_opening_ranges(&daily),
            "GOLDTEN",
            10,
            3,
            10.0,
            Some(12.0),
            Some(12.0),
        );
        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].exit_reason, "SL2");
        assert_eq!(trades[0].levels["exit_events"][0]["event"], "TP1");
        assert_eq!(trades[0].levels["exit_events"][0]["lots"], 2);
        assert_eq!(trades[0].levels["exit_events"][0]["remaining_lots"], 1);
        assert_eq!(trades[0].levels["exit_events"][1]["event"], "SL2");
        assert_eq!(trades[0].levels["exit_events"][1]["lots"], 1);
        assert_eq!(trades[0].levels["exit_events"][1]["position_closed"], true);
        assert_eq!(trades[1].direction, "SELL");
        assert_eq!(trades[1].lots, 3);
        assert_eq!(trades[1].entry_price, trades[0].exit_price);
        assert_eq!(trades[1].levels["entry_reason"], "SL2_REVERSAL");
        assert_eq!(
            trades[1].levels["reversal_of_trade_id"],
            trades[0].id.to_string()
        );
        assert_eq!(summary["trades"], 2);
        assert_eq!(summary["sl2_reversals"], 1);
        assert_eq!(summary["sl2_reversal_lots"], 3);
        assert_eq!(summary["initial_margin_per_lot"], 12.0);
    }

    #[test]
    fn simulator_treats_sl2_reversal_as_fresh_tp1_then_sl2_trade() {
        let daily = vec![
            candle(1, 150.0, 170.0, 140.0, 160.0),
            candle(2, 150.0, 160.0, 141.0, 150.0),
            candle(3, 145.0, 150.0, 142.0, 146.0),
            candle(4, 146.0, 151.0, 143.0, 147.0),
            candle(5, 147.0, 152.0, 144.0, 148.0),
        ];
        let levels =
            calculate(&[170.0, 160.0, 150.0, 151.0], &[140.0, 141.0, 142.0, 143.0]).unwrap();
        let reversal = levels_for_entry_price(levels, "SELL", levels.buy_sl2).unwrap();
        assert!(levels.sell_sl1 < levels.buy_sl2);
        assert!(reversal.sell_sl1 > levels.buy_sl2);
        let intraday = vec![
            candle(
                5,
                levels.buy_entry - 0.5,
                levels.buy_entry + 0.1,
                levels.buy_entry - 1.0,
                levels.buy_entry,
            ),
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
            candle(
                5,
                levels.buy_sl2,
                reversal.sell_sl1 - 0.1,
                reversal.sell_target - 0.1,
                reversal.sell_target,
            ),
            candle(
                5,
                reversal.sell_target,
                reversal.sell_sl2 + 0.1,
                reversal.sell_target,
                reversal.sell_sl2,
            ),
        ];

        let (trades, _) = simulate(
            &intraday,
            &daily,
            &flat_opening_ranges(&daily),
            "GOLDTEN",
            10,
            3,
            10.0,
            Some(12.0),
            Some(12.0),
        );

        assert_eq!(trades[1].levels["entry_reason"], "SL2_REVERSAL");
        assert_eq!(trades[1].exit_reason, "SL2");
        assert_eq!(trades[1].levels["exit_events"][0]["event"], "TP1");
        assert_eq!(trades[1].levels["exit_events"][0]["lots"], 2);
        assert_eq!(trades[1].levels["exit_events"][1]["event"], "SL2");
        assert_eq!(trades[1].levels["exit_events"][1]["lots"], 1);
        assert!(reversal.sell_target < trades[1].entry_price);
        assert!(reversal.sell_sl1 > trades[1].entry_price);
        assert!(reversal.sell_sl2 > trades[1].entry_price);
        assert_eq!(trades[1].levels["sell_target"], reversal.sell_target);
        assert_eq!(trades[1].levels["sell_sl1"], reversal.sell_sl1);
        assert_eq!(trades[1].levels["sell_sl2"], reversal.sell_sl2);
    }

    #[test]
    fn futures_backtest_uses_contract_specific_point_values() {
        assert_eq!(pnl_multiplier_per_lot("GOLDM"), 10.0);
        assert_eq!(pnl_multiplier_per_lot("GOLDTEN"), 1.0);
        assert_eq!(pnl_multiplier_per_lot("SILVERM"), 5.0);
        assert_eq!(pnl_multiplier_per_lot("SILVERMIC"), 1.0);
        assert_eq!(pnl_multiplier_per_lot("NATGASMINI"), 250.0);
        assert_eq!(futures_margin_per_lot(100_000.0, "GOLDM", 10.0), 100_000.0);
        assert_eq!(futures_margin_per_lot(100_000.0, "GOLDTEN", 10.0), 10_000.0);
        assert_eq!(futures_margin_per_lot(100_000.0, "SILVERM", 10.0), 50_000.0);
        assert_eq!(
            futures_margin_per_lot(100_000.0, "SILVERMIC", 10.0),
            10_000.0
        );
        assert_eq!(
            futures_margin_per_lot(100_000.0, "NATGASMINI", 10.0),
            2_500_000.0
        );
    }

    #[test]
    fn simulator_prevents_same_side_trade_before_duplicate_reversal_can_form() {
        let daily = vec![
            candle(1, 95.0, 100.0, 90.0, 96.0),
            candle(2, 96.0, 100.0, 90.0, 97.0),
            candle(3, 97.0, 100.0, 90.0, 98.0),
            candle(4, 98.0, 100.0, 90.0, 99.0),
            candle(5, 99.0, 100.0, 90.0, 100.0),
            candle(6, 100.0, 100.0, 90.0, 101.0),
            candle(7, 101.0, 100.0, 90.0, 102.0),
        ];
        let levels = calculate(&[100.0, 100.0, 100.0, 100.0], &[90.0, 90.0, 90.0, 90.0]).unwrap();
        let intraday = vec![
            candle(
                5,
                levels.buy_entry,
                levels.buy_target + 0.1,
                levels.buy_entry,
                levels.buy_target,
            ),
            candle(
                6,
                levels.buy_entry,
                levels.buy_target + 0.1,
                levels.buy_entry,
                levels.buy_target,
            ),
            candle(
                6,
                levels.buy_target,
                levels.buy_target + 0.1,
                levels.buy_entry,
                levels.buy_target,
            ),
            candle(
                7,
                levels.buy_sl2 + 0.5,
                levels.buy_sl2 + 0.5,
                levels.buy_sl2 - 0.1,
                levels.buy_sl2,
            ),
        ];

        let (trades, summary) = simulate(
            &intraday,
            &daily,
            &flat_opening_ranges(&daily),
            "GOLDTEN",
            10,
            3,
            10.0,
            Some(12.0),
            Some(12.0),
        );
        let reversals = trades
            .iter()
            .filter(|trade| trade.levels["entry_reason"] == "SL2_REVERSAL")
            .count();

        assert_eq!(
            trades
                .iter()
                .filter(|trade| trade.exit_reason == "SL2")
                .count(),
            1
        );
        assert_eq!(reversals, 1);
        assert_eq!(summary["sl2_reversals"], 1);
    }

    #[test]
    fn daily_refresh_changes_only_sl1_and_sl2() {
        let mut entry =
            calculate(&[100.0, 101.0, 102.0, 103.0], &[90.0, 91.0, 92.0, 93.0]).unwrap();
        let original = entry;
        let next = calculate(&[101.0, 102.0, 103.0, 120.0], &[91.0, 92.0, 93.0, 94.0]).unwrap();

        refresh_stop_levels(&mut entry, next);

        assert_eq!(entry.buy_target, original.buy_target);
        assert_eq!(entry.sell_target, original.sell_target);
        assert_eq!(entry.buy_entry, original.buy_entry);
        assert_eq!(entry.sell_entry, original.sell_entry);
        assert_eq!(entry.buy_sl1, next.buy_sl1);
        assert_eq!(entry.buy_sl2, next.buy_sl2);
        assert_eq!(entry.sell_sl1, next.sell_sl1);
        assert_eq!(entry.sell_sl2, next.sell_sl2);
    }

    #[test]
    fn daily_refresh_keeps_reversal_tp1_and_rebases_its_stops() {
        let entry_day =
            calculate(&[100.0, 101.0, 102.0, 103.0], &[90.0, 91.0, 92.0, 93.0]).unwrap();
        let next_day = calculate(&[101.0, 102.0, 103.0, 120.0], &[91.0, 92.0, 93.0, 94.0]).unwrap();
        let reversal_entry = entry_day.buy_sl2;
        let reversal_levels = levels_for_entry_price(entry_day, "SELL", reversal_entry).unwrap();
        let fixed_target = reversal_levels.sell_target;
        let mut position = open_position(
            &candle(
                5,
                reversal_entry,
                reversal_entry,
                reversal_entry,
                reversal_entry,
            ),
            "SELL",
            reversal_entry,
            "SL2_REVERSAL",
            Some(Uuid::new_v4()),
            2,
            10,
            1.0,
            12.0,
            reversal_levels,
        );
        let next_date = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let levels_by_date = HashMap::from([(next_date, next_day)]);
        let expected = futures_exit_levels_for_entry(
            "SELL",
            reversal_entry,
            next_day.hh2,
            next_day.ll2,
            next_day.hh4,
            next_day.ll4,
        )
        .unwrap();

        refresh_position_levels(&mut position, &levels_by_date, next_date);

        assert_eq!(position.levels.sell_target, fixed_target);
        assert_eq!(position.levels.sell_sl1, expected.sl1);
        assert_eq!(position.levels.sell_sl2, expected.sl2);
        assert!(position.levels.sell_sl1 > reversal_entry);
        assert!(position.levels.sell_sl2 > reversal_entry);
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

        let (trades, summary) = simulate(
            &intraday,
            &daily,
            &flat_opening_ranges(&daily),
            "GOLDTEN",
            10,
            3,
            10.0,
            Some(12.0),
            Some(12.0),
        );

        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].exit_reason, "SL2");
        assert_eq!(trades[0].levels["partial_exit_lots"], 2);
        assert_eq!(trades[0].levels["final_leg_lots"], 1);
        assert_eq!(trades[0].levels["partial_exit_quantity"], 20);
        assert_eq!(trades[0].levels["final_leg_quantity"], 10);
        assert!((trades[0].exit_price - next_day.buy_sl2).abs() < 1e-9);
        let expected = 2.0 * (entry_day.buy_target - entry_day.buy_entry)
            + (next_day.buy_sl2 - entry_day.buy_entry);
        assert!((trades[0].realized_pnl - expected).abs() < 1e-9);
        assert_eq!(trades[1].direction, "SELL");
        assert_eq!(trades[1].lots, 3);
        assert_eq!(trades[1].quantity, 30);
        assert!((trades[1].entry_price - next_day.buy_sl2).abs() < 1e-9);
        assert_eq!(trades[1].levels["entry_reason"], "SL2_REVERSAL");
        assert_eq!(summary["target_exit_lots"], 2);
    }

    #[test]
    fn simulator_uses_configured_lots_and_contract_point_value_for_pnl() {
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
            &flat_opening_ranges(&daily),
            "GOLDTEN",
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
        assert_eq!(
            summary["pnl_model"],
            "futures_price_points_x_contract_value_x_lots"
        );
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
            &flat_opening_ranges(&daily),
            "GOLDTEN",
            10,
            1,
            10.0,
            Some(13_218.0),
            Some(13_218.0),
        );
        assert_eq!(trades.len(), 1);
        assert_eq!(
            summary["entry_frequency"],
            "one_breakout_entry_per_trading_day_plus_sl2_reversals"
        );
    }
}
