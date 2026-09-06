//! Release health: how much traffic each release served, and how much of it ended badly.
//!
//! Reads the `session_counts` rollup, never a per-session table — there isn't one. See
//! [`crate::ingest::sessions`] for why, and [`crate::protocol::session`] for the counting rule the
//! numbers here inherit.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::api::stats::Window;
use crate::error::{AppError, AppResult};
use crate::state::ThermiteState;

/// Releases returned by default. The list is newest-first and nobody triages the twentieth
/// release back; the cap keeps the dashboard query bounded on a project with 10k of them.
const DEFAULT_LIMIT: i64 = 10;

/// Sessions a release needs before a crash-free rate is reported at all.
///
/// Below this the rate is noise dressed as a measurement: at three sessions one crash reads as
/// 66.7%, which looks like a catastrophe and means nothing. `None` renders as "not enough data",
/// which is the honest answer.
const MIN_SESSIONS_FOR_RATE: i64 = 50;

/// One rollup bucket's four counters.
#[derive(Debug, Default, Clone, Copy)]
struct BucketTotals {
    sessions: i64,
    errored: i64,
    crashed: i64,
    abnormal: i64,
}

/// Rollup buckets indexed by release, then by bucket — one query's worth for the whole page.
type ByRelease = BTreeMap<i64, BTreeMap<DateTime<Utc>, BucketTotals>>;

#[derive(Debug, Deserialize)]
pub struct ReleasesQuery {
    window: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ReleaseHealth {
    pub version: String,
    /// When Thermite first saw this release — the ordering, since version strings do not sort.
    pub first_seen: DateTime<Utc>,
    pub sessions: i64,
    pub errored: i64,
    pub crashed: i64,
    pub abnormal: i64,
    /// `1 - crashed / sessions`, or `None` below [`MIN_SESSIONS_FOR_RATE`].
    pub crash_free_rate: Option<f64>,
    /// Session volume per bucket, oldest first, zero-filled to the window.
    pub series: Vec<i64>,
}

#[derive(Debug, Serialize)]
pub struct Releases {
    pub window: &'static str,
    pub resolution: &'static str,
    pub releases: Vec<ReleaseHealth>,
}

pub async fn list(
    State(state): State<ThermiteState>,
    Path(slug): Path<String>,
    Query(query): Query<ReleasesQuery>,
) -> AppResult<Json<Releases>> {
    Ok(Json(
        for_project(&state.db, &slug, query.window.as_deref(), query.limit).await?,
    ))
}

/// Shared with the dashboard and the MCP `release_health` tool.
/// The most recently first-seen release of a project — the one a demo feeder should keep
/// reporting, so the release picture stays coherent.
pub async fn latest_version(db: &PgPool, project_id: i64) -> AppResult<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        "select version from releases where project_id = $1 order by id desc limit 1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(v,)| v))
}

pub async fn for_project(
    db: &PgPool,
    slug: &str,
    window: Option<&str>,
    limit: Option<i64>,
) -> AppResult<Releases> {
    let window = Window::parse(window)?;
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, 100);

    let project: Option<(i64,)> = sqlx::query_as("select id from projects where slug = $1")
        .bind(slug)
        .fetch_optional(db)
        .await?;
    let Some((project_id,)) = project else {
        return Err(AppError::NotFound);
    };

    // Newest first by id, which is first-sighting order — the same ordering
    // resolved-until-next-release uses, and the only one that is meaningful for git SHAs.
    let releases: Vec<(i64, String, DateTime<Utc>)> = sqlx::query_as(
        "select id, version, first_seen
           from releases
          where project_id = $1
          order by id desc
          limit $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(db)
    .await?;

    if releases.is_empty() {
        return Ok(Releases {
            window: window.label(),
            resolution: window.resolution(),
            releases: Vec::new(),
        });
    }

    // The newest bucket comes from the database clock so it lines up with the ones `date_trunc`
    // produced at ingest, even if this process's clock has drifted.
    let unit = if window.daily() { "day" } else { "hour" };
    let (latest,): (DateTime<Utc>,) =
        sqlx::query_as(&format!("select date_trunc('{unit}', now())"))
            .fetch_one(db)
            .await?;
    let oldest = latest - window.step() * ((window.buckets() - 1) as i32);

    let ids: Vec<i64> = releases.iter().map(|(id, _, _)| *id).collect();
    let rows: Vec<(i64, DateTime<Utc>, i64, i64, i64, i64)> = sqlx::query_as(&format!(
        "select release_id, {} as at,
                sum(sessions)::bigint, sum(errored)::bigint,
                sum(crashed)::bigint,  sum(abnormal)::bigint
           from session_counts
          where release_id = any($1) and bucket >= $2
          group by release_id, at",
        window.trunc()
    ))
    .bind(&ids)
    .bind(oldest)
    .fetch_all(db)
    .await?;

    let mut counted = ByRelease::new();
    for (release_id, at, sessions, errored, crashed, abnormal) in rows {
        counted.entry(release_id).or_default().insert(
            at,
            BucketTotals {
                sessions,
                errored,
                crashed,
                abnormal,
            },
        );
    }

    let releases = releases
        .into_iter()
        .map(|(id, version, first_seen)| {
            let buckets = counted.remove(&id).unwrap_or_default();

            // Walk every bucket in the window so the caller gets a continuous series rather than
            // only the buckets that happened to see traffic.
            let mut series = Vec::with_capacity(window.buckets() as usize);
            let mut total = BucketTotals::default();
            let mut at = oldest;
            while at <= latest {
                let bucket = buckets.get(&at).copied().unwrap_or_default();
                series.push(bucket.sessions);
                total.sessions += bucket.sessions;
                total.errored += bucket.errored;
                total.crashed += bucket.crashed;
                total.abnormal += bucket.abnormal;
                at += window.step();
            }
            let BucketTotals {
                sessions,
                errored,
                crashed,
                abnormal,
            } = total;

            ReleaseHealth {
                version,
                first_seen,
                sessions,
                errored,
                crashed,
                abnormal,
                crash_free_rate: (sessions >= MIN_SESSIONS_FOR_RATE)
                    .then(|| 1.0 - crashed as f64 / sessions as f64),
                series,
            }
        })
        .collect();

    Ok(Releases {
        window: window.label(),
        resolution: window.resolution(),
        releases,
    })
}
