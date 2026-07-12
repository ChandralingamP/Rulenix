use crate::{
    auth::{AuthUser, require_admin_permission},
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    Json,
    extract::{Extension, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerRequest {
    pub job_key: String,
}

const JOBS: [(&str, &str, &str, &str); 2] = [
    (
        "cleanup_otps",
        "Clean expired OTPs",
        "Deletes expired verification records.",
        "Daily at 00:00 IST",
    ),
    (
        "session_audit",
        "Audit broker sessions",
        "Marks stale Angel One sessions for reconnection.",
        "Every 30 minutes",
    ),
];

type LastRun = (
    String,
    chrono::DateTime<chrono::Utc>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<String>,
);

pub async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> AppResult<Json<Vec<Value>>> {
    require_admin_permission(&user)?;
    let mut result = Vec::new();
    for (key, label, description, schedule) in JOBS {
        let last: Option<LastRun> = sqlx::query_as("SELECT status,started_at,completed_at,error FROM job_runs WHERE job_key=$1 ORDER BY started_at DESC LIMIT 1").bind(key).fetch_optional(&state.db).await?;
        result.push(json!({"key":key,"label":label,"description":description,"schedule":schedule,"next_run":Value::Null,"last_run":last.map(|v|json!({"status":v.0,"started_at":v.1,"completed_at":v.2,"error":v.3}))}));
    }
    Ok(Json(result))
}

pub async fn trigger(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Json(input): Json<TriggerRequest>,
) -> AppResult<Json<Value>> {
    require_admin_permission(&user)?;
    if !JOBS.iter().any(|job| job.0 == input.job_key) {
        return Err(AppError::BadRequest("Unknown job key.".into()));
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO job_runs (id,job_key,status) VALUES ($1,$2,'running')")
        .bind(id)
        .bind(&input.job_key)
        .execute(&state.db)
        .await?;
    let db = state.db.clone();
    let key = input.job_key.clone();
    tokio::spawn(async move {
        let operation = match key.as_str() {
            "cleanup_otps" => sqlx::query("DELETE FROM email_otps WHERE expires_at < NOW() OR is_used=TRUE").execute(&db).await.map(|_|()),
            _ => sqlx::query("UPDATE user_profiles p SET token_state='stale' WHERE token_received_at < NOW()-INTERVAL '12 hours' AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='jwt_token')").execute(&db).await.map(|_|()),
        };
        match operation {
            Ok(_) => {
                let _ = sqlx::query(
                    "UPDATE job_runs SET status='completed',completed_at=NOW() WHERE id=$1",
                )
                .bind(id)
                .execute(&db)
                .await;
            }
            Err(error) => {
                let _ = sqlx::query(
                    "UPDATE job_runs SET status='failed',completed_at=NOW(),error=$2 WHERE id=$1",
                )
                .bind(id)
                .bind(error.to_string())
                .execute(&db)
                .await;
            }
        }
    });
    Ok(Json(json!({"detail":"Job triggered."})))
}
