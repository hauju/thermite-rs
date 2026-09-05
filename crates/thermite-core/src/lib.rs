//! Sentry-compatible error ingest, grouping, and the query layer above it.
//!
//! This crate owns everything that is true regardless of how Thermite is served: the wire protocol,
//! grouping, the digest transaction, and the queries the dashboard and the MCP tools both read. It
//! deliberately knows nothing about sessions, OAuth or rendering — the application crate mounts
//! these routers and wraps them in whatever authentication it uses.

pub mod alerts;
pub mod api;
pub mod auth;
pub mod config;
pub mod error;
pub mod ingest;
pub mod monitors;
pub mod protocol;
pub mod retention;
pub mod state;

pub use config::Config;
pub use error::{AppError, AppResult};
pub use retention::Policy as RetentionPolicy;
pub use state::ThermiteState;

use axum::Router;

/// The SDK-facing ingest endpoints.
///
/// Deliberately separate from [`api_routes`]: these authenticate with a DSN public key and must
/// stay reachable by anything on the internet, while the read API is for operators and agents.
pub fn ingest_routes(state: ThermiteState) -> Router {
    ingest::routes(state.clone()).with_state(state)
}

/// The read and triage API, **unauthenticated**.
///
/// The caller is expected to layer its own credential check over this. Thermite has no opinion on
/// what that is — the application crate owns API keys, sessions and OAuth, and inverting the
/// dependency the other way would drag all of that in here.
pub fn api_routes(state: ThermiteState) -> Router {
    api::routes().with_state(state)
}
