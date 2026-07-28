use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
use chrono::{FixedOffset, NaiveDate, Utc};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::Mutex;

const MASTER_URL: &str =
    "https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json";
const MASTER_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MasterContract {
    #[serde(deserialize_with = "string_from_any")]
    pub token: String,
    #[serde(deserialize_with = "string_from_any")]
    pub symbol: String,
    #[serde(deserialize_with = "string_from_any")]
    pub name: String,
    #[serde(deserialize_with = "string_from_any")]
    pub expiry: String,
    #[serde(default, deserialize_with = "string_from_any")]
    pub strike: String,
    #[serde(deserialize_with = "string_from_any")]
    pub lotsize: String,
    #[serde(deserialize_with = "string_from_any")]
    pub instrumenttype: String,
    #[serde(deserialize_with = "string_from_any")]
    pub exch_seg: String,
}

type CachedMaster = Option<(NaiveDate, Arc<Vec<MasterContract>>)>;
static MASTER_CACHE: OnceLock<Mutex<CachedMaster>> = OnceLock::new();

fn string_from_any<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::String(text) => text,
        Value::Number(number) => number.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

fn ist_date() -> NaiveDate {
    Utc::now()
        .with_timezone(&FixedOffset::east_opt(19_800).expect("valid IST offset"))
        .date_naive()
}

pub(crate) async fn load(state: &AppState) -> AppResult<Arc<Vec<MasterContract>>> {
    let cache = MASTER_CACHE.get_or_init(|| Mutex::new(None));
    let mut cached = cache.lock().await;
    let today = ist_date();
    if let Some((cache_date, contracts)) = cached.as_ref()
        && *cache_date == today
    {
        return Ok(contracts.clone());
    }

    let response = state
        .http
        .get(MASTER_URL)
        .timeout(MASTER_TIMEOUT)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!(
                "Unable to download Angel One contract master: {error}"
            ))
        })?
        .error_for_status()
        .map_err(|error| {
            AppError::BadRequest(format!("Angel One contract master failed: {error}"))
        })?;
    let body = response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("Unable to read Angel One contract master: {error}"))
    })?;
    let contracts: Vec<MasterContract> = serde_json::from_slice(&body).map_err(|error| {
        AppError::BadRequest(format!("Invalid Angel One contract master: {error}"))
    })?;
    if contracts.is_empty() {
        return Err(AppError::BadRequest(
            "Angel One contract master is empty.".into(),
        ));
    }

    let contracts = Arc::new(contracts);
    *cached = Some((today, contracts.clone()));
    Ok(contracts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_fields_accept_string_and_numeric_values() {
        let contract: MasterContract = serde_json::from_value(serde_json::json!({
            "token": 123,
            "symbol": "SILVERM30NOV26FUT",
            "name": "SILVERM",
            "expiry": "30NOV2026",
            "strike": 0,
            "lotsize": 5,
            "instrumenttype": "FUTCOM",
            "exch_seg": "MCX"
        }))
        .unwrap();

        assert_eq!(contract.token, "123");
        assert_eq!(contract.lotsize, "5");
        assert_eq!(contract.strike, "0");
    }
}
