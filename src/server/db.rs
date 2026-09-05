use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::models::AppError;

/// The application's PostgreSQL pools.
///
/// Three pools on purpose, not one: ingest is synchronous and holds a connection per event for a
/// six-statement transaction, so an error storm — the product's central scenario — saturates
/// whatever pool it runs on. On a shared pool that meant sessions, the dashboard and the health
/// probe all blocked behind the storm, `/health` flipped to 503, and the orchestrator restarted
/// the container mid-storm. Separate pools turn that into "ingest backpressures the SDKs while
/// the dashboard stays up", which is the design intent.
#[derive(Clone)]
pub struct Database {
    /// The interactive path: dashboard, sessions, read API, MCP, OAuth.
    pub pool: PgPool,
    /// Synchronous ingest only.
    pub ingest_pool: PgPool,
    /// The periodic loops (retention, rate-limit sweeper, alerts), so a long batched delete
    /// cannot occupy an interactive or ingest connection.
    pub background_pool: PgPool,
}

/// How long a request waits for a connection before failing.
///
/// The interactive path fails fast: a saturated pool should degrade to a prompt error on the
/// affected request, not a 30s hang across every surface (sqlx's default). Ingest waits longer —
/// backpressure is its contract, and the SDKs buffer and retry.
const APP_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const INGEST_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
const BACKGROUND_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);
const BACKGROUND_CONNECTIONS: u32 = 2;

impl Database {
    pub async fn new(
        db_url: &str,
        max_connections: u32,
        ingest_max_connections: u32,
    ) -> Result<Self, AppError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(APP_ACQUIRE_TIMEOUT)
            .connect(db_url)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to connect to PostgreSQL: {e}")))?;

        // Ping to verify connection.
        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to ping PostgreSQL: {e}")))?;

        tracing::info!("Connected to database successfully");

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to run migrations: {e}")))?;

        tracing::info!("Database migrations applied");

        // The other pools connect lazily; migrations have already run on the first.
        let ingest_pool = PgPoolOptions::new()
            .max_connections(ingest_max_connections)
            .acquire_timeout(INGEST_ACQUIRE_TIMEOUT)
            .connect_lazy(db_url)
            .map_err(|e| AppError::Internal(format!("Invalid database URL: {e}")))?;

        let background_pool = PgPoolOptions::new()
            .max_connections(BACKGROUND_CONNECTIONS)
            .acquire_timeout(BACKGROUND_ACQUIRE_TIMEOUT)
            .connect_lazy(db_url)
            .map_err(|e| AppError::Internal(format!("Invalid database URL: {e}")))?;

        Ok(Self {
            pool,
            ingest_pool,
            background_pool,
        })
    }

    /// Wrap an already-connected pool.
    ///
    /// Used by tests, where `#[sqlx::test]` hands each case its own freshly
    /// migrated database and there is no URL to connect to. All three roles
    /// share the one pool — the separation is a production property, not
    /// something the behaviour under test depends on.
    #[cfg(test)]
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            ingest_pool: pool.clone(),
            background_pool: pool.clone(),
            pool,
        }
    }
}
