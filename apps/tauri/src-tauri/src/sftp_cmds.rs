//! SFTP commands: one SFTP client (and transfer queue) per vault host, on
//! the host's shared connection. `sftp_open` starts it, `sftp_close` lets it
//! go (cancelling its transfers). Transfer progress goes out as `transfer`
//! events (`{hostId, event}`); a conflict with policy `ask` is a `prompt`.
//! Paths are SFTP paths, used verbatim (spaces, quotes, unicode intact).

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

use core_api::{TransferEvent, TransferJob, TransferQueue};
use core_model::{Host, Id};
use core_ssh::{NoProgress, RemoteEntry, SftpClient};
use serde::Serialize;
use tauri::{State, Webview};

use crate::state::{AppState, CmdError, SftpHost, TransferInfo, events, lock};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEntryDto {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Unix seconds; 0 if unknown.
    pub mtime: i64,
    /// `st_mode` (type and permission bits); 0 if unknown.
    pub mode: u32,
    pub is_symlink: bool,
}

impl From<RemoteEntry> for RemoteEntryDto {
    fn from(e: RemoteEntry) -> Self {
        Self {
            name: e.name,
            path: e.path,
            is_dir: e.is_dir,
            size: e.size,
            mtime: e.mtime,
            mode: e.mode,
            is_symlink: e.is_symlink,
        }
    }
}

/// One entry of a local directory, for the file manager's local pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalEntryDto {
    pub name: String,
    /// The listed directory joined with `name`.
    pub path: String,
    /// For a symbolic link: whether its target is a directory.
    pub is_dir: bool,
    pub size: u64,
    /// Unix seconds; 0 if unknown.
    pub mtime: i64,
    /// `st_mode` (type and permission bits); `None` where the platform has none.
    pub mode: Option<u32>,
    pub is_symlink: bool,
}

/// Where the local pane is confined, set once at start-up on mobile (iOS: the app's
/// Documents folder, which Files shows as "On My iPhone > Tether"). `None`
/// elsewhere: a desktop local pane reaches the whole file system.
static LOCAL_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Confines every local path (listing, folders, transfers, imports) to `root`
/// and makes `~` mean `root`. Called once; later calls are ignored.
#[cfg_attr(desktop, allow(dead_code))]
pub fn restrict_local_to(root: PathBuf) {
    // Real path, so `~` and the drive list agree with what `confine` returns.
    let _ = std::fs::create_dir_all(&root);
    let _ = LOCAL_ROOT.set(std::fs::canonicalize(&root).unwrap_or(root));
}

fn local_root() -> Option<&'static Path> {
    LOCAL_ROOT.get().map(PathBuf::as_path)
}

fn home_dir() -> Result<PathBuf, CmdError> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| CmdError::io("cannot find the home directory"))
}

/// `path` with a leading `~` (alone, or before a separator) expanded to
/// what `home` gives.
fn expand_tilde_to(
    home: impl FnOnce() -> Result<PathBuf, CmdError>,
    path: &str,
) -> Result<PathBuf, CmdError> {
    let rest = match path.strip_prefix('~') {
        Some("") => return home(),
        Some(rest) if rest.starts_with('/') || (cfg!(windows) && rest.starts_with('\\')) => rest,
        _ => return Ok(PathBuf::from(path)),
    };
    Ok(home()?.join(rest.trim_start_matches(['/', '\\'])))
}

/// `path` with a leading `~` expanded to the home directory.
fn expand_tilde(path: &str) -> Result<PathBuf, CmdError> {
    expand_tilde_to(home_dir, path)
}

/// `requested` lexically cleaned: `.` dropped, `..` removing the component
/// before it (at the root it stays at the root).
fn clean(requested: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in requested.components() {
        match part {
            Component::Prefix(_) | Component::RootDir => out.push(part.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

/// `path` with its existing part resolved through the file system (symlinks
/// followed) and the missing tail appended as is. `None` for a dangling
/// symlink or anything else that cannot be resolved.
fn resolve_existing(path: &Path) -> Option<PathBuf> {
    let mut tail = Vec::new();
    let mut head = path.to_path_buf();
    loop {
        match std::fs::symlink_metadata(&head) {
            Ok(_) => {
                let mut real = std::fs::canonicalize(&head).ok()?;
                real.extend(tail.iter().rev());
                return Some(real);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tail.push(head.file_name()?.to_owned());
                if !head.pop() {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
}

/// The real path of `requested` (relative paths start at `root`) if it is
/// `root` or inside it, else `None`. `..` is cleaned away first; the part
/// that exists is then canonicalised, so a symlink inside `root` that leads
/// out of it is refused, and so is a dangling one. Callers use the returned
/// path, never `requested`, so what was checked is what is touched.
pub fn confine(root: &Path, requested: &Path) -> Option<PathBuf> {
    let root = std::fs::canonicalize(root).ok()?;
    let joined = if requested.is_absolute() {
        clean(requested)
    } else {
        clean(&root.join(requested))
    };
    let real = resolve_existing(&joined)?;
    real.starts_with(&root).then_some(real)
}

/// The local path of a transfer job: `~` expanded and, where local paths are
/// confined, checked (a download target or upload source outside is refused).
fn job_local_path(root: Option<&Path>, local: &Path) -> Result<PathBuf, CmdError> {
    match (local.to_str(), root) {
        (Some(local), root) => scoped_path(root, local),
        (None, None) => Ok(local.to_path_buf()),
        (None, Some(_)) => Err(outside_folder()),
    }
}

fn outside_folder() -> CmdError {
    CmdError::new("outsideFolder", outside_folder_message(cfg!(target_os = "android")))
}

/// iOS shows Tether's folder in the Files app; Android words it as a place on the phone.
fn outside_folder_message(android: bool) -> &'static str {
    if android {
        "That is outside Termlyx's folder on this phone. Only files in that folder can be used here."
    } else {
        "That is outside Termlyx's folder in the Files app. Only files in that folder can be used here."
    }
}

/// The path a local command works on: `~` expanded (to `root` when confined,
/// else the home directory), and, when confined, checked with [`confine`].
fn scoped_path(root: Option<&Path>, path: &str) -> Result<PathBuf, CmdError> {
    let Some(root) = root else {
        return expand_tilde(path);
    };
    let expanded = expand_tilde_to(|| Ok(root.to_path_buf()), path)?;
    confine(root, &expanded).ok_or_else(outside_folder)
}

fn unix_secs(t: std::io::Result<std::time::SystemTime>) -> i64 {
    use std::time::UNIX_EPOCH;
    match t.map(|t| t.duration_since(UNIX_EPOCH)) {
        Ok(Ok(after)) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Ok(Err(before)) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.mode())
}

#[cfg(not(unix))]
fn mode_of(_: &std::fs::Metadata) -> Option<u32> {
    None
}

/// Lists a local directory: directories first, then by name (case
/// insensitive). A `~` prefix is expanded. Entries are read with `lstat`; a
/// symbolic link reports its target's attributes when the target exists (a
/// broken link keeps its own). Nothing is ever opened, so a FIFO or device
/// node in the way is listed, not read. A name that is not valid UTF-8
/// cannot round-trip as a string and is left out.
pub fn local_list_dir(path: &str) -> Result<Vec<LocalEntryDto>, CmdError> {
    list_in(local_root(), path)
}

fn list_in(root: Option<&Path>, path: &str) -> Result<Vec<LocalEntryDto>, CmdError> {
    let dir = scoped_path(root, path)?;
    let read = std::fs::read_dir(&dir)
        .map_err(|e| CmdError::io(format!("listing {}: {e}", dir.display())))?;
    let mut out = Vec::new();
    for entry in read {
        let entry = entry.map_err(|e| CmdError::io(format!("listing {}: {e}", dir.display())))?;
        let full = entry.path();
        let (Some(name), Some(path)) =
            (entry.file_name().to_str().map(str::to_owned), full.to_str())
        else {
            tracing::debug!(dir = %dir.display(), "skipping a name that is not valid UTF-8");
            continue;
        };
        let Ok(own) = entry.metadata() else {
            // Vanished between readdir and lstat.
            continue;
        };
        let is_symlink = own.file_type().is_symlink();
        let meta = if is_symlink {
            std::fs::metadata(&full).unwrap_or_else(|_| own.clone())
        } else {
            own
        };
        out.push(LocalEntryDto {
            name,
            path: path.to_owned(),
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            mtime: unix_secs(meta.modified()),
            mode: mode_of(&meta),
            is_symlink,
        });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// The absolute form of `path` for display and for jobs: a leading `~` is
/// expanded, nothing else is touched or required to exist.
pub fn local_resolve_path(path: &str) -> Result<String, CmdError> {
    resolve_in(local_root(), path)
}

fn resolve_in(root: Option<&Path>, path: &str) -> Result<String, CmdError> {
    let resolved = scoped_path(root, path)?;
    resolved
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| CmdError::io("the home directory path is not valid UTF-8"))
}

/// Creates a local directory and any missing parents; an existing
/// directory is not an error.
pub fn local_make_dir(path: &str) -> Result<(), CmdError> {
    make_dir_in(local_root(), path)
}

fn make_dir_in(root: Option<&Path>, path: &str) -> Result<(), CmdError> {
    let dir = scoped_path(root, path)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| CmdError::io(format!("creating {}: {e}", dir.display())))
}

/// A picker result as a path: a `file://` URL (what the iOS picker returns,
/// percent-encoded) or a plain path.
fn picked_path(picked: &str) -> Option<PathBuf> {
    let Some(rest) = picked.strip_prefix("file://") else {
        return Some(PathBuf::from(picked));
    };
    let path = match rest.strip_prefix("localhost") {
        Some(after) => after,
        None => rest,
    };
    if !path.starts_with('/') {
        return None;
    }
    let mut out = Vec::with_capacity(path.len());
    let mut bytes = path.bytes();
    while let Some(b) = bytes.next() {
        if b != b'%' {
            out.push(b);
            continue;
        }
        let hi = char::from(bytes.next()?).to_digit(16)?;
        let lo = char::from(bytes.next()?).to_digit(16)?;
        out.push(u8::try_from(hi * 16 + lo).ok()?);
    }
    String::from_utf8(out).ok().map(PathBuf::from)
}

/// `name` for the `n`th try at a free name: `report.txt`, `report 2.txt`,
/// `report 3.txt`...
fn numbered_name(name: &str, n: u32) -> String {
    if n <= 1 {
        return name.to_owned();
    }
    let path = Path::new(name);
    match (
        path.file_stem().and_then(|s| s.to_str()),
        path.extension().and_then(|e| e.to_str()),
    ) {
        (Some(stem), Some(ext)) => format!("{stem} {n}.{ext}"),
        _ => format!("{name} {n}"),
    }
}

/// Moves a file the system picker handed over into `root` (Tether's folder),
/// under a free name (`name 2.ext` if `name.ext` is taken), and returns the
/// new path. Only a regular file inside `scratch` (where the picker puts its
/// copies, the app's temporary folder) may be imported: nothing else of the
/// app, such as its vault, can be pulled into the folder Files shows. The
/// picker's copy is removed once imported.
fn import_file(root: &Path, scratch: &Path, picked: &str) -> Result<String, CmdError> {
    let source = picked_path(picked)
        .and_then(|p| confine(scratch, &p))
        .ok_or_else(|| CmdError::invalid("that is not a file the picker handed over"))?;
    let meta = std::fs::metadata(&source)
        .map_err(|e| CmdError::io(format!("reading {}: {e}", source.display())))?;
    if !meta.is_file() {
        return Err(CmdError::invalid("only files can be imported, not folders"));
    }
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| CmdError::invalid("the file name is not valid UTF-8"))?;
    let mut input = std::fs::File::open(&source)
        .map_err(|e| CmdError::io(format!("reading {}: {e}", source.display())))?;
    for n in 1..=10_000 {
        let dest = root.join(numbered_name(name, n));
        let mut out = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
        {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(CmdError::io(format!("creating {}: {e}", dest.display()))),
        };
        if let Err(e) = std::io::copy(&mut input, &mut out).and_then(|_| out.sync_all()) {
            drop(out);
            let _ = std::fs::remove_file(&dest);
            return Err(CmdError::io(format!("copying to {}: {e}", dest.display())));
        }
        let _ = std::fs::remove_file(&source);
        return dest
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| CmdError::io("the new path is not valid UTF-8"));
    }
    Err(CmdError::io("too many files with that name"))
}

/// What a path handed to the Android file plugin must be.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub enum Want {
    Dir,
    File,
}

/// The real path of an existing directory or file inside `root`, for the
/// Android plugin (it copies between the picker and this path, so it must
/// never be handed a path the webview chose freely).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn plugin_path_in(root: Option<&Path>, path: &str, want: Want) -> Result<String, CmdError> {
    let Some(root) = root else {
        return Err(CmdError::new("unsupported", "Only available on a phone or tablet"));
    };
    let real = scoped_path(Some(root), path)?;
    let meta = std::fs::metadata(&real).map_err(|e| CmdError::io(format!("reading {}: {e}", real.display())))?;
    match want {
        Want::Dir if !meta.is_dir() => return Err(CmdError::invalid("that is not a folder")),
        Want::File if !meta.is_file() => return Err(CmdError::invalid("only files can be saved, not folders")),
        _ => {}
    }
    real.to_str().map(str::to_owned).ok_or_else(|| CmdError::io("the path is not valid UTF-8"))
}

/// [`plugin_path_in`] for the local pane's root.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn plugin_path(path: &str, want: Want) -> Result<String, CmdError> {
    plugin_path_in(local_root(), path, want)
}

/// Drive roots (`C:\\`, `D:\\`...) for the bits of a `GetLogicalDrives` mask
/// (bit 0 is `A:`).
#[cfg(any(windows, test))]
fn drive_roots(mask: u32) -> Vec<String> {
    ('A'..='Z')
        .enumerate()
        .filter(|(bit, _)| mask & (1 << bit) != 0)
        .map(|(_, letter)| format!("{letter}:\\"))
        .collect()
}

/// The roots of the local file systems: drive roots on Windows, `/` elsewhere,
/// and the one folder Tether may use where that is confined (iOS).
pub fn local_drive_roots() -> Result<Vec<String>, CmdError> {
    match local_root() {
        Some(root) => Ok(vec![root.display().to_string()]),
        None => system_drive_roots(),
    }
}

#[cfg(windows)]
fn system_drive_roots() -> Result<Vec<String>, CmdError> {
    // SAFETY: `GetLogicalDrives` takes no arguments and only reads system state.
    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    if mask == 0 {
        return Err(CmdError::io(format!(
            "listing drives: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(drive_roots(mask))
}

#[cfg(not(windows))]
#[allow(clippy::unnecessary_wraps)] // same signature as the Windows one
fn system_drive_roots() -> Result<Vec<String>, CmdError> {
    Ok(vec!["/".to_owned()])
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnqueuedTransfer {
    pub job_id: String,
}

/// Payload of the `transfer` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferEventPayload {
    pub host_id: Id,
    pub event: TransferEvent,
}

fn is_final(event: &TransferEvent) -> Option<&str> {
    match event {
        TransferEvent::Progress { .. } => None,
        TransferEvent::Done { job, .. }
        | TransferEvent::Skipped { job }
        | TransferEvent::Cancelled { job }
        | TransferEvent::Failed { job, .. } => Some(job.as_str()),
    }
}

impl AppState {
    fn sftp_host(&self, host_id: Id) -> Option<Arc<SftpHost>> {
        lock(&self.0.sftp).get(&host_id).cloned()
    }

    fn sftp_client(&self, host_id: Id) -> Result<SftpClient, CmdError> {
        self.sftp_host(host_id)
            .map(|h| h.client.clone())
            .ok_or_else(|| CmdError::new("notOpen", "SFTP is not open for this host"))
    }

    /// Opens SFTP for a vault host (connecting if needed). A no-op while it
    /// is open on a live connection. Needs the vault unlocked, like opening
    /// a shell, even when the host's connection is already up.
    pub async fn sftp_open(&self, host_id: Id) -> Result<(), CmdError> {
        self.with_vault(move |v| match v.get::<Host>(host_id)? {
            Some(_) => Ok(()),
            None => Err(CmdError::not_found(format!("host {host_id}"))),
        })
        .await?;
        if self
            .sftp_host(host_id)
            .is_some_and(|h| !h.closed.is_closed())
        {
            return Ok(());
        }
        let (guard, conn) = self.host_connection(host_id, Arc::new(NoProgress)).await?;
        // Someone else may have opened it while we waited for the slot.
        if self
            .sftp_host(host_id)
            .is_some_and(|h| !h.closed.is_closed())
        {
            return Ok(());
        }
        let client = match SftpClient::open(&conn).await {
            Ok(client) => client,
            Err(e) => {
                drop(guard);
                // A connection made just for this must not stay pooled idle.
                self.release_if_idle(host_id).await;
                return Err(e.into());
            }
        };
        let (queue, mut events) = TransferQueue::new(client.clone(), self.prompts());
        let host = Arc::new(SftpHost {
            client,
            queue,
            closed: conn.close_signal(),
        });
        // A replaced entry (dead connection) drops its queue here, which
        // cancels its jobs; their events still arrive.
        lock(&self.0.sftp).insert(host_id, host);
        drop(guard);

        let weak = Arc::downgrade(&self.0);
        tauri::async_runtime::spawn(async move {
            // Ends when the queue's worker is gone (queue dropped).
            while let Some(event) = events.recv().await {
                let Some(inner) = weak.upgrade() else { break };
                let state = AppState(inner);
                if let Some(job) = is_final(&event) {
                    lock(&state.0.transfers).remove(job);
                }
                state.emit(events::TRANSFER, &TransferEventPayload { host_id, event });
            }
        });
        Ok(())
    }

    /// Closes SFTP for a host: queued and running transfers are cancelled,
    /// and the connection is released if nothing else uses it.
    pub async fn sftp_close(&self, host_id: Id) {
        let removed = lock(&self.0.sftp).remove(&host_id);
        drop(removed);
        self.release_if_idle(host_id).await;
    }

    pub async fn sftp_list(
        &self,
        host_id: Id,
        path: String,
    ) -> Result<Vec<RemoteEntryDto>, CmdError> {
        let entries = self.sftp_client(host_id)?.list_dir(&path).await?;
        Ok(entries.into_iter().map(RemoteEntryDto::from).collect())
    }

    pub async fn sftp_mkdir(&self, host_id: Id, path: String) -> Result<(), CmdError> {
        Ok(self.sftp_client(host_id)?.mkdir(&path).await?)
    }

    pub async fn sftp_rename(&self, host_id: Id, from: String, to: String) -> Result<(), CmdError> {
        Ok(self.sftp_client(host_id)?.rename(&from, &to).await?)
    }

    /// Removes a file, or an empty directory when `is_dir`.
    pub async fn sftp_remove(
        &self,
        host_id: Id,
        path: String,
        is_dir: bool,
    ) -> Result<(), CmdError> {
        let client = self.sftp_client(host_id)?;
        if is_dir {
            Ok(client.remove_dir(&path).await?)
        } else {
            Ok(client.remove_file(&path).await?)
        }
    }

    pub async fn sftp_chmod(&self, host_id: Id, path: String, mode: u32) -> Result<(), CmdError> {
        Ok(self.sftp_client(host_id)?.chmod(&path, mode).await?)
    }

    /// The server's canonical absolute path for `path` (`.` is the home
    /// directory); relative paths resolve against it.
    pub async fn sftp_realpath(&self, host_id: Id, path: String) -> Result<String, CmdError> {
        Ok(self.sftp_client(host_id)?.realpath(&path).await?)
    }

    /// Queues a transfer on the host's SFTP client for webview `owner`. A
    /// leading `~` of the local path is expanded here: the queue itself
    /// uses paths as they are. Where local paths are confined to a folder
    /// (iOS), the job's local path is replaced by its checked real path.
    pub fn transfer_enqueue(
        &self,
        owner: &str,
        host_id: Id,
        mut job: TransferJob,
    ) -> Result<EnqueuedTransfer, CmdError> {
        job.local = job_local_path(local_root(), &job.local)?;
        let host = self
            .sftp_host(host_id)
            .ok_or_else(|| CmdError::new("notOpen", "SFTP is not open for this host"))?;
        let info = TransferInfo {
            owner: owner.to_owned(),
            host_id,
            direction: job.direction,
            local: job.local.display().to_string(),
            remote: job.remote.clone(),
        };
        // Recorded under the lock the event pump takes to forget finished
        // jobs, so even a job that ends at once is forgotten.
        let mut transfers = lock(&self.0.transfers);
        let job_id = host.queue.enqueue(job).as_str().to_owned();
        transfers.insert(job_id.clone(), info);
        Ok(EnqueuedTransfer { job_id })
    }

    /// Cancels a queued or running transfer; `false` if it is unknown or
    /// already finished.
    pub fn transfer_cancel(&self, job_id: String) -> bool {
        let host = lock(&self.0.transfers).get(&job_id).map(|t| t.host_id);
        let job = core_api::JobId::from(job_id);
        match host.and_then(|h| self.sftp_host(h)) {
            Some(host) => host.queue.cancel(&job),
            None => false,
        }
    }
}

#[tauri::command]
pub async fn sftp_open(state: State<'_, AppState>, host_id: Id) -> Result<(), CmdError> {
    state.sftp_open(host_id).await
}

#[tauri::command]
pub async fn sftp_close(state: State<'_, AppState>, host_id: Id) -> Result<(), CmdError> {
    state.sftp_close(host_id).await;
    Ok(())
}

#[tauri::command]
pub async fn sftp_list(
    state: State<'_, AppState>,
    host_id: Id,
    path: String,
) -> Result<Vec<RemoteEntryDto>, CmdError> {
    state.sftp_list(host_id, path).await
}

#[tauri::command]
pub async fn sftp_mkdir(
    state: State<'_, AppState>,
    host_id: Id,
    path: String,
) -> Result<(), CmdError> {
    state.sftp_mkdir(host_id, path).await
}

#[tauri::command]
pub async fn sftp_rename(
    state: State<'_, AppState>,
    host_id: Id,
    from: String,
    to: String,
) -> Result<(), CmdError> {
    state.sftp_rename(host_id, from, to).await
}

#[tauri::command]
pub async fn sftp_remove(
    state: State<'_, AppState>,
    host_id: Id,
    path: String,
    is_dir: bool,
) -> Result<(), CmdError> {
    state.sftp_remove(host_id, path, is_dir).await
}

#[tauri::command]
pub async fn sftp_realpath(
    state: State<'_, AppState>,
    host_id: Id,
    path: String,
) -> Result<String, CmdError> {
    state.sftp_realpath(host_id, path).await
}

#[tauri::command]
pub async fn sftp_chmod(
    state: State<'_, AppState>,
    host_id: Id,
    path: String,
    mode: u32,
) -> Result<(), CmdError> {
    state.sftp_chmod(host_id, path, mode).await
}

#[tauri::command]
pub async fn transfer_enqueue(
    state: State<'_, AppState>,
    webview: Webview,
    host_id: Id,
    job: TransferJob,
) -> Result<EnqueuedTransfer, CmdError> {
    state.transfer_enqueue(webview.label(), host_id, job)
}

#[tauri::command]
pub async fn transfer_cancel(state: State<'_, AppState>, job_id: String) -> Result<bool, CmdError> {
    Ok(state.transfer_cancel(job_id))
}

/// Lists a local directory (see [`local_list_dir`]).
#[tauri::command]
pub async fn local_list(path: String) -> Result<Vec<LocalEntryDto>, CmdError> {
    tauri::async_runtime::spawn_blocking(move || local_list_dir(&path))
        .await
        .map_err(|e| CmdError::internal(e.to_string()))?
}

/// Expands a leading `~` of a local path (`~` alone, `~/x`); the shown
/// folder and every job path are built from the result.
#[tauri::command]
pub async fn local_resolve(path: String) -> Result<String, CmdError> {
    local_resolve_path(&path)
}

/// The roots of the local file systems (`C:\\`, `D:\\`... on Windows, `/`
/// elsewhere), for the local pane's "This PC" list.
#[tauri::command]
pub async fn local_drives() -> Result<Vec<String>, CmdError> {
    local_drive_roots()
}

/// Creates a local directory (and parents), for downloads into new folders.
#[tauri::command]
pub async fn local_mkdir(path: String) -> Result<(), CmdError> {
    tauri::async_runtime::spawn_blocking(move || local_make_dir(&path))
        .await
        .map_err(|e| CmdError::internal(e.to_string()))?
}

/// Moves a file chosen in the system file picker into Tether's folder (iOS),
/// returning its new path. `path` is what the picker gave: a `file://` URL
/// or a path.
#[tauri::command]
pub async fn local_import(path: String) -> Result<String, CmdError> {
    let Some(root) = local_root() else {
        return Err(CmdError::new(
            "unsupported",
            "Importing from Files is only available on iOS",
        ));
    };
    tauri::async_runtime::spawn_blocking(move || import_file(root, &std::env::temp_dir(), &path))
        .await
        .map_err(|e| CmdError::internal(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_list_sorts_dirs_first_and_keeps_odd_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Windows forbids quotes and newlines in file names.
        #[cfg(unix)]
        let (quoted, newline) = ("Äpfel \"quoted\" 日本語.txt", "line\nbreak");
        #[cfg(windows)]
        let (quoted, newline) = ("Äpfel ünï 日本語.txt", "semi;colon");
        std::fs::write(root.join("b file.txt"), b"hello").unwrap();
        std::fs::write(root.join(quoted), b"x").unwrap();
        std::fs::write(root.join(newline), b"").unwrap();
        std::fs::create_dir(root.join("zeta dir")).unwrap();
        std::fs::create_dir(root.join("alpha")).unwrap();
        let entries = local_list_dir(root.to_str().unwrap()).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "zeta dir", "b file.txt", newline, quoted]);
        let file = entries.iter().find(|e| e.name == "b file.txt").unwrap();
        assert!(!file.is_dir && !file.is_symlink);
        assert_eq!(file.size, 5);
        assert!(file.mtime > 0);
        assert!(entries[0].is_dir);
        // Paths are the directory joined with the name, verbatim.
        let odd = entries.iter().find(|e| e.name == newline).unwrap();
        assert_eq!(odd.path, root.join(newline).to_str().unwrap());
        assert!(std::fs::metadata(&odd.path).is_ok());
        #[cfg(unix)]
        assert!(file.mode.is_some_and(|m| m & 0o170_000 == 0o100_000));
    }

    #[cfg(unix)]
    #[test]
    fn local_list_reports_symlinks_and_lists_only_directories() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("real dir")).unwrap();
        std::fs::write(root.join("real.txt"), b"abc").unwrap();
        symlink(root.join("real dir"), root.join("link to dir")).unwrap();
        symlink(root.join("real.txt"), root.join("link to file")).unwrap();
        symlink(root.join("missing"), root.join("broken")).unwrap();
        let entries = local_list_dir(root.to_str().unwrap()).unwrap();
        let get = |n: &str| entries.iter().find(|e| e.name == n).unwrap();
        assert!(get("link to dir").is_symlink && get("link to dir").is_dir);
        assert!(get("link to file").is_symlink && !get("link to file").is_dir);
        assert_eq!(get("link to file").size, 3);
        assert!(get("broken").is_symlink && !get("broken").is_dir);
        // A link to a directory lists as that directory; a file does not list.
        let via = local_list_dir(root.join("link to dir").to_str().unwrap()).unwrap();
        assert!(via.is_empty());
        let err = local_list_dir(root.join("real.txt").to_str().unwrap()).unwrap_err();
        assert_eq!(err.code, "io");
    }

    #[test]
    fn local_list_errors_are_cmd_errors_and_tilde_expands() {
        let err = local_list_dir("/definitely/not/here").unwrap_err();
        assert_eq!(err.code, "io");
        assert!(
            err.message.contains("/definitely/not/here"),
            "{}",
            err.message
        );
        // `~` is the home directory (when the environment has one).
        if let Ok(home) = home_dir() {
            assert_eq!(expand_tilde("~").unwrap(), home);
            assert_eq!(expand_tilde("~/x y").unwrap(), home.join("x y"));
            assert_eq!(expand_tilde("~other").unwrap(), PathBuf::from("~other"));
            assert!(local_list_dir("~").is_ok());
        }
        assert_eq!(expand_tilde("/a/~/b").unwrap(), PathBuf::from("/a/~/b"));
    }

    /// A folder standing in for the app's Documents folder, and a sibling
    /// folder standing in for the rest of the device.
    fn documents() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let docs = std::fs::canonicalize(dir.path()).unwrap().join("Documents");
        let other = std::fs::canonicalize(dir.path()).unwrap().join("Library");
        std::fs::create_dir_all(docs.join("sub")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("secret"), b"vault").unwrap();
        (dir, docs, other)
    }

    #[test]
    fn ios_paths_stay_inside_documents() {
        let (_dir, docs, other) = documents();
        std::fs::write(docs.join("a.txt"), b"x").unwrap();

        // Inside: itself, children (existing or not), relative forms, `..` that stays in.
        assert_eq!(confine(&docs, &docs), Some(docs.clone()));
        assert_eq!(
            confine(&docs, &docs.join("a.txt")),
            Some(docs.join("a.txt"))
        );
        assert_eq!(
            confine(&docs, &docs.join("new/deep/b.txt")),
            Some(docs.join("new/deep/b.txt"))
        );
        assert_eq!(confine(&docs, Path::new("sub")), Some(docs.join("sub")));
        assert_eq!(confine(&docs, Path::new("")), Some(docs.clone()));
        assert_eq!(
            confine(&docs, &docs.join("sub/../a.txt")),
            Some(docs.join("a.txt"))
        );
        assert_eq!(
            confine(&docs, &docs.join("gone/../a.txt")),
            Some(docs.join("a.txt"))
        );

        // Outside: `..`, absolute paths, a sibling that shares the prefix, the root of the disk.
        assert_eq!(confine(&docs, Path::new("..")), None);
        assert_eq!(confine(&docs, Path::new("../Library/secret")), None);
        assert_eq!(confine(&docs, &docs.join("sub/../../Library/secret")), None);
        assert_eq!(confine(&docs, &docs.join("../Documents-evil")), None);
        assert_eq!(confine(&docs, &other.join("secret")), None);
        assert_eq!(confine(&docs, &other), None);
        let top = docs.ancestors().last().unwrap().to_path_buf();
        assert_eq!(confine(&docs, &top), None);
        // Excess `..` stops at the top, then is outside.
        assert_eq!(
            confine(&docs, Path::new("../../../../../../../../../../etc")),
            None
        );
        // A root that does not exist confines nothing.
        assert_eq!(
            confine(&docs.join("missing"), &docs.join("missing/x")),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn ios_symlinks_cannot_lead_out_of_documents() {
        use std::os::unix::fs::symlink;
        let (_dir, docs, other) = documents();
        std::fs::write(docs.join("a.txt"), b"x").unwrap();
        symlink(&other, docs.join("out dir")).unwrap();
        symlink(other.join("secret"), docs.join("out file")).unwrap();
        symlink(other.join("not there"), docs.join("dangling out")).unwrap();
        symlink(docs.join("nowhere"), docs.join("dangling in")).unwrap();
        symlink(docs.join("sub"), docs.join("in dir")).unwrap();
        symlink("../Library", docs.join("relative out")).unwrap();

        assert_eq!(confine(&docs, &docs.join("out dir")), None);
        assert_eq!(confine(&docs, &docs.join("out dir/secret")), None);
        assert_eq!(confine(&docs, &docs.join("out dir/new file")), None);
        assert_eq!(confine(&docs, &docs.join("out file")), None);
        assert_eq!(confine(&docs, &docs.join("dangling out")), None);
        assert_eq!(confine(&docs, &docs.join("dangling in")), None);
        assert_eq!(confine(&docs, &docs.join("relative out/secret")), None);
        // A link that stays inside resolves to its target.
        assert_eq!(confine(&docs, &docs.join("in dir")), Some(docs.join("sub")));
        assert_eq!(
            confine(&docs, &docs.join("in dir/new")),
            Some(docs.join("sub/new"))
        );
        // The root itself may be reached through a link (`/var` -> `/private/var` on iOS).
        symlink(&docs, other.join("docs link")).unwrap();
        assert_eq!(
            confine(&other.join("docs link"), &other.join("docs link/a.txt")),
            Some(docs.join("a.txt"))
        );
    }

    #[test]
    fn ios_local_commands_refuse_paths_outside_documents() {
        let (_dir, docs, other) = documents();
        std::fs::write(docs.join("a.txt"), b"x").unwrap();
        let root = Some(docs.as_path());

        // `~` is the folder; the listing and resolving work from it.
        assert_eq!(resolve_in(root, "~").unwrap(), docs.to_str().unwrap());
        assert_eq!(
            resolve_in(root, "~/sub/../a.txt").unwrap(),
            docs.join("a.txt").to_str().unwrap()
        );
        let names: Vec<_> = list_in(root, "~")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["sub", "a.txt"]);
        make_dir_in(root, "~/made/deeper").unwrap();
        assert!(docs.join("made/deeper").is_dir());

        // Anything else is refused, with a code the UI can show.
        let secret = other.join("secret");
        let outside = [
            "/",
            "/etc",
            "~/..",
            "~/../Library",
            "../Library",
            other.to_str().unwrap(),
            secret.to_str().unwrap(),
        ];
        for path in outside {
            assert_eq!(
                list_in(root, path).unwrap_err().code,
                "outsideFolder",
                "{path}"
            );
            assert_eq!(
                resolve_in(root, path).unwrap_err().code,
                "outsideFolder",
                "{path}"
            );
            assert_eq!(
                make_dir_in(root, path).unwrap_err().code,
                "outsideFolder",
                "{path}"
            );
        }
        assert!(!other.join("made").exists());
    }

    #[test]
    fn the_outside_folder_message_names_no_device_or_arrow() {
        // It is shown on iPhone and iPad alike, so it follows neither.
        let message = outside_folder().message;
        assert!(
            !message.contains("iPhone") && !message.contains("iPad"),
            "{message}"
        );
        assert!(
            !message.contains('>') && !message.contains('›'),
            "{message}"
        );
        assert!(
            message.contains(&format!("{}'s folder", crate::APP_NAME)),
            "{message}"
        );
    }

    #[test]
    fn the_outside_folder_message_says_files_app_only_on_ios() {
        assert!(outside_folder_message(false).contains("Files app"));
        let android = outside_folder_message(true);
        assert!(!android.contains("Files app") && android.contains("on this phone"), "{android}");
    }

    #[test]
    fn ios_transfer_jobs_must_stay_inside_documents() {
        let (_dir, docs, other) = documents();
        let root = Some(docs.as_path());

        // A download target that does not exist yet, an upload source that does.
        let target = job_local_path(root, &docs.join("sub/new.bin")).unwrap();
        assert_eq!(target, docs.join("sub/new.bin"));
        assert_eq!(
            job_local_path(root, Path::new("~/new.bin")).unwrap(),
            docs.join("new.bin")
        );
        // A target or source outside is refused, however it is spelt.
        for bad in [
            other.join("secret"),
            other.join("planted"),
            docs.join("../Library/planted"),
            PathBuf::from("~/../Library/planted"),
            PathBuf::from("/etc/passwd"),
        ] {
            assert_eq!(
                job_local_path(root, &bad).unwrap_err().code,
                "outsideFolder",
                "{bad:?}"
            );
        }
        // Without confinement (desktop) a job path is only tilde-expanded.
        assert_eq!(
            job_local_path(None, Path::new("/etc/passwd")).unwrap(),
            Path::new("/etc/passwd")
        );
    }

    #[test]
    fn picker_results_are_urls_or_paths() {
        assert_eq!(picked_path("/tmp/a b"), Some(PathBuf::from("/tmp/a b")));
        assert_eq!(
            picked_path("file:///tmp/a%20b/c.txt"),
            Some(PathBuf::from("/tmp/a b/c.txt"))
        );
        assert_eq!(
            picked_path("file://localhost/tmp/x"),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(
            picked_path("file:///t/%E6%97%A5%E6%9C%AC.txt"),
            Some(PathBuf::from("/t/日本.txt"))
        );
        assert_eq!(
            picked_path("file:///t/100%25"),
            Some(PathBuf::from("/t/100%"))
        );
        assert_eq!(picked_path("file://elsewhere/tmp/x"), None);
        assert_eq!(picked_path("file:///t/%zz"), None);
        assert_eq!(picked_path("file:///t/%4"), None);
        assert_eq!(picked_path("file:///t/%FF"), None);
    }

    #[test]
    fn plugin_paths_are_confined_and_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap().join("files");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"x").unwrap();
        let outside = tmp.path().join("secret.txt");
        std::fs::write(&outside, b"x").unwrap();
        let ok = |p: &str, w| plugin_path_in(Some(&root), p, w);
        assert_eq!(ok("~", Want::Dir).unwrap(), root.to_str().unwrap());
        assert_eq!(ok("~/sub", Want::Dir).unwrap(), root.join("sub").to_str().unwrap());
        assert_eq!(ok("~/a.txt", Want::File).unwrap(), root.join("a.txt").to_str().unwrap());
        // Wrong kind, missing, and anything outside the root.
        assert_eq!(ok("~/a.txt", Want::Dir).unwrap_err().code, "invalid");
        assert_eq!(ok("~/sub", Want::File).unwrap_err().code, "invalid");
        assert_eq!(ok("~/gone.txt", Want::File).unwrap_err().code, "io");
        assert_eq!(ok("~/../secret.txt", Want::File).unwrap_err().code, "outsideFolder");
        assert_eq!(ok(outside.to_str().unwrap(), Want::File).unwrap_err().code, "outsideFolder");
        assert_eq!(ok("/", Want::Dir).unwrap_err().code, "outsideFolder");
        // No root (desktop): refused.
        assert_eq!(plugin_path_in(None, "/tmp", Want::Dir).unwrap_err().code, "unsupported");
    }

    #[test]
    fn clashing_import_names_get_a_number_before_the_extension() {
        assert_eq!(numbered_name("report.txt", 1), "report.txt");
        assert_eq!(numbered_name("report.txt", 2), "report 2.txt");
        assert_eq!(numbered_name("a.tar.gz", 3), "a.tar 3.gz");
        assert_eq!(numbered_name("README", 2), "README 2");
        assert_eq!(numbered_name(".profile", 2), ".profile 2");
    }

    #[test]
    fn import_moves_a_picked_file_into_the_folder() {
        let (_dir, docs, other) = documents();
        let scratch = tempfile::tempdir().unwrap();
        let tmp = std::fs::canonicalize(scratch.path()).unwrap();
        std::fs::create_dir_all(tmp.join("app-Inbox")).unwrap();
        let picked = tmp.join("app-Inbox/my notes.txt");
        std::fs::write(&picked, b"one").unwrap();

        // A file:// URL, as the iOS picker gives it (Windows paths are not URL paths: plain there).
        let first = if cfg!(unix) {
            format!("file://{}", picked.to_str().unwrap().replace(' ', "%20"))
        } else {
            picked.to_str().unwrap().to_owned()
        };
        let got = import_file(&docs, &tmp, &first).unwrap();
        assert_eq!(got, docs.join("my notes.txt").to_str().unwrap());
        assert_eq!(std::fs::read(&got).unwrap(), b"one");
        assert!(!picked.exists(), "the picker's copy is removed");

        // The same name again: " 2", then " 3". Existing files are never overwritten.
        for (n, body) in [(2, "two"), (3, "three")] {
            std::fs::write(&picked, body).unwrap();
            let got = import_file(&docs, &tmp, picked.to_str().unwrap()).unwrap();
            assert_eq!(
                got,
                docs.join(format!("my notes {n}.txt")).to_str().unwrap()
            );
            assert_eq!(std::fs::read_to_string(&got).unwrap(), body);
        }
        assert_eq!(std::fs::read(docs.join("my notes.txt")).unwrap(), b"one");

        // Not a picker file: outside the scratch folder (the rest of the app), a folder, nothing.
        for bad in [
            other.join("secret"),
            tmp.join("app-Inbox"),
            tmp.join("app-Inbox/gone.txt"),
            tmp.join("../x"),
        ] {
            let err = import_file(&docs, &tmp, bad.to_str().unwrap()).unwrap_err();
            assert!(
                err.code == "invalid" || err.code == "io",
                "{bad:?}: {err:?}"
            );
        }
        assert_eq!(std::fs::read(other.join("secret")).unwrap(), b"vault");
        assert!(
            std::fs::read_dir(&docs).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .contains("secret"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn import_refuses_a_link_out_of_the_scratch_folder() {
        let (_dir, docs, other) = documents();
        let scratch = tempfile::tempdir().unwrap();
        let tmp = std::fs::canonicalize(scratch.path()).unwrap();
        std::os::unix::fs::symlink(other.join("secret"), tmp.join("link")).unwrap();
        let err = import_file(&docs, &tmp, tmp.join("link").to_str().unwrap()).unwrap_err();
        assert_eq!(err.code, "invalid");
        assert!(other.join("secret").exists());
        assert!(
            std::fs::read_dir(&docs)
                .unwrap()
                .all(|e| e.unwrap().file_name() == "sub")
        );
    }

    #[test]
    fn local_resolve_expands_only_a_leading_tilde() {
        if let Ok(home) = home_dir() {
            let home = home.to_str().unwrap().to_owned();
            assert_eq!(local_resolve_path("~").unwrap(), home);
            let sub = local_resolve_path("~/Downloads/a b").unwrap();
            assert!(
                sub.starts_with(&home) && sub.ends_with("Downloads/a b"),
                "{sub}"
            );
            assert!(!sub.contains('~'));
        }
        assert_eq!(local_resolve_path("/x/~y").unwrap(), "/x/~y");
        assert_eq!(local_resolve_path("~user/x").unwrap(), "~user/x");
        assert_eq!(local_resolve_path("rel/path").unwrap(), "rel/path");
    }

    #[test]
    fn local_mkdir_creates_parents_and_tolerates_existing() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let odd = "c\"d";
        #[cfg(windows)]
        let odd = "c;d";
        let target = dir.path().join("a b").join(odd);
        local_make_dir(target.to_str().unwrap()).unwrap();
        assert!(target.is_dir());
        local_make_dir(target.to_str().unwrap()).unwrap();
    }

    #[test]
    fn drive_roots_follow_the_logical_drives_mask() {
        assert_eq!(drive_roots(0), Vec::<String>::new());
        // Bit 0 is A:, bit 2 is C:, bit 25 is Z:.
        assert_eq!(drive_roots(0b100), ["C:\\"]);
        assert_eq!(
            drive_roots(1 << 2 | 1 << 3 | 1 << 25),
            ["C:\\", "D:\\", "Z:\\"]
        );
        // Bits above Z: are not drives.
        assert_eq!(drive_roots(1 << 26), Vec::<String>::new());
    }

    #[cfg(unix)]
    #[test]
    fn local_drives_is_the_root_off_windows() {
        assert_eq!(local_drive_roots().unwrap(), ["/"]);
    }

    #[cfg(windows)]
    #[test]
    fn local_drives_lists_c() {
        let drives = local_drive_roots().unwrap();
        assert!(drives.contains(&"C:\\".to_owned()), "{drives:?}");
        // Every root lists as a path the listing accepts.
        assert!(drives.iter().all(|d| d.len() == 3 && d.ends_with(":\\")));
    }

    #[test]
    fn local_list_handles_unicode_home() {
        // A user folder with a space and a non-ASCII letter, like `C:\Users\Zoë Lee`.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("Zoë Lee");
        let docs = home.join("Dokumente für mich");
        local_make_dir(docs.to_str().unwrap()).unwrap();
        std::fs::write(home.join("日本語 notes.txt"), b"hello").unwrap();
        let entries = local_list_dir(home.to_str().unwrap()).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Dokumente für mich", "日本語 notes.txt"]);
        let file = &entries[1];
        assert_eq!(file.path, home.join("日本語 notes.txt").to_str().unwrap());
        assert_eq!(file.size, 5);
        assert!(entries[0].is_dir && entries[0].path == docs.to_str().unwrap());
        // A resolved path comes back unchanged, and lists again.
        let resolved = local_resolve_path(home.to_str().unwrap()).unwrap();
        assert_eq!(resolved, home.to_str().unwrap());
        assert_eq!(local_list_dir(&entries[0].path).unwrap(), []);
    }
}
