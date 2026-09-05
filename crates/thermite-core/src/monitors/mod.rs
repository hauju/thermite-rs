//! Cron monitoring: knowing that a scheduled job *didn't* run.
//!
//! Jobs report through Sentry's check-in protocol — `check_in` envelope items on the same ingest
//! endpoint as errors, with the same DSN credential ([`crate::ingest`] routes them here). A
//! monitor is created on first sighting and carries its own schedule, so nothing has to be
//! configured twice.
//!
//! The load-bearing design decision: **a missed or overrunning job becomes an ordinary error
//! event**, digested through the same path as any exception. Grouping, the notifications outbox,
//! alert delivery, triage and retention then apply unchanged. Cron monitoring is a new *source*
//! of events, not a second pipeline running beside them.

pub mod schedule;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppResult;
use crate::ingest::digest;
use schedule::Schedule;

/// How many overdue monitors one sweep handles. A sweep that found thousands would be a
/// misconfiguration, and the next tick picks up the remainder either way.
const SWEEP_BATCH: i64 = 500;

/// One check-in as an SDK sends it.
#[derive(Debug, Deserialize)]
pub struct CheckIn {
    pub check_in_id: Option<Uuid>,
    pub monitor_slug: String,
    /// `in_progress` opens a run; `ok` / `error` close it.
    pub status: String,
    /// Seconds, sent by SDKs that time the run themselves.
    pub duration: Option<f64>,
    pub environment: Option<String>,
    pub release: Option<String>,
    #[serde(default)]
    pub monitor_config: Option<MonitorConfig>,
}

#[derive(Debug, Deserialize)]
pub struct MonitorConfig {
    pub schedule: Option<ScheduleConfig>,
    pub checkin_margin: Option<i32>,
    pub max_runtime: Option<i32>,
    pub timezone: Option<String>,
}

/// `{"type": "crontab", "value": "0 * * * *"}` or
/// `{"type": "interval", "value": 5, "unit": "minute"}`.
#[derive(Debug, Deserialize)]
pub struct ScheduleConfig {
    pub r#type: Option<String>,
    pub value: Option<Value>,
    pub unit: Option<String>,
}

/// A monitor as the read API serves it.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Monitor {
    pub id: i64,
    pub slug: String,
    pub schedule_type: String,
    pub schedule_value: String,
    pub schedule_unit: Option<String>,
    pub timezone: String,
    pub checkin_margin_minutes: i32,
    pub max_runtime_minutes: i32,
    pub status: Option<String>,
    pub last_checkin_at: Option<DateTime<Utc>>,
    pub next_due_at: Option<DateTime<Utc>>,
}

/// Records a check-in, creating or updating the monitor it names.
///
/// Returns `None` when the check-in carries no usable schedule and names no existing monitor:
/// without a schedule there is nothing to be late *for*, so inventing a monitor row would just
/// accumulate junk that can never alert.
pub async fn record_check_in(
    db: &PgPool,
    project_id: i64,
    check_in: &CheckIn,
) -> AppResult<Option<i64>> {
    let slug = check_in.monitor_slug.trim();
    if slug.is_empty() {
        return Ok(None);
    }

    let Some(monitor_id) =
        upsert_monitor(db, project_id, slug, check_in.monitor_config.as_ref()).await?
    else {
        return Ok(None);
    };

    let check_in_id = check_in.check_in_id.unwrap_or_else(Uuid::new_v4);
    let status = match check_in.status.trim().to_ascii_lowercase().as_str() {
        "in_progress" => "in_progress",
        "error" => "error",
        // `ok` and anything unrecognised: a job that bothered to report is treated as having run.
        _ => "ok",
    };

    // An SDK opens a run and closes it under the same check_in_id, so the close is an update.
    sqlx::query(
        "insert into monitor_checkins
             (monitor_id, check_in_id, status, duration_seconds, environment, release)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (monitor_id, check_in_id) do update
             set status           = excluded.status,
                 duration_seconds = coalesce(excluded.duration_seconds,
                                             monitor_checkins.duration_seconds),
                 environment      = coalesce(excluded.environment, monitor_checkins.environment),
                 release          = coalesce(excluded.release, monitor_checkins.release),
                 updated_at       = now()",
    )
    .bind(monitor_id)
    .bind(check_in_id)
    .bind(status)
    .bind(check_in.duration)
    .bind(check_in.environment.as_deref())
    .bind(check_in.release.as_deref())
    .execute(db)
    .await?;

    // An in-progress check-in says the job started, not that it finished: the due time must not
    // move yet, or a job that hangs forever would keep looking punctual.
    if status != "in_progress" {
        complete_run(db, monitor_id, status).await?;
    }

    Ok(Some(monitor_id))
}

/// Advances a monitor past a finished run: its outcome, and when the next one is expected.
async fn complete_run(db: &PgPool, monitor_id: i64, status: &str) -> AppResult<()> {
    let Some(monitor) = load(db, monitor_id).await? else {
        return Ok(());
    };

    let now = Utc::now();
    let next_due = Schedule::parse(
        &monitor.schedule_type,
        &monitor.schedule_value,
        monitor.schedule_unit.as_deref(),
    )
    .ok()
    .and_then(|schedule| schedule.next_after(now, &monitor.timezone));

    // `reported_at` is cleared here: a job that has recovered must be able to alert again the
    // next time it breaks.
    sqlx::query(
        "update monitors
            set status = $2, last_checkin_at = $3, next_due_at = $4, reported_at = null
          where id = $1",
    )
    .bind(monitor_id)
    .bind(status)
    .bind(now)
    .bind(next_due)
    .execute(db)
    .await?;

    Ok(())
}

/// Creates the monitor or updates its configuration, returning its id.
///
/// A config with an unparseable schedule is ignored rather than stored — see [`record_check_in`]
/// for why a monitor with no valid schedule is not worth creating.
async fn upsert_monitor(
    db: &PgPool,
    project_id: i64,
    slug: &str,
    config: Option<&MonitorConfig>,
) -> AppResult<Option<i64>> {
    let parsed = config.and_then(|config| {
        let schedule = config.schedule.as_ref()?;
        let kind = schedule.r#type.as_deref().unwrap_or("crontab");
        // The value is a string for crontab and a number for interval.
        let value = match schedule.value.as_ref()? {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return None,
        };
        let unit = schedule.unit.as_deref();

        match Schedule::parse(kind, &value, unit) {
            Ok(_) => Some((kind.to_string(), value, unit.map(str::to_string))),
            Err(error) => {
                tracing::warn!(%error, slug, "ignoring check-in with an invalid schedule");
                None
            }
        }
    });

    let Some((schedule_type, schedule_value, schedule_unit)) = parsed else {
        // No usable config: only an already-known monitor can accept this check-in.
        let existing: Option<(i64,)> =
            sqlx::query_as("select id from monitors where project_id = $1 and slug = $2")
                .bind(project_id)
                .bind(slug)
                .fetch_optional(db)
                .await?;
        return Ok(existing.map(|(id,)| id));
    };

    let config = config.expect("a parsed schedule implies a config");
    let (id,): (i64,) = sqlx::query_as(
        "insert into monitors (
             project_id, slug, schedule_type, schedule_value, schedule_unit,
             timezone, checkin_margin_minutes, max_runtime_minutes
         )
         values ($1, $2, $3, $4, $5, coalesce($6, 'UTC'), coalesce($7, 5), coalesce($8, 60))
         on conflict (project_id, slug) do update
             set schedule_type          = excluded.schedule_type,
                 schedule_value         = excluded.schedule_value,
                 schedule_unit          = excluded.schedule_unit,
                 timezone               = excluded.timezone,
                 checkin_margin_minutes = excluded.checkin_margin_minutes,
                 max_runtime_minutes    = excluded.max_runtime_minutes
         returning id",
    )
    .bind(project_id)
    .bind(slug)
    .bind(&schedule_type)
    .bind(&schedule_value)
    .bind(&schedule_unit)
    .bind(config.timezone.as_deref())
    .bind(config.checkin_margin)
    .bind(config.max_runtime)
    .fetch_one(db)
    .await?;

    Ok(Some(id))
}

async fn load(db: &PgPool, monitor_id: i64) -> AppResult<Option<Monitor>> {
    let monitor: Option<Monitor> = sqlx::query_as(
        "select id, slug, schedule_type, schedule_value, schedule_unit, timezone,
                checkin_margin_minutes, max_runtime_minutes, status, last_checkin_at, next_due_at
           from monitors where id = $1",
    )
    .bind(monitor_id)
    .fetch_optional(db)
    .await?;

    Ok(monitor)
}

/// Every monitor of a project, newest activity first.
pub async fn list(db: &PgPool, project_id: i64) -> AppResult<Vec<Monitor>> {
    let monitors: Vec<Monitor> = sqlx::query_as(
        "select id, slug, schedule_type, schedule_value, schedule_unit, timezone,
                checkin_margin_minutes, max_runtime_minutes, status, last_checkin_at, next_due_at
           from monitors
          where project_id = $1
          order by last_checkin_at desc nulls last, slug",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;

    Ok(monitors)
}

/// What a sweep did, for the log line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub missed: u64,
    pub timed_out: u64,
}

impl Swept {
    pub fn total(&self) -> u64 {
        self.missed + self.timed_out
    }
}

#[derive(sqlx::FromRow)]
struct OverdueRow {
    id: i64,
    project_id: i64,
    slug: String,
    schedule_type: String,
    schedule_value: String,
    schedule_unit: Option<String>,
    timezone: String,
    max_runtime_minutes: i32,
    next_due_at: DateTime<Utc>,
    /// The run that is still open, if any — which distinguishes "never started" from "started
    /// and never finished".
    open_since: Option<DateTime<Utc>>,
}

/// One pass: raise an event for every monitor whose run is overdue or overrunning.
///
/// Claiming is by `reported_at`, set in the same statement that selects the row, so concurrent
/// replicas cannot both report the same miss.
pub async fn sweep(db: &PgPool) -> AppResult<Swept> {
    let mut swept = Swept::default();

    let overdue: Vec<OverdueRow> = sqlx::query_as(
        "with due as (
             update monitors m
                set reported_at = now()
              where m.id in (
                    select id from monitors
                     where next_due_at is not null
                       and reported_at is null
                       and next_due_at + make_interval(mins => checkin_margin_minutes) < now()
                     order by next_due_at
                     limit $1
                     for update skip locked
              )
              returning *
         )
         select d.id, d.project_id, d.slug, d.schedule_type, d.schedule_value, d.schedule_unit,
                d.timezone, d.max_runtime_minutes, d.next_due_at,
                (select c.created_at from monitor_checkins c
                  where c.monitor_id = d.id and c.status = 'in_progress'
                  order by c.created_at desc limit 1) as open_since
           from due d",
    )
    .bind(SWEEP_BATCH)
    .fetch_all(db)
    .await?;

    for monitor in overdue {
        // A run that started and never reported back is a timeout; one that never started at all
        // is a miss. Both are failures, but they point at different bugs, so they group apart.
        let timed_out = monitor.open_since.is_some_and(|started| {
            Utc::now() - started > Duration::minutes(monitor.max_runtime_minutes.max(1) as i64)
        });
        let status = if timed_out { "timeout" } else { "missed" };

        let payload = failure_event(&monitor, status);
        digest::digest(db, monitor.project_id, None, &payload, Utc::now()).await?;

        // Move the window forward so the *next* missed run is reported too, rather than the
        // monitor going quiet after one alert.
        let next_due = Schedule::parse(
            &monitor.schedule_type,
            &monitor.schedule_value,
            monitor.schedule_unit.as_deref(),
        )
        .ok()
        .and_then(|schedule| schedule.next_after(Utc::now(), &monitor.timezone));

        sqlx::query("update monitors set status = $2, next_due_at = $3 where id = $1")
            .bind(monitor.id)
            .bind(status)
            .bind(next_due)
            .execute(db)
            .await?;

        if timed_out {
            swept.timed_out += 1;
        } else {
            swept.missed += 1;
        }
    }

    Ok(swept)
}

/// The synthetic event a missed or timed-out run produces.
///
/// An explicit fingerprint keyed on the monitor and the failure kind: every miss of one job is
/// one issue (so a job broken for a week is one thing to fix, not seven), and a timeout stays
/// distinct from a miss.
fn failure_event(monitor: &OverdueRow, status: &str) -> Value {
    let (kind, message) = match status {
        "timeout" => (
            "MonitorTimeout",
            format!(
                "Cron monitor {:?} started but did not finish within {} minutes",
                monitor.slug, monitor.max_runtime_minutes
            ),
        ),
        _ => (
            "MonitorMissed",
            format!(
                "Cron monitor {:?} did not check in; expected at {}",
                monitor.slug,
                monitor.next_due_at.to_rfc3339()
            ),
        ),
    };

    json!({
        "event_id": Uuid::new_v4().simple().to_string(),
        "timestamp": Utc::now().to_rfc3339(),
        "platform": "other",
        "level": "error",
        "logger": "thermite.monitors",
        "fingerprint": [format!("monitor:{}:{}", monitor.slug, status)],
        "exception": { "values": [{ "type": kind, "value": message }] },
        "tags": { "monitor": monitor.slug.clone(), "monitor_status": status },
    })
}

/// Sweeps on an interval, forever. Failures are logged and retried on the next tick: cron
/// monitoring falling behind must not take the process down.
pub fn spawn(db: PgPool, every: std::time::Duration) {
    tokio::spawn(async move {
        loop {
            match sweep(&db).await {
                Ok(swept) if swept.total() > 0 => tracing::warn!(
                    missed = swept.missed,
                    timed_out = swept.timed_out,
                    "cron monitors failed to check in"
                ),
                Ok(_) => {}
                Err(error) => tracing::error!(%error, "monitor sweep failed"),
            }

            tokio::time::sleep(every).await;
        }
    });
}
