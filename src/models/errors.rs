//! View models for the error-tracking dashboard.
//!
//! `thermite-core`'s types carry `sqlx::FromRow` and are server-only; these are plain serde structs
//! so the same shapes cross the server-function boundary into the WASM client.
//!
//! The stack frame is modelled explicitly rather than passed through as JSON: rendering source
//! context, in-app highlighting and line numbers from an untyped `Value` makes the markup
//! unreadable. Everything else an SDK sent stays untyped and is displayed generically.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: i64,
    pub slug: String,
    pub name: String,
    /// Configure an SDK with this. Not a secret — it only grants sending events.
    pub dsn: String,
    pub unresolved_issues: i64,
    pub events_last_24h: i64,
    /// Per-project alert routing; None falls back to the instance-wide configuration.
    pub alert_email: Option<String>,
    pub alert_webhook: Option<String>,
    /// Where this project's code lives. An agent triaging its issues gets this with the work.
    pub repo_url: Option<String>,
    /// Additional labeled DSNs, one per component; events through one carry
    /// `component: <label>` as a filterable tag.
    pub keys: Vec<ProjectKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectKey {
    pub label: String,
    pub dsn: String,
}

/// One project on the dashboard overview, with everything that flags it as needing attention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectOverviewRow {
    pub slug: String,
    pub name: String,
    pub unresolved_issues: i64,
    pub events_last_24h: i64,
    /// Issues first seen inside the last 24 hours.
    pub new_issues_24h: i64,
    /// Cron monitors whose last run errored, missed its window, or overran.
    pub monitors_failing: i64,
    /// Alerts given up on after repeated delivery failures.
    pub alerts_dead_lettered: i64,
    /// 24 hourly event counts, oldest first, for the sparkline.
    pub series: Vec<i64>,
}

impl ProjectOverviewRow {
    /// Whether anything on this project warrants a look right now.
    pub fn needs_attention(&self) -> bool {
        self.new_issues_24h > 0 || self.monitors_failing > 0 || self.alerts_dead_lettered > 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueRow {
    pub id: i64,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    pub status: String,
    pub times_seen: i64,
    pub first_seen: String,
    pub last_seen: String,
    /// 24 hourly counts, oldest first, for the row's sparkline.
    pub counts: Vec<i64>,
    /// Distinct users hit. 0 when the SDK sends no user context.
    pub users_affected: i64,
    /// Whether an agent has already looked at this.
    pub has_analysis: bool,
    /// First seen inside the last 24 hours. Computed server-side: the WASM client has no
    /// clock without chrono's `wasmbind`, and the list refetches often enough to stay fresh.
    pub is_new: bool,
    /// `last_seen` as a relative string ("2h ago"), computed server-side for the same reason.
    pub last_seen_ago: String,
}

/// A cron monitor and the outcome of its last run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorRow {
    pub slug: String,
    /// Human-readable schedule, e.g. `0 3 * * *` or `every 15 minutes`.
    pub schedule: String,
    pub timezone: String,
    /// `ok` | `error` | `missed` | `timeout`, or None before the first completed run.
    pub status: Option<String>,
    pub last_checkin_at: Option<String>,
    pub next_due_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectStats {
    pub window: String,
    pub resolution: String,
    pub series: Vec<StatBucket>,
    pub totals: StatTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatBucket {
    pub bucket: String,
    pub count: i64,
    /// Events not stored: dropped by ingest (over quota, unsupported, invalid), or discarded
    /// by the SDK before sending (client reports).
    pub dropped: i64,
}

/// One release and how its sessions ended. Empty until an SDK reports release health.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseHealthRow {
    pub version: String,
    pub sessions: i64,
    pub crashed: i64,
    /// None below the minimum session count, where a rate would be noise. Render "not enough data".
    pub crash_free_rate: Option<f64>,
    /// Session volume per bucket, oldest first.
    pub series: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatTotals {
    pub events: i64,
    pub dropped: i64,
    /// Reason → count, e.g. `over_quota`, `unsupported:transaction`,
    /// `client_discarded:queue_overflow`. Empty when nothing dropped.
    pub dropped_by_reason: std::collections::BTreeMap<String, i64>,
    pub unresolved_issues: i64,
    pub new_issues: i64,
    pub regressions: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueDetail {
    pub id: i64,
    pub project_slug: String,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    pub status: String,
    pub times_seen: i64,
    pub first_seen: String,
    pub last_seen: String,
    pub latest_event: Option<EventDetail>,
    pub analyses: Vec<Analysis>,
    /// Tag value counts across all the issue's events, ordered by key then frequency.
    pub tags: Vec<IssueTag>,
    /// Distinct users hit. 0 when the SDK sends no user context.
    pub users_affected: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueTag {
    pub key: String,
    pub value: String,
    pub times_seen: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventDetail {
    pub event_id: String,
    pub timestamp: String,
    pub level: String,
    pub platform: Option<String>,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub transaction: Option<String>,
    pub server_name: Option<String>,
    pub message: Option<String>,
    pub exception: Vec<ExceptionValue>,
    pub breadcrumbs: Vec<Breadcrumb>,
    /// Flattened `key = value` pairs from tags, contexts, user and request.
    pub context: Vec<ContextGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExceptionValue {
    pub kind: String,
    pub value: Option<String>,
    pub module: Option<String>,
    /// True when the SDK marked this as unhandled — it crashed something.
    pub handled: Option<bool>,
    /// Innermost frame last, as SDKs send them.
    pub frames: Vec<Frame>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub function: Option<String>,
    pub filename: Option<String>,
    pub module: Option<String>,
    pub lineno: Option<i64>,
    pub colno: Option<i64>,
    pub in_app: bool,
    pub context_line: Option<String>,
    pub pre_context: Vec<String>,
    pub post_context: Vec<String>,
}

impl Frame {
    /// True when this frame carries enough to show the offending source line.
    pub fn has_source(&self) -> bool {
        self.context_line.is_some() || !self.pre_context.is_empty() || !self.post_context.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Breadcrumb {
    pub timestamp: Option<String>,
    pub category: Option<String>,
    pub level: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextGroup {
    pub title: String,
    pub entries: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub id: i64,
    pub source: String,
    pub summary: String,
    pub details: Option<String>,
    pub suggested_fix: Option<String>,
    pub confidence: Option<String>,
    pub release: Option<String>,
    /// The pull request the agent opened, when it got that far.
    pub fix_url: Option<String>,
    /// What production made of that fix: `pending`, `held` or `regressed`.
    pub fix_verdict: Option<String>,
    /// For `regressed`, the release since the fix that the issue came back in.
    pub regressed_in: Option<String>,
    pub created_at: String,
}

/// Thousands separators, for counts that are read by magnitude rather than exactly.
///
/// `12483` and `1248` take the same glance to tell apart; `12,483` and `1,248` do not,
/// which matters on a page whose whole job is scanning for the project that spiked.
pub fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// DaisyUI colour for a severity, used for badges and chart bars.
pub fn level_class(level: &str) -> &'static str {
    match level {
        "fatal" => "badge-error",
        "error" => "badge-error",
        "warning" => "badge-warning",
        "info" => "badge-info",
        _ => "badge-ghost",
    }
}

// ============================================================================
// Server-side conversions
// ============================================================================

#[cfg(feature = "server")]
mod convert {
    use serde_json::Value;

    use super::*;

    fn text(value: &Value, key: &str) -> Option<String> {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    fn lines(value: &Value, key: &str) -> Vec<String> {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// How many dots a flattened key may carry before the rest is dropped. Contexts nest
    /// one level (`os.name`); a request body can nest arbitrarily, and dumping all of it
    /// into a key/value list stops being readable long before this depth.
    const MAX_DEPTH: usize = 3;

    /// Renders a JSON scalar for display. Objects and arrays never reach this — `flatten`
    /// expands them — so nothing here can produce a raw JSON blob.
    fn scalar(value: &Value) -> String {
        match value {
            Value::String(s) => s.clone(),
            Value::Null => "null".to_string(),
            other => other.to_string(),
        }
    }

    /// Expands one JSON value into flat `key = value` pairs: nested objects become dotted
    /// keys (`os.name`), arrays of scalars join with `, `, and anything still nested past
    /// [`MAX_DEPTH`] is dropped rather than shown as JSON.
    fn flatten(key: String, value: &Value, depth: usize, out: &mut Vec<(String, String)>) {
        match value {
            Value::Object(map) => {
                if depth < MAX_DEPTH {
                    for (k, v) in map {
                        flatten(format!("{key}.{k}"), v, depth + 1, out);
                    }
                }
            }
            Value::Array(items) if items.iter().any(|v| v.is_object() || v.is_array()) => {
                if depth < MAX_DEPTH {
                    for (i, v) in items.iter().enumerate() {
                        flatten(format!("{key}.{i}"), v, depth + 1, out);
                    }
                }
            }
            Value::Array(items) => {
                if !items.is_empty() {
                    let joined: Vec<String> = items.iter().map(scalar).collect();
                    out.push((key, joined.join(", ")));
                }
            }
            other => out.push((key, scalar(other))),
        }
    }

    /// Flattens one JSON object into sorted `key = value` pairs.
    fn group(title: &str, value: Option<&Value>) -> Option<ContextGroup> {
        let map = value?.as_object()?;

        let mut entries: Vec<(String, String)> = Vec::new();
        for (k, v) in map {
            flatten(k.clone(), v, 0, &mut entries);
        }
        if entries.is_empty() {
            return None;
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        Some(ContextGroup {
            title: title.to_string(),
            entries,
        })
    }

    /// `2m ago` / `3h ago` / `5d ago`. Takes `now` so it is testable without a clock.
    fn ago(t: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
        let minutes = (now - t).num_minutes().max(0);
        match minutes {
            0 => "just now".to_string(),
            1..=59 => format!("{minutes}m ago"),
            60..=1439 => format!("{}h ago", minutes / 60),
            _ => format!("{}d ago", minutes / 1440),
        }
    }

    impl From<thermite_core::api::issues::IssueListItem> for IssueRow {
        fn from(item: thermite_core::api::issues::IssueListItem) -> Self {
            let now = chrono::Utc::now();
            let issue = item.issue;
            Self {
                id: issue.id,
                title: issue.title,
                culprit: issue.culprit,
                level: issue.level,
                status: issue.status,
                times_seen: issue.times_seen,
                first_seen: issue.first_seen.to_rfc3339(),
                last_seen: issue.last_seen.to_rfc3339(),
                counts: item.counts,
                users_affected: item.users_affected,
                // Filled in by the caller, which knows which issues have analyses.
                has_analysis: false,
                is_new: now - issue.first_seen < chrono::Duration::hours(24),
                last_seen_ago: ago(issue.last_seen, now),
            }
        }
    }

    impl From<thermite_core::api::stats::Stats> for ProjectStats {
        fn from(stats: thermite_core::api::stats::Stats) -> Self {
            Self {
                window: stats.window.to_string(),
                resolution: stats.resolution.to_string(),
                series: stats
                    .series
                    .into_iter()
                    .map(|b| StatBucket {
                        bucket: b.bucket.to_rfc3339(),
                        count: b.count,
                        dropped: b.dropped,
                    })
                    .collect(),
                totals: StatTotals {
                    events: stats.totals.events,
                    dropped: stats.totals.dropped,
                    dropped_by_reason: stats.totals.dropped_by_reason,
                    unresolved_issues: stats.totals.unresolved_issues,
                    new_issues: stats.totals.new_issues,
                    regressions: stats.totals.regressions,
                },
            }
        }
    }

    impl From<thermite_core::api::releases::ReleaseHealth> for ReleaseHealthRow {
        fn from(health: thermite_core::api::releases::ReleaseHealth) -> Self {
            Self {
                version: health.version,
                sessions: health.sessions,
                crashed: health.crashed,
                crash_free_rate: health.crash_free_rate,
                series: health.series,
            }
        }
    }

    impl From<thermite_core::api::overview::ProjectOverview> for ProjectOverviewRow {
        fn from(p: thermite_core::api::overview::ProjectOverview) -> Self {
            Self {
                slug: p.slug,
                name: p.name,
                unresolved_issues: p.unresolved_issues,
                events_last_24h: p.events_last_24h,
                new_issues_24h: p.new_issues_24h,
                monitors_failing: p.monitors_failing,
                alerts_dead_lettered: p.alerts_dead_lettered,
                series: p.series,
            }
        }
    }

    impl From<thermite_core::api::projects::ProjectSummary> for ProjectSummary {
        fn from(p: thermite_core::api::projects::ProjectSummary) -> Self {
            Self {
                id: p.id,
                slug: p.slug,
                name: p.name,
                dsn: p.dsn,
                unresolved_issues: p.unresolved_issues,
                events_last_24h: p.events_last_24h,
                alert_email: p.alert_email,
                alert_webhook: p.alert_webhook,
                repo_url: p.repo_url,
                keys: p
                    .keys
                    .into_iter()
                    .map(|k| ProjectKey {
                        label: k.label,
                        dsn: k.dsn,
                    })
                    .collect(),
            }
        }
    }

    impl From<thermite_core::monitors::Monitor> for MonitorRow {
        fn from(m: thermite_core::monitors::Monitor) -> Self {
            // One readable string rather than three fields the UI would have to reassemble.
            let schedule = match m.schedule_type.as_str() {
                "interval" => format!(
                    "every {} {}{}",
                    m.schedule_value,
                    m.schedule_unit.as_deref().unwrap_or("minute"),
                    if m.schedule_value == "1" { "" } else { "s" }
                ),
                _ => m.schedule_value,
            };

            Self {
                slug: m.slug,
                schedule,
                timezone: m.timezone,
                status: m.status,
                last_checkin_at: m.last_checkin_at.map(|t| t.to_rfc3339()),
                next_due_at: m.next_due_at.map(|t| t.to_rfc3339()),
            }
        }
    }

    impl From<thermite_core::api::issues::IssueTag> for IssueTag {
        fn from(t: thermite_core::api::issues::IssueTag) -> Self {
            Self {
                key: t.key,
                value: t.value,
                times_seen: t.times_seen,
            }
        }
    }

    impl From<thermite_core::api::analyses::Analysis> for Analysis {
        fn from(a: thermite_core::api::analyses::Analysis) -> Self {
            Self {
                id: a.id,
                source: a.source,
                summary: a.summary,
                details: a.details,
                suggested_fix: a.suggested_fix,
                confidence: a.confidence,
                release: a.release,
                fix_url: a.fix_url,
                fix_verdict: a.fix_verdict,
                regressed_in: a.regressed_in,
                created_at: a.created_at.to_rfc3339(),
            }
        }
    }

    impl From<&Value> for Frame {
        fn from(frame: &Value) -> Self {
            Self {
                function: text(frame, "function"),
                filename: text(frame, "filename").or_else(|| text(frame, "abs_path")),
                module: text(frame, "module"),
                lineno: frame.get("lineno").and_then(Value::as_i64),
                colno: frame.get("colno").and_then(Value::as_i64),
                in_app: frame
                    .get("in_app")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                context_line: frame
                    .get("context_line")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                pre_context: lines(frame, "pre_context"),
                post_context: lines(frame, "post_context"),
            }
        }
    }

    impl From<thermite_core::api::issues::EventDetail> for EventDetail {
        fn from(event: thermite_core::api::issues::EventDetail) -> Self {
            let exception = event
                .exception
                .iter()
                .map(|value| ExceptionValue {
                    kind: text(value, "type").unwrap_or_else(|| "Error".to_string()),
                    value: text(value, "value"),
                    module: text(value, "module"),
                    handled: value.pointer("/mechanism/handled").and_then(Value::as_bool),
                    frames: value
                        .pointer("/stacktrace/frames")
                        .and_then(Value::as_array)
                        .map(|frames| frames.iter().map(Frame::from).collect())
                        .unwrap_or_default(),
                })
                .collect();

            let breadcrumbs = event
                .breadcrumbs
                .iter()
                .map(|value| Breadcrumb {
                    timestamp: text(value, "timestamp"),
                    category: text(value, "category").or_else(|| text(value, "type")),
                    level: text(value, "level"),
                    message: text(value, "message"),
                })
                .collect();

            let context = [
                group("Tags", Some(&event.tags)),
                group("User", Some(&event.user)),
                group("Request", Some(&event.request)),
                group("Contexts", Some(&event.contexts)),
                group("Extra", Some(&event.extra)),
            ]
            .into_iter()
            .flatten()
            .collect();

            Self {
                event_id: event.event_id.simple().to_string(),
                timestamp: event.timestamp.to_rfc3339(),
                level: event.level,
                platform: event.platform,
                environment: event.environment,
                release: event.release,
                transaction: event.transaction,
                server_name: event.server_name,
                message: event.message,
                exception,
                breadcrumbs,
                context,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn entries(value: serde_json::Value) -> Vec<(String, String)> {
            group("Contexts", Some(&value)).unwrap().entries
        }

        #[test]
        fn ago_picks_the_coarsest_fitting_unit() {
            let now = chrono::Utc::now();
            assert_eq!(ago(now, now), "just now");
            assert_eq!(ago(now - chrono::Duration::minutes(2), now), "2m ago");
            assert_eq!(ago(now - chrono::Duration::hours(3), now), "3h ago");
            assert_eq!(ago(now - chrono::Duration::days(5), now), "5d ago");
            // A client clock ahead of the event must not print a negative age.
            assert_eq!(ago(now + chrono::Duration::minutes(5), now), "just now");
        }

        #[test]
        fn nested_contexts_flatten_to_dotted_keys() {
            let flattened = entries(serde_json::json!({
                "runtime": {"name": "CPython", "version": "3.12.4"},
                "os": {"name": "Linux", "version": "6.8.0-45-generic"},
            }));

            assert_eq!(
                flattened,
                vec![
                    ("os.name".to_string(), "Linux".to_string()),
                    ("os.version".to_string(), "6.8.0-45-generic".to_string()),
                    ("runtime.name".to_string(), "CPython".to_string()),
                    ("runtime.version".to_string(), "3.12.4".to_string()),
                ]
            );
        }

        #[test]
        fn scalars_arrays_and_over_deep_nesting() {
            let flattened = entries(serde_json::json!({
                "count": 3,
                "device": {"cpu_brands": ["Apple M1", "Apple M2"]},
                "deep": {"a": {"b": {"c": {"d": 1}}}},
            }));

            // `deep` nests past MAX_DEPTH, so it is dropped rather than stringified.
            assert_eq!(
                flattened,
                vec![
                    ("count".to_string(), "3".to_string()),
                    (
                        "device.cpu_brands".to_string(),
                        "Apple M1, Apple M2".to_string()
                    ),
                ]
            );
        }
    }
}

#[cfg(test)]
mod format_tests {
    use super::thousands;

    #[test]
    fn groups_digits_in_threes_from_the_right() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(999), "999");
        // The boundary the naive `len % 3` version gets wrong.
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(12483), "12,483");
        assert_eq!(thousands(100000), "100,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn the_sign_is_not_grouped_with_the_digits() {
        assert_eq!(thousands(-1000), "-1,000");
        assert_eq!(thousands(i64::MIN), "-9,223,372,036,854,775,808");
    }
}
