use crate::{
    angel,
    auth::AuthUser,
    backtesting::{
        ContractSelection, build_indicators, current_contract, fetch_and_cache, ichimoku_signal,
        index_contract, indicator_exit_reason, load_candles,
    },
    credentials::BrokerCredentials,
    error::{AppError, AppResult},
    risk,
    state::AppState,
    strategy::{NewOrder, Runner, Snapshot, StoredOrder},
};
use axum::{
    Json,
    extract::{Extension, State},
    http::HeaderMap,
};
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, Timelike, Utc, Weekday};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use sqlx::FromRow;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};
use tokio::{sync::Mutex, task::JoinSet, time::MissedTickBehavior};
use uuid::Uuid;

pub const STRATEGY_KEY: &str = "ichimoku_keltner_tsi";
const INSTRUMENTS: [&str; 2] = ["NIFTY", "SENSEX"];
const MASTER_URL: &str =
    "https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json";

#[derive(Debug, Clone, FromRow)]
struct Config {
    user_id: Uuid,
    username: String,
    instrument: String,
    lots: i32,
    run_day_session: bool,
    run_evening_session: bool,
    trading_mode: String,
    interval_key: String,
    stop_loss_percent: f64,
    target_percent: f64,
    keltner_multiplier: f64,
    require_volume: bool,
    premium_min: f64,
    premium_max: f64,
}

impl Config {
    fn variant_key(&self) -> String {
        format!(
            "{}:{:.4}:{}:{:.4}:{:.4}:{:.2}:{:.2}",
            self.interval_key,
            self.keltner_multiplier,
            self.require_volume,
            self.stop_loss_percent,
            self.target_percent,
            self.premium_min,
            self.premium_max
        )
    }

    fn runner(&self) -> Runner {
        Runner {
            user_id: self.user_id,
            username: self.username.clone(),
            instrument: self.instrument.clone(),
            lots: self.lots,
            run_day_session: self.run_day_session,
            run_evening_session: self.run_evening_session,
            trading_mode: self.trading_mode.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyUpdate {
    pub instrument: String,
    pub enabled: bool,
    pub lots: i32,
    pub run_day_session: Option<bool>,
    pub run_evening_session: Option<bool>,
    pub interval_key: Option<String>,
    pub stop_loss_percent: Option<f64>,
    pub target_percent: Option<f64>,
    pub keltner_multiplier: Option<f64>,
    pub require_volume: Option<bool>,
    pub premium_min: Option<f64>,
    pub premium_max: Option<f64>,
}

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
    strike: String,
    #[serde(deserialize_with = "string_from_any")]
    lotsize: String,
    #[serde(deserialize_with = "string_from_any")]
    instrumenttype: String,
    #[serde(deserialize_with = "string_from_any")]
    exch_seg: String,
}

#[derive(Debug, Clone)]
struct ExecutionContract {
    exchange: String,
    token: String,
    symbol: String,
    lot_size: i32,
    expiry: Option<NaiveDate>,
    option_type: Option<&'static str>,
    ltp: f64,
}

fn string_from_any<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::String(value) => value,
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

fn ist_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&FixedOffset::east_opt(19_800).expect("valid IST offset"))
}

fn interval_minutes(interval: &str) -> Option<i64> {
    match interval {
        "ONE_MINUTE" => Some(1),
        "FIVE_MINUTE" => Some(5),
        "FIFTEEN_MINUTE" => Some(15),
        _ => None,
    }
}

fn market_open(instrument: &str, now: DateTime<FixedOffset>) -> bool {
    if matches!(now.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    let minute = now.hour() * 60 + now.minute();
    if instrument == "GOLDTEN" {
        (9 * 60..=23 * 60 + 30).contains(&minute)
    } else {
        (9 * 60 + 15..=15 * 60 + 30).contains(&minute)
    }
}

fn configured_session_open(config: &Config, now: DateTime<FixedOffset>) -> bool {
    if !market_open(&config.instrument, now) {
        return false;
    }
    if config.instrument != "GOLDTEN" {
        return config.run_day_session;
    }
    let minute = now.hour() * 60 + now.minute();
    if minute < 17 * 60 {
        config.run_day_session
    } else {
        config.run_evening_session
    }
}

async fn active_configs(state: &AppState) -> AppResult<Vec<Config>> {
    Ok(sqlx::query_as("SELECT c.user_id,u.username,c.instrument,c.lots,c.run_day_session,c.run_evening_session,p.trading_mode,c.interval_key,c.stop_loss_percent,c.target_percent,c.keltner_multiplier,c.require_volume,c.premium_min,c.premium_max FROM user_strategy_configs c JOIN user_strategy_activations a ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key JOIN users u ON u.id=c.user_id JOIN user_profiles p ON p.user_id=c.user_id WHERE c.strategy_key=$1 AND c.instrument IN ('NIFTY','SENSEX') AND c.enabled=TRUE AND a.is_active=TRUE AND u.is_active=TRUE AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE)) ORDER BY c.instrument,c.user_id")
        .bind(STRATEGY_KEY).fetch_all(&state.db).await?)
}

async fn shared_credentials(state: &AppState) -> AppResult<(Uuid, BrokerCredentials)> {
    let user_id: Uuid = sqlx::query_scalar("SELECT p.user_id FROM user_profiles p WHERE p.last_token_status IN ('success','refreshed') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='api_key') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='jwt_token') ORDER BY p.token_received_at DESC NULLS LAST LIMIT 1")
        .fetch_optional(&state.db).await?
        .ok_or_else(|| AppError::BadRequest("Connect Angel One before enabling continuous Ichimoku market data.".into()))?;
    let credentials = state.credentials.load(user_id).await?;
    Ok((user_id, credentials))
}

fn numeric(value: Option<&Value>) -> Option<f64> {
    value.and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
    })
}

fn parse_expiry(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(&value.to_uppercase(), "%d%b%Y").ok()
}

async fn contract_master(state: &AppState) -> AppResult<Arc<Vec<MasterContract>>> {
    let cache = CONTRACT_MASTER_CACHE.get_or_init(|| Mutex::new(None));
    let mut cached = cache.lock().await;
    if let Some((fetched_at, contracts)) = cached.as_ref()
        && fetched_at.elapsed() < CONTRACT_MASTER_CACHE_TTL
    {
        return Ok(Arc::clone(contracts));
    }
    let contracts = state
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
        })?;
    let contracts = Arc::new(contracts);
    *cached = Some((std::time::Instant::now(), Arc::clone(&contracts)));
    Ok(contracts)
}

static QUOTE_RATE_LIMIT: OnceLock<Mutex<std::time::Instant>> = OnceLock::new();
type ContractMasterCache = Mutex<Option<(std::time::Instant, Arc<Vec<MasterContract>>)>>;
static CONTRACT_MASTER_CACHE: OnceLock<ContractMasterCache> = OnceLock::new();
const CONTRACT_MASTER_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

async fn quotes(
    state: &AppState,
    credentials: &BrokerCredentials,
    exchange_tokens: Value,
) -> AppResult<Value> {
    let limiter = QUOTE_RATE_LIMIT
        .get_or_init(|| Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(2)));
    let mut last = limiter.lock().await;
    let elapsed = last.elapsed();
    if elapsed < std::time::Duration::from_secs(1) {
        tokio::time::sleep(std::time::Duration::from_secs(1) - elapsed).await;
    }
    let value = angel::market_quote(
        state,
        &credentials.api_key,
        &credentials.jwt_token,
        "LTP",
        exchange_tokens,
    )
    .await;
    *last = std::time::Instant::now();
    value
}

fn quote_rows(value: &Value) -> Vec<&Value> {
    value
        .get("fetched")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect()
}

fn option_market(instrument: &str) -> Option<(&'static str, &'static [&'static str])> {
    match instrument {
        "NIFTY" => Some(("NFO", &["NIFTY"])),
        "SENSEX" => Some(("BFO", &["SENSEX"])),
        _ => None,
    }
}

fn is_option_symbol(symbol: &str) -> bool {
    let symbol = symbol.to_uppercase();
    symbol.ends_with("CE") || symbol.ends_with("PE")
}

fn option_contract_preview(
    master: &[MasterContract],
    instrument: &str,
    today: NaiveDate,
) -> AppResult<(&'static str, NaiveDate, i32)> {
    let (exchange, names) = option_market(instrument).ok_or_else(|| {
        AppError::BadRequest("Options are available only for NIFTY 50 and SENSEX.".into())
    })?;
    let expiry = master
        .iter()
        .filter(|item| {
            item.exch_seg.eq_ignore_ascii_case(exchange)
                && names
                    .iter()
                    .any(|name| item.name.eq_ignore_ascii_case(name))
                && item.instrumenttype.eq_ignore_ascii_case("OPTIDX")
                && is_option_symbol(&item.symbol)
        })
        .filter_map(|item| parse_expiry(&item.expiry))
        .filter(|expiry| *expiry >= today)
        .min()
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "No current {instrument} option expiry was found in the Angel One contract master."
            ))
        })?;
    let lot_size = master
        .iter()
        .filter(|item| {
            item.exch_seg.eq_ignore_ascii_case(exchange)
                && names
                    .iter()
                    .any(|name| item.name.eq_ignore_ascii_case(name))
                && item.instrumenttype.eq_ignore_ascii_case("OPTIDX")
                && parse_expiry(&item.expiry) == Some(expiry)
        })
        .find_map(|item| {
            item.lotsize
                .parse::<f64>()
                .ok()
                .map(|value| value as i32)
                .filter(|value| *value > 0)
        })
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "The Angel One contract master has no valid {instrument} lot size."
            ))
        })?;
    Ok((exchange, expiry, lot_size))
}

async fn refresh_contract_preview(state: &AppState, instrument: &str) -> AppResult<()> {
    let master = contract_master(state).await?;
    let today = ist_now().date_naive();
    let (exchange, expiry, lot_size) = option_contract_preview(&master, instrument, today)?;
    let underlying = index_contract(instrument)
        .ok_or_else(|| AppError::BadRequest("Unsupported Ichimoku instrument.".into()))?;
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,fetched_at) VALUES ($1,$2,$3,$4,'ready',NULL,$5,$6,$7,'CARRYFORWARD','catalog-preview',$8,NOW()) ON CONFLICT (strategy_key,instrument,trade_date,execution_key) DO UPDATE SET status='ready',error=NULL,contract_token=NULL,contract_symbol=NULL,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,exchange_segment=EXCLUDED.exchange_segment,underlying_token=EXCLUDED.underlying_token,fetched_at=NOW()")
        .bind(Uuid::new_v4())
        .bind(STRATEGY_KEY)
        .bind(instrument)
        .bind(today)
        .bind(expiry)
        .bind(lot_size)
        .bind(exchange)
        .bind(&underlying.token)
        .execute(&state.db)
        .await?;
    Ok(())
}

async fn select_option(
    state: &AppState,
    credentials: &BrokerCredentials,
    config: &Config,
    signal: &str,
    spot: f64,
) -> AppResult<ExecutionContract> {
    let master = contract_master(state).await?;
    let (exchange, names) = option_market(&config.instrument).ok_or_else(|| {
        AppError::BadRequest("Options are available only for NIFTY 50 and SENSEX.".into())
    })?;
    let option_type = if signal == "BUY" { "CE" } else { "PE" };
    let today = ist_now().date_naive();
    let expiry = master
        .iter()
        .filter(|item| {
            item.exch_seg.eq_ignore_ascii_case(exchange)
                && names
                    .iter()
                    .any(|name| item.name.eq_ignore_ascii_case(name))
                && item.instrumenttype.eq_ignore_ascii_case("OPTIDX")
                && item.symbol.to_uppercase().ends_with(option_type)
        })
        .filter_map(|item| parse_expiry(&item.expiry))
        .filter(|expiry| *expiry >= today)
        .min()
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "No current {} {} option expiry was found.",
                config.instrument, option_type
            ))
        })?;
    let mut candidates: Vec<(MasterContract, f64)> = master
        .iter()
        .filter(|item| {
            item.exch_seg.eq_ignore_ascii_case(exchange)
                && names
                    .iter()
                    .any(|name| item.name.eq_ignore_ascii_case(name))
                && item.instrumenttype.eq_ignore_ascii_case("OPTIDX")
                && item.symbol.to_uppercase().ends_with(option_type)
                && parse_expiry(&item.expiry) == Some(expiry)
        })
        .filter_map(|item| {
            let raw = item.strike.parse::<f64>().ok()?;
            let strike = if raw > spot * 10.0 { raw / 100.0 } else { raw };
            (strike.is_finite() && (strike - spot).abs() <= spot * 0.12)
                .then_some((item.clone(), strike))
        })
        .collect();
    candidates.sort_by(|left, right| (left.1 - spot).abs().total_cmp(&(right.1 - spot).abs()));
    candidates.truncate(50);
    if candidates.is_empty() {
        return Err(AppError::BadRequest(format!(
            "No liquid {} option candidates were found near spot.",
            config.instrument
        )));
    }
    let tokens: Vec<String> = candidates.iter().map(|item| item.0.token.clone()).collect();
    let value = quotes(state, credentials, json!({exchange:tokens})).await?;
    let prices: HashMap<String, f64> = quote_rows(&value)
        .into_iter()
        .filter_map(|row| {
            let token = row
                .get("symbolToken")
                .or_else(|| row.get("symboltoken"))?
                .as_str()?
                .to_string();
            Some((token, numeric(row.get("ltp"))?))
        })
        .collect();
    let midpoint = (config.premium_min + config.premium_max) / 2.0;
    candidates
        .into_iter()
        .filter_map(|(contract, _)| {
            prices
                .get(&contract.token)
                .copied()
                .map(|ltp| (contract, ltp))
        })
        .filter(|(_, ltp)| *ltp >= config.premium_min && *ltp <= config.premium_max)
        .min_by(|left, right| {
            (left.1 - midpoint)
                .abs()
                .total_cmp(&(right.1 - midpoint).abs())
        })
        .map(|(contract, ltp)| ExecutionContract {
            exchange: exchange.into(),
            token: contract.token,
            symbol: contract.symbol,
            lot_size: contract
                .lotsize
                .parse::<f64>()
                .ok()
                .map(|value| value as i32)
                .filter(|value| *value > 0)
                .unwrap_or(1),
            expiry: Some(expiry),
            option_type: Some(if option_type == "CE" { "CE" } else { "PE" }),
            ltp,
        })
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "No {} {} option currently has a premium between ₹{:.0} and ₹{:.0}.",
                config.instrument, option_type, config.premium_min, config.premium_max
            ))
        })
}

async fn quote_one(
    state: &AppState,
    credentials: &BrokerCredentials,
    exchange: &str,
    token: &str,
) -> AppResult<f64> {
    let value = quotes(state, credentials, json!({exchange:[token]})).await?;
    quote_rows(&value)
        .into_iter()
        .find_map(|row| numeric(row.get("ltp")))
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| AppError::BadRequest("Angel One returned no current market price.".into()))
}

async fn execution_contract(
    state: &AppState,
    credentials: &BrokerCredentials,
    config: &Config,
    signal: &str,
    underlying: &ContractSelection,
    spot: f64,
) -> AppResult<ExecutionContract> {
    if config.instrument != "GOLDTEN" {
        return select_option(state, credentials, config, signal, spot).await;
    }
    let ltp = quote_one(state, credentials, &underlying.exchange, &underlying.token)
        .await
        .unwrap_or(spot);
    Ok(ExecutionContract {
        exchange: underlying.exchange.clone(),
        token: underlying.token.clone(),
        symbol: underlying.symbol.clone(),
        lot_size: underlying.lot_size,
        expiry: None,
        option_type: None,
        ltp,
    })
}

async fn create_execution_snapshot(
    state: &AppState,
    config: &Config,
    signal: &str,
    signal_time: DateTime<Utc>,
    underlying: &ContractSelection,
    contract: &ExecutionContract,
) -> AppResult<Snapshot> {
    let stop = config.stop_loss_percent / 100.0;
    let target = config.target_percent / 100.0;
    let (buy_target, buy_stop, sell_target, sell_stop) =
        exit_levels(contract.ltp, target, stop, contract.option_type.is_some());
    let execution_key = format!(
        "{}-{}-{}",
        signal_time.format("%Y%m%d%H%M"),
        signal,
        contract.token
    );
    let id: Uuid = sqlx::query_scalar("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2,fetched_at) VALUES ($1,$2,$3,$4,'ready',NULL,$5,$6,$7,$8,$9,'CARRYFORWARD',$10,$11,$12,$13,$14,$14,$12,$15,$16,$16,NOW()) ON CONFLICT (strategy_key,instrument,trade_date,execution_key) DO UPDATE SET fetched_at=NOW() RETURNING id")
        .bind(Uuid::new_v4()).bind(STRATEGY_KEY).bind(&config.instrument).bind(ist_now().date_naive())
        .bind(&contract.token).bind(&contract.symbol).bind(contract.expiry).bind(contract.lot_size)
        .bind(&contract.exchange).bind(&execution_key).bind(&underlying.token).bind(contract.ltp)
        .bind(buy_target).bind(buy_stop).bind(sell_target).bind(sell_stop)
        .fetch_one(&state.db).await?;
    Ok(Snapshot {
        id,
        strategy_key: STRATEGY_KEY.into(),
        instrument: config.instrument.clone(),
        trade_date: ist_now().date_naive(),
        status: "ready".into(),
        error: None,
        contract_token: Some(contract.token.clone()),
        contract_symbol: Some(contract.symbol.clone()),
        contract_expiry: contract.expiry,
        lot_size: Some(contract.lot_size),
        exchange_segment: contract.exchange.clone(),
        product_type: "CARRYFORWARD".into(),
        execution_key,
        underlying_token: underlying.token.clone(),
        candle_dates: vec![],
        highs: vec![],
        lows: vec![],
        hh2: None,
        ll2: None,
        hh4: None,
        ll4: None,
        buy_entry: Some(contract.ltp),
        buy_target: Some(buy_target),
        buy_sl1: Some(buy_stop),
        buy_sl2: Some(buy_stop),
        sell_entry: Some(contract.ltp),
        sell_target: Some(sell_target),
        sell_sl1: Some(sell_stop),
        sell_sl2: Some(sell_stop),
        fetched_at: Utc::now(),
    })
}

async fn place_entry(
    state: &AppState,
    config: Config,
    signal: &'static str,
    signal_time: DateTime<Utc>,
    underlying: ContractSelection,
    contract: ExecutionContract,
) -> AppResult<()> {
    let open: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 AND status='open')")
        .bind(config.user_id).bind(STRATEGY_KEY).bind(&config.instrument).fetch_one(&state.db).await?;
    if open {
        return Ok(());
    }
    risk::record_tick(state, &contract.token, contract.ltp).await?;
    let snapshot =
        create_execution_snapshot(state, &config, signal, signal_time, &underlying, &contract)
            .await?;
    let side = if contract.option_type.is_some() {
        "BUY"
    } else {
        signal
    };
    crate::strategy::place_strategy_order(
        state,
        &config.runner(),
        &snapshot,
        &format!("signal-{}", signal_time.format("%Y%m%d%H%M")),
        NewOrder {
            role: if signal == "BUY" {
                "BUY_ENTRY"
            } else {
                "SELL_ENTRY"
            },
            side,
            order_type: "MARKET",
            lots: config.lots,
            price: contract.ltp,
            trigger: None,
            trade_id: None,
            quantity: None,
        },
    )
    .await
}

async fn place_indicator_exit(
    state: &AppState,
    config: &Config,
    trade_id: Uuid,
    snapshot_id: Uuid,
    signal_time: DateTime<Utc>,
    credentials: &BrokerCredentials,
) -> AppResult<()> {
    let snapshot: Snapshot = sqlx::query_as("SELECT id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,exchange_segment,product_type,execution_key,underlying_token,candle_dates,highs,lows,hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2,fetched_at FROM strategy_market_snapshots WHERE id=$1")
        .bind(snapshot_id).fetch_one(&state.db).await?;
    let token = snapshot
        .contract_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Open Ichimoku trade has no contract token.".into()))?;
    let price = quote_one(state, credentials, &snapshot.exchange_segment, token).await?;
    risk::record_tick(state, token, price).await?;
    crate::strategy::cancel_active_exits(state, config.user_id, trade_id).await?;
    let quantity: i32 =
        sqlx::query_scalar("SELECT quantity FROM trades WHERE id=$1 AND status='open'")
            .bind(trade_id)
            .fetch_one(&state.db)
            .await?;
    crate::strategy::place_strategy_order(
        state,
        &config.runner(),
        &snapshot,
        &format!("indicator-exit-{}", signal_time.format("%Y%m%d%H%M")),
        NewOrder {
            role: "TARGET",
            side: if sqlx::query_scalar::<_, String>("SELECT direction FROM trades WHERE id=$1")
                .bind(trade_id)
                .fetch_one(&state.db)
                .await?
                == "BUY"
            {
                "SELL"
            } else {
                "BUY"
            },
            order_type: "MARKET",
            lots: ((quantity + snapshot.lot_size.unwrap_or(1) - 1)
                / snapshot.lot_size.unwrap_or(1))
            .max(1),
            price,
            trigger: None,
            trade_id: Some(trade_id),
            quantity: Some(quantity),
        },
    )
    .await
}

async fn evaluate_group(state: AppState, configs: Vec<Config>) -> AppResult<()> {
    let Some(config) = configs.first().cloned() else {
        return Ok(());
    };
    let now = Utc::now();
    let now_ist = ist_now();
    if !configured_session_open(&config, now_ist) {
        return Ok(());
    }
    let minutes = interval_minutes(&config.interval_key).ok_or_else(|| {
        AppError::BadRequest("Ichimoku live intervals must be 1, 5, or 15 minutes.".into())
    })?;
    let bucket_minute = (now_ist.minute() as i64 / minutes) * minutes;
    let current_bucket = now_ist
        .with_minute(bucket_minute as u32)
        .and_then(|value| value.with_second(0))
        .and_then(|value| value.with_nanosecond(0))
        .expect("valid candle bucket");
    let expected_candle = (current_bucket - Duration::minutes(minutes)).with_timezone(&Utc);
    let already_evaluated: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ichimoku_signal_evaluations WHERE instrument=$1 AND interval_key=$2 AND variant_key=$3 AND candle_time=$4)")
        .bind(&config.instrument).bind(&config.interval_key).bind(config.variant_key()).bind(expected_candle)
        .fetch_one(&state.db).await?;
    if already_evaluated {
        return Ok(());
    }
    let (data_user, credentials) = shared_credentials(&state).await?;
    let credentials = Arc::new(credentials);
    let underlying = if config.instrument == "GOLDTEN" {
        current_contract(&state, "GOLDTEN").await?
    } else {
        index_contract(&config.instrument)
            .ok_or_else(|| AppError::BadRequest("Unsupported Ichimoku instrument.".into()))?
    };
    let recent_max: Option<DateTime<Utc>> = sqlx::query_scalar("SELECT MAX(candle_time) FROM backtest_market_candles WHERE exchange=$1 AND symbol_token=$2 AND interval_key=$3")
        .bind(&underlying.exchange).bind(&underlying.token).bind(&config.interval_key).fetch_one(&state.db).await?;
    let from = recent_max
        .map(|value| value - Duration::minutes(minutes * 2))
        .unwrap_or(now - Duration::days(15));
    fetch_and_cache(
        &state,
        data_user,
        &credentials,
        &underlying,
        &config.instrument,
        &config.interval_key,
        from,
        now,
    )
    .await?;
    let mut candles = load_candles(
        &state,
        &underlying.exchange,
        &underlying.token,
        &config.interval_key,
        now - Duration::days(20),
        now,
    )
    .await?;
    candles.retain(|candle| candle.candle_time + Duration::minutes(minutes) <= now);
    if candles.len() < 80 {
        return Err(AppError::BadRequest(format!(
            "{} has only {} completed candles; at least 80 are required.",
            config.instrument,
            candles.len()
        )));
    }
    let indicators = build_indicators(&candles, config.keltner_multiplier);
    let index = candles.len() - 1;
    let candle_time = candles[index].candle_time;
    let previous = indicators[index - 1]
        .ok_or_else(|| AppError::BadRequest("Ichimoku indicators are still warming up.".into()))?;
    let current = indicators[index]
        .ok_or_else(|| AppError::BadRequest("Ichimoku indicators are still warming up.".into()))?;
    let signal = ichimoku_signal(&candles, &indicators, index, config.require_volume);
    let exit_buy = indicator_exit_reason(&candles[index], previous, current, "BUY").is_some();
    let exit_sell = indicator_exit_reason(&candles[index], previous, current, "SELL").is_some();
    let inserted = sqlx::query("INSERT INTO ichimoku_signal_evaluations (id,instrument,interval_key,variant_key,candle_time,signal_direction,indicator_exit_buy,indicator_exit_sell,candle_count) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (instrument,interval_key,variant_key,candle_time) DO NOTHING")
        .bind(Uuid::new_v4()).bind(&config.instrument).bind(&config.interval_key).bind(config.variant_key())
        .bind(candle_time).bind(signal).bind(exit_buy).bind(exit_sell).bind(candles.len() as i32)
        .execute(&state.db).await?;
    if inserted.rows_affected() == 0 {
        return Ok(());
    }
    crate::strategy::emit_for(
        &state,
        STRATEGY_KEY,
        None,
        &config.instrument,
        "candle_evaluated",
        json!({"candle_time":candle_time,"interval":config.interval_key,"signal":signal,"indicator_exit_buy":exit_buy,"indicator_exit_sell":exit_sell}),
    )
    .await;

    let mut exit_tasks = JoinSet::new();
    for user_config in configs.iter().cloned() {
        let open: Option<(Uuid, Uuid, String)> = sqlx::query_as("SELECT id,strategy_snapshot_id,COALESCE(signal_direction,direction) FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 AND status='open' ORDER BY entry_datetime DESC LIMIT 1")
            .bind(user_config.user_id).bind(STRATEGY_KEY).bind(&user_config.instrument).fetch_optional(&state.db).await?;
        if let Some((trade_id, snapshot_id, direction)) = open
            && ((direction == "BUY" && exit_buy) || (direction == "SELL" && exit_sell))
        {
            let cloned = state.clone();
            let cloned_credentials = Arc::clone(&credentials);
            exit_tasks.spawn(async move {
                place_indicator_exit(
                    &cloned,
                    &user_config,
                    trade_id,
                    snapshot_id,
                    candle_time,
                    &cloned_credentials,
                )
                .await
            });
        }
    }
    while let Some(result) = exit_tasks.join_next().await {
        if let Err(error) = result.unwrap_or_else(|error| Err(AppError::Internal(error.into()))) {
            tracing::warn!(%error, instrument=%config.instrument, "Ichimoku indicator exit failed");
        }
    }

    if let Some(signal) = signal {
        let spot = candles[index].close_price;
        // Contract discovery and quote selection is shared by this configuration group.
        // User-specific risk reservations and orders still execute independently below.
        let contract =
            execution_contract(&state, &credentials, &config, signal, &underlying, spot).await?;
        let mut entry_tasks = JoinSet::new();
        for user_config in configs {
            let cloned = state.clone();
            let cloned_underlying = underlying.clone();
            let cloned_contract = contract.clone();
            entry_tasks.spawn(async move {
                place_entry(
                    &cloned,
                    user_config,
                    signal,
                    candle_time,
                    cloned_underlying,
                    cloned_contract,
                )
                .await
            });
        }
        while let Some(result) = entry_tasks.join_next().await {
            if let Err(error) = result.unwrap_or_else(|error| Err(AppError::Internal(error.into())))
            {
                tracing::warn!(%error, instrument=%config.instrument, "Ichimoku entry failed");
            }
        }
    }
    Ok(())
}

async fn poll_demo_prices(state: &AppState) -> AppResult<()> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT DISTINCT s.exchange_segment,s.contract_token FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE s.strategy_key=$1 AND o.execution_mode='demo' AND o.status='submitted' AND s.contract_token IS NOT NULL LIMIT 50")
        .bind(STRATEGY_KEY).fetch_all(&state.db).await?;
    if rows.is_empty() {
        return Ok(());
    }
    let (_, credentials) = shared_credentials(state).await?;
    let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
    for (exchange, token) in rows {
        grouped.entry(exchange).or_default().push(token);
    }
    let request = Value::Object(
        grouped
            .iter()
            .map(|(exchange, tokens)| (exchange.clone(), json!(tokens)))
            .collect(),
    );
    let response = quotes(state, &credentials, request).await?;
    let mut tasks = JoinSet::new();
    for row in quote_rows(&response) {
        let token = row
            .get("symbolToken")
            .or_else(|| row.get("symboltoken"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let ltp = numeric(row.get("ltp"));
        if let (Some(token), Some(ltp)) = (token, ltp) {
            let cloned = state.clone();
            tasks.spawn(
                async move { crate::strategy::process_tick_shared(&cloned, &token, ltp).await },
            );
        }
    }
    while let Some(result) = tasks.join_next().await {
        result.map_err(|error| AppError::Internal(error.into()))??;
    }
    Ok(())
}

pub fn start(state: AppState) {
    tokio::spawn(async move {
        let in_flight = Arc::new(Mutex::new(HashSet::<String>::new()));
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(5));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut last_demo_poll = std::time::Instant::now() - std::time::Duration::from_secs(2);
        loop {
            timer.tick().await;
            if last_demo_poll.elapsed() >= std::time::Duration::from_secs(2) {
                if let Err(error) = poll_demo_prices(&state).await {
                    tracing::warn!(%error, "Ichimoku demo price polling failed");
                }
                last_demo_poll = std::time::Instant::now();
            }
            let configs = match active_configs(&state).await {
                Ok(configs) => configs,
                Err(error) => {
                    tracing::warn!(%error, "could not load active Ichimoku configurations");
                    continue;
                }
            };
            let mut groups: HashMap<String, Vec<Config>> = HashMap::new();
            for config in configs {
                if configured_session_open(&config, ist_now()) {
                    groups
                        .entry(format!("{}:{}", config.instrument, config.variant_key()))
                        .or_default()
                        .push(config);
                }
            }
            for (key, configs) in groups {
                if !in_flight.lock().await.insert(key.clone()) {
                    continue;
                }
                let cloned = state.clone();
                let active = in_flight.clone();
                tokio::spawn(async move {
                    if let Err(error) = evaluate_group(cloned, configs).await {
                        tracing::warn!(%error, worker=%key, "Ichimoku worker evaluation failed");
                    }
                    active.lock().await.remove(&key);
                });
            }
        }
    });
}

fn instrument_label(instrument: &str) -> &'static str {
    match instrument {
        "NIFTY" => "NIFTY 50 Options",
        "BANKNIFTY" => "BANK NIFTY Options",
        "SENSEX" => "SENSEX Options",
        "MIDCAPNIFTY" => "NIFTY MID SELECT Options",
        "GOLDTEN" => "GOLDTEN Futures",
        _ => "Instrument",
    }
}

pub async fn catalog_item(state: &AppState, user_id: Uuid) -> AppResult<Value> {
    let active = crate::strategy::activation_state_for(state, user_id, STRATEGY_KEY).await?;
    let configs: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('instrument',instrument,'enabled',enabled,'lots',lots,'run_day_session',run_day_session,'run_evening_session',run_evening_session,'interval_key',interval_key,'stop_loss_percent',stop_loss_percent,'target_percent',target_percent,'keltner_multiplier',keltner_multiplier,'require_volume',require_volume,'premium_min',premium_min,'premium_max',premium_max) FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2")
        .bind(user_id).bind(STRATEGY_KEY).fetch_all(&state.db).await?;
    let by_instrument: HashMap<String, Value> = configs
        .into_iter()
        .filter_map(|value| {
            let key = value.get("instrument")?.as_str()?.to_string();
            Some((key, value))
        })
        .collect();
    let today = ist_now().date_naive();
    let mut preview_errors = HashMap::new();
    for instrument in INSTRUMENTS {
        let enabled = by_instrument
            .get(instrument)
            .and_then(|value| value.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        let has_current_contract: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_market_snapshots WHERE strategy_key=$1 AND instrument=$2 AND contract_expiry >= $3)")
            .bind(STRATEGY_KEY)
            .bind(instrument)
            .bind(today)
            .fetch_one(&state.db)
            .await?;
        if !has_current_contract
            && let Err(error) = refresh_contract_preview(state, instrument).await
        {
            preview_errors.insert(instrument.to_string(), error.to_string());
        }
    }
    let alerts: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'instrument',instrument,'severity',payload->>'severity','code',payload->>'code','message',payload->>'message','created_at',created_at) FROM strategy_events WHERE strategy_key=$1 AND event_type='operational_alert' AND (user_id=$2 OR user_id IS NULL) AND created_at>NOW()-INTERVAL '24 hours' ORDER BY created_at DESC LIMIT 5")
        .bind(STRATEGY_KEY).bind(user_id).fetch_all(&state.db).await?;
    let mut instruments = Vec::new();
    for instrument in INSTRUMENTS {
        let config = by_instrument.get(instrument);
        let snapshot: Option<Value> = sqlx::query_scalar("SELECT jsonb_build_object('contract_symbol',contract_symbol,'contract_token',contract_token,'contract_expiry',contract_expiry,'lot_size',lot_size,'exchange_segment',exchange_segment,'execution_key',execution_key,'fetched_at',fetched_at) FROM strategy_market_snapshots WHERE strategy_key=$1 AND instrument=$2 AND contract_expiry >= $3 ORDER BY (execution_key='catalog-preview') ASC,fetched_at DESC LIMIT 1")
            .bind(STRATEGY_KEY).bind(instrument).bind(today).fetch_optional(&state.db).await?;
        instruments.push(json!({
            "instrument":instrument,
            "label":instrument_label(instrument),
            "enabled":config.and_then(|value|value.get("enabled")).and_then(Value::as_bool).unwrap_or(false),
            "lots":config.and_then(|value|value.get("lots")).and_then(Value::as_i64).unwrap_or(1),
            "run_day_session":config.and_then(|value|value.get("run_day_session")).and_then(Value::as_bool).unwrap_or(true),
            "run_evening_session":config.and_then(|value|value.get("run_evening_session")).and_then(Value::as_bool).unwrap_or(instrument=="GOLDTEN"),
            "interval_key":config.and_then(|value|value.get("interval_key")).and_then(Value::as_str).unwrap_or("FIVE_MINUTE"),
            "stop_loss_percent":config.and_then(|value|value.get("stop_loss_percent")).and_then(Value::as_f64).unwrap_or(5.0),
            "target_percent":config.and_then(|value|value.get("target_percent")).and_then(Value::as_f64).unwrap_or(20.0),
            "keltner_multiplier":config.and_then(|value|value.get("keltner_multiplier")).and_then(Value::as_f64).unwrap_or(2.0),
            "require_volume":config.and_then(|value|value.get("require_volume")).and_then(Value::as_bool).unwrap_or(false),
            "premium_min":config.and_then(|value|value.get("premium_min")).and_then(Value::as_f64).unwrap_or(200.0),
            "premium_max":config.and_then(|value|value.get("premium_max")).and_then(Value::as_f64).unwrap_or(300.0),
            "snapshot":snapshot,
            "contract_error":preview_errors.get(instrument)
        }));
    }
    Ok(json!({
        "key":STRATEGY_KEY,
        "name":"Ichimoku + Keltner + TSI",
        "description":"Continuous multi-confirmation options strategy for NIFTY 50 and SENSEX.",
        "active":active,
        "operational_alerts":alerts,
        "scheduler_runs":[],
        "instruments":instruments
    }))
}

fn validate_update(input: &StrategyUpdate) -> AppResult<(String, String)> {
    let instrument = input.instrument.trim().to_uppercase();
    let interval = input
        .interval_key
        .as_deref()
        .unwrap_or("FIVE_MINUTE")
        .to_uppercase();
    let stop = input.stop_loss_percent.unwrap_or(5.0);
    let target = input.target_percent.unwrap_or(20.0);
    let keltner = input.keltner_multiplier.unwrap_or(2.0);
    let premium_min = input.premium_min.unwrap_or(200.0);
    let premium_max = input.premium_max.unwrap_or(300.0);
    if !INSTRUMENTS.contains(&instrument.as_str()) {
        return Err(AppError::BadRequest(
            "Unsupported Ichimoku instrument.".into(),
        ));
    }
    if !matches!(
        interval.as_str(),
        "ONE_MINUTE" | "FIVE_MINUTE" | "FIFTEEN_MINUTE"
    ) {
        return Err(AppError::BadRequest(
            "Live Ichimoku interval must be 1, 5, or 15 minutes.".into(),
        ));
    }
    if input.lots <= 0
        || !(0.01..=100.0).contains(&stop)
        || !(0.01..=500.0).contains(&target)
        || !(0.1..=10.0).contains(&keltner)
        || !premium_min.is_finite()
        || !premium_max.is_finite()
        || premium_min <= 0.0
        || premium_max <= premium_min
    {
        return Err(AppError::BadRequest(
            "Invalid Ichimoku size, exits, Keltner multiplier, or premium range.".into(),
        ));
    }
    Ok((instrument, interval))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
    Json(input): Json<StrategyUpdate>,
) -> AppResult<Json<Value>> {
    let (instrument, interval) = validate_update(&input)?;
    if input.enabled
        && !crate::strategy::activation_state_for(&state, auth.id, STRATEGY_KEY).await?
    {
        return Err(AppError::BadRequest(
            "Activate the strategy before enabling an instrument.".into(),
        ));
    }
    if input.enabled {
        refresh_contract_preview(&state, &instrument).await?;
    }
    sqlx::query("INSERT INTO user_strategy_configs (user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session,interval_key,stop_loss_percent,target_percent,keltner_multiplier,require_volume,premium_min,premium_max) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) ON CONFLICT (user_id,strategy_key,instrument) DO UPDATE SET enabled=EXCLUDED.enabled,lots=EXCLUDED.lots,run_day_session=EXCLUDED.run_day_session,run_evening_session=EXCLUDED.run_evening_session,interval_key=EXCLUDED.interval_key,stop_loss_percent=EXCLUDED.stop_loss_percent,target_percent=EXCLUDED.target_percent,keltner_multiplier=EXCLUDED.keltner_multiplier,require_volume=EXCLUDED.require_volume,premium_min=EXCLUDED.premium_min,premium_max=EXCLUDED.premium_max,updated_at=NOW()")
        .bind(auth.id).bind(STRATEGY_KEY).bind(&instrument).bind(input.enabled).bind(input.lots)
        .bind(input.run_day_session.unwrap_or(true)).bind(input.run_evening_session.unwrap_or(instrument=="GOLDTEN"))
        .bind(&interval).bind(input.stop_loss_percent.unwrap_or(5.0)).bind(input.target_percent.unwrap_or(20.0))
        .bind(input.keltner_multiplier.unwrap_or(2.0)).bind(input.require_volume.unwrap_or(false))
        .bind(input.premium_min.unwrap_or(200.0)).bind(input.premium_max.unwrap_or(300.0))
        .execute(&state.db).await?;
    crate::strategy::emit_for(
        &state,
        STRATEGY_KEY,
        Some(auth.id),
        &instrument,
        "configuration_updated",
        json!({"enabled":input.enabled,"lots":input.lots,"interval":interval}),
    )
    .await;
    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(&state, crate::audit::AuditEvent {
        context: request_context.as_ref(), headers: Some(&headers), event_type: "strategy_configuration_changed",
        actor_user_id: Some(auth.id), target_user_id: Some(auth.id), summary: "User changed Ichimoku strategy configuration",
        metadata: json!({"strategy_key":STRATEGY_KEY,"instrument":instrument,"enabled":input.enabled,"lots":input.lots,"interval":interval}),
    }).await { tracing::warn!(%error, "could not write Ichimoku configuration audit event"); }
    crate::strategy::catalog(State(state), Extension(auth)).await
}

async fn cancel_pending_entries(state: &AppState, user_id: Uuid) -> AppResult<()> {
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as("SELECT o.id,o.broker_order_id,o.execution_mode,o.order_type FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.user_id=$1 AND s.strategy_key=$2 AND o.role IN ('BUY_ENTRY','SELL_ENTRY') AND o.status IN ('pending','submitted')")
        .bind(user_id).bind(STRATEGY_KEY).fetch_all(&state.db).await?;
    for (id, broker_id, mode, order_type) in rows {
        if mode == "live" && !broker_id.is_empty() {
            let credentials = state.credentials.load(user_id).await?;
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
            .await?;
        }
        sqlx::query("UPDATE strategy_orders SET status='cancelled',broker_status='Strategy deactivated',updated_at=NOW() WHERE id=$1 AND status IN ('pending','submitted')").bind(id).execute(&state.db).await?;
    }
    Ok(())
}

pub async fn update_activation(
    state: AppState,
    auth: AuthUser,
    headers: HeaderMap,
    context: Option<Extension<crate::security::RequestContext>>,
    active: bool,
) -> AppResult<Json<Value>> {
    if !active {
        cancel_pending_entries(&state, auth.id).await?;
    }
    sqlx::query("INSERT INTO user_strategy_activations (user_id,strategy_key,is_active,activated_at,deactivated_at) VALUES ($1,$2,$3,CASE WHEN $3 THEN NOW() END,CASE WHEN $3 THEN NULL ELSE NOW() END) ON CONFLICT (user_id,strategy_key) DO UPDATE SET is_active=EXCLUDED.is_active,activated_at=CASE WHEN EXCLUDED.is_active THEN COALESCE(user_strategy_activations.activated_at,NOW()) ELSE user_strategy_activations.activated_at END,deactivated_at=CASE WHEN EXCLUDED.is_active THEN NULL ELSE NOW() END,updated_at=NOW()")
        .bind(auth.id).bind(STRATEGY_KEY).bind(active).execute(&state.db).await?;
    crate::strategy::emit_for(
        &state,
        STRATEGY_KEY,
        Some(auth.id),
        "",
        if active {
            "strategy_activated"
        } else {
            "strategy_deactivated"
        },
        json!({"active":active}),
    )
    .await;
    let request_context = crate::audit::optional_context(context);
    if let Err(error) = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: request_context.as_ref(),
            headers: Some(&headers),
            event_type: "strategy_activation_changed",
            actor_user_id: Some(auth.id),
            target_user_id: Some(auth.id),
            summary: "User changed Ichimoku strategy activation",
            metadata: json!({"strategy_key":STRATEGY_KEY,"active":active}),
        },
    )
    .await
    {
        tracing::warn!(%error, "could not write Ichimoku activation audit event");
    }
    crate::strategy::catalog(State(state), Extension(auth)).await
}

async fn protect_trade(
    state: &AppState,
    order: &StoredOrder,
    snapshot: &Snapshot,
    trade_id: Uuid,
    signal_direction: &str,
    actual_direction: &str,
    quantity: i32,
) -> AppResult<()> {
    if quantity <= 0 {
        return Ok(());
    }
    let runner = crate::strategy::runner_for_strategy(
        state,
        order.user_id,
        STRATEGY_KEY,
        &snapshot.instrument,
    )
    .await?;
    let lot_size = snapshot.lot_size.unwrap_or(1).max(1);
    let lots = ((quantity + lot_size - 1) / lot_size).max(1);
    let (target, stop) = if signal_direction == "BUY" {
        (snapshot.buy_target, snapshot.buy_sl1)
    } else {
        (snapshot.sell_target, snapshot.sell_sl1)
    };
    let protection_session = format!("{}-protect-{}", order.session_key, order.id);
    let exit_side = if actual_direction == "BUY" {
        "SELL"
    } else {
        "BUY"
    };
    crate::strategy::place_strategy_order(
        state,
        &runner,
        snapshot,
        &protection_session,
        NewOrder {
            role: "TARGET",
            side: exit_side,
            order_type: "LIMIT",
            lots,
            price: target
                .ok_or_else(|| AppError::BadRequest("Ichimoku target is missing.".into()))?,
            trigger: None,
            trade_id: Some(trade_id),
            quantity: Some(quantity),
        },
    )
    .await?;
    crate::strategy::place_strategy_order(
        state,
        &runner,
        snapshot,
        &protection_session,
        NewOrder {
            role: "SL1",
            side: exit_side,
            order_type: "STOPLOSS_LIMIT",
            lots,
            price: stop
                .ok_or_else(|| AppError::BadRequest("Ichimoku stop loss is missing.".into()))?,
            trigger: stop,
            trade_id: Some(trade_id),
            quantity: Some(quantity),
        },
    )
    .await
}

async fn user_log(state: &AppState, user_id: Uuid, message: &str) {
    if let Ok(Some(username)) =
        sqlx::query_scalar::<_, String>("SELECT username FROM users WHERE id=$1")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await
    {
        crate::logs::append(&username, message).await;
    }
}

fn pnl(direction: &str, entry: f64, exit: f64, quantity: i32) -> f64 {
    (if direction == "BUY" {
        exit - entry
    } else {
        entry - exit
    }) * quantity as f64
}

fn exit_levels(
    entry: f64,
    target_fraction: f64,
    stop_fraction: f64,
    long_option: bool,
) -> (f64, f64, f64, f64) {
    if long_option {
        (
            entry * (1.0 + target_fraction),
            entry * (1.0 - stop_fraction),
            entry * (1.0 + target_fraction),
            entry * (1.0 - stop_fraction),
        )
    } else {
        (
            entry * (1.0 + target_fraction),
            entry * (1.0 - stop_fraction),
            entry * (1.0 - target_fraction),
            entry * (1.0 + stop_fraction),
        )
    }
}

pub async fn complete_order(
    state: &AppState,
    order: StoredOrder,
    snapshot: Snapshot,
    fill: f64,
) -> AppResult<()> {
    match order.role.as_str() {
        "BUY_ENTRY" | "SELL_ENTRY" => {
            let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 AND status='open' ORDER BY entry_datetime DESC LIMIT 1")
                .bind(order.user_id).bind(STRATEGY_KEY).bind(&snapshot.instrument).fetch_optional(&state.db).await?;
            if let Some(trade_id) = existing {
                sqlx::query("UPDATE strategy_orders SET status='filled',trade_id=$2,processed_quantity=GREATEST(processed_quantity,$3),updated_at=NOW() WHERE id=$1")
                    .bind(order.id).bind(trade_id).bind(order.quantity).execute(&state.db).await?;
                return Ok(());
            }
            let signal_direction = if order.role == "BUY_ENTRY" {
                "BUY"
            } else {
                "SELL"
            };
            let actual_direction = order.side.as_str();
            let option_type = snapshot.contract_symbol.as_deref().and_then(|symbol| {
                if symbol.ends_with("CE") {
                    Some("CE")
                } else if symbol.ends_with("PE") {
                    Some("PE")
                } else {
                    None
                }
            });
            let (target, stop) = if signal_direction == "BUY" {
                (snapshot.buy_target, snapshot.buy_sl1)
            } else {
                (snapshot.sell_target, snapshot.sell_sl1)
            };
            let trade_id = Uuid::new_v4();
            let mut tx = state.db.begin().await?;
            if order.execution_mode == "demo" {
                sqlx::query("UPDATE user_profiles SET demo_balance=(GREATEST((demo_balance::float8-$2),0::numeric))::numeric,updated_at=NOW() WHERE user_id=$1")
                    .bind(order.user_id).bind(order.margin_required).execute(&mut *tx).await?;
            }
            sqlx::query("INSERT INTO trades (id,user_id,execution_mode,status,direction,signal_direction,option_type,underlying_token,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,external_entry_id,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,sl2_price,margin_required) SELECT $1,$2,execution_mode,'open',$3,$4,$5,$6,$7,($8::float8)::numeric,($8::float8)::numeric,0,NOW(),$9,$10,broker_order_id,'Ichimoku + Keltner + TSI',$11,$12,$13,$13,$14,$15,$15,$17 FROM strategy_orders WHERE id=$16")
                .bind(trade_id).bind(order.user_id).bind(actual_direction).bind(signal_direction).bind(option_type)
                .bind(&snapshot.underlying_token).bind(order.quantity).bind(fill).bind(&snapshot.instrument)
                .bind(snapshot.contract_symbol.as_deref().unwrap_or("")).bind(STRATEGY_KEY).bind(snapshot.id)
                .bind(order.lots).bind(target).bind(stop).bind(order.id).bind(order.margin_required)
                .execute(&mut *tx).await?;
            sqlx::query("UPDATE strategy_orders SET status='filled',trade_id=$2,processed_quantity=GREATEST(processed_quantity,$3),filled_quantity=GREATEST(filled_quantity,$3),updated_at=NOW() WHERE id=$1")
                .bind(order.id).bind(trade_id).bind(order.quantity).execute(&mut *tx).await?;
            tx.commit().await?;
            protect_trade(
                state,
                &order,
                &snapshot,
                trade_id,
                signal_direction,
                actual_direction,
                order.quantity,
            )
            .await?;
            crate::strategy::emit_for(state, STRATEGY_KEY, Some(order.user_id), &snapshot.instrument, "position_opened", json!({"trade_id":trade_id,"signal_direction":signal_direction,"contract_direction":actual_direction,"contract_symbol":snapshot.contract_symbol,"fill_price":fill,"quantity":order.quantity,"mode":order.execution_mode})).await;
            user_log(
                state,
                order.user_id,
                &format!(
                    "ICHIMOKU POSITION OPENED {} {} {} @ {:.2} [{}]",
                    snapshot.instrument,
                    signal_direction,
                    snapshot.contract_symbol.as_deref().unwrap_or(""),
                    fill,
                    order.execution_mode.to_uppercase()
                ),
            )
            .await;
        }
        "TARGET" | "SL1" | "SL2" => {
            let Some(trade_id) = order.trade_id else {
                return Ok(());
            };
            let trade: Option<(String, String, i32, f64, f64, f64)> = sqlx::query_as("SELECT direction,COALESCE(signal_direction,direction),quantity,entry_price::float8,pnl::float8,margin_required FROM trades WHERE id=$1 AND status='open'")
                .bind(trade_id).fetch_optional(&state.db).await?;
            let Some((direction, signal_direction, quantity, entry, accumulated, margin)) = trade
            else {
                sqlx::query("UPDATE strategy_orders SET status='filled',processed_quantity=GREATEST(processed_quantity,$2),updated_at=NOW() WHERE id=$1")
                    .bind(order.id).bind(order.quantity).execute(&state.db).await?;
                return Ok(());
            };
            crate::strategy::cancel_active_exits(state, order.user_id, trade_id).await?;
            let closed = order.quantity.min(quantity).max(0);
            let remaining = quantity - closed;
            let realized = pnl(&direction, entry, fill, closed);
            let release = if quantity > 0 {
                margin * closed as f64 / quantity as f64
            } else {
                0.0
            };
            let remaining_margin = (margin - release).max(0.0);
            let mut tx = state.db.begin().await?;
            if remaining == 0 {
                sqlx::query("WITH changed AS (UPDATE trades SET status='closed',quantity=0,remaining_lots=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=(pnl::float8+$3)::numeric,exit_datetime=NOW(),external_exit_id=$4,margin_required=0,updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$3+$5)::numeric,updated_at=NOW() FROM changed WHERE p.user_id=changed.user_id AND changed.execution_mode='demo'")
                    .bind(trade_id).bind(fill).bind(realized).bind(&order.broker_order_id).bind(release).execute(&mut *tx).await?;
            } else {
                let lot_size = snapshot.lot_size.unwrap_or(1).max(1);
                let lots = ((remaining + lot_size - 1) / lot_size).max(1);
                sqlx::query("WITH changed AS (UPDATE trades SET quantity=$2,remaining_lots=$3,last_price=($4::float8)::numeric,pnl=(pnl::float8+$5)::numeric,margin_required=$6,updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$5+$7)::numeric,updated_at=NOW() FROM changed WHERE p.user_id=changed.user_id AND changed.execution_mode='demo'")
                    .bind(trade_id).bind(remaining).bind(lots).bind(fill).bind(realized).bind(remaining_margin).bind(release).execute(&mut *tx).await?;
            }
            sqlx::query("UPDATE strategy_orders SET status='filled',processed_quantity=GREATEST(processed_quantity,$2),filled_quantity=GREATEST(filled_quantity,$2),updated_at=NOW() WHERE id=$1")
                .bind(order.id).bind(order.quantity).execute(&mut *tx).await?;
            tx.commit().await?;
            if remaining > 0 {
                protect_trade(
                    state,
                    &order,
                    &snapshot,
                    trade_id,
                    &signal_direction,
                    &direction,
                    remaining,
                )
                .await?;
            }
            crate::strategy::emit_for(state, STRATEGY_KEY, Some(order.user_id), &snapshot.instrument, if order.role == "TARGET" { "position_exit" } else { "stop_loss_filled" }, json!({"trade_id":trade_id,"fill_price":fill,"realized_pnl":realized,"remaining_quantity":remaining,"role":order.role})).await;
            user_log(
                state,
                order.user_id,
                &format!(
                    "ICHIMOKU {} {} @ {:.2} P&L {:+.2}; {} units remain",
                    order.role,
                    snapshot.instrument,
                    fill,
                    accumulated + realized,
                    remaining
                ),
            )
            .await;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(instrument: &str) -> StrategyUpdate {
        StrategyUpdate {
            instrument: instrument.into(),
            enabled: true,
            lots: 1,
            run_day_session: Some(true),
            run_evening_session: Some(false),
            interval_key: Some("FIVE_MINUTE".into()),
            stop_loss_percent: Some(5.0),
            target_percent: Some(20.0),
            keltner_multiplier: Some(2.0),
            require_volume: Some(false),
            premium_min: Some(200.0),
            premium_max: Some(300.0),
        }
    }

    fn option_contract(
        name: &str,
        symbol: &str,
        expiry: &str,
        lot_size: &str,
        exchange: &str,
    ) -> MasterContract {
        MasterContract {
            token: symbol.into(),
            symbol: symbol.into(),
            name: name.into(),
            expiry: expiry.into(),
            strike: "2500000".into(),
            lotsize: lot_size.into(),
            instrumenttype: "OPTIDX".into(),
            exch_seg: exchange.into(),
        }
    }

    fn ist(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).expect("valid test time")
    }

    #[test]
    fn index_and_gold_market_sessions_are_bounded() {
        assert!(!market_open("NIFTY", ist("2026-07-15T09:14:00+05:30")));
        assert!(market_open("NIFTY", ist("2026-07-15T09:15:00+05:30")));
        assert!(market_open("NIFTY", ist("2026-07-15T15:30:00+05:30")));
        assert!(!market_open("NIFTY", ist("2026-07-15T15:31:00+05:30")));
        assert!(market_open("GOLDTEN", ist("2026-07-15T23:30:00+05:30")));
        assert!(!market_open("GOLDTEN", ist("2026-07-18T10:00:00+05:30")));
    }

    #[test]
    fn live_configuration_rejects_unsafe_values() {
        assert!(validate_update(&update("nifty")).is_ok());
        assert!(validate_update(&update("SENSEX")).is_ok());
        assert!(validate_update(&update("BANKNIFTY")).is_err());
        assert!(validate_update(&update("MIDCAPNIFTY")).is_err());
        assert!(validate_update(&update("GOLDTEN")).is_err());
        let mut invalid = update("NIFTY");
        invalid.premium_max = Some(100.0);
        assert!(validate_update(&invalid).is_err());
        invalid = update("UNKNOWN");
        assert!(validate_update(&invalid).is_err());
        invalid = update("NIFTY");
        invalid.interval_key = Some("ONE_HOUR".into());
        assert!(validate_update(&invalid).is_err());
    }

    #[test]
    fn contract_preview_uses_nearest_current_expiry_and_broker_lot_size() {
        let master = vec![
            option_contract("NIFTY", "NIFTY30JUL26CE", "30JUL2026", "75", "NFO"),
            option_contract("NIFTY", "NIFTY23JUL26PE", "23JUL2026", "75", "NFO"),
            option_contract("NIFTY", "NIFTY23JUL26CE", "23JUL2026", "75", "NFO"),
        ];
        let (exchange, expiry, lot_size) = option_contract_preview(
            &master,
            "NIFTY",
            NaiveDate::from_ymd_opt(2026, 7, 18).unwrap(),
        )
        .unwrap();
        assert_eq!(exchange, "NFO");
        assert_eq!(expiry, NaiveDate::from_ymd_opt(2026, 7, 23).unwrap());
        assert_eq!(lot_size, 75);
        assert!(option_contract_preview(&master, "BANKNIFTY", expiry).is_err());
    }

    #[test]
    fn bearish_option_and_future_exits_use_correct_price_direction() {
        let (_, _, option_target, option_stop) = exit_levels(250.0, 0.20, 0.05, true);
        assert_eq!(option_target, 300.0);
        assert_eq!(option_stop, 237.5);

        let (_, _, future_target, future_stop) = exit_levels(100.0, 0.20, 0.05, false);
        assert_eq!(future_target, 80.0);
        assert_eq!(future_stop, 105.0);
        assert_eq!(pnl("SELL", 100.0, 80.0, 2), 40.0);
    }
}
