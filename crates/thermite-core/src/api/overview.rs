//! The instance-wide overview: every project, and whether it needs attention.
//!
//! Reads only counters and rollups — `issues`, `event_counts`, `monitors`, `notifications` —
//! never `events`, so the landing page stays cheap no matter how much a project has ingested.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::PgPool;

use crate::api::page_size;
use crate::error::AppResult;
use crate::state::ThermiteState;

/// Hourly buckets on the sparkline, matching the "events 24h" number beside it.
const SERIES_BUCKETS: i64 = 24;

/// How far back the feed looks, in hours. The same window as "new" everywhere else.
const FEED_HOURS: i32 = 24;

/// An issue that appeared or came back recently, from any project — one row of the dashboard's
/// feed.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FeedItem {
    pub issue_id: i64,
    pub project_slug: String,
    pub project_name: String,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    pub times_seen: i64,
    pub users_affected: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// `regression` when the issue reopened inside the window, otherwise `new`.
    pub kind: String,
}

pub async fn recent_list(State(state): State<ThermiteState>) -> AppResult<Json<Vec<FeedItem>>> {
    Ok(Json(recent(&state.db, 20).await?))
}

/// Unresolved issues first seen or reopened in the last 24 hours, across every project, most
/// recent activity first. An old issue that is merely still failing is not news; one that came
/// back after a fix is. Reads `issues` and `notifications` only, like the rest of this module.
pub async fn recent(db: &PgPool, limit: i64) -> AppResult<Vec<FeedItem>> {
    let items: Vec<FeedItem> = sqlx::query_as(
        "with reopened as (
             select distinct issue_id
               from notifications
              where kind = 'regression'
                and created_at >= now() - make_interval(hours => $2)
         )
         select i.id as issue_id,
                p.slug as project_slug,
                p.name as project_name,
                i.title, i.culprit, i.level, i.times_seen, i.users_affected,
                i.first_seen, i.last_seen,
                case when r.issue_id is not null then 'regression' else 'new' end as kind
           from issues i
           join projects p on p.id = i.project_id
           left join reopened r on r.issue_id = i.id
          where i.status = 'unresolved'
            and (i.first_seen >= now() - make_interval(hours => $2) or r.issue_id is not null)
          order by i.last_seen desc, i.id desc
          limit $1",
    )
    .bind(page_size(Some(limit)))
    .bind(FEED_HOURS)
    .fetch_all(db)
    .await?;

    Ok(items)
}

#[derive(Debug, Serialize)]
pub struct ProjectOverview {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub unresolved_issues: i64,
    pub events_last_24h: i64,
    /// Issues first seen inside the last 24 hours and still unresolved — one already dealt
    /// with needs no attention.
    pub new_issues_24h: i64,
    /// Cron monitors whose last run errored, missed its window, or overran.
    pub monitors_failing: i64,
    /// Alerts dead-lettered after repeated delivery failures — the alert channel itself is
    /// broken, which no alert can report.
    pub alerts_dead_lettered: i64,
    /// 24 hourly event counts, oldest first, for a sparkline.
    pub series: Vec<i64>,
}

pub async fn list(State(state): State<ThermiteState>) -> AppResult<Json<Vec<ProjectOverview>>> {
    Ok(Json(all(&state.db).await?))
}

/// Shared with the dashboard's server function.
pub async fn all(db: &PgPool) -> AppResult<Vec<ProjectOverview>> {
    let rows: Vec<(i64, String, String, i64, i64, i64, i64)> = sqlx::query_as(
        "select p.id,
                p.slug,
                p.name,
                (select count(*) from issues i
                  where i.project_id = p.id and i.status = 'unresolved'),
                (select count(*) from issues i
                  where i.project_id = p.id and i.status = 'unresolved'
                    and i.first_seen >= now() - interval '24 hours'),
                (select count(*) from monitors m
                  where m.project_id = p.id and m.status in ('error', 'missed', 'timeout')),
                (select count(*) from notifications n
                  where n.project_id = p.id and n.alert_failed_at is not null)
           from projects p
          order by p.id",
    )
    .fetch_all(db)
    .await?;

    // The newest bucket comes from the database clock so it lines up with the buckets
    // `date_trunc` produced at ingest, exactly as `stats::series` does.
    let (latest,): (DateTime<Utc>,) = sqlx::query_as("select date_trunc('hour', now())")
        .fetch_one(db)
        .await?;
    let oldest = latest - Duration::hours(SERIES_BUCKETS - 1);

    let counted_rows: Vec<(i64, DateTime<Utc>, i64)> = sqlx::query_as(
        "select project_id, bucket, sum(count)::bigint
           from event_counts
          where bucket >= $1
          group by project_id, bucket",
    )
    .bind(oldest)
    .fetch_all(db)
    .await?;

    let mut counted: BTreeMap<i64, BTreeMap<DateTime<Utc>, i64>> = BTreeMap::new();
    for (project_id, bucket, count) in counted_rows {
        counted.entry(project_id).or_default().insert(bucket, count);
    }

    let overview = rows
        .into_iter()
        .map(
            |(id, slug, name, unresolved, new_issues, monitors, dead_lettered)| {
                let per_project = counted.remove(&id).unwrap_or_default();
                let mut series = Vec::with_capacity(SERIES_BUCKETS as usize);
                let mut at = oldest;
                while at <= latest {
                    series.push(per_project.get(&at).copied().unwrap_or(0));
                    at += Duration::hours(1);
                }

                ProjectOverview {
                    id,
                    slug,
                    name,
                    unresolved_issues: unresolved,
                    // Summed from the series rather than re-queried, so the headline number and
                    // the sparkline beside it can never disagree.
                    events_last_24h: series.iter().sum(),
                    new_issues_24h: new_issues,
                    monitors_failing: monitors,
                    alerts_dead_lettered: dead_lettered,
                    series,
                }
            },
        )
        .collect();

    Ok(overview)
}
