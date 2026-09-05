use std::sync::Arc;

use sqlx::PgPool;

use crate::config::Config;
use crate::ingest::ratelimit::RateLimiter;

#[derive(Clone)]
pub struct ThermiteState(Arc<Inner>);

pub struct Inner {
    pub db: PgPool,
    pub config: Config,
    pub rate_limiter: RateLimiter,
}

impl ThermiteState {
    pub fn new(db: PgPool, config: Config) -> Self {
        let rate_limiter = RateLimiter::new(config.rate_limit_per_minute);
        Self(Arc::new(Inner {
            db,
            config,
            rate_limiter,
        }))
    }
}

impl std::ops::Deref for ThermiteState {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
