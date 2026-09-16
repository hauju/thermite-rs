//! Router assembly: every route and middleware layer the server runs.
//!
//! Extracted from `main` so the HTTP tests drive the *same* stack the binary
//! does. Wiring assembled separately in a test proves only that the test's
//! wiring works; the ordering here — rate limiting outside the extensions it
//! reads, security headers outside the handlers whose errors they must also
//! cover — is exactly the part worth protecting.

use std::sync::Arc;

use axum::{Extension, Router};
use tower_http::CompressionLevel;
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

    // Lax rather than tower-sessions' default Strict: claude.ai opens the
    // OAuth consent page by a top-level cross-site navigation, and Strict
    // withholds the cookie on it — so a user who was already signed in got
    // the login form on every MCP connect. Cross-site POSTs carry no cookie
    // under Lax either, and dx-auth's origin check covers those.
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(app_state.config.secure_cookies)
        .with_same_site(tower_sessions::cookie::SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(Duration::days(7)))
        .with_signed(
            tower_sessions::cookie::Key::try_from(app_state.secrets.session_secret.as_slice())
                .expect("Invalid session secret"),
        );

    let ferriskey = app_state.config.ferriskey.clone();
    let auth_config = auth::AuthConfig {
        login_page_url: "/login".to_string(),
        default_post_login_url: "/dashboard".to_string(),
        dev_login_url: "/login".to_string(),
        // Empty in local mode. The FerrisKey handlers are not mounted there, so nothing reads
        // these; leaving them as `Default` keeps the one source of truth on `config.ferriskey`.
        ferriskey_url: ferriskey
            .as_ref()
            .map(|f| f.url.clone())
            .unwrap_or_default(),
        ferriskey_issuer_url: ferriskey.as_ref().and_then(|f| f.issuer_url.clone()),
        ferriskey_realm: ferriskey
            .as_ref()
            .map(|f| f.realm.clone())
            .unwrap_or_default(),
        ferriskey_client_id: ferriskey
            .as_ref()
            .map(|f| f.client_id.clone())
            .unwrap_or_default(),
        ferriskey_client_secret: app_state.secrets.ferriskey_client_secret.clone(),
        base_url: app_state.config.base_url.clone(),
        trust_proxy_headers: app_state.config.trust_proxy_headers,
        allowed_registration_emails: app_state.config.allowed_registration_emails.clone(),
        allowed_registration_domains: app_state.config.allowed_registration_domains.clone(),
        // The server-side code exchange, not the browser-redirect SSO mode: that needs a public
        // FerrisKey client and a same-site identity provider, and thermite.rs is neither.
        sso_enabled: false,
        // Closed after the first account unless the allowlists say otherwise; the waitlist is
        // how a stranger gets in (see `waitlist.rs`).
        open_registration: false,
        // Offer the password step exactly when there is a password to check — the admin
        // credential. dx-auth treats a verified one as authorization by itself.
        password_login: app_state.secrets.admin_password_hash.is_some(),
        // The version every existing account accepted under dx-auth 0.4, which hard-coded it.
        // Bumping it re-prompts everyone on their next login. The terms are the hosted
        // service's, and the pages they link to exist only with the site on: a self-hosted
        // instance has no terms step at all.
        tos_version: app_state.config.site.then(|| "1.0".to_string()),
    };

    let user_store = Arc::new(AppAuthUserStore::new(app_state.clone()));
    let passkey_store = Arc::new(server::passkey_store::AppAuthPasskeyStore::new(
        app_state.clone(),
    ));
    let email_sender =
        AppEmailSender::new(&app_state).map(|s| Arc::new(s) as Arc<dyn auth::AuthEmailSender>);

    // Count auth attempts in PostgreSQL so the quota is enforced once across every replica,
    // not once per process.
    let rate_limit_store = Some(Arc::new(server::rate_limit::AppAuthRateLimitStore::new(
        app_state.db.pool.clone(),
        auth::AUTH_REQUESTS_PER_MINUTE,
    )) as Arc<dyn auth::AuthRateLimitStore>);

    // One router or the other, never both: they own the same `/auth/session/*` paths.
    let auth_routes = match app_state.config.sign_in {
        crate::server::config::SignInMode::FerrisKey => {
            let auth_state = auth::AuthState {
                user_store,
                email_sender,
                jwks_cache: app_state
                    .jwks
                    .clone()
                    .expect("FerrisKey mode always builds a JWKS cache"),
                rate_limit_store,
                passkey_store,
            };
            auth::auth_router(auth_config, auth_state)
        }
        crate::server::config::SignInMode::Local => {
            let mut auth_state = match email_sender {
                Some(sender) => auth::AuthState::local(user_store, sender, passkey_store),
                None => auth::AuthState::local_without_email(user_store, passkey_store),
            };
            auth_state.rate_limit_store = rate_limit_store;
            auth::local_auth_router(auth_config, auth_state)
        }
    };

    // HSTS is only safe over HTTPS, so gate it on the same flag as secure cookies.
    let hsts = app_state.config.secure_cookies;
    let demo = app_state.config.demo_autologin;
    let site = app_state.config.site;
    let trust_proxy = app_state.config.trust_proxy_headers;
    // Unset: no tracker tag is written into any page, whatever UMAMI_HOST says.
    let umami_website_id = app_state.config.umami_website_id.clone();
    // Global per-IP backstop against abuse; sensitive sub-routers add stricter quotas, and the
    // ingest paths are exempted below in favour of their own limiter. High enough that a burst
    // of asset requests from an office NAT full of dashboard users never trips it.
    let global_rate_limiter = server::security::IpRateLimiter::per_minute(3000, trust_proxy);
    let pool = app_state.db.pool.clone();

    // Public sandbox: GET /demo signs anyone in as the shared demo user (see server::demo_login).
    let demo_login = if demo {
        tracing::warn!(
            "THERMITE_DEMO_AUTOLOGIN is set: GET /demo signs anyone in as the shared demo user, with write access to every project"
        );
        server::demo_login::demo_login_router()
    } else {
        Router::new()
    };

    base.merge(auth_routes)
        .merge(demo_login)
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
        // GET /og.png — the share card; the demo instance serves its own (see src/server/og).
        .merge(server::og::og_router(demo))
        // GET /robots.txt and /sitemap.xml (see src/server/seo).
        .merge(server::seo::seo_router(
            &app_state.config.base_url,
            demo,
            site,
        ))
        // GET /health (liveness, used by the Docker HEALTHCHECK) and /ready (readiness).
        .merge(server::health::health_router())
        // GET /llms.txt — orientation page so an agent can discover the MCP/REST surface itself.
        .merge(server::llms::llms_router(&app_state.config.base_url))
        // GET /stats.js and POST /api/send: the self-hosted Umami tracker, proxied same-origin
        // so ad-block lists cannot silently drop it. Both 404 unless UMAMI_HOST is set.
        .merge(umami::proxy::routes())
        // Innermost, inside the compression layer: it reads the HTML body, adding `lang` and
        // the analytics tag (see src/server/seo).
        .layer(axum::middleware::from_fn_with_state(
            umami_website_id,
            server::seo::patch_shell,
        ))
        // The marketing pages, when this instance is not the marketing site (see `site_route`).
        .layer(axum::middleware::from_fn_with_state(site, marketing_site))
        .layer(session_layer)
        // Brotli 6: 10-20% smaller than gzip at ~4 ms per page. The library default
        // (brotli 11) costs ~150 ms of CPU per 250 KiB response.
        .layer(CompressionLayer::new().quality(CompressionLevel::Precise(6)))
        .layer(Extension(app_state))
        // Per-IP rate-limit backstop (Extension must sit outside the middleware).
        .layer(axum::middleware::from_fn(backstop_except_ingest))
        .layer(Extension(global_rate_limiter))
        // Hardening headers on every response (including errors above), and `noindex` on the
        // pages that must stay out of the search index (see src/server/seo).
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| async move {
                let noindex = demo || server::seo::is_noindex_path(req.uri().path());
                let mut res = next.run(req).await;
                server::security::apply_security_headers(res.headers_mut(), hsts);
                if noindex {
                    server::seo::mark_noindex(res.headers_mut());
                }
                res
            },
        ))
        // Outermost: a request span that records the path only (never the query).
        .layer(TraceLayer::new_for_http().make_span_with(server::security::redacted_request_span))
}

/// What a marketing page answers on an instance that is not the marketing site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Blocked {
    /// `/`: the application's front door, since that is all this instance is.
    Redirect,
    /// About, pricing and the legal pages — Hauke's, not a self-hoster's, and nothing here links to
    /// them once the site is off.
    NotFound,
}

/// Which paths the marketing site owns. Everything else, the docs included, is the application
/// and passes through.
fn site_route(path: &str) -> Option<Blocked> {
    match path {
        "/" => Some(Blocked::Redirect),
        "/about" | "/pricing" => Some(Blocked::NotFound),
        _ if path.starts_with("/legal/") => Some(Blocked::NotFound),
        _ => None,
    }
}

/// Answers the marketing paths before SSR runs, unless `THERMITE_SITE` is set. Doing it here
/// rather than in the Dioxus tree means the landing page is never rendered and never flashes,
/// and the client bundle — built long before an operator sets the variable — needs no say in it.
async fn marketing_site(
    axum::extract::State(site): axum::extract::State<bool>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::{Method, StatusCode, header};
    use axum::response::IntoResponse;

    if site || !matches!(*request.method(), Method::GET | Method::HEAD) {
        return next.run(request).await;
    }
    match site_route(request.uri().path()) {
        Some(Blocked::Redirect) => {
            (StatusCode::FOUND, [(header::LOCATION, "/dashboard")]).into_response()
        }
        Some(Blocked::NotFound) => StatusCode::NOT_FOUND.into_response(),
        None => next.run(request).await,
    }
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
