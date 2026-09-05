//! Authorization endpoint, consent screen, and consent decision.
//!
//! Flow: `GET /oauth/authorize` validates the request and stashes it in the
//! session. If the user isn't logged in we bounce through `/login` and re-enter
//! at `GET /oauth/authorize/resume`. The consent screen `POST`s to
//! `/oauth/authorize/decision`, which (after CSRF checks) mints a short-lived
//! authorization code and redirects back to the client.

use axum::Form;
use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use super::store::{self, OAuthClientEntity};
use crate::server::state::AppState;

const PENDING_KEY: &str = "oauth_pending";
const CSRF_KEY: &str = "oauth_csrf";
const CODE_TTL_SECONDS: f64 = 60.0;

/// The validated authorize request, stashed in the session between the initial
/// request and the consent decision (so the consent form never carries
/// security-critical values).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingAuthorize {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    state: Option<String>,
    scope: String,
}

#[derive(Debug, Deserialize)]
pub struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    state: Option<String>,
    scope: Option<String>,
}

/// `GET /oauth/authorize`
pub async fn authorize(
    state: AppState,
    session: Session,
    Query(q): Query<AuthorizeQuery>,
) -> Response {
    let (Some(client_id), Some(redirect_uri)) = (q.client_id.clone(), q.redirect_uri.clone())
    else {
        return error_page("Missing client_id or redirect_uri.");
    };

    // Validate the client + redirect_uri BEFORE trusting them as a redirect
    // target — otherwise we'd be an open redirector for errors.
    let client = match store::find_client(&state.db, &client_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return error_page("Unknown client_id."),
        Err(_) => return error_page("Internal error."),
    };
    if !client.redirect_uris.iter().any(|u| u == &redirect_uri) {
        return error_page("redirect_uri is not registered for this client.");
    }

    // From here, parameter errors can be safely redirected to the client.
    if q.response_type.as_deref() != Some("code") {
        return redirect_error(
            &redirect_uri,
            "unsupported_response_type",
            q.state.as_deref(),
        );
    }
    if q.code_challenge_method.as_deref() != Some("S256") {
        return redirect_error(&redirect_uri, "invalid_request", q.state.as_deref());
    }
    let Some(code_challenge) = q.code_challenge.filter(|c| !c.is_empty()) else {
        return redirect_error(&redirect_uri, "invalid_request", q.state.as_deref());
    };

    let pending = PendingAuthorize {
        client_id,
        redirect_uri,
        code_challenge,
        state: q.state,
        scope: q.scope.unwrap_or_else(|| "mcp".to_string()),
    };
    if session.insert(PENDING_KEY, &pending).await.is_err() {
        return error_page("Could not start authorization.");
    }

    render_consent_or_login(&state, &session, &client, &pending.redirect_uri).await
}

/// `GET /oauth/authorize/resume` — re-entry point after the login redirect.
pub async fn resume(state: AppState, session: Session) -> Response {
    let pending: Option<PendingAuthorize> = session.get(PENDING_KEY).await.ok().flatten();
    let Some(pending) = pending else {
        return error_page(
            "No pending authorization. Please restart the connection from your client.",
        );
    };
    let client = match store::find_client(&state.db, &pending.client_id).await {
        Ok(Some(c)) => c,
        _ => return error_page("Unknown client_id."),
    };
    render_consent_or_login(&state, &session, &client, &pending.redirect_uri).await
}

#[derive(Debug, Deserialize)]
pub struct DecisionForm {
    csrf_token: String,
    decision: String,
}

/// `POST /oauth/authorize/decision`
pub async fn decision(
    state: AppState,
    session: Session,
    headers: HeaderMap,
    Form(form): Form<DecisionForm>,
) -> Response {
    // CSRF: same-origin POST + one-shot session token.
    if !origin_ok(&headers, &state.config.base_url) {
        return (StatusCode::FORBIDDEN, "Bad origin").into_response();
    }
    let stored_csrf: Option<String> = session.get(CSRF_KEY).await.ok().flatten();
    let _ = session.remove::<String>(CSRF_KEY).await;
    if stored_csrf.as_deref() != Some(form.csrf_token.as_str()) {
        return (StatusCode::FORBIDDEN, "Invalid CSRF token").into_response();
    }

    let pending: Option<PendingAuthorize> = session.get(PENDING_KEY).await.ok().flatten();
    let _ = session.remove::<PendingAuthorize>(PENDING_KEY).await;
    let Some(pending) = pending else {
        return error_page("No pending authorization.");
    };

    let logged_in = current_user(&session).await;
    let Some(user) = logged_in else {
        return Redirect::to("/login?redirect_url=/oauth/authorize/resume").into_response();
    };

    if form.decision != "approve" {
        return redirect_error(
            &pending.redirect_uri,
            "access_denied",
            pending.state.as_deref(),
        );
    }

    let user_id = match uuid::Uuid::parse_str(&user.id) {
        Ok(id) => id,
        Err(_) => return error_page("Invalid session."),
    };
    let code = match crypto::generate_invitation_token() {
        Ok(c) => c,
        Err(_) => return error_page("Internal error."),
    };

    // Remember the approval, so the *next* prompt from this client is not flagged as new — and
    // one from a look-alike still is. Recorded before the code is minted: a failure here must
    // not lose the grant, but a grant that succeeded must never leave the client unknown.
    if let Err(e) = store::record_approval(&state.db, user_id, &pending.client_id).await {
        tracing::warn!("could not record OAuth client approval: {e}");
    }
    // `expires_at` is set from the database clock (see store::insert_code).
    if store::insert_code(
        &state.db,
        store::NewAuthorizationCode {
            id: uuid::Uuid::new_v4(),
            code: &code,
            client_id: &pending.client_id,
            redirect_uri: &pending.redirect_uri,
            code_challenge: &pending.code_challenge,
            user_id,
            scope: &pending.scope,
            ttl_seconds: CODE_TTL_SECONDS,
        },
    )
    .await
    .is_err()
    {
        return error_page("Could not issue authorization code.");
    }

    redirect_success(&pending.redirect_uri, &code, pending.state.as_deref())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn current_user(session: &Session) -> Option<auth::LoggedInData> {
    session
        .get(auth::session::LOGGED_IN_USER_SESSION_KEY)
        .await
        .ok()
        .flatten()
}

async fn render_consent_or_login(
    state: &AppState,
    session: &Session,
    client: &OAuthClientEntity,
    redirect_uri: &str,
) -> Response {
    match current_user(session).await {
        Some(user) => {
            let csrf = crypto::generate_csrf_token().unwrap_or_default();
            if session.insert(CSRF_KEY, &csrf).await.is_err() {
                return error_page("Could not render consent.");
            }

            // Unknown-before clients are flagged. On any error here we treat the client as new:
            // a spurious warning costs a moment's attention, a missing one costs the account.
            let seen_before = match uuid::Uuid::parse_str(&user.id) {
                Ok(user_id) => store::has_approved_before(&state.db, user_id, &client.client_id)
                    .await
                    .unwrap_or(false),
                Err(_) => false,
            };

            consent_page(client, &user.email, &csrf, redirect_uri, seen_before)
        }
        None => Redirect::to("/login?redirect_url=/oauth/authorize/resume").into_response(),
    }
}

/// The host an approval would send the token to — the one part of the request an attacker cannot
/// fake without controlling that host, and therefore the thing worth showing the operator.
fn redirect_host(redirect_uri: &str) -> String {
    url::Url::parse(redirect_uri)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| redirect_uri.to_string())
}

fn origin_ok(headers: &HeaderMap, base_url: &str) -> bool {
    let Some(value) = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    match (url::Url::parse(value), url::Url::parse(base_url)) {
        (Ok(o), Ok(b)) => {
            o.scheme() == b.scheme()
                && o.host() == b.host()
                && o.port_or_known_default() == b.port_or_known_default()
        }
        _ => false,
    }
}

fn redirect_with_params(redirect_uri: &str, params: &[(&str, &str)]) -> Response {
    match url::Url::parse(redirect_uri) {
        Ok(mut url) => {
            url.query_pairs_mut().extend_pairs(params.iter().copied());
            Redirect::to(url.as_str()).into_response()
        }
        Err(_) => error_page("Invalid redirect_uri."),
    }
}

fn redirect_success(redirect_uri: &str, code: &str, state: Option<&str>) -> Response {
    let mut params = vec![("code", code)];
    if let Some(s) = state {
        params.push(("state", s));
    }
    redirect_with_params(redirect_uri, &params)
}

fn redirect_error(redirect_uri: &str, error: &str, state: Option<&str>) -> Response {
    let mut params = vec![("error", error)];
    if let Some(s) = state {
        params.push(("state", s));
    }
    redirect_with_params(redirect_uri, &params)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn error_page(message: &str) -> Response {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Authorization error</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem\">\
         <h1>Authorization error</h1><p>{}</p></body></html>",
        html_escape(message)
    );
    (StatusCode::BAD_REQUEST, Html(body)).into_response()
}

fn consent_page(
    client: &OAuthClientEntity,
    email: &str,
    csrf: &str,
    redirect_uri: &str,
    seen_before: bool,
) -> Response {
    let app_name = client
        .client_name
        .clone()
        .unwrap_or_else(|| client.client_id.clone());

    // Anyone can register a client under any name, so the name is decoration. These two lines
    // are the ones with security value: where the token would be sent, and whether this
    // approval has ever been granted before.
    let first_use = if seen_before {
        String::new()
    } else {
        "<div class=\"warn\"><strong>You have not approved this application before.</strong> \
         If you did not just start connecting it, close this page — approving grants full \
         access to every project and error payload.</div>"
            .to_string()
    };

    let body = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Authorize {app}</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ font-family: system-ui, -apple-system, sans-serif; max-width: 26rem; margin: 4rem auto; padding: 0 1rem; line-height: 1.5; }}
  .card {{ border: 1px solid #8883; border-radius: 14px; padding: 1.75rem; }}
  h1 {{ font-size: 1.25rem; margin: 0 0 .25rem; }}
  .muted {{ opacity: .7; font-size: .9rem; }}
  .scope {{ background: #8881; border-radius: 8px; padding: .75rem 1rem; margin: 1.25rem 0; font-size: .9rem; }}
  .warn {{ background: #f59e0b22; border: 1px solid #f59e0b66; border-radius: 8px; padding: .75rem 1rem; margin: 1.25rem 0; font-size: .9rem; }}
  .target {{ font-size: .85rem; margin-top: .75rem; }}
  .target code {{ background: #8882; border-radius: 5px; padding: .1rem .4rem; word-break: break-all; }}
  .row {{ display: flex; gap: .75rem; margin-top: 1.5rem; }}
  button {{ flex: 1; padding: .7rem 1rem; border-radius: 9px; border: 0; font-size: 1rem; cursor: pointer; }}
  .approve {{ background: #4f46e5; color: #fff; }}
  .deny {{ background: #8882; color: inherit; }}
</style>
</head>
<body>
  <div class="card">
    <h1>Authorize {app}</h1>
    <p class="muted">Signed in as {email}</p>
    <div class="scope">
      <strong>{app}</strong> is requesting full access to your account via the Model Context
      Protocol: every project, issue and error payload.
      <div class="target">Access will be sent to <code>{host}</code></div>
    </div>
    {first_use}
    <form method="post" action="/oauth/authorize/decision">
      <input type="hidden" name="csrf_token" value="{csrf}">
      <div class="row">
        <button class="deny" type="submit" name="decision" value="deny">Deny</button>
        <button class="approve" type="submit" name="decision" value="approve">Approve</button>
      </div>
    </form>
  </div>
</body>
</html>"#,
        app = html_escape(&app_name),
        email = html_escape(email),
        csrf = html_escape(csrf),
        host = html_escape(&redirect_host(redirect_uri)),
        first_use = first_use,
    );

    // Clickjacking defense in addition to the global headers.
    (
        [
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'"),
        ],
        Html(body),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client(name: &str) -> OAuthClientEntity {
        OAuthClientEntity {
            id: uuid::Uuid::new_v4(),
            client_id: "mcp_abc123".to_string(),
            redirect_uris: vec!["https://claude.ai/api/mcp/auth_callback".to_string()],
            client_name: Some(name.to_string()),
            created_at: chrono::Utc::now(),
        }
    }

    async fn render(seen_before: bool) -> String {
        let response = consent_page(
            &test_client("Claude"),
            "ops@company.example",
            "csrf-token",
            "https://claude.ai/api/mcp/auth_callback",
            seen_before,
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn the_redirect_host_is_extracted_for_display() {
        assert_eq!(
            redirect_host("https://claude.ai/api/mcp/auth_callback"),
            "claude.ai"
        );
        assert_eq!(redirect_host("http://localhost:9999/cb"), "localhost");
        // An unparseable URI shows verbatim rather than vanishing: the operator must always see
        // *something* to judge, and this string is escaped downstream.
        assert_eq!(redirect_host("not a url"), "not a url");
    }

    #[tokio::test]
    async fn consent_shows_where_the_token_would_go() {
        // The client name is attacker-chosen and therefore decoration; the redirect host is the
        // part an attacker cannot fake without controlling that host.
        let page = render(true).await;
        assert!(page.contains("claude.ai"), "{page}");
        assert!(
            page.contains("full access"),
            "the grant's scope must be plain"
        );
    }

    #[tokio::test]
    async fn a_never_before_approved_client_is_flagged() {
        let page = render(false).await;
        assert!(
            page.contains("not approved this application before"),
            "a look-alike client is only visible through this warning: {page}"
        );

        // And a client the user already uses does not cry wolf every time.
        let known = render(true).await;
        assert!(!known.contains("not approved this application before"));
    }

    #[tokio::test]
    async fn an_attacker_chosen_client_name_cannot_inject_markup() {
        let response = consent_page(
            &test_client("<script>alert(1)</script>"),
            "ops@company.example",
            "csrf",
            "https://evil.example/cb",
            false,
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let page = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(!page.contains("<script>"), "client_name must be escaped");
        assert!(page.contains("&lt;script&gt;"));
        assert!(page.contains("evil.example"), "the real target must show");
    }
}
