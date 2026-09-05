use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),

    #[error("{0}")]
    Unauthorized(String),

    #[error("payload too large")]
    PayloadTooLarge,

    /// Over the ingest quota. Carries the number of seconds the client should back off for.
    #[error("rate limit exceeded")]
    RateLimited { retry_after: u64 },

    #[error("not found")]
    NotFound,

    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AppError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            AppError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Database(err) => {
                // Never surface database detail to a client; it may contain row data.
                tracing::error!(error = %err, "database error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        let detail = match &self {
            AppError::Database(_) => "internal server error".to_string(),
            other => other.to_string(),
        };

        let mut response = (status, Json(json!({ "detail": detail }))).into_response();

        // Sentry SDKs surface `X-Sentry-Error` in their debug logs, which is the only way a
        // developer sees why their events are being dropped.
        if status.is_client_error()
            && let Ok(value) = HeaderValue::from_str(&sanitize_header(&detail))
        {
            response.headers_mut().insert("x-sentry-error", value);
        }

        if let AppError::RateLimited { retry_after } = &self {
            let headers = response.headers_mut();
            headers.insert(header::RETRY_AFTER, header::HeaderValue::from(*retry_after));
            // `retry_after:categories:scope:reason_code` — see the rate limiting spec. SDKs read
            // the first three fields and ignore the rest.
            if let Ok(value) =
                HeaderValue::from_str(&format!("{retry_after}:error:project:quota_exceeded"))
            {
                headers.insert("x-sentry-rate-limits", value);
            }
        }

        response
    }
}

/// Header values may not contain control characters, and a message built from user input can.
fn sanitize_header(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                ' '
            }
        })
        .take(200)
        .collect()
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_sets_both_backoff_headers() {
        let response = AppError::RateLimited { retry_after: 60 }.into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "60");
        assert_eq!(
            response.headers()["x-sentry-rate-limits"],
            "60:error:project:quota_exceeded"
        );
    }

    #[test]
    fn client_errors_carry_x_sentry_error() {
        let response = AppError::Unauthorized("bad key".into()).into_response();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["x-sentry-error"], "bad key");
    }

    #[test]
    fn header_sanitizer_strips_control_characters() {
        assert_eq!(sanitize_header("bad\nkey\r\0"), "bad key  ");
    }
}
