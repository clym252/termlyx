use std::collections::HashMap;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use core_ssh::{
    AuthPrompter, ConnectParams, HostKeyVerifier, NoPrompter, ShellControl, ShellEnd, ShellHandle,
    ShellOptions, SshConnection, SshError, SshTransport,
};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};

use crate::batcher::Batcher;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use crate::local_pty::LocalShellTransport;
use crate::{FRAME_INTERVAL, UNACKED_HIGH_WATERMARK};

/// UUID v7, as a string.
pub type SessionId = String;

/// Upper bound on how long [`SessionManager::close`] waits for the remote
/// close request to be queued. The control channel can be full (for example
/// when the remote window is exhausted), so this must never wait unboundedly.
const CLOSE_REQUEST_TIMEOUT: Duration = Duration::from_millis(250);

/// What a session runs.
pub enum ShellTarget {
    /// A shell on a remote host over SSH. `prompter` is asked for secrets the
    /// auth method does not carry.
    Ssh {
        params: ConnectParams,
        prompter: Arc<dyn AuthPrompter>,
    },
    /// A shell channel on an already open SSH connection (which other
    /// channels may share). Closing the session closes this channel only.
    /// `env` is set on the channel; `startup`, when set, runs instead of the
    /// login shell (see [`ShellOptions`]).
    Connection {
        conn: SshConnection,
        term: String,
        cols: u16,
        rows: u16,
        env: Vec<(String, String)>,
        startup: Option<String>,
    },
    /// A shell on a local pseudo-terminal. `shell` defaults to the user's
    /// shell (`$SHELL`, ...); `cwd` to the app's working directory. Not on
    /// iOS or Android.
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    Local {
        shell: Option<PathBuf>,
        /// Arguments for an explicit `shell` (ignored for the default one).
        args: Vec<String>,
        cwd: Option<PathBuf>,
        cols: u16,
        rows: u16,
    },
}

/// Receives batched terminal output. Called from the session task, so
/// implementations must not block.
pub trait OutputSink: Send + Sync + 'static {
    /// A batch of raw output bytes (at most `MAX_FRAME_BYTES`).
    fn frame(&self, bytes: Vec<u8>);
    /// The session ended. Called exactly once, after any final
    /// [`OutputSink::frame`] call.
    ///
    /// `reason` is `None` when the shell ended normally (the remote shell
    /// exited, or the session was closed locally), with the remote exit
    /// status in `exit_status` when the server reported one. It is
    /// `Some(why)` when the session was lost: a transport error, a
    /// disconnect, or a failure of the session task itself.
    ///
    /// When the remote side ends the shell (EOF), all pending output has been
    /// delivered first. When the session is ended locally via
    /// [`SessionManager::close`], output still buffered (not yet emitted as a
    /// frame, or not yet read from the transport) is discarded.
    fn closed(&self, reason: Option<String>, exit_status: Option<u32>);
}

struct Session {
    control: ShellControl,
    unacked: Arc<AtomicUsize>,
    notify: Arc<Notify>,
    cancel: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

type Sessions = Mutex<HashMap<SessionId, Session>>;

pub struct SessionManager {
    transport: Arc<dyn SshTransport>,
    sessions: Arc<Sessions>,
}

impl SessionManager {
    pub fn new(t: Arc<dyn SshTransport>) -> Self {
        Self {
            transport: t,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Opens a session on `target` and starts delivering its output to `sink`.
    ///
    /// `verifier` checks SSH host keys; local and existing-connection targets
    /// ignore it. Local spawn
    /// failures are reported as [`SshError::Channel`].
    pub async fn open(
        &self,
        target: ShellTarget,
        verifier: Arc<dyn HostKeyVerifier>,
        sink: Arc<dyn OutputSink>,
    ) -> Result<SessionId, SshError> {
        let handle = match target {
            ShellTarget::Ssh { params, prompter } => {
                self.transport
                    .open_shell_with_prompter(params, verifier, prompter)
                    .await?
            }
            ShellTarget::Connection {
                conn,
                term,
                cols,
                rows,
                env,
                startup,
            } => {
                conn.open_shell_with(ShellOptions {
                    term: &term,
                    cols,
                    rows,
                    env: &env,
                    startup: startup.as_deref(),
                })
                .await?
            }
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            ShellTarget::Local {
                shell,
                args,
                cwd,
                cols,
                rows,
            } => LocalShellTransport::open(shell, args, cwd, cols, rows).await?,
        };
        Ok(self.register(handle, sink))
    }

    /// An SSH shell with no one to ask for missing secrets; a thin wrapper
    /// around [`open`](Self::open).
    pub async fn open_shell(
        &self,
        p: ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
        sink: Arc<dyn OutputSink>,
    ) -> Result<SessionId, SshError> {
        self.open_shell_with_prompter(p, v, Arc::new(NoPrompter), sink)
            .await
    }

    /// Like [`open_shell`](Self::open_shell), asking `prompter` for secrets
    /// the auth method does not carry.
    pub async fn open_shell_with_prompter(
        &self,
        p: ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
        prompter: Arc<dyn AuthPrompter>,
        sink: Arc<dyn OutputSink>,
    ) -> Result<SessionId, SshError> {
        self.open(
            ShellTarget::Ssh {
                params: p,
                prompter,
            },
            v,
            sink,
        )
        .await
    }

    fn register(&self, handle: ShellHandle, sink: Arc<dyn OutputSink>) -> SessionId {
        let id = uuid::Uuid::now_v7().to_string();
        let unacked = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(Notify::new());
        let (cancel, cancel_rx) = oneshot::channel();

        // Hold the lock across spawn+insert so a task that ends immediately
        // cannot try to deregister itself before it is registered.
        let mut sessions = self.sessions.lock().unwrap();
        let task = tokio::spawn(run_session(
            id.clone(),
            (handle.output, handle.end),
            sink,
            unacked.clone(),
            notify.clone(),
            cancel_rx,
            Arc::downgrade(&self.sessions),
        ));
        sessions.insert(
            id.clone(),
            Session {
                control: handle.control,
                unacked,
                notify,
                cancel,
                task,
            },
        );
        id
    }

    fn control(&self, id: &str) -> Result<ShellControl, SshError> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.control.clone())
            .ok_or(SshError::Closed)
    }

    pub async fn write(&self, id: &str, data: Vec<u8>) -> Result<(), SshError> {
        self.control(id)?.write(data.into()).await
    }

    pub async fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), SshError> {
        self.control(id)?.resize(cols, rows).await
    }

    /// The UI has consumed `bytes` of previously delivered output. Unknown ids
    /// are ignored.
    pub fn ack(&self, id: &str, bytes: usize) {
        if let Some(s) = self.sessions.lock().unwrap().get(id) {
            let _ = s
                .unacked
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    Some(n.saturating_sub(bytes))
                });
            s.notify.notify_one();
        }
    }

    /// Close a session. Never blocks unboundedly: the remote close request is
    /// given a short grace period, then the session task is cancelled, which
    /// drops the output receiver (tearing the shell down). The sink has
    /// received `closed` by the time this returns. Unknown ids are a no-op.
    pub async fn close(&self, id: &str) {
        let Some(session) = self.sessions.lock().unwrap().remove(id) else {
            return;
        };
        let _ = timeout(CLOSE_REQUEST_TIMEOUT, session.control.close()).await;
        let _ = session.cancel.send(());
        let _ = session.task.await;
    }
}

/// Reason handed to [`OutputSink::closed`] when the session task panicked
/// (in the sink's `frame` or in the batcher).
const PANIC_REASON: &str = "session task panicked";
/// Reason when the loop task was cancelled (runtime shutdown) rather than
/// panicking.
const ABORT_REASON: &str = "session task aborted";

/// Runs the session loop in its own task so that a panic (in the sink's
/// `frame`, or in the batcher) is contained: the session is still
/// deregistered and the sink still hears `closed`, exactly once.
async fn run_session(
    id: SessionId,
    (output, mut end): (mpsc::Receiver<bytes::Bytes>, oneshot::Receiver<ShellEnd>),
    sink: Arc<dyn OutputSink>,
    unacked: Arc<AtomicUsize>,
    notify: Arc<Notify>,
    cancel: oneshot::Receiver<()>,
    sessions: Weak<Sessions>,
) {
    let looped = tokio::spawn(session_loop(output, sink.clone(), unacked, notify, cancel));
    let (reason, exit_status) = match looped.await {
        // The shell reports its end before its output closes. Nothing
        // reported (or a local close) is a normal end.
        Ok(LoopEnd::OutputClosed) => match end.try_recv() {
            Ok(ShellEnd::Lost(why)) => (Some(why), None),
            Ok(ShellEnd::Exited { exit_status }) => (None, exit_status),
            Err(_) => (None, None),
        },
        Ok(LoopEnd::Cancelled) => (None, None),
        Err(e) => {
            // The loop's state (including the output receiver) was dropped
            // while unwinding. Never log the payload: it may be sink data.
            tracing::error!(session = %id, panicked = e.is_panic(), "session task failed");
            let why = if e.is_panic() {
                PANIC_REASON
            } else {
                ABORT_REASON
            };
            (Some(why.to_string()), None)
        }
    };

    // Deregister first so the session is gone by the time the sink hears of it.
    if let Some(sessions) = sessions.upgrade() {
        sessions.lock().unwrap().remove(&id);
    }
    // A panicking `closed` must not take the runtime worker's task down with
    // an unhandled panic; there is no one left to tell.
    let notified = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sink.closed(reason, exit_status);
    }));
    if notified.is_err() {
        tracing::error!(session = %id, "output sink panicked in closed");
    }
}

/// Why [`session_loop`] returned.
enum LoopEnd {
    /// The shell's output closed: it ended on its own.
    OutputClosed,
    /// [`SessionManager::close`] ended it.
    Cancelled,
}

async fn session_loop(
    mut output: mpsc::Receiver<bytes::Bytes>,
    sink: Arc<dyn OutputSink>,
    unacked: Arc<AtomicUsize>,
    notify: Arc<Notify>,
    mut cancel: oneshot::Receiver<()>,
) -> LoopEnd {
    let mut batcher = Batcher::new();
    // When the buffered bytes must be flushed. Armed by the first buffered
    // byte and disarmed by the flush, so an idle session never wakes up.
    let mut flush_at: Option<Instant> = None;

    let emit = |frame: Vec<u8>| {
        unacked.fetch_add(frame.len(), Ordering::AcqRel);
        sink.frame(frame);
    };

    // `output` drops on return, tearing the shell down.
    loop {
        // Backpressure: stop reading while too much output is unacknowledged.
        if unacked.load(Ordering::Acquire) >= UNACKED_HIGH_WATERMARK {
            if let Some(f) = batcher.take() {
                emit(f);
            }
            flush_at = None;
            loop {
                let notified = notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if unacked.load(Ordering::Acquire) < UNACKED_HIGH_WATERMARK {
                    break;
                }
                tokio::select! {
                    _ = &mut notified => {}
                    _ = &mut cancel => return LoopEnd::Cancelled,
                }
            }
        }

        tokio::select! {
            _ = &mut cancel => return LoopEnd::Cancelled,
            msg = output.recv() => match msg {
                Some(chunk) => {
                    batcher.push(&chunk).into_iter().for_each(&emit);
                    if batcher.is_empty() {
                        flush_at = None;
                    } else if flush_at.is_none() {
                        flush_at = Some(Instant::now() + FRAME_INTERVAL);
                    }
                }
                None => {
                    if let Some(f) = batcher.take() {
                        emit(f);
                    }
                    return LoopEnd::OutputClosed;
                }
            },
            _ = sleep_until(flush_at.unwrap_or_else(Instant::now)), if flush_at.is_some() => {
                flush_at = None;
                if let Some(f) = batcher.take() {
                    emit(f);
                }
            }
        }
    }
}
