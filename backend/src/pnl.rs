use crate::{
    auth::AuthUser,
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    Json,
    body::Body,
    extract::{Extension, Query, State},
    http::{Response, header},
};
use chrono::{DateTime, Utc};
use rust_xlsxwriter::Workbook;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PnlQuery {
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub mode: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct TradeRow {
    pub id: Uuid,
    pub execution_mode: String,
    pub status: String,
    pub direction: String,
    pub quantity: i32,
    pub entry_price: Option<f64>,
    pub exit_price: Option<f64>,
    pub last_price: Option<f64>,
    pub pnl: f64,
    pub pnl_realtime: f64,
    pub entry_datetime: Option<DateTime<Utc>>,
    pub exit_datetime: Option<DateTime<Utc>>,
    pub instrument_label: String,
    pub contract_symbol: String,
    pub notes: String,
}

fn validate(query: &PnlQuery) -> AppResult<(i64, i64, String)> {
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).clamp(1, 200);
    let mode = query.mode.clone().unwrap_or_else(|| "all".into());
    if !matches!(mode.as_str(), "all" | "demo" | "live") {
        return Err(AppError::BadRequest("Invalid execution mode.".into()));
    }
    Ok((page, page_size, mode))
}

async fn rows(
    state: &AppState,
    user_id: Uuid,
    mode: &str,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<TradeRow>> {
    Ok(sqlx::query_as("SELECT id,execution_mode,status,direction,quantity,entry_price::float8 AS entry_price,exit_price::float8 AS exit_price,last_price::float8 AS last_price,pnl::float8 AS pnl,CASE WHEN status='open' AND entry_price IS NOT NULL THEN pnl::float8+(CASE WHEN direction='BUY' THEN COALESCE(last_price,entry_price)-entry_price ELSE entry_price-COALESCE(last_price,entry_price) END)*quantity ELSE pnl::float8 END AS pnl_realtime,entry_datetime,exit_datetime,instrument_label,contract_symbol,notes FROM trades WHERE user_id=$1 AND status IN ('open','closed') AND ($2='all' OR execution_mode=$2) ORDER BY exit_datetime DESC NULLS FIRST,entry_datetime DESC NULLS LAST LIMIT $3 OFFSET $4")
        .bind(user_id).bind(mode).bind(limit).bind(offset).fetch_all(&state.db).await?)
}

pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<PnlQuery>,
) -> AppResult<Json<Value>> {
    let user_id = user.id;
    let (page, page_size, mode) = validate(&query)?;
    let (count, total): (i64, f64) = sqlx::query_as("SELECT COUNT(*)::bigint,COALESCE(SUM(CASE WHEN status='open' AND entry_price IS NOT NULL THEN pnl::float8+(CASE WHEN direction='BUY' THEN COALESCE(last_price,entry_price)-entry_price ELSE entry_price-COALESCE(last_price,entry_price) END)*quantity ELSE pnl::float8 END),0)::float8 FROM trades WHERE user_id=$1 AND status IN ('open','closed') AND ($2='all' OR execution_mode=$2)")
        .bind(user_id).bind(&mode).fetch_one(&state.db).await?;
    let records = rows(&state, user_id, &mode, page_size, (page - 1) * page_size).await?;
    let total_pages = ((count + page_size - 1) / page_size).max(1);
    Ok(Json(
        json!({"results":records,"page":page,"page_size":page_size,"total_pages":total_pages,"total_records":count,"total_profit":total,"total_margin":0,"total_brokerage":0,"total_net_profit":total,"mode":mode}),
    ))
}

pub async fn export(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<PnlQuery>,
) -> AppResult<Response<Body>> {
    let user_id = user.id;
    let (_, _, mode) = validate(&query)?;
    let records = rows(&state, user_id, &mode, 100_000, 0).await?;
    if records.is_empty() {
        return Err(AppError::NotFound("No trades available for export.".into()));
    }
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    for (col, title) in [
        "#",
        "Entry Date",
        "Exit Date",
        "Instrument",
        "Symbol",
        "Mode",
        "Direction",
        "Quantity",
        "Entry @",
        "Exit @",
        "P/L",
        "Notes",
    ]
    .iter()
    .enumerate()
    {
        sheet
            .write_string(0, col as u16, *title)
            .map_err(|e| AppError::Internal(e.into()))?;
    }
    for (index, trade) in records.iter().enumerate() {
        let row = (index + 1) as u32;
        sheet
            .write_number(row, 0, (index + 1) as f64)
            .map_err(|e| AppError::Internal(e.into()))?;
        sheet
            .write_string(
                row,
                1,
                trade
                    .entry_datetime
                    .map(|v| v.to_rfc3339())
                    .unwrap_or_default(),
            )
            .map_err(|e| AppError::Internal(e.into()))?;
        sheet
            .write_string(
                row,
                2,
                trade
                    .exit_datetime
                    .map(|v| v.to_rfc3339())
                    .unwrap_or_default(),
            )
            .map_err(|e| AppError::Internal(e.into()))?;
        sheet
            .write_string(row, 3, &trade.instrument_label)
            .map_err(|e| AppError::Internal(e.into()))?;
        sheet
            .write_string(row, 4, &trade.contract_symbol)
            .map_err(|e| AppError::Internal(e.into()))?;
        sheet
            .write_string(row, 5, &trade.execution_mode)
            .map_err(|e| AppError::Internal(e.into()))?;
        sheet
            .write_string(row, 6, &trade.direction)
            .map_err(|e| AppError::Internal(e.into()))?;
        sheet
            .write_number(row, 7, trade.quantity as f64)
            .map_err(|e| AppError::Internal(e.into()))?;
        if let Some(v) = trade.entry_price {
            sheet
                .write_number(row, 8, v)
                .map_err(|e| AppError::Internal(e.into()))?;
        }
        if let Some(v) = trade.exit_price {
            sheet
                .write_number(row, 9, v)
                .map_err(|e| AppError::Internal(e.into()))?;
        }
        sheet
            .write_number(row, 10, trade.pnl_realtime)
            .map_err(|e| AppError::Internal(e.into()))?;
        sheet
            .write_string(row, 11, &trade.notes)
            .map_err(|e| AppError::Internal(e.into()))?;
    }
    let bytes = workbook
        .save_to_buffer()
        .map_err(|e| AppError::Internal(e.into()))?;
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        )
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=Rulenix-PnL.xlsx",
        )
        .body(Body::from(bytes))
        .map_err(|e| AppError::Internal(e.into()))
}
