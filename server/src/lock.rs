//! The data directory lock: `{data}/server.lock`.
//!
//! A running server holds an exclusive OS advisory lock on this file for as
//! long as it lives (the OS drops it when the process exits, however it
//! exits), so `restore` can tell a running server from a stopped one, and two
//! servers cannot share a data directory. The file itself is never deleted: a
//! stale file with no lock on it blocks nobody.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// File name of the lock inside the data directory.
pub const LOCK_FILE: &str = "server.lock";

/// How long [`DataLock::acquire`] waits for a lock that looks held.
const GRACE: Duration = Duration::from_millis(300);

/// Why the lock could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another process holds it.
    #[error(
        "another termlyx-server is already running on {0} (it holds {0}/server.lock); stop it first"
    )]
    Held(PathBuf),
    /// The lock file could not be created or locked.
    #[error("cannot lock {path}: {reason}")]
    Io {
        /// The lock file.
        path: PathBuf,
        /// The underlying error.
        reason: String,
    },
}

/// An exclusive hold on a data directory; released when dropped.
#[derive(Debug)]
pub struct DataLock {
    _file: File,
}

impl DataLock {
    /// Take the lock of `data_dir`, creating the directory and the lock file
    /// if needed. A held lock is [`LockError::Held`], after a short grace
    /// ([`GRACE`]): a process that just stopped (or a child forked at that
    /// moment, which briefly inherits the descriptor) lets go within
    /// milliseconds, and a restart right after a stop must not fail on it.
    pub fn acquire(data_dir: &Path) -> Result<Self, LockError> {
        let path = data_dir.join(LOCK_FILE);
        let io = |e: std::io::Error| LockError::Io {
            path: path.clone(),
            reason: e.to_string(),
        };
        std::fs::create_dir_all(data_dir).map_err(io)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io)?;
        let deadline = Instant::now() + GRACE;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(LockError::Held(data_dir.to_path_buf()));
                }
                Err(TryLockError::Error(e)) => return Err(io(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_released_on_drop_and_stale_files_do_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let a = DataLock::acquire(dir.path()).unwrap();
        assert!(matches!(
            DataLock::acquire(dir.path()),
            Err(LockError::Held(_))
        ));
        drop(a);
        // The file stays; with no lock on it, it blocks nobody.
        assert!(dir.path().join(LOCK_FILE).exists());
        let _b = DataLock::acquire(dir.path()).unwrap();
        // A lock file nobody ever locked.
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join(LOCK_FILE), b"left by a crash").unwrap();
        let _c = DataLock::acquire(other.path()).unwrap();
    }
}
