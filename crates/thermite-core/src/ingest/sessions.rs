//! Release health: turning session items into the `session_counts` rollup.
//!
//! Sessions ride the same endpoint and DSN credential as errors, the way check-ins do. Unlike
//! check-ins they never become events — a session is not a thing that went wrong, it is the
//! denominator that says how much traffic a release served, so a crash count means something.
//!
//! Nothing per-session is stored. [`crate::protocol::session`] carries the counting rule that
//! makes that possible; this module is the write side of it.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::AppResult;
use crate::ingest::releases;
use crate::protocol::envelope::Item;
use crate::protocol::event;
use crate::protocol::session::{Aggregates, Delta, Session};

/// Folds one session item into the rollup.
///
/// A malformed payload is logged and skipped rather than failing the envelope — one bad item must
/// not discard the events beside it. Database failures propagate, so a rollup write that cannot
/// happen is not silently acknowledged.
pub async fn record(
    db: &PgPool,
    project_id: i64,
    item: &Item<'_>,
    received_at: DateTime<Utc>,
) -> AppResult<()> {
    let Some(parsed) = parse(project_id, item, received_at) else {
        return Ok(());
    };

    // Release health that cannot be attributed to a release answers no question, and the rollup is
    // keyed on `releases.id` — whose per-project cap is what bounds this table. A session with no
    // release is counted nowhere rather than under a null key.
    let Some(release) = parsed.release.filter(|version| !version.is_empty()) else {
        tracing::debug!(project_id, "dropping session item with no release");
        return Ok(());
    };

    let mut tx = db.begin().await?;

    // `None` means the project is at its release cap; treat the session as having no release.
    let Some(release_id) = releases::resolve(&mut tx, project_id, &release).await? else {
        return Ok(());
    };

    for (bucket, delta) in parsed.buckets {
        sqlx::query(
            "insert into session_counts
                 (project_id, release_id, bucket, sessions, errored, crashed, abnormal)
             values ($1, $2, date_trunc('hour', $3::timestamptz), $4, $5, $6, $7)
             on conflict (release_id, bucket) do update
                 set sessions = session_counts.sessions + excluded.sessions,
                     errored  = session_counts.errored  + excluded.errored,
                     crashed  = session_counts.crashed  + excluded.crashed,
                     abnormal = session_counts.abnormal + excluded.abnormal",
        )
        .bind(project_id)
        .bind(release_id)
        .bind(bucket)
        .bind(delta.sessions)
        .bind(delta.errored)
        .bind(delta.crashed)
        .bind(delta.abnormal)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(())
}

/// One session item, reduced to what the rollup needs.
struct Parsed {
    release: Option<String>,
    buckets: Vec<(DateTime<Utc>, Delta)>,
}

/// Reads either item type into the release it belongs to and the buckets it moves.
///
/// Returns `None` for a payload that is unparseable or contributes nothing, so the caller does not
/// open a transaction to write zeroes.
fn parse(project_id: i64, item: &Item<'_>, received_at: DateTime<Utc>) -> Option<Parsed> {
    // Both timestamps come through `event::clamp`, so a broken client clock cannot mint buckets
    // outside the retention window — where the sweep would never reach them.
    let at =
        |raw: Option<&serde_json::Value>| event::clamp(event::parse_timestamp(raw), received_at);

    if item.item_type() == "session" {
        let session: Session = parsed(project_id, item)?;
        let delta = session.delta();
        if delta.is_empty() {
            return None;
        }

        let bucket = at(session.started_at().as_ref());
        return Some(Parsed {
            release: session.attrs.release,
            buckets: vec![(bucket, delta)],
        });
    }

    let batch: Aggregates = parsed(project_id, item)?;

    let buckets: Vec<(DateTime<Utc>, Delta)> = batch
        .aggregates
        .iter()
        .map(|aggregate| (at(aggregate.started.as_ref()), aggregate.delta()))
        .filter(|(_, delta)| !delta.is_empty())
        .fold(Vec::new(), |mut merged, (bucket, delta)| {
            // A batch may repeat a bucket across entries (one per distinct user, say). Merging here
            // keeps the statement count proportional to buckets rather than to entries.
            match merged.iter_mut().find(|(at, _)| *at == bucket) {
                Some((_, existing)) => existing.merge(delta),
                None => merged.push((bucket, delta)),
            }
            merged
        });

    (!buckets.is_empty()).then_some(Parsed {
        release: batch.attrs.release,
        buckets,
    })
}

fn parsed<T: serde::de::DeserializeOwned>(project_id: i64, item: &Item<'_>) -> Option<T> {
    match serde_json::from_slice(item.payload) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            tracing::warn!(
                project_id,
                item_type = item.item_type(),
                error = %e,
                "skipping unparseable session item"
            );
            None
        }
    }
}
