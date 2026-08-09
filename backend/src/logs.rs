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

const MAX_LOG_READ_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentQuery {
    pub filename: String,
    pub lines: Option<usize>,
    pub tail: Option<bool>,
    pub since_session: Option<bool>,
}

fn logs_dir() -> PathBuf {
    std::env::var_os("RULENIX_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("logs"))
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
    let dir = logs_dir();
    if let Err(error) = tokio::fs::create_dir_all(&dir).await {
        tracing::warn!(%error, path=%dir.display(), "could not create market log directory");
        return;
    }
    let path = dir.join(format!("{}_market.log", safe_username(username)));
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
    let dir = logs_dir();
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Json(json!({"count":0,"files":[]})));
        }
        Err(error) => return Err(AppError::Internal(error.into())),
    };
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

async fn read_log_text(path: &Path, tail: bool) -> AppResult<(String, std::fs::Metadata)> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let meta = tokio::fs::metadata(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::NotFound("Log file not found.".into())
        } else {
            AppError::Internal(e.into())
        }
    })?;
    if meta.len() <= MAX_LOG_READ_BYTES {
        let raw = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        return Ok((raw, meta));
    }
    if !tail {
        return Err(AppError::BadRequest(
            "Log file is too large; use the tail view.".into(),
        ));
    }

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    file.seek(std::io::SeekFrom::Start(meta.len() - MAX_LOG_READ_BYTES))
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    let mut bytes = Vec::with_capacity(MAX_LOG_READ_BYTES as usize);
    file.read_to_end(&mut bytes)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    let text = String::from_utf8_lossy(&bytes)
        .split_once('\n')
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
    Ok((text, meta))
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
    let tail = query.tail.unwrap_or(true);
    let (raw, meta) = read_log_text(&path, tail).await?;
    let lines: Vec<&str> = raw.lines().collect();
    let count = query.lines.unwrap_or(500).clamp(1, 5000);
    let selected = if query.since_session.unwrap_or(false) {
        let marker = lines.iter().rposition(|line| {
            line.contains("MARKET DATA SESSION") || line.contains("BROKER SESSION")
        });
        &lines[marker.unwrap_or(lines.len().saturating_sub(count))..]
    } else if tail {
        &lines[lines.len().saturating_sub(count)..]
    } else {
        &lines[..lines.len().min(count)]
    };
    Ok(Json(
        json!({"filename":query.filename,"content":selected.join("\n"),"lines_returned":selected.len(),"size":meta.len(),"size_mb":meta.len() as f64/1_048_576.0}),
    ))
}
