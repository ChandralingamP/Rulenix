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
struct Candle {
    candle_time: DateTime<Utc>,
    open_price: f64,
    high_price: f64,
    low_price: f64,
    close_price: f64,
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
struct ContractSelection {
    token: String,
    symbol: String,
    lot_size: i32,
    buy_margin_per_lot: Option<f64>,
    sell_margin_per_lot: Option<f64>,
}

#[derive(Debug)]
struct CacheStats {
    data_points: i64,
    reused_points: i64,
    fetched_points: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestRequest {
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
    remaining_lots: i32,
    pnl_multiplier_per_lot: f64,
    margin_per_lot: f64,
    margin_used: f64,
    realized_pnl: f64,
    target_done: bool,
    levels: Levels,
}

pub fn require_backtest_permission(user: &AuthUser) -> AppResult<()> {
    if user.can_backtest {
        Ok(())
    } else {
        Err(AppError::Forbidden("Backtesting access required.".into()))
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
                token: contract.token.clone(),
                symbol: contract.symbol.clone(),
                lot_size,
                buy_margin_per_lot: None,
                sell_margin_per_lot: None,
            })
        })
}

async fn current_contract(state: &AppState, instrument: &str) -> AppResult<ContractSelection> {
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
    token: &str,
    interval: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> AppResult<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM backtest_market_candles WHERE exchange='MCX' AND symbol_token=$1 AND interval_key=$2 AND candle_time BETWEEN $3 AND $4")
        .bind(token).bind(interval).bind(from).bind(to).fetch_one(&state.db).await?)
}

#[allow(clippy::too_many_arguments)]
async fn fetch_and_cache(
    state: &AppState,
    user: &AuthUser,
    credentials: &crate::credentials::BrokerCredentials,
    contract: &ContractSelection,
    instrument: &str,
    interval: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> AppResult<i64> {
    let raw = angel::get_candles_with_interval(
        state,
        &credentials.api_key,
        &credentials.jwt_token,
        &contract.token,
        interval,
        &from
            .with_timezone(&ist_offset())
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        &to.with_timezone(&ist_offset())
            .format("%Y-%m-%d %H:%M")
            .to_string(),
    )
    .await?;
    let candles = parse_candles(raw);
    for candle in &candles {
        sqlx::query("INSERT INTO backtest_market_candles (id,exchange,instrument,symbol_token,trading_symbol,interval_key,candle_time,open_price,high_price,low_price,close_price,volume,fetched_by) VALUES ($1,'MCX',$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) ON CONFLICT (exchange,symbol_token,interval_key,candle_time) DO UPDATE SET instrument=EXCLUDED.instrument,trading_symbol=EXCLUDED.trading_symbol,open_price=EXCLUDED.open_price,high_price=EXCLUDED.high_price,low_price=EXCLUDED.low_price,close_price=EXCLUDED.close_price,volume=EXCLUDED.volume,fetched_by=EXCLUDED.fetched_by,fetched_at=NOW()")
            .bind(Uuid::new_v4()).bind(instrument).bind(&contract.token).bind(&contract.symbol).bind(interval)
            .bind(candle.candle_time).bind(candle.open_price).bind(candle.high_price).bind(candle.low_price).bind(candle.close_price).bind(candle.volume).bind(user.id)
            .execute(&state.db).await?;
    }
    Ok(candles.len() as i64)
}

#[allow(clippy::too_many_arguments)]
async fn ensure_candles(
    state: &AppState,
    user: &AuthUser,
    credentials: &crate::credentials::BrokerCredentials,
    contract: &ContractSelection,
    instrument: &str,
    interval: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> AppResult<CacheStats> {
    let before = cached_count(state, &contract.token, interval, from, to).await?;
    let bounds: (Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as("SELECT MIN(candle_time),MAX(candle_time) FROM backtest_market_candles WHERE exchange='MCX' AND symbol_token=$1 AND interval_key=$2 AND candle_time BETWEEN $3 AND $4")
        .bind(&contract.token).bind(interval).bind(from).bind(to).fetch_one(&state.db).await?;
    let mut fetched_points = 0;
    match bounds {
        (Some(min_time), Some(max_time))
            if min_time <= from && max_time >= to - Duration::minutes(90) => {}
        (Some(min_time), Some(max_time)) => {
            if min_time > from {
                fetched_points += fetch_and_cache(
                    state,
                    user,
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
                    user,
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
                user,
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
    let data_points = cached_count(state, &contract.token, interval, from, to).await?;
    Ok(CacheStats {
        data_points,
        reused_points: before,
        fetched_points,
    })
}

async fn load_candles(
    state: &AppState,
    token: &str,
    interval: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> AppResult<Vec<Candle>> {
    Ok(sqlx::query_as("SELECT candle_time,open_price,high_price,low_price,close_price,volume FROM backtest_market_candles WHERE exchange='MCX' AND symbol_token=$1 AND interval_key=$2 AND candle_time BETWEEN $3 AND $4 ORDER BY candle_time")
        .bind(token).bind(interval).bind(from).bind(to).fetch_all(&state.db).await?)
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

pub async fn run(
    State(state): State<AppState>,
    axum::extract::Extension(user): axum::extract::Extension<AuthUser>,
    Json(input): Json<BacktestRequest>,
) -> AppResult<Json<Value>> {
    require_backtest_permission(&user)?;
    let instrument = input
        .instrument
        .unwrap_or_else(|| SUPPORTED_INSTRUMENT.into())
        .trim()
        .to_uppercase();
    if instrument != SUPPORTED_INSTRUMENT {
        return Err(AppError::BadRequest(
            "Only GOLDTEN is supported for this strategy backtest.".into(),
        ));
    }
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
    let interval = normalize_interval(input.interval)?;
    let credentials = state.credentials.load(user.id).await?;
    if credentials.api_key.is_empty() || credentials.jwt_token.is_empty() {
        return Err(AppError::BadRequest(
            "Connect Angel One before running a backtest so historical market data can be fetched."
                .into(),
        ));
    }
    let contract = current_contract(&state, &instrument).await?;
    let buy_margin = crate::margin::estimate(
        &state,
        user.id,
        &credentials.api_key,
        &credentials.jwt_token,
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
        &user,
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
        &user,
        &credentials,
        &contract,
        &instrument,
        &interval,
        from_time,
        to_time,
    )
    .await?;
    let daily = load_candles(&state, &contract.token, "ONE_DAY", warmup_from, to_time).await?;
    let intraday = load_candles(&state, &contract.token, &interval, from_time, to_time).await?;
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
    let runs: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'strategy_key',strategy_key,'instrument',instrument,'trading_symbol',trading_symbol,'interval',interval_key,'lookback_months',lookback_months,'from_time',from_time,'to_time',to_time,'lots',lots,'lot_size',lot_size,'status',status,'summary',summary,'error',error,'data_points',data_points,'reused_points',reused_points,'fetched_points',fetched_points,'created_at',created_at) FROM backtest_runs WHERE user_id=$1 ORDER BY created_at DESC LIMIT 20")
        .bind(user.id).fetch_all(&state.db).await?;
    Ok(Json(json!({"runs":runs})))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            },
            Candle {
                candle_time: Utc.with_ymd_and_hms(2026, 1, 5, 3, 50, 0).single().unwrap(),
                open_price: levels.buy_entry,
                high_price: levels.buy_target + 0.1,
                low_price: levels.buy_entry,
                close_price: levels.buy_target,
            },
            Candle {
                candle_time: Utc.with_ymd_and_hms(2026, 1, 5, 3, 55, 0).single().unwrap(),
                open_price: levels.buy_entry,
                high_price: levels.buy_entry + 0.1,
                low_price: 99.0,
                close_price: levels.buy_entry,
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
