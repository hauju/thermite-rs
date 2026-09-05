//! Release health: sessions, the denominator the error count needs.
//!
//! An error count rises with traffic, so on its own it cannot tell a broken release from a busy
//! one. A session is one run of the process; the crash-free rate is what those two numbers make
//! together, and it is the reason to send these at all.
//!
//! Thermite folds sessions into counters rather than storing rows, and it counts only the updates
//! an SDK sends **exactly once per session** — the `init` that opens one, and the terminal status
//! that closes it. So this deliberately sends exactly two updates per session and no heartbeats in
//! between: a running update changes no counter, and one sent twice would inflate the totals.
//!
//! There is no `abnormal` status here. It means a session that ended without the SDK getting to
//! say so, which is by definition not something the SDK can report; Sentry infers it on the next
//! run from a session it left open on disk, and this crate persists nothing.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Still running. Contributes nothing on its own — the `init` that opened the session already
    /// counted it.
    Ok,
    Exited,
    Crashed,
}

/// Attributes thermite keys the rollup on.
#[derive(Debug, Clone, Serialize)]
pub struct Attrs {
    /// Not optional. A session naming no release is dropped on arrival, because the rollup is
    /// keyed on the release row — whose per-project cap is the only thing bounding that table's
    /// cardinality.
    pub release: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}

/// One session update, as it goes on the wire.
#[derive(Debug, Clone, Serialize)]
pub struct Update {
    #[serde(serialize_with = "crate::event::unhyphenated")]
    pub sid: Uuid,
    /// True on exactly one update per session. It is what thermite counts as a start.
    pub init: bool,
    /// When the session began — *not* when this update was produced.
    ///
    /// Thermite buckets a session's counters on this, so a session that starts at 10:59 and
    /// crashes at 11:01 files its total and its crash in the same hour. A bucket that received the
    /// crash without the total would report a crash rate above 100%.
    pub started: DateTime<Utc>,
    /// When this update was produced.
    pub timestamp: DateTime<Utc>,
    pub status: Status,
    pub errors: u32,
    pub attrs: Attrs,
}

/// One run of the process.
#[derive(Debug)]
pub struct Session {
    sid: Uuid,
    started: DateTime<Utc>,
    status: Status,
    errors: u32,
    attrs: Attrs,
}

impl Session {
    pub fn new(release: String, environment: Option<String>) -> Self {
        Self {
            sid: Uuid::new_v4(),
            started: Utc::now(),
            status: Status::Ok,
            errors: 0,
            attrs: Attrs {
                release,
                environment,
            },
        }
    }

    /// The update that opens the session, and the only one carrying `init`.
    pub fn opened(&self) -> Update {
        self.update(true)
    }

    /// Notes that an error was reported during this session.
    ///
    /// Counted locally and sent once at the end, because thermite reads `errored` off the terminal
    /// update. Sending a running update per error would be traffic that moves no counter.
    pub fn record_error(&mut self) {
        self.errors = self.errors.saturating_add(1);
    }

    /// Marks the session as having ended in a crash rather than an ordinary exit.
    pub fn mark_crashed(&mut self) {
        self.status = Status::Crashed;
    }

    /// The update that closes the session — the one thermite reads the outcome from.
    pub fn closed(&self) -> Update {
        let mut update = self.update(false);
        update.status = match self.status {
            Status::Crashed => Status::Crashed,
            Status::Ok | Status::Exited => Status::Exited,
        };
        update
    }

    fn update(&self, init: bool) -> Update {
        Update {
            sid: self.sid,
            init,
            started: self.started,
            timestamp: Utc::now(),
            status: self.status,
            errors: self.errors,
            attrs: self.attrs.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new("1.4.2".to_string(), Some("production".to_string()))
    }

    #[test]
    fn the_opening_update_is_the_one_that_counts_a_session() {
        let update = session().opened();

        assert!(update.init);
        assert_eq!(update.status, Status::Ok);
        assert_eq!(update.errors, 0);
    }

    /// `init` on the terminal update too would count the session twice.
    #[test]
    fn the_closing_update_does_not_repeat_init() {
        let session = session();

        assert!(!session.closed().init);
    }

    #[test]
    fn a_clean_run_exits_with_no_errors() {
        let update = session().closed();

        assert_eq!(update.status, Status::Exited);
        assert_eq!(update.errors, 0);
    }

    /// `exited` with a non-zero error count is exactly what thermite counts as errored.
    #[test]
    fn a_run_that_reported_an_error_exits_with_the_count() {
        let mut session = session();
        session.record_error();
        session.record_error();

        let update = session.closed();
        assert_eq!(update.status, Status::Exited);
        assert_eq!(update.errors, 2);
    }

    /// A crash is not also an errored session: thermite counts `errored` only for `exited`, so the
    /// terminal status has to stay `crashed` even though an error was reported first.
    #[test]
    fn a_crash_outranks_the_errors_that_preceded_it() {
        let mut session = session();
        session.record_error();
        session.mark_crashed();

        assert_eq!(session.closed().status, Status::Crashed);
    }

    /// Both updates carry the same `started`, which is what puts the total and the outcome in one
    /// bucket even when the session spans the hour boundary between them.
    #[test]
    fn both_updates_share_the_sessions_start_and_id() {
        let session = session();
        let (opened, closed) = (session.opened(), session.closed());

        assert_eq!(opened.started, closed.started);
        assert_eq!(opened.sid, closed.sid);
    }

    #[test]
    fn the_release_rides_on_every_update() {
        let update = session().opened();

        assert_eq!(update.attrs.release, "1.4.2");
        assert_eq!(update.attrs.environment.as_deref(), Some("production"));
    }
}
