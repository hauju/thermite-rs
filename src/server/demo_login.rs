//! `GET /demo`: sign in as the shared demo user, no credentials asked.
//!
//! Mounted only when `THERMITE_DEMO_AUTOLOGIN` is set, which turns the instance into a public
//! sandbox: projects are instance-wide, so every visitor becomes the same user with write access
//! to all of them. That is the point on demo.thermite.rs and a disaster anywhere else, which is
//! why the flag defaults off, the route does not exist without it, and boot logs a warning.

use auth::types::NewAuthUser;
use auth::{AuthError, AuthResult, AuthUserStore, LoggedInData, login};
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Extension, Router};
use serde::Deserialize;

use crate::server::auth_store::AppAuthUserStore;
use crate::server::state::AppState;

const DEMO_SUB: &str = "demo";
const DEMO_EMAIL: &str = "demo@thermite.rs";
const DEMO_USERNAME: &str = "demo";

/// Where a signed-in visitor lands.
pub const AFTER_LOGIN: &str = "/dashboard";

pub fn demo_login_router() -> Router {
    Router::new().route("/demo", get(demo_login))
}

#[derive(Deserialize)]
struct Next {
    /// Where to go afterwards — the page a visitor deep-linked into, typically.
    next: Option<String>,
}

/// Only a path on this site: `next` comes off the URL, and a redirect to another host would make
/// `/demo` an open redirector.
fn destination(next: Option<&str>) -> &str {
    match next {
        Some(path)
            if path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\") =>
        {
            path
        }
        _ => AFTER_LOGIN,
    }
}

async fn demo_login(
    Extension(state): Extension<AppState>,
    Query(query): Query<Next>,
    session: tower_sessions::Session,
) -> Response {
    match sign_in(state, session).await {
        Ok(()) => Redirect::to(destination(query.next.as_deref())).into_response(),
        Err(error) => {
            tracing::error!(%error, "demo login failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "demo login failed").into_response()
        }
    }
}

async fn sign_in(state: AppState, session: tower_sessions::Session) -> AuthResult<()> {
    let store = AppAuthUserStore::new(state);
    let user = match store.get_user_by_sub(DEMO_SUB).await? {
        Some(user) => user,
        None => {
            store
                .create_user(NewAuthUser {
                    sub: DEMO_SUB.to_string(),
                    email: DEMO_EMAIL.to_string(),
                })
                .await?
        }
    };

    // A fresh session id per sign-in, so one visitor never continues another's session.
    session
        .cycle_id()
        .await
        .map_err(|e| AuthError::ServerStateError(format!("session error: {e}")))?;

    login(
        &session,
        &LoggedInData {
            id: user.id,
            sub: user.sub,
            email: user.email,
            username: DEMO_USERNAME.to_string(),
            avatar_url: None,
            // The auth crate's marker for a session with no identity provider behind it:
            // logout then ends the local session instead of calling FerrisKey.
            id_token: "dev_mode_token".to_string(),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{AFTER_LOGIN, destination};

    #[test]
    fn only_a_local_path_is_a_destination() {
        assert_eq!(
            destination(Some("/issues/42?tab=events")),
            "/issues/42?tab=events"
        );
        assert_eq!(destination(None), AFTER_LOGIN);
        assert_eq!(destination(Some("")), AFTER_LOGIN);
        assert_eq!(destination(Some("https://evil.example/")), AFTER_LOGIN);
        assert_eq!(destination(Some("//evil.example/")), AFTER_LOGIN);
        assert_eq!(destination(Some("/\\evil.example/")), AFTER_LOGIN);
    }
}
