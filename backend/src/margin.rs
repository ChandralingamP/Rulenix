use crate::{
    angel,
    error::{AppError, AppResult},
    state::AppState,
};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct MarginEstimate {
    pub margin_per_lot: f64,
    pub margin_required: f64,
}

fn numeric(value: Option<&Value>) -> Option<f64> {
    value.and_then(|item| {
        item.as_f64()
            .or_else(|| item.as_str().and_then(|text| text.parse::<f64>().ok()))
    })
}

fn parse_margin(value: &Value) -> Option<f64> {
    let direct = [
        "totalMarginRequired",
        "totalmarginrequired",
        "totalMargin",
        "requiredMargin",
        "marginRequired",
        "net",
    ]
    .iter()
    .find_map(|key| numeric(value.get(*key)));
    if direct.is_some() {
        return direct;
    }
    let data = value.get("data").unwrap_or(value);
    let data_direct = [
        "totalMarginRequired",
        "totalmarginrequired",
        "totalMargin",
        "requiredMargin",
        "marginRequired",
        "net",
    ]
    .iter()
    .find_map(|key| numeric(data.get(*key)));
    if data_direct.is_some() {
        return data_direct;
    }
    data.get("marginBreakup")
        .or_else(|| data.get("marginbreakup"))
        .and_then(|breakup| {
            [
                "totalMarginRequired",
                "totalmarginrequired",
                "totalMargin",
                "requiredMargin",
                "marginRequired",
                "net",
                "span",
            ]
            .iter()
            .find_map(|key| numeric(breakup.get(*key)))
        })
}

#[allow(clippy::too_many_arguments)]
pub async fn estimate(
    state: &AppState,
    user_id: Uuid,
    api_key: &str,
    jwt_token: &str,
    exchange: &str,
    product_type: &str,
    token: &str,
    symbol: &str,
    order_type: &str,
    trade_type: &str,
    lot_size: i32,
    lots: i32,
) -> AppResult<MarginEstimate> {
    if lot_size <= 0 || lots <= 0 {
        return Err(AppError::BadRequest("Invalid margin quantity.".into()));
    }
    let order_type = order_type.to_uppercase();
    let trade_type = trade_type.to_uppercase();
    let exchange = exchange.to_uppercase();
    let product_type = product_type.to_uppercase();
    if !matches!(trade_type.as_str(), "BUY" | "SELL") {
        return Err(AppError::BadRequest("Invalid margin side.".into()));
    }
    let cached: Option<f64> = sqlx::query_scalar("SELECT margin_per_lot FROM broker_margin_estimates WHERE exchange=$1 AND symbol_token=$2 AND product_type=$3 AND trade_type=$4 AND lot_size=$5 AND order_type=$6 AND fetched_at>NOW()-INTERVAL '1 day' ORDER BY fetched_at DESC LIMIT 1")
        .bind(&exchange).bind(token).bind(&product_type).bind(&trade_type).bind(lot_size).bind(&order_type).fetch_optional(&state.db).await?;
    if let Some(margin_per_lot) = cached.filter(|value| value.is_finite() && *value > 0.0) {
        return Ok(MarginEstimate {
            margin_per_lot,
            margin_required: margin_per_lot * lots as f64,
        });
    }

    let mut last_response = Value::Null;
    for _ in 0..2 {
        let response = match angel::calculate_margin(
            state,
            api_key,
            jwt_token,
            &exchange,
            &product_type,
            token,
            lot_size,
            &order_type,
            &trade_type,
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                return Err(AppError::BadRequest(format!(
                    "Unable to calculate broker margin for {symbol}. Please retry after reconnecting Angel One if this continues. ({error})"
                )));
            }
        };
        last_response = response.clone();
        if let Some(margin_per_lot) = parse_margin(&response).filter(|value| *value > 0.0) {
            let raw_response = serde_json::json!({
                "order_type":order_type,
                "broker_response":response
            });
            sqlx::query("INSERT INTO broker_margin_estimates (id,exchange,symbol_token,trading_symbol,product_type,order_type,trade_type,lot_size,margin_per_lot,raw_response,fetched_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT (exchange,symbol_token,product_type,order_type,trade_type,lot_size) DO UPDATE SET trading_symbol=EXCLUDED.trading_symbol,margin_per_lot=EXCLUDED.margin_per_lot,raw_response=EXCLUDED.raw_response,fetched_by=EXCLUDED.fetched_by,fetched_at=NOW()")
                .bind(Uuid::new_v4()).bind(&exchange).bind(token).bind(symbol).bind(&product_type).bind(&order_type).bind(&trade_type).bind(lot_size).bind(margin_per_lot).bind(&raw_response).bind(user_id)
                .execute(&state.db).await?;
            return Ok(MarginEstimate {
                margin_per_lot,
                margin_required: margin_per_lot * lots as f64,
            });
        }
    }

    Err(AppError::BadRequest(format!(
        "Angel margin calculator did not return a positive margin for {symbol} {trade_type}: {last_response}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_common_margin_payloads() {
        assert_eq!(
            parse_margin(&json!({"totalMarginRequired":"12345.50"})),
            Some(12345.50)
        );
        assert_eq!(
            parse_margin(&json!({"marginBreakup":{"totalMarginRequired":9876}})),
            Some(9876.0)
        );
    }
}
