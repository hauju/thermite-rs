//! Aggregate numbers for the dashboard: error rate over time, and the current state of a project.
//!
//! Everything here reads the `event_counts` rollup rather than scanning `events`.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::error::{AppError, AppResult};
use crate::state::ThermiteState;

/// A charting window, and the bucket size it implies.
///
/// Resolution is derived rather than configurable: hourly is the finest the rollup stores, and a
/// month of hourly points is 720 values nobody can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Day,
    Week,
    Month,
}

impl Window {
    pub(crate) fn parse(raw: Option<&str>) -> AppResult<Self> {
        match raw.unwrap_or("24h") {
            "24h" => Ok(Window::Day),
            "7d" => Ok(Window::Week),
            "30d" => Ok(Window::Month),
            other => Err(AppError::BadRequest(format!(
                "unknown window {other:?}; expected 24h, 7d or 30d"
            ))),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Window::Day => "24h",
            Window::Week => "7d",
            Window::Month => "30d",
        }
    }

    pub(crate) fn daily(self) -> bool {
        matches!(self, Window::Month)
    }

    pub(crate) fn resolution(self) -> &'static str {
        if self.daily() { "1d" } else { "1h" }
    }

    /// Number of buckets in the window, including the current partial one.
    pub(crate) fn buckets(self) -> i64 {
        match self {
            Window::Day => 24,
            Window::Week => 24 * 7,
            Window::Month => 30,
        }
    }

    pub(crate) fn step(self) -> Duration {
        if self.daily() {
            Duration::days(1)
        } else {
            Duration::hours(1)
        }
    }

    /// SQL to collapse hourly rollup rows to this window's resolution.
    pub(crate) fn trunc(self) -> &'static str {
        if self.daily() {
            "date_trunc('day', bucket)"
        } else {
            "bucket"
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct StatsQuery {
    window: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Stats {
    pub window: &'static str,
    pub resolution: &'static str,
    /// Continuous buckets, oldest first, zero-filled. A chart can render this directly without
    /// having to reason about gaps.
    pub series: Vec<Bucket>,
    pub totals: Totals,
}

#[derive(Debug, Serialize)]
pub struct Bucket {
    pub bucket: DateTime<Utc>,
    pub count: i64,
    /// Per-level counts. Only levels actually present appear.
    pub levels: BTreeMap<String, i64>,
    /// Events that did not get stored: dropped by ingest (over quota, unsupported, invalid) or
    /// discarded by the SDK before it sent them (client reports). Read from `ingest_outcomes`,
    /// which buckets on arrival time while `count` buckets on the event's own timestamp — close
    /// enough to chart side by side, not exact per-bucket bookkeeping.
    pub dropped: i64,
}

#[derive(Debug, Serialize)]
pub struct Totals {
    pub events: i64,
    /// Total drops inside the window, with the per-reason split alongside. Reasons that carry
    /// a detail spell it out (`unsupported:transaction`, `client_discarded:queue_overflow`), so
    /// the number can be acted on rather than only noticed.
    pub dropped: i64,
    pub dropped_by_reason: BTreeMap<String, i64>,
    pub unresolved_issues: i64,
    /// Issues first seen inside the window and still unresolved.
    pub new_issues: i64,
    /// Issues that were resolved and came back inside the window.
    pub regressions: i64,
}

pub async fn project(
    State(state): State<ThermiteState>,
    Path(slug): Path<String>,
    Query(query): Query<StatsQuery>,
) -> AppResult<Json<Stats>> {
    Ok(Json(
        for_project(&state.db, &slug, query.window.as_deref()).await?,
    ))
}

/// Shared with the MCP `project_stats` tool.
pub async fn for_project(db: &PgPool, slug: &str, window: Option<&str>) -> AppResult<Stats> {
    let window = Window::parse(window)?;

    let project_id: Option<(i64,)> = sqlx::query_as("select id from projects where slug = $1")
        .bind(slug)
        .fetch_optional(db)
        .await?;
    let Some((project_id,)) = project_id else {
        return Err(AppError::NotFound);
    };

    let series = series(db, project_id, window).await?;
    let totals = totals(db, project_id, &series).await?;

    Ok(Stats {
        window: window.label(),
        resolution: window.resolution(),
        series,
        totals,
    })
}

async fn series(db: &PgPool, project_id: i64, window: Window) -> AppResult<Vec<Bucket>> {
    // The newest bucket is derived from the database clock, so buckets line up with the ones
    // `date_trunc` produced at ingest even if this process's clock has drifted.
    let unit = if window.daily() { "day" } else { "hour" };
    let (latest,): (DateTime<Utc>,) =
        sqlx::query_as(&format!("select date_trunc('{unit}', now())"))
            .fetch_one(db)
            .await?;

    let oldest = latest - window.step() * ((window.buckets() - 1) as i32);

    let rows: Vec<(DateTime<Utc>, String, i64)> = sqlx::query_as(&format!(
        "select {} as at, level, sum(count)::bigint
           from event_counts
          where project_id = $1 and bucket >= $2
          group by at, level",
        window.trunc()
    ))
    .bind(project_id)
    .bind(oldest)
    .fetch_all(db)
    .await?;

    let dropped_rows: Vec<(DateTime<Utc>, i64)> = sqlx::query_as(&format!(
        "select {} as at, sum(count)::bigint
           from ingest_outcomes
          where project_id = $1 and bucket >= $2 and outcome <> 'accepted'
          group by at",
        window.trunc()
    ))
    .bind(project_id)
    .bind(oldest)
    .fetch_all(db)
    .await?;
    let mut dropped: BTreeMap<DateTime<Utc>, i64> = dropped_rows.into_iter().collect();

    // Index what came back, then walk every bucket in the window so the caller gets a continuous
    // series rather than only the buckets that happened to have events.
    let mut counted: BTreeMap<DateTime<Utc>, BTreeMap<String, i64>> = BTreeMap::new();
    for (at, level, count) in rows {
        counted.entry(at).or_default().insert(level, count);
    }

    let mut series = Vec::with_capacity(window.buckets() as usize);
    let mut at = oldest;
    while at <= latest {
        let levels = counted.remove(&at).unwrap_or_default();
        series.push(Bucket {
            bucket: at,
            count: levels.values().sum(),
            levels,
            dropped: dropped.remove(&at).unwrap_or(0),
        });
        at += window.step();
    }

    Ok(series)
}

/// The window is taken from the series rather than passed separately, so "new issues" and the chart
/// can never be measuring different periods.
async fn totals(db: &PgPool, project_id: i64, series: &[Bucket]) -> AppResult<Totals> {
    let since = series.first().map(|b| b.bucket).unwrap_or_else(Utc::now);

    let (unresolved_issues, new_issues, regressions): (i64, i64, i64) = sqlx::query_as(
        "select
             (select count(*) from issues
               where project_id = $1 and status = 'unresolved'),
             (select count(*) from issues
               where project_id = $1 and status = 'unresolved' and first_seen >= $2),
             (select count(*) from notifications
               where project_id = $1 and kind = 'regression' and created_at >= $2)",
    )
    .bind(project_id)
    .bind(since)
    .fetch_one(db)
    .await?;

    let dropped_by_reason: BTreeMap<String, i64> = sqlx::query_as(
        "select outcome, sum(count)::bigint
           from ingest_outcomes
          where project_id = $1 and bucket >= $2 and outcome <> 'accepted'
          group by outcome",
    )
    .bind(project_id)
    .bind(since)
    .fetch_all(db)
    .await?
    .into_iter()
    .collect();

    Ok(Totals {
        // Summed from the series we already have rather than re-queried, so the headline number
        // and the chart can never disagree.
        events: series.iter().map(|b| b.count).sum(),
        dropped: series.iter().map(|b| b.dropped).sum(),
        dropped_by_reason,
        unresolved_issues,
        new_issues,
        regressions,
    })
}

/// 24 hourly counts per issue, oldest first, for the sparkline on each issue-list row.
///
/// One query for the whole page rather than one per row.
pub async fn sparklines(db: &PgPool, issue_ids: &[i64]) -> AppResult<BTreeMap<i64, Vec<i64>>> {
    const BUCKETS: i64 = 24;

    if issue_ids.is_empty() {
        return Ok(BTreeMap::new());
    }

    let (latest,): (DateTime<Utc>,) = sqlx::query_as("select date_trunc('hour', now())")
        .fetch_one(db)
        .await?;
    let oldest = latest - Duration::hours(BUCKETS - 1);

    let rows: Vec<(i64, DateTime<Utc>, i64)> = sqlx::query_as(
        "select issue_id, bucket, sum(count)::bigint
           from event_counts
          where issue_id = any($1) and bucket >= $2
          group by issue_id, bucket",
    )
    .bind(issue_ids)
    .bind(oldest)
    .fetch_all(db)
    .await?;

    let mut counted: BTreeMap<i64, BTreeMap<DateTime<Utc>, i64>> = BTreeMap::new();
    for (issue_id, bucket, count) in rows {
        counted.entry(issue_id).or_default().insert(bucket, count);
    }

    let mut out = BTreeMap::new();
    for &issue_id in issue_ids {
        let per_issue = counted.remove(&issue_id).unwrap_or_default();
        let mut at = oldest;
        let mut counts = Vec::with_capacity(BUCKETS as usize);
        while at <= latest {
            counts.push(per_issue.get(&at).copied().unwrap_or(0));
            at += Duration::hours(1);
        }
        out.insert(issue_id, counts);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_supported_windows() {
        assert_eq!(Window::parse(None).unwrap(), Window::Day);
        assert_eq!(Window::parse(Some("24h")).unwrap(), Window::Day);
        assert_eq!(Window::parse(Some("7d")).unwrap(), Window::Week);
        assert_eq!(Window::parse(Some("30d")).unwrap(), Window::Month);
        assert!(Window::parse(Some("1h")).is_err());
        assert!(Window::parse(Some("banana")).is_err());
    }

    #[test]
    fn resolution_follows_the_window() {
        assert_eq!(Window::Day.resolution(), "1h");
        assert_eq!(Window::Week.resolution(), "1h");
        // A month of hourly points is 720 values nobody can read.
        assert_eq!(Window::Month.resolution(), "1d");
    }

    #[test]
    fn windows_span_the_period_they_claim() {
        assert_eq!(
            Window::Day.step() * (Window::Day.buckets() as i32 - 1),
            Duration::hours(23)
        );
        assert_eq!(
            Window::Week.step() * (Window::Week.buckets() as i32 - 1),
            Duration::hours(167)
        );
        assert_eq!(
            Window::Month.step() * (Window::Month.buckets() as i32 - 1),
            Duration::days(29)
        );
    }
}
