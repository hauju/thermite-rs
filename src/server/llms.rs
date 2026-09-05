//! `GET /llms.txt` — a plain-text orientation page for AI agents.
//!
//! An agent that just received a DSN from `create_project`, or was pointed at this instance with
//! nothing but a URL, can read this to learn what Thermite is, how to connect, and what the triage
//! loop looks like — without a human relaying the docs. Unauthenticated on purpose: it describes
//! the interface, never data, and discovery is its whole point.

use axum::Router;
use axum::routing::get;

const LLMS_TXT: &str = "\
# Thermite

Self-hosted error tracker speaking Sentry's wire protocol. Unmodified Sentry SDKs report into it,
events are grouped into issues, and the same data is served to humans (dashboard) and agents
(MCP + REST).

## MCP (recommended for agents)

POST /mcp — Model Context Protocol over Streamable HTTP. Authenticate with
`Authorization: Bearer <api key>` (create one under Settings -> API keys) or via the OAuth flow
your MCP client starts automatically.

Tools: list_projects, create_project, list_issues, get_issue, get_event, project_stats,
release_health, list_monitors, pending_triage, claim_triage, ack_triage, post_analysis,
fix_record, set_issue_status.

The triage loop: claim_triage (leases work so parallel agents never collide) -> get_issue (one
call: exception chain, stack frames with source context, breadcrumbs, prior analyses) -> diagnose
-> post_analysis (persist your findings on the issue) -> ack_triage. Each item carries the release
the error came from; when it is a git SHA, check out that revision before reading code. A
regression also carries regressed_from_release, the last known good — `git diff
<regressed_from_release>..<release>` is the change set that reintroduced the bug. An item also
carries repo_url when the project declares one: open the pull request yourself and hand the link
back as fix_url on post_analysis (same host as repo_url; Thermite stores the link and never calls
it). After fixing,
set_issue_status with status=resolved and in_next_release=true so lingering traffic from the
broken deploy does not reopen the issue.

## REST

The same operations under /api/v1 (e.g. GET /api/v1/projects, /api/v1/projects/{slug}/issues,
/api/v1/triage/pending), same Authorization header.

## Sending errors

Point any Sentry SDK at the DSN returned by create_project — no SDK changes needed. Send
`release` (ideally the git SHA) to enable regression detection, session tracking for release
health, and cron check-ins to monitor scheduled jobs.
";

pub fn llms_router() -> Router {
    Router::new().route("/llms.txt", get(|| async { LLMS_TXT }))
}
