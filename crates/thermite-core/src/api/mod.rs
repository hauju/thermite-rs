//! The read API.
//!
//! Shaped for an agent rather than for a UI: `GET /api/v1/issues/{id}` returns the issue *and* its
//! latest event with the full stack trace, breadcrumbs and contexts, so diagnosing a bug takes one
//! request rather than a walk down a resource tree. The MCP tools call the same functions these
//! handlers do, so the two surfaces cannot drift apart.
//!
//! These routes carry **no authentication**. The application crate owns credentials — API keys,
//! sessions, OAuth — and layers its own check over this router.

pub mod admin;
pub mod alerts;
pub mod analyses;
pub mod fixes;
pub mod issues;
pub mod monitors;
pub mod overview;
pub mod projects;
pub mod releases;
pub mod stats;
pub mod tags;
pub mod triage;

use axum::Router;
use axum::routing::{delete, get, post, put};

use crate::state::ThermiteState;

pub fn routes() -> Router<ThermiteState> {
    Router::new()
        .route("/api/v1/overview", get(overview::list))
        .route("/api/v1/overview/recent", get(overview::recent_list))
        .route("/api/v1/projects", get(projects::list).post(admin::create))
        .route(
            "/api/v1/projects/{slug}",
            axum::routing::patch(admin::patch_project).delete(admin::delete),
        )
        .route(
            "/api/v1/projects/{slug}/keys/{label}",
            delete(admin::delete_key),
        )
        .route(
            "/api/v1/projects/{slug}/alerts",
            put(admin::put_alert_routing),
        )
        .route("/api/v1/fixes", get(fixes::list))
        .route("/api/v1/projects/{slug}/fixes", get(fixes::project))
        .route("/api/v1/projects/{slug}/repo", put(admin::put_repo))
        .route(
            "/api/v1/projects/{slug}/keys",
            post(admin::post_project_key),
        )
        .route("/api/v1/projects/{slug}/issues", get(issues::list))
        .route("/api/v1/projects/{slug}/stats", get(stats::project))
        .route("/api/v1/projects/{slug}/releases", get(releases::list))
        .route("/api/v1/projects/{slug}/tags/{key}", get(tags::values))
        .route("/api/v1/projects/{slug}/monitors", get(monitors::list))
        .route("/api/v1/issues/{id}", get(issues::detail))
        .route("/api/v1/issues/{id}/status", post(issues::set_status))
        .route("/api/v1/issues/{id}/events", get(issues::events))
        .route("/api/v1/events/{id}", get(issues::event))
        // The triage loop an agent drives: claim → diagnose → post analysis → ack.
        .route("/api/v1/triage/pending", get(triage::pending))
        .route("/api/v1/triage/claim", post(triage::claim))
        .route("/api/v1/triage/{id}/ack", post(triage::ack))
        .route("/api/v1/alerts/dead", get(alerts::dead))
        .route("/api/v1/alerts/{id}/retry", post(alerts::retry))
        .route("/api/v1/triage/{id}/release", post(triage::release))
        .route(
            "/api/v1/issues/{id}/analyses",
            get(analyses::list).post(analyses::create),
        )
}

/// Clamps a caller-supplied page size.
pub fn page_size(limit: Option<i64>) -> i64 {
    limit.unwrap_or(50).clamp(1, 100)
}
