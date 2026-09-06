//! Server functions behind the error-tracking dashboard.
//!
//! These call `thermite-core` directly rather than round-tripping through `/api/v1` — the REST API
//! and these share the same functions, so there is no second implementation to drift.

use dioxus::prelude::*;

use crate::models::errors::{
    DeadLetterRow, EventDetail, EventRef, FeedRow, IssueDetail, IssueQuery, IssueRow, MonitorRow,
    ProjectOverviewRow, ProjectStats, ProjectSummary, ReleaseHealthRow,
};

#[cfg(feature = "server")]
use crate::models::AppError;

/// Maps a `thermite-core` error onto the application's, so a bad slug reads as a validation
/// failure rather than a server fault.
#[cfg(feature = "server")]
fn app_error(error: thermite_core::AppError) -> ServerFnError {
    use thermite_core::AppError as Core;

    ServerFnError::from(match error {
        Core::NotFound => AppError::NotFound,
        Core::BadRequest(message) => AppError::Validation(message),
        Core::Unauthorized(_) => AppError::Unauthorized,
        other => AppError::Internal(other.to_string()),
    })
}

/// Resolves the request's `ThermiteState`, rejecting callers without a session.
#[cfg(feature = "server")]
fn thermite(session: auth::UserSession) -> Result<thermite_core::ThermiteState, ServerFnError> {
    session
        .data()
        .map_err(|_| ServerFnError::from(AppError::Unauthorized))?;
    Ok(crate::server::state::AppState::global().thermite.clone())
}

/// Who is asking. A signed-in user reads everything; a visitor reads only the demo project,
/// when one is configured (`THERMITE_DEMO_PROJECT`). Writes always need a session.
#[cfg(feature = "server")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reader {
    User,
    Anonymous,
}

/// The request's state and who is asking, for the reads that may serve a visitor. The caller
/// still has to run `allow_read` against the project in question before returning anything.
#[cfg(feature = "server")]
fn reader(session: auth::UserSession) -> (Reader, thermite_core::ThermiteState) {
    let reader = if session.data().is_ok() {
        Reader::User
    } else {
        Reader::Anonymous
    };
    (
        reader,
        crate::server::state::AppState::global().thermite.clone(),
    )
}

/// The rule itself, kept pure so it can be tested: anonymous reads reach exactly the configured
/// demo project and nothing else.
#[cfg(feature = "server")]
fn readable(reader: Reader, demo: Option<&str>, slug: &str) -> bool {
    reader == Reader::User || demo == Some(slug)
}

#[cfg(feature = "server")]
fn allow_read(reader: Reader, slug: &str) -> Result<(), ServerFnError> {
    let config = &crate::server::state::AppState::global().config;
    if readable(reader, config.demo_project.as_deref(), slug) {
        Ok(())
    } else {
        Err(ServerFnError::from(AppError::Unauthorized))
    }
}

/// The demo project's slug, if one is configured — what the shell and the landing page need to
/// know before anyone signs in, so this one takes no session.
#[post("/api/errors/demo")]
pub async fn demo_project() -> Result<Option<String>, ServerFnError> {
    Ok(crate::server::state::AppState::global()
        .config
        .demo_project
        .clone())
}

/// Every project with its attention flags, for the dashboard landing page.
#[post("/api/errors/overview", session: auth::UserSession)]
pub async fn project_overview() -> Result<Vec<ProjectOverviewRow>, ServerFnError> {
    let state = thermite(session)?;
    let overview = thermite_core::api::overview::all(&state.db)
        .await
        .map_err(app_error)?;
    Ok(overview.into_iter().map(ProjectOverviewRow::from).collect())
}

/// What appeared or came back in the last 24 hours, across every project.
#[post("/api/errors/recent", session: auth::UserSession)]
pub async fn recent_issues() -> Result<Vec<FeedRow>, ServerFnError> {
    let state = thermite(session)?;
    let items = thermite_core::api::overview::recent(&state.db, 20)
        .await
        .map_err(app_error)?;
    Ok(items.into_iter().map(FeedRow::from).collect())
}

/// Alerts delivery gave up on, most recent first.
#[post("/api/errors/alerts/dead", session: auth::UserSession)]
pub async fn dead_letters() -> Result<Vec<DeadLetterRow>, ServerFnError> {
    let state = thermite(session)?;
    let rows = thermite_core::alerts::dead_lettered(&state.db)
        .await
        .map_err(app_error)?;
    Ok(rows.into_iter().map(DeadLetterRow::from).collect())
}

/// Puts a dead-lettered alert back in the queue. `false` when there was nothing to retry.
#[post("/api/errors/alerts/retry", session: auth::UserSession)]
pub async fn retry_alert(id: i64) -> Result<bool, ServerFnError> {
    let state = thermite(session)?;
    thermite_core::alerts::retry(&state.db, id)
        .await
        .map_err(app_error)
}

/// One project's summary, for the settings page. Reuses the list query — a self-hosted
/// instance has few projects, and a second query shape would be more code than it saves.
#[post("/api/errors/projects/one", session: auth::UserSession)]
pub async fn get_project(slug: String) -> Result<ProjectSummary, ServerFnError> {
    let (reader, state) = reader(session);
    allow_read(reader, &slug)?;
    let projects = thermite_core::api::projects::all(&state.db, &state.config)
        .await
        .map_err(app_error)?;
    let mut project = projects
        .into_iter()
        .find(|p| p.slug == slug)
        .map(ProjectSummary::from)
        .ok_or_else(|| ServerFnError::from(AppError::NotFound))?;
    // A visitor gets the board, not the credential to write into it or where its alerts go.
    if reader == Reader::Anonymous {
        project.dsn = String::new();
        project.keys.clear();
        project.alert_email = None;
        project.alert_webhook = None;
    }
    Ok(project)
}

#[post("/api/errors/projects", session: auth::UserSession)]
pub async fn list_projects() -> Result<Vec<ProjectSummary>, ServerFnError> {
    let state = thermite(session)?;
    let projects = thermite_core::api::projects::all(&state.db, &state.config)
        .await
        .map_err(app_error)?;
    Ok(projects.into_iter().map(ProjectSummary::from).collect())
}

#[post("/api/errors/projects/create", session: auth::UserSession)]
pub async fn create_project(slug: String, name: String) -> Result<ProjectSummary, ServerFnError> {
    let state = thermite(session)?;
    let name = name.trim();

    let created = thermite_core::api::admin::create_project(
        &state.db,
        &state.config,
        &thermite_core::api::admin::NewProject {
            slug,
            name: (!name.is_empty()).then(|| name.to_string()),
        },
    )
    .await
    .map_err(app_error)?;

    Ok(ProjectSummary {
        id: created.id,
        slug: created.slug,
        name: created.name,
        dsn: created.dsn,
        unresolved_issues: 0,
        total_issues: 0,
        events_last_24h: 0,
        alert_email: None,
        alert_webhook: None,
        repo_url: None,
        keys: Vec::new(),
    })
}

/// Mint an additional, labeled DSN for one component of a project ('worker', 'saas').
/// Events sent through it share the project's issue stream and carry
/// `component: <label>` as a filterable tag.
#[post("/api/errors/projects/keys", session: auth::UserSession)]
pub async fn create_project_key(
    slug: String,
    label: String,
) -> Result<crate::models::errors::ProjectKey, ServerFnError> {
    let state = thermite(session)?;

    let created =
        thermite_core::api::admin::create_project_key(&state.db, &state.config, &slug, &label)
            .await
            .map_err(app_error)?;

    Ok(crate::models::errors::ProjectKey {
        label: created.label,
        dsn: created.dsn,
    })
}

/// Set or clear the repository a triaging agent opens its pull request against. Blank clears it.
#[post("/api/errors/projects/repo", session: auth::UserSession)]
pub async fn set_repo_url(slug: String, repo_url: String) -> Result<(), ServerFnError> {
    let state = thermite(session)?;

    thermite_core::api::admin::set_repo_url(
        &state.db,
        &slug,
        &thermite_core::api::admin::RepoSettings {
            repo_url: Some(repo_url),
        },
    )
    .await
    .map_err(app_error)?;

    Ok(())
}

/// Set or clear where this project's alerts are delivered. Blank falls back to the instance-wide
/// `THERMITE_ALERT_EMAIL` / `THERMITE_ALERT_WEBHOOK`.
#[post("/api/errors/projects/alerts", session: auth::UserSession)]
pub async fn set_alert_routing(
    slug: String,
    alert_email: String,
    alert_webhook: String,
) -> Result<(), ServerFnError> {
    let state = thermite(session)?;

    thermite_core::api::admin::set_alert_routing(
        &state.db,
        &slug,
        &thermite_core::api::admin::AlertRouting {
            alert_email: Some(alert_email),
            alert_webhook: Some(alert_webhook),
        },
    )
    .await
    .map_err(app_error)?;

    Ok(())
}

/// Change a project's display name. The slug is fixed — it lives in every configured DSN.
#[post("/api/errors/projects/rename", session: auth::UserSession)]
pub async fn rename_project(slug: String, name: String) -> Result<(), ServerFnError> {
    let state = thermite(session)?;
    thermite_core::api::admin::rename_project(&state.db, &slug, &name)
        .await
        .map_err(app_error)?;
    Ok(())
}

/// Delete a project and everything under it: issues, events, rollups, monitors, keys.
/// Irreversible; its DSNs stop authenticating immediately.
#[post("/api/errors/projects/delete", session: auth::UserSession)]
pub async fn delete_project(slug: String) -> Result<(), ServerFnError> {
    let state = thermite(session)?;
    thermite_core::api::admin::delete_project(&state.db, &slug)
        .await
        .map_err(app_error)?;
    Ok(())
}

/// Revoke a labeled component DSN. The default (unlabeled) key cannot be revoked here.
#[post("/api/errors/projects/keys/delete", session: auth::UserSession)]
pub async fn delete_project_key(slug: String, label: String) -> Result<(), ServerFnError> {
    let state = thermite(session)?;
    thermite_core::api::admin::delete_project_key(&state.db, &slug, &label)
        .await
        .map_err(app_error)?;
    Ok(())
}

#[post("/api/errors/issues", session: auth::UserSession)]
pub async fn list_issues(query: IssueQuery) -> Result<Vec<IssueRow>, ServerFnError> {
    let (reader, state) = reader(session);
    allow_read(reader, &query.project)?;

    // The component filter is just a tag filter — the label is synthesized into
    // issue_tags at ingest, exactly like environment. An explicit tag filter takes the one
    // slot the API offers.
    let tag = query
        .tag
        .or_else(|| query.component.map(|c| format!("component:{c}")));
    let issues = thermite_core::api::issues::for_project(
        &state.db,
        &query.project,
        query.status.as_deref(),
        query.query.as_deref(),
        query.environment.as_deref(),
        tag.as_deref(),
        Some(&query.sort),
        Some(query.limit),
        Some(query.offset),
    )
    .await
    .map_err(app_error)?;

    // Which issues an agent has already looked at, in one query rather than one per row.
    let ids: Vec<i64> = issues.iter().map(|i| i.issue.id).collect();
    let analysed = thermite_core::api::analyses::issues_with_analyses(&state.db, &ids)
        .await
        .map_err(app_error)?;

    Ok(issues
        .into_iter()
        .map(|item| {
            let mut row = IssueRow::from(item);
            row.has_analysis = analysed.contains(&row.id);
            row
        })
        .collect())
}

/// The environments a project has reported from, most active first. Feeds the filter dropdown.
#[post("/api/errors/environments", session: auth::UserSession)]
pub async fn environments(project: String) -> Result<Vec<String>, ServerFnError> {
    let (reader, state) = reader(session);
    allow_read(reader, &project)?;
    let values = thermite_core::api::tags::values_of(&state.db, &project, "environment")
        .await
        .map_err(app_error)?;
    Ok(values.into_iter().map(|v| v.value).collect())
}

/// The components a project's events have reported through (labeled DSN keys),
/// most active first. Feeds the filter dropdown.
#[post("/api/errors/components", session: auth::UserSession)]
pub async fn components(project: String) -> Result<Vec<String>, ServerFnError> {
    let (reader, state) = reader(session);
    allow_read(reader, &project)?;
    let values = thermite_core::api::tags::values_of(&state.db, &project, "component")
        .await
        .map_err(app_error)?;
    Ok(values.into_iter().map(|v| v.value).collect())
}

/// A project's cron monitors and whether their last run was on time.
#[post("/api/errors/monitors", session: auth::UserSession)]
pub async fn list_monitors(project: String) -> Result<Vec<MonitorRow>, ServerFnError> {
    let (reader, state) = reader(session);
    allow_read(reader, &project)?;
    let monitors = thermite_core::api::monitors::of_project(&state.db, &project)
        .await
        .map_err(app_error)?;
    Ok(monitors.into_iter().map(MonitorRow::from).collect())
}

#[post("/api/errors/stats", session: auth::UserSession)]
pub async fn project_stats(project: String, window: String) -> Result<ProjectStats, ServerFnError> {
    let (reader, state) = reader(session);
    allow_read(reader, &project)?;
    let stats = thermite_core::api::stats::for_project(&state.db, &project, Some(&window))
        .await
        .map_err(app_error)?;
    Ok(stats.into())
}

/// Release health for the project's newest releases. Empty when no SDK reports sessions, which is
/// what keeps the panel off the page entirely.
#[post("/api/errors/releases", session: auth::UserSession)]
pub async fn release_health(
    project: String,
    window: String,
) -> Result<Vec<ReleaseHealthRow>, ServerFnError> {
    let (reader, state) = reader(session);
    allow_read(reader, &project)?;
    let releases =
        thermite_core::api::releases::for_project(&state.db, &project, Some(&window), Some(5))
            .await
            .map_err(app_error)?;

    Ok(releases
        .releases
        .into_iter()
        .map(ReleaseHealthRow::from)
        .collect())
}

#[post("/api/errors/issue", session: auth::UserSession)]
pub async fn issue_detail(id: i64) -> Result<IssueDetail, ServerFnError> {
    let (reader, state) = reader(session);
    let detail = thermite_core::api::issues::detail_of(&state.db, id)
        .await
        .map_err(app_error)?;
    let slug = thermite_core::api::projects::slug_of(&state.db, detail.issue.project_id)
        .await
        .map_err(app_error)?;
    allow_read(reader, &slug)?;
    let now = chrono::Utc::now();

    Ok(IssueDetail {
        id: detail.issue.id,
        project_slug: slug,
        title: detail.issue.title,
        culprit: detail.issue.culprit,
        level: detail.issue.level,
        status: detail.issue.status,
        times_seen: detail.issue.times_seen,
        first_seen: detail.issue.first_seen.to_rfc3339(),
        last_seen: detail.issue.last_seen.to_rfc3339(),
        first_seen_ago: crate::models::errors::ago(detail.issue.first_seen, now),
        last_seen_ago: crate::models::errors::ago(detail.issue.last_seen, now),
        latest_event: detail.latest_event.map(Into::into),
        analyses: detail.analyses.into_iter().map(Into::into).collect(),
        tags: detail.tags.into_iter().map(Into::into).collect(),
        users_affected: detail.users_affected,
        first_seen_release: detail.first_seen_release,
        regressed_from_release: detail.regressed_from_release,
        repo_url: detail.repo_url,
    })
}

/// The issue's retained events, newest first, for stepping through them on the issue page.
#[post("/api/errors/issue/events", session: auth::UserSession)]
pub async fn issue_events(id: i64) -> Result<Vec<EventRef>, ServerFnError> {
    let (reader, state) = reader(session);
    let slug = thermite_core::api::issues::project_slug_of_issue(&state.db, id)
        .await
        .map_err(app_error)?;
    allow_read(reader, &slug)?;
    let refs = thermite_core::api::issues::event_refs(&state.db, id, Some(100))
        .await
        .map_err(app_error)?;
    Ok(refs.into_iter().map(Into::into).collect())
}

/// One event in full, by the id the SDK assigned it.
#[post("/api/errors/event", session: auth::UserSession)]
pub async fn event_detail(event_id: String) -> Result<EventDetail, ServerFnError> {
    let (reader, state) = reader(session);
    let event = thermite_core::api::issues::event_by_id(&state.db, &event_id)
        .await
        .map_err(app_error)?;
    let slug = thermite_core::api::issues::project_slug_of_issue(&state.db, event.issue_id)
        .await
        .map_err(app_error)?;
    allow_read(reader, &slug)?;
    Ok(event.into())
}

/// A note from a person, kept beside the agents' analyses so the next agent to pick the issue
/// up reads it too — "not the cache, the retry loop" is the most valuable line on the page.
/// Stored as an analysis with `metadata.kind = "note"` and the user's name as `source`.
#[post("/api/errors/issue/note", session: auth::UserSession)]
pub async fn post_note(issue_id: i64, text: String) -> Result<(), ServerFnError> {
    let user = session
        .data()
        .map_err(|_| ServerFnError::from(AppError::Unauthorized))?;
    let state = crate::server::state::AppState::global().thermite.clone();

    thermite_core::api::analyses::record(
        &state.db,
        issue_id,
        thermite_core::api::analyses::NewAnalysis {
            source: user.username,
            summary: text.trim().to_string(),
            details: None,
            suggested_fix: None,
            confidence: None,
            release: None,
            fix_url: None,
            metadata: Some(serde_json::json!({ "kind": "note" })),
        },
    )
    .await
    .map_err(app_error)?;
    Ok(())
}

#[post("/api/errors/issue/status", session: auth::UserSession)]
pub async fn set_issue_status(
    id: i64,
    status: String,
    in_next_release: bool,
) -> Result<(), ServerFnError> {
    let state = thermite(session)?;
    thermite_core::api::issues::update_status(&state.db, id, &status, in_next_release)
        .await
        .map_err(app_error)?;
    Ok(())
}

/// One status for several issues — the bulk bar on the issue list. One round trip rather than one
/// per issue; each update is still its own statement, so an id that fails does not roll back the
/// others.
#[post("/api/errors/issues/status", session: auth::UserSession)]
pub async fn set_issues_status(
    ids: Vec<i64>,
    status: String,
    in_next_release: bool,
) -> Result<(), ServerFnError> {
    let state = thermite(session)?;
    for id in ids {
        thermite_core::api::issues::update_status(&state.db, id, &status, in_next_release)
            .await
            .map_err(app_error)?;
    }
    Ok(())
}

/// Most synthetic events one click may send.
///
/// The playground exists to populate a dashboard, not to load-test: a burst large enough to be
/// interesting is small, and the per-project quota would refuse the rest anyway.
#[cfg(feature = "server")]
const MAX_DEMO_BURST: u32 = 25;

/// Raises `count` synthetic errors of `kind` against a project, through the real ingest endpoint.
///
/// Returns how many the endpoint accepted. Sending stops at the first refusal and reports it —
/// a spent quota is a legitimate answer, and pretending otherwise would misrepresent the result.
#[post("/api/errors/demo/raise", session: auth::UserSession)]
pub async fn raise_demo_error(
    project_id: i64,
    kind: String,
    environment: String,
    release: String,
    count: u32,
) -> Result<u32, ServerFnError> {
    use crate::server::demo_events;

    let state = thermite(session)?;

    let environment = non_empty(&environment, "production");
    let release = non_empty(&release, "1.0.0");
    let count = count.clamp(1, MAX_DEMO_BURST);

    // The DSN is the credential an SDK would be configured with, so the playground authenticates
    // exactly as one: look the project up, then send with its own public key.
    let projects = thermite_core::api::projects::all(&state.db, &state.config)
        .await
        .map_err(app_error)?;
    let project = projects
        .iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| ServerFnError::from(AppError::NotFound))?;
    let public_key = thermite_core::auth::sentry_key_from_dsn(&project.dsn).ok_or_else(|| {
        ServerFnError::from(AppError::Internal(
            "project DSN has no public key".to_string(),
        ))
    })?;

    let client = reqwest::Client::new();
    let mut sent = 0;

    for seq in 0..count as usize {
        let event = demo_events::payload(&kind, &environment, &release, seq).ok_or_else(|| {
            ServerFnError::from(AppError::Validation(format!("unknown error kind: {kind}")))
        })?;

        demo_events::send(
            &client,
            &state.config.base_url,
            project_id,
            &public_key,
            &event,
        )
        .await
        .map_err(|e| {
            // Partial success is worth reporting rather than discarding.
            ServerFnError::from(AppError::Internal(format!(
                "sent {sent} of {count}, then: {e}"
            )))
        })?;

        sent += 1;
    }

    Ok(sent)
}

#[cfg(feature = "server")]
fn non_empty(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::{Reader, readable};

    #[test]
    fn a_visitor_reads_the_demo_project_and_nothing_else() {
        assert!(readable(Reader::Anonymous, Some("demo"), "demo"));
        assert!(!readable(Reader::Anonymous, Some("demo"), "billing"));
        assert!(!readable(Reader::Anonymous, None, "demo"));
    }

    #[test]
    fn a_user_reads_everything() {
        assert!(readable(Reader::User, None, "billing"));
        assert!(readable(Reader::User, Some("demo"), "billing"));
    }
}
