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
use chrono::{DateTime, Datelike, FixedOffset, Timelike, Utc, Weekday};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
use tokio::time::{Duration, Instant, interval};
use tokio_tungstenite::{
    client_async_tls,
    tungstenite::{Message as AngelMessage, client::IntoClientRequest},
};

type AngelSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_angel_ws(
    state: &AppState,
    user_id: uuid::Uuid,
    request: tokio_tungstenite::tungstenite::handshake::client::Request,
) -> anyhow::Result<AngelSocket> {
    let host = request
        .uri()
        .host()
        .ok_or_else(|| anyhow::anyhow!("Angel One WebSocket URL has no host"))?
        .to_owned();
    let port = request.uri().port_u16().unwrap_or(443);
    let remote = tokio::net::lookup_host((host.as_str(), port))
        .await?
        .find(|address| address.is_ipv4())
        .ok_or_else(|| anyhow::anyhow!("Angel One WebSocket host has no IPv4 address"))?;
    let socket = tokio::net::TcpSocket::new_v4()?;
    if let Some(source) = crate::egress::source_ip_for_user(state, user_id).await? {
        socket.bind((source, 0).into()).map_err(|error| {
            anyhow::anyhow!(
                "Configured Angel egress IP {source} is unavailable; broker operation blocked: {error}"
            )
        })?;
    }
    let stream = socket.connect(remote).await.map_err(|error| {
        anyhow::anyhow!("Angel One WebSocket connection failed without fallback: {error}")
    })?;
    Ok(client_async_tls(request, stream).await?.0)
}

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
    let angel = connect_angel_ws(&state, profile.user_id, request).await?;
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

fn tick_timestamp(tick: &serde_json::Value) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    tick["exchange_timestamp"]
        .as_i64()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        // Reject corrupt broker timestamps so one bad packet cannot create a
        // candle in the wrong trading session.
        .filter(|at| (*at - now).num_hours().abs() <= 24)
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

fn feed_generation_is_current(
    active: &std::collections::HashMap<String, uuid::Uuid>,
    exchange: &str,
    generation: uuid::Uuid,
) -> bool {
    active.get(exchange) == Some(&generation)
}

fn market_feed_failure_code(message: &str) -> &'static str {
    if message.contains("subscription is empty") {
        "market_data_subscription_empty"
    } else if message.contains("feed is stale") {
        "market_data_no_ticks"
    } else {
        "market_feed_disconnected"
    }
}

async fn state_feed_generation_is_current(
    state: &AppState,
    key: &str,
    generation: uuid::Uuid,
) -> bool {
    feed_generation_is_current(&*state.strategy_feeds.lock().await, key, generation)
}

pub async fn ensure_strategy_feed(state: AppState, exchange: String, token: String) {
    let exchange = exchange.to_uppercase();
    if !exchange_feed_expected(&exchange) {
        return;
    }
    const SHARED_FEED_KEY: &str = "ALL";
    let generation = uuid::Uuid::new_v4();
    {
        let mut requested = state.strategy_feed_tokens.lock().await;
        requested.entry(exchange.clone()).or_default().insert(token);
        let mut active = state.strategy_feeds.lock().await;
        // WebSocket V2 accepts multiple exchange groups in one subscription.
        // One physical socket keeps every strategy under Angel One's
        // three-connections-per-client limit.
        if active.contains_key(SHARED_FEED_KEY) {
            return;
        }
        active.insert(SHARED_FEED_KEY.to_owned(), generation);
    }
    tokio::spawn(async move {
        let mut attempt = 0_u32;
        loop {
            let requested = refresh_all_requested_tokens(&state).await;
            if requested.is_empty() {
                break;
            }
            if !state_feed_generation_is_current(&state, SHARED_FEED_KEY, generation).await {
                break;
            }
            let rate_limited;
            match run_strategy_feed(&state, generation).await {
                Ok(()) => {
                    attempt = 0;
                    rate_limited = false;
                }
                Err(error) => {
                    rate_limited = crate::angel::is_rate_limit_error(&error.to_string());
                    let token_count: usize = requested.values().map(HashSet::len).sum();
                    tracing::warn!(exchanges = requested.len(), tokens = token_count, %error, attempt, "shared strategy market feed stopped");
                    let message = error.to_string();
                    let code = market_feed_failure_code(&message);
                    crate::strategy::operational_alert(
                        &state,
                        None,
                        "",
                        code,
                        "error",
                        &format!(
                            "Shared market feed stopped and will reconnect automatically: {error}"
                        ),
                    )
                    .await;
                    attempt = attempt.saturating_add(1);
                }
            }
            if refresh_all_requested_tokens(&state).await.is_empty()
                || !state_feed_generation_is_current(&state, SHARED_FEED_KEY, generation).await
            {
                // Pre-order requests are session-scoped. Durable order/trade
                // tokens are reconstructed from PostgreSQL next session.
                state.strategy_feed_tokens.lock().await.clear();
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
        // A feed task must always release its own lease.  Previously the lease
        // was retained when old pending demo orders still requested tokens,
        // leaving the next trading day with no task and no ticks until restart.
        let mut active = state.strategy_feeds.lock().await;
        if feed_generation_is_current(&active, SHARED_FEED_KEY, generation) {
            active.remove(SHARED_FEED_KEY);
        }
    });
}

pub async fn reset_strategy_feeds(state: &AppState) {
    state.strategy_feed_tokens.lock().await.clear();
    state.strategy_feeds.lock().await.clear();
}

async fn refresh_requested_tokens(state: &AppState, exchange: &str) -> HashSet<String> {
    let query = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT s.contract_token
         FROM strategy_orders o
         JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
         WHERE s.exchange_segment=$1 AND s.contract_token IS NOT NULL
           AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
           AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE)
         UNION
         SELECT DISTINCT s.contract_token
         FROM trades t
         JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
         WHERE t.status='open' AND s.exchange_segment=$1 AND s.contract_token IS NOT NULL
           AND (s.contract_expiry IS NULL OR s.contract_expiry>=CURRENT_DATE)
         UNION
         SELECT DISTINCT CASE c.instrument
             WHEN 'SENSEX' THEN '99919000'
             WHEN 'NIFTY' THEN '99926000'
         END
         FROM user_strategy_configs c
         JOIN user_strategy_activations a
           ON a.user_id=c.user_id AND a.strategy_key=c.strategy_key
         JOIN users u ON u.id=c.user_id
         WHERE c.strategy_key=$2 AND c.enabled=TRUE AND a.is_active=TRUE AND u.is_active=TRUE
           AND (($1='BSE' AND c.instrument='SENSEX') OR ($1='NSE' AND c.instrument='NIFTY'))",
    )
        .bind(exchange)
        .bind(crate::strategy::SUPERTREND_INDEX_OPTIONS_STRATEGY_KEY)
        .fetch_all(&state.db)
        .await;
    let mut requested = state.strategy_feed_tokens.lock().await;
    let entry = requested.entry(exchange.to_owned()).or_default();
    if let Ok(tokens) = query {
        // A selected contract can be requested before its durable order row
        // exists. Replacing this set dropped that token before subscription.
        merge_requested_tokens(entry, tokens);
    }
    entry.clone()
}

fn merge_requested_tokens(
    requested: &mut HashSet<String>,
    derived: impl IntoIterator<Item = String>,
) {
    requested.extend(derived);
}

async fn refresh_all_requested_tokens(
    state: &AppState,
) -> std::collections::HashMap<String, HashSet<String>> {
    let mut requested = std::collections::HashMap::new();
    for exchange in ["NSE", "NFO", "BSE", "BFO", "MCX", "NCDEX"] {
        if !exchange_feed_expected(exchange) {
            continue;
        }
        let tokens = refresh_requested_tokens(state, exchange).await;
        if !tokens.is_empty() {
            requested.insert(exchange.to_owned(), tokens);
        }
    }
    requested
}

fn subscribe_groups_message(
    groups: &std::collections::HashMap<String, HashSet<String>>,
) -> AngelMessage {
    let token_list: Vec<serde_json::Value> = groups
        .iter()
        .filter_map(|(exchange, tokens)| {
            exchange_type(exchange).map(|exchange_type| {
                json!({"exchangeType":exchange_type,"tokens":tokens.iter().collect::<Vec<_>>()})
            })
        })
        .collect();
    AngelMessage::Text(
        json!({
            "correlationID": uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
            "action": 1,
            "params": {"mode": 1, "tokenList": token_list}
        })
        .to_string()
        .into(),
    )
}

fn shared_freshness_threshold(
    groups: &std::collections::HashMap<String, HashSet<String>>,
) -> Duration {
    if groups
        .keys()
        .any(|exchange| !exchange.eq_ignore_ascii_case("MCX"))
    {
        Duration::from_secs(45)
    } else {
        stale_threshold("MCX")
    }
}

async fn run_strategy_feed(state: &AppState, generation: uuid::Uuid) -> anyhow::Result<()> {
    const SHARED_FEED_KEY: &str = "ALL";
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
    let socket = connect_angel_ws(state, profile.user_id, request).await?;
    let (mut sender, mut receiver) = socket.split();
    let mut subscribed = refresh_all_requested_tokens(state).await;
    if subscribed.is_empty() {
        anyhow::bail!("shared market data subscription is empty");
    }
    let subscribed_token_count: usize = subscribed.values().map(HashSet::len).sum();
    sender.send(subscribe_groups_message(&subscribed)).await?;
    tracing::info!(
        exchanges = subscribed.len(),
        tokens = subscribed_token_count,
        "shared strategy market feed subscribed"
    );
    let mut heartbeat = interval(Duration::from_secs(10));
    let mut freshness = interval(Duration::from_secs(5));
    let mut subscriptions = interval(Duration::from_secs(5));
    let mut last_tick = Instant::now();
    let mut first_tick_received = false;
    let mut freshness_threshold = shared_freshness_threshold(&subscribed);
    loop {
        tokio::select! {
            _=heartbeat.tick()=>sender.send(AngelMessage::Text("ping".into())).await?,
            _=freshness.tick(), if last_tick.elapsed()>freshness_threshold=>{
                anyhow::bail!(
                    "shared Angel One feed is stale (no tick for {} seconds)",
                    freshness_threshold.as_secs()
                );
            },
            _=subscriptions.tick()=> {
                if !state_feed_generation_is_current(state, SHARED_FEED_KEY, generation).await {
                    return Ok(());
                }
                let desired = refresh_all_requested_tokens(state).await;
                if desired.is_empty() { return Ok(()); }
                let mut added = std::collections::HashMap::new();
                for (exchange, tokens) in &desired {
                    let current = subscribed.entry(exchange.clone()).or_default();
                    let new_tokens: HashSet<String> = tokens.difference(current).cloned().collect();
                    if !new_tokens.is_empty() {
                        current.extend(new_tokens.clone());
                        added.insert(exchange.clone(), new_tokens);
                    }
                }
                if !added.is_empty() {
                    sender.send(subscribe_groups_message(&added)).await?;
                }
                freshness_threshold = shared_freshness_threshold(&desired);
            },
            incoming=receiver.next()=>match incoming {
                Some(Ok(AngelMessage::Binary(data)))=>if let Some(tick)=parse_tick(&data)
                    && let Some(tick_exchange_type)=tick["exchange_type"].as_u64()
                    && let Ok(tick_exchange_type)=u8::try_from(tick_exchange_type)
                    && let exchange=exchange_segment(tick_exchange_type)
                    && tick["token"].as_str().is_some_and(|token| {
                        subscribed.get(exchange).is_some_and(|tokens| tokens.contains(token))
                    })
                    && let Some(ltp)=tick["last_traded_price"].as_f64()
                    && let Some(sequence)=tick["sequence_number"].as_i64()
                    && let Some(tick_at)=tick_timestamp(&tick) {
                    let token = tick["token"].as_str().unwrap_or_default();
                    last_tick=Instant::now();
                    if !first_tick_received {
                        first_tick_received = true;
                        tracing::info!(exchange, "shared strategy market feed received its first tick");
                    }
                    crate::strategy::process_tick_shared(
                        state,
                        exchange,
                        token,
                        ltp,
                        tick_at,
                        sequence,
                    ).await?;
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
    fn parses_exchange_timestamp_for_candle_bucketing() {
        let expected = Utc::now();
        let tick = json!({"exchange_timestamp": expected.timestamp_millis()});
        let actual = tick_timestamp(&tick).unwrap();
        assert!((actual - expected).num_milliseconds().abs() < 2);
        assert!(tick_timestamp(&json!({"exchange_timestamp":0})).is_none());
    }

    #[test]
    fn shared_subscription_groups_multiple_exchanges_on_one_socket() {
        let groups = std::collections::HashMap::from([
            ("NSE".to_owned(), HashSet::from(["99926000".to_owned()])),
            ("BSE".to_owned(), HashSet::from(["99919000".to_owned()])),
            ("MCX".to_owned(), HashSet::from(["silver".to_owned()])),
        ]);
        let AngelMessage::Text(text) = subscribe_groups_message(&groups) else {
            panic!("expected a text subscription message");
        };
        let payload: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
        let token_list = payload["params"]["tokenList"].as_array().unwrap();
        assert_eq!(token_list.len(), 3);
        assert_eq!(
            token_list
                .iter()
                .flat_map(|group| group["tokens"].as_array().unwrap())
                .count(),
            3
        );
    }

    #[test]
    fn stale_feed_generation_cannot_clear_replacement_lease() {
        let old = uuid::Uuid::new_v4();
        let replacement = uuid::Uuid::new_v4();
        let active = std::collections::HashMap::from([("MCX".to_string(), replacement)]);
        assert!(!feed_generation_is_current(&active, "MCX", old));
        assert!(feed_generation_is_current(&active, "MCX", replacement));
    }

    #[test]
    fn derived_tokens_do_not_discard_a_pre_order_subscription() {
        let mut requested = HashSet::from(["pre-order-option".to_owned()]);
        merge_requested_tokens(&mut requested, ["durable-order".to_owned()]);
        assert_eq!(requested.len(), 2);
        assert!(requested.contains("pre-order-option"));
        assert!(requested.contains("durable-order"));
    }

    #[test]
    fn empty_and_stale_feeds_have_actionable_failure_codes() {
        assert_eq!(
            market_feed_failure_code("shared market data subscription is empty"),
            "market_data_subscription_empty"
        );
        assert_eq!(
            market_feed_failure_code("shared Angel One feed is stale (no tick for 45 seconds)"),
            "market_data_no_ticks"
        );
        assert_eq!(
            market_feed_failure_code("connection reset"),
            "market_feed_disconnected"
        );
    }
}
