//! Keeps the public demo project alive.
//!
//! A demo project (`THERMITE_DEMO_PROJECT`) nobody feeds goes flat within a day: the sparklines
//! empty, the "new" badges expire, the dashboard says nothing happened. This raises one of the
//! playground's synthetic events into it every so often, through the real ingest endpoint exactly
//! as the playground does — so the demo also stays a smoke test of ingest.
//!
//! Assumes one replica. A second would double the rate, which is harmless.

use std::time::Duration;

use crate::models::AppError;
use crate::server::demo_events;
use crate::server::state::AppState;

/// Between two ticks, in minutes. Enough traffic to draw a sparkline, not enough to look like
/// a load test or to matter against the project's quota.
const MIN_MINUTES: u64 = 10;
const MAX_MINUTES: u64 = 30;

/// The first tick comes soon after boot, so a fresh deploy shows life without waiting half an
/// hour.
const FIRST_TICK: Duration = Duration::from_secs(120);

/// What one tick raises: the kinds weighted towards the ones that demonstrate grouping, and
/// occasionally a small burst so an issue's sparkline is not a flat line of ones.
fn pick(entropy: u64) -> (&'static str, usize) {
    const KINDS: [&str; 12] = [
        "exception",
        "exception",
        "exception",
        "db_timeout",
        "db_timeout",
        "db_timeout",
        "db_timeout",
        "fingerprint",
        "fingerprint",
        "log_message",
        "log_message",
        "warning",
    ];
    let kind = KINDS[(entropy % KINDS.len() as u64) as usize];
    let count = match (entropy >> 8) % 10 {
        0 => 3,
        1 | 2 => 2,
        _ => 1,
    };
    (kind, count)
}

/// Random enough for a demo, from the one entropy source already in the tree.
fn entropy() -> u64 {
    uuid::Uuid::new_v4().as_u128() as u64
}

pub fn spawn(state: AppState) {
    let Some(slug) = state.config.demo_project.clone() else {
        return;
    };
    tracing::info!(project = %slug, "demo feed running");

    tokio::spawn(async move {
        let client = reqwest::Client::new();
        tokio::time::sleep(FIRST_TICK).await;
        loop {
            match tick(&state, &client, &slug).await {
                Ok((kind, count)) => tracing::info!(kind, count, "demo feed raised events"),
                Err(error) => tracing::warn!(%error, "demo feed tick failed"),
            }
            let minutes = MIN_MINUTES + entropy() % (MAX_MINUTES - MIN_MINUTES + 1);
            tokio::time::sleep(Duration::from_secs(minutes * 60)).await;
        }
    });
}

async fn tick(
    state: &AppState,
    client: &reqwest::Client,
    slug: &str,
) -> Result<(&'static str, usize), AppError> {
    let thermite = &state.thermite;
    let projects = thermite_core::api::projects::all(&thermite.db, &thermite.config)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let project = projects
        .into_iter()
        .find(|p| p.slug == slug)
        .ok_or_else(|| AppError::Internal(format!("demo project {slug:?} does not exist")))?;
    let public_key = thermite_core::auth::sentry_key_from_dsn(&project.dsn)
        .ok_or_else(|| AppError::Internal("project DSN has no public key".to_string()))?;

    // Keep reporting the release the project last saw, so the feed never reads as a new deploy
    // — a resolved-until-next-release issue must not reopen just because the feeder ran.
    let release = thermite_core::api::releases::latest_version(&thermite.db, project.id)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .unwrap_or_else(|| "demo@1.0.0".to_string());

    let seed = entropy();
    let (kind, count) = pick(seed);
    for i in 0..count {
        let event = demo_events::payload(kind, "production", &release, (seed as usize) + i)
            .ok_or_else(|| AppError::Internal(format!("unknown demo kind {kind}")))?;
        demo_events::send(
            client,
            &thermite.config.base_url,
            project.id,
            &public_key,
            &event,
        )
        .await?;
    }
    Ok((kind, count))
}

#[cfg(test)]
mod tests {
    use super::pick;
    use crate::models::demo::DEMO_KINDS;

    #[test]
    fn every_pick_is_a_kind_the_playground_knows() {
        for entropy in 0..5_000u64 {
            let (kind, count) = pick(entropy);
            assert!(DEMO_KINDS.iter().any(|k| k.id == kind), "{kind}");
            assert!((1..=3).contains(&count));
        }
    }

    #[test]
    fn grouping_kinds_dominate() {
        let mut timeouts = 0;
        for entropy in 0..1_200u64 {
            if pick(entropy).0 == "db_timeout" {
                timeouts += 1;
            }
        }
        assert!(timeouts >= 300, "{timeouts}");
    }
}
