//! Agent findings written back onto an issue.
//!
//! This is what closes the loop. Without it an agent's diagnosis evaporates the moment its run
//! ends; with it, the work is sitting on the issue when a human opens it.
//!
//! Thermite still never calls a model — these rows arrive over the API from whatever agent drained
//! the triage queue, which is why every row records its `source`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;

use crate::error::{AppError, AppResult};
use crate::state::ThermiteState;

const CONFIDENCE: [&str; 3] = ["high", "medium", "low"];

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Analysis {
    pub id: i64,
    pub issue_id: i64,
    pub source: String,
    pub summary: String,
    pub details: Option<String>,
    pub suggested_fix: Option<String>,
    pub confidence: Option<String>,
    pub release: Option<String>,
    /// The pull request the agent opened, when it got that far.
    pub fix_url: Option<String>,
    /// What production made of that fix: `pending`, `held` or `regressed`. Derived on read by
    /// [`crate::api::fixes::grade`], never stored — see that module for why.
    #[sqlx(default)]
    pub fix_verdict: Option<String>,
    /// For a `regressed` verdict, the release since the fix that the issue came back in.
    #[sqlx(default)]
    pub regressed_in: Option<String>,
    pub metadata: Option<Value>,
    pub created_at: DateTime<Utc>,
}

const ANALYSIS_COLUMNS: &str = "id, issue_id, source, summary, details, suggested_fix,
                                confidence, release, fix_url, metadata, created_at";

#[derive(Debug, Deserialize)]
pub struct NewAnalysis {
    /// Who produced this, e.g. "claude-code" or "ci-triage".
    pub source: String,
    /// One-line conclusion. Shown in the issue list.
    pub summary: String,
    /// The full reasoning.
    #[serde(default)]
    pub details: Option<String>,
    /// A concrete patch, or the change to make.
    #[serde(default)]
    pub suggested_fix: Option<String>,
    /// How sure the agent is. A confident wrong answer is the failure mode worth surfacing.
    #[serde(default)]
    pub confidence: Option<String>,
    /// The revision this was reasoned against, so a reader can tell if it is still current.
    #[serde(default)]
    pub release: Option<String>,
    /// The pull request the agent opened for this issue. Part of the analysis rather than a tool
    /// of its own: the lease covers one unit of work, so the agent diagnoses, attempts the fix,
    /// and reports both together — or reports the diagnosis alone when the fix did not come out.
    ///
    /// Must share a host with the project's `repo_url`. Thermite never fetches it.
    #[serde(default)]
    pub fix_url: Option<String>,
    /// Anything else worth keeping: linked PRs, files read, tokens spent.
    #[serde(default)]
    pub metadata: Option<Value>,
}

pub async fn create(
    State(state): State<ThermiteState>,
    Path(issue_id): Path<i64>,
    Json(body): Json<NewAnalysis>,
) -> AppResult<(StatusCode, Json<Analysis>)> {
    let analysis = record(&state.db, issue_id, body).await?;
    Ok((StatusCode::CREATED, Json(analysis)))
}

/// Shared with the MCP `post_analysis` tool.
pub async fn record(db: &PgPool, issue_id: i64, body: NewAnalysis) -> AppResult<Analysis> {
    if body.summary.trim().is_empty() {
        return Err(AppError::BadRequest("summary must not be empty".into()));
    }

    if let Some(confidence) = &body.confidence
        && !CONFIDENCE.contains(&confidence.as_str())
    {
        return Err(AppError::BadRequest(format!(
            "unknown confidence {confidence:?}; expected one of {}",
            CONFIDENCE.join(", ")
        )));
    }

    // Reject up front rather than letting the foreign key fail, so the caller gets a 404 instead of
    // a 500. The project's repository comes back in the same round trip: it is what `fix_url` is
    // validated against.
    let owner: Option<(Option<String>,)> = sqlx::query_as(
        "select p.repo_url from issues i join projects p on p.id = i.project_id where i.id = $1",
    )
    .bind(issue_id)
    .fetch_optional(db)
    .await?;
    let Some((repo_url,)) = owner else {
        return Err(AppError::NotFound);
    };

    let fix_url = check_fix_url(body.fix_url.as_deref(), repo_url.as_deref())?;

    let analysis: Analysis = sqlx::query_as(
        "insert into issue_analyses
             (issue_id, source, summary, details, suggested_fix, confidence, release, fix_url,
              metadata)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         returning id, issue_id, source, summary, details, suggested_fix,
                   confidence, release, fix_url, metadata, created_at",
    )
    .bind(issue_id)
    .bind(body.source.trim())
    .bind(body.summary.trim())
    .bind(&body.details)
    .bind(&body.suggested_fix)
    .bind(&body.confidence)
    .bind(&body.release)
    .bind(&fix_url)
    .bind(&body.metadata)
    .fetch_one(db)
    .await?;

    Ok(analysis)
}

pub async fn list(
    State(state): State<ThermiteState>,
    Path(issue_id): Path<i64>,
) -> AppResult<Json<Vec<Analysis>>> {
    Ok(Json(for_issue(&state.db, issue_id).await?))
}

/// Which of `issue_ids` already carry at least one analysis.
///
/// One query for a whole page rather than one per row.
pub async fn issues_with_analyses(
    db: &PgPool,
    issue_ids: &[i64],
) -> AppResult<std::collections::HashSet<i64>> {
    if issue_ids.is_empty() {
        return Ok(std::collections::HashSet::new());
    }

    let rows: Vec<(i64,)> =
        sqlx::query_as("select distinct issue_id from issue_analyses where issue_id = any($1)")
            .bind(issue_ids)
            .fetch_all(db)
            .await?;

    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Shared with the issue detail endpoint so one request shows any prior agent work.
pub async fn for_issue(db: &PgPool, issue_id: i64) -> AppResult<Vec<Analysis>> {
    let mut rows: Vec<Analysis> = sqlx::query_as(&format!(
        "select {ANALYSIS_COLUMNS}
           from issue_analyses
          where issue_id = $1
          order by created_at desc, id desc"
    ))
    .bind(issue_id)
    .fetch_all(db)
    .await?;

    // A proposed fix is shown with what became of it, or the page is asking the reader to go and
    // find out themselves.
    super::fixes::grade(db, issue_id, &mut rows).await?;

    Ok(rows)
}

/// Validates an agent-supplied pull request link against the project's repository.
///
/// The issue page renders this as a clickable link an operator is expected to trust, and its value
/// comes from whatever agent drained the queue — so it is constrained to the host the operator
/// declared. Host, not full prefix: a pull request opened from a fork lives under a different path
/// on the same host, and rejecting that would only teach agents to stuff the link into `metadata`
/// instead, where nothing checks it at all.
///
/// A project with no `repo_url` cannot accept one. There is nothing to compare against, and an
/// agent that found a repository thermite was never told about is reporting a link no operator
/// vouched for.
fn check_fix_url(fix_url: Option<&str>, repo_url: Option<&str>) -> AppResult<Option<String>> {
    let Some(fix_url) = fix_url.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };

    let Some(repo_url) = repo_url else {
        return Err(AppError::BadRequest(
            "fix_url needs the project to have a repo_url configured to validate it against".into(),
        ));
    };

    let fix = url::Url::parse(fix_url)
        .map_err(|_| AppError::BadRequest("fix_url must be an http(s) URL".into()))?;
    if !matches!(fix.scheme(), "http" | "https") {
        return Err(AppError::BadRequest(
            "fix_url must be an http(s) URL".into(),
        ));
    }

    let repo = url::Url::parse(repo_url)
        .map_err(|_| AppError::BadRequest("the project's repo_url is not a URL".into()))?;

    if fix.host_str() != repo.host_str() {
        return Err(AppError::BadRequest(format!(
            "fix_url must be on the same host as the project's repo_url ({})",
            repo.host_str().unwrap_or("none")
        )));
    }

    Ok(Some(fix_url.to_string()))
}

#[cfg(test)]
mod fix_url_tests {
    use super::check_fix_url;

    const REPO: Option<&str> = Some("https://github.com/hauju/thermite-rs");

    #[test]
    fn a_pull_request_on_the_repo_host_is_accepted() {
        let url = "https://github.com/hauju/thermite-rs/pull/12";
        assert_eq!(
            check_fix_url(Some(url), REPO).unwrap().as_deref(),
            Some(url)
        );
    }

    #[test]
    fn a_fork_on_the_same_host_is_accepted() {
        // The fix may well come from a fork; the host is what the operator vouched for.
        let url = "https://github.com/someone-else/thermite-rs/pull/3";
        assert_eq!(
            check_fix_url(Some(url), REPO).unwrap().as_deref(),
            Some(url)
        );
    }

    #[test]
    fn another_host_is_rejected() {
        assert!(check_fix_url(Some("https://evil.example.com/pull/1"), REPO).is_err());
    }

    #[test]
    fn a_non_http_scheme_is_rejected() {
        // The seat this closes: `javascript:` rendered into an href on a trusted page.
        assert!(check_fix_url(Some("javascript:alert(1)"), REPO).is_err());
        assert!(check_fix_url(Some("file:///etc/passwd"), REPO).is_err());
    }

    #[test]
    fn a_project_with_no_repo_cannot_accept_one() {
        assert!(check_fix_url(Some("https://github.com/a/b/pull/1"), None).is_err());
    }

    #[test]
    fn no_fix_url_is_the_common_case() {
        assert_eq!(check_fix_url(None, REPO).unwrap(), None);
        assert_eq!(check_fix_url(Some("   "), REPO).unwrap(), None);
        // And an analysis with no fix does not need a repo configured.
        assert_eq!(check_fix_url(None, None).unwrap(), None);
    }
}
