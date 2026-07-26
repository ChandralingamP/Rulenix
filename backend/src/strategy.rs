use crate::{
    angel,
    auth::AuthUser,
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
use std::collections::{HashMap, HashSet};
use tokio::time::{MissedTickBehavior, interval};
use uuid::Uuid;

pub const STRATEGY_KEY: &str = "futures_breakout_v3";
pub const OPTION_ENTRY_STRATEGY_KEY: &str = "option_entry_v1";
const SENSEX_INDEX_TOKEN: &str = "99919000";
const OPTION_INTERVAL: &str = "FIVE_MINUTE";
const OPTION_MIN_PREMIUM: f64 = 220.0;
const OPTION_MAX_PREMIUM: f64 = 300.0;
const OPTION_TARGET_PREMIUM: f64 = 260.0;
const KELTNER_EMA_PERIOD: usize = 20;
const KELTNER_ATR_PERIOD: usize = 10;
const KELTNER_MULTIPLIER: f64 = 2.0;
const TSI_LONG_PERIOD: usize = 25;
const TSI_SHORT_PERIOD: usize = 13;
const MASTER_URL: &str =
    "https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json";

#[derive(Debug, Clone, Deserialize)]
struct MasterContract {
    token: String,
    symbol: String,
    name: String,
    expiry: String,
    strike: String,
    lotsize: String,
    instrumenttype: String,
    exch_seg: String,
}

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
        match self {
            Self::Call => "BUY",
            Self::Put => "SELL",
        }
    }

    fn exit_side(self) -> &'static str {
        match self {
            Self::Call => "SELL",
            Self::Put => "BUY",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct OptionSignal {
    side: OptionSide,
    entry_price: f64,
    stop_loss: f64,
    target_band: f64,
    confirmation_at: NaiveDateTime,
    signal_at: NaiveDateTime,
}

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
            at: NaiveDateTime,
        },
    }
    let mut setup = Setup::Idle;
    let mut signal = None;
    for item in candles {
        let candle = item.candle;
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
                            at: candle.at,
                        };
                    }
                }
                Setup::AwaitBreak { high, low, at } => {
                    if candle.close > high && item.tsi > 0.0 {
                        signal = Some(OptionSignal {
                            side,
                            entry_price: candle.close,
                            stop_loss: low,
                            target_band: item.upper,
                            confirmation_at: at,
                            signal_at: candle.at,
                        });
                    } else if candle.low < item.middle {
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
                            at: candle.at,
                        };
                    }
                }
                Setup::AwaitBreak { high, low, at } => {
                    if candle.close < low && item.tsi < 0.0 {
                        signal = Some(OptionSignal {
                            side,
                            entry_price: candle.close,
                            stop_loss: high,
                            target_band: item.lower,
                            confirmation_at: at,
                            signal_at: candle.at,
                        });
                    } else if candle.high > item.middle {
                        setup = Setup::AwaitConfirmation;
                    }
                }
            },
        }
    }
    signal
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
    let Some(nearest_expiry) = candidates.iter().map(|contract| contract.expiry).min() else {
        return Vec::new();
    };
    candidates.retain(|contract| contract.expiry == nearest_expiry);
    candidates
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

fn quote_price(map: &serde_json::Map<String, Value>) -> Option<f64> {
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

async fn select_sensex_option_contract(
    state: &AppState,
    credentials: &BrokerCredentials,
    contracts: &[MasterContract],
    date: NaiveDate,
    option_type: &'static str,
    underlying_ltp: f64,
) -> AppResult<Option<OptionContract>> {
    let mut candidates = sensex_option_candidates(contracts, date, option_type);
    candidates.sort_by(|left, right| {
        (left.strike - underlying_ltp)
            .abs()
            .total_cmp(&(right.strike - underlying_ltp).abs())
    });
    if candidates.is_empty() {
        return Ok(None);
    }

    let mut premiums = HashMap::new();
    for chunk in candidates.chunks(50) {
        let tokens: Vec<String> = chunk
            .iter()
            .map(|contract| contract.token.clone())
            .collect();
        let quote = angel::market_quote(
            state,
            &credentials.api_key,
            &credentials.jwt_token,
            "LTP",
            json!({"BFO":tokens}),
        )
        .await?;
        premiums.extend(extract_quote_ltps(&quote));
    }

    Ok(choose_premium_contract(
        &candidates,
        &premiums,
        underlying_ltp,
    ))
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
    "SELECT id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,candle_dates,highs,lows,hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2,fetched_at FROM strategy_market_snapshots"
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
    snapshot.contract_token.is_some()
        && snapshot.contract_symbol.is_some()
        && snapshot.contract_expiry.is_some()
        && snapshot.lot_size.is_some()
}

async fn upsert_contract_metadata(
    state: &AppState,
    contracts: &[MasterContract],
    instrument: &str,
    date: NaiveDate,
) -> AppResult<Snapshot> {
    let (contract, expiry) = select_contract(contracts, instrument, date).ok_or_else(|| {
        AppError::BadRequest(format!(
            "No eligible MCX {instrument} FUTCOM contract is at least 10 trading days from expiry."
        ))
    })?;
    let lot_size = parse_lot_size(&contract.lotsize)
        .filter(|value| *value > 0)
        .ok_or_else(|| AppError::BadRequest("Selected contract has an invalid lot size.".into()))?;
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size) VALUES ($1,$2,$3,$4,'missing','Daily market levels are pending.',$5,$6,$7,$8) ON CONFLICT (strategy_key,instrument,trade_date,execution_key) DO UPDATE SET contract_token=EXCLUDED.contract_token,contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,error=CASE WHEN strategy_market_snapshots.status='ready' THEN strategy_market_snapshots.error ELSE EXCLUDED.error END,fetched_at=NOW()")
        .bind(Uuid::new_v4()).bind(STRATEGY_KEY).bind(instrument).bind(date)
        .bind(&contract.token).bind(&contract.symbol).bind(expiry).bind(lot_size)
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
        && has_contract_metadata(&snapshot)
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
            Some(snapshot) if has_contract_metadata(&snapshot) => {
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

async fn create_snapshot(
    state: &AppState,
    instrument: &str,
    date: NaiveDate,
) -> AppResult<Snapshot> {
    if let Some(snapshot) = load_snapshot(state, instrument, date).await?
        && snapshot.status == "ready"
    {
        return Ok(snapshot);
    }
    let contract_snapshot = ensure_contract_metadata(state, instrument, date).await?;
    let profile_id: Uuid = sqlx::query_scalar(
        "SELECT p.user_id FROM user_profiles p WHERE EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='api_key') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='jwt_token') ORDER BY CASE WHEN p.last_token_status='success' THEN 0 WHEN p.last_token_status='refreshed' THEN 1 ELSE 2 END,p.token_received_at DESC NULLS LAST LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::BadRequest("No connected Angel One session is available for the shared market snapshot.".into()))?;
    let credentials = state.credentials.load(profile_id).await?;
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
    let raw = angel::get_candles(
        state,
        &credentials.api_key,
        &credentials.jwt_token,
        token,
        &format!("{} 00:00", from.format("%Y-%m-%d")),
        &format!("{} 23:59", to.format("%Y-%m-%d")),
    )
    .await;
    let raw = match raw {
        Ok(value) => value,
        Err(error) => {
            if angel::is_invalid_api_key_error(&error.to_string()) {
                crate::home::mark_invalid(
                    state,
                    profile_id,
                    "Angel One API token is invalid. Please establish the broker connection again.",
                )
                .await?;
            }
            return Err(error);
        }
    };
    let mut candles: Vec<(NaiveDate, f64, f64)> = raw
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
            (day < date && high.is_finite() && low.is_finite()).then_some((day, high, low))
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
    let levels = calculate(&highs, &lows);
    let status = if levels.is_some() { "ready" } else { "missing" };
    let error = (levels.is_none()).then(|| {
        format!(
            "Expected 4 completed trading days, received {}.",
            candles.len()
        )
    });
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,candle_dates,highs,lows,hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2,fetched_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,NOW()) ON CONFLICT (strategy_key,instrument,trade_date,execution_key) DO UPDATE SET status=EXCLUDED.status,error=EXCLUDED.error,contract_token=EXCLUDED.contract_token,contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,candle_dates=EXCLUDED.candle_dates,highs=EXCLUDED.highs,lows=EXCLUDED.lows,hh2=EXCLUDED.hh2,ll2=EXCLUDED.ll2,hh4=EXCLUDED.hh4,ll4=EXCLUDED.ll4,buy_entry=EXCLUDED.buy_entry,buy_target=EXCLUDED.buy_target,buy_sl1=EXCLUDED.buy_sl1,buy_sl2=EXCLUDED.buy_sl2,sell_entry=EXCLUDED.sell_entry,sell_target=EXCLUDED.sell_target,sell_sl1=EXCLUDED.sell_sl1,sell_sl2=EXCLUDED.sell_sl2,fetched_at=NOW()")
        .bind(id).bind(STRATEGY_KEY).bind(instrument).bind(date).bind(status).bind(&error)
        .bind(token).bind(symbol).bind(expiry).bind(lot_size)
        .bind(&dates).bind(&highs).bind(&lows)
        .bind(levels.map(|v|v.hh2)).bind(levels.map(|v|v.ll2)).bind(levels.map(|v|v.hh4)).bind(levels.map(|v|v.ll4))
        .bind(levels.map(|v|v.buy_entry)).bind(levels.map(|v|v.buy_target)).bind(levels.map(|v|v.buy_sl1)).bind(levels.map(|v|v.buy_sl2))
        .bind(levels.map(|v|v.sell_entry)).bind(levels.map(|v|v.sell_target)).bind(levels.map(|v|v.sell_sl1)).bind(levels.map(|v|v.sell_sl2))
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

async fn connected_market_credentials(state: &AppState) -> AppResult<(Uuid, BrokerCredentials)> {
    let profile_id: Uuid = sqlx::query_scalar(
        "SELECT p.user_id FROM user_profiles p WHERE EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='api_key') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='jwt_token') ORDER BY CASE WHEN p.last_token_status='success' THEN 0 WHEN p.last_token_status='refreshed' THEN 1 ELSE 2 END,p.token_received_at DESC NULLS LAST LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::BadRequest("No connected Angel One session is available for the option strategy.".into()))?;
    let credentials = state.credentials.load(profile_id).await?;
    Ok((profile_id, credentials))
}

async fn load_contract_master(state: &AppState) -> AppResult<Vec<MasterContract>> {
    state
        .http
        .get(MASTER_URL)
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

async fn sensex_ltp(
    state: &AppState,
    profile_id: Uuid,
    credentials: &BrokerCredentials,
) -> AppResult<f64> {
    let quote = angel::market_quote(
        state,
        &credentials.api_key,
        &credentials.jwt_token,
        "LTP",
        json!({"BSE":[SENSEX_INDEX_TOKEN]}),
    )
    .await;
    match quote {
        Ok(value) => find_quote_ltp(&value).ok_or_else(|| {
            AppError::BadRequest("Angel One SENSEX quote did not include LTP.".into())
        }),
        Err(error) => {
            if angel::is_invalid_api_key_error(&error.to_string()) {
                crate::home::mark_invalid(
                    state,
                    profile_id,
                    "Angel One API token is invalid. Please establish the broker connection again.",
                )
                .await?;
            }
            Err(error)
        }
    }
}

async fn option_snapshot_for_signal(
    state: &AppState,
    side: OptionSide,
    date: NaiveDate,
) -> AppResult<Snapshot> {
    let (profile_id, credentials) = connected_market_credentials(state).await?;
    let underlying = sensex_ltp(state, profile_id, &credentials).await?;
    let contracts = load_contract_master(state).await?;
    let contract = select_sensex_option_contract(
        state,
        &credentials,
        &contracts,
        date,
        side.option_type(),
        underlying,
    )
    .await?
    .ok_or_else(|| {
            AppError::BadRequest(format!(
                "No BFO SENSEX {} option contract with premium between Rs. {OPTION_MIN_PREMIUM:.0} and Rs. {OPTION_MAX_PREMIUM:.0} is available for {date}.",
                side.option_type(),
            ))
        })?;
    let id = Uuid::new_v4();
    let instrument = side.instrument();
    let now = Utc::now();
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,fetched_at) VALUES ($1,$2,$3,$4,'ready','',$5,$6,$7,$8,'BFO','CARRYFORWARD',$9,$10,NOW()) ON CONFLICT (strategy_key,instrument,trade_date,execution_key) DO UPDATE SET status='ready',error='',contract_token=EXCLUDED.contract_token,contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,exchange_segment='BFO',product_type='CARRYFORWARD',underlying_token=EXCLUDED.underlying_token,fetched_at=NOW()")
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
    Ok(snapshot)
}

async fn option_candles(
    state: &AppState,
    snapshot: &Snapshot,
    lookback: Duration,
    to: DateTime<FixedOffset>,
) -> AppResult<Vec<IntradayCandle>> {
    let (profile_id, credentials) = connected_market_credentials(state).await?;
    let token = snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Option snapshot has no contract token.".into()))?;
    let from = to - lookback;
    let raw = angel::get_candles_with_exchange_interval(
        state,
        &credentials.api_key,
        &credentials.jwt_token,
        &snapshot.exchange_segment,
        token,
        OPTION_INTERVAL,
        &format!("{}", from.format("%Y-%m-%d %H:%M")),
        &format!("{}", to.format("%Y-%m-%d %H:%M")),
    )
    .await;
    match raw {
        Ok(value) => Ok(parse_intraday_candles(&value)),
        Err(error) => {
            if angel::is_invalid_api_key_error(&error.to_string()) {
                crate::home::mark_invalid(
                    state,
                    profile_id,
                    "Angel One API token is invalid. Please establish the broker connection again.",
                )
                .await?;
            }
            Err(error)
        }
    }
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
    let entry_credentials = if !protective {
        Some(state.credentials.load(runner.user_id).await?)
    } else {
        None
    };
    let margin_required = if protective {
        0.0
    } else {
        let credentials = entry_credentials
            .as_ref()
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("entry credentials missing")))?;
        crate::margin::estimate(
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
        .await?
        .margin_required
    };
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
            let message = error.to_string();
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
    let buy = snapshot
        .buy_entry
        .ok_or_else(|| AppError::BadRequest("Buy entry is missing.".into()))?;
    let sell = snapshot
        .sell_entry
        .ok_or_else(|| AppError::BadRequest("Sell entry is missing.".into()))?;
    if let Some(token) = snapshot.contract_token.clone() {
        crate::market_ws::ensure_strategy_feed(
            state.clone(),
            snapshot.exchange_segment.clone(),
            token,
        )
        .await;
    }
    place_strategy_order(
        state,
        runner,
        snapshot,
        session,
        NewOrder {
            role: "BUY_ENTRY",
            side: "BUY",
            order_type: "STOPLOSS_LIMIT",
            lots: runner.lots,
            price: buy,
            trigger: Some(buy),
            trade_id: None,
            quantity: None,
        },
    )
    .await?;
    place_strategy_order(
        state,
        runner,
        snapshot,
        session,
        NewOrder {
            role: "SELL_ENTRY",
            side: "SELL",
            order_type: "STOPLOSS_LIMIT",
            lots: runner.lots,
            price: sell,
            trigger: Some(sell),
            trade_id: None,
            quantity: None,
        },
    )
    .await
}

async fn run_entries(
    state: AppState,
    instrument: String,
    date: NaiveDate,
    session: &'static str,
) -> AppResult<()> {
    let snapshot = create_snapshot(&state, &instrument, date).await?;
    if snapshot.status != "ready" {
        return Err(AppError::BadRequest(
            snapshot
                .error
                .unwrap_or_else(|| "Strategy snapshot is not ready.".into()),
        ));
    }
    let runners: Vec<Runner> = sqlx::query_as("SELECT c.user_id,u.username,c.instrument,c.lots,c.run_day_session,c.run_evening_session,p.trading_mode FROM user_strategy_configs c JOIN user_strategy_activations a ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key JOIN users u ON u.id=c.user_id JOIN user_profiles p ON p.user_id=c.user_id WHERE c.enabled=TRUE AND a.is_active=TRUE AND c.strategy_key=$1 AND c.instrument=$2 AND u.is_active=TRUE AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))")
        .bind(STRATEGY_KEY).bind(&instrument).fetch_all(&state.db).await?;
    let mut tasks = tokio::task::JoinSet::new();
    for runner in runners.into_iter().filter(|r| {
        if session == "day" {
            r.run_day_session
        } else {
            r.run_evening_session
        }
    }) {
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
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => errors.push(error.to_string()),
            Err(error) => errors.push(error.to_string()),
        }
    }
    if errors.is_empty() {
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

async fn user_has_option_exposure(
    state: &AppState,
    user_id: Uuid,
    instrument: &str,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 AND status='open') OR EXISTS(SELECT 1 FROM strategy_orders WHERE user_id=$1 AND role IN ('BUY_ENTRY','SELL_ENTRY') AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND snapshot_id IN (SELECT id FROM strategy_market_snapshots WHERE strategy_key=$2 AND instrument=$3))")
        .bind(user_id)
        .bind(OPTION_ENTRY_STRATEGY_KEY)
        .bind(instrument)
        .fetch_one(&state.db)
        .await?)
}

async fn has_active_option_exit(state: &AppState, trade_id: Uuid, role: &str) -> AppResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE trade_id=$1 AND role=$2 AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling'))")
        .bind(trade_id)
        .bind(role)
        .fetch_one(&state.db)
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
    emit_for(
        state,
        OPTION_ENTRY_STRATEGY_KEY,
        None,
        signal.side.instrument(),
        "option_entry_signal",
        json!({"side":signal.side.option_type(),"signal_at":signal.signal_at,"confirmation_at":signal.confirmation_at,"entry_price":signal.entry_price,"stop_loss":signal.stop_loss,"target_band":signal.target_band}),
    )
    .await;
    let mut errors = Vec::new();
    for runner in runners {
        if user_has_option_exposure(state, runner.user_id, signal.side.instrument()).await? {
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
                price: signal.entry_price,
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
) -> AppResult<()> {
    let runners = option_runners(state, "SENSEX").await?;
    if runners.is_empty() {
        return Ok(());
    }
    let date = now.date_naive();
    let snapshot = option_snapshot_for_signal(state, side, date).await?;
    let candles = option_candles(state, &snapshot, Duration::days(2), now).await?;
    let indicators = indicator_candles(&candles);
    if let Some(signal) = option_signal(&indicators, side)
        && signal.signal_at.date() == date
        && indicators
            .last()
            .is_some_and(|latest| latest.candle.at == signal.signal_at)
    {
        place_option_entries_for_signal(state, &runners, snapshot, signal).await?;
    }
    Ok(())
}

async fn process_option_exits(state: &AppState, now: DateTime<FixedOffset>) -> AppResult<()> {
    let trades: Vec<(Uuid, Uuid, String, i32, i32, Option<Uuid>, f64)> = sqlx::query_as("SELECT id,user_id,instrument_label,quantity,remaining_lots,strategy_snapshot_id,sl1_price FROM trades WHERE strategy_key=$1 AND status='open' AND strategy_snapshot_id IS NOT NULL")
        .bind(OPTION_ENTRY_STRATEGY_KEY)
        .fetch_all(&state.db)
        .await?;
    let mut errors = Vec::new();
    for (trade_id, user_id, instrument, quantity, remaining_lots, snapshot_id, stop_loss) in trades
    {
        let Some(snapshot_id) = snapshot_id else {
            continue;
        };
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await?;
        let side = if instrument == OptionSide::Put.instrument() {
            OptionSide::Put
        } else {
            OptionSide::Call
        };
        let candles = option_candles(state, &snapshot, Duration::days(2), now).await?;
        let Some(latest) = indicator_candles(&candles).last().copied() else {
            continue;
        };
        let Some((role, price)) = option_exit(latest, side, stop_loss) else {
            continue;
        };
        if has_active_option_exit(state, trade_id, role).await? {
            continue;
        }
        let runner =
            runner_for_strategy(state, user_id, OPTION_ENTRY_STRATEGY_KEY, "SENSEX").await?;
        let session = format!(
            "optx-{}-{}-{}",
            latest.candle.at.format("%Y%m%d"),
            latest.candle.at.format("%H%M"),
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
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::BadRequest(errors.join("; ")))
    }
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
    process_option_exits(state, now).await?;
    process_option_entry_side(state, OptionSide::Call, now).await?;
    process_option_entry_side(state, OptionSide::Put, now).await
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
    strategy_snapshot_id: Option<Uuid>,
    instrument_label: String,
}

async fn cancel_active_exit_role(
    state: &AppState,
    user_id: Uuid,
    trade_id: Uuid,
    target_role: &str,
    exclude_session: &str,
) -> AppResult<()> {
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
    cancel_exit_orders(state, user_id, orders).await
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
    let trades: Vec<OpenTrade> = sqlx::query_as("SELECT id,user_id,direction,quantity,remaining_lots,total_lots,strategy_snapshot_id,instrument_label FROM trades WHERE status='open' AND strategy_key=$1 AND instrument_label=$2 AND remaining_lots>0")
        .bind(STRATEGY_KEY).bind(instrument).fetch_all(&state.db).await?;
    let mut errors = Vec::new();
    for trade in trades {
        if trade.strategy_snapshot_id.is_none() {
            continue;
        }
        let runner = runner_for(state, trade.user_id, &trade.instrument_label).await?;
        if role == "TARGET" && trade.remaining_lots < trade.total_lots {
            continue;
        }
        let (side, price, trigger) = match (trade.direction.as_str(), role) {
            ("BUY", "TARGET") => ("SELL", snapshot.buy_target, None),
            ("SELL", "TARGET") => ("BUY", snapshot.sell_target, None),
            ("BUY", "SL2") => ("SELL", snapshot.buy_sl2, snapshot.buy_sl2),
            ("SELL", "SL2") => ("BUY", snapshot.sell_sl2, snapshot.sell_sl2),
            _ => continue,
        };
        if let Some(price) = price {
            let key = format!("carry-{}-{}", date, session);
            let lots = if role == "TARGET" {
                target_exit_lots(trade.total_lots)
            } else {
                trade.remaining_lots
            };
            if let Err(error) =
                cancel_active_exit_role(state, trade.user_id, trade.id, role, &key).await
            {
                errors.push(error.to_string());
                continue;
            }
            if let Err(error) = place_strategy_order(
                state,
                &runner,
                &snapshot,
                &key,
                NewOrder {
                    role: if role == "TARGET" { "TARGET" } else { "SL2" },
                    side,
                    order_type: if role == "TARGET" {
                        "LIMIT"
                    } else {
                        "STOPLOSS_LIMIT"
                    },
                    lots,
                    price,
                    trigger,
                    trade_id: Some(trade.id),
                    quantity: Some(if role == "TARGET" {
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
            if role == "TARGET" {
                sqlx::query("UPDATE trades SET strategy_snapshot_id=$2,target_price=$3,updated_at=NOW() WHERE id=$1 AND status='open'")
                    .bind(trade.id)
                    .bind(snapshot.id)
                    .bind(price)
                    .execute(&state.db)
                    .await?;
            } else {
                sqlx::query("UPDATE trades SET strategy_snapshot_id=$2,sl2_price=$3,updated_at=NOW() WHERE id=$1 AND status='open'")
                    .bind(trade.id)
                    .bind(snapshot.id)
                    .bind(price)
                    .execute(&state.db)
                    .await?;
            }
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
    let claimed: Option<Uuid> = sqlx::query_scalar("UPDATE strategy_scheduler_runs SET status='running',attempts=attempts+1,started_at=NOW(),updated_at=NOW() WHERE strategy_key=$1 AND instrument=$2 AND trade_date=$3 AND session_key=$4 AND action=$5 AND status IN ('pending','failed') AND next_attempt_at<=NOW() RETURNING id")
        .bind(STRATEGY_KEY).bind(instrument).bind(date).bind(session).bind(action)
        .fetch_optional(&state.db).await?;
    let Some(run_id) = claimed else {
        return Ok(());
    };
    let result = if action == "target" {
        place_carry_orders(state, date, session, "TARGET", instrument).await
    } else {
        let carry = place_carry_orders(state, date, session, "SL2", instrument).await;
        let entries = run_entries(state.clone(), instrument.to_string(), date, session).await;
        match (carry, entries) {
            (Ok(()), Ok(())) => Ok(()),
            (left, right) => Err(AppError::BadRequest(
                [
                    left.err().map(|v| v.to_string()),
                    right.err().map(|v| v.to_string()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; "),
            )),
        }
    };
    match result {
        Ok(()) => {
            sqlx::query("UPDATE strategy_scheduler_runs SET status='completed',completed_at=NOW(),last_error='',updated_at=NOW() WHERE id=$1")
                .bind(run_id).execute(&state.db).await?;
        }
        Err(error) => {
            let message = error.to_string();
            sqlx::query("UPDATE strategy_scheduler_runs SET status='failed',next_attempt_at=NOW()+INTERVAL '30 seconds',last_error=$2,updated_at=NOW() WHERE id=$1")
                .bind(run_id).bind(&message).execute(&state.db).await?;
            operational_alert(
                state,
                None,
                instrument,
                "scheduler_retry",
                "error",
                &format!("{session} {action} failed; retrying: {message}"),
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
    for (action, minute_offset) in [("target", 0_u32), ("entry", 10_u32)] {
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
            let mut contracts_ready = true;
            for instrument in FUTURES_BREAKOUT_INSTRUMENTS {
                let ready = load_snapshot(&state, instrument, date)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|snapshot| has_contract_metadata(&snapshot));
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
                    let metadata_ready = snapshot.as_ref().is_some_and(has_contract_metadata);
                    let levels_ready = snapshot.is_some_and(|snapshot| snapshot.status == "ready");
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
            if minute_of_day >= 9 * 60 + 20
                && minute_of_day <= 15 * 60 + 30
                && minute_of_day % 5 == 0
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
            let demo_tokens: Vec<(String, String)> = sqlx::query_as("SELECT DISTINCT s.exchange_segment,s.contract_token FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.execution_mode='demo' AND o.status='submitted' AND s.contract_token IS NOT NULL")
                .fetch_all(&state.db).await.unwrap_or_default();
            for (exchange, token) in demo_tokens {
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

fn bounded_exit_quantity(requested: i32, open_quantity: i32) -> i32 {
    requested.max(0).min(open_quantity.max(0))
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
        let trade: Option<(Uuid, Uuid, String, String, String, i32, i32)> =
            sqlx::query_as("SELECT user_id,strategy_snapshot_id,strategy_key,instrument_label,direction,quantity,remaining_lots FROM trades WHERE id=$1 AND execution_mode='live' AND status='open' FOR UPDATE")
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
        let (role, stop) = (
            "SL2",
            if direction == "BUY" {
                snapshot.buy_sl2
            } else {
                snapshot.sell_sl2
            },
        );
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

fn required_exit_level(value: Option<f64>, label: &str) -> AppResult<f64> {
    value
        .filter(|level| level.is_finite() && *level > 0.0)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "Futures Breakout snapshot has no valid {label}; the position was not opened."
            ))
        })
}

#[cfg(test)]
fn demo_margin_amount(quantity: i32, price: f64, margin_requirement_percent: f64) -> f64 {
    quantity as f64 * price * margin_requirement_percent / 100.0
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
    let snapshot: Snapshot = sqlx::query_as(&query)
        .bind(intent.snapshot_id)
        .fetch_one(&state.db)
        .await?;
    if snapshot.strategy_key != STRATEGY_KEY {
        return Ok(Sl2ReversalOutcome::Cancelled(
            "The reversal snapshot does not belong to Futures Breakout v3.".into(),
        ));
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
           AND session_key=$3
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
    let intent: Option<Sl2ReversalIntent> = sqlx::query_as(
        "UPDATE strategy_reversal_intents
         SET status='processing',
             attempts=attempts+1,
             updated_at=NOW()
         WHERE source_trade_id=$1
           AND status IN ('pending','waiting','failed')
           AND next_attempt_at<=NOW()
         RETURNING source_trade_id,user_id,snapshot_id,instrument,source_direction,reversal_direction,lots,entry_price,order_session_key",
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
            let changed = sqlx::query(
                "UPDATE strategy_reversal_intents
                 SET status='failed',
                     next_attempt_at=NOW()+INTERVAL '30 seconds',
                     last_error=$2,
                     updated_at=NOW()
                 WHERE source_trade_id=$1 AND status='processing'",
            )
            .bind(intent.source_trade_id)
            .bind(&message)
            .execute(&state.db)
            .await?;
            if changed.rows_affected() > 0 {
                operational_alert(
                    state,
                    Some(intent.user_id),
                    &intent.instrument,
                    "sl2_reversal_retry",
                    "error",
                    &format!("The full-lot SL2 reversal will retry automatically: {message}"),
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
    if snapshot.strategy_key != OPTION_ENTRY_STRATEGY_KEY
        || !matches!(order.role.as_str(), "BUY_ENTRY" | "SELL_ENTRY")
    {
        return Ok(false);
    }
    let target = if order.side == "BUY" {
        snapshot.buy_target
    } else {
        snapshot.sell_target
    }
    .ok_or_else(|| AppError::BadRequest("Option target band is missing.".into()))?;
    let stop = if order.side == "BUY" {
        snapshot.buy_sl1
    } else {
        snapshot.sell_sl1
    }
    .ok_or_else(|| AppError::BadRequest("Option stop loss is missing.".into()))?;
    let trade_id = Uuid::new_v4();
    let mut fill_tx = state.db.begin().await?;
    if order.execution_mode == "demo" {
        sqlx::query("UPDATE user_profiles SET demo_balance=(GREATEST((demo_balance::float8 - $2),0::numeric))::numeric,updated_at=NOW() WHERE user_id=$1")
            .bind(order.user_id)
            .bind(order.margin_required)
            .execute(&mut *fill_tx)
            .await?;
    }
    sqlx::query("INSERT INTO trades (id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,external_entry_id,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,margin_required) SELECT $1,$2,execution_mode,'open',$3,$4,($5::float8)::numeric,($5::float8)::numeric,0,NOW(),$6,$7,broker_order_id,'Option Entry Strategy V1.0',$8,$9,$10,$10,$11,$12,$14 FROM strategy_orders WHERE id=$13")
        .bind(trade_id)
        .bind(order.user_id)
        .bind(&order.side)
        .bind(order.quantity)
        .bind(fill)
        .bind(&snapshot.instrument)
        .bind(snapshot.contract_symbol.as_deref().unwrap_or(""))
        .bind(OPTION_ENTRY_STRATEGY_KEY)
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
    emit_for(
        state,
        OPTION_ENTRY_STRATEGY_KEY,
        Some(order.user_id),
        &snapshot.instrument,
        "option_position_opened",
        json!({"trade_id":trade_id,"side":order.side,"fill_price":fill,"target_band":target,"stop_loss":stop,"lots":order.lots}),
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
            let (target, sl1, sl2) = if direction == "BUY" {
                (snapshot.buy_target, snapshot.buy_sl1, snapshot.buy_sl2)
            } else {
                (snapshot.sell_target, snapshot.sell_sl1, snapshot.sell_sl2)
            };
            let target = required_exit_level(target, "target")?;
            let sl1 = required_exit_level(sl1, "initial stop loss")?;
            let sl2 = required_exit_level(sl2, "continuation stop loss")?;
            if let Some(existing)=sqlx::query_as::<_,(Uuid,String,i32,f64,i32,i32,f64,String)>("SELECT id,direction,quantity,entry_price::float8,total_lots,remaining_lots,margin_required,COALESCE(contract_symbol,'') FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 AND status='open' ORDER BY entry_datetime DESC LIMIT 1")
                .bind(order.user_id).bind(STRATEGY_KEY).bind(&instrument).fetch_optional(&state.db).await? {
                if existing.1!=direction {
                    cancel_active_exits(state,order.user_id,existing.0).await?;
                    let pnl=trade_pnl(&existing.1,existing.3,fill,runtime_pnl_units(&instrument, existing.2, snapshot.lot_size));
                    let release_margin = existing.6;
                    sqlx::query("WITH closed AS (UPDATE trades SET status='closed',exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=($3::float8)::numeric,exit_datetime=NOW(),remaining_lots=0,notes=CONCAT(notes,'; SAR reversal'),updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$3+$4)::numeric,updated_at=NOW() FROM closed WHERE p.user_id=closed.user_id AND closed.execution_mode='demo'")
                        .bind(existing.0).bind(fill).bind(pnl).bind(release_margin).execute(&state.db).await?;
                    let contract_label = contract_log_label(&instrument, Some(&existing.7));
                    append_user_log(state, order.user_id, &format!("STRATEGY POSITION CLOSED {} SAR @ {:.2} P&L {:+.2}", contract_label, fill, pnl)).await;
                } else {
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
                    if let Some(source_trade_id) = order.trade_id {
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
                                price: target,
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
            sqlx::query("INSERT INTO trades (id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,external_entry_id,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,sl2_price,margin_required) SELECT $1,$2,execution_mode,'open',$3,$4,($5::float8)::numeric,($5::float8)::numeric,0,NOW(),$6,$7,broker_order_id,'Futures Breakout v3',$8,$9,$10,$10,$11,$12,$13,$15 FROM strategy_orders WHERE id=$14")
                .bind(trade_id).bind(order.user_id).bind(direction).bind(order.quantity).bind(fill).bind(&instrument).bind(snapshot.contract_symbol.as_deref().unwrap_or(""))
                .bind(STRATEGY_KEY).bind(snapshot.id).bind(order.lots.max(1)).bind(target).bind(sl1).bind(sl2).bind(order.id).bind(order.margin_required).execute(&mut *fill_tx).await?;
            sqlx::query("UPDATE strategy_orders SET status='filled',trade_id=$2,processed_quantity=GREATEST(processed_quantity,$3),filled_quantity=GREATEST(filled_quantity,$3),updated_at=NOW() WHERE id=$1").bind(order.id).bind(trade_id).bind(cumulative_fill).execute(&mut *fill_tx).await?;
            if let Some(source_trade_id) = order.trade_id {
                sqlx::query("UPDATE strategy_reversal_intents SET status='completed',last_error='',updated_at=NOW() WHERE source_trade_id=$1")
                    .bind(source_trade_id)
                    .execute(&mut *fill_tx)
                    .await?;
            }
            fill_tx.commit().await?;
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
            emit(state,Some(order.user_id),&instrument,"position_opened",json!({"trade_id":trade_id,"direction":direction,"fill_price":fill,"lots":order.lots})).await;
            append_user_log(
                state,
                order.user_id,
                &format!(
                    "STRATEGY POSITION OPENED {} {} {} lots @ {:.2} MARGIN {:.2} [{}]",
                    snapshot_contract_label,
                    direction,
                    order.lots,
                    fill,
                    order.margin_required,
                    runner.trading_mode.to_uppercase()
                ),
            )
            .await;
        }
        "TARGET" => {
            if let Some(trade_id) = order.trade_id {
                let trade:(String,i32,i32,i32,f64,f64,String)=sqlx::query_as("SELECT direction,total_lots,remaining_lots,quantity,entry_price::float8,margin_required,COALESCE(contract_symbol,'') FROM trades WHERE id=$1").bind(trade_id).fetch_one(&state.db).await?;
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
                let mut fill_tx = state.db.begin().await?;
                if remaining_quantity == 0 {
                    sqlx::query("WITH closed AS (UPDATE trades SET status='closed',remaining_lots=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=(pnl::float8+$3)::numeric,exit_datetime=NOW(),updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$3+$4)::numeric,updated_at=NOW() FROM closed WHERE p.user_id=closed.user_id AND closed.execution_mode='demo'").bind(trade_id).bind(fill).bind(realized).bind(release_margin).execute(&mut *fill_tx).await?;
                } else {
                    sqlx::query("WITH reduced AS (UPDATE trades SET remaining_lots=$2,quantity=$3,last_price=($4::float8)::numeric,pnl=(pnl::float8+$5)::numeric,margin_required=$7,updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$5+$6)::numeric,updated_at=NOW() FROM reduced WHERE p.user_id=reduced.user_id AND reduced.execution_mode='demo'").bind(trade_id).bind(remaining).bind(remaining_quantity).bind(fill).bind(realized).bind(release_margin).bind(remaining_margin).execute(&mut *fill_tx).await?;
                }
                sqlx::query("UPDATE strategy_orders SET status='filled',processed_quantity=GREATEST(processed_quantity,$2),filled_quantity=GREATEST(filled_quantity,$2),updated_at=NOW() WHERE id=$1").bind(order.id).bind(cumulative_fill).execute(&mut *fill_tx).await?;
                fill_tx.commit().await?;
                if remaining_quantity > 0 {
                    let runner = runner_for(state, order.user_id, &instrument).await?;
                    let sl2 = if trade.0 == "BUY" {
                        snapshot.buy_sl2
                    } else {
                        snapshot.sell_sl2
                    };
                    let sl2 = required_exit_level(sl2, "continuation stop loss")?;
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
        "SL1" | "SL2" => {
            if let Some(trade_id) = order.trade_id {
                let trade:(String,i32,i32,i32,f64,f64,f64,String)=sqlx::query_as("SELECT direction,quantity,remaining_lots,total_lots,entry_price::float8,pnl::float8,margin_required,COALESCE(contract_symbol,'') FROM trades WHERE id=$1").bind(trade_id).fetch_one(&state.db).await?;
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
                let reversal = if order.role == "SL2"
                    && remaining_quantity == 0
                    && snapshot.strategy_key == STRATEGY_KEY
                {
                    sl2_reversal_plan(&trade.0, trade.3)
                } else {
                    None
                };
                let mut fill_tx = state.db.begin().await?;
                if remaining_quantity == 0 {
                    sqlx::query("WITH changed AS (UPDATE trades SET status='closed',quantity=0,remaining_lots=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=($3::float8)::numeric,exit_datetime=NOW(),updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$4+$5)::numeric,updated_at=NOW() FROM changed WHERE p.user_id=changed.user_id AND changed.execution_mode='demo'").bind(trade_id).bind(fill).bind(pnl).bind(closing_pnl).bind(release_margin).execute(&mut *fill_tx).await?;
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
                        contract_log_label(&instrument, Some(&trade.7)),
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
                            contract_log_label(&instrument, Some(&trade.7)),
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
                if remaining_quantity > 0 {
                    let runner = runner_for(state, order.user_id, &instrument).await?;
                    let sl2 = if trade.0 == "BUY" {
                        snapshot.buy_sl2
                    } else {
                        snapshot.sell_sl2
                    };
                    let sl2 = required_exit_level(sl2, "continuation stop loss")?;
                    place_strategy_order(
                        state,
                        &runner,
                        &snapshot,
                        &order.session_key,
                        NewOrder {
                            role: "SL2",
                            side: if trade.0 == "BUY" { "SELL" } else { "BUY" },
                            order_type: "STOPLOSS_LIMIT",
                            lots: remaining_lots.max(1),
                            price: sl2,
                            trigger: Some(sl2),
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
    risk::record_tick(state, exchange_segment, token, ltp).await?;
    process_demo_tick(state, user_id, exchange_segment, token, ltp).await
}

async fn process_demo_tick(
    state: &AppState,
    user_id: Uuid,
    exchange_segment: &str,
    token: &str,
    ltp: f64,
) -> AppResult<()> {
    sqlx::query("UPDATE trades t SET last_price=($4::float8)::numeric,updated_at=NOW() FROM strategy_market_snapshots s WHERE t.strategy_snapshot_id=s.id AND t.user_id=$1 AND t.execution_mode='demo' AND t.status='open' AND s.exchange_segment=$2 AND s.contract_token=$3")
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
    risk::record_tick(state, exchange_segment, token, ltp).await?;
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
    let active = activation_state(&state, user).await?;
    let option_active = activation_state_for(&state, user, OPTION_ENTRY_STRATEGY_KEY).await?;
    let configs: Vec<(String, bool, i32, bool, bool)> = sqlx::query_as("SELECT instrument,enabled,lots,run_day_session,run_evening_session FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2")
        .bind(user).bind(STRATEGY_KEY).fetch_all(&state.db).await?;
    let option_config: Option<(bool, i32, bool, bool)> = sqlx::query_as("SELECT enabled,lots,run_day_session,run_evening_session FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2 AND instrument='SENSEX'")
        .bind(user).bind(OPTION_ENTRY_STRATEGY_KEY).fetch_optional(&state.db).await?;
    let snapshots = ensure_supported_contract_metadata(&state, ist_now().date_naive()).await?;
    // The strategy card is a current-status surface, not an incident log. Keep the
    // complete event history in strategy_events/logs and return only the newest
    // recent alert here so resolved retries do not clutter the trading controls.
    let alerts: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'instrument',instrument,'severity',payload->>'severity','code',payload->>'code','message',payload->>'message','created_at',created_at) FROM strategy_events WHERE strategy_key=$1 AND event_type='operational_alert' AND (user_id=$2 OR user_id IS NULL) AND created_at>NOW()-INTERVAL '10 minutes' ORDER BY created_at DESC LIMIT 1")
        .bind(STRATEGY_KEY).bind(user).fetch_all(&state.db).await?;
    let runs: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('instrument',instrument,'session',session_key,'action',action,'status',status,'attempts',attempts,'scheduled_for',scheduled_for,'last_error',last_error,'updated_at',updated_at) FROM strategy_scheduler_runs WHERE strategy_key=$1 AND trade_date=$2 ORDER BY scheduled_for,action")
        .bind(STRATEGY_KEY).bind(ist_now().date_naive()).fetch_all(&state.db).await?;
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
        "description":"5-minute SENSEX option entries using Keltner Channel retracement confirmation, TSI zero-line filter, and Rs. 220-300 option premium selection.",
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
                "product_type":"CARRYFORWARD",
                "underlying_token":SENSEX_INDEX_TOKEN
            }
        }]
    });
    Ok(Json(json!({"strategies":[breakout,option_strategy]})))
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
        STRATEGY_KEY | OPTION_ENTRY_STRATEGY_KEY
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
        STRATEGY_KEY | OPTION_ENTRY_STRATEGY_KEY
    ) {
        return Err(AppError::NotFound("Strategy not found.".into()));
    }
    let instrument = input
        .instrument
        .unwrap_or_else(|| {
            if strategy_key == OPTION_ENTRY_STRATEGY_KEY {
                "SENSEX".into()
            } else {
                "GOLDTEN".into()
            }
        })
        .trim()
        .to_uppercase();
    if strategy_key == STRATEGY_KEY && !is_futures_breakout_instrument(&instrument) {
        return Err(AppError::BadRequest(
            "Futures Breakout supports GOLD, GOLDM, and GOLDTEN.".into(),
        ));
    }
    if strategy_key == OPTION_ENTRY_STRATEGY_KEY && instrument != "SENSEX" {
        return Err(AppError::BadRequest(
            "Option Entry Strategy V1.0 supports only SENSEX.".into(),
        ));
    }
    if input.enabled && !activation_state_for(&state, user, &strategy_key).await? {
        return Err(AppError::BadRequest(
            "Activate the strategy before enabling an instrument.".into(),
        ));
    }
    sqlx::query("INSERT INTO user_strategy_configs (user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (user_id,strategy_key,instrument) DO UPDATE SET enabled=EXCLUDED.enabled,lots=EXCLUDED.lots,run_day_session=EXCLUDED.run_day_session,run_evening_session=EXCLUDED.run_evening_session,updated_at=NOW()")
        .bind(user).bind(&strategy_key).bind(&instrument).bind(input.enabled).bind(input.lots).bind(input.run_day_session.unwrap_or(true)).bind(input.run_evening_session.unwrap_or(strategy_key != OPTION_ENTRY_STRATEGY_KEY)).execute(&state.db).await?;
    emit_for(
        &state,
        &strategy_key,
        Some(user),
        &instrument,
        "configuration_updated",
        json!({"enabled":input.enabled,"lots":input.lots}),
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
            metadata: json!({"strategy_key":&strategy_key,"instrument":&instrument,"enabled":input.enabled,"lots":input.lots}),
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
        return Err(AppError::BadRequest(
            "Futures Breakout supports GOLD, GOLDM, and GOLDTEN.".into(),
        ));
    }
    let config:Option<(bool,i32,bool,bool)>=sqlx::query_as("SELECT enabled,lots,run_day_session,run_evening_session FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2 AND instrument=$3").bind(user).bind(STRATEGY_KEY).bind(&instrument).fetch_optional(&state.db).await?;
    let strategy_active = activation_state(&state, user).await?;
    let snapshot = load_snapshot(&state, &instrument, ist_now().date_naive()).await?;
    let orders:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'role',role,'side',side,'status',status,'lots',lots,'quantity',quantity,'price',price,'trigger_price',trigger_price,'margin_required',margin_required,'client_order_id',client_order_id,'broker_order_id',broker_order_id,'filled_quantity',filled_quantity,'average_fill_price',average_fill_price,'broker_error_class',broker_error_class,'broker_error_code',broker_error_code,'broker_http_status',broker_http_status,'last_reconciled_at',last_reconciled_at,'created_at',created_at) FROM strategy_orders WHERE user_id=$1 ORDER BY created_at DESC LIMIT 100").bind(user).fetch_all(&state.db).await?;
    let trades:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'status',status,'direction',direction,'lots',total_lots,'remaining_lots',remaining_lots,'quantity',quantity,'entry_price',entry_price,'exit_price',exit_price,'pnl',pnl,'margin_required',margin_required,'trigger_time',entry_datetime,'exit_time',exit_datetime,'contract_symbol',contract_symbol,'target',target_price,'sl1',sl1_price,'sl2',sl2_price) FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 ORDER BY created_at DESC LIMIT 100").bind(user).bind(STRATEGY_KEY).bind(&instrument).fetch_all(&state.db).await?;
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
    #[test]
    fn formulas_match_v3() {
        let v = calculate(&[100.0, 110.0, 105.0, 108.0], &[90.0, 92.0, 94.0, 93.0]).unwrap();
        assert_eq!(v.hh4, 110.0);
        assert_eq!(v.ll2, 93.0);
        assert!((v.buy_entry - 110.132).abs() < 1e-9);
        assert!((v.sell_entry - 89.892).abs() < 1e-9);
    }

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

    #[test]
    fn option_call_signal_follows_retrace_confirmation_break() {
        let candles = vec![
            ic(15, 100.0, 113.0, 99.0, 112.0, 100.0, 110.0, 90.0, 8.0),
            ic(20, 112.0, 113.0, 99.0, 101.0, 100.0, 110.0, 90.0, 7.0),
            ic(25, 99.0, 106.0, 98.0, 104.0, 100.0, 110.0, 90.0, 6.0),
            ic(30, 104.0, 106.5, 101.0, 105.0, 100.0, 110.0, 90.0, 5.0),
            ic(35, 105.0, 108.0, 103.0, 107.0, 100.0, 110.0, 90.0, 4.0),
        ];
        let signal = option_signal(&candles, OptionSide::Call).unwrap();
        assert_eq!(signal.side, OptionSide::Call);
        assert_eq!(signal.confirmation_at, candles[2].candle.at);
        assert_eq!(signal.signal_at, candles[4].candle.at);
        assert_eq!(signal.stop_loss, 98.0);
        assert_eq!(signal.entry_price, 107.0);
    }

    #[test]
    fn option_put_signal_follows_retrace_confirmation_break() {
        let candles = vec![
            ic(15, 100.0, 101.0, 87.0, 88.0, 100.0, 110.0, 90.0, -8.0),
            ic(20, 88.0, 101.0, 87.0, 99.0, 100.0, 110.0, 90.0, -7.0),
            ic(25, 101.0, 102.0, 94.0, 96.0, 100.0, 110.0, 90.0, -6.0),
            ic(30, 96.0, 99.0, 93.5, 94.5, 100.0, 110.0, 90.0, -5.0),
            ic(35, 94.5, 95.0, 91.0, 93.0, 100.0, 110.0, 90.0, -4.0),
        ];
        let signal = option_signal(&candles, OptionSide::Put).unwrap();
        assert_eq!(signal.side, OptionSide::Put);
        assert_eq!(signal.confirmation_at, candles[2].candle.at);
        assert_eq!(signal.signal_at, candles[4].candle.at);
        assert_eq!(signal.stop_loss, 102.0);
        assert_eq!(signal.entry_price, 93.0);
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
            ("inside_farther".to_string(), 292.0),
            ("outside_high".to_string(), 300.05),
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
        let premiums = HashMap::from([("below".to_string(), 219.0), ("above".to_string(), 301.0)]);

        assert!(choose_premium_contract(&candidates, &premiums, 76100.0).is_none());
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
    fn selects_each_supported_gold_contract_independently() {
        let items = vec![
            contract_for("GOLD", "31AUG2026", 1),
            contract_for("GOLDM", "31AUG2026", 100),
            contract_for("GOLDTEN", "31AUG2026", 10),
        ];
        let date = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        for (instrument, lot_size) in [("GOLD", 1), ("GOLDM", 100), ("GOLDTEN", 10)] {
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
    fn gold_runtime_pnl_uses_each_contract_point_value() {
        assert_eq!(runtime_pnl_units("GOLD", 4, Some(1)), 400.0);
        assert_eq!(runtime_pnl_units("GOLDM", 400, Some(100)), 40.0);
        assert_eq!(runtime_pnl_units("GOLDTEN", 40, Some(10)), 4.0);
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
