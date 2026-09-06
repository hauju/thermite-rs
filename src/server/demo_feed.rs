//! Keeps the public demo project alive.
//!
//! A demo project (`THERMITE_DEMO_PROJECT`) nobody feeds goes flat within a day: the sparklines
//! empty, the "new" badges expire, the dashboard says nothing happened. This raises one of the
//! playground's synthetic events into it every so often, through the real ingest endpoint exactly
//! as the playground does — so the demo also stays a smoke test of ingest.
//!
//! The project is declared by configuration, so the feed also makes it so: a tick recreates the
//! project if it is missing and seeds it if it is empty. On a sandbox instance every visitor can
//! delete it, and this turns that into a reset rather than an outage.
//!
//! Assumes one replica. A second would double the rate, which is harmless.

use std::time::Duration;

use thermite_core::api::admin::{self, NewProject};

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

/// What a fresh demo project starts with — the burst the playground would raise by hand: every
/// kind, two environments, and two releases so the board has a history to show.
const SEED: &[(&str, &str, &str, usize)] = &[
    ("exception", "production", "1.0.0", 12),
    ("db_timeout", "production", "1.0.0", 8),
    ("log_message", "production", "1.0.0", 5),
    ("warning", "production", "1.0.0", 4),
    ("fingerprint", "production", "1.0.0", 3),
    ("exception", "staging", "1.1.0", 4),
    ("db_timeout", "staging", "1.1.0", 2),
    ("warning", "staging", "1.1.0", 2),
];

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

/// The demo project as the feed needs it.
struct Target {
    id: i64,
    public_key: String,
    /// No issues at all: just created, or its history wiped. Issues outlive retention, so a
    /// project that merely aged out its events is not empty and is not reseeded.
    empty: bool,
}

/// The project the configuration names, created if it does not exist.
async fn ensure_project(state: &AppState, slug: &str) -> Result<Target, AppError> {
    let thermite = &state.thermite;
    let projects = thermite_core::api::projects::all(&thermite.db, &thermite.config)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let (id, dsn, empty) = match projects.into_iter().find(|p| p.slug == slug) {
        Some(p) => (p.id, p.dsn, p.total_issues == 0),
        None => {
            tracing::warn!(project = %slug, "demo project is missing; recreating it");
            let created = admin::create_project(
                &thermite.db,
                &thermite.config,
                &NewProject {
                    slug: slug.to_string(),
                    name: Some("Demo".to_string()),
                },
            )
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
            (created.id, created.dsn, true)
        }
    };
    let public_key = thermite_core::auth::sentry_key_from_dsn(&dsn)
        .ok_or_else(|| AppError::Internal("project DSN has no public key".to_string()))?;
    Ok(Target {
        id,
        public_key,
        empty,
    })
}

async fn tick(
    state: &AppState,
    client: &reqwest::Client,
    slug: &str,
) -> Result<(&'static str, usize), AppError> {
    let thermite = &state.thermite;
    let project = ensure_project(state, slug).await?;

    if project.empty {
        let mut raised = 0;
        for (kind, environment, release, count) in SEED {
            for i in 0..*count {
                let event = demo_events::payload(kind, environment, release, raised + i)
                    .ok_or_else(|| AppError::Internal(format!("unknown demo kind {kind}")))?;
                demo_events::send(
                    client,
                    &thermite.config.base_url,
                    project.id,
                    &project.public_key,
                    &event,
                )
                .await?;
            }
            raised += count;
        }
        return Ok(("seed", raised));
    }

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
            &project.public_key,
            &event,
        )
        .await?;
    }
    Ok((kind, count))
}

#[cfg(test)]
mod tests {
    use super::{SEED, ensure_project, pick};
    use crate::models::demo::DEMO_KINDS;
    use crate::server::db::Database;
    use crate::server::test_support::test_state;
    use sqlx::PgPool;

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

    #[test]
    fn the_seed_is_made_of_kinds_the_playground_knows() {
        for (kind, ..) in SEED {
            assert!(DEMO_KINDS.iter().any(|k| k.id == *kind), "{kind}");
        }
        assert!(SEED.iter().map(|s| s.3).sum::<usize>() >= 25);
    }

    #[sqlx::test]
    async fn a_missing_demo_project_is_created_once_and_found_after(pool: PgPool) {
        let state = test_state(Database::from_pool(pool));
        let first = ensure_project(&state, "demo").await.unwrap();
        assert!(first.empty);

        let second = ensure_project(&state, "demo").await.unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.public_key, first.public_key);
        assert!(second.empty, "no events yet, so still empty");
    }
}
