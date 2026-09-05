//! Server-wide security middleware.
//!
//! Three independent pieces, all wired in `main.rs`:
//! - [`apply_security_headers`] — hardening response headers on every response.
//! - [`redacted_request_span`] — a tracing span that records the request path
//!   only, never the query string (so tokens carried in URLs never hit logs).
//! - [`IpRateLimiter`] / [`ip_rate_limit`] — a reusable per-IP rate limiter the
//!   main router uses as a global backstop and the API / OAuth / webhook
//!   sub-routers reuse with stricter quotas.

use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::Arc;

use axum::{
    Extension,
    extract::{ConnectInfo, Request},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use governor::{Quota, RateLimiter, state::keyed::DefaultKeyedStateStore};
use sqlx::PgPool;

use crate::server::rate_limit::SharedRateLimiter;

// ============================================================================
// Response headers
// ============================================================================

/// Apply hardening headers to a response.
///
/// `hsts` (true in production, mirrors `SECURE_COOKIES`) gates
/// `Strict-Transport-Security`, which must only be advertised over HTTPS.
///
/// `Content-Security-Policy: frame-ancestors 'none'` and `X-Frame-Options: DENY`
/// together block clickjacking; the app is never meant to be framed.
pub fn apply_security_headers(headers: &mut HeaderMap, hsts: bool) {
    use axum::http::HeaderValue;

    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    if hsts {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
}

// ============================================================================
// Request span (query-redacted)
// ============================================================================

/// Build a request span that records the method and path only.
///
/// OAuth and SSE endpoints carry tokens in the query string; recording the full
/// URI would leak them into logs regardless of log level. We record `path()`,
/// which excludes the query.
pub fn redacted_request_span<B>(req: &axum::http::Request<B>) -> tracing::Span {
    tracing::info_span!(
        "request",
        method = %req.method(),
        path = %req.uri().path(),
    )
}

// ============================================================================
// Per-IP rate limiter
// ============================================================================

/// Keyed rate limiter: one token bucket per client-IP string.
type KeyedLimiter =
    RateLimiter<String, DefaultKeyedStateStore<String>, governor::clock::DefaultClock>;

/// Where the counters live.
#[derive(Clone)]
enum Backend {
    /// Per-process token buckets. No coordination, so an N-replica deployment
    /// allows up to N× the quota — acceptable for a coarse flood backstop.
    Local(Arc<KeyedLimiter>),
    /// Counters in PostgreSQL, shared by every replica. Costs one round-trip
    /// per request, so reserve it for low-volume routes.
    Shared(SharedRateLimiter),
}

/// A reusable per-IP rate limiter, paired with [`ip_rate_limit`] as middleware.
///
/// Insert one as an `Extension` next to the middleware on any router; nested
/// sub-routers can each carry their own quota.
#[derive(Clone)]
pub struct IpRateLimiter {
    backend: Backend,
    trust_proxy_headers: bool,
}

/// How often idle per-IP buckets are reclaimed. Governor's keyed store never shrinks on its own,
/// so without this every client IP the process has ever seen occupies an entry until restart.
const KEY_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

impl IpRateLimiter {
    /// Allow `per_minute` requests per client IP per minute, counted in-process.
    ///
    /// `trust_proxy_headers` mirrors `AuthConfig::trust_proxy_headers`: when set,
    /// the client IP is read from the trusted proxy's `X-Forwarded-For` hop rather
    /// than the socket peer address (see [`rate_limit_key`]).
    pub fn per_minute(per_minute: u32, trust_proxy_headers: bool) -> Self {
        let quota = Quota::per_minute(NonZeroU32::new(per_minute).expect("per_minute must be > 0"));
        let limiter = Arc::new(RateLimiter::keyed(quota));

        // Spawned here rather than left to each call site: these limiters are constructed in six
        // different routers, and a leak that depends on remembering to start a sweeper is a leak.
        // The task holds a Weak, so it stops on its own if the limiter is ever dropped.
        let weak = Arc::downgrade(&limiter);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(KEY_SWEEP_INTERVAL);
            loop {
                ticker.tick().await;
                match weak.upgrade() {
                    Some(limiter) => limiter.retain_recent(),
                    None => return,
                }
            }
        });

        Self {
            backend: Backend::Local(limiter),
            trust_proxy_headers,
        }
    }

    /// Same quota, but counted in PostgreSQL so it holds across replicas.
    ///
    /// `scope` namespaces the keys, so routers with different quotas don't draw
    /// from the same bucket.
    pub fn shared_per_minute(
        pool: PgPool,
        scope: &str,
        per_minute: u32,
        trust_proxy_headers: bool,
    ) -> Self {
        Self {
            backend: Backend::Shared(SharedRateLimiter::per_minute(pool, scope, per_minute)),
            trust_proxy_headers,
        }
    }

    /// Takes `&String` rather than `&str` deliberately: governor's keyed limiter
    /// is keyed by `String`, so a `&str` here would allocate one on every
    /// request — including static assets, which is the hottest path in the app.
    /// The caller already owns the key.
    #[allow(clippy::ptr_arg)]
    async fn check(&self, key: &String) -> bool {
        match &self.backend {
            Backend::Local(limiter) => limiter.check_key(key).is_ok(),
            // Fail open on database errors: every route behind a shared limiter
            // needs the same database to serve a real response, so rejecting
            // here would convert a database blip into a hard outage while
            // denying an attacker nothing. The global in-process backstop still
            // applies.
            Backend::Shared(limiter) => match limiter.check(key).await {
                Ok(allowed) => allowed,
                Err(e) => {
                    tracing::error!("shared rate limit check failed, allowing request: {e}");
                    true
                }
            },
        }
    }
}

/// The bucket key for a request: the client IP as our own proxy saw it.
///
/// With `trust_proxy_headers`, the **rightmost** `X-Forwarded-For` entry is used — the one hop
/// appended by the trusted proxy in front of us, which the client cannot choose. Everything left
/// of it arrived inside the client's own request: keying on the leftmost entry would let any
/// client mint a fresh bucket per request with `X-Forwarded-For: <random>`, silently disabling
/// every per-IP quota in the process. A deployment with more than one proxy hop must make its
/// edge append to (never pass through) the client-supplied header — nginx's
/// `$proxy_add_x_forwarded_for` and every managed CDN already do.
fn rate_limit_key(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    trust_proxy_headers: bool,
) -> String {
    if trust_proxy_headers
        && let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(last) = xff.split(',').next_back()
        && let Ok(ip) = last.trim().parse::<IpAddr>()
    {
        return format!("fwd:{ip}");
    }

    match peer {
        Some(addr) => format!("peer:{}", addr.ip()),
        None => "peer:unknown".to_string(),
    }
}

/// Whether this client is inside `limiter`'s quota. Logs when it is not.
///
/// Takes the pieces rather than the `Request` (whose body is `!Sync`, which would make every
/// calling middleware's future `!Send`). Public for middlewares that need a non-default
/// rejection — the ingest router's 429 must carry the Sentry backoff headers and CORS, not
/// [`ip_rate_limit`]'s plain body.
pub async fn within_quota(
    limiter: &IpRateLimiter,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> bool {
    let key = rate_limit_key(headers, peer, limiter.trust_proxy_headers);

    let allowed = limiter.check(&key).await;
    if !allowed {
        tracing::warn!(rate_limit_key = %key, "Rate limit exceeded");
    }
    allowed
}

/// The socket peer, as recorded by `into_make_service_with_connect_info::<SocketAddr>()`.
pub fn peer_addr(request: &Request) -> Option<SocketAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr)
}

/// Axum middleware enforcing the [`IpRateLimiter`] found in request extensions.
///
/// Returns `429 Too Many Requests` when the per-IP quota is exhausted. The peer
/// address requires the server to be served with
/// `into_make_service_with_connect_info::<SocketAddr>()`.
pub async fn ip_rate_limit(
    Extension(limiter): Extension<IpRateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let peer = peer_addr(&request);
    if within_quota(&limiter, request.headers(), peer).await {
        next.run(request).await
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests. Please try again later.",
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_peer_when_proxy_not_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.1".parse().unwrap());
        let peer = "203.0.113.7:443".parse().unwrap();

        assert_eq!(
            rate_limit_key(&headers, Some(peer), false),
            "peer:203.0.113.7"
        );
        assert_eq!(
            rate_limit_key(&headers, Some(peer), true),
            "fwd:198.51.100.1"
        );
    }

    #[test]
    fn takes_the_hop_our_own_proxy_appended() {
        // The proxy appends the address it saw to whatever the client sent, so the rightmost
        // entry is the trustworthy one.
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "10.0.0.1, 198.51.100.9".parse().unwrap());
        assert_eq!(rate_limit_key(&headers, None, true), "fwd:198.51.100.9");
    }

    #[test]
    fn a_client_cannot_choose_its_bucket_by_prepending_hops() {
        // An attacker sends X-Forwarded-For: <random> per request; the proxy appends the real
        // address. Keying on anything but the rightmost entry would hand out a fresh bucket
        // per request — every per-IP quota silently disabled.
        let peer = "127.0.0.1:80".parse().unwrap();
        for junk in ["1.2.3.4", "5.6.7.8"] {
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-forwarded-for",
                format!("{junk}, 198.51.100.9").parse().unwrap(),
            );
            assert_eq!(
                rate_limit_key(&headers, Some(peer), true),
                "fwd:198.51.100.9",
                "the spoofed hop must not become the key"
            );
        }
    }

    #[test]
    fn security_headers_gate_hsts() {
        let mut headers = HeaderMap::new();
        apply_security_headers(&mut headers, false);
        assert_eq!(headers.get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
        assert!(headers.get(header::STRICT_TRANSPORT_SECURITY).is_none());

        apply_security_headers(&mut headers, true);
        assert!(headers.get(header::STRICT_TRANSPORT_SECURITY).is_some());
    }
}
