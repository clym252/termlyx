//! The `users` and `devices` admin subcommands.
//!
//! Each function opens the database under `cfg.data_dir` (which must already
//! exist: a typo in `TERMLYX_DATA` must not create an empty server), does one
//! thing and returns the text for stdout. Output never contains verifiers,
//! tokens or key material, and text that came from clients (device names) has
//! control characters replaced so it cannot drive the admin's terminal.

use std::path::Path;

use anyhow::{Context, bail};
use sqlx::SqlitePool;

use crate::accounts;
use crate::config::Config;
use crate::db::{self, DB_FILE};
use crate::devices;

/// Open the existing database of `data_dir` (migrating it if this binary is
/// newer than the file).
pub async fn open_existing(data_dir: &Path) -> anyhow::Result<SqlitePool> {
    if !data_dir.join(DB_FILE).is_file() {
        bail!(
            "no database in {} (is TERMLYX_DATA right? the server creates it on first start)",
            data_dir.display()
        );
    }
    db::open(data_dir).await
}

/// The id of the account named `username` (any case).
pub async fn account_id(pool: &SqlitePool, username: &str) -> anyhow::Result<String> {
    let username =
        accounts::normalize_username(username).map_err(|e| anyhow::anyhow!("{}", e.message()))?;
    sqlx::query_scalar("SELECT id FROM accounts WHERE username = ?")
        .bind(&username)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no account named {username}"))
}

/// Run `f` on a pool for the existing database, closing the pool afterwards.
async fn with_pool<T>(
    cfg: &Config,
    f: impl AsyncFnOnce(&SqlitePool) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let pool = open_existing(&cfg.data_dir).await?;
    let out = f(&pool).await;
    pool.close().await;
    out
}

/// `YYYY-MM-DD HH:MM` in UTC for a Unix-millisecond timestamp.
fn when(ms: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(ms.div_euclid(1000)) {
        Ok(t) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute()
        ),
        Err(_) => "?".into(),
    }
}

/// `text` with control characters (terminal escapes, newlines) replaced.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Left-aligned columns separated by two spaces; the last column is not padded.
fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                out.push_str(cell);
            } else {
                out.push_str(&format!("{cell:<width$}  ", width = widths[i]));
            }
        }
        out.push('\n');
        out
    };
    let mut text = line(headers.to_vec());
    for row in rows {
        text.push_str(&line(row.iter().map(String::as_str).collect()));
    }
    text
}

fn yes_no(flag: i64) -> String {
    if flag != 0 { "yes" } else { "no" }.into()
}

/// `users list`: username, created, disabled, number of active devices.
pub async fn users_list(cfg: &Config) -> anyhow::Result<String> {
    with_pool(cfg, async |pool| {
        let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
            "SELECT a.username, a.created_at, a.disabled, \
                    (SELECT count(*) FROM devices d WHERE d.account_id = a.id AND d.revoked = 0) \
             FROM accounts a ORDER BY a.created_at, a.username",
        )
        .fetch_all(pool)
        .await?;
        let rows: Vec<Vec<String>> = rows
            .into_iter()
            .map(|(name, created, disabled, devices)| {
                vec![
                    clean(&name),
                    when(created),
                    yes_no(disabled),
                    devices.to_string(),
                ]
            })
            .collect();
        Ok(table(
            &["username", "created (UTC)", "disabled", "devices"],
            &rows,
        ))
    })
    .await
}

/// `users disable` / `users enable`. A disabled account gets `forbidden` on
/// login and refresh, and its access tokens stop working at once.
pub async fn set_disabled(cfg: &Config, username: &str, disabled: bool) -> anyhow::Result<String> {
    with_pool(cfg, async |pool| {
        let id = account_id(pool, username).await?;
        sqlx::query("UPDATE accounts SET disabled = ? WHERE id = ?")
            .bind(i64::from(disabled))
            .bind(&id)
            .execute(pool)
            .await?;
        let name = accounts::normalize_username(username).unwrap_or_default();
        Ok(if disabled {
            format!("disabled {name}: its devices are signed out on their next request\n")
        } else {
            format!("enabled {name}\n")
        })
    })
    .await
}

/// `users reset-schema`: set the account's record-schema high-water mark back
/// to 0, so clients older than the newest one that ever connected can write
/// again (after a bad client upgrade). The next request of any client records
/// its own schema again.
pub async fn reset_schema(cfg: &Config, username: &str) -> anyhow::Result<String> {
    with_pool(cfg, async |pool| {
        let id = account_id(pool, username).await?;
        sqlx::query("UPDATE accounts SET max_schema = 0 WHERE id = ?")
            .bind(&id)
            .execute(pool)
            .await?;
        let name = accounts::normalize_username(username).unwrap_or_default();
        Ok(format!(
            "reset the client schema mark of {name}: older clients can write again\n"
        ))
    })
    .await
}

fn plural(n: i64, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// `users delete USER [--yes]`: remove a disabled account and every row that
/// belongs to it (devices, their sessions, add-device codes, the recovery
/// token, the vault's records and sequence), so the username can be
/// registered again. Shared rows (invites, `meta`) stay. One write
/// transaction: all of it goes or none of it does.
///
/// Refuses an account that is not disabled (so its devices were already cut
/// off and a running server is serving it nothing), and without `yes` only
/// reports what it would delete. A running server keeps no sessions in
/// memory: every request looks its token up in the database, and an open
/// events socket closes at its next check once its device row is gone.
pub async fn delete_user(cfg: &Config, username: &str, yes: bool) -> anyhow::Result<String> {
    with_pool(cfg, async |pool| {
        let id = account_id(pool, username).await?;
        let mut tx = db::begin_write(pool).await?;
        let row: Option<(String, String, i64)> =
            sqlx::query_as("SELECT username, vault_id, disabled FROM accounts WHERE id = ?")
                .bind(&id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((name, vault_id, disabled)) = row else {
            bail!("no account named {}", clean(username));
        };
        let name = clean(&name);
        if disabled == 0 {
            bail!("{name} is not disabled: run `users disable {name}` first");
        }
        let devices: i64 = sqlx::query_scalar("SELECT count(*) FROM devices WHERE account_id = ?")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
        let records: i64 = sqlx::query_scalar("SELECT count(*) FROM records WHERE vault_id = ?")
            .bind(&vault_id)
            .fetch_one(&mut *tx)
            .await?;
        let what = format!(
            "{}, {}",
            plural(devices, "device"),
            plural(records, "record")
        );
        if !yes {
            bail!(
                "not deleting {name} ({what}) without --yes; \
                 run `users delete {name} --yes` to delete it for good"
            );
        }
        // Explicit deletes, children first: `records` and `vault_seq` have no
        // foreign key, and the rest must not depend on `foreign_keys` being on.
        for sql in [
            "DELETE FROM access_tokens WHERE account_id = ?1",
            "DELETE FROM refresh_tokens WHERE device_id IN \
             (SELECT id FROM devices WHERE account_id = ?1)",
            "DELETE FROM link_tokens WHERE account_id = ?1",
            "DELETE FROM devices WHERE account_id = ?1",
        ] {
            sqlx::query(sql).bind(&id).execute(&mut *tx).await?;
        }
        for sql in [
            "DELETE FROM records WHERE vault_id = ?",
            "DELETE FROM vault_seq WHERE vault_id = ?",
        ] {
            sqlx::query(sql).bind(&vault_id).execute(&mut *tx).await?;
        }
        sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(format!("deleted {name}: {what}\n"))
    })
    .await
}

/// `devices list USER`: id, name, platform, last seen, revoked.
pub async fn devices_list(cfg: &Config, username: &str) -> anyhow::Result<String> {
    with_pool(cfg, async |pool| {
        let id = account_id(pool, username).await?;
        let rows: Vec<(String, String, String, i64, i64)> = sqlx::query_as(
            "SELECT id, name, platform, last_seen, revoked FROM devices \
             WHERE account_id = ? ORDER BY created_at, id",
        )
        .bind(&id)
        .fetch_all(pool)
        .await?;
        let rows: Vec<Vec<String>> = rows
            .into_iter()
            .map(|(id, name, platform, seen, revoked)| {
                vec![
                    id,
                    clean(&name),
                    clean(&platform),
                    when(seen),
                    yes_no(revoked),
                ]
            })
            .collect();
        Ok(table(
            &["id", "name", "platform", "last seen (UTC)", "revoked"],
            &rows,
        ))
    })
    .await
}

/// `devices revoke DEVICE_ID`: cut a device off. Its tokens stop working at
/// once; an open events connection closes at its next check.
pub async fn revoke_device(cfg: &Config, device_id: &str) -> anyhow::Result<String> {
    with_pool(cfg, async |pool| {
        let row: Option<(String, String, i64, String)> = sqlx::query_as(
            "SELECT d.account_id, d.name, d.revoked, a.username \
             FROM devices d JOIN accounts a ON a.id = d.account_id WHERE d.id = ?",
        )
        .bind(device_id)
        .fetch_optional(pool)
        .await?;
        let Some((account_id, name, revoked, username)) = row else {
            bail!("no device with id {}", clean(device_id));
        };
        let name = clean(&name);
        if revoked != 0 {
            return Ok(format!(
                "device {device_id} ({name}) of {username} was already revoked\n"
            ));
        }
        let mut conn = pool.acquire().await?;
        devices::revoke_device(&mut conn, &account_id, device_id)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e.message()))
            .context("cannot revoke the device")?;
        Ok(format!(
            "revoked device {device_id} ({name}) of {username}\n"
        ))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_align_and_control_characters_are_replaced() {
        let t = table(
            &["a", "bbb"],
            &[vec!["xx".into(), "1".into()], vec!["y".into(), "22".into()]],
        );
        assert_eq!(t, "a   bbb\nxx  1\ny   22\n");
        assert_eq!(clean("a\u{1b}[31m\nb"), "a?[31m?b");
        assert_eq!(when(0), "1970-01-01 00:00");
        assert_eq!(when(1_760_968_980_000), "2025-10-20 14:03");
    }
}
