use crate::{
    auth::AuthUser,
    error::{AppError, AppResult},
};
use axum::{
    Json,
    extract::{Extension, Query},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentQuery {
    pub filename: String,
    pub lines: Option<usize>,
    pub tail: Option<bool>,
    pub since_session: Option<bool>,
}

fn logs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("logs")
}
fn safe_username(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>()
        .to_uppercase()
}

pub async fn append(username: &str, message: &str) {
    use tokio::io::AsyncWriteExt;
    let path = logs_dir().join(format!("{}_market.log", safe_username(username)));
    if let Ok(mut file) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
    {
        let line = format!("{} {}\n", Utc::now().to_rfc3339(), message);
        let _ = file.write_all(line.as_bytes()).await;
    }
}

pub async fn files(Extension(user): Extension<AuthUser>) -> AppResult<Json<Value>> {
    let prefix = safe_username(&user.username);
    let mut files = Vec::new();
    let mut entries = tokio::fs::read_dir(logs_dir())
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| AppError::Internal(e.into()))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.to_uppercase().starts_with(&prefix) && name.ends_with(".log") {
            let meta = entry
                .metadata()
                .await
                .map_err(|e| AppError::Internal(e.into()))?;
            let modified: DateTime<Utc> = meta
                .modified()
                .map(DateTime::from)
                .unwrap_or_else(|_| Utc::now());
            files.push(json!({"filename":name,"username":prefix,"size":meta.len(),"size_mb":(meta.len() as f64 / 1_048_576.0 * 100.0).round()/100.0,"modified":modified,"modified_display":modified.format("%Y-%m-%d %H:%M:%S").to_string()}));
        }
    }
    files.sort_by(|a, b| b["modified"].as_str().cmp(&a["modified"].as_str()));
    Ok(Json(json!({"count":files.len(),"files":files})))
}

pub async fn content(
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ContentQuery>,
) -> AppResult<Json<Value>> {
    let allowed_prefix = safe_username(&user.username);
    if query.filename.contains(['/', '\\'])
        || query.filename.contains("..")
        || !query.filename.to_uppercase().starts_with(&allowed_prefix)
    {
        return Err(AppError::BadRequest("Invalid filename.".into()));
    }
    let path = logs_dir().join(&query.filename);
    let raw = tokio::fs::read_to_string(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::NotFound("Log file not found.".into())
        } else {
            AppError::Internal(e.into())
        }
    })?;
    let lines: Vec<&str> = raw.lines().collect();
    let count = query.lines.unwrap_or(500).clamp(1, 5000);
    let selected = if query.since_session.unwrap_or(false) {
        let marker = lines.iter().rposition(|line| {
            line.contains("MARKET DATA SESSION") || line.contains("BROKER SESSION")
        });
        &lines[marker.unwrap_or(lines.len().saturating_sub(count))..]
    } else if query.tail.unwrap_or(true) {
        &lines[lines.len().saturating_sub(count)..]
    } else {
        &lines[..lines.len().min(count)]
    };
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(Json(
        json!({"filename":query.filename,"content":selected.join("\n"),"lines_returned":selected.len(),"size":meta.len(),"size_mb":meta.len() as f64/1_048_576.0}),
    ))
}
