use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("Too many requests. Try again in {retry_after} seconds.")]
    RateLimited { retry_after: u64 },
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let mut retry_after = None;
        let (status, detail) = match self {
            Self::BadRequest(v) => (StatusCode::BAD_REQUEST, v),
            Self::Unauthorized(v) => (StatusCode::UNAUTHORIZED, v),
            Self::Forbidden(v) => (StatusCode::FORBIDDEN, v),
            Self::NotFound(v) => (StatusCode::NOT_FOUND, v),
            Self::RateLimited {
                retry_after: seconds,
            } => {
                retry_after = Some(seconds);
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    format!("Too many requests. Try again in {seconds} seconds."),
                )
            }
            Self::Sqlx(error) => {
                tracing::error!(?error, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Database operation failed".into(),
                )
            }
            Self::Internal(error) => {
                tracing::error!(?error, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".into(),
                )
            }
        };
        let mut response = (
            status,
            Json(json!({
                "detail": detail,
                "retry_after": retry_after,
            })),
        )
            .into_response();
        if let Some(seconds) = retry_after {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from_str(&seconds.to_string()).expect("integer header value"),
            );
        }
        response
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_response_includes_retry_after_header() {
        let response = AppError::RateLimited { retry_after: 17 }.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "17");
    }
}
