//! SQLite access: open the pool, run migrations, read the `meta` values.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use crate::util::{hex, random_bytes};

/// File name of the database inside the data directory.
pub const DB_FILE: &str = "tether.db";

/// How long a connection waits for a lock before failing with "busy".
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Values stored in `meta` the first time the server starts.
pub struct Meta {
    /// Identifies this database; changes only if it is replaced.
    pub epoch: String,
    /// Keys the fake-prelogin HMAC.
    pub server_secret: [u8; 32],
}

/// Open (creating if needed) `{data_dir}/tether.db` in WAL mode and apply all
/// pending migrations.
pub async fn open(data_dir: &Path) -> anyhow::Result<SqlitePool> {
    private_dir_and_file(data_dir)?;
    let opts = SqliteConnectOptions::from_str("sqlite://")?
        .filename(data_dir.join(DB_FILE))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// The data dir is 0700 and `tether.db` 0600 (SQLite gives `-wal`/`-shm` the
/// main file's mode): the file holds the setup code and invite keys in clear,
/// and a bare-metal install must not leave them to other local users.
#[cfg(unix)]
fn private_dir_and_file(data_dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(data_dir)?;
    // An existing dir (older install, 0755) is tightened too; best effort, as it may not be ours.
    let _ = std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700));
    let file = data_dir.join(DB_FILE);
    std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&file)?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    // Sidecars left by an older install (SQLite reuses them as they are).
    for ext in ["tether.db-wal", "tether.db-shm"] {
        let side = data_dir.join(ext);
        if side.exists() {
            std::fs::set_permissions(&side, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn private_dir_and_file(data_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    Ok(())
}

/// Begin a write transaction with `BEGIN IMMEDIATE`: the write lock is taken
/// up front, so a read followed by a write inside it can never fail with a
/// busy-snapshot error, and concurrent writers queue instead of racing.
pub async fn begin_write(
    pool: &SqlitePool,
) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, sqlx::Error> {
    pool.begin_with("BEGIN IMMEDIATE").await
}

/// Read `epoch` and `server_secret` from `meta`, creating them on first use.
pub async fn ensure_meta(pool: &SqlitePool) -> anyhow::Result<Meta> {
    // `INSERT OR IGNORE` makes this safe if two processes race on first start.
    let epoch = hex(&random_bytes::<16>());
    sqlx::query("INSERT OR IGNORE INTO meta (k, v) VALUES ('epoch', ?)")
        .bind(epoch.into_bytes())
        .execute(pool)
        .await?;
    sqlx::query("INSERT OR IGNORE INTO meta (k, v) VALUES ('server_secret', ?)")
        .bind(random_bytes::<32>().to_vec())
        .execute(pool)
        .await?;

    let epoch: Vec<u8> = sqlx::query_scalar("SELECT v FROM meta WHERE k = 'epoch'")
        .fetch_one(pool)
        .await?;
    let secret: Vec<u8> = sqlx::query_scalar("SELECT v FROM meta WHERE k = 'server_secret'")
        .fetch_one(pool)
        .await?;
    Ok(Meta {
        epoch: String::from_utf8(epoch)?,
        server_secret: secret
            .try_into()
            .map_err(|_| anyhow::anyhow!("meta.server_secret is not 32 bytes"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_migrates_and_meta_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let pool = open(dir.path()).await.unwrap();
        let a = ensure_meta(&pool).await.unwrap();
        let b = ensure_meta(&pool).await.unwrap();
        assert_eq!(a.epoch.len(), 32);
        assert_eq!(a.epoch, b.epoch);
        assert_eq!(a.server_secret, b.server_secret);
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mode, "wal");
        // Every table of the schema exists.
        for t in [
            "meta",
            "accounts",
            "invites",
            "devices",
            "refresh_tokens",
            "access_tokens",
            "link_tokens",
            "records",
            "vault_seq",
        ] {
            let n: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name = ?")
                .bind(t)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(n, 1, "{t}");
        }
        pool.close().await;
        // Reopening keeps the values and re-running migrations is a no-op.
        let pool = open(dir.path()).await.unwrap();
        assert_eq!(ensure_meta(&pool).await.unwrap().epoch, a.epoch);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn data_dir_and_database_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("state");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        // Fresh dir, and an older install's world-readable dir and file.
        for pre in [false, true] {
            if pre {
                // Stale sidecars from an older install (a crash leaves them behind).
                for f in ["tether.db-wal", "tether.db-shm"] {
                    std::fs::write(data.join(f), b"").unwrap();
                    std::fs::set_permissions(data.join(f), std::fs::Permissions::from_mode(0o644)).unwrap();
                }
                std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
                std::fs::set_permissions(data.join(DB_FILE), std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            let pool = open(&data).await.unwrap();
            ensure_meta(&pool).await.unwrap();
            assert_eq!(mode(&data), 0o700);
            for f in [DB_FILE, "tether.db-wal", "tether.db-shm"] {
                assert_eq!(mode(&data.join(f)), 0o600, "{f}");
            }
            pool.close().await;
        }
    }
}
