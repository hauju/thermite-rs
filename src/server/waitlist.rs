//! The waitlist table: one row per address.

use crate::models::AppError;
use crate::server::db::Database;

/// Record an address, or note that it asked again. Idempotent by design: the primary key is the
/// address, so the count of rows is the count of people.
pub async fn join(db: &Database, email: &str) -> Result<(), AppError> {
    sqlx::query!(
        "insert into waitlist (email) values ($1)
         on conflict (email) do update set updated_at = now()",
        email
    )
    .execute(&db.pool)
    .await
    .map_err(|e| AppError::Internal(format!("waitlist insert failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::join;
    use crate::server::db::Database;
    use sqlx::PgPool;

    #[sqlx::test]
    async fn the_same_address_is_one_row(pool: PgPool) {
        let db = Database::from_pool(pool.clone());
        join(&db, "ada@example.com").await.unwrap();
        join(&db, "ada@example.com").await.unwrap();
        join(&db, "bob@example.com").await.unwrap();

        let rows: i64 = sqlx::query_scalar("select count(*) from waitlist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 2);
    }
}
