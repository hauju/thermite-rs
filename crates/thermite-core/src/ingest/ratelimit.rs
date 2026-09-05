use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-project ingest quota, enforced as a fixed one-minute window.
///
/// In-process on purpose: a shared store (Valkey/Redis) would add a network round trip per event
/// to enforce a limit that is approximate by nature. This becomes wrong only when ingest runs on
/// several nodes, at which point each node enforces its own share of the quota.
pub struct RateLimiter {
    per_minute: Option<u32>,
    windows: Mutex<HashMap<i64, Window>>,
}

#[derive(Debug, Clone, Copy)]
struct Window {
    minute: u64,
    count: u32,
}

impl RateLimiter {
    pub fn new(per_minute: Option<u32>) -> Self {
        Self {
            per_minute,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Locks `windows`, recovering from a poisoned mutex rather than propagating the panic.
    ///
    /// The guarded state is a map of per-minute counters, so a partially applied update is at
    /// worst one miscounted event. Panicking instead would take every subsequent ingest request
    /// down with it for the life of the process.
    fn lock_windows(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Window>> {
        self.windows.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Records one event against `project_id`'s quota.
    ///
    /// Returns `Err(retry_after_seconds)` when the project is over quota, where the value is the
    /// time until the current window rolls over.
    ///
    /// Called once per *event*, not per request: an envelope may carry many events, and charging
    /// per envelope let one request cost the quota of one while storing hundreds.
    pub fn check(&self, project_id: i64) -> Result<(), u64> {
        self.check_at(project_id, now_secs())
    }

    /// Whether the project is already at its limit, charging nothing.
    ///
    /// Lets the caller reject an over-quota request *before* doing work proportional to the body,
    /// while leaving the actual accounting to the per-event [`check`](Self::check).
    pub fn exhausted(&self, project_id: i64) -> Option<u64> {
        self.exhausted_at(project_id, now_secs())
    }

    fn exhausted_at(&self, project_id: i64, now: u64) -> Option<u64> {
        let limit = self.per_minute?;
        let minute = now / 60;
        let windows = self.lock_windows();

        match windows.get(&project_id) {
            // A window from an earlier minute has already rolled over.
            Some(window) if window.minute == minute && window.count >= limit => {
                Some(60 - (now % 60).min(59))
            }
            _ => None,
        }
    }

    fn check_at(&self, project_id: i64, now: u64) -> Result<(), u64> {
        let Some(limit) = self.per_minute else {
            return Ok(());
        };

        let minute = now / 60;
        let mut windows = self.lock_windows();
        let window = windows
            .entry(project_id)
            .or_insert(Window { minute, count: 0 });

        if window.minute != minute {
            *window = Window { minute, count: 0 };
        }

        if window.count >= limit {
            // Seconds remaining in this window, and never 0 — a `Retry-After: 0` invites an
            // immediate retry that would just be rejected again.
            return Err(60 - (now % 60).min(59));
        }

        window.count += 1;
        Ok(())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_limiter_always_allows() {
        let limiter = RateLimiter::new(None);
        for _ in 0..1000 {
            assert!(limiter.check(1).is_ok());
        }
    }

    #[test]
    fn allows_up_to_the_limit_then_rejects() {
        let limiter = RateLimiter::new(Some(2));

        assert!(limiter.check_at(1, 100).is_ok());
        assert!(limiter.check_at(1, 100).is_ok());
        assert!(limiter.check_at(1, 100).is_err());
    }

    #[test]
    fn quota_is_per_project() {
        let limiter = RateLimiter::new(Some(1));

        assert!(limiter.check_at(1, 100).is_ok());
        assert!(limiter.check_at(1, 100).is_err());
        assert!(limiter.check_at(2, 100).is_ok());
    }

    #[test]
    fn window_rolls_over_into_the_next_minute() {
        let limiter = RateLimiter::new(Some(1));

        assert!(limiter.check_at(1, 100).is_ok());
        assert!(limiter.check_at(1, 100).is_err());
        assert!(limiter.check_at(1, 160).is_ok());
    }

    #[test]
    fn retry_after_is_time_left_in_the_window_and_never_zero() {
        let limiter = RateLimiter::new(Some(0));

        // 10s into the minute, so 50s remain.
        assert_eq!(limiter.check_at(1, 70), Err(50));
        // Last second of the minute still asks for a 1s wait rather than 0.
        assert_eq!(limiter.check_at(1, 119), Err(1));
    }

    #[test]
    fn exhausted_reports_the_limit_without_charging_for_the_check() {
        let limiter = RateLimiter::new(Some(2));

        // Nothing recorded yet.
        assert_eq!(limiter.exhausted_at(1, 100), None);
        // Asking must not itself consume quota, or the pre-flight check would spend the budget
        // it exists to protect.
        assert!(limiter.check_at(1, 100).is_ok());
        assert_eq!(limiter.exhausted_at(1, 100), None);
        assert!(limiter.check_at(1, 100).is_ok());

        // 100s is 40s into the minute, so 20s remain — the same backoff `check` would report.
        assert_eq!(limiter.exhausted_at(1, 100), Some(20));
        assert_eq!(limiter.check_at(1, 100), Err(20));
    }

    #[test]
    fn a_stale_window_is_not_reported_as_exhausted() {
        let limiter = RateLimiter::new(Some(1));
        assert!(limiter.check_at(1, 100).is_ok());
        assert_eq!(limiter.exhausted_at(1, 100), Some(20));

        // The next minute starts clean, so a pre-flight check must not reject on last minute's
        // count.
        assert_eq!(limiter.exhausted_at(1, 160), None);
    }
}
