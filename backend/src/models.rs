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
    pub trading_mode: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
pub struct BrokerageProfile {
    pub user_id: Uuid,
    pub brokerage_user_id: String,
    pub token_state: String,
    pub token_received_at: Option<DateTime<Utc>>,
    pub last_token_check_at: Option<DateTime<Utc>>,
    pub last_token_status: String,
    pub last_token_message: String,
    pub updated_at: DateTime<Utc>,
}
