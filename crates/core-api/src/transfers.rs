//! The transfer queue: uploads and downloads of single files over an
//! [`SftpClient`], with progress, cancel, resume and conflict handling.
//!
//! * Jobs run **one at a time**, in enqueue order. They all share one SFTP
//!   channel, whose throughput is the limit, so parallel jobs would only
//!   interleave; it also keeps conflict prompts to one at a time.
//! * Data moves in [`CHUNK_SIZE`] chunks. Remote reads and writes are
//!   pipelined by the SFTP client (several requests in flight).
//! * [`TransferEvent::Progress`] is sent at most every [`PROGRESS_INTERVAL`]
//!   per job, plus one when the job starts and a final one before `Done`.
//! * [`TransferQueue::cancel`] stops a job at the next chunk boundary (or
//!   at once while it waits on I/O or a prompt). The partial destination is
//!   kept as a contiguous prefix of the source, so
//!   [`ConflictPolicy::Resume`] can continue it.
//! * When the destination exists, the job's [`ConflictPolicy`] decides; `Ask`
//!   asks the user through the [`PromptBridge`]
//!   ([`Prompt::TransferConflict`]).
//!
//! Resume trusts the existing destination to be a prefix of the source (as
//! `sftp reput`/`reget` do): it only compares sizes. Downloads write the
//! local file in order, so a local partial file is an exact prefix and a
//! download resumes at its size. Uploads pipeline up to
//! [`SftpClient::write_window`] bytes of writes; if an earlier attempt
//! failed or the connection dropped, the server may have applied a later
//! write but lost an earlier one, so the remote size can run past a hole. A
//! resumed upload therefore restarts one write window below the remote size
//! (or from 0) and re-sends that range, which is idempotent; this also
//! applies when the remote file is already as large as the source.
//!
//! A job whose source turns out shorter or longer than its size at the start
//! fails ("source changed during transfer") instead of reporting `Done`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use core_ssh::{RemoteFile, SftpClient};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::prompts::{Prompt, PromptBridge, PromptResponse};

/// Bytes moved per read/write step.
pub const CHUNK_SIZE: usize = 256 * 1024;
/// Minimum time between two progress events of one job.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// How long a cancelled upload waits for its pending writes to be
/// acknowledged before the handle is dropped anyway.
const CANCEL_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on `name (n).ext` candidates tried by `Rename`.
const MAX_RENAME_ATTEMPTS: u32 = 10_000;

/// What to do when the destination already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictPolicy {
    /// Truncate and rewrite it.
    Overwrite,
    /// Leave it alone; the job ends with [`TransferEvent::Skipped`].
    Skip,
    /// Write to the first free `name (1).ext`, `name (2).ext`, ... instead.
    Rename,
    /// Continue from its size if it is smaller than the source; if it is not,
    /// the job counts as already done (nothing is truncated).
    Resume,
    /// Ask the user ([`Prompt::TransferConflict`]). An unanswered prompt, a
    /// rejection or an `Ask` answer means `Skip`.
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    /// `local` → `remote`.
    Upload,
    /// `remote` → `local`.
    Download,
}

/// One file to move. `remote` is an SFTP path, used verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferJob {
    pub direction: Direction,
    pub local: PathBuf,
    pub remote: String,
    pub policy: ConflictPolicy,
}

/// Identifies a job (a UUIDv7 string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(String);

impl JobId {
    fn new() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for JobId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// What the queue reports. Every job ends with exactly one of `Done`,
/// `Skipped`, `Cancelled` or `Failed`. Serializes as
/// `{"kind": "progress", "job": "...", ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TransferEvent {
    /// `bytes` of the destination are written (including a resumed prefix)
    /// out of `total` (the source size).
    Progress {
        job: JobId,
        bytes: u64,
        total: u64,
    },
    /// Finished; `dest` is where the data went (it differs from the job's
    /// destination after a `Rename`). Local paths are shown lossily.
    Done {
        job: JobId,
        dest: String,
    },
    /// The destination existed and the policy (or the user) chose to skip.
    Skipped {
        job: JobId,
    },
    /// Cancelled; a partial destination is left in place.
    Cancelled {
        job: JobId,
    },
    Failed {
        job: JobId,
        error: String,
    },
}

/// Runs transfer jobs on one SFTP client; see the module docs. Dropping the
/// queue cancels its running and queued jobs (each still gets its
/// `Cancelled` event).
pub struct TransferQueue {
    jobs: mpsc::UnboundedSender<(JobId, TransferJob)>,
    shared: Arc<Shared>,
}

impl fmt::Debug for TransferQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransferQueue")
            .field("active", &self.active_jobs().len())
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Shared {
    /// Queued and running jobs; `true` once cancelled.
    cancels: Mutex<HashMap<JobId, watch::Sender<bool>>>,
}

impl Shared {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<JobId, watch::Sender<bool>>> {
        self.cancels.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl TransferQueue {
    /// Starts the queue's worker task (needs a tokio runtime). Conflict
    /// prompts go through `prompts`. Events arrive on the returned receiver;
    /// it is unbounded, but progress is throttled, so it stays small.
    pub fn new(
        client: SftpClient,
        prompts: Arc<PromptBridge>,
    ) -> (Self, mpsc::UnboundedReceiver<TransferEvent>) {
        let (jobs, rx) = mpsc::unbounded_channel();
        let (events_tx, events) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared::default());
        tokio::spawn(worker(client, prompts, rx, shared.clone(), events_tx));
        (Self { jobs, shared }, events)
    }

    /// Queues `job` behind the ones already queued.
    pub fn enqueue(&self, job: TransferJob) -> JobId {
        let id = JobId::new();
        let (cancel, _) = watch::channel(false);
        self.shared.map().insert(id.clone(), cancel);
        if self.jobs.send((id.clone(), job)).is_err() {
            // Only if the worker is gone, which it never is while we exist.
            warn!(job = %id, "transfer worker is gone; job dropped");
            self.shared.map().remove(&id);
        }
        id
    }

    /// Cancels a queued or running job. Returns `false` if it is unknown or
    /// already finished.
    pub fn cancel(&self, job: &JobId) -> bool {
        match self.shared.map().get(job) {
            Some(cancel) => {
                cancel.send_replace(true);
                true
            }
            None => false,
        }
    }

    /// Jobs queued or running, in no particular order (for example for a
    /// "transfers still running" confirmation on quit).
    pub fn active_jobs(&self) -> Vec<JobId> {
        self.shared.map().keys().cloned().collect()
    }
}

impl Drop for TransferQueue {
    fn drop(&mut self) {
        for cancel in self.shared.map().values() {
            cancel.send_replace(true);
        }
    }
}

async fn worker(
    client: SftpClient,
    prompts: Arc<PromptBridge>,
    mut jobs: mpsc::UnboundedReceiver<(JobId, TransferJob)>,
    shared: Arc<Shared>,
    events: mpsc::UnboundedSender<TransferEvent>,
) {
    while let Some((id, job)) = jobs.recv().await {
        let cancel = shared.map().get(&id).map(|tx| tx.subscribe());
        let end = match cancel {
            Some(cancel) if !*cancel.borrow() => {
                let mut ctx = JobCtx {
                    client: &client,
                    prompts: &prompts,
                    id: &id,
                    cancel,
                    events: &events,
                };
                match run_job(&mut ctx, &job).await {
                    Ok(Outcome::Done(dest)) => TransferEvent::Done {
                        job: id.clone(),
                        dest,
                    },
                    Ok(Outcome::Skipped) => TransferEvent::Skipped { job: id.clone() },
                    Ok(Outcome::Cancelled) => TransferEvent::Cancelled { job: id.clone() },
                    Err(error) => {
                        debug!(job = %id, %error, "transfer failed");
                        TransferEvent::Failed {
                            job: id.clone(),
                            error,
                        }
                    }
                }
            }
            _ => TransferEvent::Cancelled { job: id.clone() },
        };
        // Forgotten before the final event, so `active_jobs` agrees with it.
        shared.map().remove(&id);
        let _ = events.send(end);
    }
}

struct JobCtx<'a> {
    client: &'a SftpClient,
    prompts: &'a PromptBridge,
    id: &'a JobId,
    cancel: watch::Receiver<bool>,
    events: &'a mpsc::UnboundedSender<TransferEvent>,
}

impl JobCtx<'_> {
    /// Resolves once the job is cancelled.
    async fn cancelled(&mut self) {
        if self.cancel.wait_for(|c| *c).await.is_err() {
            // The queue keeps the sender until the job ends.
            std::future::pending::<()>().await;
        }
    }

    fn is_cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    fn progress(&self, bytes: u64, total: u64) {
        let _ = self.events.send(TransferEvent::Progress {
            job: self.id.clone(),
            bytes,
            total: total.max(bytes),
        });
    }
}

enum Outcome {
    Done(String),
    Skipped,
    Cancelled,
}

/// How to write the destination.
enum Plan {
    /// It does not exist: create it (never clobbering one that appears in
    /// the meantime).
    Create,
    Overwrite,
    /// Resume over an existing destination of this size; each direction
    /// decides where to restart.
    Resume(u64),
    Rename,
    Skip,
    Cancelled,
}

/// Applies the conflict policy (asking if needed). `existing` is the
/// destination's size if it exists.
async fn plan(
    ctx: &mut JobCtx<'_>,
    policy: ConflictPolicy,
    dest: &str,
    existing: Option<u64>,
    total: u64,
) -> Plan {
    let Some(existing) = existing else {
        return Plan::Create;
    };
    let policy = match policy {
        ConflictPolicy::Ask => {
            let question = Prompt::TransferConflict {
                job_id: ctx.id.to_string(),
                path: dest.to_owned(),
                existing_size: existing,
                incoming_size: total,
            };
            let prompts = ctx.prompts;
            let answer = tokio::select! {
                answer = prompts.ask(question) => answer,
                () = ctx.cancelled() => return Plan::Cancelled,
            };
            match answer {
                PromptResponse::Conflict(p) if p != ConflictPolicy::Ask => p,
                _ => ConflictPolicy::Skip,
            }
        }
        p => p,
    };
    match policy {
        ConflictPolicy::Overwrite => Plan::Overwrite,
        ConflictPolicy::Skip | ConflictPolicy::Ask => Plan::Skip,
        ConflictPolicy::Rename => Plan::Rename,
        ConflictPolicy::Resume => Plan::Resume(existing),
    }
}

async fn run_job(ctx: &mut JobCtx<'_>, job: &TransferJob) -> Result<Outcome, String> {
    match job.direction {
        Direction::Upload => upload(ctx, job).await,
        Direction::Download => download(ctx, job).await,
    }
}

async fn upload(ctx: &mut JobCtx<'_>, job: &TransferJob) -> Result<Outcome, String> {
    let local = &job.local;
    // The source is opened before anything is created at the destination.
    let mut src = tokio::fs::File::open(local)
        .await
        .map_err(|e| format!("{}: {e}", local.display()))?;
    let meta = src
        .metadata()
        .await
        .map_err(|e| format!("{}: {e}", local.display()))?;
    if !meta.is_file() {
        return Err(format!("{}: not a regular file", local.display()));
    }
    let total = meta.len();
    let client = ctx.client;

    let mut dest = job.remote.clone();
    let existing = client.try_stat(&dest).await.map_err(|e| e.to_string())?;
    if let Some(e) = &existing
        && e.is_dir
    {
        return Err(format!("{dest:?} is a directory"));
    }
    let existing = existing.map(|e| e.size);

    let (mut remote, start) = match plan(ctx, job.policy, &dest, existing, total).await {
        Plan::Create => (create_remote(client, &dest).await?, 0),
        Plan::Overwrite => (open_remote(client, &dest, 0, true).await?, 0),
        Plan::Resume(existing) => {
            match upload_resume_start(existing, total, client.write_window()) {
                Some(start) => {
                    let file = client
                        .resume_write(&dest, start)
                        .await
                        .map_err(|e| e.to_string())?;
                    (file, start)
                }
                None => {
                    ctx.progress(total, total);
                    return Ok(Outcome::Done(dest));
                }
            }
        }
        Plan::Rename => {
            let (renamed, file) = create_renamed_remote(client, &dest).await?;
            dest = renamed;
            (file, 0)
        }
        Plan::Skip => return Ok(Outcome::Skipped),
        Plan::Cancelled => return Ok(Outcome::Cancelled),
    };

    if start > 0
        && let Err(e) = src.seek(SeekFrom::Start(start)).await
    {
        let _ = remote.close().await;
        return Err(format!("{}: {e}", local.display()));
    }
    match copy(ctx, &mut src, &mut remote, start, total).await {
        Ok(Copied::All(end)) => {
            remote
                .close()
                .await
                .map_err(|e| format!("upload to {dest:?}: {e}"))?;
            if end != total {
                return Err(source_changed(&local.display().to_string(), end, total));
            }
            // Every write was acknowledged; check the result's size too.
            let size = client.stat(&dest).await.map_err(|e| e.to_string())?.size;
            if size != end {
                return Err(format!(
                    "upload to {dest:?}: the server has {size} bytes, expected {end}"
                ));
            }
            ctx.progress(end, total);
            Ok(Outcome::Done(dest))
        }
        Ok(Copied::Cancelled) => {
            // Let the writes already sent land, so the partial file is a
            // clean prefix when this returns.
            match tokio::time::timeout(CANCEL_CLOSE_TIMEOUT, remote.close()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => debug!(job = %ctx.id, error = %e, "closing a cancelled upload"),
                Err(_) => debug!(job = %ctx.id, "closing a cancelled upload timed out"),
            }
            Ok(Outcome::Cancelled)
        }
        Err(e) => {
            // Best effort; a resumed upload re-sends the write window anyway.
            let _ = tokio::time::timeout(CANCEL_CLOSE_TIMEOUT, remote.close()).await;
            Err(format!("upload to {dest:?}: {e}"))
        }
    }
}

async fn download(ctx: &mut JobCtx<'_>, job: &TransferJob) -> Result<Outcome, String> {
    let client = ctx.client;
    let source = client.stat(&job.remote).await.map_err(|e| e.to_string())?;
    if source.is_dir {
        return Err(format!("{:?} is a directory", job.remote));
    }
    let total = source.size;
    // The source is opened before anything is created at the destination.
    let mut remote = client
        .open_read(&job.remote, 0)
        .await
        .map_err(|e| e.to_string())?;

    let mut dest = job.local.clone();
    let existing = match tokio::fs::metadata(&dest).await {
        Ok(m) if m.is_dir() => return Err(format!("{} is a directory", dest.display())),
        Ok(m) => Some(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", dest.display())),
    };
    let shown = dest.to_string_lossy().into_owned();

    let (mut local, start) = match plan(ctx, job.policy, &shown, existing, total).await {
        Plan::Create => (open_local_write(&dest, LocalOpen::CreateNew).await?, 0),
        Plan::Overwrite => (open_local_write(&dest, LocalOpen::Truncate).await?, 0),
        // The local file was written in order: all of it is proven.
        Plan::Resume(existing) if existing < total => (
            open_local_write(&dest, LocalOpen::At(existing)).await?,
            existing,
        ),
        Plan::Resume(_) => {
            ctx.progress(total, total);
            return Ok(Outcome::Done(shown));
        }
        Plan::Rename => {
            let (renamed, file) = create_renamed_local(&dest).await?;
            dest = renamed;
            (file, 0)
        }
        Plan::Skip => return Ok(Outcome::Skipped),
        Plan::Cancelled => return Ok(Outcome::Cancelled),
    };
    let shown = dest.to_string_lossy().into_owned();

    if start > 0
        && let Err(e) = remote.seek(SeekFrom::Start(start)).await
    {
        return Err(format!("{:?}: {e}", job.remote));
    }
    let copied = copy(ctx, &mut remote, &mut local, start, total).await;
    // Whatever happened, what was written reaches the file before we report.
    let flushed = local.flush().await;
    drop(local);
    let _ = remote.close().await;
    match copied {
        Ok(Copied::All(end)) => {
            flushed.map_err(|e| format!("{shown}: {e}"))?;
            if end != total {
                return Err(source_changed(&format!("{:?}", job.remote), end, total));
            }
            let size = tokio::fs::metadata(&dest)
                .await
                .map_err(|e| format!("{shown}: {e}"))?
                .len();
            if size != end {
                return Err(format!("{shown}: has {size} bytes, expected {end}"));
            }
            ctx.progress(end, total);
            Ok(Outcome::Done(shown))
        }
        Ok(Copied::Cancelled) => Ok(Outcome::Cancelled),
        Err(e) => Err(format!("download of {:?}: {e}", job.remote)),
    }
}

enum Copied {
    /// Reached the source's end; the destination now ends at this offset.
    All(u64),
    Cancelled,
}

/// Copies `src` to `dst` in [`CHUNK_SIZE`] steps; both are positioned at
/// `pos`. Cancel takes effect between steps or while one waits; an
/// interrupted step leaves a prefix of its chunk written.
async fn copy<R, W>(
    ctx: &mut JobCtx<'_>,
    src: &mut R,
    dst: &mut W,
    mut pos: u64,
    total: u64,
) -> Result<Copied, String>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; CHUNK_SIZE];
    ctx.progress(pos, total);
    let mut last = Instant::now();
    loop {
        if ctx.is_cancelled() {
            return Ok(Copied::Cancelled);
        }
        let step = async {
            let n = src.read(&mut buf).await.map_err(|e| format!("read: {e}"))?;
            if n > 0 {
                dst.write_all(&buf[..n])
                    .await
                    .map_err(|e| format!("write: {e}"))?;
            }
            Ok::<usize, String>(n)
        };
        let n = tokio::select! {
            biased;
            () = ctx.cancelled() => return Ok(Copied::Cancelled),
            n = step => n?,
        };
        if n == 0 {
            return Ok(Copied::All(pos));
        }
        pos += n as u64;
        if last.elapsed() >= PROGRESS_INTERVAL {
            ctx.progress(pos, total);
            last = Instant::now();
        }
    }
}

async fn create_remote(client: &SftpClient, path: &str) -> Result<RemoteFile, String> {
    client.create_new(path).await.map_err(|e| e.to_string())
}

async fn open_remote(
    client: &SftpClient,
    path: &str,
    offset: u64,
    truncate: bool,
) -> Result<RemoteFile, String> {
    client
        .open_write(path, offset, truncate)
        .await
        .map_err(|e| e.to_string())
}

/// Where a resumed upload restarts over a remote file of `existing` bytes
/// (see the module docs): one write `window` below it, so that a hole left
/// by a lost in-flight write is re-sent. `None` (nothing to do) only when
/// the remote file is larger than the source, so it cannot be a prefix of
/// it and is left alone.
fn upload_resume_start(existing: u64, total: u64, window: u64) -> Option<u64> {
    (existing <= total).then(|| existing.saturating_sub(window))
}

fn source_changed(source: &str, read: u64, expected: u64) -> String {
    format!("source changed during transfer: read {read} bytes of {source}, expected {expected}")
}

enum LocalOpen {
    CreateNew,
    Truncate,
    At(u64),
}

async fn open_local_write(path: &Path, how: LocalOpen) -> Result<tokio::fs::File, String> {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true);
    match how {
        LocalOpen::CreateNew => opts.create_new(true),
        LocalOpen::Truncate => opts.create(true).truncate(true),
        LocalOpen::At(_) => &mut opts,
    };
    let mut f = opts
        .open(path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if let LocalOpen::At(offset) = how {
        f.seek(SeekFrom::Start(offset))
            .await
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(f)
}

/// `name (n).ext` for a file name: the extension is what follows the last
/// dot, unless that dot starts the name (`.bashrc` → `.bashrc (1)`).
fn numbered_name(name: &str, n: u32) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} ({n}){}", &name[..i], &name[i..]),
        _ => format!("{name} ({n})"),
    }
}

/// Creates the first free `name (n).ext` next to `path`. A name taken
/// between the check and the (exclusive) create moves on to the next number.
async fn create_renamed_remote(
    client: &SftpClient,
    path: &str,
) -> Result<(String, RemoteFile), String> {
    let (dir, name) = match path.rfind('/') {
        Some(i) => path.split_at(i + 1),
        None => ("", path),
    };
    if name.is_empty() {
        return Err(format!("{path:?} has no file name"));
    }
    let taken = |candidate: String| async move {
        client
            .try_stat(&candidate)
            .await
            .map(|e| e.is_some())
            .map_err(|e| e.to_string())
    };
    for n in 1..=MAX_RENAME_ATTEMPTS {
        let candidate = format!("{dir}{}", numbered_name(name, n));
        if taken(candidate.clone()).await? {
            continue;
        }
        match client.create_new(&candidate).await {
            Ok(file) => return Ok((candidate, file)),
            // SFTP v3 has no "already exists" status, so look again.
            Err(_) if taken(candidate.clone()).await? => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(format!("no free name for {path:?}"))
}

/// The local twin of [`numbered_name`], on `OsStr` so that no name is
/// mangled (`Path::file_stem` and `extension` split the same way).
fn numbered_path(path: &Path, n: u32) -> Option<PathBuf> {
    let stem = path.file_stem()?;
    let mut name = OsString::from(stem);
    name.push(format!(" ({n})"));
    if let Some(ext) = path.extension() {
        name.push(".");
        name.push(ext);
    }
    Some(path.with_file_name(name))
}

/// Creates the first free `name (n).ext` next to `path` (exclusively, so a
/// name taken in the meantime moves on to the next number).
async fn create_renamed_local(path: &Path) -> Result<(PathBuf, tokio::fs::File), String> {
    for n in 1..=MAX_RENAME_ATTEMPTS {
        let candidate =
            numbered_path(path, n).ok_or_else(|| format!("{} has no file name", path.display()))?;
        let opened = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
            .await;
        match opened {
            Ok(file) => return Ok((candidate, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", candidate.display())),
        }
    }
    Err(format!("no free name for {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_names() {
        assert_eq!(numbered_name("file.txt", 1), "file (1).txt");
        assert_eq!(numbered_name("archive.tar.gz", 2), "archive.tar (2).gz");
        assert_eq!(numbered_name("README", 1), "README (1)");
        assert_eq!(numbered_name(".bashrc", 1), ".bashrc (1)");
        assert_eq!(numbered_name(".bashrc.bak", 1), ".bashrc (1).bak");
        assert_eq!(numbered_name("日本語 'q'.txt", 3), "日本語 'q' (3).txt");
        for name in [
            "file.txt",
            "archive.tar.gz",
            "README",
            ".bashrc",
            ".bashrc.bak",
            "x.",
        ] {
            let p = Path::new("/d").join(name);
            assert_eq!(
                numbered_path(&p, 4).unwrap(),
                Path::new("/d").join(numbered_name(name, 4)),
                "{name}"
            );
        }
    }

    #[test]
    fn upload_resume_backs_off_one_write_window() {
        const W: u64 = 2 * 1024 * 1024;
        // A partial file: restart one window below its size.
        assert_eq!(upload_resume_start(5 * W, 10 * W, W), Some(4 * W));
        assert_eq!(upload_resume_start(W + 1, 10 * W, W), Some(1));
        // Less than a window: from the start.
        assert_eq!(upload_resume_start(W - 1, 10 * W, W), Some(0));
        assert_eq!(upload_resume_start(0, 10 * W, W), Some(0));
        // As large as the source: its tail is still re-sent.
        assert_eq!(upload_resume_start(10 * W, 10 * W, W), Some(9 * W));
        assert_eq!(upload_resume_start(0, 0, W), Some(0));
        // Larger than the source: not a prefix, left alone.
        assert_eq!(upload_resume_start(10 * W + 1, 10 * W, W), None);
    }

    #[test]
    fn events_and_policies_serialize_camel_case() {
        let e = TransferEvent::Progress {
            job: JobId::from("j".to_owned()),
            bytes: 1,
            total: 2,
        };
        assert_eq!(
            serde_json::to_value(e).unwrap(),
            serde_json::json!({"kind": "progress", "job": "j", "bytes": 1, "total": 2})
        );
        let p: ConflictPolicy = serde_json::from_str(r#""resume""#).unwrap();
        assert_eq!(p, ConflictPolicy::Resume);
        let j: TransferJob = serde_json::from_value(serde_json::json!({
            "direction": "upload", "local": "/a b", "remote": "/r\nx", "policy": "ask"
        }))
        .unwrap();
        assert_eq!(j.remote, "/r\nx");
    }

    #[test]
    fn conflict_prompt_and_answer_serde() {
        let p = Prompt::TransferConflict {
            job_id: "j".into(),
            path: "/d/a b".into(),
            existing_size: 1,
            incoming_size: 2,
        };
        assert_eq!(
            serde_json::to_value(p).unwrap(),
            serde_json::json!({
                "kind": "transferConflict",
                "jobId": "j",
                "path": "/d/a b",
                "existingSize": 1,
                "incomingSize": 2
            })
        );
        let r: PromptResponse = serde_json::from_str(r#"{"conflict":"rename"}"#).unwrap();
        assert!(matches!(
            r,
            PromptResponse::Conflict(ConflictPolicy::Rename)
        ));
    }
}
