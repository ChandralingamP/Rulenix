use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, FromRow)]
pub struct AdminUser {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub can_administer: bool,
    pub can_live_trade: bool,
    pub can_backtest: bool,
    pub can_backtest_on_trading_days: bool,
    pub trading_mode: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub brokerage_user_id: Option<String>,
    pub broker_egress_ip_id: Option<Uuid>,
    pub broker_egress_ip: Option<String>,
    pub broker_egress_configuration_status: Option<String>,
    pub broker_egress_verification_status: Option<String>,
}

#[derive(Debug, FromRow)]
pub struct BrokerageProfile {
    pub user_id: Uuid,
    pub brokerage_user_id: String,
    pub broker_credential_revision: i64,
    pub token_state: String,
    pub token_received_at: Option<DateTime<Utc>>,
    pub last_token_check_at: Option<DateTime<Utc>>,
    pub last_token_status: String,
    pub last_token_message: String,
    pub updated_at: DateTime<Utc>,
}
