//! Router assembly: every route and middleware layer the server runs.
//!
//! Extracted from `main` so the HTTP tests drive the *same* stack the binary
//! does. Wiring assembled separately in a test proves only that the test's
//! wiring works; the ordering here — rate limiting outside the extensions it
//! reads, security headers outside the handlers whose errors they must also
//! cover — is exactly the part worth protecting.

use std::sync::Arc;

use axum::{Extension, Router};
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tower_sessions::cookie::time::Duration;
use tower_sessions::{ExpiredDeletion, Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::PostgresStore;

use crate::server;
use crate::server::auth_store::{AppAuthUserStore, AppEmailSender};
use crate::server::state::AppState;

/// Mount the application's routes and middleware onto `base`.
///
/// `main` passes the Dioxus SSR router as `base`. Tests pass `Router::new()`,
/// which exercises everything except server-side rendering — that needs a built
/// client bundle on disk and is not what these tests are about.
pub async fn build(base: Router, app_state: AppState) -> Router {
    // PostgreSQL session store (reuses the application connection pool).
    let session_store = PostgresStore::new(app_state.db.pool.clone());
    session_store
        .migrate()
        .await
        .expect("Failed to migrate session store");

    // Drop elapsed rate-limit windows periodically (see server::rate_limit).
    server::rate_limit::spawn_sweeper(app_state.db.background_pool.clone());

    // Drop events past the retention policy, hourly. Without this a self-hosted instance grows
    // until the disk fills; the rollup, issues and analyses survive so history does not.
    // On the background pool: a batched delete must not occupy an interactive connection.
    thermite_core::retention::spawn(
        app_state.db.background_pool.clone(),
        thermite_core::RetentionPolicy::from_env(),
        std::time::Duration::from_secs(60 * 60),
    );

    // Raise an event for every cron monitor that failed to check in. Every minute, because the
    // point of the feature is noticing promptly; a sweep with nothing overdue is one indexed query.
    thermite_core::monitors::spawn(
        app_state.db.background_pool.clone(),
        std::time::Duration::from_secs(60),
    );

    // Deliver new-issue/regression alerts to email/webhook, if configured (see server::alerts).
    server::alerts::spawn(app_state.clone());

    // Keep the public demo project showing recent activity, if one is configured.
    server::demo_feed::spawn(app_state.clone());

    // Prune expired sessions hourly so the table doesn't grow unbounded.
    tokio::task::spawn(
        session_store
            .clone()
            .continuously_delete_expired(tokio::time::Duration::from_secs(60 * 60)),
    );

    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(app_state.config.secure_cookies)
        .with_expiry(Expiry::OnInactivity(Duration::days(7)))
        .with_signed(
            tower_sessions::cookie::Key::try_from(app_state.secrets.session_secret.as_slice())
                .expect("Invalid session secret"),
        );

    let auth_config = auth::AuthConfig {
        login_page_url: "/login".to_string(),
        default_post_login_url: "/dashboard".to_string(),
        dev_login_url: "/login".to_string(),
        ferriskey_url: app_state.config.ferriskey_url.clone(),
        ferriskey_issuer_url: app_state.config.ferriskey_issuer_url.clone(),
        ferriskey_realm: app_state.config.ferriskey_realm.clone(),
        ferriskey_client_id: app_state.config.ferriskey_client_id.clone(),
        ferriskey_client_secret: app_state.secrets.ferriskey_client_secret.clone(),
        base_url: app_state.config.base_url.clone(),
        trust_proxy_headers: app_state.config.trust_proxy_headers,
        allowed_registration_emails: app_state.config.allowed_registration_emails.clone(),
        allowed_registration_domains: app_state.config.allowed_registration_domains.clone(),
    };

    let auth_state = auth::AuthState {
        user_store: Arc::new(AppAuthUserStore::new(app_state.clone())),
        email_sender: Arc::new(AppEmailSender::new(app_state.clone())),
        jwks_cache: app_state.jwks.clone(),
        // Count auth attempts in PostgreSQL so the quota is enforced once
        // across every replica, not once per process.
        rate_limit_store: Some(Arc::new(server::rate_limit::AppAuthRateLimitStore::new(
            app_state.db.pool.clone(),
            auth::AUTH_REQUESTS_PER_MINUTE,
        ))),
    };

    let auth_routes = auth::auth_router(auth_config, auth_state);

    // HSTS is only safe over HTTPS, so gate it on the same flag as secure cookies.
    let hsts = app_state.config.secure_cookies;
    let trust_proxy = app_state.config.trust_proxy_headers;
    // Global per-IP backstop against abuse; sensitive sub-routers add stricter quotas, and the
    // ingest paths are exempted below in favour of their own limiter. High enough that a burst
    // of asset requests from an office NAT full of dashboard users never trips it.
    let global_rate_limiter = server::security::IpRateLimiter::per_minute(3000, trust_proxy);
    let pool = app_state.db.pool.clone();

    base.merge(auth_routes)
        // OAuth 2.1 authorization server + MCP connector (see src/server/oauth, mcp).
        .merge(server::oauth::oauth_router(pool.clone(), trust_proxy))
        .merge(server::mcp::mcp_router(app_state.clone(), trust_proxy))
        // Sentry-compatible ingest. Public by necessity — SDKs authenticate with a DSN key, not
        // a session — so it sits outside everything session-related.
        .merge(server::thermite::ingest_router(
            app_state.clone(),
            trust_proxy,
        ))
        // The read and triage API, behind the same API auth as the rest of the application.
        .merge(server::thermite::api_router(app_state.clone()))
        // PWA manifest, service worker, and app icons (see src/server/pwa).
        .merge(server::pwa::pwa_router())
        // GET /health (liveness, used by the Docker HEALTHCHECK) and /ready (readiness).
        .merge(server::health::health_router())
        // GET /llms.txt — orientation page so an agent can discover the MCP/REST surface itself.
        .merge(server::llms::llms_router())
        .layer(session_layer)
        .layer(CompressionLayer::new())
        .layer(Extension(app_state))
        // Per-IP rate-limit backstop (Extension must sit outside the middleware).
        .layer(axum::middleware::from_fn(backstop_except_ingest))
        .layer(Extension(global_rate_limiter))
        // Hardening headers on every response (including errors above).
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| async move {
                let mut res = next.run(req).await;
                server::security::apply_security_headers(res.headers_mut(), hsts);
                res
            },
        ))
        // Outermost: a request span that records the path only (never the query).
        .layer(TraceLayer::new_for_http().make_span_with(server::security::redacted_request_span))
}

/// The global backstop, leaving the ingest paths to their own limiter: ingest's 429 carries the
/// Sentry backoff headers and CORS, which this middleware's plain rejection would strip from
/// exactly the SDKs that honour them.
async fn backstop_except_ingest(
    limiter: Extension<server::security::IpRateLimiter>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if server::thermite::is_ingest_path(request.uri().path()) {
        return next.run(request).await;
    }
    server::security::ip_rate_limit(limiter, request, next).await
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
