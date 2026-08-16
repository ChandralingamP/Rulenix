use crate::{state::AppState, strategy};
use chrono::{DateTime, FixedOffset, Utc};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{MultiPart, SinglePart, header::ContentType},
    transport::smtp::authentication::Credentials,
};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, FromRow)]
struct TradeEmailRow {
    username: String,
    email: String,
    strategy_key: String,
    execution_mode: String,
    direction: String,
    quantity: i32,
    total_lots: i32,
    remaining_lots: i32,
    entry_price: f64,
    entry_datetime: Option<DateTime<Utc>>,
    instrument_label: String,
    contract_symbol: String,
    target_price: Option<f64>,
    sl1_price: Option<f64>,
    sl2_price: Option<f64>,
}

fn strategy_name(key: &str) -> &'static str {
    match key {
        strategy::STRATEGY_KEY => "Futures Breakout v3",
        strategy::OPTION_ENTRY_STRATEGY_KEY => "Option Entry v1",
        strategy::SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY => "SuperTrend Index Options v1",
        _ => "Rulenix Strategy",
    }
}

fn money(value: f64) -> String {
    format!("₹{value:.2}")
}

fn optional_money(value: Option<f64>) -> String {
    value.map(money).unwrap_or_else(|| "—".into())
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn entry_time_ist(value: Option<DateTime<Utc>>) -> String {
    let Some(value) = value else {
        return "—".into();
    };
    value
        .with_timezone(&FixedOffset::east_opt(19_800).expect("valid IST offset"))
        .format("%d %b %Y, %I:%M %p IST")
        .to_string()
}

fn text_body(row: &TradeEmailRow) -> String {
    format!(
        "Hi {username},\n\nYour Rulenix strategy has placed a trade.\n\nStrategy: {strategy}\nMode: {mode}\nContract: {contract}\nDirection: {direction}\nLots: {lots}\nQuantity: {quantity}\nEntry: {entry}\nTarget / TP: {target}\nSL1: {sl1}\nSL2: {sl2}\nTime: {time}\n\nRulenix will continue monitoring the trade based on the configured strategy rules.\n\n— Team Rulenix\n",
        username = row.username,
        strategy = strategy_name(&row.strategy_key),
        mode = row.execution_mode.to_uppercase(),
        contract = if row.contract_symbol.trim().is_empty() {
            row.instrument_label.as_str()
        } else {
            row.contract_symbol.as_str()
        },
        direction = row.direction,
        lots = row.total_lots.max(row.remaining_lots),
        quantity = row.quantity,
        entry = money(row.entry_price),
        target = optional_money(row.target_price),
        sl1 = optional_money(row.sl1_price),
        sl2 = optional_money(row.sl2_price),
        time = entry_time_ist(row.entry_datetime),
    )
}

fn html_row(label: &str, value: String) -> String {
    format!(
        "<tr><td style=\"padding:8px 12px;color:#64748b;border-bottom:1px solid #e2e8f0;\">{}</td><td style=\"padding:8px 12px;color:#0f172a;font-weight:600;border-bottom:1px solid #e2e8f0;\">{}</td></tr>",
        escape_html(label),
        escape_html(&value)
    )
}

fn html_body(row: &TradeEmailRow) -> String {
    let strategy = strategy_name(&row.strategy_key);
    let contract = if row.contract_symbol.trim().is_empty() {
        row.instrument_label.as_str()
    } else {
        row.contract_symbol.as_str()
    };
    let rows = [
        html_row("Strategy", strategy.into()),
        html_row("Mode", row.execution_mode.to_uppercase()),
        html_row("Contract", contract.into()),
        html_row("Direction", row.direction.clone()),
        html_row("Lots", row.total_lots.max(row.remaining_lots).to_string()),
        html_row("Quantity", row.quantity.to_string()),
        html_row("Entry", money(row.entry_price)),
        html_row("Target / TP", optional_money(row.target_price)),
        html_row("SL1", optional_money(row.sl1_price)),
        html_row("SL2", optional_money(row.sl2_price)),
        html_row("Time", entry_time_ist(row.entry_datetime)),
    ]
    .join("");
    format!(
        "<!doctype html><html><body style=\"margin:0;background:#f8fafc;font-family:Arial,sans-serif;color:#0f172a;\"><div style=\"max-width:640px;margin:0 auto;padding:28px 16px;\"><div style=\"background:#ffffff;border:1px solid #e2e8f0;border-radius:16px;overflow:hidden;\"><div style=\"background:#0f172a;color:#ffffff;padding:20px 24px;\"><div style=\"font-size:13px;letter-spacing:.08em;text-transform:uppercase;color:#93c5fd;\">Rulenix Trade Alert</div><h1 style=\"margin:8px 0 0;font-size:22px;line-height:1.3;\">Trade placed successfully</h1></div><div style=\"padding:24px;\"><p style=\"margin:0 0 16px;font-size:15px;line-height:1.6;\">Hi {username},</p><p style=\"margin:0 0 20px;font-size:15px;line-height:1.6;\">Your strategy has placed a new trade. Here are the details:</p><table role=\"presentation\" cellspacing=\"0\" cellpadding=\"0\" style=\"width:100%;border-collapse:collapse;border:1px solid #e2e8f0;border-radius:12px;overflow:hidden;\">{rows}</table><p style=\"margin:20px 0 0;font-size:14px;line-height:1.6;color:#475569;\">Rulenix will continue monitoring this trade based on the configured strategy rules.</p></div><div style=\"padding:14px 24px;background:#f1f5f9;color:#64748b;font-size:12px;line-height:1.5;\">Automated notification from Rulenix. This email is for trade tracking only.</div></div></div></body></html>",
        username = escape_html(&row.username),
        rows = rows
    )
}

async fn send_trade_opened_email(state: &AppState, trade_id: Uuid) -> anyhow::Result<()> {
    let Some(host) = &state.config.smtp_host else {
        tracing::info!(%trade_id, "SMTP not configured; skipping trade-opened email");
        return Ok(());
    };
    let row: TradeEmailRow = sqlx::query_as(
        "SELECT u.username,u.email,t.strategy_key,t.execution_mode,t.direction,t.quantity,t.total_lots,t.remaining_lots,t.entry_price::float8,t.entry_datetime,t.instrument_label,t.contract_symbol,t.target_price::float8,t.sl1_price::float8,t.sl2_price::float8
         FROM trades t
         JOIN users u ON u.id=t.user_id
         WHERE t.id=$1",
    )
    .bind(trade_id)
    .fetch_one(&state.db)
    .await?;
    let subject = format!(
        "Rulenix trade placed: {} {} {}",
        strategy_name(&row.strategy_key),
        row.instrument_label,
        row.direction
    );
    let message = Message::builder()
        .from(state.config.smtp_from.parse()?)
        .to(row.email.parse()?)
        .subject(subject)
        .multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::plain(text_body(&row)))
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_HTML)
                        .body(html_body(&row)),
                ),
        )?;
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

pub fn notify_trade_opened(state: AppState, trade_id: Uuid) {
    tokio::spawn(async move {
        if let Err(error) = send_trade_opened_email(&state, trade_id).await {
            tracing::warn!(%trade_id, %error, "trade-opened email notification failed");
        }
    });
}
