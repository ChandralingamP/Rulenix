//! Test-only JSON fixture adapter. It is compiled only with `phase10-adapter`.
//! It never initializes AppState, PostgreSQL, credentials, HTTP, or Angel.

use crate::strategy;
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use std::io::{self, Read};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
struct Envelope {
    fixture_id: String,
    category: String,
    operation: String,
    request: Value,
}

pub(crate) async fn run() -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let envelope: Envelope = serde_json::from_str(&input).context("invalid phase10 fixture JSON")?;
    let result = match envelope.operation.as_str() {
        "future_breakout" => future_breakout(&envelope.request),
        "supertrend" => supertrend(&envelope.request),
        "tick" => tick(&envelope.request),
        "db_state" => state_fixture(&envelope.request).await,
        operation => anyhow::bail!("unsupported phase10 operation: {operation}"),
    };
    let (status, body) = match result {
        Ok(body) => (200, body),
        Err(error) => (422, json!({"code": "fixture_error", "message": error.to_string()})),
    };
    println!("{}", serde_json::to_string(&json!({
        "fixture_id": envelope.fixture_id,
        "category": envelope.category,
        "status": status,
        "body": body,
    }))?);
    Ok(())
}

const STATE_USER: Uuid = Uuid::from_u128(0x00000000000000000000000000000010);
const STATE_SIGNAL: Uuid = Uuid::from_u128(0x00000000000000000000000000000011);
const STATE_INTENT: Uuid = Uuid::from_u128(0x00000000000000000000000000000012);
const STATE_SNAPSHOT: Uuid = Uuid::from_u128(0x00000000000000000000000000000013);
const STATE_ORDER: Uuid = Uuid::from_u128(0x00000000000000000000000000000014);
const STATE_TRADE: Uuid = Uuid::from_u128(0x00000000000000000000000000000015);

async fn state_fixture(request: &Value) -> Result<Value> {
    let case = request.get("case").and_then(Value::as_str).context("missing state case")?;
    let raw_url = std::env::var("TEST_DATABASE_URL").context("TEST_DATABASE_URL is required")?;
    let url = raw_url
        .strip_prefix("postgresql+asyncpg://")
        .map_or(raw_url.clone(), |value| format!("postgresql://{value}"));
    let parsed = url::Url::parse(&url).context("invalid TEST_DATABASE_URL")?;
    let database = parsed.path().trim_start_matches('/');
    if !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1")) || !database.starts_with("rulenix_test_") {
        anyhow::bail!("state adapter requires a loopback rulenix_test_* database");
    }
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await?;
    reset_state_fixture(&pool).await?;
    seed_state_user(&pool).await?;
    let value = match case {
        "strategy_signal" => strategy_signal_state(&pool).await?,
        "execution_intent" => execution_intent_state(&pool).await?,
        "order_transition" => order_transition_state(&pool).await?,
        "partial_fill" => partial_fill_state(&pool).await?,
        "reversal_intent" => intent_state(&pool, "REVERSAL").await?,
        "manual_close_intent" => intent_state(&pool, "SQUARE_OFF").await?,
        "kill_switch" => kill_switch_state(&pool).await?,
        "readiness" => readiness_state(&pool).await?,
        _ => anyhow::bail!("unsupported state case: {case}"),
    };
    reset_state_fixture(&pool).await?;
    pool.close().await;
    Ok(value)
}

async fn reset_state_fixture(pool: &sqlx::PgPool) -> Result<()> {
    sqlx::query("DELETE FROM strategy_execution_intents WHERE id=$1 OR signal_id=$2 OR trade_id=$3")
        .bind(STATE_INTENT).bind(STATE_SIGNAL).bind(STATE_TRADE).execute(pool).await?;
    sqlx::query("DELETE FROM strategy_orders WHERE id=$1").bind(STATE_ORDER).execute(pool).await?;
    sqlx::query("DELETE FROM trades WHERE id=$1").bind(STATE_TRADE).execute(pool).await?;
    sqlx::query("DELETE FROM strategy_signals WHERE id=$1").bind(STATE_SIGNAL).execute(pool).await?;
    sqlx::query("DELETE FROM strategy_market_snapshots WHERE id=$1").bind(STATE_SNAPSHOT).execute(pool).await?;
    sqlx::query("DELETE FROM risk_kill_switches WHERE user_id=$1").bind(STATE_USER).execute(pool).await?;
    sqlx::query("DELETE FROM broker_reconciliation_health WHERE user_id=$1").bind(STATE_USER).execute(pool).await?;
    sqlx::query("DELETE FROM user_profiles WHERE user_id=$1").bind(STATE_USER).execute(pool).await?;
    sqlx::query("DELETE FROM users WHERE id=$1").bind(STATE_USER).execute(pool).await?;
    Ok(())
}

async fn seed_state_user(pool: &sqlx::PgPool) -> Result<()> {
    sqlx::query("INSERT INTO users(id,username,email,password_hash) VALUES($1,'phase10_state','phase10_state@example.test','fixture')")
        .bind(STATE_USER).execute(pool).await?;
    sqlx::query("INSERT INTO user_profiles(user_id,trading_mode) VALUES($1,'demo')")
        .bind(STATE_USER).execute(pool).await?;
    Ok(())
}

async fn seed_snapshot(pool: &sqlx::PgPool) -> Result<()> {
    sqlx::query("INSERT INTO strategy_market_snapshots(id,strategy_key,instrument,trade_date,status,error) VALUES($1,'futures_breakout_v3','GOLDTEN',CURRENT_DATE,'ready','')")
        .bind(STATE_SNAPSHOT).execute(pool).await?;
    Ok(())
}

async fn seed_signal(pool: &sqlx::PgPool) -> Result<()> {
    sqlx::query("INSERT INTO strategy_signals(id,strategy_key,instrument,session_key,signal_at,signal_type,status,expected_users,payload) VALUES($1,'futures_breakout_v3','GOLDTEN','phase10-state',NOW(),'ENTRY','confirmed',1,'{}')")
        .bind(STATE_SIGNAL).execute(pool).await?;
    Ok(())
}

async fn strategy_signal_state(pool: &sqlx::PgPool) -> Result<Value> {
    seed_signal(pool).await?;
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('id',id,'strategy_key',strategy_key,'instrument',instrument,'status',status,'expected_users',expected_users,'payload',payload) FROM strategy_signals WHERE id=$1")
        .bind(STATE_SIGNAL).fetch_one(pool).await?)
}

async fn execution_intent_state(pool: &sqlx::PgPool) -> Result<Value> {
    seed_signal(pool).await?;
    sqlx::query("INSERT INTO strategy_execution_intents(id,signal_id,user_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,status,last_error) VALUES($1,$2,$3,'futures_breakout_v3','GOLDTEN','phase10-state','ENTRY','BUY_ENTRY','BUY','LIMIT',1,1,110.132,'submitted','')")
        .bind(STATE_INTENT).bind(STATE_SIGNAL).bind(STATE_USER).execute(pool).await?;
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('id',id,'user_id',user_id,'action',action,'role',role,'side',side,'quantity',quantity,'price',price,'status',status,'attempts',attempts) FROM strategy_execution_intents WHERE id=$1")
        .bind(STATE_INTENT).fetch_one(pool).await?)
}

async fn order_transition_state(pool: &sqlx::PgPool) -> Result<Value> {
    seed_snapshot(pool).await?;
    sqlx::query("INSERT INTO strategy_orders(id,user_id,snapshot_id,session_key,role,side,execution_mode,lots,quantity,price,status,idempotency_key) VALUES($1,$2,$3,'phase10-state','BUY_ENTRY','BUY','demo',1,1,110.132,'pending','phase10-order')")
        .bind(STATE_ORDER).bind(STATE_USER).bind(STATE_SNAPSHOT).execute(pool).await?;
    sqlx::query("UPDATE strategy_orders SET status='submitted',broker_order_id='rust-state-order',updated_at=NOW() WHERE id=$1").bind(STATE_ORDER).execute(pool).await?;
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('id',id,'user_id',user_id,'side',side,'quantity',quantity,'price',price,'status',status,'broker_order_id',broker_order_id) FROM strategy_orders WHERE id=$1")
        .bind(STATE_ORDER).fetch_one(pool).await?)
}

async fn seed_trade(pool: &sqlx::PgPool) -> Result<()> {
    sqlx::query("INSERT INTO trades(id,user_id,execution_mode,status,direction,quantity,entry_price,last_price,pnl,instrument_label,contract_symbol,notes) VALUES($1,$2,'demo','open','BUY',10,100,100,0,'GOLDTEN','PHASE10','state fixture')")
        .bind(STATE_TRADE).bind(STATE_USER).execute(pool).await?;
    Ok(())
}

async fn partial_fill_state(pool: &sqlx::PgPool) -> Result<Value> {
    seed_trade(pool).await?;
    sqlx::query("UPDATE trades SET quantity=4,last_price=101,pnl=4,updated_at=NOW() WHERE id=$1").bind(STATE_TRADE).execute(pool).await?;
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('id',id,'user_id',user_id,'direction',direction,'quantity',quantity,'entry_price',entry_price,'last_price',last_price,'pnl',pnl,'status',status) FROM trades WHERE id=$1")
        .bind(STATE_TRADE).fetch_one(pool).await?)
}

async fn intent_state(pool: &sqlx::PgPool, action: &str) -> Result<Value> {
    seed_signal(pool).await?;
    seed_trade(pool).await?;
    sqlx::query("INSERT INTO strategy_execution_intents(id,signal_id,user_id,trade_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,status,last_error) VALUES($1,$2,$3,$4,'futures_breakout_v3','GOLDTEN','phase10-state',$5,'TARGET','SELL','MARKET',1,1,101,'pending','')")
        .bind(STATE_INTENT).bind(STATE_SIGNAL).bind(STATE_USER).bind(STATE_TRADE).bind(action).execute(pool).await?;
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('id',id,'user_id',user_id,'trade_id',trade_id,'action',action,'role',role,'side',side,'quantity',quantity,'status',status) FROM strategy_execution_intents WHERE id=$1")
        .bind(STATE_INTENT).fetch_one(pool).await?)
}

async fn kill_switch_state(pool: &sqlx::PgPool) -> Result<Value> {
    sqlx::query("INSERT INTO risk_kill_switches(user_id,enabled,reason,updated_by) VALUES($1,TRUE,'phase10 fixture',$1)").bind(STATE_USER).execute(pool).await?;
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('user_id',user_id,'enabled',enabled,'reason',reason,'updated_by',updated_by) FROM risk_kill_switches WHERE user_id=$1")
        .bind(STATE_USER).fetch_one(pool).await?)
}

async fn readiness_state(pool: &sqlx::PgPool) -> Result<Value> {
    sqlx::query("INSERT INTO broker_reconciliation_health(user_id,healthy,detail,checked_at) VALUES($1,FALSE,'phase10 fixture',NOW())").bind(STATE_USER).execute(pool).await?;
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('user_id',user_id,'healthy',healthy,'detail',detail) FROM broker_reconciliation_health WHERE user_id=$1")
        .bind(STATE_USER).fetch_one(pool).await?)
}

fn number_array(request: &Value, key: &str) -> Result<Vec<f64>> {
    Ok(request
        .get(key)
        .and_then(Value::as_array)
        .context(format!("missing {key}"))?
        .iter()
        .map(|value| value.as_f64().context(format!("invalid {key} value")))
        .collect::<Result<Vec<_>>>()?)
}

fn future_breakout(request: &Value) -> Result<Value> {
    let highs = number_array(request, "highs")?;
    let lows = number_array(request, "lows")?;
    let levels = strategy::calculate(&highs, &lows).context("insufficient breakout history")?;
    let mut result = json!({
        "hh2": levels.hh2,
        "ll2": levels.ll2,
        "hh4": levels.hh4,
        "ll4": levels.ll4,
        "buy_entry": levels.buy_entry,
        "buy_target": levels.buy_target,
        "buy_sl1": levels.buy_sl1,
        "buy_sl2": levels.buy_sl2,
        "sell_entry": levels.sell_entry,
        "sell_target": levels.sell_target,
        "sell_sl1": levels.sell_sl1,
        "sell_sl2": levels.sell_sl2,
    });
    if let Some(open) = request.get("market_open").and_then(Value::as_f64) {
        let plan = strategy::futures_missed_entry_plan(open, levels.buy_entry, levels.sell_entry)
            .context("invalid missed-entry request")?;
        result["missed_entry"] = json!(plan.as_str());
        result["buy_missed"] = json!(plan.buy_missed);
        result["sell_missed"] = json!(plan.sell_missed);
    }
    if let Some(direction) = request.get("direction").and_then(Value::as_str) {
        let entry = request.get("entry").and_then(Value::as_f64).context("missing entry")?;
        let exits = strategy::futures_exit_levels_for_entry(direction, entry, levels.hh2, levels.ll2, levels.hh4, levels.ll4)
            .context("invalid exit request")?;
        result["exit"] = json!({"target": exits.target, "sl1": exits.sl1, "sl2": exits.sl2});
    }
    Ok(result)
}

fn supertrend(request: &Value) -> Result<Value> {
    let candles = request
        .get("candles")
        .and_then(Value::as_array)
        .context("missing candles")?
        .iter()
        .map(|value| {
            Ok(strategy::IntradayCandle {
                at: NaiveDateTime::parse_from_str(value.get("at").and_then(Value::as_str).context("missing candle time")?, "%Y-%m-%dT%H:%M:%S")?,
                open: value.get("open").and_then(Value::as_f64).context("missing open")?,
                high: value.get("high").and_then(Value::as_f64).context("missing high")?,
                low: value.get("low").and_then(Value::as_f64).context("missing low")?,
                close: value.get("close").and_then(Value::as_f64).context("missing close")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let period = request.get("atr_period").and_then(Value::as_u64).unwrap_or(7) as usize;
    let factor = request.get("factor").and_then(Value::as_f64).unwrap_or(2.0);
    let points = strategy::supertrend_points(&candles, period, factor);
    let rendered = points.iter().map(|point| json!({
        "at": point.candle.at.format("%Y-%m-%dT%H:%M:%S").to_string(),
        "value": point.value,
        "direction": point.direction.as_str(),
    })).collect::<Vec<_>>();
    let mut result = json!({"points": rendered, "atr_period": period, "factor": factor});
    if let Some(value) = request.get("at").and_then(Value::as_str) {
        let at = DateTime::parse_from_rfc3339(value)?;
        result["entry_allowed"] = json!(strategy::supertrend_entry_allowed(at));
        result["eod_due"] = json!(strategy::option_square_off_due(at));
    }
    Ok(result)
}

fn tick(request: &Value) -> Result<Value> {
    let price = request.get("price").and_then(Value::as_f64).context("missing price")?;
    let tick = request.get("tick_size").and_then(Value::as_f64).context("missing tick_size")?;
    let side = request.get("side").and_then(Value::as_str).context("missing side")?;
    Ok(json!({"normalized": strategy::normalize_to_tick(price, tick, side)}))
}
