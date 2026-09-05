//! Creating and listing projects.
//!
//! Separated from the read API because these mutate what SDKs can report into. The application
//! mounts them behind the same credential as the rest of `/api/v1`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::auth;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::state::ThermiteState;

#[derive(Debug, Deserialize)]
pub struct NewProject {
    /// URL-safe identifier, used in API paths.
    pub slug: String,
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreatedProject {
    pub id: i64,
    pub slug: String,
    pub name: String,
    /// Configure an SDK with this. The public key it contains is not a secret — it only grants
    /// the ability to send events to this project.
    pub dsn: String,
}

pub async fn create(
    State(state): State<ThermiteState>,
    Json(body): Json<NewProject>,
) -> AppResult<(StatusCode, Json<CreatedProject>)> {
    let project = create_project(&state.db, &state.config, &body).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// Shared with the MCP tool of the same name.
pub async fn create_project(
    db: &PgPool,
    config: &Config,
    body: &NewProject,
) -> AppResult<CreatedProject> {
    let slug = body.slug.trim();
    // The slug appears in API paths, so keep it to characters that need no escaping.
    if slug.is_empty()
        || !slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AppError::BadRequest(
            "slug must be non-empty and contain only letters, digits, '-' or '_'".into(),
        ));
    }

    let name = body.name.as_deref().map(str::trim).unwrap_or(slug);
    let public_key = auth::generate_public_key();

    let existing: Option<(i64,)> = sqlx::query_as("select id from projects where slug = $1")
        .bind(slug)
        .fetch_optional(db)
        .await?;
    if existing.is_some() {
        return Err(AppError::BadRequest(format!(
            "a project with the slug {slug:?} already exists"
        )));
    }

    // Ingest authenticates against project_keys, so the default key is seeded there in the same
    // transaction — a project whose DSN cannot report would be a broken invariant, not a race.
    let mut tx = db.begin().await?;
    let (id,): (i64,) = sqlx::query_as(
        "insert into projects (slug, name, public_key) values ($1, $2, $3) returning id",
    )
    .bind(slug)
    .bind(name)
    .bind(&public_key)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("insert into project_keys (project_id, public_key) values ($1, $2)")
        .bind(id)
        .bind(&public_key)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(CreatedProject {
        dsn: config.dsn(&public_key, id),
        id,
        slug: slug.to_string(),
        name: name.to_string(),
    })
}

#[derive(Debug, Deserialize)]
pub struct ProjectUpdate {
    pub name: String,
}

/// `PATCH /api/v1/projects/{slug}` — rename the project's display name.
pub async fn patch_project(
    State(state): State<ThermiteState>,
    Path(slug): Path<String>,
    Json(body): Json<ProjectUpdate>,
) -> AppResult<StatusCode> {
    rename_project(&state.db, &slug, &body.name).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Shared with the dashboard's server function. The slug stays fixed — it is the identifier in
/// API paths and DSNs, so renaming it would silently break every configured SDK.
pub async fn rename_project(db: &PgPool, slug: &str, name: &str) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name must be non-empty".into()));
    }

    let updated = sqlx::query("update projects set name = $2 where slug = $1")
        .bind(slug)
        .bind(name)
        .execute(db)
        .await?
        .rows_affected();

    if updated == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// `DELETE /api/v1/projects/{slug}` — delete the project and everything under it.
pub async fn delete(
    State(state): State<ThermiteState>,
    Path(slug): Path<String>,
) -> AppResult<StatusCode> {
    delete_project(&state.db, &slug).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Shared with the dashboard's server function.
///
/// One statement: every child table references `projects` with `on delete cascade`, so issues,
/// events, rollups, monitors, keys and notifications go with it. Irreversible, and the project's
/// DSNs stop authenticating immediately.
pub async fn delete_project(db: &PgPool, slug: &str) -> AppResult<()> {
    let deleted = sqlx::query("delete from projects where slug = $1")
        .bind(slug)
        .execute(db)
        .await?
        .rows_affected();

    if deleted == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct NewProjectKey {
    /// Component name stamped onto events ingested with this key ('worker', 'saas').
    pub label: String,
}

#[derive(Debug, Serialize)]
pub struct CreatedProjectKey {
    pub label: String,
    /// Configure the component's SDK with this. Same project, so its events share the
    /// issue stream and carry `component: <label>` as a filterable tag.
    pub dsn: String,
}

/// `POST /api/v1/projects/{slug}/keys` — mint an additional, labeled DSN for one
/// component of the project.
pub async fn post_project_key(
    State(state): State<ThermiteState>,
    Path(slug): Path<String>,
    Json(body): Json<NewProjectKey>,
) -> AppResult<(StatusCode, Json<CreatedProjectKey>)> {
    let key = create_project_key(&state.db, &state.config, &slug, &body.label).await?;
    Ok((StatusCode::CREATED, Json(key)))
}

/// Shared with the dashboard's server function.
pub async fn create_project_key(
    db: &PgPool,
    config: &Config,
    slug: &str,
    label: &str,
) -> AppResult<CreatedProjectKey> {
    let label = label.trim();
    // The label becomes a tag value and a filter chip; the slug's charset keeps it
    // clean in both places.
    if label.is_empty()
        || !label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AppError::BadRequest(
            "label must be non-empty and contain only letters, digits, '-' or '_'".into(),
        ));
    }

    let project: Option<(i64,)> = sqlx::query_as("select id from projects where slug = $1")
        .bind(slug)
        .fetch_optional(db)
        .await?;
    let Some((project_id,)) = project else {
        return Err(AppError::NotFound);
    };

    let duplicate: Option<(i64,)> =
        sqlx::query_as("select id from project_keys where project_id = $1 and label = $2")
            .bind(project_id)
            .bind(label)
            .fetch_optional(db)
            .await?;
    if duplicate.is_some() {
        return Err(AppError::BadRequest(format!(
            "a key labeled {label:?} already exists on this project"
        )));
    }

    let public_key = auth::generate_public_key();
    sqlx::query("insert into project_keys (project_id, public_key, label) values ($1, $2, $3)")
        .bind(project_id)
        .bind(&public_key)
        .bind(label)
        .execute(db)
        .await?;

    Ok(CreatedProjectKey {
        label: label.to_string(),
        dsn: config.dsn(&public_key, project_id),
    })
}

/// `DELETE /api/v1/projects/{slug}/keys/{label}` — revoke a labeled component DSN.
pub async fn delete_key(
    State(state): State<ThermiteState>,
    Path((slug, label)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    delete_project_key(&state.db, &slug, &label).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Shared with the dashboard's server function.
///
/// Matches on `label`, which only labeled keys carry — the unlabeled seed key is the project's
/// default DSN and stays. Events already ingested keep their `component` tag; the key just stops
/// authenticating.
pub async fn delete_project_key(db: &PgPool, slug: &str, label: &str) -> AppResult<()> {
    let deleted = sqlx::query(
        "delete from project_keys
          where label = $2
            and project_id = (select id from projects where slug = $1)",
    )
    .bind(slug)
    .bind(label)
    .execute(db)
    .await?
    .rows_affected();

    if deleted == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AlertRouting {
    /// Comma-separated recipients. Replaces the instance-wide `THERMITE_ALERT_EMAIL` for this
    /// project; null (or blank) falls back to it.
    pub alert_email: Option<String>,
    /// Replaces `THERMITE_ALERT_WEBHOOK` for this project; null (or blank) falls back to it.
    pub alert_webhook: Option<String>,
}

/// `PUT /api/v1/projects/{slug}/alerts` — set or clear where this project's alerts go.
pub async fn put_alert_routing(
    State(state): State<ThermiteState>,
    Path(slug): Path<String>,
    Json(body): Json<AlertRouting>,
) -> AppResult<Json<AlertRouting>> {
    Ok(Json(set_alert_routing(&state.db, &slug, &body).await?))
}

/// Shared with the dashboard's server function.
pub async fn set_alert_routing(
    db: &PgPool,
    slug: &str,
    routing: &AlertRouting,
) -> AppResult<AlertRouting> {
    let email = normalize(routing.alert_email.as_deref());
    let webhook = normalize(routing.alert_webhook.as_deref());

    // Enough validation to catch a pasted wrong value; the SMTP library does the real parsing at
    // delivery time and logs what it rejects.
    if let Some(email) = &email
        && email.split(',').any(|entry| !entry.trim().contains('@'))
    {
        return Err(AppError::BadRequest(
            "alert_email must be a comma-separated list of email addresses".into(),
        ));
    }
    if let Some(webhook) = &webhook
        && !(webhook.starts_with("https://") || webhook.starts_with("http://"))
    {
        return Err(AppError::BadRequest(
            "alert_webhook must be an http(s) URL".into(),
        ));
    }

    let updated =
        sqlx::query("update projects set alert_email = $2, alert_webhook = $3 where slug = $1")
            .bind(slug)
            .bind(&email)
            .bind(&webhook)
            .execute(db)
            .await?
            .rows_affected();

    if updated == 0 {
        return Err(AppError::NotFound);
    }

    Ok(AlertRouting {
        alert_email: email,
        alert_webhook: webhook,
    })
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RepoSettings {
    /// Where this project's code lives. Null (or blank) clears it.
    pub repo_url: Option<String>,
}

/// `PUT /api/v1/projects/{slug}/repo` — set or clear the repository a triaging agent should open
/// its pull request against.
pub async fn put_repo(
    State(state): State<ThermiteState>,
    Path(slug): Path<String>,
    Json(body): Json<RepoSettings>,
) -> AppResult<Json<RepoSettings>> {
    Ok(Json(set_repo_url(&state.db, &slug, &body).await?))
}

/// Shared with the dashboard's server function.
///
/// Held to http(s) so it can anchor the host check on an analysis's `fix_url`. That also rules out
/// the `git@host:owner/repo.git` form, which is deliberate: it is a transport, not a link, and
/// nothing here can turn one into the other reliably.
pub async fn set_repo_url(
    db: &PgPool,
    slug: &str,
    settings: &RepoSettings,
) -> AppResult<RepoSettings> {
    let repo_url = normalize(settings.repo_url.as_deref());

    if let Some(url) = &repo_url
        && !(url.starts_with("https://") || url.starts_with("http://"))
    {
        return Err(AppError::BadRequest(
            "repo_url must be an http(s) URL, e.g. https://github.com/owner/repo".into(),
        ));
    }

    let updated = sqlx::query("update projects set repo_url = $2 where slug = $1")
        .bind(slug)
        .bind(&repo_url)
        .execute(db)
        .await?
        .rows_affected();

    if updated == 0 {
        return Err(AppError::NotFound);
    }

    Ok(RepoSettings { repo_url })
}

/// Trimmed value, with blank collapsing to null ("clear the override").
fn normalize(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}
