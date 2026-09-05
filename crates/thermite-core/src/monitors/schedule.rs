//! When a monitored job is next expected.
//!
//! Two schedule kinds, matching Sentry's `monitor_config`:
//!
//! - **crontab** — a five-field expression, evaluated in the monitor's timezone. Cron is
//!   wall-clock: `0 3 * * *` means 03:00 where the job runs, which is a different instant across
//!   a DST boundary than 03:00 UTC.
//! - **interval** — every N minutes/hours/days/weeks/months, measured from the last check-in.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use std::str::FromStr;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScheduleError {
    #[error("unsupported schedule type {0:?}")]
    UnknownType(String),
    #[error("invalid crontab expression {value:?}: {reason}")]
    InvalidCrontab { value: String, reason: String },
    #[error("invalid interval {value:?} {unit:?}")]
    InvalidInterval { value: String, unit: String },
}

/// A monitor's schedule, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    Crontab(String),
    Interval { every: i64, unit: IntervalUnit },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalUnit {
    Minute,
    Hour,
    Day,
    Week,
    Month,
}

impl IntervalUnit {
    fn parse(raw: &str) -> Option<Self> {
        // SDKs send singular units; tolerate plurals rather than failing a monitor over an "s".
        match raw.trim().to_ascii_lowercase().trim_end_matches('s') {
            "minute" => Some(Self::Minute),
            "hour" => Some(Self::Hour),
            "day" => Some(Self::Day),
            "week" => Some(Self::Week),
            "month" => Some(Self::Month),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    /// Months are approximated at 30 days: this drives "when should I have heard from you",
    /// not billing, and a monitor firing a day early on a monthly job is not worth a calendar.
    fn duration(self, every: i64) -> Duration {
        match self {
            Self::Minute => Duration::minutes(every),
            Self::Hour => Duration::hours(every),
            Self::Day => Duration::days(every),
            Self::Week => Duration::weeks(every),
            Self::Month => Duration::days(30 * every),
        }
    }
}

impl Schedule {
    /// Parses a schedule as stored on the monitor row.
    pub fn parse(
        schedule_type: &str,
        value: &str,
        unit: Option<&str>,
    ) -> Result<Self, ScheduleError> {
        match schedule_type {
            "crontab" => {
                // Validate now rather than at sweep time: a monitor with an unparseable schedule
                // would otherwise look healthy forever, which is the one outcome cron monitoring
                // exists to prevent.
                parse_cron(value)?;
                Ok(Self::Crontab(value.trim().to_string()))
            }
            "interval" => {
                let every: i64 =
                    value
                        .trim()
                        .parse()
                        .map_err(|_| ScheduleError::InvalidInterval {
                            value: value.to_string(),
                            unit: unit.unwrap_or_default().to_string(),
                        })?;
                let parsed_unit = unit.and_then(IntervalUnit::parse);

                match (every > 0, parsed_unit) {
                    (true, Some(unit)) => Ok(Self::Interval { every, unit }),
                    _ => Err(ScheduleError::InvalidInterval {
                        value: value.to_string(),
                        unit: unit.unwrap_or_default().to_string(),
                    }),
                }
            }
            other => Err(ScheduleError::UnknownType(other.to_string())),
        }
    }

    /// The next time a run is expected after `after`.
    ///
    /// `timezone` applies to crontab schedules only; an unknown name falls back to UTC rather
    /// than failing, so a typo degrades to a working monitor with a shifted window instead of a
    /// silent one.
    pub fn next_after(&self, after: DateTime<Utc>, timezone: &str) -> Option<DateTime<Utc>> {
        match self {
            Self::Crontab(expression) => {
                let tz: Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
                let local = after.with_timezone(&tz);
                parse_cron(expression)
                    .ok()?
                    .after(&local)
                    .next()
                    .map(|next| next.with_timezone(&Utc))
            }
            Self::Interval { every, unit } => Some(after + unit.duration(*every)),
        }
    }
}

/// The `cron` crate expects seconds as a sixth leading field; Sentry (and crontab) use five.
fn parse_cron(expression: &str) -> Result<cron::Schedule, ScheduleError> {
    let expression = expression.trim();
    let field_count = expression.split_whitespace().count();

    let with_seconds = match field_count {
        5 => format!("0 {expression}"),
        6 => expression.to_string(),
        _ => {
            return Err(ScheduleError::InvalidCrontab {
                value: expression.to_string(),
                reason: format!("expected 5 fields, got {field_count}"),
            });
        }
    };

    cron::Schedule::from_str(&with_seconds).map_err(|e| ScheduleError::InvalidCrontab {
        value: expression.to_string(),
        reason: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn crontab_next_run_is_the_next_matching_minute() {
        let schedule = Schedule::parse("crontab", "0 * * * *", None).unwrap();
        assert_eq!(
            schedule.next_after(at("2026-08-04T10:15:00Z"), "UTC"),
            Some(at("2026-08-04T11:00:00Z"))
        );
    }

    #[test]
    fn crontab_is_evaluated_in_the_monitors_timezone() {
        // 03:00 in Berlin is 01:00 UTC in summer — a monitor keyed on UTC would call this job
        // missed two hours before it was ever due.
        let schedule = Schedule::parse("crontab", "0 3 * * *", None).unwrap();
        assert_eq!(
            schedule.next_after(at("2026-08-04T00:00:00Z"), "Europe/Berlin"),
            Some(at("2026-08-04T01:00:00Z"))
        );
        assert_eq!(
            schedule.next_after(at("2026-08-04T00:00:00Z"), "UTC"),
            Some(at("2026-08-04T03:00:00Z"))
        );
    }

    #[test]
    fn an_unknown_timezone_falls_back_to_utc_rather_than_failing() {
        let schedule = Schedule::parse("crontab", "0 3 * * *", None).unwrap();
        assert_eq!(
            schedule.next_after(at("2026-08-04T00:00:00Z"), "Mars/Olympus"),
            Some(at("2026-08-04T03:00:00Z"))
        );
    }

    #[test]
    fn interval_counts_from_the_last_check_in() {
        let schedule = Schedule::parse("interval", "15", Some("minute")).unwrap();
        assert_eq!(
            schedule.next_after(at("2026-08-04T10:00:00Z"), "UTC"),
            Some(at("2026-08-04T10:15:00Z"))
        );

        let daily = Schedule::parse("interval", "2", Some("days")).unwrap();
        assert_eq!(
            daily.next_after(at("2026-08-04T10:00:00Z"), "UTC"),
            Some(at("2026-08-06T10:00:00Z"))
        );
    }

    #[test]
    fn a_broken_schedule_is_rejected_at_parse_time() {
        // Not at sweep time: a monitor whose schedule never yields a next run would look healthy
        // forever, which is exactly the failure cron monitoring is supposed to catch.
        assert!(Schedule::parse("crontab", "not a cron", None).is_err());
        assert!(Schedule::parse("crontab", "0 *", None).is_err());
        assert!(Schedule::parse("interval", "0", Some("minute")).is_err());
        assert!(Schedule::parse("interval", "5", Some("fortnight")).is_err());
        assert!(Schedule::parse("interval", "5", None).is_err());
        assert!(Schedule::parse("sundial", "always", None).is_err());
    }

    #[test]
    fn six_field_expressions_keep_their_seconds() {
        let schedule = Schedule::parse("crontab", "30 0 * * * *", None).unwrap();
        let next = schedule
            .next_after(Utc.with_ymd_and_hms(2026, 8, 4, 10, 0, 0).unwrap(), "UTC")
            .unwrap();
        assert_eq!(next, at("2026-08-04T10:00:30Z"));
    }
}
