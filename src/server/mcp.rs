//! MCP (Model Context Protocol) endpoint backed by `rmcp` over Streamable HTTP.
//!
//! `POST /mcp` is served by rmcp's [`StreamableHttpService`] (JSON-RPC in,
//! SSE/JSON out). Tools live on [`McpTools`] via the `#[tool_router]` / `#[tool]`
//! macros and pull the authenticated user from the request headers using the
//! shared [`api_auth::authenticate`] logic.
//!
//! Auth is two-layered: [`mcp_auth_challenge`] does a presence-only check at the
//! edge (no credential → `401` + `WWW-Authenticate` pointing at the
//! protected-resource metadata, which triggers the OAuth discovery flow); each
//! tool then performs the real API-key / JWT validation.

use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::handler::server::tool::{Extension, ToolRouter};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use serde::Serialize;
use thermite_core::api::{
    admin, analyses, fixes, issues, monitors, projects, releases, stats, triage,
};
use thermite_core::{AppError, AppResult};

use crate::server::api_auth::{self, ApiAuth};
use crate::server::security::{IpRateLimiter, ip_rate_limit};
use crate::server::state::AppState;

/// The MCP server: holds app state and the generated tool router.
#[derive(Clone)]
pub struct McpTools {
    state: AppState,
    tool_router: ToolRouter<Self>,
}

impl McpTools {
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    /// Resolve the authenticated user from the request headers, or an MCP error.
    ///
    /// A bad credential is a *protocol* error, not a tool result: the client needs to see it as an
    /// auth failure so it starts the OAuth flow rather than treating it as a tool that ran.
    async fn authenticate(&self, parts: &Parts) -> Result<ApiAuth, McpError> {
        api_auth::authenticate(&self.state, &parts.headers)
            .await
            .map_err(|_| {
                McpError::invalid_request(
                    "Missing or invalid credential. Provide 'Authorization: Bearer <token>' or 'X-API-Key: <token>'.",
                    None,
                )
            })
    }

    fn db(&self) -> &sqlx::PgPool {
        &self.state.thermite.db
    }
}

/// Renders a result for the caller.
///
/// Structured content is what an agent consumes; the text copy is there for clients that only
/// render text.
///
/// A `NotFound` or `BadRequest` becomes a tool-level *error result* rather than a protocol error,
/// per the MCP guidance: the request routed fine and the tool ran, so the caller should see the
/// message and be able to correct itself. Only genuine infrastructure failures are protocol errors.
fn present<T: Serialize>(result: AppResult<T>) -> Result<CallToolResult, McpError> {
    match result {
        Ok(value) => {
            let value = serde_json::to_value(value).map_err(internal)?;
            let text = serde_json::to_string_pretty(&value).map_err(internal)?;

            // `CallToolResult` is #[non_exhaustive], so it is built and then adjusted.
            let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
            result.structured_content = Some(value);
            Ok(result)
        }
        Err(error @ (AppError::NotFound | AppError::BadRequest(_))) => Ok(CallToolResult::error(
            vec![ContentBlock::text(error.to_string())],
        )),
        Err(AppError::Unauthorized(message)) => Err(McpError::invalid_request(message, None)),
        Err(other) => Err(internal(other)),
    }
}

fn internal<E: std::fmt::Display>(error: E) -> McpError {
    McpError::internal_error(error.to_string(), None)
}

// ---------------------------------------------------------------------------
// Tool inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListIssues {
    /// Project slug, as shown by `list_projects`.
    pub project: String,
    /// `unresolved`, `resolved` or `ignored`. Omit for all.
    #[serde(default)]
    pub status: Option<String>,
    /// Case-insensitive substring match on the issue title.
    #[serde(default)]
    pub query: Option<String>,
    /// Only issues with events in this environment, e.g. `production`.
    #[serde(default)]
    pub environment: Option<String>,
    /// `key:value` — only issues with events carrying this tag, e.g. `server_name:web-3`
    /// or `component:worker` (events ingested through a labeled DSN key).
    #[serde(default)]
    pub tag: Option<String>,
    /// `events` for the highest-volume issues first, `last_seen` (default) for the most recent.
    #[serde(default)]
    pub sort: Option<String>,
    /// Max issues to return. Capped at 100.
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NewProject {
    /// URL-safe identifier used in API paths: letters, digits, '-' and '_'.
    pub slug: String,
    /// Display name. Defaults to the slug.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IssueRef {
    pub issue_id: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EventRef {
    /// The event id the SDK assigned, as it appears in application logs.
    pub event_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectRef {
    /// Project slug, as shown by `list_projects`.
    pub project: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StatsInput {
    pub project: String,
    /// `24h` (default), `7d` or `30d`.
    #[serde(default)]
    pub window: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TriageInput {
    /// Restrict to one project.
    #[serde(default)]
    pub project: Option<String>,
    /// `new_issue` or `regression`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Only issues at this level, e.g. `error`.
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    /// How long the claim holds before the work is offered to someone else. Default 900.
    #[serde(default)]
    pub lease_seconds: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NotificationRef {
    pub notification_id: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PostAnalysis {
    pub issue_id: i64,
    /// One-line conclusion.
    pub summary: String,
    /// The full reasoning.
    #[serde(default)]
    pub details: Option<String>,
    /// The concrete change to make.
    #[serde(default)]
    pub suggested_fix: Option<String>,
    /// `high`, `medium` or `low`. Say low rather than guessing confidently.
    #[serde(default)]
    pub confidence: Option<String>,
    /// The revision this was reasoned against, so a reader can tell if it is still current.
    #[serde(default)]
    pub release: Option<String>,
    /// The pull request you opened for this issue, if you got that far. Must be on the same host
    /// as the project's `repo_url`, which the triage item and `get_issue` both carry. Opening it
    /// is your job — thermite holds no git credential and never calls the host.
    #[serde(default)]
    pub fix_url: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FixRecordInput {
    /// Restrict to one project. Omit for every project.
    #[serde(default)]
    pub project: Option<String>,
    /// Max fixes to return, newest first. Capped at 200.
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetStatus {
    pub issue_id: i64,
    /// `resolved`, `ignored` or `unresolved`.
    pub status: String,
    /// With `resolved`: stay resolved for events from releases already seen (the broken deploy
    /// still out there), and reopen as a regression only when a newer release reports the error.
    /// Use after fixing a bug whose fix has not shipped yet. Requires the SDK to send a release.
    #[serde(default)]
    pub in_next_release: bool,
}

#[tool_router]
impl McpTools {
    #[tool(
        description = "List projects reporting errors to Thermite, with their unresolved issue count and events in the last 24 hours."
    )]
    pub async fn list_projects(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;
        present(projects::all(self.db(), &self.state.thermite.config).await)
    }

    #[tool(
        description = "Create a project and return the DSN to configure an SDK with. The DSN's public key is not a secret — it only grants the ability to send events to this project."
    )]
    pub async fn create_project(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<NewProject>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;

        present(
            admin::create_project(
                self.db(),
                &self.state.thermite.config,
                &admin::NewProject {
                    slug: input.slug,
                    name: input.name,
                },
            )
            .await,
        )
    }

    #[tool(
        description = "List issues in a project, each with a 24-hour sparkline. Sorted by most recent activity by default; pass sort=events for the highest-volume issues first, which is usually what you want when deciding what to fix. Filter with environment=production or tag=key:value. Returns titles, culprits and counts — use get_issue for the stack trace."
    )]
    pub async fn list_issues(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<ListIssues>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;

        present(
            issues::for_project(
                self.db(),
                &input.project,
                input.status.as_deref(),
                input.query.as_deref(),
                input.environment.as_deref(),
                input.tag.as_deref(),
                input.sort.as_deref(),
                input.limit,
                None,
            )
            .await,
        )
    }

    #[tool(
        description = "Everything needed to diagnose one issue in a single call: the exception chain, every stack frame with its source context, breadcrumbs, runtime contexts, and any analysis already posted by another agent. For a regression, regressed_from_release is the last known good — `git diff <regressed_from_release>..<latest release>` is the change set that reintroduced the bug."
    )]
    pub async fn get_issue(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<IssueRef>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;
        present(issues::detail_of(self.db(), input.issue_id).await)
    }

    #[tool(
        description = "Fetch one event by the id the SDK assigned it, which is the id that appears in application logs."
    )]
    pub async fn get_event(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<EventRef>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;
        present(issues::event_by_id(self.db(), &input.event_id).await)
    }

    #[tool(
        description = "Error rate over time plus the current state of a project: events per bucket by level, unresolved issues, new issues and regressions."
    )]
    pub async fn project_stats(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<StatsInput>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;
        present(stats::for_project(self.db(), &input.project, input.window.as_deref()).await)
    }

    #[tool(
        description = "Release health: how many sessions each recent release served and how many crashed, newest release first. Use this to tell a release that is broken from one that is merely busy — an error count alone cannot, because it rises with traffic. crash_free_rate is null below 50 sessions, where the rate would be noise. Requires an SDK with release health enabled; empty otherwise."
    )]
    pub async fn release_health(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<StatsInput>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;
        present(
            releases::for_project(self.db(), &input.project, input.window.as_deref(), None).await,
        )
    }

    #[tool(
        description = "Cron monitors of a project: each scheduled job's schedule, last check-in and current status (ok, error, missed or timeout). A job that misses a run or overruns becomes an ordinary error event automatically; this answers \"are my scheduled jobs healthy\" before that happens. Monitors are created by the jobs themselves via Sentry check-ins, so an empty list means no job is instrumented."
    )]
    pub async fn list_monitors(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<ProjectRef>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;
        present(monitors::of_project(self.db(), &input.project).await)
    }

    #[tool(
        description = "See which issues are waiting for triage without taking them. Use claim_triage to actually pick work up."
    )]
    pub async fn pending_triage(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<TriageInput>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;

        present(
            triage::pending_items(
                self.db(),
                input.project.as_deref(),
                input.kind.as_deref(),
                input.level.as_deref(),
                input.limit,
            )
            .await,
        )
    }

    #[tool(
        description = "Take issues off the triage queue. Claimed items are hidden from other agents until the lease expires, so the same issue is not diagnosed twice. Each item carries the release the error came from — check out that revision before reading the code. A regression also carries regressed_from_release, the release the fix was verified against: `git diff <regressed_from_release>..<release>` is the change set that reintroduced the bug. Call ack_triage when done."
    )]
    pub async fn claim_triage(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<TriageInput>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;

        present(
            triage::claim_items(
                self.db(),
                triage::ClaimRequest {
                    project: input.project,
                    kind: input.kind,
                    level: input.level,
                    limit: input.limit,
                    lease_seconds: input.lease_seconds,
                    claimed_by: Some("mcp".to_string()),
                },
            )
            .await,
        )
    }

    #[tool(
        description = "Mark a claimed triage item done so it is not handed out again. Idempotent."
    )]
    pub async fn ack_triage(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<NotificationRef>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;
        present(triage::ack_item(self.db(), input.notification_id).await)
    }

    #[tool(
        description = "Write a diagnosis back onto an issue so it is waiting for whoever opens it next. This is how triage work is preserved — without it the analysis is lost when the run ends. If you also opened a pull request for the fix, pass its URL as fix_url and it is shown alongside the diagnosis."
    )]
    pub async fn post_analysis(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<PostAnalysis>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;

        present(
            analyses::record(
                self.db(),
                input.issue_id,
                analyses::NewAnalysis {
                    source: "mcp".to_string(),
                    summary: input.summary,
                    details: input.details,
                    suggested_fix: input.suggested_fix,
                    confidence: input.confidence,
                    release: input.release,
                    fix_url: input.fix_url,
                    metadata: None,
                },
            )
            .await,
        )
    }

    #[tool(
        description = "What became of the fixes agents proposed: every analysis carrying a fix_url, graded against production, plus a per-agent tally. A fix is `held` when releases shipped after it was proposed and the issue was seen in none of them, `regressed` when the issue came back in a release that first appeared after it, and `pending` while nothing has shipped since. Read your own record here before trusting your own confidence."
    )]
    pub async fn fix_record(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<FixRecordInput>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;
        present(fixes::record(self.db(), input.project.as_deref(), input.limit).await)
    }

    #[tool(
        description = "Resolve, ignore or reopen an issue. Resolving is not final: a resolved issue that happens again is reopened and queued as a regression — pass in_next_release=true after fixing a bug whose fix has not shipped, so lingering traffic from already-seen releases does not reopen it. Ignoring is what suppresses an issue for good."
    )]
    pub async fn set_issue_status(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<SetStatus>,
    ) -> Result<CallToolResult, McpError> {
        self.authenticate(&parts).await?;
        present(
            issues::update_status(
                self.db(),
                input.issue_id,
                &input.status,
                input.in_next_release,
            )
            .await,
        )
    }
}

/// `#[tool_handler]` generates `call_tool`, `list_tools`, and `get_tool`. The
/// explicit `router = self.tool_router` matters: the attribute otherwise defaults
/// to `Self::tool_router()`, which rebuilds the router — re-deriving every tool's
/// JSON schema — on every `tools/list` and `tools/call`. Pointing it at the field
/// reuses the one built in [`McpTools::new`].
///
/// The remaining trait methods — notably `initialize` — keep their defaults, which
/// negotiate the protocol version against what the client asked for. That matters
/// under `2026-07-28`: per SEP-2567 the version decides whether a request is served
/// statelessly, so pinning it would break older clients.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("thermite", env!("CARGO_PKG_VERSION"))
                    .with_title("Thermite MCP"),
            )
            .with_instructions(
                "Thermite collects application errors and groups them into issues.\n\n\
                 To triage: claim_triage takes work off the queue, get_issue gives you the full \
                 stack trace and source context in one call, post_analysis writes your findings \
                 back onto the issue, and ack_triage marks it done.\n\n\
                 Each triage item carries the `release` the error came from. When that is a git \
                 SHA, check out that revision before reading the code — diagnosing against the \
                 wrong revision produces confident but wrong answers.\n\n\
                 An item also carries `repo_url` when the project has one configured. With it you \
                 can go past a written diagnosis: open a pull request against that repository and \
                 pass the link back as `fix_url` on post_analysis. Do not resolve the issue \
                 yourself — an unmerged fix that is already resolved makes the next crash look \
                 like a regression.",
            )
    }
}

/// Presence-only OAuth challenge for `/mcp` (RFC 9728).
///
/// Any `Bearer`/`X-API-Key` credential passes through here; the actual validation
/// happens inside each tool. With no credential we return `401` plus a
/// `WWW-Authenticate` header pointing at the protected-resource metadata, which
/// is what makes an MCP client (e.g. Claude) start its OAuth discovery flow.
pub async fn mcp_auth_challenge(
    axum::Extension(state): axum::Extension<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let has_bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .is_some_and(|t| !t.trim().is_empty());
    let has_x_api_key = headers
        .get("x-api-key")
        .is_some_and(|v| v.to_str().is_ok_and(|s| !s.trim().is_empty()));

    if has_bearer || has_x_api_key {
        return next.run(request).await;
    }

    let metadata_url = format!(
        "{}/.well-known/oauth-protected-resource",
        state.config.base_url.trim_end_matches('/')
    );
    let challenge = format!("Bearer resource_metadata=\"{metadata_url}\"");
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, challenge)],
    )
        .into_response()
}

/// Hosts accepted in the `Host` header of `/mcp` requests.
///
/// rmcp validates `Host` to block DNS rebinding, and its default allowlist is
/// loopback-only — which would reject every request to a deployed instance. The
/// deployment's own hostname comes from `BASE_URL`; loopback stays on the list so
/// local development and container health checks keep working.
///
/// Entries are bare hostnames, without a port, because rmcp treats a portless
/// entry as "any port": the public URL and the port the process actually listens
/// on differ behind a reverse proxy.
fn allowed_hosts(base_url: &str) -> Vec<String> {
    let mut hosts = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if let Some(host) = url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        && !hosts.contains(&host)
    {
        hosts.push(host);
    }
    hosts
}

/// Build the `/mcp` router: rmcp Streamable-HTTP service + auth challenge + rate
/// limit.
///
/// The service factory builds a fresh [`McpTools`] per session — and, for clients
/// negotiating `2026-07-28`, per request, since SEP-2567 drops sessions from that
/// version. Keep the factory cheap for that reason: it clones [`AppState`]
/// (pool handles behind `Arc`) and builds the tool router, nothing more.
pub fn mcp_router(state: AppState, trust_proxy_headers: bool) -> Router {
    let limiter =
        IpRateLimiter::shared_per_minute(state.db.pool.clone(), "mcp", 120, trust_proxy_headers);
    let session_manager = Arc::new(LocalSessionManager::default());
    let server_config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_hosts(&state.config.base_url));

    let service = StreamableHttpService::new(
        move || Ok(McpTools::new(state.clone())),
        session_manager,
        server_config,
    );

    Router::new()
        .route_service("/mcp", service)
        .layer(axum::middleware::from_fn(mcp_auth_challenge))
        .layer(axum::middleware::from_fn(ip_rate_limit))
        .layer(axum::Extension(limiter))
}

#[cfg(test)]
mod tests {
    use super::allowed_hosts;

    #[test]
    fn deployment_host_is_derived_from_base_url() {
        let hosts = allowed_hosts("https://app.example.com");
        assert!(hosts.contains(&"app.example.com".to_string()));
    }

    #[test]
    fn port_is_stripped_so_any_port_matches() {
        // rmcp treats a portless entry as "any port"; keeping the port would
        // reject requests whose Host carries the proxy's port instead.
        let hosts = allowed_hosts("https://app.example.com:8443");
        assert!(hosts.contains(&"app.example.com".to_string()));
        assert!(!hosts.iter().any(|h| h.contains(':') && h != "::1"));
    }

    #[test]
    fn loopback_is_always_allowed() {
        let hosts = allowed_hosts("https://app.example.com");
        for expected in ["localhost", "127.0.0.1", "::1"] {
            assert!(hosts.contains(&expected.to_string()), "missing {expected}");
        }
    }

    #[test]
    fn local_base_url_does_not_duplicate_loopback() {
        let hosts = allowed_hosts("http://localhost:8080");
        assert_eq!(hosts.iter().filter(|h| *h == "localhost").count(), 1);
    }

    #[test]
    fn unparseable_base_url_still_leaves_loopback() {
        let hosts = allowed_hosts("not a url");
        assert_eq!(hosts, vec!["localhost", "127.0.0.1", "::1"]);
    }
}
