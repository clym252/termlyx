//! The first-start setup code: a short invite for the first account.

use core_proto::ShortCode;
use sqlx::SqlitePool;

use crate::accounts::create_short_invite;

/// The text printed at start and by `setup-code`. Print it with `println!`
/// only: the code is a secret and must never reach the logs.
pub fn banner(code: &ShortCode) -> String {
    format!(
        "Termlyx server is ready. Setup code: {code}\n\
         In Termlyx: Settings → Sync → Connect to server, then enter this\n\
         code and this computer's address."
    )
}

/// The setup code to show, or `None` once the server has an account. The same
/// code is returned until it is used.
pub async fn ensure(db: &SqlitePool, now: i64) -> anyhow::Result<Option<ShortCode>> {
    // One write transaction: `setup-code` may race `serve`'s first start.
    let mut tx = crate::db::begin_write(db).await?;
    let accounts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts")
        .fetch_one(&mut *tx)
        .await?;
    if accounts > 0 {
        close(&mut tx, now).await?;
        tx.commit().await?;
        return Ok(None);
    }
    let stored: Option<String> = sqlx::query_scalar("SELECT code FROM setup_code WHERE id = 1")
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(code) = stored.and_then(|t| ShortCode::parse(&t).ok()) {
        let unused: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM invites WHERE token_hash = ? AND used_at IS NULL")
                .bind(code.key().to_vec())
                .fetch_optional(&mut *tx)
                .await?;
        if unused.is_some() {
            tx.commit().await?;
            return Ok(Some(code));
        }
    }
    let code = create_short_invite(&mut *tx, now, Some("setup"), i64::MAX - now).await?;
    sqlx::query("INSERT OR REPLACE INTO setup_code (id, code) VALUES (1, ?)")
        .bind(code.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(code))
}

/// Retire the setup code: mark its invite used and delete the stored row.
/// Called inside the transaction that creates the first account (and by
/// `ensure` for databases that already have one).
pub async fn close(conn: &mut sqlx::SqliteConnection, now: i64) -> Result<(), sqlx::Error> {
    let stored: Option<String> = sqlx::query_scalar("SELECT code FROM setup_code WHERE id = 1")
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(code) = stored.and_then(|t| ShortCode::parse(&t).ok()) {
        sqlx::query("UPDATE invites SET used_at = ? WHERE token_hash = ? AND used_at IS NULL")
            .bind(now)
            .bind(code.key().to_vec())
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query("DELETE FROM setup_code")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).await.unwrap();
        (dir, db)
    }

    #[tokio::test]
    async fn setup_code_is_stable_until_used() {
        let (_d, db) = pool().await;
        let a = ensure(&db, 1_000).await.unwrap().unwrap();
        let b = ensure(&db, 2_000).await.unwrap().unwrap();
        assert_eq!(a.to_string(), b.to_string());
        assert!(banner(&a).contains(&format!("Setup code: {a}")));
        // A used code is replaced by a fresh one while no account exists.
        sqlx::query("UPDATE invites SET used_at = 5")
            .execute(&db)
            .await
            .unwrap();
        let c = ensure(&db, 3_000).await.unwrap().unwrap();
        assert_ne!(a.to_string(), c.to_string());
    }
}
