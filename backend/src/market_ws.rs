use crate::{
    auth::AuthUser,
    credentials::BrokerCredentials,
    error::{AppError, AppResult},
    models::BrokerageProfile,
    state::AppState,
};
use axum::{
    extract::{
        Extension, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
};
use chrono::{Datelike, FixedOffset, Timelike, Utc, Weekday};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
use tokio::time::{Duration, Instant, interval};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message as AngelMessage, client::IntoClientRequest},
};

fn ist_minute_of_day() -> Option<(Weekday, u32)> {
    let now = Utc::now().with_timezone(&FixedOffset::east_opt(19_800)?);
    Some((now.weekday(), now.hour() * 60 + now.minute()))
}

fn exchange_feed_expected(exchange: &str) -> bool {
    let Some((weekday, minute)) = ist_minute_of_day() else {
        return true;
    };
    if matches!(weekday, Weekday::Sat | Weekday::Sun) {
        return false;
    }
    match exchange.to_ascii_uppercase().as_str() {
        "NSE" | "NFO" | "BSE" | "BFO" => (9 * 60 + 15..=15 * 60 + 30).contains(&minute),
        "MCX" => (9 * 60..=23 * 60 + 30).contains(&minute),
        _ => true,
    }
}

fn stale_threshold(exchange: &str) -> Duration {
    if exchange.eq_ignore_ascii_case("MCX") {
        Duration::from_secs(120)
    } else {
        Duration::from_secs(45)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketQuery {
    pub tokens: String,
    pub exchange_type: Option<u8>,
    pub mode: Option<u8>,
}

pub async fn upgrade(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<MarketQuery>,
    ws: WebSocketUpgrade,
) -> AppResult<Response> {
    if query.tokens.split(',').all(|v| v.trim().is_empty()) {
        return Err(AppError::BadRequest(
            "At least one token is required.".into(),
        ));
    }
    let profile: BrokerageProfile = sqlx::query_as("SELECT * FROM user_profiles WHERE user_id=$1")
        .bind(user.id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User profile not found.".into()))?;
    let credentials = state.credentials.load(user.id).await?;
    if credentials.jwt_token.is_empty() || credentials.feed_token.is_empty() {
        return Err(AppError::Unauthorized(
            "Connect your Angel One session first.".into(),
        ));
    }
    Ok(ws.on_upgrade(move |socket| {
        bridge(socket, state, profile, credentials, query, user.username)
    }))
}

async fn bridge(
    mut browser: WebSocket,
    state: AppState,
    profile: BrokerageProfile,
    credentials: BrokerCredentials,
    query: MarketQuery,
    username: String,
) {
    crate::logs::append(&username, "MARKET DATA SESSION opened").await;
    if let Err(error) = run_bridge(&mut browser, state, profile, credentials, query).await {
        crate::logs::append(&username, &format!("MARKET DATA SESSION error: {error}")).await;
        let _ = browser
            .send(Message::Text(
                json!({"type":"error","detail":error.to_string()})
                    .to_string()
                    .into(),
            ))
            .await;
    }
    crate::logs::append(&username, "MARKET DATA SESSION closed").await;
}

async fn run_bridge(
    browser: &mut WebSocket,
    state: AppState,
    profile: BrokerageProfile,
    credentials: BrokerCredentials,
    query: MarketQuery,
) -> anyhow::Result<()> {
    let mut request = state.config.angel_ws_url.clone().into_client_request()?;
    let headers = request.headers_mut();
    headers.insert("Authorization", credentials.jwt_token.parse()?);
    headers.insert("x-api-key", credentials.api_key.parse()?);
    headers.insert("x-client-code", profile.brokerage_user_id.parse()?);
    headers.insert("x-feed-token", credentials.feed_token.parse()?);
    let (angel, _) = connect_async(request).await?;
    let (mut angel_tx, mut angel_rx) = angel.split();
    let tokens: Vec<String> = query
        .tokens
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
        .collect();
    angel_tx.send(AngelMessage::Text(json!({
        "correlationID": uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
        "action": 1,
        "params": {"mode": query.mode.unwrap_or(1), "tokenList": [{"exchangeType":query.exchange_type.unwrap_or(1),"tokens":tokens}]}
    }).to_string().into())).await?;
    browser
        .send(Message::Text(
            json!({"type":"connected","provider":"Angel One SmartAPI"})
                .to_string()
                .into(),
        ))
        .await?;
    let mut heartbeat = interval(Duration::from_secs(10));
    let mut freshness = interval(Duration::from_secs(5));
    let mut last_tick = Instant::now();
    loop {
        tokio::select! {
            _ = heartbeat.tick() => angel_tx.send(AngelMessage::Text("ping".into())).await?,
            _ = freshness.tick(), if last_tick.elapsed()>Duration::from_secs(30) => anyhow::bail!("Angel One market feed is stale (no tick for 30 seconds)"),
            incoming = angel_rx.next() => match incoming {
                Some(Ok(AngelMessage::Binary(data))) => {
                    if let Some(tick) = parse_tick(&data) {
                        last_tick=Instant::now();
                        if let (Some(token), Some(ltp)) = (tick["token"].as_str(), tick["last_traded_price"].as_f64())
                            && let Err(error) = crate::strategy::process_tick(
                                &state,
                                profile.user_id,
                                exchange_segment(query.exchange_type.unwrap_or(1)),
                                token,
                                ltp,
                            ).await {
                            tracing::warn!(%error, "demo strategy tick processing failed");
                        }
                        browser.send(Message::Text(tick.to_string().into())).await?;
                    }
                }
                Some(Ok(AngelMessage::Text(text))) => browser.send(Message::Text(text.to_string().into())).await?,
                Some(Ok(AngelMessage::Ping(data))) => angel_tx.send(AngelMessage::Pong(data)).await?,
                Some(Ok(AngelMessage::Close(_))) | None => break,
                Some(Err(error)) => return Err(error.into()),
                _ => {}
            },
            incoming = browser.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(Message::Ping(data))) => browser.send(Message::Pong(data)).await?,
                _ => {}
            }
        }
    }
    Ok(())
}

fn le_i64(data: &[u8], start: usize) -> Option<i64> {
    Some(i64::from_le_bytes(
        data.get(start..start + 8)?.try_into().ok()?,
    ))
}

fn parse_tick(data: &[u8]) -> Option<serde_json::Value> {
    if data.len() < 51 {
        return None;
    }
    let mode = data[0];
    let token_bytes = data.get(2..27)?;
    let end = token_bytes
        .iter()
        .position(|v| *v == 0)
        .unwrap_or(token_bytes.len());
    let token = String::from_utf8_lossy(&token_bytes[..end]).to_string();
    let mut tick = json!({
        "type":"tick", "subscription_mode":mode, "exchange_type":data[1], "token":token,
        "sequence_number":le_i64(data,27)?, "exchange_timestamp":le_i64(data,35)?,
        "last_traded_price":le_i64(data,43)? as f64 / 100.0
    });
    if mode >= 2 && data.len() >= 123 {
        tick["last_traded_quantity"] = json!(le_i64(data, 51)?);
        tick["average_traded_price"] = json!(le_i64(data, 59)? as f64 / 100.0);
        tick["volume_trade_for_the_day"] = json!(le_i64(data, 67)?);
        tick["open_price_of_the_day"] = json!(le_i64(data, 91)? as f64 / 100.0);
        tick["high_price_of_the_day"] = json!(le_i64(data, 99)? as f64 / 100.0);
        tick["low_price_of_the_day"] = json!(le_i64(data, 107)? as f64 / 100.0);
        tick["closed_price"] = json!(le_i64(data, 115)? as f64 / 100.0);
    }
    Some(tick)
}

fn exchange_type(segment: &str) -> Option<u8> {
    match segment.to_uppercase().as_str() {
        "NSE" => Some(1),
        "NFO" => Some(2),
        "BSE" => Some(3),
        "BFO" => Some(4),
        "MCX" => Some(5),
        "NCDEX" => Some(7),
        _ => None,
    }
}

fn exchange_segment(exchange_type: u8) -> &'static str {
    match exchange_type {
        1 => "NSE",
        2 => "NFO",
        3 => "BSE",
        4 => "BFO",
        5 => "MCX",
        7 => "NCDEX",
        _ => "UNKNOWN",
    }
}

pub async fn ensure_strategy_feed(state: AppState, exchange: String, token: String) {
    let exchange = exchange.to_uppercase();
    if !exchange_feed_expected(&exchange) {
        return;
    }
    {
        let mut requested = state.strategy_feed_tokens.lock().await;
        requested.entry(exchange.clone()).or_default().insert(token);
        let mut active = state.strategy_feeds.lock().await;
        // One websocket per exchange serves all active contracts. Per-token
        // sockets exceed Angel One's connection/rate limits under load.
        if !active.insert(exchange.clone()) {
            return;
        }
    }
    tokio::spawn(async move {
        let mut attempt = 0_u32;
        loop {
            let tokens = refresh_requested_tokens(&state, &exchange).await;
            if tokens.is_empty() {
                break;
            }
            if !exchange_feed_expected(&exchange) {
                break;
            }
            let rate_limited;
            match run_strategy_feed(&state, &exchange, &tokens).await {
                Ok(()) => {
                    attempt = 0;
                    rate_limited = false;
                }
                Err(error) => {
                    rate_limited = crate::angel::is_rate_limit_error(&error.to_string());
                    tracing::warn!(exchange = %exchange, tokens = tokens.len(), %error, attempt, "shared strategy market feed stopped");
                    crate::strategy::operational_alert(
                        &state,
                        None,
                        "",
                        "market_feed_disconnected",
                        "error",
                        &format!(
                            "Shared market feed stopped and will reconnect automatically: {error}"
                        ),
                    )
                    .await;
                    attempt = attempt.saturating_add(1);
                }
            }
            if refresh_requested_tokens(&state, &exchange).await.is_empty() {
                break;
            }
            let ceiling = if rate_limited {
                60
            } else {
                (1_u64 << attempt.min(6)).min(90)
            };
            let jitter = rand::thread_rng().gen_range(0..=ceiling * 250);
            tokio::time::sleep(Duration::from_millis(ceiling * 1000 + jitter)).await;
        }
        let mut requested = state.strategy_feed_tokens.lock().await;
        if requested.get(&exchange).is_none_or(HashSet::is_empty) {
            requested.remove(&exchange);
            state.strategy_feeds.lock().await.remove(&exchange);
        }
    });
}

pub async fn reset_strategy_feeds(state: &AppState) {
    state.strategy_feed_tokens.lock().await.clear();
    state.strategy_feeds.lock().await.clear();
}

async fn refresh_requested_tokens(state: &AppState, exchange: &str) -> HashSet<String> {
    let query = sqlx::query_scalar::<_, String>("SELECT DISTINCT s.contract_token FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE s.exchange_segment=$1 AND s.contract_token IS NOT NULL AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE) UNION SELECT DISTINCT s.contract_token FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id WHERE t.status='open' AND s.exchange_segment=$1 AND s.contract_token IS NOT NULL AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE)")
        .bind(exchange)
        .fetch_all(&state.db)
        .await;
    let mut requested = state.strategy_feed_tokens.lock().await;
    let entry = requested.entry(exchange.to_owned()).or_default();
    if let Ok(tokens) = query {
        *entry = tokens.into_iter().collect();
    }
    entry.clone()
}

fn subscribe_message(exchange_type: u8, tokens: &HashSet<String>) -> AngelMessage {
    AngelMessage::Text(
        json!({
            "correlationID": uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
            "action": 1,
            "params": {"mode": 1, "tokenList": [{"exchangeType": exchange_type, "tokens": tokens.iter().collect::<Vec<_>>()}]}
        })
        .to_string()
        .into(),
    )
}

async fn run_strategy_feed(
    state: &AppState,
    exchange: &str,
    tokens: &HashSet<String>,
) -> anyhow::Result<()> {
    let exchange_type = exchange_type(exchange)
        .ok_or_else(|| anyhow::anyhow!("unsupported Angel One exchange segment {exchange}"))?;
    let profile: BrokerageProfile = sqlx::query_as(
        "SELECT p.* FROM user_profiles p WHERE p.last_token_status IN ('success','refreshed') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='jwt_token') AND EXISTS (SELECT 1 FROM broker_secrets s WHERE s.user_id=p.user_id AND s.secret_kind='feed_token') ORDER BY p.token_received_at DESC NULLS LAST LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| anyhow::anyhow!("no connected Angel One session is available"))?;
    let credentials = state.credentials.load(profile.user_id).await?;
    let mut request = state.config.angel_ws_url.clone().into_client_request()?;
    let headers = request.headers_mut();
    headers.insert("Authorization", credentials.jwt_token.parse()?);
    headers.insert("x-api-key", credentials.api_key.parse()?);
    headers.insert("x-client-code", profile.brokerage_user_id.parse()?);
    headers.insert("x-feed-token", credentials.feed_token.parse()?);
    let (socket, _) = connect_async(request).await?;
    let (mut sender, mut receiver) = socket.split();
    let mut subscribed = tokens.clone();
    sender
        .send(subscribe_message(exchange_type, &subscribed))
        .await?;
    let mut heartbeat = interval(Duration::from_secs(10));
    let mut freshness = interval(Duration::from_secs(5));
    let mut subscriptions = interval(Duration::from_secs(5));
    let mut last_tick = Instant::now();
    let freshness_threshold = stale_threshold(exchange);
    loop {
        tokio::select! {
            _=heartbeat.tick()=>sender.send(AngelMessage::Text("ping".into())).await?,
            _=freshness.tick(), if last_tick.elapsed()>freshness_threshold=>{
                if exchange_feed_expected(exchange) {
                    anyhow::bail!(
                        "shared Angel One feed is stale (no tick for {} seconds)",
                        freshness_threshold.as_secs()
                    );
                }
                return Ok(());
            },
            _=subscriptions.tick()=> {
                let desired = refresh_requested_tokens(state, exchange).await;
                if desired.is_empty() { return Ok(()); }
                let added: HashSet<String> = desired.difference(&subscribed).cloned().collect();
                if !added.is_empty() {
                    sender.send(subscribe_message(exchange_type, &added)).await?;
                    subscribed.extend(added);
                }
            },
            incoming=receiver.next()=>match incoming {
                Some(Ok(AngelMessage::Binary(data)))=>if let Some(tick)=parse_tick(&data)
                    && tick["exchange_type"].as_u64()==Some(u64::from(exchange_type))
                    && tick["token"].as_str().is_some_and(|token| subscribed.contains(token))
                    && let Some(ltp)=tick["last_traded_price"].as_f64() {
                    let token = tick["token"].as_str().unwrap_or_default();
                    last_tick=Instant::now();
                    crate::strategy::process_tick_shared(state,exchange,token,ltp).await?;
                },
                Some(Ok(AngelMessage::Ping(data)))=>sender.send(AngelMessage::Pong(data)).await?,
                Some(Ok(AngelMessage::Close(_)))|None=>break,
                Some(Err(error))=>return Err(error.into()),
                _=>{}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_ltp_packet() {
        let mut data = vec![0_u8; 51];
        data[0] = 1;
        data[1] = 1;
        data[2..7].copy_from_slice(b"12345");
        data[43..51].copy_from_slice(&12345_i64.to_le_bytes());
        let value = parse_tick(&data).unwrap();
        assert_eq!(value["token"], "12345");
        assert_eq!(value["last_traded_price"], 123.45);
    }

    #[test]
    fn shared_subscription_contains_all_tokens() {
        let tokens = ["100", "200"]
            .into_iter()
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        let AngelMessage::Text(text) = subscribe_message(5, &tokens) else {
            panic!("expected a text subscription message");
        };
        let payload: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
        assert_eq!(payload["action"], 1);
        assert_eq!(payload["params"]["tokenList"][0]["exchangeType"], 5);
        assert_eq!(
            payload["params"]["tokenList"][0]["tokens"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}
