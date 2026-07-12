use anyhow::{Context, Result};
use ipnet::IpNet;
use lettre::message::Mailbox;
use std::{env, net::IpAddr};
use url::Url;

#[derive(Clone)]
pub struct Config {
    pub app_env: String,
    pub host: IpAddr,
    pub port: u16,
    pub database_url: String,
    pub frontend_origins: Vec<String>,
    pub otp_ttl_minutes: i64,
    pub otp_max_attempts: i32,
    pub otp_resend_cooldown_seconds: i64,
    pub otp_hash_key: Vec<u8>,
    pub rate_limit_window_seconds: u64,
    pub login_rate_limit: usize,
    pub otp_rate_limit: usize,
    pub sensitive_rate_limit: usize,
    pub login_lockout_threshold: i32,
    pub login_lockout_base_seconds: i64,
    pub login_lockout_max_seconds: i64,
    pub max_request_body_bytes: usize,
    pub trusted_proxies: Vec<IpNet>,
    pub session_idle_minutes: i64,
    pub session_absolute_hours: i64,
    pub smtp_host: Option<String>,
    pub smtp_port: u16,
    pub smtp_username: Option<String>,
    pub smtp_password: Option<String>,
    pub smtp_from: String,
    pub angel_api_base: String,
    pub angel_ws_url: String,
    pub client_public_ip: String,
    pub client_local_ip: String,
    pub client_mac_address: String,
    pub alert_webhook_url: Option<String>,
    pub alert_email_to: Option<String>,
    pub force_demo_trading: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| env::var(key).ok())
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Result<Self> {
        let app_env = lookup("APP_ENV").unwrap_or_else(|| "development".into());
        let production = app_env.eq_ignore_ascii_case("production");
        let otp_hash_key = non_empty(&mut lookup, "OTP_HASH_KEY")
            .unwrap_or_else(|| "rulenix-local-development-otp-key-material".into())
            .into_bytes();
        if production && otp_hash_key.len() < 32 {
            anyhow::bail!("OTP_HASH_KEY must contain at least 32 bytes in production");
        }
        let trusted_proxies = non_empty(&mut lookup, "TRUSTED_PROXIES")
            .map(|value| {
                value
                    .split(',')
                    .map(|part| {
                        part.trim()
                            .parse::<IpNet>()
                            .with_context(|| format!("invalid trusted proxy network: {part}"))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let frontend_origins = parse_origins(
            non_empty(&mut lookup, "FRONTEND_ORIGINS")
                .or_else(|| non_empty(&mut lookup, "FRONTEND_ORIGIN"))
                .unwrap_or_else(|| "http://localhost:5173".into()),
        )?;
        let config = Self {
            app_env,
            host: lookup("HOST")
                .unwrap_or_else(|| "0.0.0.0".into())
                .parse()
                .context("invalid HOST")?,
            port: lookup("PORT")
                .unwrap_or_else(|| "8080".into())
                .parse()
                .context("invalid PORT")?,
            database_url: lookup("DATABASE_URL").context("DATABASE_URL is required")?,
            frontend_origins,
            otp_ttl_minutes: lookup("OTP_TTL_MINUTES")
                .unwrap_or_else(|| "10".into())
                .parse()
                .context("invalid OTP_TTL_MINUTES")?,
            otp_max_attempts: parse_env(&mut lookup, "OTP_MAX_ATTEMPTS", 5)?,
            otp_resend_cooldown_seconds: parse_env(&mut lookup, "OTP_RESEND_COOLDOWN_SECONDS", 60)?,
            otp_hash_key,
            rate_limit_window_seconds: parse_env(&mut lookup, "RATE_LIMIT_WINDOW_SECONDS", 60)?,
            login_rate_limit: parse_env(&mut lookup, "LOGIN_RATE_LIMIT", 10)?,
            otp_rate_limit: parse_env(&mut lookup, "OTP_RATE_LIMIT", 5)?,
            sensitive_rate_limit: parse_env(&mut lookup, "SENSITIVE_RATE_LIMIT", 10)?,
            login_lockout_threshold: parse_env(&mut lookup, "LOGIN_LOCKOUT_THRESHOLD", 5)?,
            login_lockout_base_seconds: parse_env(&mut lookup, "LOGIN_LOCKOUT_BASE_SECONDS", 30)?,
            login_lockout_max_seconds: parse_env(&mut lookup, "LOGIN_LOCKOUT_MAX_SECONDS", 3600)?,
            max_request_body_bytes: parse_env(&mut lookup, "MAX_REQUEST_BODY_BYTES", 65_536)?,
            trusted_proxies,
            session_idle_minutes: lookup("SESSION_IDLE_MINUTES")
                .unwrap_or_else(|| "30".into())
                .parse()
                .context("invalid SESSION_IDLE_MINUTES")?,
            session_absolute_hours: lookup("SESSION_ABSOLUTE_HOURS")
                .unwrap_or_else(|| "24".into())
                .parse()
                .context("invalid SESSION_ABSOLUTE_HOURS")?,
            smtp_host: non_empty(&mut lookup, "SMTP_HOST"),
            smtp_port: lookup("SMTP_PORT")
                .unwrap_or_else(|| "587".into())
                .parse()
                .context("invalid SMTP_PORT")?,
            smtp_username: non_empty(&mut lookup, "SMTP_USERNAME"),
            smtp_password: non_empty(&mut lookup, "SMTP_PASSWORD"),
            smtp_from: lookup("SMTP_FROM").unwrap_or_else(|| "noreply@rulenix.local".into()),
            angel_api_base: lookup("ANGEL_API_BASE")
                .unwrap_or_else(|| "https://apiconnect.angelone.in".into()),
            angel_ws_url: lookup("ANGEL_WS_URL")
                .unwrap_or_else(|| "wss://smartapisocket.angelone.in/smart-stream".into()),
            client_public_ip: lookup("CLIENT_PUBLIC_IP").unwrap_or_else(|| "127.0.0.1".into()),
            client_local_ip: lookup("CLIENT_LOCAL_IP").unwrap_or_else(|| "127.0.0.1".into()),
            client_mac_address: lookup("CLIENT_MAC_ADDRESS")
                .unwrap_or_else(|| "00:00:00:00:00:00".into()),
            alert_webhook_url: non_empty(&mut lookup, "ALERT_WEBHOOK_URL"),
            alert_email_to: non_empty(&mut lookup, "ALERT_EMAIL_TO"),
            force_demo_trading: parse_env(&mut lookup, "FORCE_DEMO_TRADING", false)?,
        };
        config.validate(&mut lookup)?;
        Ok(config)
    }

    pub fn is_production(&self) -> bool {
        self.app_env.eq_ignore_ascii_case("production")
    }

    fn validate(&self, mut lookup: impl FnMut(&str) -> Option<String>) -> Result<()> {
        if self.otp_ttl_minutes <= 0
            || self.otp_max_attempts <= 0
            || self.otp_resend_cooldown_seconds <= 0
            || self.rate_limit_window_seconds == 0
            || self.login_rate_limit == 0
            || self.otp_rate_limit == 0
            || self.sensitive_rate_limit == 0
            || self.login_lockout_threshold <= 0
            || self.login_lockout_base_seconds <= 0
            || self.login_lockout_max_seconds < self.login_lockout_base_seconds
            || self.max_request_body_bytes < 1024
        {
            anyhow::bail!(
                "security limits must be positive and MAX_REQUEST_BODY_BYTES must be at least 1024"
            );
        }
        validate_url(&self.angel_api_base, "ANGEL_API_BASE", &["https"])?;
        validate_url(&self.angel_ws_url, "ANGEL_WS_URL", &["wss"])?;
        self.smtp_from
            .parse::<Mailbox>()
            .context("invalid SMTP_FROM")?;
        if let Some(value) = &self.alert_webhook_url {
            validate_url(value, "ALERT_WEBHOOK_URL", &["https"])?;
        }
        if let Some(value) = &self.alert_email_to {
            value.parse::<Mailbox>().context("invalid ALERT_EMAIL_TO")?;
        }
        let public_ip: IpAddr = self
            .client_public_ip
            .parse()
            .context("invalid CLIENT_PUBLIC_IP")?;
        self.client_local_ip
            .parse::<IpAddr>()
            .context("invalid CLIENT_LOCAL_IP")?;
        validate_mac(&self.client_mac_address)?;
        validate_database_url(&self.database_url, self.is_production())?;
        if self.is_production() {
            validate_production_secrets(self, &mut lookup, public_ip)?;
        }
        Ok(())
    }
}

fn non_empty(lookup: &mut impl FnMut(&str) -> Option<String>, key: &str) -> Option<String> {
    lookup(key).filter(|value| !value.trim().is_empty())
}

fn parse_env<T>(lookup: &mut impl FnMut(&str) -> Option<String>, key: &str, default: T) -> Result<T>
where
    T: std::str::FromStr + ToString,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    lookup(key)
        .unwrap_or_else(|| default.to_string())
        .parse()
        .with_context(|| format!("invalid {key}"))
}

fn parse_origins(value: String) -> Result<Vec<String>> {
    let origins = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|origin| {
            let parsed = Url::parse(origin).with_context(|| format!("invalid origin: {origin}"))?;
            if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
                anyhow::bail!("origin must not include path, query, or fragment: {origin}");
            }
            Ok(origin.trim_end_matches('/').to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    if origins.is_empty() {
        anyhow::bail!("at least one frontend origin is required");
    }
    Ok(origins)
}

fn validate_url(value: &str, key: &str, schemes: &[&str]) -> Result<Url> {
    let parsed = Url::parse(value).with_context(|| format!("invalid {key}"))?;
    if !schemes.iter().any(|scheme| parsed.scheme() == *scheme) {
        anyhow::bail!(
            "{key} must use one of these schemes: {}",
            schemes.join(", ")
        );
    }
    if parsed.host_str().is_none() {
        anyhow::bail!("{key} must include a host");
    }
    Ok(parsed)
}

fn validate_database_url(value: &str, production: bool) -> Result<()> {
    let parsed = validate_url(value, "DATABASE_URL", &["postgres", "postgresql"])?;
    if parsed.username().is_empty() || parsed.password().is_none() {
        anyhow::bail!("DATABASE_URL must include database credentials");
    }
    if production {
        let query = parsed.query().unwrap_or("");
        let ssl_ok = query.contains("sslmode=verify-full") || query.contains("sslmode=require");
        if !ssl_ok {
            anyhow::bail!(
                "production DATABASE_URL must include sslmode=verify-full or sslmode=require"
            );
        }
        if matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1")) {
            anyhow::bail!("production DATABASE_URL must not point at localhost");
        }
    }
    Ok(())
}

fn validate_mac(value: &str) -> Result<()> {
    let parts: Vec<&str> = value.split([':', '-']).collect();
    if parts.len() != 6
        || parts
            .iter()
            .any(|part| part.len() != 2 || !part.chars().all(|c| c.is_ascii_hexdigit()))
    {
        anyhow::bail!("CLIENT_MAC_ADDRESS must be a six-byte MAC address");
    }
    Ok(())
}

fn validate_production_secrets(
    config: &Config,
    lookup: &mut impl FnMut(&str) -> Option<String>,
    public_ip: IpAddr,
) -> Result<()> {
    for origin in &config.frontend_origins {
        let parsed = Url::parse(origin)?;
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
        {
            anyhow::bail!("production frontend origins must be concrete HTTPS origins");
        }
    }
    let required = [
        "DATABASE_URL",
        "CREDENTIAL_ENCRYPTION_PRIMARY_VERSION",
        "CREDENTIAL_ENCRYPTION_KEYS",
        "OTP_HASH_KEY",
        "SMTP_HOST",
        "SMTP_USERNAME",
        "SMTP_PASSWORD",
        "SMTP_FROM",
        "CLIENT_PUBLIC_IP",
        "CLIENT_LOCAL_IP",
        "CLIENT_MAC_ADDRESS",
        "FORCE_DEMO_TRADING",
    ];
    for key in required {
        let value = lookup(key)
            .filter(|value| !value.trim().is_empty())
            .with_context(|| format!("{key} is required in production"))?;
        if looks_like_placeholder(&value) {
            anyhow::bail!("{key} must not contain an example or placeholder value in production");
        }
    }
    if config.smtp_host.is_none()
        || config.smtp_username.is_none()
        || config.smtp_password.is_none()
    {
        anyhow::bail!("SMTP_HOST, SMTP_USERNAME, and SMTP_PASSWORD are required in production");
    }
    if public_ip.is_loopback() || public_ip.is_unspecified() {
        anyhow::bail!("CLIENT_PUBLIC_IP must be a routable broker-registered IP in production");
    }
    if config.client_mac_address == "00:00:00:00:00:00" {
        anyhow::bail!("CLIENT_MAC_ADDRESS must be the broker-registered MAC in production");
    }
    Ok(())
}

fn looks_like_placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("replace")
        || lower.contains("example")
        || lower.contains("inject")
        || lower.contains("change-this")
        || lower.contains("12345678")
        || lower.contains("password")
        || lower.contains("noreply@rulenix.local")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config_from(values: &[(&str, &str)]) -> Result<Config> {
        let map = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>();
        Config::from_lookup(|key| map.get(key).cloned())
    }

    fn production_values() -> Vec<(&'static str, &'static str)> {
        vec![
            ("APP_ENV", "production"),
            (
                "DATABASE_URL",
                "postgres://rulenix:strong-secret@db.rulenix.internal:5432/rulenix?sslmode=verify-full",
            ),
            (
                "FRONTEND_ORIGINS",
                "https://app.rulenix.internal,https://admin.rulenix.internal",
            ),
            ("OTP_HASH_KEY", "prod-otp-key-material-alpha-bravo"),
            ("CREDENTIAL_ENCRYPTION_PRIMARY_VERSION", "1"),
            (
                "CREDENTIAL_ENCRYPTION_KEYS",
                "1:AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=",
            ),
            ("SMTP_HOST", "smtp.rulenix.internal"),
            ("SMTP_USERNAME", "mailer@rulenix.internal"),
            ("SMTP_PASSWORD", "smtp-secret-value"),
            ("SMTP_FROM", "\"Rulenix Alerts\" <alerts@rulenix.internal>"),
            ("CLIENT_PUBLIC_IP", "203.0.113.10"),
            ("CLIENT_LOCAL_IP", "10.0.0.10"),
            ("CLIENT_MAC_ADDRESS", "12:34:56:78:9A:BC"),
            ("FORCE_DEMO_TRADING", "false"),
        ]
    }

    #[test]
    fn quoted_smtp_sender_names_parse() {
        let config = config_from(&production_values()).unwrap();
        assert_eq!(
            config.smtp_from,
            "\"Rulenix Alerts\" <alerts@rulenix.internal>"
        );
        assert_eq!(config.frontend_origins.len(), 2);
    }

    #[test]
    fn production_rejects_insecure_frontend_origin() {
        let mut values = production_values();
        values.retain(|(key, _)| *key != "FRONTEND_ORIGINS");
        values.push(("FRONTEND_ORIGIN", "http://localhost:5173"));
        assert!(config_from(&values).is_err());
    }

    #[test]
    fn production_rejects_placeholder_secrets() {
        let mut values = production_values();
        values.retain(|(key, _)| *key != "SMTP_PASSWORD");
        values.push(("SMTP_PASSWORD", "REPLACE_WITH_SECRET"));
        assert!(config_from(&values).is_err());
    }
}
