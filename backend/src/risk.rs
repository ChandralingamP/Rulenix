use crate::{
    auth::{AuthUser, require_admin_permission},
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Limits {
    pub max_lots: i32,
    pub max_quantity: i32,
    pub max_notional: f64,
    pub max_open_positions: i32,
    pub max_trades_per_day: i32,
    pub max_daily_realized_loss: f64,
    pub max_daily_unrealized_loss: f64,
    pub max_price_age_seconds: i32,
    pub margin_requirement_percent: f64,
}

#[derive(Debug)]
pub struct OrderRisk<'a> {
    pub user_id: Uuid,
    pub snapshot_id: Uuid,
    pub trade_id: Option<Uuid>,
    pub session: &'a str,
    pub role: &'a str,
    pub side: &'a str,
    pub mode: &'a str,
    pub lots: i32,
    pub quantity: i32,
    pub price: f64,
    pub trigger_price: Option<f64>,
    pub idempotency_key: &'a str,
    pub snapshot_ready: bool,
    pub snapshot_current: bool,
    pub contract_token: &'a str,
    pub live_margin_available: Option<f64>,
    pub live_reconciled: bool,
}

#[derive(Debug, Default)]
struct Metrics {
    lots: i64,
    quantity: i64,
    notional: f64,
    positions: i64,
    trades_today: i64,
    realized_pnl: f64,
    unrealized_pnl: f64,
}

fn reject(code: &'static str, message: &'static str) -> (&'static str, &'static str) {
    (code, message)
}

fn valid_order_values(lots: i32, quantity: i32, price: f64, trigger: Option<f64>) -> bool {
    lots > 0
        && quantity > 0
        && price.is_finite()
        && price > 0.0
        && trigger.is_none_or(|value| value.is_finite() && value > 0.0)
}

fn evaluate_limits(limits: &Limits, projected: &Metrics) -> Option<(&'static str, &'static str)> {
    if projected.lots > limits.max_lots as i64 {
        return Some(reject(
            "max_lots",
            "Order rejected: maximum lot exposure would be exceeded.",
        ));
    }
    if projected.quantity > limits.max_quantity as i64 {
        return Some(reject(
            "max_quantity",
            "Order rejected: maximum quantity exposure would be exceeded.",
        ));
    }
    if projected.notional > limits.max_notional {
        return Some(reject(
            "max_notional",
            "Order rejected: maximum notional exposure would be exceeded.",
        ));
    }
    if projected.positions > limits.max_open_positions as i64 {
        return Some(reject(
            "max_open_positions",
            "Order rejected: maximum open positions would be exceeded.",
        ));
    }
    if projected.trades_today > limits.max_trades_per_day as i64 {
        return Some(reject(
            "max_trades_per_day",
            "Order rejected: daily trade limit would be exceeded.",
        ));
    }
    if -projected.realized_pnl >= limits.max_daily_realized_loss {
        return Some(reject(
            "daily_realized_loss",
            "Order rejected: daily realized loss limit has been reached.",
        ));
    }
    if -projected.unrealized_pnl >= limits.max_daily_unrealized_loss {
        return Some(reject(
            "daily_unrealized_loss",
            "Order rejected: daily unrealized loss limit has been reached.",
        ));
    }
    None
}

async fn effective_limits(tx: &mut Transaction<'_, Postgres>, user_id: Uuid) -> AppResult<Limits> {
    Ok(sqlx::query_as(
        "SELECT COALESCE(u.max_lots,g.max_lots)::int4 max_lots,COALESCE(u.max_quantity,g.max_quantity)::int4 max_quantity,COALESCE(u.max_notional,g.max_notional)::float8 max_notional,COALESCE(u.max_open_positions,g.max_open_positions)::int4 max_open_positions,COALESCE(u.max_trades_per_day,g.max_trades_per_day)::int4 max_trades_per_day,COALESCE(u.max_daily_realized_loss,g.max_daily_realized_loss)::float8 max_daily_realized_loss,COALESCE(u.max_daily_unrealized_loss,g.max_daily_unrealized_loss)::float8 max_daily_unrealized_loss,COALESCE(u.max_price_age_seconds,g.max_price_age_seconds)::int4 max_price_age_seconds,COALESCE(u.margin_requirement_percent,g.margin_requirement_percent)::float8 margin_requirement_percent FROM risk_limits g LEFT JOIN risk_limits u ON u.user_id=$1 WHERE g.user_id IS NULL"
    ).bind(user_id).fetch_one(&mut **tx).await?)
}

async fn persist_decision(
    tx: &mut Transaction<'_, Postgres>,
    order: &OrderRisk<'_>,
    order_id: Uuid,
    allowed: bool,
    code: &str,
    message: &str,
    values: &Value,
) -> AppResult<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO risk_decisions (id,user_id,order_id,execution_mode,order_role,allowed,reason_code,message,values) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(id).bind(order.user_id).bind(order_id).bind(order.mode).bind(order.role)
        .bind(allowed).bind(code).bind(message).bind(values).execute(&mut **tx).await?;
    Ok(id)
}

/// Atomically assesses risk and reserves capacity by inserting the pending order in the
/// same user-scoped advisory-lock transaction. `None` means the idempotent order exists.
pub async fn assess_and_reserve(
    state: &AppState,
    order: &OrderRisk<'_>,
) -> AppResult<Option<Uuid>> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rulenix:risk:global'))")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,0))")
        .bind(order.user_id)
        .execute(&mut *tx)
        .await?;
    let existing: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id,status,broker_order_id FROM strategy_orders WHERE idempotency_key=$1",
    )
    .bind(order.idempotency_key)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((id, status, broker_id)) = existing {
        if status == "failed" && broker_id.is_empty() {
            sqlx::query("DELETE FROM strategy_orders WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        } else {
            tx.commit().await?;
            return Ok(None);
        }
    }

    let limits = effective_limits(&mut tx, order.user_id).await?;
    let protective = matches!(order.role, "TARGET" | "SL1" | "SL2");
    let order_id = Uuid::new_v4();
    let mut code = "allowed";
    let mut message = "Order passed all risk checks.";

    if !valid_order_values(order.lots, order.quantity, order.price, order.trigger_price) {
        (code, message) = reject(
            "invalid_order",
            "Order rejected: quantity, lots, and prices must be positive finite values.",
        );
    } else if !order.snapshot_ready || (!protective && !order.snapshot_current) {
        (code, message) = reject(
            "unsafe_snapshot",
            "Order rejected: the market snapshot is missing, stale, or unsafe.",
        );
    }

    let mut metrics = Metrics::default();
    let mut tick_age: Option<f64> = None;
    let mut global_kill = false;
    let mut user_kill = false;
    let mut account_safe = true;
    if code == "allowed" && !protective {
        let kills: (bool, bool) = sqlx::query_as("SELECT COALESCE((SELECT enabled FROM risk_kill_switches WHERE user_id IS NULL),FALSE),COALESCE((SELECT enabled FROM risk_kill_switches WHERE user_id=$1),FALSE)")
            .bind(order.user_id).fetch_one(&mut *tx).await?;
        (global_kill, user_kill) = kills;
        let account: Option<(bool, bool, String, String)> = sqlx::query_as("SELECT u.is_active,u.can_live_trade,COALESCE(p.trading_mode,'demo'),COALESCE(p.last_token_status,'') FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=$1")
            .bind(order.user_id).fetch_optional(&mut *tx).await?;
        account_safe = account.as_ref().is_some_and(|a| {
            a.0 && a.2 == order.mode
                && (order.mode == "demo"
                    || (a.1 && matches!(a.3.as_str(), "success" | "refreshed")))
        });
        if global_kill {
            (code, message) = reject(
                "global_kill_switch",
                "Order rejected: trading is paused by the global emergency kill switch.",
            );
        } else if user_kill {
            (code, message) = reject(
                "user_kill_switch",
                "Order rejected: trading is paused for this account.",
            );
        } else if !account_safe {
            (code, message) = reject(
                "unsafe_account",
                "Order rejected: account or broker session health is unsafe.",
            );
        }

        let tick: Option<(f64, DateTime<Utc>)> = sqlx::query_as(
            "SELECT price,received_at FROM market_price_ticks WHERE contract_token=$1",
        )
        .bind(order.contract_token)
        .fetch_optional(&mut *tx)
        .await?;
        tick_age = tick
            .as_ref()
            .map(|(_, at)| (Utc::now() - *at).num_milliseconds() as f64 / 1000.0);
        if code == "allowed"
            && tick.as_ref().is_none_or(|(price, _)| {
                !price.is_finite()
                    || *price <= 0.0
                    || tick_age.unwrap_or(f64::INFINITY) > limits.max_price_age_seconds as f64
            })
        {
            (code, message) = reject(
                "unsafe_market_feed",
                "Order rejected: no fresh valid market price is available.",
            );
        }
        if code == "allowed" && order.mode == "live" && !order.live_reconciled {
            (code, message) = reject(
                "broker_reconciliation",
                "Order rejected: broker reconciliation is not healthy.",
            );
        }

        let exposure: (i64,i64,f64,i64,i64,f64,f64) = sqlx::query_as(
            "WITH boundary AS (SELECT date_trunc('day',NOW() AT TIME ZONE 'Asia/Kolkata') AT TIME ZONE 'Asia/Kolkata' at), pending AS (SELECT COALESCE(SUM(lots),0)::bigint lots,COALESCE(SUM(quantity),0)::bigint quantity,COALESCE(SUM(quantity*price),0)::float8 notional,COUNT(*)::bigint positions FROM strategy_orders WHERE user_id=$1 AND role IN ('BUY_ENTRY','SELL_ENTRY') AND status IN ('pending','submitted')), open AS (SELECT COALESCE(SUM(total_lots),0)::bigint lots,COALESCE(SUM(quantity),0)::bigint quantity,COALESCE(SUM(quantity*COALESCE(last_price,entry_price)),0)::float8 notional,COUNT(*)::bigint positions,COALESCE(SUM(pnl+(CASE WHEN direction='BUY' THEN COALESCE(last_price,entry_price)-entry_price ELSE entry_price-COALESCE(last_price,entry_price) END)*quantity),0)::float8 unrealized FROM trades WHERE user_id=$1 AND status='open'), daily AS (SELECT (SELECT COUNT(*) FROM trades,boundary WHERE user_id=$1 AND entry_datetime>=boundary.at)::bigint trades,(SELECT COALESCE(SUM(pnl),0)::float8 FROM trades,boundary WHERE user_id=$1 AND status='closed' AND exit_datetime>=boundary.at) realized) SELECT pending.lots+open.lots,pending.quantity+open.quantity,pending.notional+open.notional,pending.positions+open.positions,daily.trades+(SELECT COUNT(*) FROM strategy_orders,boundary WHERE user_id=$1 AND role IN ('BUY_ENTRY','SELL_ENTRY') AND status IN ('pending','submitted') AND created_at>=boundary.at),daily.realized,open.unrealized FROM pending,open,daily"
        ).bind(order.user_id).fetch_one(&mut *tx).await?;
        metrics = Metrics {
            lots: exposure.0 + order.lots as i64,
            quantity: exposure.1 + order.quantity as i64,
            notional: exposure.2 + order.quantity as f64 * order.price,
            positions: exposure.3 + 1,
            trades_today: exposure.4 + 1,
            realized_pnl: exposure.5,
            unrealized_pnl: exposure.6,
        };
        if code == "allowed"
            && let Some(reason) = evaluate_limits(&limits, &metrics)
        {
            (code, message) = reason;
        }
        let required_margin =
            order.quantity as f64 * order.price * limits.margin_requirement_percent / 100.0;
        if code == "allowed"
            && order.mode == "live"
            && order
                .live_margin_available
                .is_none_or(|v| !v.is_finite() || v < required_margin)
        {
            (code, message) = reject(
                "insufficient_margin",
                "Order rejected: available broker margin is insufficient.",
            );
        }
    } else if code == "allowed" && protective {
        code = "protective_exit_allowed";
        message = "Protective exit passed safety validation.";
    }

    let values = json!({"limits":limits,"order":{"lots":order.lots,"quantity":order.quantity,"price":order.price,"role":order.role},"projected":{"lots":metrics.lots,"quantity":metrics.quantity,"notional":metrics.notional,"open_positions":metrics.positions,"trades_today":metrics.trades_today,"realized_pnl":metrics.realized_pnl,"unrealized_pnl":metrics.unrealized_pnl},"health":{"global_kill":global_kill,"user_kill":user_kill,"account_safe":account_safe,"snapshot_ready":order.snapshot_ready,"snapshot_current":order.snapshot_current,"market_price_age_seconds":tick_age,"broker_reconciled":order.live_reconciled,"margin_available":order.live_margin_available}});
    let allowed = matches!(code, "allowed" | "protective_exit_allowed");
    let decision_id =
        persist_decision(&mut tx, order, order_id, allowed, code, message, &values).await?;
    if allowed {
        sqlx::query("INSERT INTO strategy_orders (id,user_id,snapshot_id,trade_id,session_key,role,side,execution_mode,lots,quantity,price,trigger_price,status,idempotency_key,risk_decision_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'pending',$13,$14)")
            .bind(order_id).bind(order.user_id).bind(order.snapshot_id).bind(order.trade_id).bind(order.session).bind(order.role).bind(order.side).bind(order.mode).bind(order.lots).bind(order.quantity).bind(order.price).bind(order.trigger_price).bind(order.idempotency_key).bind(decision_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    if allowed {
        Ok(Some(order_id))
    } else {
        Err(AppError::Forbidden(message.into()))
    }
}

pub async fn record_tick(state: &AppState, token: &str, price: f64) -> AppResult<()> {
    if !price.is_finite() || price <= 0.0 {
        return Ok(());
    }
    sqlx::query("INSERT INTO market_price_ticks (contract_token,price,received_at) VALUES ($1,$2,NOW()) ON CONFLICT (contract_token) DO UPDATE SET price=EXCLUDED.price,received_at=NOW()")
        .bind(token).bind(price).execute(&state.db).await?;
    Ok(())
}

pub async fn set_reconciliation_health(
    state: &AppState,
    user: Uuid,
    healthy: bool,
    detail: &str,
) -> AppResult<()> {
    sqlx::query("INSERT INTO broker_reconciliation_health (user_id,healthy,detail,checked_at) VALUES ($1,$2,$3,NOW()) ON CONFLICT (user_id) DO UPDATE SET healthy=EXCLUDED.healthy,detail=EXCLUDED.detail,checked_at=NOW()")
        .bind(user).bind(healthy).bind(detail).execute(&state.db).await?;
    Ok(())
}

pub async fn cancel_pending_entries(
    state: &AppState,
    user_id: Option<Uuid>,
    reason: &str,
) -> AppResult<Vec<(Uuid, Uuid, String, String, String)>> {
    let rows=sqlx::query_as("UPDATE strategy_orders SET status='cancelling',broker_status=$2,updated_at=NOW() WHERE role IN ('BUY_ENTRY','SELL_ENTRY') AND status IN ('pending','submitted') AND ($1::uuid IS NULL OR user_id=$1) RETURNING id,user_id,execution_mode,broker_order_id,role")
        .bind(user_id).bind(reason).fetch_all(&state.db).await?;
    Ok(rows)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsUpdate {
    pub max_lots: Option<i32>,
    pub max_quantity: Option<i32>,
    pub max_notional: Option<f64>,
    pub max_open_positions: Option<i32>,
    pub max_trades_per_day: Option<i32>,
    pub max_daily_realized_loss: Option<f64>,
    pub max_daily_unrealized_loss: Option<f64>,
    pub max_price_age_seconds: Option<i32>,
    pub margin_requirement_percent: Option<f64>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KillUpdate {
    pub enabled: bool,
    pub reason: Option<String>,
}

pub async fn admin_status(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
) -> AppResult<Json<Value>> {
    require_admin_permission(&admin)?;
    let global: Value =
        sqlx::query_scalar("SELECT to_jsonb(r) FROM risk_limits r WHERE user_id IS NULL")
            .fetch_one(&state.db)
            .await?;
    let kill:Value=sqlx::query_scalar("SELECT jsonb_build_object('enabled',enabled,'reason',reason,'updated_at',updated_at) FROM risk_kill_switches WHERE user_id IS NULL").fetch_one(&state.db).await?;
    let users:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',u.id,'username',u.username,'limits',to_jsonb(l)-'user_id'-'updated_by','kill_switch',jsonb_build_object('enabled',COALESCE(k.enabled,FALSE),'reason',COALESCE(k.reason,''))) FROM users u LEFT JOIN risk_limits l ON l.user_id=u.id LEFT JOIN risk_kill_switches k ON k.user_id=u.id ORDER BY u.username").fetch_all(&state.db).await?;
    Ok(Json(
        json!({"global_limits":global,"global_kill_switch":kill,"users":users}),
    ))
}

async fn update_limits(
    state: &AppState,
    admin: Uuid,
    user: Option<Uuid>,
    v: LimitsUpdate,
) -> AppResult<()> {
    let vals = [
        v.max_lots.map(|x| x as f64),
        v.max_quantity.map(|x| x as f64),
        v.max_notional,
        v.max_open_positions.map(|x| x as f64),
        v.max_trades_per_day.map(|x| x as f64),
        v.max_daily_realized_loss,
        v.max_daily_unrealized_loss,
        v.max_price_age_seconds.map(|x| x as f64),
        v.margin_requirement_percent,
    ];
    if vals.iter().flatten().any(|x| !x.is_finite() || *x <= 0.0)
        || v.margin_requirement_percent.is_some_and(|x| x > 100.0)
    {
        return Err(AppError::BadRequest(
            "Risk limits must be positive finite values; margin percent cannot exceed 100.".into(),
        ));
    }
    if user.is_none() && vals.iter().any(Option::is_none) {
        return Err(AppError::BadRequest(
            "All global risk limits are required.".into(),
        ));
    }
    let mut tx = state.db.begin().await?;
    if let Some(user_id) = user {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,0))")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rulenix:risk:global'))")
            .execute(&mut *tx)
            .await?;
    }
    if user.is_some() {
        sqlx::query("INSERT INTO risk_limits (user_id,max_lots,max_quantity,max_notional,max_open_positions,max_trades_per_day,max_daily_realized_loss,max_daily_unrealized_loss,max_price_age_seconds,margin_requirement_percent,updated_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT (user_id) WHERE user_id IS NOT NULL DO UPDATE SET max_lots=EXCLUDED.max_lots,max_quantity=EXCLUDED.max_quantity,max_notional=EXCLUDED.max_notional,max_open_positions=EXCLUDED.max_open_positions,max_trades_per_day=EXCLUDED.max_trades_per_day,max_daily_realized_loss=EXCLUDED.max_daily_realized_loss,max_daily_unrealized_loss=EXCLUDED.max_daily_unrealized_loss,max_price_age_seconds=EXCLUDED.max_price_age_seconds,margin_requirement_percent=EXCLUDED.margin_requirement_percent,updated_by=EXCLUDED.updated_by,updated_at=NOW()")
        .bind(user).bind(v.max_lots).bind(v.max_quantity).bind(v.max_notional).bind(v.max_open_positions).bind(v.max_trades_per_day).bind(v.max_daily_realized_loss).bind(v.max_daily_unrealized_loss).bind(v.max_price_age_seconds).bind(v.margin_requirement_percent).bind(admin).execute(&mut *tx).await?;
    } else {
        sqlx::query("UPDATE risk_limits SET max_lots=$1,max_quantity=$2,max_notional=$3,max_open_positions=$4,max_trades_per_day=$5,max_daily_realized_loss=$6,max_daily_unrealized_loss=$7,max_price_age_seconds=$8,margin_requirement_percent=$9,updated_by=$10,updated_at=NOW() WHERE user_id IS NULL").bind(v.max_lots).bind(v.max_quantity).bind(v.max_notional).bind(v.max_open_positions).bind(v.max_trades_per_day).bind(v.max_daily_realized_loss).bind(v.max_daily_unrealized_loss).bind(v.max_price_age_seconds).bind(v.margin_requirement_percent).bind(admin).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
pub async fn update_global_limits(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
    Json(v): Json<LimitsUpdate>,
) -> AppResult<Json<Value>> {
    require_admin_permission(&admin)?;
    update_limits(&state, admin.id, None, v).await?;
    admin_status(State(state), Extension(admin)).await
}
pub async fn update_user_limits(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
    Path(user): Path<Uuid>,
    Json(v): Json<LimitsUpdate>,
) -> AppResult<Json<Value>> {
    require_admin_permission(&admin)?;
    update_limits(&state, admin.id, Some(user), v).await?;
    admin_status(State(state), Extension(admin)).await
}

pub async fn update_kill(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
    user: Option<Path<Uuid>>,
    Json(v): Json<KillUpdate>,
) -> AppResult<Json<Value>> {
    require_admin_permission(&admin)?;
    let user = user.map(|Path(v)| v);
    let reason = v.reason.unwrap_or_else(|| {
        if v.enabled {
            "Emergency trading pause".into()
        } else {
            "Cleared by staff".into()
        }
    });
    let mut tx = state.db.begin().await?;
    if let Some(user_id) = user {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,0))")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rulenix:risk:global'))")
            .execute(&mut *tx)
            .await?;
    }
    if user.is_none() {
        sqlx::query("UPDATE risk_kill_switches SET enabled=$1,reason=$2,updated_by=$3,updated_at=NOW() WHERE user_id IS NULL").bind(v.enabled).bind(&reason).bind(admin.id).execute(&mut *tx).await?;
    } else {
        sqlx::query("INSERT INTO risk_kill_switches(user_id,enabled,reason,updated_by) VALUES($1,$2,$3,$4) ON CONFLICT(user_id) WHERE user_id IS NOT NULL DO UPDATE SET enabled=EXCLUDED.enabled,reason=EXCLUDED.reason,updated_by=EXCLUDED.updated_by,updated_at=NOW() ").bind(user).bind(v.enabled).bind(&reason).bind(admin.id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    if v.enabled {
        let pending = cancel_pending_entries(&state, user, &reason).await?;
        crate::strategy::finish_kill_cancellations(&state, pending).await?;
    }
    admin_status(State(state), Extension(admin)).await
}
pub async fn update_global_kill(
    s: State<AppState>,
    a: Extension<AuthUser>,
    j: Json<KillUpdate>,
) -> AppResult<Json<Value>> {
    update_kill(s, a, None, j).await
}
pub async fn update_user_kill(
    s: State<AppState>,
    a: Extension<AuthUser>,
    p: Path<Uuid>,
    j: Json<KillUpdate>,
) -> AppResult<Json<Value>> {
    update_kill(s, a, Some(p), j).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn limits() -> Limits {
        Limits {
            max_lots: 2,
            max_quantity: 20,
            max_notional: 2000.0,
            max_open_positions: 2,
            max_trades_per_day: 2,
            max_daily_realized_loss: 100.0,
            max_daily_unrealized_loss: 100.0,
            max_price_age_seconds: 30,
            margin_requirement_percent: 10.0,
        }
    }

    #[test]
    fn boundaries_are_inclusive_for_capacity() {
        let mut m = Metrics {
            lots: 2,
            quantity: 20,
            notional: 2000.0,
            positions: 2,
            trades_today: 2,
            ..Default::default()
        };
        assert!(evaluate_limits(&limits(), &m).is_none());
        m.lots = 3;
        assert_eq!(evaluate_limits(&limits(), &m).unwrap().0, "max_lots");
    }

    #[test]
    fn every_exposure_limit_rejects_one_unit_over() {
        let cases = [
            (
                Metrics {
                    quantity: 21,
                    ..Default::default()
                },
                "max_quantity",
            ),
            (
                Metrics {
                    notional: 2000.01,
                    ..Default::default()
                },
                "max_notional",
            ),
            (
                Metrics {
                    positions: 3,
                    ..Default::default()
                },
                "max_open_positions",
            ),
            (
                Metrics {
                    trades_today: 3,
                    ..Default::default()
                },
                "max_trades_per_day",
            ),
        ];
        for (metrics, expected) in cases {
            assert_eq!(evaluate_limits(&limits(), &metrics).unwrap().0, expected);
        }
    }

    #[test]
    fn profits_and_sub_limit_losses_do_not_trip_daily_stops() {
        for pnl in [100.0, 0.0, -99.99] {
            let metrics = Metrics {
                realized_pnl: pnl,
                unrealized_pnl: pnl,
                ..Default::default()
            };
            assert!(evaluate_limits(&limits(), &metrics).is_none());
        }
    }

    #[test]
    fn loss_limits_block_at_the_boundary() {
        let realized = Metrics {
            realized_pnl: -100.0,
            ..Default::default()
        };
        assert_eq!(
            evaluate_limits(&limits(), &realized).unwrap().0,
            "daily_realized_loss"
        );
        let unrealized = Metrics {
            unrealized_pnl: -100.0,
            ..Default::default()
        };
        assert_eq!(
            evaluate_limits(&limits(), &unrealized).unwrap().0,
            "daily_unrealized_loss"
        );
    }

    #[test]
    fn invalid_prices_and_sizes_are_rejected() {
        for price in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(!valid_order_values(1, 1, price, None));
        }
        assert!(!valid_order_values(0, 1, 1.0, None));
        assert!(!valid_order_values(1, 0, 1.0, None));
        assert!(!valid_order_values(1, 1, 1.0, Some(f64::NAN)));
        assert!(valid_order_values(1, 1, 1.0, Some(1.0)));
    }

    #[tokio::test]
    async fn serialized_concurrent_reservations_cannot_cross_the_limit() {
        let reserved = Arc::new(Mutex::new(0_i64));
        let mut tasks = Vec::new();
        for _ in 0..100 {
            let reserved = reserved.clone();
            tasks.push(tokio::spawn(async move {
                let mut held = reserved.lock().await;
                let projected = Metrics {
                    lots: *held + 1,
                    ..Default::default()
                };
                if evaluate_limits(&limits(), &projected).is_none() {
                    *held += 1;
                    true
                } else {
                    false
                }
            }));
        }
        let mut accepted = 0;
        for task in tasks {
            accepted += usize::from(task.await.unwrap());
        }
        assert_eq!(accepted, 2);
        assert_eq!(*reserved.lock().await, 2);
    }
}
