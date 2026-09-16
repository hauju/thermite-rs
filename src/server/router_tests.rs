use super::*;
use crate::server::db::Database;
use crate::server::test_support::test_state;
use sqlx::PgPool;

/// Serve the real router on a loopback port and return its base URL.
///
/// Uses `into_make_service_with_connect_info` exactly as `main` does — the
/// per-IP rate limiters read the peer address from it, so without it they
/// would silently key every caller the same way.
async fn serve(pool: PgPool) -> String {
    serve_with_base_url(pool, None).await
}

/// Serve with an overridden `base_url`, so tests can exercise behaviour that
/// keys off the deployment's public hostname rather than the loopback
/// address the test listener actually binds.
async fn serve_with_base_url(pool: PgPool, base_url: Option<&str>) -> String {
    serve_with_state(pool, Router::new(), |state| {
        if let Some(url) = base_url {
            state.config.base_url = url.to_string();
        }
    })
    .await
}

/// Serve with the state adjusted first, for behaviour behind a configuration flag.
async fn serve_with_state(
    pool: PgPool,
    base: Router,
    adjust: impl FnOnce(&mut AppState),
) -> String {
    let mut state = test_state(Database::from_pool(pool));
    adjust(&mut state);
    let router = build(base, state).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

fn client() -> reqwest::Client {
    // Redirects off: several assertions are about the redirect itself.
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

#[sqlx::test]
async fn health_reports_ok_when_the_database_is_reachable(pool: PgPool) {
    let base = serve(pool).await;
    let res = client().get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "ok");
}

#[sqlx::test]
async fn ready_reports_ok_when_the_database_is_reachable(pool: PgPool) {
    let base = serve(pool).await;
    let res = client().get(format!("{base}/ready")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "ready");
}

/// The discovery page for agents is public: it describes the interface, never data, so an
/// agent handed nothing but a URL can find the MCP endpoint on its own.
#[sqlx::test]
async fn llms_txt_is_served_without_authentication(pool: PgPool) {
    let base = serve(pool).await;
    let res = client()
        .get(format!("{base}/llms.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    assert!(body.contains("POST /mcp"), "must point at the MCP endpoint");
    assert!(
        body.contains("claim_triage"),
        "must describe the triage loop"
    );
    assert!(
        body.contains("http://localhost:8099/llms-full.txt")
            && body.contains("http://localhost:8099/docs/getting-started/introduction.md"),
        "must point at the docs as Markdown:\n{body}"
    );
}

/// The docs are readable without rendering them: each page as Markdown, and all of them as
/// one file — the same content the pages are built from.
#[sqlx::test]
async fn docs_are_served_as_markdown(pool: PgPool) {
    let base = serve(pool).await;
    let page = client()
        .get(format!("{base}/docs/getting-started/introduction.md"))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200);
    assert_eq!(
        page.headers()["content-type"],
        "text/markdown; charset=utf-8"
    );
    let page = page.text().await.unwrap();
    assert!(page.contains("Thermite"), "{page}");

    let full = client()
        .get(format!("{base}/llms-full.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(full.status(), 200);
    let full = full.text().await.unwrap();
    assert!(
        full.contains("http://localhost:8099/docs/getting-started/introduction"),
        "{full}"
    );
    assert!(full.contains(&page), "the full file carries every page");
}

/// The share card is public, and the file is the size the tags declare: a scraper lays the
/// card out from `og:image:width` / `height` before fetching, and a mismatch shows no image.
#[sqlx::test]
async fn share_card_is_served_at_its_declared_size(pool: PgPool) {
    use crate::components::meta::{SHARE_IMAGE_HEIGHT, SHARE_IMAGE_WIDTH};

    let base = serve(pool).await;
    let res = client().get(format!("{base}/og.png")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "image/png");
    assert_eq!(
        res.headers()["cache-control"],
        "public, max-age=31536000, immutable",
        "the tags version the URL, so the file itself never changes"
    );
    let png = res.bytes().await.unwrap();
    assert_eq!(png_size(&png), (SHARE_IMAGE_WIDTH, SHARE_IMAGE_HEIGHT));
}

/// The demo instance serves its own card at the same URL, so the tags never have to know
/// which instance they are on.
#[sqlx::test]
async fn the_demo_instance_serves_the_demo_card(pool: PgPool) {
    use crate::components::meta::{SHARE_IMAGE_HEIGHT, SHARE_IMAGE_WIDTH};

    let plain = serve(pool.clone()).await;
    let demo = serve_with_state(pool, Router::new(), |state| {
        state.config.demo_autologin = true;
    })
    .await;
    let card = |base: String| async move {
        let res = client().get(format!("{base}/og.png")).send().await.unwrap();
        assert_eq!(res.status(), 200);
        res.bytes().await.unwrap()
    };
    let (plain, demo) = (card(plain).await, card(demo).await);
    assert_eq!(png_size(&demo), (SHARE_IMAGE_WIDTH, SHARE_IMAGE_HEIGHT));
    assert_ne!(plain, demo, "the demo instance must serve the demo card");
}

/// A stand-in for the SSR shell on routes the tests own, so the outbound middleware can be
/// exercised against a real page response.
fn shell_routes() -> Router {
    let page =
        || async { axum::response::Html("<!DOCTYPE html><html><head></head><body></body></html>") };
    Router::new()
        .route("/", axum::routing::get(page))
        .route("/about", axum::routing::get(page))
        .route("/pricing", axum::routing::get(page))
        .route("/legal/privacy", axum::routing::get(page))
        .route(
            "/docs/getting-started/introduction",
            axum::routing::get(page),
        )
        .route("/dashboard", axum::routing::get(page))
}

/// The marketing site is four paths and nothing else: the docs, the application pages and the
/// machine endpoints are the product, and a self-hoster keeps all of them.
#[test]
fn only_the_marketing_paths_belong_to_the_site() {
    assert_eq!(site_route("/"), Some(Blocked::Redirect));
    assert_eq!(site_route("/about"), Some(Blocked::NotFound));
    assert_eq!(site_route("/pricing"), Some(Blocked::NotFound));
    assert_eq!(site_route("/legal/privacy"), Some(Blocked::NotFound));
    for path in [
        "/dashboard",
        "/docs/getting-started/introduction",
        "/llms.txt",
        "/api/1/envelope/",
        "/pricing/enterprise",
        "/aboutus",
        "/legalese",
    ] {
        assert_eq!(site_route(path), None, "{path}");
    }
}

/// The published image is the application, not thermite.rs: without `THERMITE_SITE` the
/// landing page is the dashboard's front door and the marketing pages are gone. The docs stay
/// — a self-hoster needs the SDK, MCP and cron pages — and the sitemap lists only those, since
/// the marketing entries would point at the 404 below.
#[sqlx::test]
async fn without_the_site_flag_the_instance_is_only_the_application(pool: PgPool) {
    let base = serve_with_state(pool, shell_routes(), |_| {}).await;
    let root = client().get(format!("{base}/")).send().await.unwrap();
    assert_eq!(root.status(), 302);
    assert_eq!(root.headers()["location"], "/dashboard");

    for path in ["/about", "/pricing", "/legal/privacy"] {
        let res = client().get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(res.status(), 404, "{path}");
    }

    let docs = client()
        .get(format!("{base}/docs/getting-started/introduction"))
        .send()
        .await
        .unwrap();
    assert_eq!(docs.status(), 200, "the docs are the product, not the site");

    let sitemap = client()
        .get(format!("{base}/sitemap.xml"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!sitemap.contains("/about"), "{sitemap}");
    assert!(!sitemap.contains("/pricing"), "{sitemap}");
    assert!(!sitemap.contains("/legal/"), "{sitemap}");
    assert!(
        sitemap.contains("<loc>http://localhost:8099/docs/getting-started/introduction</loc>"),
        "{sitemap}"
    );
}

/// With the flag set — thermite.rs and demo.thermite.rs — the whole site is there.
#[sqlx::test]
async fn the_site_flag_serves_the_marketing_pages(pool: PgPool) {
    let base = serve_with_state(pool, shell_routes(), |state| state.config.site = true).await;
    for path in ["/", "/about", "/pricing", "/legal/privacy"] {
        let res = client().get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(res.status(), 200, "{path}");
    }

    let sitemap = client()
        .get(format!("{base}/sitemap.xml"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for loc in [
        "http://localhost:8099/",
        "http://localhost:8099/about",
        "http://localhost:8099/pricing",
    ] {
        assert!(sitemap.contains(&format!("<loc>{loc}</loc>")), "{loc}");
    }
}

/// `robots.txt` keeps crawlers off the machine endpoints and points at the sitemap, which
/// lists the public pages and the docs — and never an application page.
#[sqlx::test]
async fn robots_and_sitemap_cover_the_public_pages_only(pool: PgPool) {
    let base = serve_with_state(pool, Router::new(), |state| state.config.site = true).await;
    let robots = client()
        .get(format!("{base}/robots.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(robots.status(), 200);
    let robots = robots.text().await.unwrap();
    assert!(
        robots.contains("Sitemap: http://localhost:8099/sitemap.xml"),
        "{robots}"
    );
    assert!(robots.contains("Disallow: /api/"));
    assert!(
        !robots.contains("Disallow: /dashboard"),
        "app pages are noindex, never disallowed: link previews must still fetch them"
    );

    let sitemap = client()
        .get(format!("{base}/sitemap.xml"))
        .send()
        .await
        .unwrap();
    assert_eq!(sitemap.status(), 200);
    assert_eq!(
        sitemap.headers()["content-type"],
        "application/xml; charset=utf-8"
    );
    let sitemap = sitemap.text().await.unwrap();
    for loc in [
        "http://localhost:8099/",
        "http://localhost:8099/pricing",
        "http://localhost:8099/docs/getting-started/introduction",
    ] {
        assert!(
            sitemap.contains(&format!("<loc>{loc}</loc>")),
            "{loc}:\n{sitemap}"
        );
    }
    assert!(!sitemap.contains("/dashboard"));
}

/// The application pages say `noindex` themselves; the marketing pages do not.
#[sqlx::test]
async fn application_pages_are_noindex_and_marketing_pages_are_not(pool: PgPool) {
    let base = serve_with_state(pool, shell_routes(), |state| state.config.site = true).await;
    let dashboard = client()
        .get(format!("{base}/dashboard"))
        .send()
        .await
        .unwrap();
    assert_eq!(dashboard.headers()["x-robots-tag"], "noindex");
    let pricing = client()
        .get(format!("{base}/pricing"))
        .send()
        .await
        .unwrap();
    assert!(pricing.headers().get("x-robots-tag").is_none());
}

/// The demo instance duplicates every marketing page, so all of it is `noindex` and it
/// advertises no sitemap — while still answering, so its links keep unfurling.
#[sqlx::test]
async fn the_demo_instance_is_noindex_everywhere(pool: PgPool) {
    let base = serve_with_state(pool, shell_routes(), |state| {
        state.config.demo_autologin = true;
        state.config.site = true;
    })
    .await;
    let pricing = client()
        .get(format!("{base}/pricing"))
        .send()
        .await
        .unwrap();
    assert_eq!(pricing.status(), 200);
    assert_eq!(pricing.headers()["x-robots-tag"], "noindex");
    let robots = client()
        .get(format!("{base}/robots.txt"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!robots.contains("Sitemap:"), "{robots}");
    let sitemap = client()
        .get(format!("{base}/sitemap.xml"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        sitemap.status(),
        404,
        "a sitemap of noindex URLs is a contradiction"
    );
}

/// The SSR shell leaves `<html>` bare; the page goes out with a language, and nothing else
/// is touched.
#[sqlx::test]
async fn the_ssr_shell_gets_a_language(pool: PgPool) {
    let base = serve_with_state(pool, shell_routes(), |state| state.config.site = true).await;
    let page = client()
        .get(format!("{base}/pricing"))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200);
    let html = page.text().await.unwrap();
    assert!(
        html.starts_with("<!DOCTYPE html><html lang=\"en\">"),
        "{html}"
    );
    let robots = client()
        .get(format!("{base}/robots.txt"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(robots.starts_with("User-agent: *"), "{robots}");
}

/// The analytics tag is runtime configuration, not part of the bundle: the published image is
/// the same for every self-hoster, so an instance with no website id must ship no tracker and
/// call nothing.
#[sqlx::test]
async fn the_tracker_tag_appears_only_where_a_website_id_is_configured(pool: PgPool) {
    const ID: &str = "af413d12-39d7-4060-bc8d-6856f5b74ae1";
    let tag = format!(r#"<script defer src="/stats.js" data-website-id="{ID}"></script>"#);

    let tracked = serve_with_state(pool.clone(), shell_routes(), |state| {
        state.config.umami_website_id = Some(ID.to_string());
        state.config.site = true;
    })
    .await;
    let page = |base: String| async move {
        let res = client()
            .get(format!("{base}/pricing"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        res.text().await.unwrap()
    };
    let html = page(tracked).await;
    assert!(html.contains(&tag), "{html}");
    assert!(html.contains(&format!("{tag}</head>")), "{html}");

    let html =
        page(serve_with_state(pool, shell_routes(), |state| state.config.site = true).await).await;
    assert!(!html.contains("stats.js"), "{html}");
}

/// Width and height from the PNG header: IHDR is always the first chunk.
fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    let be = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

/// The split that keeps an error storm from restarting the container: the Docker
/// HEALTHCHECK (which restarts on failure) watches `/health`, so `/health` must stay
/// green when the database is gone — a restart would not fix it. `/ready` is the one
/// that flips, and only load balancers should act on it.
#[sqlx::test]
async fn liveness_survives_a_dead_database_but_readiness_does_not(pool: PgPool) {
    let base = serve(pool.clone()).await;
    pool.close().await;

    let res = client().get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(
        res.status(),
        200,
        "liveness must not depend on the database"
    );

    let res = client().get(format!("{base}/ready")).send().await.unwrap();
    assert_eq!(res.status(), 503);
}

#[sqlx::test]
async fn every_response_carries_the_hardening_headers(pool: PgPool) {
    let base = serve(pool).await;
    let res = client().get(format!("{base}/health")).send().await.unwrap();
    let h = res.headers();

    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        h.get("content-security-policy").unwrap(),
        "frame-ancestors 'none'"
    );
    assert_eq!(
        h.get("referrer-policy").unwrap(),
        "strict-origin-when-cross-origin"
    );
    // HSTS is gated on secure_cookies; the test config runs plaintext, and
    // advertising HSTS over plain HTTP would be wrong.
    assert!(h.get("strict-transport-security").is_none());
}

#[sqlx::test]
async fn oauth_discovery_metadata_advertises_this_deployment(pool: PgPool) {
    let base = serve(pool).await;
    let res = client()
        .get(format!("{base}/.well-known/oauth-authorization-server"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["issuer"], "http://localhost:8099");
    assert_eq!(body["code_challenge_methods_supported"][0], "S256");
    assert!(
        body["token_endpoint"]
            .as_str()
            .unwrap()
            .ends_with("/oauth/token")
    );
}

#[sqlx::test]
async fn a_client_can_register_itself_and_is_then_known(pool: PgPool) {
    let base = serve(pool).await;
    let res = client()
        .post(format!("{base}/oauth/register"))
        .json(&serde_json::json!({
            "redirect_uris": ["http://localhost:9999/callback"],
            "client_name": "Test Client",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 201);

    let body: serde_json::Value = res.json().await.unwrap();
    let client_id = body["client_id"].as_str().unwrap();
    assert!(client_id.starts_with("mcp_"));
    assert_eq!(body["token_endpoint_auth_method"], "none");

    // An authorize request for a registered client reaches the login
    // redirect; an unknown one is rejected before that point.
    let res = client()
        .get(format!("{base}/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", "http://localhost:9999/callback"),
            ("code_challenge", "abc"),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 303, "a known client is sent to log in");
}

#[sqlx::test]
async fn authorize_rejects_a_redirect_uri_that_was_never_registered(pool: PgPool) {
    let base = serve(pool).await;
    let client_id = client()
        .post(format!("{base}/oauth/register"))
        .json(&serde_json::json!({ "redirect_uris": ["http://localhost:9999/callback"] }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["client_id"]
        .as_str()
        .unwrap()
        .to_string();

    // The allowlist is the security boundary: an attacker-supplied redirect
    // must never be honoured, and must not be bounced to either.
    let res = client()
        .get(format!("{base}/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://attacker.example/steal"),
            ("code_challenge", "abc"),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400, "rejected with an error page");
    assert!(
        res.headers().get("location").is_none(),
        "must not become an open redirector"
    );
    let body = res.text().await.unwrap();
    assert!(body.contains("not registered"));
    assert!(
        !body.contains("attacker.example"),
        "must not reflect the supplied URI"
    );
}

#[sqlx::test]
async fn the_token_endpoint_rejects_an_unknown_code(pool: PgPool) {
    let base = serve(pool).await;
    let res = client()
        .post(format!("{base}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", "never-issued"),
            ("redirect_uri", "http://localhost:9999/callback"),
            ("client_id", "mcp_whatever"),
            ("code_verifier", "v"),
        ])
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"], "invalid_grant");
}

#[sqlx::test]
async fn mcp_without_a_credential_points_the_client_at_discovery(pool: PgPool) {
    let base = serve(pool).await;
    let res = client()
        .post(format!("{base}/mcp"))
        .json(&serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }))
        .send()
        .await
        .unwrap();

    // This 401 is what makes an MCP client start the OAuth flow, so the
    // header must carry the metadata URL.
    assert_eq!(res.status(), 401);
    let challenge = res
        .headers()
        .get("www-authenticate")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(challenge.contains("resource_metadata="));
    assert!(challenge.contains("/.well-known/oauth-protected-resource"));
}

/// One MCP request over the Streamable HTTP transport.
///
/// The transport answers with SSE, and the first frame is an empty SEP-1699
/// priming event — so the JSON-RPC payload is the first `data:` line that
/// actually parses, not simply the first one. `session` carries the
/// `Mcp-Session-Id` that `initialize` hands out; every later call must echo
/// it back or the transport answers `422`.
async fn mcp_call(
    base: &str,
    token: &str,
    session: Option<&str>,
    body: serde_json::Value,
) -> (
    reqwest::StatusCode,
    Option<String>,
    Option<serde_json::Value>,
) {
    let mut req = client()
        .post(format!("{base}/mcp"))
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json, text/event-stream")
        .json(&body);
    if let Some(id) = session {
        req = req.header("mcp-session-id", id);
    }
    let res = req.send().await.unwrap();

    let status = res.status();
    let session_id = res
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let text = res.text().await.unwrap();
    let payload = text
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .find_map(|d| serde_json::from_str::<serde_json::Value>(d.trim()).ok())
        .or_else(|| serde_json::from_str(&text).ok());
    (status, session_id, payload)
}

/// Run the `initialize` + `notifications/initialized` handshake and return
/// the session id for subsequent calls.
async fn mcp_handshake(base: &str, token: &str, version: &str) -> String {
    let (status, session, body) = mcp_call(
        base,
        token,
        None,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }),
    )
    .await;
    assert_eq!(status, 200, "initialize failed: {body:?}");
    let session = session.expect("initialize returned no session id");

    mcp_call(
        base,
        token,
        Some(&session),
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;

    session
}

async fn seed_key(pool: &PgPool) -> String {
    let db = Database::from_pool(pool.clone());
    let user = crate::server::test_support::seed_user(&db, "mcp").await;
    let (token, _) = crate::server::api_key::create(&db, user, "mcp test")
        .await
        .unwrap();
    token
}

/// The server must answer with the version the *client* asked for, not its
/// own newest. rmcp's default `initialize` negotiates this; a hand-written
/// one that returns `get_info()` verbatim would pin every client to
/// `2026-07-28` and break older ones.
#[sqlx::test]
async fn mcp_initialize_negotiates_down_to_the_clients_protocol_version(pool: PgPool) {
    let token = seed_key(&pool).await;
    let base = serve(pool).await;

    let (status, _, body) = mcp_call(
        &base,
        &token,
        None,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }),
    )
    .await;

    assert_eq!(status, 200);
    let body = body.expect("initialize returned no JSON-RPC payload");
    assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(body["result"]["serverInfo"]["name"], "thermite");
}

#[sqlx::test]
async fn mcp_advertises_its_tools_over_the_streamable_transport(pool: PgPool) {
    let token = seed_key(&pool).await;
    let base = serve(pool).await;
    let session = mcp_handshake(&base, &token, "2025-06-18").await;

    let (status, _, body) = mcp_call(
        &base,
        &token,
        Some(&session),
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;

    assert_eq!(status, 200);
    let body = body.expect("tools/list returned no JSON-RPC payload");
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    // The triage loop an agent drives, plus the reads it needs to do it.
    for expected in [
        "list_projects",
        "list_issues",
        "get_issue",
        "get_event",
        "project_stats",
        "list_monitors",
        "pending_triage",
        "claim_triage",
        "ack_triage",
        "post_analysis",
        "set_issue_status",
    ] {
        assert!(
            names.contains(&expected),
            "missing {expected}: got {names:?}"
        );
    }
}

/// rmcp validates `Host` to block DNS rebinding. The allowlist is derived
/// from `BASE_URL`, so a Host the deployment does not answer to is refused
/// before the request reaches a tool.
#[sqlx::test]
async fn mcp_rejects_a_host_header_the_deployment_does_not_serve(pool: PgPool) {
    let token = seed_key(&pool).await;
    let base = serve(pool).await;

    let res = client()
        .post(format!("{base}/mcp"))
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json, text/event-stream")
        .header("host", "attacker.example.com")
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2026-07-28",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 403);
    // Pin the cause: a 403 from anywhere else in the stack would pass a
    // bare status assertion while proving nothing about Host validation.
    assert!(
        res.text()
            .await
            .unwrap()
            .contains("Host header is not allowed")
    );
}

/// The counterpart to the rejection above, and the case that actually needs
/// the allowlist to be derived from `BASE_URL`: rmcp's own default permits
/// loopback only, so a deployed instance answering on its public hostname
/// would 403 every MCP request.
#[sqlx::test]
async fn mcp_accepts_the_host_from_base_url(pool: PgPool) {
    let token = seed_key(&pool).await;
    let base = serve_with_base_url(pool, Some("https://app.example.test")).await;

    let res = client()
        .post(format!("{base}/mcp"))
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json, text/event-stream")
        .header("host", "app.example.test")
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 200);
}

#[sqlx::test]
async fn auth_endpoints_are_rate_limited_through_the_shared_store(pool: PgPool) {
    let base = serve(pool.clone()).await;
    let http = client();

    // Exercises the whole chain: middleware -> AuthRateLimitStore trait ->
    // AppAuthRateLimitStore -> the rate_limits table.
    let mut limited = 0;
    for _ in 0..(auth::AUTH_REQUESTS_PER_MINUTE + 5) {
        let res = http
            .post(format!("{base}/auth/dev-login"))
            // Required: CSRF runs outside the limiter, so an origin-less POST
            // is rejected before it is ever counted.
            .header("Origin", "http://localhost:8099")
            .send()
            .await
            .unwrap();
        if res.status() == 429 {
            limited += 1;
        }
    }
    assert!(limited > 0, "the quota must eventually reject requests");

    let scopes: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT split_part(key, ':', 1) FROM rate_limits")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        scopes,
        vec!["auth"],
        "auth traffic must be counted in the database"
    );
}

#[sqlx::test]
async fn csrf_rejection_happens_before_anything_is_counted(pool: PgPool) {
    let base = serve(pool.clone()).await;
    let http = client();

    // axum applies the last-added layer outermost, so CSRF sits outside the
    // rate limiter. If that order ever flips, cross-origin junk would start
    // burning a real user's quota and writing rows on every request.
    for _ in 0..(auth::AUTH_REQUESTS_PER_MINUTE + 5) {
        let res = http
            .post(format!("{base}/auth/dev-login"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 403, "an origin-less POST is refused by CSRF");
    }

    let counted: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_limits")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        counted, 0,
        "rejected requests must not reach the shared counter"
    );
}

#[sqlx::test]
async fn discovery_traffic_does_not_consume_the_token_budget(pool: PgPool) {
    let base = serve(pool.clone()).await;
    let http = client();

    // Discovery is static JSON on an in-process limiter; if it shared the
    // OAuth bucket, a client re-running discovery could lock out token
    // issuance for everyone on that IP.
    for _ in 0..70 {
        let res = http
            .get(format!("{base}/.well-known/oauth-authorization-server"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            200,
            "discovery must not be throttled at this volume"
        );
    }

    let oauth_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rate_limits WHERE key LIKE 'oauth:%'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        oauth_rows, 0,
        "discovery must not touch the shared OAuth counter"
    );
}

#[sqlx::test]
async fn demo_login_does_not_exist_unless_enabled(pool: PgPool) {
    let base = serve(pool).await;
    let res = client().get(format!("{base}/demo")).send().await.unwrap();
    assert_eq!(res.status(), 404);
}

#[sqlx::test]
async fn demo_login_signs_a_visitor_in_and_returns_them_to_where_they_were(pool: PgPool) {
    let db = pool.clone();
    // Server functions are registered by `main`, not `build`, so a route that needs the
    // session stands in for them.
    let whoami = Router::new().route(
        "/whoami",
        axum::routing::get(|session: auth::UserSession| async move {
            session
                .data()
                .map(|data| data.email)
                .map_err(|_| axum::http::StatusCode::UNAUTHORIZED)
        }),
    );
    let base = serve_with_state(pool, whoami, |state| state.config.demo_autologin = true).await;

    let res = client()
        .get(format!("{base}/demo?next=/issues/42"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 303);
    assert_eq!(res.headers()["location"], "/issues/42");
    let cookie = res.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    // The session is real: a request that needs one is answered as the demo user.
    let res = client()
        .get(format!("{base}/whoami"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "demo@thermite.rs");

    // Signing in twice is the same account, not a second one.
    let again = client().get(format!("{base}/demo")).send().await.unwrap();
    assert_eq!(again.headers()["location"], "/dashboard");
    let users: i64 = sqlx::query_scalar("select count(*) from users where sub = 'demo'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(users, 1);
}

// ── Local sign-in (no identity provider) ────────────────────────────────────

/// Configure a state the way an instance with only `THERMITE_ADMIN_EMAIL` +
/// `THERMITE_ADMIN_PASSWORD` boots: no FerrisKey, no SMTP, the password hashed once.
fn with_admin_credential(state: &mut AppState, email: &str, password: &str) {
    state.config.sign_in = crate::server::config::SignInMode::Local;
    state.config.ferriskey = None;
    state.config.smtp = None;
    state.config.admin_email = Some(email.to_string());
    state.jwks = None;
    state.secrets.admin_password_hash = Some(crypto::hash_secret(password).unwrap());
    state.secrets.dummy_password_hash = crypto::hash_secret("not-the-admin-password").unwrap();
}

/// Both `otp: false` and `password: true` matter: the page has no emailed-code branch to
/// offer, and needs to be told the password field is worth rendering.
#[sqlx::test]
async fn local_sign_in_offers_the_password_step_and_nothing_else(pool: PgPool) {
    let base = serve_with_state(pool, Router::new(), |state| {
        with_admin_credential(state, "admin@example.test", "hunter2");
    })
    .await;

    let res = client()
        .post(format!("{base}/auth/session/start"))
        .header("Origin", "http://localhost:8099")
        .json(&serde_json::json!({ "email": "admin@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["password"], true, "{body}");
    assert_eq!(body["otp"], false, "no SMTP, so no emailed code: {body}");
}

#[sqlx::test]
async fn the_admin_credential_signs_in_and_creates_its_account(pool: PgPool) {
    let db = pool.clone();
    // Server functions are registered by `main`, not `build`, so a route that needs the
    // session stands in for them.
    let whoami = Router::new().route(
        "/whoami",
        axum::routing::get(|session: auth::UserSession| async move {
            session
                .data()
                .map(|data| data.email)
                .map_err(|_| axum::http::StatusCode::UNAUTHORIZED)
        }),
    );
    let base = serve_with_state(pool, whoami, |state| {
        with_admin_credential(state, "admin@example.test", "hunter2");
    })
    .await;

    let res = client()
        .post(format!("{base}/auth/session/password/verify"))
        .header("Origin", "http://localhost:8099")
        .json(&serde_json::json!({
            "email": "admin@example.test",
            "password": "hunter2",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "{:?}", res.text().await);
    let cookie = res.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    // The session is real, and the account was created on this first login — nothing about
    // the credential is written to the database.
    let res = client()
        .get(format!("{base}/whoami"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "admin@example.test");

    let users: i64 = sqlx::query_scalar("select count(*) from users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(users, 1);
}

#[sqlx::test]
async fn a_wrong_password_is_refused(pool: PgPool) {
    let base = serve_with_state(pool.clone(), Router::new(), |state| {
        with_admin_credential(state, "admin@example.test", "hunter2");
    })
    .await;

    for (email, password) in [
        ("admin@example.test", "wrong"),
        ("someone@example.test", "hunter2"),
    ] {
        let res = client()
            .post(format!("{base}/auth/session/password/verify"))
            .header("Origin", "http://localhost:8099")
            .json(&serde_json::json!({ "email": email, "password": password }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401, "{email} / {password} must be refused");
    }

    let users: i64 = sqlx::query_scalar("select count(*) from users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(users, 0, "a refused login must not create an account");
}
