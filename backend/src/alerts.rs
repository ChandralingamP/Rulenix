use crate::{error::AppResult, state::AppState};
use serde_json::{Value, json};

pub async fn deliver(
    state: &AppState,
    event_type: &str,
    severity: &str,
    payload: Value,
) -> AppResult<()> {
    if let Some(destination) = &state.config.alert_webhook_url {
        let result = state
            .http
            .post(destination)
            .json(&json!({
                "service":"rulenix",
                "event_type":event_type,
                "severity":severity,
                "payload":payload,
            }))
            .send()
            .await;
        match result {
            Ok(response) if response.status().is_success() => {
                record_attempt(
                    state,
                    AlertAttempt {
                        event_type,
                        severity,
                        channel: "webhook",
                        destination,
                        status: "sent",
                        error: "",
                        payload: &payload,
                    },
                )
                .await?;
            }
            Ok(response) => {
                let error = format!("webhook returned HTTP {}", response.status());
                record_attempt(
                    state,
                    AlertAttempt {
                        event_type,
                        severity,
                        channel: "webhook",
                        destination,
                        status: "failed",
                        error: &error,
                        payload: &payload,
                    },
                )
                .await?;
            }
            Err(error) => {
                let error = error.to_string();
                record_attempt(
                    state,
                    AlertAttempt {
                        event_type,
                        severity,
                        channel: "webhook",
                        destination,
                        status: "failed",
                        error: &error,
                        payload: &payload,
                    },
                )
                .await?;
            }
        }
    }
    if let Some(destination) = &state.config.alert_email_to {
        record_attempt(
            state,
            AlertAttempt {
                event_type,
                severity,
                channel: "email",
                destination,
                status: "skipped",
                error: "SMTP alert email rendering is documented; webhook delivery is active in this build",
                payload: &payload,
            },
        )
        .await?;
    }
    Ok(())
}

struct AlertAttempt<'a> {
    event_type: &'a str,
    severity: &'a str,
    channel: &'a str,
    destination: &'a str,
    status: &'a str,
    error: &'a str,
    payload: &'a Value,
}

async fn record_attempt(state: &AppState, attempt: AlertAttempt<'_>) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO alert_delivery_attempts (event_type,severity,channel,destination,status,error,payload) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(attempt.event_type)
    .bind(attempt.severity)
    .bind(attempt.channel)
    .bind(attempt.destination)
    .bind(attempt.status)
    .bind(attempt.error)
    .bind(attempt.payload)
    .execute(&state.db)
    .await?;
    Ok(())
}
