use crate::{error::AppResult, state::AppState};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    transport::smtp::authentication::Credentials,
};
use serde_json::{Value, json};

async fn send_email(
    state: &AppState,
    destination: &str,
    event_type: &str,
    severity: &str,
    payload: &Value,
) -> anyhow::Result<()> {
    let host = state
        .config
        .smtp_host
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("SMTP is not configured"))?;
    let message = Message::builder()
        .from(state.config.smtp_from.parse()?)
        .to(destination.parse()?)
        .subject(format!(
            "[Rulenix {}] {}",
            severity.to_uppercase(),
            event_type
        ))
        .body(format!(
            "Rulenix emitted an operational alert.\n\nEvent: {event_type}\nSeverity: {severity}\n\nPayload:\n{}\n",
            serde_json::to_string_pretty(payload)?
        ))?;
    let mut builder =
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?.port(state.config.smtp_port);
    if let (Some(username), Some(password)) =
        (&state.config.smtp_username, &state.config.smtp_password)
    {
        builder = builder.credentials(Credentials::new(username.clone(), password.clone()));
    }
    builder.build().send(message).await?;
    Ok(())
}

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
        let result = send_email(state, destination, event_type, severity, &payload).await;
        let (status, error) = match result {
            Ok(()) => ("sent", String::new()),
            Err(error) => ("failed", error.to_string()),
        };
        record_attempt(
            state,
            AlertAttempt {
                event_type,
                severity,
                channel: "email",
                destination,
                status,
                error: &error,
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
