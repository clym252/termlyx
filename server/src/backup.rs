//! `backup` and `restore`.
//!
//! * [`backup`] writes a consistent copy of the database with SQLite's
//!   `VACUUM INTO` (one read snapshot, so it is safe while the server runs and
//!   writes) and, with `with_tls`, the self-signed key and certificate as
//!   `{out}.tls.tar` next to it. The copy holds exactly what the server holds:
//!   ciphertext, wrapped keys, hashes and sync metadata, never a plaintext
//!   field. Both files are mode 0600 and an existing file is never overwritten.
//! * [`restore`] needs the server stopped (it takes `{data}/server.lock`),
//!   validates the file, swaps it in atomically and **writes a new `epoch`**
//!   into it, so clients that see the epoch change know the server was rewound
//!   and resync instead of trusting their cursor.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, bail};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{Connection, SqliteConnection, SqlitePool};

use crate::db::DB_FILE;
use crate::lock::{DataLock, LockError};
use crate::tls;
use crate::util::{hex, random_bytes};

/// Tables every Tether server database has (all created by migration 0001).
const REQUIRED_TABLES: &[&str] = &[
    "meta",
    "accounts",
    "invites",
    "devices",
    "refresh_tokens",
    "access_tokens",
    "link_tokens",
    "records",
    "vault_seq",
];

/// The two TLS files a `--with-tls` backup carries, in tar order.
const TLS_FILES: [&str; 2] = ["key.pem", "cert.pem"];

/// No TLS file is bigger than this (a tar entry beyond it is refused).
const MAX_TLS_FILE: usize = 64 * 1024;

/// Where the TLS tar for the backup at `out` lives: `{out}.tls.tar`.
pub fn tls_tar_path(out: &Path) -> PathBuf {
    let mut name: OsString = out.as_os_str().to_owned();
    name.push(".tls.tar");
    PathBuf::from(name)
}

/// What [`backup`] wrote.
#[derive(Debug, Clone)]
pub struct BackupReport {
    /// The database copy.
    pub db: PathBuf,
    /// The TLS tar, with `with_tls`.
    pub tls: Option<PathBuf>,
}

impl std::fmt::Display for BackupReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "backed up the database to {}", self.db.display())?;
        if let Some(t) = &self.tls {
            write!(
                f,
                "\nbacked up the TLS key and certificate to {} (keep it private)",
                t.display()
            )?;
        }
        Ok(())
    }
}

/// What [`restore`] did.
#[derive(Debug, Clone)]
pub struct RestoreReport {
    /// The new database identity clients will see.
    pub epoch: String,
    /// Whether TLS material was restored too.
    pub tls_restored: bool,
}

impl std::fmt::Display for RestoreReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "restored the database; new epoch {} (clients will notice the rewind and resync)",
            self.epoch
        )?;
        if self.tls_restored {
            write!(f, "\nrestored the TLS key and certificate")?;
        }
        Ok(())
    }
}

fn create_new_private(path: &Path) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn sync_dir(dir: &Path) {
    // Best effort; not every platform can open a directory.
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

// ------------------------------------------------------------------ backup

/// Write a consistent copy of the database behind `db` to `out`, and with
/// `with_tls` the key and certificate under `{data_dir}/tls` to
/// `{out}.tls.tar`. Safe while the server is serving. Refuses to overwrite
/// either file; on failure neither is left behind.
pub async fn backup(
    db: &SqlitePool,
    data_dir: &Path,
    out: &Path,
    with_tls: bool,
) -> anyhow::Result<BackupReport> {
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if !parent.is_dir() {
        bail!("the directory {} does not exist", parent.display());
    }
    let tar_path = tls_tar_path(out);
    for p in [Some(out), with_tls.then_some(tar_path.as_path())]
        .into_iter()
        .flatten()
    {
        if exists(p) {
            bail!("{} already exists; refusing to overwrite it", p.display());
        }
    }
    // Read the TLS files first: a missing key must not leave a half backup.
    let tls_tar = if with_tls {
        let dir = data_dir.join("tls");
        let mut entries = vec![];
        for name in TLS_FILES {
            let bytes = fs::read(dir.join(name)).with_context(|| {
                format!(
                    "no TLS material to include: cannot read {} (--with-tls covers the \
                     self-signed certificate under the data directory)",
                    dir.join(name).display()
                )
            })?;
            entries.push((name, bytes));
        }
        Some(tar_encode(&entries)?)
    } else {
        None
    };

    // `VACUUM INTO` accepts an existing *empty* file, so create it first with
    // the right mode (and the create-new check closes the race on `exists`).
    let target = out
        .to_str()
        .context("the backup path must be valid UTF-8")?;
    create_new_private(out).with_context(|| format!("cannot create {}", out.display()))?;
    let copy = async {
        sqlx::query("VACUUM INTO ?")
            .bind(target)
            .execute(db)
            .await?;
        // Windows needs write access to flush.
        OpenOptions::new().write(true).open(out)?.sync_all()?;
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = copy {
        let _ = fs::remove_file(out);
        return Err(e.context("cannot write the database backup"));
    }

    let mut tls_path = None;
    if let Some(tar) = tls_tar {
        let written = (|| -> std::io::Result<()> {
            let mut f = create_new_private(&tar_path)?;
            f.write_all(&tar)?;
            f.sync_all()
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(out);
            let _ = fs::remove_file(&tar_path);
            return Err(
                anyhow::Error::new(e).context(format!("cannot write {}", tar_path.display()))
            );
        }
        tls_path = Some(tar_path);
    }
    sync_dir(parent);
    Ok(BackupReport {
        db: out.to_path_buf(),
        tls: tls_path,
    })
}

// ----------------------------------------------------------------- restore

/// Replace the database in `data_dir` with the backup `from`, and with
/// `with_tls` the TLS key and certificate with those in `{from}.tls.tar`.
///
/// Refuses while a server runs on `data_dir` (it holds the directory lock).
/// The file is validated first (`PRAGMA integrity_check`, the expected tables,
/// a migration history this build knows) and nothing is touched if it fails.
/// The copy is swapped in atomically (temp file, then rename), stale `-wal` and
/// `-shm` files of the replaced database are removed, and a new random `epoch`
/// is written into the restored database.
pub async fn restore(
    data_dir: &Path,
    from: &Path,
    with_tls: bool,
) -> anyhow::Result<RestoreReport> {
    if !from.is_file() {
        bail!("{} is not a file", from.display());
    }
    // Do not copy the live database over itself.
    let live = data_dir.join(DB_FILE);
    if let (Ok(a), Ok(b)) = (from.canonicalize(), live.canonicalize())
        && a == b
    {
        bail!(
            "{} is the database in use; give a backup file",
            from.display()
        );
    }
    let tls_files = if with_tls {
        let tar_path = tls_tar_path(from);
        let bytes = fs::read(&tar_path).with_context(|| {
            format!(
                "--with-tls needs {} (made by `backup --with-tls`)",
                tar_path.display()
            )
        })?;
        let files = tar_decode(&bytes).with_context(|| tar_path.display().to_string())?;
        check_tls_pems(&files).with_context(|| tar_path.display().to_string())?;
        Some(files)
    } else {
        None
    };

    let _lock = DataLock::acquire(data_dir).map_err(|e| match e {
        LockError::Held(_) => anyhow::anyhow!(
            "the server is running on {} (it holds the data directory lock); \
             stop it before restoring",
            data_dir.display()
        ),
        other => other.into(),
    })?;

    let tmp = data_dir.join(format!("{DB_FILE}.restoring"));
    let result = swap_in(data_dir, from, &tmp).await;
    if result.is_err() {
        remove_with_sidecars(&tmp);
    }
    let epoch = result?;

    let mut tls_restored = false;
    if let Some(files) = tls_files {
        let dir = data_dir.join("tls");
        tls::create_private_dir(&dir)?;
        for (name, bytes) in &files {
            tls::write_private(&dir.join(name), bytes)?;
        }
        tls_restored = true;
    }
    Ok(RestoreReport {
        epoch,
        tls_restored,
    })
}

/// Copy `from` to `tmp`, validate it, give it a new epoch, and rename it over
/// the database. Returns the new epoch. The caller removes `tmp` on error.
async fn swap_in(data_dir: &Path, from: &Path, tmp: &Path) -> anyhow::Result<String> {
    remove_with_sidecars(tmp); // a crashed earlier restore
    {
        let mut src =
            File::open(from).with_context(|| format!("cannot read {}", from.display()))?;
        let mut dst =
            create_new_private(tmp).with_context(|| format!("cannot create {}", tmp.display()))?;
        std::io::copy(&mut src, &mut dst)?;
        dst.sync_all()?;
    }
    let epoch = hex(&random_bytes::<16>());
    validate_and_stamp(tmp, &epoch)
        .await
        .with_context(|| format!("{} is not a usable Termlyx database", from.display()))?;
    // The temp file is now one self-contained file (journal mode DELETE).
    OpenOptions::new().write(true).open(tmp)?.sync_all()?;

    // Remove the old database's WAL/SHM *before* the swap: a stale WAL next to
    // a different database file could be replayed into it.
    let db = data_dir.join(DB_FILE);
    for suffix in ["-wal", "-shm"] {
        let side = data_dir.join(format!("{DB_FILE}{suffix}"));
        match fs::remove_file(&side) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("cannot remove {}", side.display())),
        }
    }
    fs::rename(tmp, &db).with_context(|| format!("cannot replace {}", db.display()))?;
    sync_dir(data_dir);
    Ok(epoch)
}

fn remove_with_sidecars(path: &Path) {
    remove_file_retrying(path);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let _ = fs::remove_file(PathBuf::from(name));
    }
}

/// Best-effort delete. On Windows a file SQLite has only just let go of can
/// stay locked for a moment (it closes on a worker thread), so try again for a
/// little while rather than leave a stale copy behind.
fn remove_file_retrying(path: &Path) {
    for _ in 0..40 {
        match fs::remove_file(path) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if cfg!(windows) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_) => return,
        }
    }
}

/// Open the copy at `path`, check it, and set `meta.epoch` to `epoch`.
async fn validate_and_stamp(path: &Path, epoch: &str) -> anyhow::Result<()> {
    let opts = SqliteConnectOptions::from_str("sqlite://")?
        .filename(path)
        .create_if_missing(false)
        // One self-contained file, whatever mode the backup was in.
        .journal_mode(SqliteJournalMode::Delete);
    let mut conn = SqliteConnection::connect_with(&opts)
        .await
        .context("not an SQLite database")?;
    let checked = check(&mut conn, epoch).await;
    let closed = conn.close().await;
    checked?;
    closed?;
    Ok(())
}

async fn check(conn: &mut SqliteConnection, epoch: &str) -> anyhow::Result<()> {
    let rows: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&mut *conn)
        .await
        .context("PRAGMA integrity_check failed")?;
    if rows != ["ok"] {
        bail!("integrity check failed: {}", rows.join("; "));
    }
    for table in REQUIRED_TABLES {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(&mut *conn)
        .await?;
        if n != 1 {
            bail!("it has no `{table}` table (not a Termlyx server database)");
        }
    }
    // The migration history must be one this build knows exactly: a database
    // from a newer server (or with a changed migration) would fail to start.
    let known = sqlx::migrate!("./migrations");
    let applied: Vec<(i64, Vec<u8>, bool)> =
        sqlx::query_as("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *conn)
            .await
            .context("it has no migration history")?;
    if applied.is_empty() {
        bail!("it has no migration history");
    }
    for (version, checksum, success) in applied {
        let Some(m) = known.iter().find(|m| m.version == version) else {
            bail!("it was made by a newer termlyx-server (migration {version}); update this one");
        };
        if !success {
            bail!("migration {version} did not finish in that database");
        }
        if m.checksum.as_ref() != checksum.as_slice() {
            bail!("migration {version} differs from this build's");
        }
    }
    // The point of a restore: a new identity, so clients detect the rewind.
    sqlx::query("INSERT OR REPLACE INTO meta (k, v) VALUES ('epoch', ?)")
        .bind(epoch.as_bytes().to_vec())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

fn check_tls_pems(files: &[(String, Vec<u8>)]) -> anyhow::Result<()> {
    for (name, bytes) in files {
        match name.as_str() {
            "key.pem" => {
                PrivateKeyDer::from_pem_slice(bytes).context("key.pem is not a PEM private key")?;
            }
            _ => {
                CertificateDer::from_pem_slice(bytes)
                    .context("cert.pem is not a PEM certificate")?;
            }
        }
    }
    Ok(())
}

// --------------------------------------------------------------------- tar

const BLOCK: usize = 512;

fn octal(field: &mut [u8], value: u64) {
    // Zero-padded octal filling the field up to a trailing NUL.
    let digits = field.len() - 1;
    let text = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(text.as_bytes());
    field[digits] = 0;
}

/// The header checksum: the sum of all bytes with the checksum field as spaces.
fn header_sum(h: &[u8]) -> u32 {
    h.iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u32::from(b)
            }
        })
        .sum()
}

/// A minimal POSIX ustar archive of regular files, mode 0600.
fn tar_encode(entries: &[(&str, Vec<u8>)]) -> anyhow::Result<Vec<u8>> {
    let mtime = crate::util::system_now_ms().max(0) as u64 / 1000;
    let mut out = vec![];
    for (name, bytes) in entries {
        if name.len() >= 100 || bytes.len() > MAX_TLS_FILE {
            bail!("{name} cannot go into the archive");
        }
        let mut h = [0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        octal(&mut h[100..108], 0o600);
        octal(&mut h[108..116], 0);
        octal(&mut h[116..124], 0);
        octal(&mut h[124..136], bytes.len() as u64);
        octal(&mut h[136..148], mtime);
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let text = format!("{:06o}\0 ", header_sum(&h));
        h[148..156].copy_from_slice(text.as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(bytes);
        out.resize(out.len().next_multiple_of(BLOCK), 0);
    }
    out.resize(out.len() + 2 * BLOCK, 0);
    Ok(out)
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(field).ok()?;
    let text = text.trim_matches(|c| c == '\0' || c == ' ');
    u64::from_str_radix(text, 8).ok()
}

/// Read what [`tar_encode`] wrote (and what `tar` makes of the same files):
/// regular files named exactly `key.pem` and `cert.pem`, nothing else. Both
/// must be present. Returns `(name, bytes)` in archive order.
fn tar_decode(data: &[u8]) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let mut files: Vec<(String, Vec<u8>)> = vec![];
    let mut pos = 0;
    loop {
        let Some(h) = data.get(pos..pos + BLOCK) else {
            bail!("the archive is truncated");
        };
        if h.iter().all(|&b| b == 0) {
            break;
        }
        let stored = parse_octal(&h[148..156]).context("bad header checksum field")?;
        if u64::from(header_sum(h)) != stored {
            bail!("the archive header is corrupt");
        }
        let name_end = h[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&h[..name_end]).context("bad file name")?;
        let name = name.strip_prefix("./").unwrap_or(name);
        if !matches!(h[156], b'0' | 0) {
            bail!("unsupported entry `{name}` (only plain files are allowed)");
        }
        if !TLS_FILES.contains(&name) {
            bail!("unexpected entry `{name}` (expected key.pem and cert.pem)");
        }
        if files.iter().any(|(n, _)| n == name) {
            bail!("`{name}` appears twice");
        }
        let size = parse_octal(&h[124..136]).context("bad size")? as usize;
        if size == 0 || size > MAX_TLS_FILE {
            bail!("`{name}` has an implausible size");
        }
        let start = pos + BLOCK;
        let Some(body) = data.get(start..start + size) else {
            bail!("the archive is truncated");
        };
        files.push((name.to_string(), body.to_vec()));
        pos = start + size.next_multiple_of(BLOCK);
    }
    for want in TLS_FILES {
        if !files.iter().any(|(n, _)| n == want) {
            bail!("the archive has no {want}");
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tar_round_trips_and_rejects_everything_else() {
        let key = b"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n".to_vec();
        let cert = vec![b'c'; 1500]; // spans several blocks
        let tar = tar_encode(&[("key.pem", key.clone()), ("cert.pem", cert.clone())]).unwrap();
        assert_eq!(tar.len() % BLOCK, 0);
        let files = tar_decode(&tar).unwrap();
        assert_eq!(files[0], ("key.pem".to_string(), key.clone()));
        assert_eq!(files[1], ("cert.pem".to_string(), cert.clone()));

        // A name outside the two files (path traversal included) is refused,
        // even with a valid checksum.
        for bad in ["../key.pem", "/etc/passwd", "other.pem", "tls/key.pem"] {
            let mut t = tar.clone();
            t[..100].fill(0);
            t[..bad.len()].copy_from_slice(bad.as_bytes());
            let text = format!("{:06o}\0 ", header_sum(&t[..BLOCK]));
            t[148..156].copy_from_slice(text.as_bytes());
            let err = tar_decode(&t).unwrap_err().to_string();
            assert!(err.contains("unexpected entry"), "{bad}: {err}");
        }
        // Corruption, truncation, a missing file, duplicates.
        let mut t = tar.clone();
        t[10] ^= 1;
        assert!(tar_decode(&t).is_err());
        assert!(tar_decode(&tar[..BLOCK + 10]).is_err());
        assert!(tar_decode(&tar[..tar.len() - 2 * BLOCK - 1]).is_err());
        assert!(tar_decode(&tar_encode(&[("key.pem", key.clone())]).unwrap()).is_err());
        assert!(
            tar_decode(&tar_encode(&[("key.pem", key.clone()), ("key.pem", key)]).unwrap())
                .is_err()
        );
        assert!(tar_decode(&[]).is_err());
    }

    #[test]
    fn tar_path_is_next_to_the_backup() {
        assert_eq!(
            tls_tar_path(Path::new("/b/tether.db")),
            Path::new("/b/tether.db.tls.tar")
        );
    }
}
