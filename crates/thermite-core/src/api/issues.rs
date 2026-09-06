use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
// The SQL below is assembled from compile-time constants only, never from request data.
use sqlx::PgPool;
use uuid::Uuid;

use crate::api::page_size;
use crate::error::{AppError, AppResult};
use crate::protocol::event as event_payload;
use crate::state::ThermiteState;

const STATUSES: [&str; 3] = ["unresolved", "resolved", "ignored"];

#[derive(Debug, Deserialize)]
pub struct IssueListQuery {
    status: Option<String>,
    /// Case-insensitive substring match on the issue title.
    q: Option<String>,
    /// Only issues that have events in this environment.
    environment: Option<String>,
    /// `key:value` — only issues that have events carrying this tag.
    tag: Option<String>,
    /// `last_seen` (default), `events` or `users`.
    sort: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// Splits a `key:value` tag filter. Only the first colon separates, so values may contain colons
/// (`url:https://…`).
fn parse_tag(tag: &str) -> AppResult<(String, String)> {
    match tag.split_once(':') {
        Some((key, value)) if !key.trim().is_empty() && !value.trim().is_empty() => {
            Ok((key.trim().to_string(), value.trim().to_string()))
        }
        _ => Err(AppError::BadRequest(format!(
            "malformed tag filter {tag:?}; expected key:value"
        ))),
    }
}

/// How to order an issue list.
///
/// `last_seen` answers "what just happened"; `events` answers "what is worst"; `users` answers
/// "what is worst for the most people" — 500 events on 400 users and 10,000 on one are different
/// problems. All three are wanted, and recency alone is a poor default for triage — a three-event
/// blip outranks a thousand-event outage whenever the blip fired more recently.
const SORTS: [&str; 3] = ["last_seen", "events", "users"];

fn order_by(sort: Option<&str>) -> AppResult<&'static str> {
    match sort.unwrap_or("last_seen") {
        "last_seen" => Ok("i.last_seen desc, i.id desc"),
        // Ties broken by recency, so equal-volume issues still read newest-first.
        "events" => Ok("i.times_seen desc, i.last_seen desc, i.id desc"),
        "users" => Ok("i.users_affected desc, i.times_seen desc, i.last_seen desc, i.id desc"),
        other => Err(AppError::BadRequest(format!(
            "unknown sort {other:?}; expected one of {}",
            SORTS.join(", ")
        ))),
    }
}

/// An issue row, plus the 24h sparkline the list renders next to it.
#[derive(Debug, Serialize)]
pub struct IssueListItem {
    #[serde(flatten)]
    pub issue: IssueSummary,
    /// 24 hourly counts, oldest first.
    pub counts: Vec<i64>,
    /// Distinct users this issue has hit. 0 when the SDK sends no user context.
    pub users_affected: i64,
    /// Where the issue stands in the triage queue: `queued` while its notification waits for an
    /// agent, `claimed` while one holds the lease. Absent once the work is acked, or if the issue
    /// was never queued. An agent listing issues can tell what is already in hand.
    pub triage: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct IssueSummary {
    pub id: i64,
    pub project_id: i64,
    pub title: String,
    pub culprit: Option<String>,
    pub exception_type: Option<String>,
    pub exception_value: Option<String>,
    pub level: String,
    pub platform: Option<String>,
    pub status: String,
    pub times_seen: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

const ISSUE_COLUMNS: &str = "id, project_id, title, culprit, exception_type, exception_value,
                             level, platform, status, times_seen, first_seen, last_seen";

/// An issue plus its most recent event, in full.
///
/// One request is enough to diagnose the issue: the exception chain, every stack frame with its
/// source context, the breadcrumbs leading up to it, and the runtime contexts.
#[derive(Debug, Serialize)]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: IssueSummary,
    pub latest_event: Option<EventDetail>,
    /// What agents have already concluded about this issue, newest first. Included here so a
    /// second agent picking the issue up can see prior work instead of redoing it.
    pub analyses: Vec<crate::api::analyses::Analysis>,
    /// Tag value counts across *all* the issue's events, not just the latest one — "every one of
    /// the 400 events has server_name=web-3" is a diagnosis in itself. Read from the issue_tags
    /// rollup, so it stays true after retention drops the events.
    pub tags: Vec<IssueTag>,
    /// Distinct users this issue has hit. 0 when the SDK sends no user context.
    pub users_affected: i64,
    /// The release the oldest retained event reported — for a new issue, the upper bound on where
    /// the bug was introduced.
    pub first_seen_release: Option<String>,
    /// For a regression: the release the issue was resolved against before it reopened — the last
    /// known good. With the latest event's release as the bad side,
    /// `git diff <regressed_from_release>..<release>` is the change set that reintroduced the bug.
    pub regressed_from_release: Option<String>,
    /// The project's repository, when one is configured — the same value the triage item carries,
    /// so an agent that came in through `get_issue` rather than a claim is not missing it.
    pub repo_url: Option<String>,
    /// What happened to the issue, oldest first: status changes, reopens, analyses and notes. An
    /// agent reading "resolved twice already, came back both times" reasons differently.
    pub activity: Vec<crate::api::activity::Activity>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct IssueTag {
    pub key: String,
    pub value: String,
    pub times_seen: i64,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct EventDetail {
    pub id: i64,
    pub event_id: Uuid,
    pub issue_id: i64,
    pub timestamp: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub level: String,
    pub platform: Option<String>,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub transaction: Option<String>,
    pub server_name: Option<String>,
    pub message: Option<String>,

    /// The exception chain, always an array — SDKs send this as `{"values": [...]}`, a bare list or
    /// a bare object, and normalizing here means a caller never has to branch.
    pub exception: Vec<Value>,
    /// Always an array, for the same reason.
    pub breadcrumbs: Vec<Value>,

    pub tags: Value,
    pub contexts: Value,
    pub extra: Value,
    pub user: Value,
    pub request: Value,
    pub sdk: Value,
    pub modules: Value,
}

#[derive(sqlx::FromRow)]
struct EventRow {
    id: i64,
    event_id: Uuid,
    issue_id: i64,
    timestamp: DateTime<Utc>,
    received_at: DateTime<Utc>,
    level: String,
    platform: Option<String>,
    environment: Option<String>,
    release: Option<String>,
    transaction: Option<String>,
    server_name: Option<String>,
    message: Option<String>,
    data: Value,
}

const EVENT_COLUMNS: &str = "id, event_id, issue_id, timestamp, received_at, level, platform,
                             environment, release, transaction, server_name, message, data";

impl From<EventRow> for EventDetail {
    fn from(row: EventRow) -> Self {
        let owned = |key: &str| row.data.get(key).cloned().unwrap_or(Value::Null);

        Self {
            exception: event_payload::values_of(row.data.get("exception"))
                .into_iter()
                .cloned()
                .collect(),
            breadcrumbs: event_payload::values_of(row.data.get("breadcrumbs"))
                .into_iter()
                .cloned()
                .collect(),
            tags: owned("tags"),
            contexts: owned("contexts"),
            extra: owned("extra"),
            user: owned("user"),
            request: owned("request"),
            sdk: owned("sdk"),
            modules: owned("modules"),

            id: row.id,
            event_id: row.event_id,
            issue_id: row.issue_id,
            timestamp: row.timestamp,
            received_at: row.received_at,
            level: row.level,
            platform: row.platform,
            environment: row.environment,
            release: row.release,
            transaction: row.transaction,
            server_name: row.server_name,
            message: row.message,
        }
    }
}

pub async fn list(
    State(state): State<ThermiteState>,
    Path(slug): Path<String>,
    Query(query): Query<IssueListQuery>,
) -> AppResult<Json<Vec<IssueListItem>>> {
    Ok(Json(
        for_project(
            &state.db,
            &slug,
            query.status.as_deref(),
            query.q.as_deref(),
            query.environment.as_deref(),
            query.tag.as_deref(),
            query.sort.as_deref(),
            query.limit,
            query.offset,
        )
        .await?,
    ))
}

/// Shared with the MCP tool of the same name.
#[allow(clippy::too_many_arguments)]
pub async fn for_project(
    db: &PgPool,
    slug: &str,
    status: Option<&str>,
    query: Option<&str>,
    environment: Option<&str>,
    tag: Option<&str>,
    sort: Option<&str>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<Vec<IssueListItem>> {
    if let Some(status) = status
        && !STATUSES.contains(&status)
    {
        return Err(AppError::BadRequest(format!(
            "unknown status {status:?}; expected one of {}",
            STATUSES.join(", ")
        )));
    }

    let order = order_by(sort)?;
    // `environment=x` is sugar for `tag=environment:x` — ingest synthesizes the promoted fields
    // into issue_tags, so both filters are the same mechanism.
    let environment = environment.map(|v| ("environment".to_string(), v.to_string()));
    let tag = tag.map(parse_tag).transpose()?;

    let project_id: Option<(i64,)> = sqlx::query_as("select id from projects where slug = $1")
        .bind(slug)
        .fetch_optional(db)
        .await?;
    let Some((project_id,)) = project_id else {
        return Err(AppError::NotFound);
    };

    let issues: Vec<IssueSummary> = sqlx::query_as(&format!(
        "select {ISSUE_COLUMNS}
           from issues i
          where i.project_id = $1
            and ($2::text is null or i.status = $2)
            and ($3::text is null or i.title ilike '%' || $3 || '%')
            and ($4::text is null or exists (select 1 from issue_tags t
                 where t.issue_id = i.id and t.key = $4 and t.value = $5))
            and ($6::text is null or exists (select 1 from issue_tags t
                 where t.issue_id = i.id and t.key = $6 and t.value = $7))
          order by {order}
          limit $8 offset $9"
    ))
    .bind(project_id)
    .bind(status)
    .bind(query)
    .bind(environment.as_ref().map(|(key, _)| key.as_str()))
    .bind(environment.as_ref().map(|(_, value)| value.as_str()))
    .bind(tag.as_ref().map(|(key, _)| key.as_str()))
    .bind(tag.as_ref().map(|(_, value)| value.as_str()))
    .bind(page_size(limit))
    .bind(offset.unwrap_or(0).max(0))
    .fetch_all(db)
    .await?;

    // One rollup query for the whole page rather than one per row.
    let ids: Vec<i64> = issues.iter().map(|i| i.id).collect();
    let mut sparklines = crate::api::stats::sparklines(db, &ids).await?;
    let mut users = users_affected(db, &ids).await?;
    let mut triage = triage_state(db, &ids).await?;

    Ok(issues
        .into_iter()
        .map(|issue| IssueListItem {
            counts: sparklines.remove(&issue.id).unwrap_or_default(),
            users_affected: users.remove(&issue.id).unwrap_or(0),
            triage: triage.remove(&issue.id),
            issue,
        })
        .collect())
}

/// The triage state of a page of issues, in one query. An issue with an unacked notification is
/// `claimed` while the lease on it is live and `queued` otherwise — an expired lease is work that
/// is available again, not work in hand. Newest notification wins, since a regression re-queues
/// an issue that was acked before.
async fn triage_state(
    db: &PgPool,
    ids: &[i64],
) -> AppResult<std::collections::HashMap<i64, String>> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "select distinct on (issue_id)
                issue_id,
                case when lease_until > now() then 'claimed' else 'queued' end
           from notifications
          where issue_id = any($1)
            and acked_at is null
          order by issue_id, id desc",
    )
    .bind(ids)
    .fetch_all(db)
    .await?;

    Ok(rows.into_iter().collect())
}

/// Distinct-user counts for a page of issues, in one query — from the counter `digest()`
/// maintains, never `count(*)` over `issue_tags`: the rollup's per-key cardinality is capped and
/// its old rows are pruned by retention, so the counter is both cheaper and the only value that
/// stays true over time.
async fn users_affected(
    db: &PgPool,
    ids: &[i64],
) -> AppResult<std::collections::HashMap<i64, i64>> {
    let rows: Vec<(i64, i64)> =
        sqlx::query_as("select id, users_affected from issues where id = any($1)")
            .bind(ids)
            .fetch_all(db)
            .await?;

    Ok(rows.into_iter().collect())
}

#[derive(Debug, Deserialize)]
pub struct StatusUpdate {
    pub status: String,
    /// With `status=resolved`: stay resolved for events from any release already seen, and reopen
    /// only when a release first reported *after* this moment (or an event with no release) hits
    /// the issue again.
    #[serde(default)]
    pub in_next_release: bool,
}

/// Resolve, ignore, or reopen an issue.
///
/// Note that resolving is not permanent by itself: a resolved issue that sees traffic again is
/// reopened and queued as a regression. `in_next_release` narrows what counts as "again" to
/// releases newer than the newest one seen so far; ignoring is the one that always sticks.
pub async fn set_status(
    State(state): State<ThermiteState>,
    Path(id): Path<i64>,
    Json(body): Json<StatusUpdate>,
) -> AppResult<Json<IssueSummary>> {
    Ok(Json(
        update_status(
            &state.db,
            id,
            &body.status,
            body.in_next_release,
            Some("api"),
        )
        .await?,
    ))
}

/// Shared with the MCP tool of the same name. `actor` is who did it, for the issue's history;
/// the REST layer cannot know, so it says `api`.
pub async fn update_status(
    db: &PgPool,
    id: i64,
    status: &str,
    in_next_release: bool,
    actor: Option<&str>,
) -> AppResult<IssueSummary> {
    if !STATUSES.contains(&status) {
        return Err(AppError::BadRequest(format!(
            "unknown status {status:?}; expected one of {}",
            STATUSES.join(", ")
        )));
    }

    if in_next_release && status != "resolved" {
        return Err(AppError::BadRequest(
            "in_next_release only makes sense with status=resolved".to_string(),
        ));
    }

    // "Until the next release" anchors on the newest release seen so far. Without any release
    // there is nothing to anchor on — refusing beats a resolve that silently behaves like a
    // plain one.
    let resolved_in: Option<i64> = if in_next_release {
        let latest: Option<i64> = sqlx::query_scalar(
            "select r.id
               from releases r
               join issues i on i.project_id = r.project_id
              where i.id = $1
              order by r.id desc
              limit 1",
        )
        .bind(id)
        .fetch_optional(db)
        .await?;

        match latest {
            Some(release_id) => Some(release_id),
            None => {
                return Err(AppError::BadRequest(
                    "this project has no releases; configure the SDK to send one".to_string(),
                ));
            }
        }
    } else {
        None
    };

    // The change and its history line commit together: a status without the line that says who
    // set it, or the reverse, would be a history that lies.
    let mut tx = db.begin().await?;
    let before: Option<String> =
        sqlx::query_scalar("select status from issues where id = $1 for update")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(before) = before else {
        return Err(AppError::NotFound);
    };

    let issue: IssueSummary = sqlx::query_as(&format!(
        "update issues set status = $2, resolved_in_release_id = $3
          where id = $1 returning {ISSUE_COLUMNS}"
    ))
    .bind(id)
    .bind(status)
    .bind(resolved_in)
    .fetch_one(&mut *tx)
    .await?;

    let anchor: Option<String> = match resolved_in {
        Some(release_id) => {
            sqlx::query_scalar("select version from releases where id = $1")
                .bind(release_id)
                .fetch_optional(&mut *tx)
                .await?
        }
        None => None,
    };
    crate::api::activity::record_status(
        &mut tx,
        id,
        actor,
        &before,
        status,
        in_next_release,
        anchor.as_deref(),
    )
    .await?;
    tx.commit().await?;

    Ok(issue)
}

pub async fn detail(
    State(state): State<ThermiteState>,
    Path(id): Path<i64>,
) -> AppResult<Json<IssueDetail>> {
    Ok(Json(detail_of(&state.db, id).await?))
}

/// Shared with the MCP `get_issue` tool.
pub async fn detail_of(db: &PgPool, id: i64) -> AppResult<IssueDetail> {
    let issue: Option<IssueSummary> =
        sqlx::query_as(&format!("select {ISSUE_COLUMNS} from issues where id = $1"))
            .bind(id)
            .fetch_optional(db)
            .await?;

    let Some(issue) = issue else {
        return Err(AppError::NotFound);
    };

    let (first_seen_release, regressed_from_release, repo_url): (
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "select (select e.release from events e where e.issue_id = i.id
                  order by e.timestamp asc, e.id asc limit 1),
                (select r.version from releases r where r.id = i.regressed_from_release_id),
                (select p.repo_url from projects p where p.id = i.project_id)
           from issues i where i.id = $1",
    )
    .bind(id)
    .fetch_one(db)
    .await?;

    Ok(IssueDetail {
        latest_event: latest_event(db, id).await?,
        analyses: crate::api::analyses::for_issue(db, id).await?,
        tags: tags_of(db, id).await?,
        users_affected: users_affected(db, &[id]).await?.remove(&id).unwrap_or(0),
        first_seen_release,
        regressed_from_release,
        repo_url,
        activity: crate::api::activity::for_issue(db, id).await?,
        issue,
    })
}

/// The issue's tag distribution, most frequent value first within each key. Capped: a
/// high-cardinality tag (say, a user id) should not turn the detail response into a dump.
async fn tags_of(db: &PgPool, issue_id: i64) -> AppResult<Vec<IssueTag>> {
    let tags: Vec<IssueTag> = sqlx::query_as(
        "select key, value, times_seen, last_seen
           from (select *, row_number() over (partition by key order by times_seen desc, value)
                        as rank
                   from issue_tags
                  where issue_id = $1) ranked
          where rank <= 10
          order by key, times_seen desc, value",
    )
    .bind(issue_id)
    .fetch_all(db)
    .await?;

    Ok(tags)
}

async fn latest_event(db: &PgPool, issue_id: i64) -> AppResult<Option<EventDetail>> {
    let row: Option<EventRow> = sqlx::query_as(&format!(
        "select {EVENT_COLUMNS}
           from events
          where issue_id = $1
          order by timestamp desc, id desc
          limit 1"
    ))
    .bind(issue_id)
    .fetch_optional(db)
    .await?;

    Ok(row.map(EventDetail::from))
}

#[derive(Debug, Deserialize)]
pub struct EventListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

pub async fn events(
    State(state): State<ThermiteState>,
    Path(id): Path<i64>,
    Query(query): Query<EventListQuery>,
) -> AppResult<Json<Vec<EventDetail>>> {
    let rows: Vec<EventRow> = sqlx::query_as(&format!(
        "select {EVENT_COLUMNS}
           from events
          where issue_id = $1
          order by timestamp desc, id desc
          limit $2 offset $3"
    ))
    .bind(id)
    .bind(page_size(query.limit))
    .bind(query.offset.unwrap_or(0).max(0))
    .fetch_all(&state.db)
    .await?;

    Ok(Json(rows.into_iter().map(EventDetail::from).collect()))
}

/// The project an issue belongs to, for a caller holding only the issue id that must decide
/// access before reading anything else.
pub async fn project_slug_of_issue(db: &PgPool, issue_id: i64) -> AppResult<String> {
    let row: Option<(String,)> = sqlx::query_as(
        "select p.slug from issues i join projects p on p.id = i.project_id where i.id = $1",
    )
    .bind(issue_id)
    .fetch_optional(db)
    .await?;
    row.map(|(slug,)| slug).ok_or(AppError::NotFound)
}

/// A pointer to one of an issue's events: enough to pick it out of a list, without the event.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EventRef {
    pub event_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub release: Option<String>,
}

/// The issue's retained events, newest first — for stepping through them one at a time. The
/// oldest retained one is where a regression diagnosis starts; the latest is what `detail_of`
/// already carries.
pub async fn event_refs(
    db: &PgPool,
    issue_id: i64,
    limit: Option<i64>,
) -> AppResult<Vec<EventRef>> {
    let refs: Vec<EventRef> = sqlx::query_as(
        "select event_id, timestamp, release
           from events
          where issue_id = $1
          order by timestamp desc, id desc
          limit $2",
    )
    .bind(issue_id)
    .bind(page_size(limit))
    .fetch_all(db)
    .await?;

    Ok(refs)
}

/// Looks an event up by the id the SDK assigned it, which is what appears in application logs.
pub async fn event(
    State(state): State<ThermiteState>,
    Path(event_id): Path<String>,
) -> AppResult<Json<EventDetail>> {
    Ok(Json(event_by_id(&state.db, &event_id).await?))
}

/// Shared with the MCP `get_event` tool.
pub async fn event_by_id(db: &PgPool, event_id: &str) -> AppResult<EventDetail> {
    let parsed = Uuid::parse_str(event_id)
        .map_err(|_| AppError::BadRequest(format!("{event_id:?} is not a valid event id")))?;

    let row: Option<EventRow> = sqlx::query_as(&format!(
        "select {EVENT_COLUMNS} from events where event_id = $1 order by id desc limit 1"
    ))
    .bind(parsed)
    .fetch_optional(db)
    .await?;

    row.map(EventDetail::from).ok_or(AppError::NotFound)
}
