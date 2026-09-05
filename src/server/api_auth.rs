//! Dual-auth extractor for machine-to-machine API requests.
//!
//! Authenticates a request via **either** an `oat_` API key (sent as `X-API-Key`
//! or `Authorization: Bearer oat_…`) **or** a FerrisKey-issued JWT
//! (`Authorization: Bearer <jwt>`). Browser/session auth is handled separately by
//! [`auth::UserSession`]; this path is for API clients and the MCP connector.

use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::header;
use axum::http::request::Parts;

use crate::models::AppError;
use crate::models::user::UserEntity;
use crate::server::api_key;
use crate::server::state::AppState;
use crate::server::user;

/// How a request authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthVia {
    ApiKey,
    Jwt,
}

/// An authenticated API caller, resolved to the owning user.
///
/// Both fields are deliberately unread today: Thermite is single-tenant, so authentication is a
/// binary gate and every authenticated principal is an operator over every project. `require_api_auth`
/// resolves this value and drops it. That is a decision, not an oversight — but it is also the only
/// place a caller's identity exists, so this struct is the seam any future per-project authorization
/// would be threaded through. The `expect` below fails once something reads a field, which is the
/// signal that the scoping work has begun and this note needs revisiting.
#[expect(
    dead_code,
    reason = "single-tenant trust model: the resolved identity is intentionally not consulted"
)]
pub struct ApiAuth {
    pub user: UserEntity,
    pub via: AuthVia,
}

impl<S: Send + Sync> FromRequestParts<S> for ApiAuth {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, AppError> {
        let state = parts
            .extensions
            .get::<AppState>()
            .cloned()
            .ok_or_else(|| AppError::Internal("AppState not in extensions".to_string()))?;
        authenticate(&state, &parts.headers).await
    }
}

/// Authenticate from request headers: `X-API-Key`, `Authorization: Bearer oat_…`,
/// or `Authorization: Bearer <jwt>`. Shared by the [`ApiAuth`] extractor and the
/// MCP tools (which only have the request `Parts`, not an extractor context).
pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<ApiAuth, AppError> {
    // 1. Explicit API-key header.
    if let Some(key) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        return resolve_api_key(state, key.trim()).await;
    }

    // 2. Authorization: Bearer <token> — oat_ keys and JWTs share this header.
    if let Some(token) = bearer_token(headers) {
        if token.starts_with("oat_") {
            return resolve_api_key(state, token).await;
        }
        return resolve_jwt(state, token).await;
    }

    Err(AppError::Unauthorized)
}

/// Extract the bearer token, treating the scheme case-insensitively per RFC 7235.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

async fn resolve_api_key(state: &AppState, token: &str) -> Result<ApiAuth, AppError> {
    let entity = api_key::verify(&state.db, token)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let user = user::find_by_id(&state.db, entity.user_id)
        .await?
        .ok_or(AppError::Unauthorized)?;

    Ok(ApiAuth {
        user,
        via: AuthVia::ApiKey,
    })
}

async fn resolve_jwt(state: &AppState, token: &str) -> Result<ApiAuth, AppError> {
    let claims = state
        .jwks
        .validate_token(token)
        .await
        .map_err(|_| AppError::Unauthorized)?;

    let user = user::find_by_sub(&state.db, &claims.sub)
        .await?
        .ok_or(AppError::Unauthorized)?;

    Ok(ApiAuth {
        user,
        via: AuthVia::Jwt,
    })
}
