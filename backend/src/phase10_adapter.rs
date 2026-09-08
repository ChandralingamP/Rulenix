//! Test-only JSON fixture adapter. It is compiled only with `phase10-adapter`.
//! It never initializes AppState, PostgreSQL, credentials, HTTP, or Angel.

use crate::strategy;
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, Read};

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
