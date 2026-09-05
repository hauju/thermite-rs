//! Recording the release an event or session came from.
//!
//! Shared by [`super::digest`] and [`super::sessions`], which must agree on release identity: the
//! row's id is the release *ordering* ("first reported later" means newer), and both
//! resolved-until-next-release and release health compare against it.

use crate::error::AppResult;

/// Distinct releases recorded per project. Release strings are client-controlled; past the cap an
/// event's release is treated as absent rather than minting rows forever. This cap is also what
/// bounds `session_counts`, which is keyed on `releases.id`.
pub const MAX_RELEASES_PER_PROJECT: i64 = 10_000;

/// Finds the release, inserting it on first sighting.
///
/// Returns `None` when the project is at its release cap — the caller then treats the payload as
/// carrying no release at all.
pub async fn resolve(
    conn: &mut sqlx::PgConnection,
    project_id: i64,
    version: &str,
) -> AppResult<Option<i64>> {
    let known: Option<i64> =
        sqlx::query_scalar("select id from releases where project_id = $1 and version = $2")
            .bind(project_id)
            .bind(version)
            .fetch_optional(&mut *conn)
            .await?;

    if let Some(id) = known {
        return Ok(Some(id));
    }

    // The no-op update exists only because `do nothing` would return no id on a concurrent first
    // sighting.
    Ok(sqlx::query_scalar(
        "insert into releases (project_id, version)
         select $1, $2
          where (select count(*) from releases where project_id = $1) < $3
         on conflict (project_id, version) do update set version = releases.version
         returning id",
    )
    .bind(project_id)
    .bind(version)
    .bind(MAX_RELEASES_PER_PROJECT)
    .fetch_optional(conn)
    .await?)
}
