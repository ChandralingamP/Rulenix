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
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde::Deserialize;
use serde_json::json;
use tokio::time::{Duration, Instant, interval};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message as AngelMessage, client::IntoClientRequest},
};

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
                            && let Err(error) = crate::strategy::process_tick(&state, profile.user_id, token, ltp).await {
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

pub async fn ensure_strategy_feed(state: AppState, token: String) {
    {
        let mut active = state.strategy_feeds.lock().await;
        if !active.insert(token.clone()) {
            return;
        }
    }
    tokio::spawn(async move {
        let mut attempt = 0_u32;
        loop {
            match run_strategy_feed(&state, &token).await {
                Ok(()) => attempt = 0,
                Err(error) => {
                    tracing::warn!(%token,%error,attempt,"shared strategy market feed stopped");
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
            let needed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id WHERE s.contract_token=$1 AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing'))")
                .bind(&token).fetch_one(&state.db).await.unwrap_or(false);
            if !needed {
                break;
            }
            let ceiling = (1_u64 << attempt.min(6)).min(60);
            let jitter = rand::thread_rng().gen_range(0..=ceiling * 250);
            tokio::time::sleep(Duration::from_millis(ceiling * 1000 + jitter)).await;
        }
        state.strategy_feeds.lock().await.remove(&token);
    });
}

async fn run_strategy_feed(state: &AppState, token: &str) -> anyhow::Result<()> {
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
    sender
        .send(AngelMessage::Text(
            json!({
                "correlationID":uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
                "action":1,
                "params":{"mode":1,"tokenList":[{"exchangeType":5,"tokens":[token]}]}
            })
            .to_string()
            .into(),
        ))
        .await?;
    let mut heartbeat = interval(Duration::from_secs(10));
    let mut freshness = interval(Duration::from_secs(5));
    let mut last_tick = Instant::now();
    loop {
        tokio::select! {
            _=heartbeat.tick()=>sender.send(AngelMessage::Text("ping".into())).await?,
            _=freshness.tick(), if last_tick.elapsed()>Duration::from_secs(30)=>anyhow::bail!("shared Angel One feed is stale (no tick for 30 seconds)"),
            incoming=receiver.next()=>match incoming {
                Some(Ok(AngelMessage::Binary(data)))=>if let Some(tick)=parse_tick(&data)
                    && tick["token"].as_str()==Some(token)
                    && let Some(ltp)=tick["last_traded_price"].as_f64() {
                    last_tick=Instant::now();
                    crate::strategy::process_tick_shared(state,token,ltp).await?;
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
}
