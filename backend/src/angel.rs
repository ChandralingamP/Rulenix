use crate::{
    credentials::redact_sensitive,
    error::{AppError, AppResult},
    state::AppState,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Debug, Deserialize)]
pub struct AngelEnvelope<T> {
    #[serde(alias = "success")]
    pub status: bool,
    #[serde(default, deserialize_with = "null_string")]
    pub message: String,
    #[serde(default, alias = "errorCode")]
    pub errorcode: Option<String>,
    pub data: Option<T>,
}

fn null_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerErrorClass {
    Retryable,
    Rejected,
    Authentication,
    Ambiguous,
}

impl BrokerErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retryable => "retryable",
            Self::Rejected => "rejected",
            Self::Authentication => "authentication",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct BrokerError {
    pub class: BrokerErrorClass,
    pub status: Option<u16>,
    pub code: String,
    pub message: String,
    pub diagnostic: String,
}

fn bounded_diagnostic(body: &[u8], secrets: &[&str]) -> String {
    let text = String::from_utf8_lossy(&body[..body.len().min(1024)]);
    redact_sensitive(&text, secrets).replace(['\r', '\n'], " ")
}

fn classify(
    status: reqwest::StatusCode,
    code: Option<&str>,
    message: &str,
    submission: bool,
) -> BrokerErrorClass {
    if status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
        || is_expiry_error(message, code)
    {
        return BrokerErrorClass::Authentication;
    }
    if submission && (status.is_server_error() || status == reqwest::StatusCode::REQUEST_TIMEOUT) {
        return BrokerErrorClass::Ambiguous;
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
        || ["temporar", "timeout", "unavailable", "rate limit"]
            .iter()
            .any(|v| message.to_lowercase().contains(v))
    {
        return BrokerErrorClass::Retryable;
    }
    BrokerErrorClass::Rejected
}

fn decode_bytes<T: DeserializeOwned>(
    status: reqwest::StatusCode,
    content_type: &str,
    body: &[u8],
    operation: &str,
    secrets: &[&str],
    submission: bool,
) -> Result<AngelEnvelope<T>, BrokerError> {
    serde_json::from_slice(body).map_err(|_| BrokerError {
        class: if submission {
            BrokerErrorClass::Ambiguous
        } else {
            BrokerErrorClass::Retryable
        },
        status: Some(status.as_u16()),
        code: "invalid_response".into(),
        message: format!(
            "Angel One {operation} returned HTTP {status} with an invalid {content_type} response."
        ),
        diagnostic: bounded_diagnostic(body, secrets),
    })
}

async fn decode_response_detailed<T: DeserializeOwned>(
    response: reqwest::Response,
    operation: &str,
    secrets: &[&str],
    submission: bool,
) -> Result<(reqwest::StatusCode, AngelEnvelope<T>), BrokerError> {
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .split(';')
        .next()
        .unwrap_or("unknown")
        .to_owned();
    let body = response.bytes().await.map_err(|error| BrokerError {
        class: if submission {
            BrokerErrorClass::Ambiguous
        } else {
            BrokerErrorClass::Retryable
        },
        status: Some(status.as_u16()),
        code: "body_read_failed".into(),
        message: format!(
            "Angel One {operation} returned HTTP {status}, but its response body could not be read."
        ),
        diagnostic: redact_sensitive(&error.to_string(), secrets),
    })?;
    let payload = decode_bytes(status, &content_type, &body, operation, secrets, submission)?;
    Ok((status, payload))
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase")]
pub struct AngelSession {
    pub jwt_token: String,
    pub refresh_token: String,
    pub feed_token: String,
}

#[derive(Serialize)]
struct LoginRequest<'a> {
    #[serde(rename = "clientcode")]
    client_code: &'a str,
    password: &'a str,
    totp: &'a str,
    state: &'a str,
}

fn base_headers(state: &AppState, api_key: &str) -> AppResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    for (key, value) in [
        ("x-privatekey", api_key),
        ("x-usertype", "USER"),
        ("x-sourceid", "WEB"),
        ("x-clientlocalip", &state.config.client_local_ip),
        ("x-clientpublicip", &state.config.client_public_ip),
        ("x-macaddress", &state.config.client_mac_address),
    ] {
        headers.insert(
            HeaderName::from_bytes(key.as_bytes()).map_err(|e| AppError::Internal(e.into()))?,
            HeaderValue::from_str(value).map_err(|e| AppError::Internal(e.into()))?,
        );
    }
    Ok(headers)
}

fn authenticated_headers(state: &AppState, api_key: &str, jwt_token: &str) -> AppResult<HeaderMap> {
    let mut headers = base_headers(state, api_key)?;
    headers.insert(
        reqwest::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {jwt_token}"))
            .map_err(|error| AppError::Internal(error.into()))?,
    );
    Ok(headers)
}

async fn decode_response<T: DeserializeOwned>(
    response: reqwest::Response,
    operation: &str,
) -> AppResult<(reqwest::StatusCode, AngelEnvelope<T>)> {
    decode_response_detailed(response, operation, &[], false)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))
}

pub async fn create_session(
    state: &AppState,
    client_code: &str,
    api_key: &str,
    mpin: &str,
    totp: &str,
) -> AppResult<AngelSession> {
    let endpoint = format!(
        "{}/rest/auth/angelbroking/user/v1/loginByPassword",
        state.config.angel_api_base
    );
    let mut response = None;
    for attempt in 0..2 {
        match state
            .http
            .post(&endpoint)
            .timeout(std::time::Duration::from_secs(8))
            .headers(base_headers(state, api_key)?)
            .json(&LoginRequest {
                client_code,
                password: mpin,
                totp,
                state: "STATE_VARIABLE",
            })
            .send()
            .await
        {
            Ok(value) => {
                response = Some(value);
                break;
            }
            Err(_) if attempt == 0 => {
                tokio::time::sleep(std::time::Duration::from_millis(350)).await;
            }
            Err(_) => {}
        }
    }
    let response = response.ok_or_else(|| {
        AppError::BadRequest(
            "Angel One did not respond after a retry. Check the network route and try again with a fresh TOTP."
                .into(),
        )
    })?;

    let (status, payload): (_, AngelEnvelope<Value>) = decode_response(response, "login").await?;
    if !status.is_success() || !payload.status {
        return Err(AppError::BadRequest(redact_sensitive(
            &payload.message,
            &[api_key, mpin, totp],
        )));
    }
    let data = payload
        .data
        .ok_or_else(|| AppError::BadRequest("Angel One returned no session tokens".into()))?;
    serde_json::from_value(data)
        .map_err(|_| AppError::BadRequest("Angel One returned malformed session tokens".into()))
}

fn numeric_value(value: Option<&Value>) -> Option<f64> {
    value.and_then(|item| {
        item.as_f64()
            .or_else(|| item.as_str().and_then(|text| text.parse::<f64>().ok()))
    })
}

pub async fn get_margin(state: &AppState, api_key: &str, jwt_token: &str) -> AppResult<Value> {
    let headers = authenticated_headers(state, api_key, jwt_token)?;
    let response = state
        .http
        .get(format!(
            "{}/rest/secure/angelbroking/user/v1/getRMS",
            state.config.angel_api_base
        ))
        .headers(headers)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Unable to reach Angel One: {error}")))?;
    let (status, payload): (_, AngelEnvelope<Value>) =
        decode_response(response, "margin request").await?;
    if !status.is_success() || !payload.status {
        return Err(AppError::BadRequest(redact_sensitive(
            &payload.message,
            &[api_key, jwt_token],
        )));
    }
    let data = payload
        .data
        .ok_or_else(|| AppError::BadRequest("Angel One returned no margin data.".into()))?;
    let available = numeric_value(data.get("availablecash"))
        .or_else(|| numeric_value(data.get("availableCash")))
        .or_else(|| numeric_value(data.get("net")))
        .ok_or_else(|| {
            AppError::BadRequest("Angel One margin response has no available balance.".into())
        })?;
    Ok(serde_json::json!({"available_balance":available,"provider_data":data}))
}

#[allow(clippy::too_many_arguments)]
pub async fn calculate_margin(
    state: &AppState,
    api_key: &str,
    jwt_token: &str,
    exchange: &str,
    product_type: &str,
    token: &str,
    quantity: i32,
    order_type: &str,
    trade_type: &str,
) -> AppResult<Value> {
    let body = margin_payload(
        exchange,
        product_type,
        token,
        quantity,
        order_type,
        trade_type,
    );
    secure_json(
        state,
        reqwest::Method::POST,
        "/rest/secure/angelbroking/margin/v1/batch",
        api_key,
        jwt_token,
        Some(body),
    )
    .await
}

fn margin_payload(
    exchange: &str,
    product_type: &str,
    token: &str,
    quantity: i32,
    order_type: &str,
    trade_type: &str,
) -> Value {
    json!({
        "positions":[{
            "exchange":exchange,
            "orderType":order_type,
            "qty":quantity.to_string(),
            "price":"0",
            "productType":product_type,
            "token":token,
            "tradeType":trade_type,
        }]
    })
}

pub async fn market_quote(
    state: &AppState,
    api_key: &str,
    jwt_token: &str,
    mode: &str,
    exchange_tokens: Value,
) -> AppResult<Value> {
    secure_json(
        state,
        reqwest::Method::POST,
        "/rest/secure/angelbroking/market/v1/quote",
        api_key,
        jwt_token,
        Some(json!({"mode":mode,"exchangeTokens":exchange_tokens})),
    )
    .await
}

async fn secure_json(
    state: &AppState,
    method: reqwest::Method,
    path: &str,
    api_key: &str,
    jwt_token: &str,
    body: Option<Value>,
) -> AppResult<Value> {
    let mut request = state
        .http
        .request(method, format!("{}{}", state.config.angel_api_base, path))
        .headers(authenticated_headers(state, api_key, jwt_token)?);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Unable to reach Angel One: {error}")))?;
    let (status, payload): (_, AngelEnvelope<Value>) = match decode_response_detailed(
        response,
        "API request",
        &[api_key, jwt_token],
        false,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(status=?error.status,code=%error.code,class=?error.class,diagnostic=%error.diagnostic,"Angel One response decoding failed");
            return Err(AppError::BadRequest(error.to_string()));
        }
    };
    if !status.is_success() || !payload.status {
        return Err(AppError::BadRequest(redact_sensitive(
            &payload.message,
            &[api_key, jwt_token],
        )));
    }
    Ok(payload.data.unwrap_or(Value::Null))
}

pub async fn get_candles(
    state: &AppState,
    api_key: &str,
    jwt_token: &str,
    token: &str,
    from_date: &str,
    to_date: &str,
) -> AppResult<Value> {
    get_candles_with_interval(
        state, api_key, jwt_token, token, "ONE_DAY", from_date, to_date,
    )
    .await
}

pub async fn get_candles_with_interval(
    state: &AppState,
    api_key: &str,
    jwt_token: &str,
    token: &str,
    interval: &str,
    from_date: &str,
    to_date: &str,
) -> AppResult<Value> {
    get_candles_with_exchange_interval(
        state, api_key, jwt_token, "MCX", token, interval, from_date, to_date,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn get_candles_with_exchange_interval(
    state: &AppState,
    api_key: &str,
    jwt_token: &str,
    exchange: &str,
    token: &str,
    interval: &str,
    from_date: &str,
    to_date: &str,
) -> AppResult<Value> {
    let body = json!({
        "exchange":exchange,
        "symboltoken":token,
        "interval":interval,
        "fromdate":from_date,
        "todate":to_date,
    });
    for attempt in 0..2 {
        match secure_json(
            state,
            reqwest::Method::POST,
            "/rest/secure/angelbroking/historical/v1/getCandleData",
            api_key,
            jwt_token,
            Some(body.clone()),
        )
        .await
        {
            Ok(value) => return Ok(value),
            Err(_) if attempt == 0 => {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the bounded candle retry loop always returns")
}

#[derive(Debug, Clone)]
pub struct OrderRequest<'a> {
    pub symbol: &'a str,
    pub token: &'a str,
    pub exchange: &'a str,
    pub product_type: &'a str,
    pub side: &'a str,
    pub order_type: &'a str,
    pub quantity: i32,
    pub price: f64,
    pub trigger_price: Option<f64>,
    pub client_order_id: &'a str,
}

pub fn may_retry_submission(class: BrokerErrorClass) -> bool {
    class == BrokerErrorClass::Retryable
}

fn classify_transport(error: &reqwest::Error) -> BrokerErrorClass {
    if error.is_connect() {
        BrokerErrorClass::Retryable
    } else {
        BrokerErrorClass::Ambiguous
    }
}

fn order_payload(order: &OrderRequest<'_>) -> Value {
    let price = if order.order_type == "MARKET" {
        0.0
    } else {
        order.price
    };
    json!({
        "variety":if order.order_type.starts_with("STOPLOSS") { "STOPLOSS" } else { "NORMAL" },
        "tradingsymbol":order.symbol,"symboltoken":order.token,"transactiontype":order.side,"exchange":order.exchange,
        "ordertype":order.order_type,"producttype":order.product_type,"duration":"DAY","price":format!("{price:.2}"),
        "squareoff":"0","stoploss":"0","quantity":order.quantity.to_string(),
        "triggerprice":order.trigger_price.map(|value|format!("{value:.2}")).unwrap_or_else(||"0".into()),
        "ordertag":order.client_order_id,
    })
}

pub async fn place_order(
    state: &AppState,
    api_key: &str,
    jwt_token: &str,
    order: &OrderRequest<'_>,
) -> Result<String, BrokerError> {
    let body = order_payload(order);
    let response=state.http.post(format!("{}/rest/secure/angelbroking/order/v1/placeOrder",state.config.angel_api_base))
        .headers(authenticated_headers(state,api_key,jwt_token).map_err(|e|BrokerError{class:BrokerErrorClass::Authentication,status:None,code:"invalid_headers".into(),message:e.to_string(),diagnostic:String::new()})?)
        .json(&body).send().await.map_err(|error|BrokerError{class:classify_transport(&error),status:error.status().map(|s|s.as_u16()),code:"transport".into(),message:if error.is_connect(){"Angel One could not be reached before submission.".into()}else{"Angel One submission outcome is unknown and will be reconciled; it will not be retried automatically.".into()},diagnostic:redact_sensitive(&error.to_string(),&[api_key,jwt_token])})?;
    let (status, payload): (_, AngelEnvelope<Value>) =
        decode_response_detailed(response, "order submission", &[api_key, jwt_token], true).await?;
    if !status.is_success() || !payload.status {
        return Err(BrokerError {
            class: classify(status, payload.errorcode.as_deref(), &payload.message, true),
            status: Some(status.as_u16()),
            code: payload
                .errorcode
                .unwrap_or_else(|| "broker_rejected".into()),
            message: if payload.message.is_empty() {
                format!("Angel One rejected order submission with HTTP {status}.")
            } else {
                redact_sensitive(&payload.message, &[api_key, jwt_token])
            },
            diagnostic: format!(
                "status={}; response={}",
                status,
                redact_sensitive(&payload.message, &[api_key, jwt_token])
            ),
        });
    }
    let data = payload.data.unwrap_or(Value::Null);
    data.get("orderid")
        .or_else(|| data.get("orderId"))
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| BrokerError {
            class: BrokerErrorClass::Ambiguous,
            status: Some(status.as_u16()),
            code: "missing_order_id".into(),
            message:
                "Angel One accepted the submission without an order ID; reconciliation is required."
                    .into(),
            diagnostic: bounded_diagnostic(data.to_string().as_bytes(), &[api_key, jwt_token]),
        })
}

pub async fn cancel_order(
    state: &AppState,
    api_key: &str,
    jwt_token: &str,
    order_id: &str,
    variety: &str,
) -> AppResult<()> {
    secure_json(
        state,
        reqwest::Method::POST,
        "/rest/secure/angelbroking/order/v1/cancelOrder",
        api_key,
        jwt_token,
        Some(json!({"variety":variety,"orderid":order_id})),
    )
    .await?;
    Ok(())
}

pub async fn order_book(state: &AppState, api_key: &str, jwt_token: &str) -> AppResult<Value> {
    secure_json(
        state,
        reqwest::Method::GET,
        "/rest/secure/angelbroking/order/v1/getOrderBook",
        api_key,
        jwt_token,
        None,
    )
    .await
}

#[derive(Debug, PartialEq)]
pub enum SessionCheck {
    Valid,
    Expired(String),
    Unavailable(String),
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RefreshedSession {
    pub jwt_token: String,
    pub refresh_token: Option<String>,
    pub feed_token: String,
}

pub enum RefreshCheck {
    Refreshed(RefreshedSession),
    Invalid(String),
    Unavailable(String),
}

fn jwt_expiration(token: &str) -> Option<i64> {
    let token = token.strip_prefix("Bearer ").unwrap_or(token);
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Value>(&decoded)
        .ok()?
        .get("exp")?
        .as_i64()
}

pub fn jwt_expires_within(token: &str, seconds: i64) -> bool {
    jwt_expiration(token).is_some_and(|expiry| expiry <= Utc::now().timestamp() + seconds)
}

fn is_expiry_error(message: &str, code: Option<&str>) -> bool {
    let value = message.to_lowercase();
    matches!(
        code,
        Some("AG8001" | "AG8002" | "AG8003" | "AG8004" | "AB1010")
    ) || ["token", "session", "jwt", "expired", "unauthorized"]
        .iter()
        .any(|word| value.contains(word))
}

pub fn is_invalid_api_key_error(message: &str) -> bool {
    let value = message.to_lowercase();
    value.contains("invalid api key") || value.contains("ag8004")
}

pub async fn refresh_session(
    state: &AppState,
    api_key: &str,
    jwt_token: &str,
    refresh_token: &str,
) -> RefreshCheck {
    if refresh_token.trim().is_empty() {
        return RefreshCheck::Invalid(
            "Angel One refresh token is missing. Please reconnect.".into(),
        );
    }
    let headers = match authenticated_headers(state, api_key, jwt_token) {
        Ok(headers) => headers,
        Err(error) => return RefreshCheck::Unavailable(error.to_string()),
    };
    let endpoint = format!(
        "{}/rest/auth/angelbroking/jwt/v1/generateTokens",
        state.config.angel_api_base
    );
    for attempt in 0..2 {
        let response = state
            .http
            .post(&endpoint)
            .headers(headers.clone())
            .json(&json!({"refreshToken":refresh_token}))
            .send()
            .await;
        let result = match response {
            Ok(response) => {
                let status = response.status();
                match response.bytes().await {
                    Ok(body) => {
                        parse_refresh_response(status, &body, api_key, jwt_token, refresh_token)
                    }
                    Err(_) => RefreshCheck::Unavailable(
                        "Angel One token refresh could not be read.".into(),
                    ),
                }
            }
            Err(_) => RefreshCheck::Unavailable(
                "Angel One token refresh is temporarily unavailable.".into(),
            ),
        };
        if !matches!(result, RefreshCheck::Unavailable(_)) || attempt == 1 {
            return result;
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
    unreachable!("the bounded token refresh loop always returns")
}

fn parse_refresh_response(
    status: reqwest::StatusCode,
    body: &[u8],
    api_key: &str,
    jwt_token: &str,
    refresh_token: &str,
) -> RefreshCheck {
    let payload = match serde_json::from_slice::<Value>(body) {
        Ok(payload) => payload,
        Err(_)
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN =>
        {
            return RefreshCheck::Invalid(
                "Angel One refresh token is invalid or expired. Please reconnect.".into(),
            );
        }
        Err(_) => {
            return RefreshCheck::Unavailable(format!(
                "Angel One token refresh returned an unreadable HTTP {status} response."
            ));
        }
    };
    let message = payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let code = payload
        .get("errorcode")
        .or_else(|| payload.get("errorCode"))
        .and_then(Value::as_str);
    let accepted = payload
        .get("status")
        .or_else(|| payload.get("success"))
        .and_then(Value::as_bool);
    if status.is_success() && accepted == Some(true) {
        let data = payload.get("data").unwrap_or(&Value::Null);
        let jwt = data
            .get("jwtToken")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let feed = data
            .get("feedToken")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let refresh = data
            .get("refreshToken")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if !jwt.is_empty() && !feed.is_empty() {
            return RefreshCheck::Refreshed(RefreshedSession {
                jwt_token: jwt.to_owned(),
                refresh_token: refresh,
                feed_token: feed.to_owned(),
            });
        }
        return RefreshCheck::Unavailable(
            "Angel One token refresh succeeded without returning session tokens.".into(),
        );
    }
    if status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
        || is_expiry_error(message, code)
    {
        return RefreshCheck::Invalid(
            "Angel One refresh token is invalid or expired. Please reconnect.".into(),
        );
    }
    RefreshCheck::Unavailable(format!(
        "Angel One could not refresh the session: {}",
        redact_sensitive(message, &[api_key, jwt_token, refresh_token])
    ))
}

pub async fn verify_session(
    state: &AppState,
    api_key: &str,
    jwt_token: &str,
    refresh_token: &str,
) -> SessionCheck {
    if jwt_expiration(jwt_token).is_some_and(|expiry| expiry <= Utc::now().timestamp()) {
        return SessionCheck::Expired("Angel One session has expired. Please reconnect.".into());
    }
    let response = state
        .http
        .get(format!(
            "{}/rest/secure/angelbroking/user/v1/getProfile",
            state.config.angel_api_base
        ))
        .headers(match authenticated_headers(state, api_key, jwt_token) {
            Ok(headers) => headers,
            Err(error) => return SessionCheck::Unavailable(error.to_string()),
        })
        .query(&[("refreshToken", refresh_token)])
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(_) => {
            return SessionCheck::Unavailable(
                "Angel One session verification is temporarily unavailable.".into(),
            );
        }
    };
    let status = response.status();
    let body = match response.bytes().await {
        Ok(body) => body,
        Err(error) => {
            return SessionCheck::Unavailable(format!(
                "Unable to read Angel One verification response: {error}"
            ));
        }
    };
    let payload = serde_json::from_slice::<AngelEnvelope<Value>>(&body);
    match payload {
        Ok(payload) if status.is_success() && payload.status => SessionCheck::Valid,
        Ok(payload)
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
                || is_expiry_error(&payload.message, payload.errorcode.as_deref()) =>
        {
            SessionCheck::Expired("Angel One session has expired. Please reconnect.".into())
        }
        Ok(payload) => SessionCheck::Unavailable(format!(
            "Angel One could not verify the session: {}",
            redact_sensitive(&payload.message, &[api_key, jwt_token, refresh_token])
        )),
        Err(_)
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN =>
        {
            SessionCheck::Expired("Angel One session has expired. Please reconnect.".into())
        }
        Err(_) => SessionCheck::Unavailable(format!(
            "Angel One session verification returned HTTP {status}."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post,
    };
    use std::{collections::HashMap, sync::Arc, time::Duration};
    use tokio::sync::Mutex;

    async fn mock_server(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{address}"), task)
    }

    #[test]
    fn reads_expiry_from_bearer_jwt() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":1900000000}"#);
        let token = format!("Bearer header.{payload}.signature");
        assert_eq!(jwt_expiration(&token), Some(1_900_000_000));
    }

    #[test]
    fn detects_refresh_window() {
        let soon = URL_SAFE_NO_PAD
            .encode(format!(r#"{{"exp":{}}}"#, Utc::now().timestamp() + 300).as_bytes());
        let later = URL_SAFE_NO_PAD
            .encode(format!(r#"{{"exp":{}}}"#, Utc::now().timestamp() + 1_200).as_bytes());
        assert!(jwt_expires_within(&format!("h.{soon}.s"), 600));
        assert!(!jwt_expires_within(&format!("h.{later}.s"), 600));
    }

    #[test]
    fn recognizes_angel_token_errors() {
        assert!(is_expiry_error("Invalid Token", None));
        assert!(is_expiry_error("Request rejected", Some("AG8001")));
        assert!(is_expiry_error("Invalid API Key", Some("AG8004")));
        assert!(!is_expiry_error("Service temporarily unavailable", None));
        assert!(is_invalid_api_key_error("Invalid API Key"));
        assert!(is_invalid_api_key_error("Broker error AG8004"));
        assert!(!is_invalid_api_key_error("Service temporarily unavailable"));
    }

    #[test]
    fn accepts_alternate_angel_error_envelope() {
        let payload: AngelEnvelope<Value> = serde_json::from_slice(
            br#"{"success":false,"message":"Invalid API Key","errorCode":"AG8004","data":""}"#,
        )
        .unwrap();
        assert!(!payload.status);
        assert_eq!(payload.errorcode.as_deref(), Some("AG8004"));

        let refresh = parse_refresh_response(
            reqwest::StatusCode::OK,
            br#"{"success":false,"message":"Invalid API Key","errorCode":"AG8004","data":""}"#,
            "api-key",
            "jwt",
            "refresh",
        );
        assert!(matches!(refresh, RefreshCheck::Invalid(_)));
    }

    #[test]
    fn accepts_refresh_success_with_nullable_metadata() {
        let result = parse_refresh_response(
            reqwest::StatusCode::OK,
            br#"{"status":true,"message":null,"errorcode":null,"data":{"jwtToken":"new-jwt","feedToken":"new-feed"}}"#,
            "api-key",
            "old-jwt",
            "refresh",
        );
        match result {
            RefreshCheck::Refreshed(tokens) => {
                assert_eq!(tokens.jwt_token, "new-jwt");
                assert_eq!(tokens.feed_token, "new-feed");
                assert!(tokens.refresh_token.is_none());
            }
            _ => panic!("valid refresh payload was rejected"),
        }
    }

    #[tokio::test]
    async fn mock_angel_accepts_nullable_message_and_preserves_order_tag() {
        async fn place(
            State(tags): State<Arc<Mutex<HashMap<String, String>>>>,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            let tag = body["ordertag"].as_str().unwrap().to_string();
            let mut tags = tags.lock().await;
            let next = format!("ORDER-{}", tags.len() + 1);
            let id = tags.entry(tag).or_insert(next).clone();
            Json(json!({"status":true,"message":null,"errorcode":null,"data":{"orderid":id}}))
        }
        let tags = Arc::new(Mutex::new(HashMap::new()));
        let (base, server) = mock_server(
            Router::new()
                .route("/", post(place))
                .with_state(tags.clone()),
        )
        .await;
        let order = OrderRequest {
            symbol: "GOLDTEN",
            token: "1",
            exchange: "MCX",
            product_type: "CARRYFORWARD",
            side: "BUY",
            order_type: "LIMIT",
            quantity: 1,
            price: 100.0,
            trigger_price: None,
            client_order_id: "RX123",
        };
        let client = reqwest::Client::new();
        for _ in 0..2 {
            let response = client
                .post(&base)
                .json(&order_payload(&order))
                .send()
                .await
                .unwrap();
            let (_, payload): (_, AngelEnvelope<Value>) =
                decode_response_detailed(response, "mock order", &[], true)
                    .await
                    .unwrap();
            assert_eq!(payload.data.unwrap()["orderid"], "ORDER-1");
        }
        assert_eq!(
            tags.lock().await.len(),
            1,
            "the stable tag prevents duplicate broker identities"
        );
        server.abort();
    }

    #[test]
    fn stoploss_limit_payload_keeps_limit_and_trigger_prices() {
        let order = OrderRequest {
            symbol: "GOLDTEN",
            token: "1",
            exchange: "MCX",
            product_type: "CARRYFORWARD",
            side: "BUY",
            order_type: "STOPLOSS_LIMIT",
            quantity: 1,
            price: 100.25,
            trigger_price: Some(100.0),
            client_order_id: "RXSTOPLIMIT",
        };

        let payload = order_payload(&order);

        assert_eq!(payload["variety"], "STOPLOSS");
        assert_eq!(payload["ordertype"], "STOPLOSS_LIMIT");
        assert_eq!(payload["price"], "100.25");
        assert_eq!(payload["triggerprice"], "100.00");
    }

    #[test]
    fn margin_payload_includes_required_order_type() {
        let payload = margin_payload("MCX", "CARRYFORWARD", "123", 10, "STOPLOSS_LIMIT", "BUY");
        let position = &payload["positions"][0];
        assert_eq!(position["orderType"], "STOPLOSS_LIMIT");
        assert_eq!(position["productType"], "CARRYFORWARD");
        assert_eq!(position["tradeType"], "BUY");
        assert_eq!(position["token"], "123");
        assert_eq!(position["qty"], "10");
    }

    #[tokio::test]
    async fn malformed_submission_is_ambiguous_and_diagnostics_are_redacted() {
        async fn malformed() -> impl IntoResponse {
            (StatusCode::BAD_GATEWAY, "gateway leaked api-secret")
        }
        let (base, server) = mock_server(Router::new().route("/", post(malformed))).await;
        let response = reqwest::Client::new().post(&base).send().await.unwrap();
        let error =
            decode_response_detailed::<Value>(response, "order submission", &["api-secret"], true)
                .await
                .unwrap_err();
        assert_eq!(error.class, BrokerErrorClass::Ambiguous);
        assert_eq!(error.status, Some(502));
        assert!(!error.diagnostic.contains("api-secret"));
        assert!(error.diagnostic.contains("[REDACTED]"));
        assert!(!may_retry_submission(error.class));
        server.abort();
    }

    #[tokio::test]
    async fn timeout_or_disconnect_is_never_blindly_retried() {
        async fn delayed() -> Json<Value> {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Json(json!({"status":true,"message":"","data":{"orderid":"late"}}))
        }
        let (base, server) = mock_server(Router::new().route("/", post(delayed))).await;
        let error = reqwest::Client::builder()
            .timeout(Duration::from_millis(20))
            .build()
            .unwrap()
            .post(&base)
            .send()
            .await
            .unwrap_err();
        assert_eq!(classify_transport(&error), BrokerErrorClass::Ambiguous);
        assert!(!may_retry_submission(classify_transport(&error)));
        server.abort();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let dropper = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket);
        });
        let disconnect = reqwest::Client::new()
            .post(format!("http://{address}"))
            .body("submission")
            .send()
            .await
            .unwrap_err();
        assert_eq!(classify_transport(&disconnect), BrokerErrorClass::Ambiguous);
        assert!(!may_retry_submission(classify_transport(&disconnect)));
        dropper.await.unwrap();
    }

    #[test]
    fn broker_errors_are_classified_for_execution_policy() {
        assert_eq!(
            classify(
                reqwest::StatusCode::UNAUTHORIZED,
                Some("AG8001"),
                "expired",
                true
            ),
            BrokerErrorClass::Authentication
        );
        assert_eq!(
            classify(reqwest::StatusCode::SERVICE_UNAVAILABLE, None, "down", true),
            BrokerErrorClass::Ambiguous
        );
        assert_eq!(
            classify(
                reqwest::StatusCode::TOO_MANY_REQUESTS,
                None,
                "rate limit",
                false
            ),
            BrokerErrorClass::Retryable
        );
        assert_eq!(
            classify(
                reqwest::StatusCode::BAD_REQUEST,
                Some("AB1004"),
                "invalid quantity",
                true
            ),
            BrokerErrorClass::Rejected
        );
    }
}
