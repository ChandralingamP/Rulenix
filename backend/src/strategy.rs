use crate::{
    angel,
    auth::AuthUser,
    error::{AppError, AppResult},
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
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveTime, TimeZone, Timelike, Utc,
    Weekday,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use std::collections::{HashMap, HashSet};
use tokio::time::{MissedTickBehavior, interval};
use uuid::Uuid;

pub const STRATEGY_KEY: &str = "futures_breakout_v3";
const SUPPORTED_INSTRUMENTS: [&str; 1] = ["GOLDTEN"];
const MASTER_URL: &str =
    "https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json";

#[derive(Debug, Clone, Deserialize)]
struct MasterContract {
    token: String,
    symbol: String,
    name: String,
    expiry: String,
    lotsize: String,
    instrumenttype: String,
    exch_seg: String,
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

fn snapshot_select() -> &'static str {
    "SELECT id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,candle_dates,highs,lows,hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2,fetched_at FROM strategy_market_snapshots"
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

async fn ensure_contract_metadata(
    state: &AppState,
    instrument: &str,
    date: NaiveDate,
) -> AppResult<Snapshot> {
    if let Some(snapshot) = load_snapshot(state, instrument, date).await?
        && snapshot.contract_token.is_some()
        && snapshot.contract_symbol.is_some()
        && snapshot.contract_expiry.is_some()
        && snapshot.lot_size.is_some()
    {
        return Ok(snapshot);
    }
    let contracts: Vec<MasterContract> = state
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
    let (contract, expiry) = select_contract(&contracts, instrument, date).ok_or_else(|| {
        AppError::BadRequest(format!(
            "No eligible MCX {instrument} FUTCOM contract is at least 10 trading days from expiry."
        ))
    })?;
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
        .filter(|value| *value > 0)
        .ok_or_else(|| AppError::BadRequest("Selected contract has an invalid lot size.".into()))?;
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size) VALUES ($1,$2,$3,$4,'missing','Daily market levels are pending.',$5,$6,$7,$8) ON CONFLICT (strategy_key,instrument,trade_date) DO UPDATE SET contract_token=EXCLUDED.contract_token,contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,error=CASE WHEN strategy_market_snapshots.status='ready' THEN strategy_market_snapshots.error ELSE EXCLUDED.error END,fetched_at=NOW()")
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
    .await?;
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
    sqlx::query("INSERT INTO strategy_market_snapshots (id,strategy_key,instrument,trade_date,status,error,contract_token,contract_symbol,contract_expiry,lot_size,candle_dates,highs,lows,hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,sell_sl2,fetched_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,NOW()) ON CONFLICT (strategy_key,instrument,trade_date) DO UPDATE SET status=EXCLUDED.status,error=EXCLUDED.error,contract_token=EXCLUDED.contract_token,contract_symbol=EXCLUDED.contract_symbol,contract_expiry=EXCLUDED.contract_expiry,lot_size=EXCLUDED.lot_size,candle_dates=EXCLUDED.candle_dates,highs=EXCLUDED.highs,lows=EXCLUDED.lows,hh2=EXCLUDED.hh2,ll2=EXCLUDED.ll2,hh4=EXCLUDED.hh4,ll4=EXCLUDED.ll4,buy_entry=EXCLUDED.buy_entry,buy_target=EXCLUDED.buy_target,buy_sl1=EXCLUDED.buy_sl1,buy_sl2=EXCLUDED.buy_sl2,sell_entry=EXCLUDED.sell_entry,sell_target=EXCLUDED.sell_target,sell_sl1=EXCLUDED.sell_sl1,sell_sl2=EXCLUDED.sell_sl2,fetched_at=NOW()")
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

async fn record_snapshot_failure(state: &AppState, instrument: &str, date: NaiveDate, error: &str) {
    if let Err(database_error) = sqlx::query("UPDATE strategy_market_snapshots SET status='failed',error=$4,fetched_at=NOW() WHERE strategy_key=$1 AND instrument=$2 AND trade_date=$3 AND status<>'ready'")
        .bind(STRATEGY_KEY).bind(instrument).bind(date).bind(error).execute(&state.db).await {
        tracing::warn!(%database_error, "could not persist market snapshot failure");
    }
}

#[derive(Debug, FromRow)]
struct Runner {
    user_id: Uuid,
    username: String,
    instrument: String,
    lots: i32,
    run_day_session: bool,
    run_evening_session: bool,
    trading_mode: String,
}

#[derive(Debug, Clone)]
struct NewOrder {
    role: &'static str,
    side: &'static str,
    lots: i32,
    price: f64,
    trigger: Option<f64>,
    trade_id: Option<Uuid>,
    quantity: Option<i32>,
}

type ProtectiveRetryRow = (
    Uuid,
    Uuid,
    Option<Uuid>,
    String,
    String,
    String,
    i32,
    i32,
    f64,
    Option<f64>,
);

async fn place_strategy_order(
    state: &AppState,
    runner: &Runner,
    snapshot: &Snapshot,
    session: &str,
    order: NewOrder,
) -> AppResult<()> {
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
    if runner.trading_mode == "live" && !protective {
        let credentials = state.credentials.load(runner.user_id).await?;
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
            idempotency_key: &key,
            snapshot_ready: snapshot.status == "ready",
            snapshot_current: snapshot.trade_date == ist_now().date_naive()
                && Utc::now() - snapshot.fetched_at < Duration::hours(26),
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
            operational_alert(
                state,
                Some(runner.user_id),
                &runner.instrument,
                "risk_rejected",
                "warning",
                &message,
            )
            .await;
            crate::logs::append(
                &runner.username,
                &format!("RISK REJECTED {} {}: {}", order.role, order.side, message),
            )
            .await;
            return Err(error);
        }
    };
    let Some(id) = active_id else {
        return Ok(());
    };
    let client_order_id = format!("RX{}", &id.simple().to_string()[..18]).to_uppercase();
    sqlx::query("UPDATE strategy_orders SET client_order_id=$2,updated_at=NOW() WHERE id=$1")
        .bind(id)
        .bind(&client_order_id)
        .execute(&state.db)
        .await?;
    let result = if runner.trading_mode == "live" {
        let claimed=sqlx::query("UPDATE strategy_orders SET status='submitting',submission_attempts=submission_attempts+1,state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status='pending'")
            .bind(id).execute(&state.db).await?;
        if claimed.rows_affected() == 0 {
            return Ok(());
        }
        sqlx::query("INSERT INTO broker_order_events(order_id,user_id,from_state,to_state,event_type,diagnostic) VALUES($1,$2,'pending','submitting','submission_started',$3)")
            .bind(id).bind(runner.user_id).bind(format!("client_order_id={client_order_id}")).execute(&state.db).await?;
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
            let order_type = if order.trigger.is_some() {
                "STOPLOSS_LIMIT"
            } else {
                "LIMIT"
            };
            angel::place_order(
                state,
                &credentials.api_key,
                &credentials.jwt_token,
                &angel::OrderRequest {
                    symbol,
                    token,
                    side: order.side,
                    order_type,
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
            emit(state, Some(runner.user_id), &runner.instrument, "order_submitted", json!({"order_id":id,"broker_order_id":broker_id,"role":order.role,"side":order.side,"price":order.price,"trigger_price":order.trigger,"lots":order.lots,"mode":runner.trading_mode})).await;
            crate::logs::append(
                &runner.username,
                &format!(
                    "STRATEGY {} {} {} lots @ {:.2}",
                    order.role, order.side, order.lots, order.price
                ),
            )
            .await;
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
            emit(
                state,
                Some(runner.user_id),
                &runner.instrument,
                "order_failed",
                json!({"order_id":id,"role":order.role,"error":error.to_string(),"classification":error.class}),
            )
            .await;
            operational_alert(
                state,
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
        crate::market_ws::ensure_strategy_feed(state.clone(), token).await;
    }
    place_strategy_order(
        state,
        runner,
        snapshot,
        session,
        NewOrder {
            role: "BUY_ENTRY",
            side: "BUY",
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

async fn runner_for(state: &AppState, user_id: Uuid, instrument: &str) -> AppResult<Runner> {
    Ok(sqlx::query_as("SELECT c.user_id,u.username,c.instrument,c.lots,c.run_day_session,c.run_evening_session,p.trading_mode FROM user_strategy_configs c JOIN users u ON u.id=c.user_id JOIN user_profiles p ON p.user_id=c.user_id WHERE c.user_id=$1 AND c.strategy_key=$2 AND c.instrument=$3 AND (p.trading_mode='demo' OR (p.trading_mode='live' AND u.can_live_trade=TRUE))")
        .bind(user_id).bind(STRATEGY_KEY).bind(instrument).fetch_one(&state.db).await?)
}

#[derive(Debug, FromRow)]
struct OpenTrade {
    id: Uuid,
    user_id: Uuid,
    direction: String,
    remaining_lots: i32,
    total_lots: i32,
    strategy_snapshot_id: Option<Uuid>,
    instrument_label: String,
}

async fn place_carry_orders(
    state: &AppState,
    date: NaiveDate,
    session: &str,
    role: &str,
    instrument: &str,
) -> AppResult<()> {
    let trades: Vec<OpenTrade> = sqlx::query_as("SELECT id,user_id,direction,remaining_lots,total_lots,strategy_snapshot_id,instrument_label FROM trades WHERE status='open' AND strategy_key=$1 AND instrument_label=$2 AND remaining_lots>0")
        .bind(STRATEGY_KEY).bind(instrument).fetch_all(&state.db).await?;
    let mut errors = Vec::new();
    for trade in trades {
        let Some(snapshot_id) = trade.strategy_snapshot_id else {
            continue;
        };
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await?;
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
                (trade.total_lots / 2 + 1).min(trade.total_lots)
            } else {
                trade.remaining_lots
            };
            if let Err(error) = place_strategy_order(
                state,
                &runner,
                &snapshot,
                &key,
                NewOrder {
                    role: if role == "TARGET" { "TARGET" } else { "SL2" },
                    side,
                    lots,
                    price,
                    trigger,
                    trade_id: Some(trade.id),
                    quantity: None,
                },
            )
            .await
            {
                errors.push(error.to_string());
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
        for instrument in SUPPORTED_INSTRUMENTS {
            if let Err(error) = ensure_contract_metadata(&state, instrument, startup_date).await {
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
                && let Err(error) = sqlx::query("UPDATE strategy_orders o SET status='cancelled',broker_status='DAY order expired',updated_at=NOW() FROM strategy_market_snapshots s WHERE s.id=o.snapshot_id AND s.trade_date<$1 AND o.status IN ('pending','submitted')")
                    .bind(date).execute(&state.db).await {
                tracing::warn!(%error, "could not expire prior-day strategy orders");
            }
            for instrument in SUPPORTED_INSTRUMENTS {
                if dispatched.insert(format!("{date}:contract:{instrument}")) {
                    let cloned = state.clone();
                    tokio::spawn(async move {
                        if let Err(error) =
                            ensure_contract_metadata(&cloned, instrument, date).await
                        {
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
                    });
                }
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
                for instrument in SUPPORTED_INSTRUMENTS {
                    let ready = load_snapshot(&state, instrument, date)
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|snapshot| snapshot.status == "ready");
                    if !ready
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
            for instrument in SUPPORTED_INSTRUMENTS {
                if let Err(error) = schedule_session(&state, now, instrument, "day", 9).await {
                    tracing::warn!(%instrument, %error, "day scheduler failed");
                }
                if let Err(error) = schedule_session(&state, now, instrument, "evening", 17).await {
                    tracing::warn!(%instrument, %error, "evening scheduler failed");
                }
            }
            let demo_tokens: Vec<String> = sqlx::query_scalar("SELECT DISTINCT s.contract_token FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.execution_mode='demo' AND o.status='submitted' AND s.contract_token IS NOT NULL")
                .fetch_all(&state.db).await.unwrap_or_default();
            for token in demo_tokens {
                crate::market_ws::ensure_strategy_feed(state.clone(), token).await;
            }
            if let Err(error) = retry_failed_protective_orders(&state).await {
                tracing::warn!(%error, "protective order recovery failed");
            }
            if let Err(error) = reconcile_live(&state).await {
                tracing::warn!(%error,"strategy order reconciliation failed");
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
        for instrument in SUPPORTED_INSTRUMENTS {
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
struct StoredOrder {
    id: Uuid,
    user_id: Uuid,
    snapshot_id: Uuid,
    trade_id: Option<Uuid>,
    session_key: String,
    role: String,
    side: String,
    execution_mode: String,
    lots: i32,
    quantity: i32,
    price: f64,
    broker_order_id: String,
    client_order_id: String,
    status: String,
    filled_quantity: i32,
}

async fn reconcile_live(state: &AppState) -> AppResult<()> {
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
                    "submitted" | "processing" | "filled" | "cancelled" | "cancelling"
                )
                | (
                    "processing",
                    "submitted" | "partially_filled" | "filled" | "cancelled" | "rejected"
                )
                | (
                    "cancelling",
                    "submitted" | "partially_filled" | "cancelled" | "processing"
                )
                | ("failed", "pending")
        )
}

fn reconciled_state(status: &str, filled: i32) -> &'static str {
    if matches!(status, "complete" | "completed" | "filled") {
        "submitted"
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
    let orders: Vec<StoredOrder>=sqlx::query_as("SELECT id,user_id,snapshot_id,trade_id,session_key,role,side,execution_mode,lots,quantity,price,broker_order_id,client_order_id,status,filled_quantity FROM strategy_orders WHERE user_id=$1 AND execution_mode='live' AND status IN ('submitting','ambiguous','submitted','partially_filled','processing','cancelling')")
            .bind(user_id).fetch_all(&state.db).await?;
    for order in orders {
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
        let filled = broker_i32(
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
        let price = broker_f64(item, &["averageprice", "averagePrice"])
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(order.price);
        let next = reconciled_state(&status, filled);
        let stored_next =
            if order.status == "cancelling" && matches!(next, "submitted" | "partially_filled") {
                "cancelling"
            } else if matches!(next, "rejected" | "cancelled") && filled > 0 {
                "submitted"
            } else {
                next
            };
        if !valid_order_transition(&order.status, stored_next) {
            operational_alert(
                state,
                Some(user_id),
                "",
                "invalid_order_transition",
                "error",
                &format!(
                    "Blocked invalid order transition {} -> {} for {}",
                    order.status, stored_next, order.id
                ),
            )
            .await;
            continue;
        }
        sqlx::query("UPDATE strategy_orders SET status=$2,broker_order_id=$3,filled_quantity=$4,average_fill_price=$5,last_reconciled_at=NOW(),broker_status=$6,state_version=state_version+1,updated_at=NOW() WHERE id=$1")
                .bind(order.id).bind(stored_next).bind(broker_id).bind(filled).bind(price).bind(format!("status={status}; filled_quantity={filled}; average_fill_price={price:.4}")).execute(&state.db).await?;
        sqlx::query("INSERT INTO broker_order_events(order_id,user_id,from_state,to_state,event_type,broker_order_id,diagnostic,broker_payload) VALUES($1,$2,$3,$4,'reconciled',$5,$6,$7)")
                .bind(order.id).bind(user_id).bind(&order.status).bind(next).bind(broker_id).bind(format!("broker_status={status}; filled={filled}/{}",order.quantity)).bind(json!({"status":status,"filled_quantity":filled,"average_fill_price":price,"broker_order_id":broker_id,"client_order_id":order.client_order_id})).execute(&state.db).await?;
        if next == "partially_filled" {
            if order.status == "cancelling" {
                continue;
            }
            let variety = if matches!(
                order.role.as_str(),
                "BUY_ENTRY" | "SELL_ENTRY" | "SL1" | "SL2"
            ) {
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
                    sqlx::query("UPDATE strategy_orders SET status='cancelling',broker_status='Partial fill detected; unfilled remainder cancellation requested.',state_version=state_version+1,updated_at=NOW() WHERE id=$1").bind(order.id).execute(&state.db).await?;
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
            continue;
        }
        if matches!(status.as_str(), "complete" | "completed" | "filled")
            || (matches!(next, "rejected" | "cancelled") && filled > 0)
        {
            let mut filled_order = order.clone();
            filled_order.broker_order_id = broker_id.to_string();
            filled_order.filled_quantity = filled;
            filled_order.quantity = filled;
            filled_order.lots = ((order.lots as i64 * filled as i64 + order.quantity as i64 - 1)
                / order.quantity as i64) as i32;
            filled_order.status = "submitted".into();
            if let Err(error) = complete_order(state, filled_order, price).await {
                operational_alert(
                    state,
                    Some(user_id),
                    "",
                    "fill_processing_failed",
                    "error",
                    &format!("Broker fill could not be processed; it will retry: {error}"),
                )
                .await;
            }
            if matches!(next, "rejected" | "cancelled") {
                sqlx::query("UPDATE strategy_orders SET status=$2,processed_quantity=$3,updated_at=NOW() WHERE id=$1").bind(order.id).bind(next).bind(filled).execute(&state.db).await?;
            }
        }
    }
    Ok(())
}

async fn retry_failed_protective_orders(state: &AppState) -> AppResult<()> {
    let orders: Vec<ProtectiveRetryRow> = sqlx::query_as("SELECT user_id,snapshot_id,trade_id,session_key,role,side,lots,quantity,price,trigger_price FROM strategy_orders WHERE execution_mode='live' AND status='failed' AND role IN ('TARGET','SL1','SL2') AND broker_order_id='' AND broker_error_class IN ('authentication','retryable') ORDER BY created_at LIMIT 100")
        .fetch_all(&state.db).await?;
    for (user_id, snapshot_id, trade_id, session, role, side, lots, quantity, price, trigger) in
        orders
    {
        let query = format!("{} WHERE id=$1", snapshot_select());
        let snapshot: Snapshot = sqlx::query_as(&query)
            .bind(snapshot_id)
            .fetch_one(&state.db)
            .await?;
        let runner = runner_for(state, user_id, &snapshot.instrument).await?;
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
        if let Err(error) = place_strategy_order(
            state,
            &runner,
            &snapshot,
            &session,
            NewOrder {
                role,
                side,
                lots,
                price,
                trigger,
                trade_id,
                quantity: Some(quantity),
            },
        )
        .await
        {
            operational_alert(
                state,
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

async fn cancel_active_exits(state: &AppState, user_id: Uuid, trade_id: Uuid) -> AppResult<()> {
    let credentials = state.credentials.load(user_id).await?;
    let orders:Vec<(Uuid,String,String)>=sqlx::query_as("SELECT id,broker_order_id,role FROM strategy_orders WHERE trade_id=$1 AND role IN ('TARGET','SL1','SL2') AND status='submitted'").bind(trade_id).fetch_all(&state.db).await?;
    for (id, broker_id, role) in orders {
        if !broker_id.starts_with("DEMO-")
            && !credentials.api_key.is_empty()
            && !credentials.jwt_token.is_empty()
        {
            let variety = if role == "TARGET" {
                "NORMAL"
            } else {
                "STOPLOSS"
            };
            let _ = angel::cancel_order(
                state,
                &credentials.api_key,
                &credentials.jwt_token,
                &broker_id,
                variety,
            )
            .await;
        }
        sqlx::query("UPDATE strategy_orders SET status='cancelled',updated_at=NOW() WHERE id=$1 AND status='submitted'").bind(id).execute(&state.db).await?;
    }
    Ok(())
}

fn trade_pnl(direction: &str, entry: f64, exit: f64, quantity: i32) -> f64 {
    let movement = if direction == "BUY" {
        exit - entry
    } else {
        entry - exit
    };
    movement * quantity as f64
}

fn demo_margin_amount(quantity: i32, price: f64, margin_requirement_percent: f64) -> f64 {
    quantity as f64 * price * margin_requirement_percent / 100.0
}

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

async fn effective_demo_margin_requirement(state: &AppState, user_id: Uuid) -> AppResult<f64> {
    let percent: f64 = sqlx::query_scalar(
        "SELECT COALESCE(u.margin_requirement_percent,g.margin_requirement_percent,10.0)::float8 FROM risk_limits g LEFT JOIN risk_limits u ON u.user_id=$1 WHERE g.user_id IS NULL",
    )
    .bind(user_id)
    .fetch_one(&state.db)
    .await?;
    Ok(percent)
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

async fn complete_order(state: &AppState, order: StoredOrder, fill: f64) -> AppResult<()> {
    let claimed=sqlx::query("UPDATE strategy_orders SET status='processing',filled_price=$2,average_fill_price=$2,filled_quantity=GREATEST(filled_quantity,$3),filled_at=NOW(),state_version=state_version+1,updated_at=NOW() WHERE id=$1 AND status IN ('submitted','partially_filled') AND processed_quantity<$3")
        .bind(order.id).bind(fill).bind(order.quantity).execute(&state.db).await?;
    if claimed.rows_affected() == 0 {
        return Ok(());
    }
    let query = format!("{} WHERE id=$1", snapshot_select());
    let snapshot: Snapshot = sqlx::query_as(&query)
        .bind(order.snapshot_id)
        .fetch_one(&state.db)
        .await?;
    let instrument = snapshot.instrument.clone();
    match order.role.as_str() {
        "BUY_ENTRY" | "SELL_ENTRY" => {
            let direction = if order.role == "BUY_ENTRY" {
                "BUY"
            } else {
                "SELL"
            };
            if let Some(existing)=sqlx::query_as::<_,(Uuid,String,i32,f64)>("SELECT id,direction,quantity,entry_price::float8 FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 AND status='open' ORDER BY entry_datetime DESC LIMIT 1")
                .bind(order.user_id).bind(STRATEGY_KEY).bind(&instrument).fetch_optional(&state.db).await? {
                if existing.1!=direction {
                    cancel_active_exits(state,order.user_id,existing.0).await?;
                    let pnl=trade_pnl(&existing.1,existing.3,fill,existing.2);
                    let margin_requirement_percent = effective_demo_margin_requirement(state, order.user_id).await?;
                    let release_margin = demo_margin_release(existing.2, existing.3, margin_requirement_percent, existing.2);
                    sqlx::query("WITH closed AS (UPDATE trades SET status='closed',exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=($3::float8)::numeric,exit_datetime=NOW(),remaining_lots=0,notes=CONCAT(notes,'; SAR reversal'),updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$3+$4)::numeric,updated_at=NOW() FROM closed WHERE p.user_id=closed.user_id AND closed.execution_mode='demo'")
                        .bind(existing.0).bind(fill).bind(pnl).bind(release_margin).execute(&state.db).await?;
                    append_user_log(state, order.user_id, &format!("STRATEGY POSITION CLOSED {} SAR @ {:.2} P&L {:+.2}", instrument, fill, pnl)).await;
                } else {
                    sqlx::query("UPDATE strategy_orders SET status='filled',trade_id=$3,processed_quantity=GREATEST(processed_quantity,$2),updated_at=NOW() WHERE id=$1").bind(order.id).bind(order.quantity).bind(existing.0).execute(&state.db).await?;
                    return Ok(());
                }
            }
            let trade_id = Uuid::new_v4();
            let (target, sl1, sl2) = if direction == "BUY" {
                (snapshot.buy_target, snapshot.buy_sl1, snapshot.buy_sl2)
            } else {
                (snapshot.sell_target, snapshot.sell_sl1, snapshot.sell_sl2)
            };
            let margin_requirement_percent =
                effective_demo_margin_requirement(state, order.user_id).await?;
            let reserved_margin =
                demo_margin_amount(order.quantity, fill, margin_requirement_percent);
            let mut fill_tx = state.db.begin().await?;
            if order.execution_mode == "demo" {
                sqlx::query("UPDATE user_profiles SET demo_balance=(GREATEST((demo_balance::float8 - $2),0::numeric))::numeric,updated_at=NOW() WHERE user_id=$1")
                    .bind(order.user_id).bind(reserved_margin).execute(&mut *fill_tx).await?;
            }
            sqlx::query("INSERT INTO trades (id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,entry_datetime,instrument_label,contract_symbol,external_entry_id,notes,strategy_key,strategy_snapshot_id,total_lots,remaining_lots,target_price,sl1_price,sl2_price) SELECT $1,$2,execution_mode,'open',$3,$4,($5::float8)::numeric,($5::float8)::numeric,0,NOW(),$6,$7,broker_order_id,'Futures Breakout v3',$8,$9,$10,$10,$11,$12,$13 FROM strategy_orders WHERE id=$14")
                .bind(trade_id).bind(order.user_id).bind(direction).bind(order.quantity).bind(fill).bind(&instrument).bind(snapshot.contract_symbol.as_deref().unwrap_or(""))
                .bind(STRATEGY_KEY).bind(snapshot.id).bind(order.lots).bind(target).bind(sl1).bind(sl2).bind(order.id).execute(&mut *fill_tx).await?;
            sqlx::query("UPDATE strategy_orders SET status='filled',trade_id=$2,processed_quantity=GREATEST(processed_quantity,$3),updated_at=NOW() WHERE id=$1").bind(order.id).bind(trade_id).bind(order.quantity).execute(&mut *fill_tx).await?;
            fill_tx.commit().await?;
            let runner = runner_for(state, order.user_id, &instrument).await?;
            let close_lots = order.lots / 2 + 1;
            let exit_side = if direction == "BUY" { "SELL" } else { "BUY" };
            place_strategy_order(
                state,
                &runner,
                &snapshot,
                &order.session_key,
                NewOrder {
                    role: "TARGET",
                    side: exit_side,
                    lots: close_lots.min(order.lots),
                    price: target.unwrap(),
                    trigger: None,
                    trade_id: Some(trade_id),
                    quantity: Some(
                        (close_lots.min(order.lots) * snapshot.lot_size.unwrap_or(1))
                            .min(order.quantity),
                    ),
                },
            )
            .await?;
            place_strategy_order(
                state,
                &runner,
                &snapshot,
                &order.session_key,
                NewOrder {
                    role: "SL1",
                    side: exit_side,
                    lots: order.lots,
                    price: sl1.unwrap(),
                    trigger: sl1,
                    trade_id: Some(trade_id),
                    quantity: Some(order.quantity),
                },
            )
            .await?;
            emit(state,Some(order.user_id),&instrument,"position_opened",json!({"trade_id":trade_id,"direction":direction,"fill_price":fill,"lots":order.lots})).await;
            append_user_log(
                state,
                order.user_id,
                &format!(
                    "STRATEGY POSITION OPENED {} {} {} lots @ {:.2} [{}]",
                    instrument,
                    direction,
                    order.lots,
                    fill,
                    runner.trading_mode.to_uppercase()
                ),
            )
            .await;
        }
        "TARGET" => {
            if let Some(trade_id) = order.trade_id {
                let trade:(String,i32,i32,f64,f64)=sqlx::query_as("SELECT direction,total_lots,remaining_lots,entry_price::float8,pnl::float8 FROM trades WHERE id=$1").bind(trade_id).fetch_one(&state.db).await?;
                cancel_active_exits(state, order.user_id, trade_id).await?;
                let closed = order.lots.min(trade.2);
                let remaining = trade.2 - closed;
                let realized = trade_pnl(
                    &trade.0,
                    trade.3,
                    fill,
                    closed * snapshot.lot_size.unwrap_or(1),
                );
                let margin_requirement_percent =
                    effective_demo_margin_requirement(state, order.user_id).await?;
                let release_margin = demo_margin_release(
                    trade.1 * snapshot.lot_size.unwrap_or(1),
                    trade.3,
                    margin_requirement_percent,
                    closed * snapshot.lot_size.unwrap_or(1),
                );
                let mut fill_tx = state.db.begin().await?;
                if remaining == 0 {
                    sqlx::query("WITH closed AS (UPDATE trades SET status='closed',remaining_lots=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=(pnl::float8+$3)::numeric,exit_datetime=NOW(),updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$3+$4)::numeric,updated_at=NOW() FROM closed WHERE p.user_id=closed.user_id AND closed.execution_mode='demo'").bind(trade_id).bind(fill).bind(realized).bind(release_margin).execute(&mut *fill_tx).await?;
                } else {
                    sqlx::query("WITH reduced AS (UPDATE trades SET remaining_lots=$2,quantity=$3,last_price=($4::float8)::numeric,pnl=(pnl::float8+$5)::numeric,updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$5+$6)::numeric,updated_at=NOW() FROM reduced WHERE p.user_id=reduced.user_id AND reduced.execution_mode='demo'").bind(trade_id).bind(remaining).bind(remaining*snapshot.lot_size.unwrap_or(1)).bind(fill).bind(realized).bind(release_margin).execute(&mut *fill_tx).await?;
                }
                sqlx::query("UPDATE strategy_orders SET status='filled',processed_quantity=GREATEST(processed_quantity,$2),updated_at=NOW() WHERE id=$1").bind(order.id).bind(order.quantity).execute(&mut *fill_tx).await?;
                fill_tx.commit().await?;
                if remaining > 0 {
                    let runner = runner_for(state, order.user_id, &instrument).await?;
                    let sl2 = if trade.0 == "BUY" {
                        snapshot.buy_sl2
                    } else {
                        snapshot.sell_sl2
                    }
                    .unwrap();
                    let side = if trade.0 == "BUY" { "SELL" } else { "BUY" };
                    place_strategy_order(
                        state,
                        &runner,
                        &snapshot,
                        &order.session_key,
                        NewOrder {
                            role: "SL2",
                            side,
                            lots: remaining,
                            price: sl2,
                            trigger: Some(sl2),
                            trade_id: Some(trade_id),
                            quantity: Some(remaining * snapshot.lot_size.unwrap_or(1)),
                        },
                    )
                    .await?;
                }
                emit(state,Some(order.user_id),&instrument,"target_filled",json!({"trade_id":trade_id,"fill_price":fill,"closed_lots":closed,"remaining_lots":remaining})).await;
                append_user_log(state, order.user_id, &format!("STRATEGY TARGET FILLED {} {} lots @ {:.2} REALIZED P&L {:+.2}; {} lots remain", instrument, closed, fill, realized, remaining)).await;
            }
        }
        "SL1" | "SL2" => {
            if let Some(trade_id) = order.trade_id {
                let trade:(String,i32,i32,f64,f64)=sqlx::query_as("SELECT direction,quantity,remaining_lots,entry_price::float8,pnl::float8 FROM trades WHERE id=$1").bind(trade_id).fetch_one(&state.db).await?;
                cancel_active_exits(state, order.user_id, trade_id).await?;
                let closed_quantity = order.quantity.min(trade.1);
                let remaining_quantity = trade.1 - closed_quantity;
                let closed_lots = order.lots.min(trade.2);
                let remaining_lots = (trade.2 - closed_lots).max(0);
                let closing_pnl = trade_pnl(&trade.0, trade.3, fill, closed_quantity);
                let pnl = trade.4 + closing_pnl;
                let margin_requirement_percent =
                    effective_demo_margin_requirement(state, order.user_id).await?;
                let release_margin = demo_margin_release(
                    trade.1,
                    trade.3,
                    margin_requirement_percent,
                    closed_quantity,
                );
                let mut fill_tx = state.db.begin().await?;
                if remaining_quantity == 0 {
                    sqlx::query("WITH changed AS (UPDATE trades SET status='closed',quantity=0,remaining_lots=0,exit_price=($2::float8)::numeric,last_price=($2::float8)::numeric,pnl=($3::float8)::numeric,exit_datetime=NOW(),updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$4+$5)::numeric,updated_at=NOW() FROM changed WHERE p.user_id=changed.user_id AND changed.execution_mode='demo'").bind(trade_id).bind(fill).bind(pnl).bind(closing_pnl).bind(release_margin).execute(&mut *fill_tx).await?;
                } else {
                    sqlx::query("WITH changed AS (UPDATE trades SET quantity=$2,remaining_lots=$3,last_price=($4::float8)::numeric,pnl=($5::float8)::numeric,updated_at=NOW() WHERE id=$1 RETURNING user_id,execution_mode) UPDATE user_profiles p SET demo_balance=(p.demo_balance::float8+$6+$7)::numeric,updated_at=NOW() FROM changed WHERE p.user_id=changed.user_id AND changed.execution_mode='demo'").bind(trade_id).bind(remaining_quantity).bind(remaining_lots).bind(fill).bind(pnl).bind(closing_pnl).bind(release_margin).execute(&mut *fill_tx).await?;
                }
                sqlx::query("UPDATE strategy_orders SET status='filled',processed_quantity=GREATEST(processed_quantity,$2),updated_at=NOW() WHERE id=$1").bind(order.id).bind(order.quantity).execute(&mut *fill_tx).await?;
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
                        order.role, instrument, fill, pnl
                    ),
                )
                .await;
                if remaining_quantity > 0 {
                    let runner = runner_for(state, order.user_id, &instrument).await?;
                    let sl2 = if trade.0 == "BUY" {
                        snapshot.buy_sl2
                    } else {
                        snapshot.sell_sl2
                    }
                    .unwrap();
                    place_strategy_order(
                        state,
                        &runner,
                        &snapshot,
                        &order.session_key,
                        NewOrder {
                            role: "SL2",
                            side: if trade.0 == "BUY" { "SELL" } else { "BUY" },
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
        .bind(order.id).bind(order.quantity).execute(&state.db).await?;
    Ok(())
}

pub async fn process_tick(state: &AppState, user_id: Uuid, token: &str, ltp: f64) -> AppResult<()> {
    risk::record_tick(state, token, ltp).await?;
    sqlx::query("UPDATE trades t SET last_price=($3::float8)::numeric,updated_at=NOW() FROM strategy_market_snapshots s WHERE t.strategy_snapshot_id=s.id AND t.user_id=$1 AND t.execution_mode='demo' AND t.status='open' AND s.contract_token=$2")
        .bind(user_id).bind(token).bind(ltp).execute(&state.db).await?;
    let orders:Vec<StoredOrder>=sqlx::query_as("SELECT o.id,o.user_id,o.snapshot_id,o.trade_id,o.session_key,o.role,o.side,o.execution_mode,o.lots,o.quantity,o.price,o.broker_order_id,o.client_order_id,o.status,o.filled_quantity FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.user_id=$1 AND o.execution_mode='demo' AND o.status='submitted' AND s.contract_token=$2 ORDER BY o.created_at")
        .bind(user_id).bind(token).fetch_all(&state.db).await?;
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
            complete_order(state, order, ltp).await?;
        }
    }
    Ok(())
}

pub async fn process_tick_shared(state: &AppState, token: &str, ltp: f64) -> AppResult<()> {
    risk::record_tick(state, token, ltp).await?;
    let users: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT o.user_id FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.execution_mode='demo' AND o.status='submitted' AND s.contract_token=$1")
        .bind(token).fetch_all(&state.db).await?;
    for user in users {
        process_tick(state, user, token, ltp).await?;
    }
    Ok(())
}

pub async fn finish_kill_cancellations(
    state: &AppState,
    orders: Vec<(Uuid, Uuid, String, String, String)>,
) -> AppResult<()> {
    for (id, user_id, mode, broker_id, _role) in orders {
        if mode == "live" && !broker_id.is_empty() {
            let credentials = state.credentials.load(user_id).await?;
            if let Err(error) = angel::cancel_order(
                state,
                &credentials.api_key,
                &credentials.jwt_token,
                &broker_id,
                "STOPLOSS",
            )
            .await
            {
                sqlx::query("UPDATE strategy_orders SET status='submitted',broker_status=$2,updated_at=NOW() WHERE id=$1 AND status='cancelling'")
                    .bind(id).bind(format!("Kill-switch cancellation failed: {error}")).execute(&state.db).await?;
                operational_alert(state,Some(user_id),"","kill_switch_cancel_failed","error","An emergency entry cancellation was not confirmed by the broker; retry or review immediately.").await;
                continue;
            }
        }
        sqlx::query("UPDATE strategy_orders SET status='cancelled',updated_at=NOW() WHERE id=$1 AND status='cancelling'")
            .bind(id).execute(&state.db).await?;
    }
    Ok(())
}

async fn emit(
    state: &AppState,
    user_id: Option<Uuid>,
    instrument: &str,
    event_type: &str,
    payload: Value,
) {
    let envelope = json!({"type":event_type,"user_id":user_id,"strategy_key":STRATEGY_KEY,"instrument":instrument,"payload":payload,"created_at":Utc::now()});
    if let Err(error)=sqlx::query("INSERT INTO strategy_events (user_id,strategy_key,instrument,event_type,payload) VALUES ($1,$2,$3,$4,$5)").bind(user_id).bind(STRATEGY_KEY).bind(instrument).bind(event_type).bind(&payload).execute(&state.db).await { tracing::warn!(%error,"could not persist strategy event"); }
    let _ = state.strategy_events.send(envelope);
}

pub async fn operational_alert(
    state: &AppState,
    user_id: Option<Uuid>,
    instrument: &str,
    code: &str,
    severity: &str,
    message: &str,
) {
    let payload = json!({"code":code,"severity":severity,"message":message});
    let inserted: Result<Option<i64>, sqlx::Error> = sqlx::query_scalar("INSERT INTO strategy_events (user_id,strategy_key,instrument,event_type,payload) SELECT $1,$2,$3,'operational_alert',$4 WHERE NOT EXISTS (SELECT 1 FROM strategy_events WHERE user_id IS NOT DISTINCT FROM $1 AND strategy_key=$2 AND instrument=$3 AND event_type='operational_alert' AND payload->>'code'=$5 AND created_at>NOW()-INTERVAL '5 minutes') RETURNING id")
        .bind(user_id).bind(STRATEGY_KEY).bind(instrument).bind(&payload).bind(code)
        .fetch_optional(&state.db).await;
    match inserted {
        Ok(Some(_)) => {
            let envelope = json!({"type":"operational_alert","user_id":user_id,"strategy_key":STRATEGY_KEY,"instrument":instrument,"payload":payload,"created_at":Utc::now()});
            let _ = state.strategy_events.send(envelope);
            if let Err(error) = crate::alerts::deliver(
                state,
                code,
                severity,
                json!({"user_id":user_id,"strategy_key":STRATEGY_KEY,"instrument":instrument,"message":message}),
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
    Ok(sqlx::query_scalar(
        "SELECT is_active FROM user_strategy_activations WHERE user_id=$1 AND strategy_key=$2",
    )
    .bind(user)
    .bind(STRATEGY_KEY)
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
    let config: Option<(bool, i32, bool, bool)> = sqlx::query_as("SELECT enabled,lots,run_day_session,run_evening_session FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2 AND instrument='GOLDTEN'")
        .bind(user).bind(STRATEGY_KEY).fetch_optional(&state.db).await?;
    let snapshot = Some(ensure_contract_metadata(&state, "GOLDTEN", ist_now().date_naive()).await?);
    // The strategy card is a current-status surface, not an incident log. Keep the
    // complete event history in strategy_events/logs and return only the newest
    // recent alert here so resolved retries do not clutter the trading controls.
    let alerts: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'instrument',instrument,'severity',payload->>'severity','code',payload->>'code','message',payload->>'message','created_at',created_at) FROM strategy_events WHERE strategy_key=$1 AND event_type='operational_alert' AND (user_id=$2 OR user_id IS NULL) AND created_at>NOW()-INTERVAL '10 minutes' ORDER BY created_at DESC LIMIT 1")
        .bind(STRATEGY_KEY).bind(user).fetch_all(&state.db).await?;
    let runs: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('instrument',instrument,'session',session_key,'action',action,'status',status,'attempts',attempts,'scheduled_for',scheduled_for,'last_error',last_error,'updated_at',updated_at) FROM strategy_scheduler_runs WHERE strategy_key=$1 AND trade_date=$2 ORDER BY scheduled_for,action")
        .bind(STRATEGY_KEY).bind(ist_now().date_naive()).fetch_all(&state.db).await?;
    let instrument = config.unwrap_or((false, 1, true, true));
    Ok(Json(json!({"strategies":[{
        "key":STRATEGY_KEY,
        "name":"Futures Breakout v3",
        "description":"Four-day MCX futures breakout with stop-and-reverse trade management.",
        "active":active,
        "operational_alerts":alerts,
        "scheduler_runs":runs,
        "instruments":[{
            "instrument":"GOLDTEN",
            "label":"Gold Futures",
            "enabled":instrument.0,
            "lots":instrument.1,
            "run_day_session":instrument.2,
            "run_evening_session":instrument.3,
            "snapshot":snapshot
        }]
    }]})))
}

async fn cancel_pending_entries(state: &AppState, user: Uuid) -> AppResult<()> {
    let orders: Vec<(Uuid, String, String)> = sqlx::query_as("SELECT o.id,o.broker_order_id,o.execution_mode FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE o.user_id=$1 AND s.strategy_key=$2 AND o.role IN ('BUY_ENTRY','SELL_ENTRY') AND o.status='submitted'")
        .bind(user).bind(STRATEGY_KEY).fetch_all(&state.db).await?;
    let credentials = state.credentials.load(user).await?;
    for (id, broker_id, mode) in orders {
        if mode == "live"
            && !broker_id.is_empty()
            && let Err(error) = angel::cancel_order(
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
    if strategy_key != STRATEGY_KEY {
        return Err(AppError::NotFound("Strategy not found.".into()));
    }
    let user = auth.id;
    if !input.active {
        cancel_pending_entries(&state, user).await?;
    }
    sqlx::query("INSERT INTO user_strategy_activations (user_id,strategy_key,is_active,activated_at,deactivated_at) VALUES ($1,$2,$3,CASE WHEN $3 THEN NOW() END,CASE WHEN $3 THEN NULL ELSE NOW() END) ON CONFLICT (user_id,strategy_key) DO UPDATE SET is_active=EXCLUDED.is_active,activated_at=CASE WHEN EXCLUDED.is_active THEN COALESCE(user_strategy_activations.activated_at,NOW()) ELSE user_strategy_activations.activated_at END,deactivated_at=CASE WHEN EXCLUDED.is_active THEN NULL ELSE NOW() END,updated_at=NOW()")
        .bind(user).bind(STRATEGY_KEY).bind(input.active).execute(&state.db).await?;
    emit(
        &state,
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
            metadata: json!({"strategy_key":STRATEGY_KEY,"active":input.active}),
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
    let instrument = input
        .instrument
        .unwrap_or_else(|| "GOLDTEN".into())
        .trim()
        .to_uppercase();
    if instrument.is_empty() || instrument.len() > 32 {
        return Err(AppError::BadRequest("Invalid instrument.".into()));
    }
    if input.enabled && !activation_state(&state, user).await? {
        return Err(AppError::BadRequest(
            "Activate the strategy before enabling an instrument.".into(),
        ));
    }
    sqlx::query("INSERT INTO user_strategy_configs (user_id,strategy_key,instrument,enabled,lots,run_day_session,run_evening_session) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (user_id,strategy_key,instrument) DO UPDATE SET enabled=EXCLUDED.enabled,lots=EXCLUDED.lots,run_day_session=EXCLUDED.run_day_session,run_evening_session=EXCLUDED.run_evening_session,updated_at=NOW()")
        .bind(user).bind(STRATEGY_KEY).bind(&instrument).bind(input.enabled).bind(input.lots).bind(input.run_day_session.unwrap_or(true)).bind(input.run_evening_session.unwrap_or(true)).execute(&state.db).await?;
    emit(
        &state,
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
            metadata: json!({"strategy_key":STRATEGY_KEY,"instrument":&instrument,"enabled":input.enabled,"lots":input.lots}),
        },
    )
    .await
    {
        tracing::warn!(%error, "could not write strategy configuration audit event");
    }
    status(
        State(state),
        Extension(auth),
        Query(StrategyQuery {
            instrument: Some(instrument),
        }),
    )
    .await
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
    let config:Option<(bool,i32,bool,bool)>=sqlx::query_as("SELECT enabled,lots,run_day_session,run_evening_session FROM user_strategy_configs WHERE user_id=$1 AND strategy_key=$2 AND instrument=$3").bind(user).bind(STRATEGY_KEY).bind(&instrument).fetch_optional(&state.db).await?;
    let strategy_active = activation_state(&state, user).await?;
    let snapshot = load_snapshot(&state, &instrument, ist_now().date_naive()).await?;
    let orders:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'role',role,'side',side,'status',status,'lots',lots,'quantity',quantity,'price',price,'trigger_price',trigger_price,'client_order_id',client_order_id,'broker_order_id',broker_order_id,'filled_quantity',filled_quantity,'average_fill_price',average_fill_price,'broker_error_class',broker_error_class,'broker_error_code',broker_error_code,'broker_http_status',broker_http_status,'last_reconciled_at',last_reconciled_at,'created_at',created_at) FROM strategy_orders WHERE user_id=$1 ORDER BY created_at DESC LIMIT 100").bind(user).fetch_all(&state.db).await?;
    let trades:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'status',status,'direction',direction,'lots',total_lots,'remaining_lots',remaining_lots,'quantity',quantity,'entry_price',entry_price,'exit_price',exit_price,'pnl',pnl,'trigger_time',entry_datetime,'exit_time',exit_datetime,'contract_symbol',contract_symbol,'target',target_price,'sl1',sl1_price,'sl2',sl2_price) FROM trades WHERE user_id=$1 AND strategy_key=$2 AND instrument_label=$3 ORDER BY created_at DESC LIMIT 100").bind(user).bind(STRATEGY_KEY).bind(&instrument).fetch_all(&state.db).await?;
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
    fn contract(expiry: &str) -> MasterContract {
        MasterContract {
            token: "1".into(),
            symbol: format!("GOLDTEN{expiry}FUT"),
            name: "GOLDTEN".into(),
            expiry: expiry.into(),
            lotsize: "1".into(),
            instrumenttype: "FUTCOM".into(),
            exch_seg: "MCX".into(),
        }
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
    fn target_lot_split() {
        for (lots, closed) in [(1, 1), (2, 2), (3, 2), (4, 3), (5, 3), (6, 4)] {
            assert_eq!((lots / 2 + 1).min(lots), closed);
        }
    }
    #[test]
    fn demo_margin_is_calculated_from_quantity_and_price() {
        assert_eq!(demo_margin_amount(100, 200.0, 10.0), 2000.0);
        assert_eq!(demo_margin_release(100, 200.0, 10.0, 25), 500.0);
    }

    #[test]
    fn pnl_supports_long_and_short_positions() {
        assert_eq!(trade_pnl("BUY", 100.0, 112.5, 4), 50.0);
        assert_eq!(trade_pnl("SELL", 100.0, 87.5, 4), 50.0);
        assert_eq!(trade_pnl("BUY", 100.0, 87.5, 4), -50.0);
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
        assert!(valid_order_transition("processing", "filled"));
        assert!(!valid_order_transition("filled", "submitting"));
        assert!(!valid_order_transition("cancelled", "processing"));
        assert!(!valid_order_transition("rejected", "pending"));
    }
    #[test]
    fn reconciliation_maps_partial_and_terminal_broker_states() {
        assert_eq!(reconciled_state("open", 0), "submitted");
        assert_eq!(reconciled_state("open", 2), "partially_filled");
        assert_eq!(reconciled_state("complete", 2), "submitted");
        assert_eq!(reconciled_state("rejected", 0), "rejected");
        assert_eq!(reconciled_state("canceled", 1), "cancelled");
    }
}
