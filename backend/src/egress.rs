use crate::{
    auth::{AuthUser, require_admin_permission},
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
    http::HeaderMap,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::{IpAddr, Ipv4Addr};
use uuid::Uuid;

const BROKEN_EGRESS_MESSAGE: &str =
    "Configured Angel egress IP is unavailable; broker operation blocked.";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EgressIpView {
    pub id: Uuid,
    pub ip_address: String,
    pub configuration_status: String,
    pub verification_status: String,
    pub status_message: String,
    pub configured_at: Option<DateTime<Utc>>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub assigned_user_id: Option<Uuid>,
    pub assigned_username: Option<String>,
    pub assigned_broker_account: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddEgressIp {
    pub ip_address: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignEgressIp {
    pub egress_ip_id: Option<Uuid>,
}

#[cfg(unix)]
#[derive(Debug, Serialize, Deserialize)]
struct HelperRequest<'a> {
    operation: &'a str,
    ip_address: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct HelperResponse {
    ok: bool,
    configured: bool,
    verified: bool,
    observed_ip: Option<String>,
    message: String,
}

#[derive(Debug, sqlx::FromRow)]
struct EgressSelection {
    ip_address: String,
    configuration_status: String,
    verification_status: String,
}

pub fn validate_public_ipv4(value: &str) -> AppResult<Ipv4Addr> {
    let ip: Ipv4Addr = value
        .trim()
        .parse()
        .map_err(|_| AppError::BadRequest("Enter a valid public IPv4 address.".into()))?;
    let octets = ip.octets();
    let invalid = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip == Ipv4Addr::BROADCAST
        || octets[0] == 0
        || octets[0] >= 240
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 169 && octets[1] == 254)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113);
    if invalid {
        return Err(AppError::BadRequest(
            "The Angel egress address must be a globally routable public IPv4 address.".into(),
        ));
    }
    Ok(ip)
}

pub fn binding_ipv4(public: Ipv4Addr) -> Ipv4Addr {
    let raw = u32::from(public);
    let slot = (raw ^ (raw >> 22)).wrapping_mul(0x9e37_79b1) & 0x003f_ffff;
    Ipv4Addr::new(
        100,
        64 + ((slot >> 16) & 0x3f) as u8,
        ((slot >> 8) & 0xff) as u8,
        (slot & 0xff) as u8,
    )
}

fn inventory_query() -> &'static str {
    "SELECT e.id,host(e.ip_address) AS ip_address,e.configuration_status,e.verification_status,
            e.status_message,e.configured_at,e.last_verified_at,e.created_at,e.updated_at,
            p.user_id AS assigned_user_id,u.username AS assigned_username,
            p.brokerage_user_id AS assigned_broker_account
       FROM broker_egress_ips e
       LEFT JOIN user_profiles p ON p.broker_egress_ip_id=e.id
       LEFT JOIN users u ON u.id=p.user_id
      ORDER BY e.ip_address"
}

pub async fn list(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
) -> AppResult<Json<Vec<EgressIpView>>> {
    require_admin_permission(&admin)?;
    let values = sqlx::query_as(inventory_query())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(values))
}

#[cfg(unix)]
async fn call_helper(state: &AppState, ip: Ipv4Addr) -> AppResult<HelperResponse> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let operation = async {
        let mut stream = UnixStream::connect(&state.config.egress_helper_socket)
            .await
            .map_err(|error| anyhow::anyhow!("restricted egress helper is unavailable: {error}"))?;
        let payload = serde_json::to_vec(&HelperRequest {
            operation: "configure_and_verify",
            ip_address: ip.to_string(),
        })?;
        stream.write_all(&payload).await?;
        stream.write_all(b"\n").await?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await?;
        if line.len() > 8_192 {
            anyhow::bail!("restricted egress helper returned an oversized response");
        }
        Ok::<_, anyhow::Error>(serde_json::from_str::<HelperResponse>(&line)?)
    };
    tokio::time::timeout(std::time::Duration::from_secs(30), operation)
        .await
        .map_err(|_| AppError::Internal(anyhow::anyhow!("restricted egress helper timed out")))?
        .map_err(AppError::Internal)
}

#[cfg(not(unix))]
async fn call_helper(_state: &AppState, _ip: Ipv4Addr) -> AppResult<HelperResponse> {
    Err(AppError::Internal(anyhow::anyhow!(
        "restricted egress configuration is supported only on the production Linux host"
    )))
}

async fn mark_failed(state: &AppState, id: Uuid, configuration: bool, message: &str) {
    let bounded: String = message.chars().take(512).collect();
    let query = if configuration {
        "UPDATE broker_egress_ips SET configuration_status='CONFIGURATION_FAILED',verification_status='UNVERIFIED',status_message=$2,updated_at=NOW() WHERE id=$1"
    } else {
        "UPDATE broker_egress_ips SET verification_status='VERIFICATION_FAILED',status_message=$2,updated_at=NOW() WHERE id=$1"
    };
    if let Err(error) = sqlx::query(query)
        .bind(id)
        .bind(bounded)
        .execute(&state.db)
        .await
    {
        tracing::error!(%error, %id, "could not persist Angel egress failure status");
    }
}

async fn configure_and_verify(state: &AppState, id: Uuid, force: bool) -> AppResult<EgressIpView> {
    let row: Option<EgressSelection> = sqlx::query_as(
        "SELECT host(ip_address) AS ip_address,configuration_status,verification_status
           FROM broker_egress_ips WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let row = row.ok_or_else(|| AppError::NotFound("Egress IP not found.".into()))?;
    if !force && row.configuration_status == "CONFIGURED" && row.verification_status == "VERIFIED" {
        return sqlx::query_as(&format!(
            "{} WHERE e.id=$1",
            inventory_query().replace(" ORDER BY e.ip_address", "")
        ))
        .bind(id)
        .fetch_one(&state.db)
        .await
        .map(Json)
        .map(|value| value.0)
        .map_err(Into::into);
    }
    let ip = validate_public_ipv4(&row.ip_address)?;
    sqlx::query("UPDATE broker_egress_ips SET configuration_status='CONFIGURING',verification_status='VERIFYING',status_message='',updated_at=NOW() WHERE id=$1")
        .bind(id)
        .execute(&state.db)
        .await?;
    let response = match call_helper(state, ip).await {
        Ok(value) => value,
        Err(error) => {
            mark_failed(state, id, true, &error.to_string()).await;
            return Err(AppError::BadRequest(format!(
                "Could not configure Angel egress IP {ip}: {error}"
            )));
        }
    };
    if !response.configured {
        mark_failed(state, id, true, &response.message).await;
        return Err(AppError::BadRequest(format!(
            "Could not configure Angel egress IP {ip}: {}",
            response.message
        )));
    }
    if !response.ok
        || !response.verified
        || response.observed_ip.as_deref() != Some(&ip.to_string())
    {
        mark_failed(state, id, false, &response.message).await;
        return Err(AppError::BadRequest(format!(
            "Outbound verification failed for Angel egress IP {ip}: {}",
            response.message
        )));
    }
    sqlx::query("UPDATE broker_egress_ips SET configuration_status='CONFIGURED',verification_status='VERIFIED',status_message='',configured_at=COALESCE(configured_at,NOW()),last_verified_at=NOW(),updated_at=NOW() WHERE id=$1")
        .bind(id)
        .execute(&state.db)
        .await?;
    let query = format!(
        "{} WHERE e.id=$1",
        inventory_query().replace(" ORDER BY e.ip_address", "")
    );
    Ok(sqlx::query_as(&query).bind(id).fetch_one(&state.db).await?)
}

pub async fn add(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
    headers: HeaderMap,
    Json(input): Json<AddEgressIp>,
) -> AppResult<Json<Value>> {
    require_admin_permission(&admin)?;
    let ip = validate_public_ipv4(&input.ip_address)?;
    let inserted: Result<Uuid, sqlx::Error> = sqlx::query_scalar(
        "INSERT INTO broker_egress_ips(ip_address,created_by) VALUES($1::inet,$2) RETURNING id",
    )
    .bind(ip.to_string())
    .bind(admin.id)
    .fetch_one(&state.db)
    .await;
    let id = match inserted {
        Ok(id) => id,
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|value| value.is_unique_violation()) =>
        {
            return Err(AppError::BadRequest(
                "That egress IPv4 is already registered.".into(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let configured = configure_and_verify(&state, id, false).await?;
    let _ = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: None,
            headers: Some(&headers),
            event_type: "angel_egress_ip_registered",
            actor_user_id: Some(admin.id),
            target_user_id: None,
            summary: "Administrator registered and verified an Angel egress IP",
            metadata: json!({"egress_ip_id":id,"ip_address":ip.to_string()}),
        },
    )
    .await;
    Ok(Json(json!({"egress_ip":configured})))
}

pub async fn verify(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    require_admin_permission(&admin)?;
    let value = configure_and_verify(&state, id, true).await?;
    Ok(Json(json!({"egress_ip":value})))
}

pub async fn assign(
    State(state): State<AppState>,
    Extension(admin): Extension<AuthUser>,
    Path(user_id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<AssignEgressIp>,
) -> AppResult<Json<Value>> {
    require_admin_permission(&admin)?;
    if let Some(id) = input.egress_ip_id {
        configure_and_verify(&state, id, false).await?;
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::uuid::text,0))")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    if let Some(id) = input.egress_ip_id {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::uuid::text,1))")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let ready: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM broker_egress_ips WHERE id=$1 AND configuration_status='CONFIGURED' AND verification_status='VERIFIED')")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if !ready {
            return Err(AppError::BadRequest(BROKEN_EGRESS_MESSAGE.into()));
        }
    }
    let updated = sqlx::query(
        "UPDATE user_profiles SET broker_egress_ip_id=$2,updated_at=NOW() WHERE user_id=$1",
    )
    .bind(user_id)
    .bind(input.egress_ip_id)
    .execute(&mut *tx)
    .await;
    match updated {
        Ok(result) if result.rows_affected() == 0 => {
            return Err(AppError::NotFound("Angel broker account not found.".into()));
        }
        Ok(_) => {}
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|value| value.is_unique_violation()) =>
        {
            return Err(AppError::BadRequest(
                "That dedicated egress IP is already assigned to another Angel account.".into(),
            ));
        }
        Err(error) => return Err(error.into()),
    }
    tx.commit().await?;
    let mode = if input.egress_ip_id.is_some() {
        "explicit"
    } else {
        "default"
    };
    tracing::info!(%user_id, egress_mode=mode, egress_ip_id=?input.egress_ip_id, "Angel egress assignment changed");
    let _ = crate::audit::record(
        &state,
        crate::audit::AuditEvent {
            context: None,
            headers: Some(&headers),
            event_type: "angel_egress_assignment_changed",
            actor_user_id: Some(admin.id),
            target_user_id: Some(user_id),
            summary: "Administrator changed an Angel static egress assignment",
            metadata: json!({"egress_mode":mode,"egress_ip_id":input.egress_ip_id}),
        },
    )
    .await;
    Ok(Json(
        json!({"user_id":user_id,"egress_mode":mode,"egress_ip_id":input.egress_ip_id}),
    ))
}

pub async fn source_ip_for_user(state: &AppState, user_id: Uuid) -> AppResult<Option<Ipv4Addr>> {
    let selection: Option<EgressSelection> = sqlx::query_as(
        "SELECT host(e.ip_address) AS ip_address,e.configuration_status,e.verification_status
           FROM user_profiles p
           JOIN broker_egress_ips e ON e.id=p.broker_egress_ip_id
          WHERE p.user_id=$1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(selection) = selection else {
        tracing::debug!(%user_id, egress_mode="default", "selected Angel networking");
        return Ok(None);
    };
    let ip = validate_public_ipv4(&selection.ip_address).map_err(|_| {
        AppError::Forbidden(format!(
            "Configured Angel egress IP {} is unavailable; broker operation blocked.",
            selection.ip_address
        ))
    })?;
    if selection.configuration_status != "CONFIGURED" || selection.verification_status != "VERIFIED"
    {
        return Err(AppError::Forbidden(format!(
            "Configured Angel egress IP {ip} is unavailable; broker operation blocked."
        )));
    }
    let binding_ip = binding_ipv4(ip);
    tracing::debug!(%user_id, egress_mode="explicit", egress_ip=%ip, %binding_ip, "selected Angel networking");
    Ok(Some(binding_ip))
}

pub async fn rehydrate_configured_ips(state: &AppState) {
    let ids: Result<Vec<Uuid>, sqlx::Error> = sqlx::query_scalar(
        "SELECT id FROM broker_egress_ips WHERE configuration_status='CONFIGURED' ORDER BY created_at",
    )
    .fetch_all(&state.db)
    .await;
    match ids {
        Ok(ids) => {
            for id in ids {
                if let Err(error) = configure_and_verify(state, id, true).await {
                    tracing::error!(%id, %error, "could not rehydrate configured Angel egress IP; explicit assignments remain fail-closed");
                }
            }
        }
        Err(error) => {
            tracing::error!(%error, "could not load Angel egress inventory for startup rehydration")
        }
    }
}

pub async fn http_client_for_user(state: &AppState, user_id: Uuid) -> AppResult<reqwest::Client> {
    let Some(ip) = source_ip_for_user(state, user_id).await? else {
        return Ok(state.http.clone());
    };
    build_http_client(Some(ip))
}

fn build_http_client(source: Option<Ipv4Addr>) -> AppResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .no_proxy();
    if let Some(ip) = source {
        builder = builder.local_address(IpAddr::V4(ip));
    }
    builder
        .build()
        .map_err(|error| AppError::Internal(error.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ipv4_validation_rejects_non_public_ranges() {
        for value in [
            "not-an-ip",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.0.1",
            "169.254.1.1",
            "224.0.0.1",
            "0.0.0.0",
            "100.64.0.1",
            "192.0.2.1",
        ] {
            assert!(validate_public_ipv4(value).is_err(), "accepted {value}");
        }
        assert_eq!(
            validate_public_ipv4("51.161.140.103").unwrap(),
            Ipv4Addr::new(51, 161, 140, 103)
        );
    }

    #[test]
    fn public_egress_addresses_have_stable_private_binding_aliases() {
        let primary = binding_ipv4(Ipv4Addr::new(139, 99, 155, 62));
        let additional = binding_ipv4(Ipv4Addr::new(51, 161, 140, 103));
        assert_ne!(primary, additional);
        for alias in [primary, additional] {
            let octets = alias.octets();
            assert_eq!(octets[0], 100);
            assert!((64..=127).contains(&octets[1]));
        }
    }

    async fn observed_peer(client: reqwest::Client) -> std::io::Result<Ipv4Addr> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (mut stream, peer) = listener.accept().await?;
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await?;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await?;
            Ok::<_, std::io::Error>(peer.ip())
        });
        client
            .get(format!("http://{address}"))
            .send()
            .await
            .map_err(std::io::Error::other)?;
        match server.await.map_err(std::io::Error::other)?? {
            IpAddr::V4(ip) => Ok(ip),
            IpAddr::V6(_) => Err(std::io::Error::other("expected IPv4 peer")),
        }
    }

    #[tokio::test]
    async fn explicit_client_uses_the_bound_tcp_source() {
        let observed = observed_peer(build_http_client(Some(Ipv4Addr::new(127, 0, 0, 2))).unwrap())
            .await
            .unwrap();
        assert_eq!(observed, Ipv4Addr::new(127, 0, 0, 2));
    }

    #[tokio::test]
    async fn default_client_has_no_hard_coded_production_source() {
        let observed = observed_peer(build_http_client(None).unwrap())
            .await
            .unwrap();
        assert_eq!(observed, Ipv4Addr::LOCALHOST);
    }

    #[tokio::test]
    async fn unavailable_explicit_source_does_not_fall_back() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let result = build_http_client(Some(Ipv4Addr::new(192, 0, 2, 123)))
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .await;
        assert!(result.is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    }
}
