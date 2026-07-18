use crate::{
    angel,
    auth::AuthUser,
    error::{AppError, AppResult},
    state::AppState,
    strategy::STRATEGY_KEY,
};
use axum::{Json, extract::State};
use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Timelike, Utc, Weekday,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use std::collections::HashSet;
use uuid::Uuid;

const MASTER_URL: &str =
    "https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json";
const SUPPORTED_INSTRUMENT: &str = "GOLDTEN";
const ICHIMOKU_STRATEGY_KEY: &str = "ichimoku_keltner_tsi";
const SUPPORTED_ICHIMOKU_INSTRUMENTS: [&str; 2] = ["NIFTY", "SENSEX"];
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
    pub stop_loss_percent: Option<f64>,
    pub target_percent: Option<f64>,
    pub keltner_multiplier: Option<f64>,
    pub require_volume: Option<bool>,
    pub slippage_bps: Option<f64>,
    pub cost_bps: Option<f64>,
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
    remaining_lots: i32,
    pnl_multiplier_per_lot: f64,
    margin_per_lot: f64,
    margin_used: f64,
    realized_pnl: f64,
    target_done: bool,
    levels: Levels,
}

#[derive(Debug, Clone, Copy)]
struct IchimokuParameters {
    stop_loss_percent: f64,
    target_percent: f64,
    keltner_multiplier: f64,
    require_volume: bool,
    slippage_bps: f64,
    cost_bps: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct IndicatorPoint {
    pub tenkan: f64,
    pub kijun: f64,
    pub span_a: f64,
    pub span_b: f64,
    pub keltner_middle: f64,
    pub keltner_upper: f64,
    pub keltner_lower: f64,
    pub tsi: f64,
    pub tsi_signal: f64,
    pub volume_average: f64,
}

#[derive(Debug)]
struct IndicatorPosition {
    trade_date: NaiveDate,
    direction: &'static str,
    entry_time: DateTime<Utc>,
    entry_price: f64,
    stop_price: f64,
    target_price: f64,
    lots: i32,
    quantity: i32,
    entry_cost: f64,
    signal: IndicatorPoint,
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

pub(crate) async fn current_contract(
    state: &AppState,
    instrument: &str,
) -> AppResult<ContractSelection> {
    let cached = cached_contract(state, instrument).await?;
    let response = match state
        .http
        .get(MASTER_URL)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return cached.ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Unable to download Angel One contract master: {error}"
                ))
            });
        }
    };
    let response = match response.error_for_status() {
        Ok(response) => response,
        Err(error) => {
            return cached.ok_or_else(|| {
                AppError::BadRequest(format!("Angel One contract master failed: {error}"))
            });
        }
    };
    let contracts: Vec<MasterContract> = match response.json().await {
        Ok(contracts) => contracts,
        Err(error) => {
            return cached.ok_or_else(|| {
                AppError::BadRequest(format!("Invalid Angel One contract master: {error}"))
            });
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

pub(crate) fn index_contract(instrument: &str) -> Option<ContractSelection> {
    let (exchange, token, symbol) = match instrument {
        "NIFTY" => ("NSE", "99926000", "Nifty 50"),
        "BANKNIFTY" => ("NSE", "99926009", "Nifty Bank"),
        "SENSEX" => ("BSE", "99919000", "SENSEX"),
        "MIDCAPNIFTY" => ("NSE", "99926074", "NIFTY MID SELECT"),
        _ => return None,
    };
    Some(ContractSelection {
        exchange: exchange.into(),
        token: token.into(),
        symbol: symbol.into(),
        // Index candles represent the underlying index, not an expiring derivative.
        // One configured lot therefore means one index unit in this research backtest.
        lot_size: 1,
        buy_margin_per_lot: None,
        sell_margin_per_lot: None,
    })
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
        let candles = parse_candles(raw);
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

fn margin_per_lot(entry_price: f64, lot_size: i32, margin_requirement_percent: f64) -> f64 {
    entry_price * lot_size as f64 * margin_requirement_percent / 100.0
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

fn close_position(
    position: Position,
    candle: &Candle,
    exit_price: f64,
    reason: &str,
) -> TradeResult {
    let pnl_units = position.remaining_lots as f64 * position.pnl_multiplier_per_lot;
    let pnl = position.realized_pnl
        + trade_pnl(
            &position.direction,
            position.entry_price,
            exit_price,
            pnl_units,
        );
    TradeResult {
        id: Uuid::new_v4(),
        trade_date: position.trade_date,
        direction: position.direction,
        entry_time: position.entry_time,
        entry_price: position.entry_price,
        exit_time: candle.candle_time,
        exit_price,
        lots: position.lots,
        quantity: position.lots,
        margin_per_lot: position.margin_per_lot,
        margin_used: position.margin_used,
        realized_pnl: pnl,
        exit_reason: reason.into(),
        levels: levels_json(position.levels),
    }
}

fn process_exit(position: &mut Option<Position>, candle: &Candle) -> Option<TradeResult> {
    let mut current = position.take()?;
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
        let close_lots = (current.lots / 2 + 1).min(current.remaining_lots);
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
    let mut position: Option<Position> = None;
    let mut trades = Vec::new();
    let mut equity: f64 = 0.0;
    let mut peak: f64 = 0.0;
    let mut max_drawdown: f64 = 0.0;
    let mut entered_sessions: HashSet<(NaiveDate, &'static str)> = HashSet::new();

    for candle in intraday {
        if let Some(trade) = process_exit(&mut position, candle) {
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
            remaining_lots: lots,
            pnl_multiplier_per_lot: 1.0,
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
        "pnl_multiplier_per_lot": 1.0,
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

fn ichimoku_parameters(input: &BacktestRequest) -> AppResult<IchimokuParameters> {
    let parameters = IchimokuParameters {
        stop_loss_percent: input.stop_loss_percent.unwrap_or(5.0),
        target_percent: input.target_percent.unwrap_or(20.0),
        keltner_multiplier: input.keltner_multiplier.unwrap_or(2.0),
        require_volume: input.require_volume.unwrap_or(false),
        slippage_bps: input.slippage_bps.unwrap_or(5.0),
        cost_bps: input.cost_bps.unwrap_or(2.0),
    };
    if !(0.01..=100.0).contains(&parameters.stop_loss_percent)
        || !(0.01..=500.0).contains(&parameters.target_percent)
        || !(0.1..=10.0).contains(&parameters.keltner_multiplier)
        || !(0.0..=1_000.0).contains(&parameters.slippage_bps)
        || !(0.0..=1_000.0).contains(&parameters.cost_bps)
    {
        return Err(AppError::BadRequest(
            "Ichimoku percentages, Keltner multiplier, slippage, or costs are outside the supported range."
                .into(),
        ));
    }
    Ok(parameters)
}

fn rolling_midpoint(candles: &[Candle], index: usize, period: usize) -> Option<f64> {
    let start = index.checked_add(1)?.checked_sub(period)?;
    let window = candles.get(start..=index)?;
    let high = window
        .iter()
        .map(|candle| candle.high_price)
        .reduce(f64::max)?;
    let low = window
        .iter()
        .map(|candle| candle.low_price)
        .reduce(f64::min)?;
    Some((high + low) / 2.0)
}

fn ema_optional(values: &[Option<f64>], period: usize, alpha: f64) -> Vec<Option<f64>> {
    let mut result = vec![None; values.len()];
    let mut seed = Vec::with_capacity(period);
    let mut current = None;
    for (index, value) in values.iter().copied().enumerate() {
        let Some(value) = value.filter(|value| value.is_finite()) else {
            seed.clear();
            current = None;
            continue;
        };
        if let Some(previous) = current {
            let next = alpha * value + (1.0 - alpha) * previous;
            current = Some(next);
            result[index] = Some(next);
        } else {
            seed.push(value);
            if seed.len() > period {
                seed.remove(0);
            }
            if seed.len() == period {
                let next = seed.iter().sum::<f64>() / period as f64;
                current = Some(next);
                result[index] = Some(next);
            }
        }
    }
    result
}

fn ema(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let values: Vec<Option<f64>> = values.iter().copied().map(Some).collect();
    ema_optional(&values, period, 2.0 / (period as f64 + 1.0))
}

pub(crate) fn build_indicators(
    candles: &[Candle],
    keltner_multiplier: f64,
) -> Vec<Option<IndicatorPoint>> {
    let closes: Vec<f64> = candles.iter().map(|candle| candle.close_price).collect();
    let middle = ema(&closes, 20);

    let true_ranges: Vec<Option<f64>> = candles
        .iter()
        .enumerate()
        .map(|(index, candle)| {
            let previous_close = index
                .checked_sub(1)
                .and_then(|previous| candles.get(previous))
                .map(|previous| previous.close_price)
                .unwrap_or(candle.close_price);
            Some(
                (candle.high_price - candle.low_price)
                    .max((candle.high_price - previous_close).abs())
                    .max((candle.low_price - previous_close).abs()),
            )
        })
        .collect();
    let atr = ema_optional(&true_ranges, 10, 1.0 / 10.0);

    let momentum: Vec<Option<f64>> = closes
        .iter()
        .enumerate()
        .map(|(index, close)| {
            Some(if index == 0 {
                0.0
            } else {
                close - closes[index - 1]
            })
        })
        .collect();
    let absolute_momentum: Vec<Option<f64>> =
        momentum.iter().map(|value| value.map(f64::abs)).collect();
    let momentum_long = ema_optional(&momentum, 25, 2.0 / 26.0);
    let absolute_long = ema_optional(&absolute_momentum, 25, 2.0 / 26.0);
    let momentum_double = ema_optional(&momentum_long, 13, 2.0 / 14.0);
    let absolute_double = ema_optional(&absolute_long, 13, 2.0 / 14.0);
    let tsi: Vec<Option<f64>> = momentum_double
        .iter()
        .zip(&absolute_double)
        .map(
            |(numerator, denominator)| match (*numerator, *denominator) {
                (Some(numerator), Some(denominator)) if denominator.abs() > f64::EPSILON => {
                    Some(100.0 * numerator / denominator)
                }
                _ => None,
            },
        )
        .collect();
    let tsi_signal = ema_optional(&tsi, 13, 2.0 / 14.0);

    let volume_average: Vec<Option<f64>> = (0..candles.len())
        .map(|index| {
            let start = index.checked_add(1)?.checked_sub(20)?;
            Some(
                candles[start..=index]
                    .iter()
                    .map(|candle| candle.volume)
                    .sum::<f64>()
                    / 20.0,
            )
        })
        .collect();
    let tenkan: Vec<Option<f64>> = (0..candles.len())
        .map(|index| rolling_midpoint(candles, index, 9))
        .collect();
    let kijun: Vec<Option<f64>> = (0..candles.len())
        .map(|index| rolling_midpoint(candles, index, 26))
        .collect();

    (0..candles.len())
        .map(|index| {
            let cloud_source = index.checked_sub(26)?;
            let tenkan_now = tenkan[index]?;
            let kijun_now = kijun[index]?;
            let span_a = (tenkan[cloud_source]? + kijun[cloud_source]?) / 2.0;
            let span_b = rolling_midpoint(candles, cloud_source, 52)?;
            let middle = middle[index]?;
            let atr = atr[index]?;
            Some(IndicatorPoint {
                tenkan: tenkan_now,
                kijun: kijun_now,
                span_a,
                span_b,
                keltner_middle: middle,
                keltner_upper: middle + keltner_multiplier * atr,
                keltner_lower: middle - keltner_multiplier * atr,
                tsi: tsi[index]?,
                tsi_signal: tsi_signal[index]?,
                volume_average: volume_average[index]?,
            })
        })
        .collect()
}

pub(crate) fn ichimoku_signal(
    candles: &[Candle],
    indicators: &[Option<IndicatorPoint>],
    index: usize,
    require_volume: bool,
) -> Option<&'static str> {
    let previous_index = index.checked_sub(1)?;
    let previous = indicators[previous_index]?;
    let current = indicators[index]?;
    let candle = candles.get(index)?;
    let price_26_periods_ago = candles.get(index.checked_sub(26)?)?.close_price;
    let volume_ok =
        !require_volume || (candle.volume > 0.0 && candle.volume > current.volume_average);
    if !volume_ok {
        return None;
    }
    let bullish = candle.close_price > current.span_a.max(current.span_b)
        && previous.tenkan <= previous.kijun
        && current.tenkan > current.kijun
        && candle.close_price > price_26_periods_ago
        && current.span_a > current.span_b
        && candle.close_price > current.keltner_middle
        && candle.close_price > current.keltner_upper
        && previous.tsi <= previous.tsi_signal
        && current.tsi > current.tsi_signal
        && current.tsi > 0.0;
    if bullish {
        return Some("BUY");
    }
    let bearish = candle.close_price < current.span_a.min(current.span_b)
        && previous.tenkan >= previous.kijun
        && current.tenkan < current.kijun
        && candle.close_price < price_26_periods_ago
        && current.span_a < current.span_b
        && candle.close_price < current.keltner_middle
        && candle.close_price < current.keltner_lower
        && previous.tsi >= previous.tsi_signal
        && current.tsi < current.tsi_signal
        && current.tsi < 0.0;
    bearish.then_some("SELL")
}

pub(crate) fn indicator_exit_reason(
    candle: &Candle,
    previous: IndicatorPoint,
    current: IndicatorPoint,
    direction: &str,
) -> Option<&'static str> {
    let inside_cloud = candle.close_price >= current.span_a.min(current.span_b)
        && candle.close_price <= current.span_a.max(current.span_b);
    if inside_cloud {
        return Some("CLOUD_EXIT");
    }
    let opposite_tsi = if direction == "BUY" {
        previous.tsi >= previous.tsi_signal && current.tsi < current.tsi_signal
    } else {
        previous.tsi <= previous.tsi_signal && current.tsi > current.tsi_signal
    };
    opposite_tsi.then_some("TSI_EXIT")
}

fn apply_entry_slippage(price: f64, direction: &str, bps: f64) -> f64 {
    if direction == "BUY" {
        price * (1.0 + bps / 10_000.0)
    } else {
        price * (1.0 - bps / 10_000.0)
    }
}

fn apply_exit_slippage(price: f64, direction: &str, bps: f64) -> f64 {
    if direction == "BUY" {
        price * (1.0 - bps / 10_000.0)
    } else {
        price * (1.0 + bps / 10_000.0)
    }
}

fn close_indicator_position(
    position: IndicatorPosition,
    candle: &Candle,
    raw_exit_price: f64,
    reason: &str,
    parameters: IchimokuParameters,
) -> TradeResult {
    let exit_price =
        apply_exit_slippage(raw_exit_price, position.direction, parameters.slippage_bps);
    let gross_pnl = trade_pnl(
        position.direction,
        position.entry_price,
        exit_price,
        position.quantity as f64,
    );
    let exit_cost = exit_price * position.quantity as f64 * parameters.cost_bps / 10_000.0;
    let costs = position.entry_cost + exit_cost;
    TradeResult {
        id: Uuid::new_v4(),
        trade_date: position.trade_date,
        direction: position.direction.into(),
        entry_time: position.entry_time,
        entry_price: position.entry_price,
        exit_time: candle.candle_time,
        exit_price,
        lots: position.lots,
        quantity: position.quantity,
        margin_per_lot: position.entry_price,
        margin_used: position.entry_price * position.quantity as f64,
        realized_pnl: gross_pnl - costs,
        exit_reason: reason.into(),
        levels: json!({
            "stop":position.stop_price,"target":position.target_price,
            "tenkan":position.signal.tenkan,"kijun":position.signal.kijun,
            "senkou_a":position.signal.span_a,"senkou_b":position.signal.span_b,
            "keltner_middle":position.signal.keltner_middle,
            "keltner_upper":position.signal.keltner_upper,"keltner_lower":position.signal.keltner_lower,
            "tsi":position.signal.tsi,"tsi_signal":position.signal.tsi_signal,
            "volume_average":position.signal.volume_average,
            "gross_pnl":gross_pnl,"costs":costs
        }),
    }
}

fn intrabar_indicator_exit(
    position: &IndicatorPosition,
    candle: &Candle,
) -> Option<(f64, &'static str)> {
    if position.direction == "BUY" {
        if candle.open_price <= position.stop_price {
            return Some((candle.open_price, "STOP_LOSS"));
        }
        if candle.low_price <= position.stop_price {
            return Some((position.stop_price, "STOP_LOSS"));
        }
        if candle.open_price >= position.target_price {
            return Some((candle.open_price, "TARGET"));
        }
        if candle.high_price >= position.target_price {
            return Some((position.target_price, "TARGET"));
        }
    } else {
        if candle.open_price >= position.stop_price {
            return Some((candle.open_price, "STOP_LOSS"));
        }
        if candle.high_price >= position.stop_price {
            return Some((position.stop_price, "STOP_LOSS"));
        }
        if candle.open_price <= position.target_price {
            return Some((candle.open_price, "TARGET"));
        }
        if candle.low_price <= position.target_price {
            return Some((position.target_price, "TARGET"));
        }
    }
    None
}

fn take_pending_indicator_exit(
    position: &mut Option<IndicatorPosition>,
    pending_exit: &mut Option<&'static str>,
) -> Option<(IndicatorPosition, &'static str)> {
    let reason = pending_exit.take()?;
    position.take().map(|current| (current, reason))
}

fn summarize_ichimoku_trades(
    trades: &[TradeResult],
    lot_size: i32,
    parameters: IchimokuParameters,
) -> Value {
    let net_pnl: f64 = trades.iter().map(|trade| trade.realized_pnl).sum();
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
    let total_costs: f64 = trades
        .iter()
        .filter_map(|trade| trade.levels.get("costs").and_then(Value::as_f64))
        .sum();
    let initial_capital = trades
        .iter()
        .map(|trade| trade.margin_used)
        .reduce(f64::max)
        .unwrap_or(0.0);
    let mut equity = 0.0_f64;
    let mut peak = initial_capital;
    let mut max_drawdown = 0.0_f64;
    let mut max_drawdown_percent = 0.0_f64;
    let mut equity_curve = Vec::with_capacity(trades.len());
    let mut returns = Vec::with_capacity(trades.len());
    for trade in trades {
        equity += trade.realized_pnl;
        let capital = initial_capital + equity;
        peak = peak.max(capital);
        let drawdown = (peak - capital).max(0.0);
        max_drawdown = max_drawdown.max(drawdown);
        if peak > 0.0 {
            max_drawdown_percent = max_drawdown_percent.max(drawdown * 100.0 / peak);
        }
        if trade.margin_used > 0.0 {
            returns.push(trade.realized_pnl / trade.margin_used);
        }
        equity_curve.push(json!({"time":trade.exit_time,"equity":equity}));
    }
    let sharpe_ratio = if returns.len() > 1 {
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / (returns.len() - 1) as f64;
        (variance > 0.0).then_some(mean / variance.sqrt() * (returns.len() as f64).sqrt())
    } else {
        None
    };
    json!({
        "strategy_key":ICHIMOKU_STRATEGY_KEY,
        "strategy_name":"Ichimoku + Keltner + TSI",
        "trades":trades.len(),"wins":wins,"losses":losses,
        "win_rate":if trades.is_empty(){0.0}else{wins as f64*100.0/trades.len() as f64},
        "net_pnl":net_pnl,"gross_profit":gross_profit,"gross_loss":gross_loss,
        "average_pnl":if trades.is_empty(){0.0}else{net_pnl/trades.len() as f64},
        "average_win":if wins==0{0.0}else{gross_profit/wins as f64},
        "average_loss":if losses==0{0.0}else{gross_loss/losses as f64},
        "profit_factor":(gross_loss.abs()>0.0).then_some(gross_profit/gross_loss.abs()),
        "max_drawdown":max_drawdown,"max_drawdown_percent":max_drawdown_percent,
        "sharpe_ratio":sharpe_ratio,"total_costs":total_costs,"initial_capital":initial_capital,
        "initial_margin":initial_capital,"initial_margin_per_lot":trades.first().map(|trade|trade.entry_price).unwrap_or(0.0),
        "max_margin_used":initial_capital,"lot_size":lot_size,"pnl_multiplier_per_lot":lot_size,
        "buy_trades":trades.iter().filter(|trade|trade.direction=="BUY").count(),
        "sell_trades":trades.iter().filter(|trade|trade.direction=="SELL").count(),
        "equity_curve":equity_curve,
        "parameters":{
            "ichimoku":{"tenkan":9,"kijun":26,"senkou_b":52,"displacement":26},
            "keltner":{"ema":20,"atr":10,"multiplier":parameters.keltner_multiplier},
            "tsi":{"long":25,"short":13,"signal":13},
            "stop_loss_percent":parameters.stop_loss_percent,"target_percent":parameters.target_percent,
            "require_volume":parameters.require_volume,"volume_average_period":20,
            "slippage_bps":parameters.slippage_bps,"cost_bps_per_side":parameters.cost_bps,
            "execution":"next_candle_open"
        }
    })
}

fn simulate_ichimoku(
    candles: &[Candle],
    test_from: DateTime<Utc>,
    lot_size: i32,
    lots: i32,
    parameters: IchimokuParameters,
) -> (Vec<TradeResult>, Value) {
    let indicators = build_indicators(candles, parameters.keltner_multiplier);
    let mut position: Option<IndicatorPosition> = None;
    let mut pending_entry: Option<(&'static str, IndicatorPoint)> = None;
    let mut pending_exit: Option<&'static str> = None;
    let mut trades = Vec::new();

    for (index, candle) in candles.iter().enumerate() {
        if candle.candle_time < test_from {
            continue;
        }
        if let Some((current, reason)) =
            take_pending_indicator_exit(&mut position, &mut pending_exit)
        {
            trades.push(close_indicator_position(
                current,
                candle,
                candle.open_price,
                reason,
                parameters,
            ));
        }
        if position.is_none()
            && let Some((direction, signal)) = pending_entry.take()
        {
            let entry_price =
                apply_entry_slippage(candle.open_price, direction, parameters.slippage_bps);
            let quantity = lots.saturating_mul(lot_size);
            let stop_factor = parameters.stop_loss_percent / 100.0;
            let target_factor = parameters.target_percent / 100.0;
            position = Some(IndicatorPosition {
                trade_date: candle_date(candle.candle_time),
                direction,
                entry_time: candle.candle_time,
                entry_price,
                stop_price: if direction == "BUY" {
                    entry_price * (1.0 - stop_factor)
                } else {
                    entry_price * (1.0 + stop_factor)
                },
                target_price: if direction == "BUY" {
                    entry_price * (1.0 + target_factor)
                } else {
                    entry_price * (1.0 - target_factor)
                },
                lots,
                quantity,
                entry_cost: entry_price * quantity as f64 * parameters.cost_bps / 10_000.0,
                signal,
            });
        }
        if let Some(current) = position.as_ref()
            && let Some((price, reason)) = intrabar_indicator_exit(current, candle)
        {
            let current = position.take().expect("position exists");
            trades.push(close_indicator_position(
                current, candle, price, reason, parameters,
            ));
            continue;
        }
        if index == 0 {
            continue;
        }
        if let (Some(current_position), Some(previous), Some(current)) =
            (position.as_ref(), indicators[index - 1], indicators[index])
        {
            pending_exit =
                indicator_exit_reason(candle, previous, current, current_position.direction);
        } else if position.is_none()
            && pending_entry.is_none()
            && let Some(direction) =
                ichimoku_signal(candles, &indicators, index, parameters.require_volume)
            && let Some(signal) = indicators[index]
        {
            pending_entry = Some((direction, signal));
        }
    }
    if let (Some(current), Some(last)) = (position, candles.last()) {
        trades.push(close_indicator_position(
            current,
            last,
            last.close_price,
            "END_OF_TEST",
            parameters,
        ));
    }
    let summary = summarize_ichimoku_trades(&trades, lot_size, parameters);
    (trades, summary)
}

async fn run_ichimoku_backtest(
    state: &AppState,
    user: &AuthUser,
    input: &BacktestRequest,
    instrument: &str,
    interval: &str,
    credentials: &crate::credentials::BrokerCredentials,
) -> AppResult<Json<Value>> {
    if !SUPPORTED_ICHIMOKU_INSTRUMENTS.contains(&instrument) {
        return Err(AppError::BadRequest(format!(
            "Ichimoku backtesting supports only {}.",
            SUPPORTED_ICHIMOKU_INSTRUMENTS.join(", ")
        )));
    }
    let contract = if instrument == "GOLDTEN" {
        current_contract(state, instrument).await?
    } else {
        index_contract(instrument).expect("validated index instrument")
    };
    let parameters = ichimoku_parameters(input)?;
    let to_time = Utc::now();
    let from_time = to_time - Duration::days(i64::from(input.lookback_months) * 31);
    // The displaced 52-period cloud needs at least 78 completed bars. Extra calendar
    // days also cover weekends and exchange holidays without using future data.
    let warmup_from = from_time - Duration::days(60);
    let stats = ensure_candles(
        state,
        user.id,
        credentials,
        &contract,
        instrument,
        interval,
        warmup_from,
        to_time,
    )
    .await?;
    let candles = load_candles(
        state,
        &contract.exchange,
        &contract.token,
        interval,
        warmup_from,
        to_time,
    )
    .await?;
    if candles.len() < 80 {
        return Err(AppError::BadRequest(
            "At least 80 historical candles are required to warm up the displaced Ichimoku cloud."
                .into(),
        ));
    }
    let (trades, mut summary) = simulate_ichimoku(
        &candles,
        from_time,
        contract.lot_size,
        input.lots,
        parameters,
    );
    summary["interval_candles"] = json!(candles.len());
    summary["exchange"] = json!(contract.exchange);
    summary["data_basis"] = json!(if instrument == "GOLDTEN" {
        "current_goldten_futures_contract"
    } else {
        "cash_index"
    });
    let run_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO backtest_runs (id,user_id,strategy_key,instrument,trading_symbol,symbol_token,interval_key,lookback_months,from_time,to_time,lots,lot_size,status,summary,data_points,reused_points,fetched_points) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'completed',$13,$14,$15,$16)")
        .bind(run_id).bind(user.id).bind(ICHIMOKU_STRATEGY_KEY).bind(instrument).bind(&contract.symbol).bind(&contract.token).bind(interval)
        .bind(input.lookback_months).bind(from_time).bind(to_time).bind(input.lots).bind(contract.lot_size)
        .bind(&summary).bind(stats.data_points as i32).bind(stats.reused_points as i32).bind(stats.fetched_points as i32)
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
            "id":run_id,"strategy_key":ICHIMOKU_STRATEGY_KEY,"instrument":instrument,
            "trading_symbol":contract.symbol,"symbol_token":contract.token,"exchange":contract.exchange,
            "interval":interval,"lookback_months":input.lookback_months,"from_time":from_time,"to_time":to_time,
            "lots":input.lots,"lot_size":contract.lot_size,"summary":summary,
            "data_points":stats.data_points,"reused_points":stats.reused_points,"fetched_points":stats.fetched_points,
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
        .unwrap_or_else(|| {
            if strategy_key == ICHIMOKU_STRATEGY_KEY {
                "NIFTY".into()
            } else {
                SUPPORTED_INSTRUMENT.into()
            }
        })
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
    let credentials = state.credentials.load(user.id).await?;
    if credentials.api_key.is_empty() || credentials.jwt_token.is_empty() {
        return Err(AppError::BadRequest(
            "Connect Angel One before running a backtest so historical market data can be fetched."
                .into(),
        ));
    }
    if strategy_key == ICHIMOKU_STRATEGY_KEY {
        return run_ichimoku_backtest(&state, &user, &input, &instrument, &interval, &credentials)
            .await;
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
    fn simulator_does_not_multiply_pnl_by_broker_lot_size() {
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
        let (trades, _) = simulate(
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
        assert_eq!(trades[0].quantity, 1);
        assert!((trades[0].realized_pnl - expected).abs() < 1e-9);
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

    fn point(
        tenkan: f64,
        kijun: f64,
        span_a: f64,
        span_b: f64,
        tsi: f64,
        tsi_signal: f64,
    ) -> IndicatorPoint {
        IndicatorPoint {
            tenkan,
            kijun,
            span_a,
            span_b,
            keltner_middle: 110.0,
            keltner_upper: 115.0,
            keltner_lower: 105.0,
            tsi,
            tsi_signal,
            volume_average: 100.0,
        }
    }

    #[test]
    fn index_contracts_use_official_cash_index_segments() {
        let nifty = index_contract("NIFTY").unwrap();
        let bank = index_contract("BANKNIFTY").unwrap();
        let sensex = index_contract("SENSEX").unwrap();
        let midcap = index_contract("MIDCAPNIFTY").unwrap();
        assert_eq!(
            (nifty.exchange.as_str(), nifty.token.as_str()),
            ("NSE", "99926000")
        );
        assert_eq!(bank.token, "99926009");
        assert_eq!(
            (sensex.exchange.as_str(), sensex.token.as_str()),
            ("BSE", "99919000")
        );
        assert_eq!(midcap.token, "99926074");
    }

    #[test]
    fn ichimoku_buy_requires_all_crosses_and_can_skip_index_volume() {
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 3, 45, 0).single().unwrap();
        let mut candles: Vec<Candle> = (0..80)
            .map(|index| Candle {
                candle_time: start + Duration::minutes(index as i64 * 5),
                open_price: 90.0,
                high_price: 91.0,
                low_price: 89.0,
                close_price: 90.0,
                volume: 0.0,
            })
            .collect();
        candles[79].open_price = 119.0;
        candles[79].high_price = 121.0;
        candles[79].low_price = 118.0;
        candles[79].close_price = 120.0;
        let mut indicators = vec![None; candles.len()];
        indicators[78] = Some(point(99.0, 100.0, 104.0, 100.0, 1.0, 2.0));
        indicators[79] = Some(point(101.0, 100.0, 105.0, 100.0, 3.0, 2.0));

        assert_eq!(
            ichimoku_signal(&candles, &indicators, 79, false),
            Some("BUY")
        );
        assert_eq!(ichimoku_signal(&candles, &indicators, 79, true), None);
    }

    #[test]
    fn ichimoku_intrabar_collision_uses_conservative_stop_first() {
        let candle = candle(5, 100.0, 125.0, 94.0, 110.0);
        let position = IndicatorPosition {
            trade_date: candle_date(candle.candle_time),
            direction: "BUY",
            entry_time: candle.candle_time,
            entry_price: 100.0,
            stop_price: 95.0,
            target_price: 120.0,
            lots: 1,
            quantity: 1,
            entry_cost: 0.0,
            signal: point(101.0, 100.0, 105.0, 100.0, 3.0, 2.0),
        };
        assert_eq!(
            intrabar_indicator_exit(&position, &candle),
            Some((95.0, "STOP_LOSS"))
        );
    }

    #[test]
    fn ichimoku_position_survives_when_no_exit_is_pending() {
        let entry_candle = candle(5, 100.0, 101.0, 99.0, 100.0);
        let mut position = Some(IndicatorPosition {
            trade_date: candle_date(entry_candle.candle_time),
            direction: "BUY",
            entry_time: entry_candle.candle_time,
            entry_price: 100.0,
            stop_price: 95.0,
            target_price: 120.0,
            lots: 1,
            quantity: 1,
            entry_cost: 0.0,
            signal: point(101.0, 100.0, 105.0, 100.0, 3.0, 2.0),
        });
        let mut pending_exit = None;

        assert!(take_pending_indicator_exit(&mut position, &mut pending_exit).is_none());
        assert!(position.is_some());

        pending_exit = Some("TSI_EXIT");
        let (_, reason) = take_pending_indicator_exit(&mut position, &mut pending_exit).unwrap();
        assert_eq!(reason, "TSI_EXIT");
        assert!(position.is_none());
    }

    #[test]
    fn displaced_cloud_does_not_read_future_candles() {
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 3, 45, 0).single().unwrap();
        let candles: Vec<Candle> = (0..120)
            .map(|index| {
                let close = 100.0 + index as f64 * 0.1;
                Candle {
                    candle_time: start + Duration::minutes(index as i64 * 5),
                    open_price: close - 0.1,
                    high_price: close + 0.5,
                    low_price: close - 0.5,
                    close_price: close,
                    volume: 100.0,
                }
            })
            .collect();
        let before = build_indicators(&candles, 2.0)[100].unwrap();
        let mut changed = candles.clone();
        changed[119].high_price = 1_000_000.0;
        changed[119].close_price = 1_000_000.0;
        let after = build_indicators(&changed, 2.0)[100].unwrap();
        assert_eq!(before.span_a, after.span_a);
        assert_eq!(before.span_b, after.span_b);
        assert_eq!(before.tsi, after.tsi);
    }
}
