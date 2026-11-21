//! A high-level SFTP client on a shared [`SshConnection`].
//!
//! [`SftpClient`] runs `russh-sftp` over the connection's "sftp" subsystem
//! channel ([`SshConnection::open_sftp`]), so file browsing and transfers
//! share the host's connection with its shells. The client is cheap to clone;
//! clones share the channel, and requests from several tasks are multiplexed
//! on it.
//!
//! Paths are SFTP paths (POSIX style, `/`-separated) and travel verbatim as
//! strings: nothing is quoted, split or expanded, so spaces, quotes, newlines
//! and non-ASCII names round-trip. Names the server sends that are not valid
//! UTF-8 are decoded lossily by `russh-sftp`.
//!
//! Server data is never trusted to be well-formed: missing attributes read as
//! zero, and directory entries whose name is empty, `.`, `..` or contains a
//! `/` are dropped (with a warning) so that a hostile server cannot make a
//! caller build a path outside the listed directory.

use std::io::{self, SeekFrom};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::fs::{File, Metadata};
use russh_sftp::client::{Config, SftpSession};
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use tokio::io::{AsyncRead, AsyncSeek, AsyncSeekExt, AsyncWrite, ReadBuf};
use tracing::warn;

use crate::connection::SshConnection;
use crate::error::SshError;

/// Seconds a single SFTP request may wait for its reply.
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// Writes in flight per open file (each up to `WRITE_PACKET` bytes).
const CONCURRENT_WRITES: usize = 32;
/// Preferred size of one SFTP write request on the wire.
const WRITE_PACKET: u32 = 64 * 1024;

const S_IFMT: u32 = 0o170_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFLNK: u32 = 0o120_000;

/// One remote file or directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    /// The last path component.
    pub name: String,
    /// The full path, as given (for [`SftpClient::stat`]) or as the listed
    /// directory joined with `name` (for [`SftpClient::list_dir`]).
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Modification time, unix seconds; 0 if the server did not say.
    pub mtime: i64,
    /// `st_mode` as reported by the server (file type and permission bits);
    /// 0 if the server did not say.
    pub mode: u32,
    /// The entry itself is a symbolic link.
    pub is_symlink: bool,
}

impl RemoteEntry {
    fn new(name: String, path: String, attrs: &Metadata, is_symlink: bool) -> Self {
        let mode = attrs.permissions.unwrap_or(0);
        Self {
            name,
            path,
            is_dir: mode & S_IFMT == S_IFDIR,
            size: attrs.size.unwrap_or(0),
            mtime: attrs.mtime.map_or(0, i64::from),
            mode,
            is_symlink,
        }
    }
}

/// SFTP on one channel of a shared connection; see the module docs.
#[derive(Clone)]
pub struct SftpClient {
    session: Arc<SftpSession>,
}

impl std::fmt::Debug for SftpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpClient").finish_non_exhaustive()
    }
}

impl SftpClient {
    /// Opens the "sftp" subsystem on `conn` and starts the SFTP session.
    pub async fn open(conn: &SshConnection) -> Result<Self, SshError> {
        let channel = conn.open_sftp().await?;
        let cfg = Config {
            request_timeout_secs: REQUEST_TIMEOUT_SECS,
            max_concurrent_writes: CONCURRENT_WRITES,
            max_write_packet_len: WRITE_PACKET,
            ..Config::default()
        };
        let session = SftpSession::new_with_config(channel.into_stream(), cfg)
            .await
            .map_err(|e| SshError::Sftp(format!("starting the sftp session: {}", describe(&e))))?;
        Ok(Self {
            session: Arc::new(session),
        })
    }

    /// Lists a directory (without `.` and `..`), sorted by name. Symbolic
    /// links are reported as links (`is_symlink`), with the link's own
    /// attributes.
    pub async fn list_dir(&self, path: &str) -> Result<Vec<RemoteEntry>, SshError> {
        let entries = self
            .session
            .read_dir(path)
            .await
            .map_err(|e| op_err("list", path, &e))?;
        let mut out: Vec<RemoteEntry> = entries
            .filter_map(|entry| {
                let name = entry.file_name();
                if name.is_empty() || name == "." || name == ".." || name.contains('/') {
                    warn!(dir = %path, name = ?name, "ignoring an invalid name in a directory listing");
                    return None;
                }
                let attrs = entry.metadata();
                let is_symlink = attrs.permissions.unwrap_or(0) & S_IFMT == S_IFLNK;
                let full = join(path, &name);
                Some(RemoteEntry::new(name, full, &attrs, is_symlink))
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Stats `path`. A symbolic link is reported with `is_symlink` set and
    /// its target's attributes (the link's own if the target is missing).
    pub async fn stat(&self, path: &str) -> Result<RemoteEntry, SshError> {
        let lattrs = self
            .session
            .symlink_metadata(path)
            .await
            .map_err(|e| op_err("stat", path, &e))?;
        Ok(self.entry_from_lstat(path, lattrs).await)
    }

    /// Like [`stat`](Self::stat), but a path that does not exist is
    /// `Ok(None)`.
    pub async fn try_stat(&self, path: &str) -> Result<Option<RemoteEntry>, SshError> {
        match self.session.symlink_metadata(path).await {
            Ok(lattrs) => Ok(Some(self.entry_from_lstat(path, lattrs).await)),
            Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => Ok(None),
            Err(e) => Err(op_err("stat", path, &e)),
        }
    }

    /// The entry for `path` from its lstat attributes; a symlink's target is
    /// stat'ed for its attributes (the link's own are kept if that fails).
    async fn entry_from_lstat(&self, path: &str, lattrs: Metadata) -> RemoteEntry {
        let is_symlink = lattrs.permissions.unwrap_or(0) & S_IFMT == S_IFLNK;
        let attrs = if is_symlink {
            self.session.metadata(path).await.unwrap_or(lattrs)
        } else {
            lattrs
        };
        RemoteEntry::new(
            base_name(path).to_owned(),
            path.to_owned(),
            &attrs,
            is_symlink,
        )
    }

    /// The server's canonical absolute form of `path` (`.` is the login
    /// directory; `..` and symbolic links are resolved).
    pub async fn realpath(&self, path: &str) -> Result<String, SshError> {
        self.session
            .canonicalize(path)
            .await
            .map_err(|e| op_err("realpath", path, &e))
    }

    /// Creates one directory (its parent must exist).
    pub async fn mkdir(&self, path: &str) -> Result<(), SshError> {
        self.session
            .create_dir(path)
            .await
            .map_err(|e| op_err("mkdir", path, &e))
    }

    pub async fn remove_file(&self, path: &str) -> Result<(), SshError> {
        self.session
            .remove_file(path)
            .await
            .map_err(|e| op_err("remove", path, &e))
    }

    /// Removes an empty directory.
    pub async fn remove_dir(&self, path: &str) -> Result<(), SshError> {
        self.session
            .remove_dir(path)
            .await
            .map_err(|e| op_err("rmdir", path, &e))
    }

    /// Renames `from` to `to`. With SFTP v3 (OpenSSH) this fails if `to`
    /// exists.
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), SshError> {
        self.session
            .rename(from, to)
            .await
            .map_err(|e| SshError::Sftp(format!("rename {from:?} to {to:?}: {}", describe(&e))))
    }

    /// Sets the permission bits (`mode & 0o7777`).
    pub async fn chmod(&self, path: &str, mode: u32) -> Result<(), SshError> {
        let attrs = FileAttributes {
            permissions: Some(mode & 0o7777),
            ..FileAttributes::empty()
        };
        self.session
            .set_metadata(path, attrs)
            .await
            .map_err(|e| op_err("chmod", path, &e))
    }

    /// Opens a file for reading, positioned at `offset`.
    pub async fn open_read(&self, path: &str, offset: u64) -> Result<RemoteFile, SshError> {
        let file = self
            .session
            .open_with_flags(path, OpenFlags::READ)
            .await
            .map_err(|e| op_err("open", path, &e))?;
        RemoteFile::at(file, path, offset).await
    }

    /// Opens a file for writing (created if missing), positioned at
    /// `offset`; `truncate` empties it first.
    pub async fn open_write(
        &self,
        path: &str,
        offset: u64,
        truncate: bool,
    ) -> Result<RemoteFile, SshError> {
        let mut flags = OpenFlags::WRITE | OpenFlags::CREATE;
        if truncate {
            flags |= OpenFlags::TRUNCATE;
        }
        let file = self
            .session
            .open_with_flags(path, flags)
            .await
            .map_err(|e| op_err("open", path, &e))?;
        RemoteFile::at(file, path, offset).await
    }

    /// Opens an existing file for writing at `offset`, without truncating;
    /// fails if `path` does not exist (so a resumed write never recreates a
    /// vanished file and writes after a gap).
    pub async fn resume_write(&self, path: &str, offset: u64) -> Result<RemoteFile, SshError> {
        let file = self
            .session
            .open_with_flags(path, OpenFlags::WRITE)
            .await
            .map_err(|e| op_err("open", path, &e))?;
        RemoteFile::at(file, path, offset).await
    }

    /// Upper bound on the bytes of writes one [`RemoteFile`] has in flight
    /// (sent, not yet acknowledged). If a transfer stops abnormally, the
    /// server may have applied a later write but not an earlier one, so only
    /// the data below `size - write_window()` of the destination is proven.
    pub fn write_window(&self) -> u64 {
        CONCURRENT_WRITES as u64 * u64::from(WRITE_PACKET)
    }

    /// Creates a new file for writing; fails if `path` already exists.
    pub async fn create_new(&self, path: &str) -> Result<RemoteFile, SshError> {
        let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE;
        let file = self
            .session
            .open_with_flags(path, flags)
            .await
            .map_err(|e| op_err("create", path, &e))?;
        Ok(RemoteFile { file })
    }
}

/// An open remote file: `AsyncRead` (pipelined reads), `AsyncSeek` and
/// `AsyncWrite` (pipelined writes, acknowledged by [`close`](Self::close) at
/// the latest).
/// Dropping it closes the handle without waiting for pending replies, so a
/// write error after the last `write` is only reported by `close`.
pub struct RemoteFile {
    file: File,
}

impl std::fmt::Debug for RemoteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteFile").finish_non_exhaustive()
    }
}

impl RemoteFile {
    async fn at(mut file: File, path: &str, offset: u64) -> Result<Self, SshError> {
        if offset > 0 {
            file.seek(SeekFrom::Start(offset))
                .await
                .map_err(|e| SshError::Sftp(format!("seek {path:?}: {e}")))?;
        }
        Ok(Self { file })
    }

    /// Waits for every pending write to be acknowledged, then closes the
    /// handle. Returns the first write error, if any.
    pub async fn close(self) -> Result<(), SshError> {
        self.file
            .close()
            .await
            .map_err(|e| SshError::Sftp(format!("close: {e}")))
    }
}

impl AsyncRead for RemoteFile {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().file).poll_read(cx, buf)
    }
}

/// Seeking drops read-ahead; `SeekFrom::End` costs a round trip.
impl AsyncSeek for RemoteFile {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        Pin::new(&mut self.get_mut().file).start_seek(position)
    }

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Pin::new(&mut self.get_mut().file).poll_complete(cx)
    }
}

impl AsyncWrite for RemoteFile {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().file).poll_write(cx, buf)
    }

    /// Waits for pending writes (and fsyncs when the server supports it).
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().file).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().file).poll_shutdown(cx)
    }
}

/// `dir` joined with `name` by one `/`.
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The last component of `path` (trailing slashes ignored); `/` for the
/// root.
fn base_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() { "" } else { "/" };
    }
    match trimmed.rfind('/') {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    }
}

fn describe(e: &SftpError) -> String {
    match e {
        SftpError::Status(s) if !s.error_message.is_empty() => s.error_message.clone(),
        SftpError::Status(s) => s.status_code.to_string(),
        SftpError::Timeout => "no reply from the server (timed out)".to_owned(),
        other => other.to_string(),
    }
}

fn op_err(op: &str, path: &str, e: &SftpError) -> SshError {
    SshError::Sftp(format!("{op} {path:?}: {}", describe(e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_name_and_join() {
        assert_eq!(base_name("/a/b c/d.txt"), "d.txt");
        assert_eq!(base_name("/a/dir/"), "dir");
        assert_eq!(base_name("/"), "/");
        assert_eq!(base_name("rel"), "rel");
        assert_eq!(base_name("line\nbreak"), "line\nbreak");
        assert_eq!(join("/", "x"), "/x");
        assert_eq!(join("/a", "x y"), "/a/x y");
        assert_eq!(join("", "x"), "x");
    }
}
