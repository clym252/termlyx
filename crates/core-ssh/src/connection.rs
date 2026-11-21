//! One SSH connection shared by many channels (shells, SFTP, port forwards),
//! with flow control per channel.
//!
//! # Flow control
//!
//! Stock russh 0.64.1 refills a channel's receive window as soon as data
//! *arrives*, not when the application reads it:
//!
//! - `russh-0.64.1/src/client/encrypted.rs:577-591` (`CHANNEL_DATA`; and
//!   `:604-614` for `CHANNEL_EXTENDED_DATA`) calls `adjust_window_size` with
//!   `config.window_size` for every packet.
//! - `russh-0.64.1/src/session.rs:305-335` (`adjust_window_size`) sends
//!   `CHANNEL_WINDOW_ADJUST` back up to `window_size` once the window drops
//!   below half.
//!
//! The session loop then hands the data to the channel with an awaited send
//! on a bounded queue (`encrypted.rs:589`, `channel_buffer_size` = 100
//! messages). So a channel nobody reads either stalls the whole connection
//! (the loop stops reading the socket) or, if drained, buffers without bound.
//! `Handler::adjust_window` is no hook: the client never reads its return
//! value (`client/mod.rs:3084`, `encrypted.rs:581-585`).
//!
//! We therefore use a vendored russh with a small opt-in patch
//! (`vendor/russh/TETHER-PATCH.md`): `client::Config::manual_window_adjust`
//! turns the automatic top-up off, and `client::WindowGranter` grants window
//! per channel. Grants go on russh's priority queue, which the session loop
//! serves even while some channel's outbound data waits on the server's
//! window (the normal outbound queue is not served then, see below), so a
//! write-blocked channel cannot stop other channels' inbound data. On top of it, this module implements approach
//! (a) of the plan, "withhold the SSH window per channel":
//!
//! - Every channel has its own pump task that **always** drains russh's
//!   per-channel queue at once, so the russh session loop never waits on us
//!   and our `client::Handler` callbacks never await a consumer.
//! - Drained bytes go to a per-channel buffer. The server may only send what
//!   the window allows, and window is granted back only for bytes that have
//!   left the buffer for the consumer, so the buffer never holds more than
//!   [`CHANNEL_WINDOW`] bytes. A server that sends past its window breaks the
//!   protocol, and that channel is closed.
//! - Per channel, memory is bounded by the window plus the consumer queue:
//!   `CHANNEL_WINDOW + OUT_SLOTS * 32 KiB` (2 MiB + 512 KiB; 32 KiB is the
//!   largest packet we accept), plus small bookkeeping per queued packet
//!   (packets under 1 KiB are coalesced, so that stays a few percent).
//! - A consumer that stops reading stops that channel's window and nothing
//!   else; no data is dropped.
//!
//! Known limitation (stock russh 0.64.1, not changed by the patch): while
//! any channel's outbound data waits on the server's window, the session
//! loop does not take new outbound messages from the normal queue
//! (`can_receive_outbound` in `russh-0.64.1/src/client/mod.rs`, around
//! line 1373). So every channel's *writes* (and channel opens, requests,
//! close) wait until the server grants that channel more window. Inbound
//! data and window grants keep flowing. A remote side that never reads one
//! channel's input therefore stalls uploads and new channels on the same
//! connection; SFTP uploads and forwards (Tasks 10/11) can hit this.
//!
//! Extended data (stderr) is merged into a shell's output, as in Phase 0.
//! Byte-stream channels (SFTP, forwards) carry no extended data, so any that
//! arrives is discarded and its window granted back at once.
//!
//! # Lifetime
//!
//! [`SshConnection`] clones and every channel handle hold a reference to the
//! connection: a shell's pump task (which lives as long as its
//! [`ShellHandle`]), [`ChannelStream`] (so also [`SftpChannel`]) and
//! [`RemoteListener`]. When the last one drops, the connection disconnects and
//! its russh task ends. No explicit close is needed to avoid a leak.
//! [`SshConnection::close_signal`] observes the end without keeping the
//! connection alive.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::{Pin, pin};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use russh::client::{Handle, Msg, WindowGranter};
use russh::{Channel, ChannelId, ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Disconnect};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, timeout_at};
use tracing::{debug, warn};

use crate::error::SshError;
use crate::hostkey::HostKeyVerifier;
use crate::progress::{ConnectProgress, NoProgress};
use crate::prompter::AuthPrompter;
use crate::russh_impl::{ClientHandler, establish};
use crate::transport::{ConnectParams, ControlMsg, ShellControl, ShellEnd, ShellHandle};

/// Receive window per channel; also the cap on its inbound buffer.
pub(crate) const CHANNEL_WINDOW: u32 = 2 * 1024 * 1024;
/// Window is granted back in batches of at least this many bytes.
const GRANT_THRESHOLD: u32 = CHANNEL_WINDOW / 4;
/// Chunks (at most one SSH packet each, <= 32 KiB) queued for a consumer
/// beyond the buffer.
const OUT_SLOTS: usize = 16;
/// Packets smaller than this are coalesced in the buffer...
const SMALL_PACKET: usize = 1024;
/// ...into chunks of up to this size.
const COALESCE_TO: usize = 16 * 1024;
const CONTROL_BUFFER: usize = 64;
/// Max writes queued locally while one is blocked on the SSH window. Bounds
/// memory and keeps backpressure on `ShellControl::write` callers.
const MAX_PENDING_WRITES: usize = 64;
/// Forwarded connections waiting in a [`RemoteListener`] before new ones are
/// rejected.
const FORWARD_BACKLOG: usize = 16;
/// How long a forwarded connection that matches no listener waits for a
/// port-0 forward still being set up.
const PENDING_ROUTE_WAIT: Duration = Duration::from_secs(5);

type ConnRef = Arc<Inner>;

struct Inner {
    /// `None` only while dropping.
    handle: Option<Handle<ClientHandler>>,
    closed: watch::Receiver<bool>,
    forwards: Arc<ForwardRoutes>,
    /// Grants window to this connection's channels.
    granter: WindowGranter,
    /// Bounds the server replies when opening a channel.
    setup_timeout: Duration,
    /// Requested before every shell (`ConnectParams::env`).
    env: Vec<(String, String)>,
    /// Why the session ended, once it has (set by the russh handler).
    disconnect: DisconnectSlot,
}

/// Why a connection's session ended: the server's disconnect message or the
/// transport error. Written by the russh handler, read by channel pumps.
pub(crate) type DisconnectSlot = Arc<Mutex<Option<String>>>;

impl Inner {
    fn handle(&self) -> &Handle<ClientHandler> {
        self.handle
            .as_ref()
            .expect("the handle is only taken when the connection drops")
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        debug!("last reference to the ssh connection dropped; disconnecting");
        // Dropping the handle alone also ends the session once every channel
        // is gone; disconnecting tells the server why.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
            });
        }
    }
}

/// An authenticated SSH connection that channels are opened on. Cheap to
/// clone; see the module docs for its lifetime.
#[derive(Clone)]
pub struct SshConnection {
    inner: ConnRef,
}

impl std::fmt::Debug for SshConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshConnection")
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl SshConnection {
    /// Connects and authenticates. `p.term`, `p.cols` and `p.rows` are not
    /// used (they are the legacy one-shell path's parameters).
    pub async fn connect(
        p: &ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
        prompter: Arc<dyn AuthPrompter>,
    ) -> Result<Self, SshError> {
        Self::connect_with_progress(p, v, prompter, Arc::new(NoProgress)).await
    }

    /// [`connect`](Self::connect), reporting each connect step to `progress`
    /// as it starts.
    pub async fn connect_with_progress(
        p: &ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
        prompter: Arc<dyn AuthPrompter>,
        progress: Arc<dyn ConnectProgress>,
    ) -> Result<Self, SshError> {
        let forwards = Arc::new(ForwardRoutes::default());
        let disconnect = DisconnectSlot::default();
        let (handle, closed) = establish(
            p,
            v,
            prompter,
            forwards.clone(),
            disconnect.clone(),
            progress.as_ref(),
        )
        .await?;
        let granter = handle.window_granter();
        // Before any forward is requested, so before any forwarded channel.
        let _ = forwards.granter.set(granter.clone());
        Ok(Self {
            inner: Arc::new(Inner {
                handle: Some(handle),
                closed,
                forwards,
                granter,
                setup_timeout: p.connect_timeout,
                env: p.env.clone(),
                disconnect,
            }),
        })
    }

    /// Opens an interactive shell (pty + shell) channel.
    pub async fn open_shell(
        &self,
        term: &str,
        cols: u16,
        rows: u16,
    ) -> Result<ShellHandle, SshError> {
        self.open_shell_with(ShellOptions {
            term,
            cols,
            rows,
            env: &[],
            startup: None,
        })
        .await
    }

    /// Opens an interactive pty channel running the user's shell, or
    /// `o.startup` when set, with `o.env` on top of the connection-wide
    /// environment.
    pub async fn open_shell_with(&self, o: ShellOptions<'_>) -> Result<ShellHandle, SshError> {
        let deadline = self.setup_deadline();
        let mut channel = self
            .setup(deadline, self.inner.handle().channel_open_session())
            .await?;
        // The pty reply is awaited (nothing else can arrive before it); the
        // shell's is not, since its output may follow right away (a refused
        // shell shows up as a channel that closes).
        let requests = async {
            channel
                .request_pty(
                    true,
                    o.term,
                    u32::from(o.cols),
                    u32::from(o.rows),
                    0,
                    0,
                    &[],
                )
                .await?;
            request_reply(&mut channel).await?;
            // No reply asked for: a server that refuses a variable (no
            // matching `AcceptEnv`) just ignores it.
            for (name, value) in self.inner.env.iter().chain(o.env) {
                channel
                    .set_env(false, name.as_str(), value.as_str())
                    .await?;
            }
            match o.startup {
                Some(command) => channel.exec(true, command).await,
                None => channel.request_shell(true).await,
            }
        };
        if let Err(e) = self.setup(deadline, requests).await {
            close_unused(channel);
            return Err(e);
        }
        let (out_tx, out_rx) = mpsc::channel(OUT_SLOTS);
        let (ctl_tx, ctl_rx) = mpsc::channel(CONTROL_BUFFER);
        let (end_tx, end_rx) = oneshot::channel();
        tokio::spawn(shell_pump(
            self.inner.clone(),
            channel,
            self.inner.granter.clone(),
            ShellIo {
                out: out_tx,
                ctl: ctl_rx,
                end: end_tx,
            },
        ));
        Ok(ShellHandle::new(out_rx, ShellControl::new(ctl_tx), end_rx))
    }

    /// Runs `command` on a channel of its own, without a pty, and collects
    /// its standard output (at most `max_bytes` of it; the rest is read and
    /// dropped). Standard input is closed at once. The whole call, the wait
    /// for the command to finish included, is bounded by the connection's
    /// setup timeout.
    pub async fn exec_capture(
        &self,
        command: &str,
        max_bytes: usize,
    ) -> Result<ExecOutput, SshError> {
        let deadline = self.setup_deadline();
        let mut channel = self
            .setup(deadline, self.inner.handle().channel_open_session())
            .await?;
        // The reply comes before any output (the command starts after it),
        // so awaiting it drops no data.
        let request = async {
            channel.exec(true, command).await?;
            request_reply(&mut channel).await?;
            channel.eof().await
        };
        if let Err(e) = self.setup(deadline, request).await {
            close_unused(channel);
            return Err(e);
        }
        let (mut read, write) = channel.split();
        let (tx, mut rx) = mpsc::channel(OUT_SLOTS);
        let window = (&self.inner.granter, write.id());
        let run = async {
            let mut exit_status = None;
            let pump = async {
                let end =
                    pump_inbound(&mut read, window, &tx, Extended::Discard, &mut exit_status).await;
                drop(tx);
                end
            };
            let collect = async {
                let mut stdout = Vec::new();
                while let Some(chunk) = rx.recv().await {
                    let room = max_bytes.saturating_sub(stdout.len());
                    stdout.extend_from_slice(&chunk[..chunk.len().min(room)]);
                }
                stdout
            };
            let (end, stdout) = tokio::join!(pump, collect);
            if end == InboundEnd::Eof && exit_status.is_none() {
                exit_status = await_exit_status(&mut read).await;
            }
            (end, stdout, exit_status)
        };
        let finished = timeout_at(deadline, run).await;
        // Spawned: the close may have to wait for the outbound queue.
        tokio::spawn(async move {
            let _ = write.close().await;
        });
        match finished {
            Ok((InboundEnd::Eof, stdout, exit_status)) => Ok(ExecOutput {
                stdout,
                exit_status,
            }),
            Ok((InboundEnd::Overrun, ..)) => Err(SshError::Channel(
                "the server sent more than the channel window".into(),
            )),
            Ok(_) => Err(SshError::Closed),
            Err(_) => Err(SshError::Channel("timed out".into())),
        }
    }

    /// Opens the "sftp" subsystem channel.
    pub async fn open_sftp(&self) -> Result<SftpChannel, SshError> {
        Ok(SftpChannel {
            stream: self.open_subsystem("sftp").await?,
        })
    }

    async fn open_subsystem(&self, name: &str) -> Result<ChannelStream, SshError> {
        let deadline = self.setup_deadline();
        let mut channel = self
            .setup(deadline, self.inner.handle().channel_open_session())
            .await?;
        let request = async {
            channel.request_subsystem(true, name).await?;
            // A subsystem says nothing before the client does.
            request_reply(&mut channel).await
        };
        if let Err(e) = self.setup(deadline, request).await {
            close_unused(channel);
            return Err(e);
        }
        Ok(ChannelStream::spawn(
            channel,
            self.inner.granter.clone(),
            Some(self.inner.clone()),
        ))
    }

    /// Opens a `direct-tcpip` channel to `host:port` as seen from the server
    /// (local and dynamic forwards). `originator` is the local peer that
    /// asked for it.
    pub async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u16,
        originator: SocketAddr,
    ) -> Result<ChannelStream, SshError> {
        let channel = self
            .setup(
                self.setup_deadline(),
                self.inner.handle().channel_open_direct_tcpip(
                    host,
                    u32::from(port),
                    originator.ip().to_string(),
                    u32::from(originator.port()),
                ),
            )
            .await?;
        Ok(ChannelStream::spawn(
            channel,
            self.inner.granter.clone(),
            Some(self.inner.clone()),
        ))
    }

    /// Asks the server to listen on `bind_addr:bind_port` (remote forward).
    /// Connections to it arrive on the returned listener. `bind_port` 0 lets
    /// the server pick; see [`RemoteListener::port`].
    pub async fn tcpip_forward(
        &self,
        bind_addr: &str,
        bind_port: u16,
    ) -> Result<RemoteListener, SshError> {
        let (tx, rx) = mpsc::channel(FORWARD_BACKLOG);
        let routes = &self.inner.forwards;
        // Route before asking, so a connection that arrives right after the
        // reply finds its listener. A server-picked port is only known from
        // the reply, so it is routed then; until that route is in place, a
        // forwarded connection that finds no listener waits for it (see
        // `ForwardRoutes::route`). The guard ends the wait, also on error.
        let mut key = (bind_addr.to_owned(), u32::from(bind_port));
        let pending = (bind_port == 0).then(|| routes.begin_pending());
        if bind_port != 0 {
            routes.insert(key.clone(), tx.clone());
        }
        let bound = self
            .setup(
                self.setup_deadline(),
                self.inner
                    .handle()
                    .tcpip_forward(bind_addr, u32::from(bind_port)),
            )
            .await;
        let port = match bound {
            Ok(0) => u32::from(bind_port),
            Ok(port) => port,
            Err(e) => {
                routes.remove(&key, &tx);
                return Err(e);
            }
        };
        if bind_port == 0 {
            key.1 = port;
            routes.insert(key.clone(), tx.clone());
        }
        drop(pending);
        Ok(RemoteListener {
            conn: self.inner.clone(),
            key,
            tx,
            rx,
            cancelled: false,
        })
    }

    /// Whether the connection is gone (disconnected by either side).
    pub fn is_closed(&self) -> bool {
        *self.inner.closed.borrow() || self.inner.handle().is_closed()
    }

    /// Resolves once the connection is gone.
    pub async fn closed(&self) {
        self.close_signal().wait().await;
    }

    /// A handle that observes the end of the connection without keeping it
    /// alive.
    pub fn close_signal(&self) -> ClosedSignal {
        ClosedSignal {
            rx: self.inner.closed.clone(),
        }
    }

    fn setup_deadline(&self) -> Instant {
        Instant::now() + self.inner.setup_timeout
    }

    /// Runs one step of channel setup, bounded by `deadline`.
    async fn setup<T>(
        &self,
        deadline: Instant,
        f: impl Future<Output = Result<T, russh::Error>>,
    ) -> Result<T, SshError> {
        match timeout_at(deadline, f).await {
            Ok(Ok(t)) => Ok(t),
            Ok(Err(e)) if self.is_closed() => {
                debug!(error = %e, "channel setup on a closed connection");
                Err(SshError::Closed)
            }
            Ok(Err(e)) => Err(SshError::Channel(e.to_string())),
            Err(_) => Err(SshError::Channel("timed out".into())),
        }
    }
}

/// What [`SshConnection::open_shell_with`] opens.
#[derive(Clone)]
pub struct ShellOptions<'a> {
    pub term: &'a str,
    pub cols: u16,
    pub rows: u16,
    /// Environment for this shell only, sent after the connection-wide
    /// [`ConnectParams::env`]. Like that one, a variable the server does not
    /// accept (`AcceptEnv`) is dropped without an error.
    pub env: &'a [(String, String)],
    /// A command to run in place of the login shell, sent as an `exec`
    /// request on the pty channel. The server runs it through the user's
    /// login shell (`$SHELL -c`); see [`crate::tmux_reattach_command`].
    pub startup: Option<&'a str>,
}

impl std::fmt::Debug for ShellOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellOptions")
            .field("term", &self.term)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            // Names only: values may be sensitive, and the startup command
            // can hold a session name.
            .field(
                "env",
                &self.env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            )
            .field("startup", &self.startup.map(|_| "<set>"))
            .finish()
    }
}

/// What a command run by [`SshConnection::exec_capture`] printed and how it
/// ended.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    /// Standard output, cut off at the caller's limit. Standard error is
    /// dropped.
    pub stdout: Vec<u8>,
    /// `None` when the server reported no status (for example it ended the
    /// command with a signal).
    pub exit_status: Option<u32>,
}

/// Observes the end of an [`SshConnection`] without keeping it alive.
#[derive(Clone, Debug)]
pub struct ClosedSignal {
    rx: watch::Receiver<bool>,
}

impl ClosedSignal {
    pub fn is_closed(&self) -> bool {
        *self.rx.borrow() || self.rx.has_changed().is_err()
    }

    /// Resolves once the connection is gone.
    pub async fn wait(mut self) {
        // An error means the sender (owned by the session) is gone: closed.
        let _ = self.rx.wait_for(|closed| *closed).await;
    }
}

/// The sender half of the close signal, owned by the russh session's
/// handler: it fires when the session ends for any reason.
pub(crate) struct CloseOnDrop(pub(crate) watch::Sender<bool>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

/// The "sftp" subsystem channel; an SFTP client runs over
/// [`into_stream`](Self::into_stream).
#[derive(Debug)]
pub struct SftpChannel {
    stream: ChannelStream,
}

impl SftpChannel {
    /// The channel as a byte stream (for example for
    /// `russh_sftp::client::SftpSession::new`). It keeps the connection alive.
    pub fn into_stream(self) -> ChannelStream {
        self.stream
    }
}

/// A channel as an `AsyncRead + AsyncWrite` byte stream (SFTP, forwards).
/// Reading returns EOF once the server sent EOF; shutting down sends EOF;
/// dropping closes the channel. Holds a reference to the connection.
pub struct ChannelStream {
    rx: mpsc::Receiver<Bytes>,
    unread: Bytes,
    tx: Pin<Box<dyn AsyncWrite + Send>>,
    /// Dropped with the stream, which tells the pump to close the channel.
    _alive: oneshot::Sender<()>,
    conn: Option<ConnRef>,
}

impl std::fmt::Debug for ChannelStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelStream").finish_non_exhaustive()
    }
}

impl ChannelStream {
    /// Starts the channel's pump. `conn` is `None` only for forwarded
    /// channels created inside the session's handler, which must not own the
    /// connection; [`RemoteListener::accept`] attaches it.
    fn spawn(channel: Channel<Msg>, granter: WindowGranter, conn: Option<ConnRef>) -> Self {
        let tx = Box::pin(channel.make_writer());
        let (out_tx, rx) = mpsc::channel(OUT_SLOTS);
        let (alive, gone) = oneshot::channel();
        tokio::spawn(stream_pump(channel, granter, out_tx, gone));
        Self {
            rx,
            unread: Bytes::new(),
            tx,
            _alive: alive,
            conn,
        }
    }
}

impl AsyncRead for ChannelStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.unread.is_empty() {
            match ready!(this.rx.poll_recv(cx)) {
                Some(chunk) => this.unread = chunk,
                None => return Poll::Ready(Ok(())),
            }
        }
        let n = buf.remaining().min(this.unread.len());
        buf.put_slice(&this.unread.split_to(n));
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for ChannelStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.get_mut().tx.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().tx.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().tx.as_mut().poll_shutdown(cx)
    }
}

/// A connection the server accepted on a remote forward.
pub struct ForwardedConnection {
    pub stream: ChannelStream,
    /// Who connected to the server's listening socket.
    pub originator_address: String,
    pub originator_port: u32,
}

/// A remote (`-R`) forward: connections the server accepts on the bound
/// address arrive via [`accept`](Self::accept). Dropping it stops routing
/// new connections and cancels the forward on the server.
pub struct RemoteListener {
    conn: ConnRef,
    /// (bind address, bound port) as the server reports them.
    key: (String, u32),
    tx: mpsc::Sender<ForwardedConnection>,
    rx: mpsc::Receiver<ForwardedConnection>,
    /// [`cancel`](Self::cancel) got an answer to `cancel-tcpip-forward`.
    cancelled: bool,
}

impl std::fmt::Debug for RemoteListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteListener")
            .field("addr", &self.key.0)
            .field("port", &self.key.1)
            .finish_non_exhaustive()
    }
}

impl RemoteListener {
    /// The port the server listens on (picked by the server if 0 was asked).
    pub fn port(&self) -> u32 {
        self.key.1
    }

    /// The next forwarded connection; `None` once the connection is gone.
    pub async fn accept(&mut self) -> Option<ForwardedConnection> {
        tokio::select! {
            next = self.rx.recv() => {
                let mut next = next?;
                next.stream.conn = Some(self.conn.clone());
                Some(next)
            }
            () = (ClosedSignal { rx: self.conn.closed.clone() }).wait() => None,
        }
    }

    /// Stops routing new connections and cancels the forward on the server,
    /// waiting (at most the connection's setup timeout) for its reply.
    /// Connections already accepted are not affected.
    ///
    /// If this future is dropped before the server answered (or it timed
    /// out), dropping the listener sends the cancel again in the
    /// background, as an un-cancelled listener's drop does.
    pub async fn cancel(mut self) -> Result<(), SshError> {
        self.conn.forwards.remove(&self.key, &self.tx);
        let (addr, port) = self.key.clone();
        let deadline = Instant::now() + self.conn.setup_timeout;
        let result = timeout_at(
            deadline,
            self.conn.handle().cancel_tcpip_forward(addr, port),
        )
        .await;
        // Only an answer (or a dead session) settles it; otherwise Drop
        // tries again.
        self.cancelled = result.is_ok();
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) if self.conn.handle().is_closed() || *self.conn.closed.borrow() => {
                Err(SshError::Closed)
            }
            Ok(Err(e)) => Err(SshError::Channel(e.to_string())),
            Err(_) => Err(SshError::Channel("timed out".into())),
        }
    }
}

impl Drop for RemoteListener {
    fn drop(&mut self) {
        self.conn.forwards.remove(&self.key, &self.tx);
        if self.cancelled {
            return;
        }
        let (addr, port) = self.key.clone();
        let conn = self.conn.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                if let Err(e) = conn.handle().cancel_tcpip_forward(addr, port).await {
                    debug!(error = %e, "cancel-tcpip-forward failed");
                }
            });
        }
    }
}

/// Where forwarded-tcpip channels go: (bind address, port) to the listener's
/// queue. Shared by the connection and the session's handler, holding no
/// reference to the connection itself.
pub(crate) struct ForwardRoutes {
    routes: Mutex<HashMap<(String, u32), mpsc::Sender<ForwardedConnection>>>,
    /// Set once the connection is established.
    granter: OnceLock<WindowGranter>,
    /// `tcpip-forward` requests for port 0 whose route is not in place yet.
    pending: watch::Sender<usize>,
    /// Runs between the two steps of [`lookup_now`](Self::lookup_now), so a
    /// test can force the interleaving it guards against.
    #[cfg(test)]
    lookup_hook: Mutex<Option<LookupHook>>,
}

#[cfg(test)]
type LookupHook = Box<dyn FnOnce(&ForwardRoutes) + Send>;

impl Default for ForwardRoutes {
    fn default() -> Self {
        Self {
            routes: Mutex::default(),
            granter: OnceLock::new(),
            pending: watch::Sender::new(0),
            #[cfg(test)]
            lookup_hook: Mutex::default(),
        }
    }
}

/// What [`ForwardRoutes::lookup_now`] decided.
enum Lookup {
    /// Deliver to this listener, or reject (`None`).
    Now(Option<mpsc::Sender<ForwardedConnection>>),
    /// No listener yet, but a port-0 forward may still route it.
    Wait,
}

/// Marks a port-0 `tcpip-forward` request in flight; dropped once its route
/// is in place (or the request failed).
pub(crate) struct PendingRoute<'a>(&'a ForwardRoutes);

impl Drop for PendingRoute<'_> {
    fn drop(&mut self) {
        self.0.pending.send_modify(|n| *n = n.saturating_sub(1));
    }
}

impl ForwardRoutes {
    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<(String, u32), mpsc::Sender<ForwardedConnection>>> {
        self.routes.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn insert(&self, key: (String, u32), tx: mpsc::Sender<ForwardedConnection>) {
        self.lock().insert(key, tx);
    }

    /// Removes `key` if it still routes to `tx`.
    fn remove(&self, key: &(String, u32), tx: &mpsc::Sender<ForwardedConnection>) {
        let mut routes = self.lock();
        if routes.get(key).is_some_and(|t| t.same_channel(tx)) {
            routes.remove(key);
        }
    }

    /// Starts a port-0 request; see [`PendingRoute`].
    fn begin_pending(&self) -> PendingRoute<'_> {
        self.pending.send_modify(|n| *n += 1);
        PendingRoute(self)
    }

    /// The listener for a connection to `addr:port`: an exact match, else
    /// the only listener on that port (servers may report the address in
    /// another form than it was requested in).
    fn find(&self, addr: &str, port: u32) -> Option<mpsc::Sender<ForwardedConnection>> {
        let routes = self.lock();
        if let Some(tx) = routes.get(&(addr.to_owned(), port)) {
            return Some(tx.clone());
        }
        let mut on_port = routes.iter().filter(|((_, p), _)| *p == port);
        match (on_port.next(), on_port.next()) {
            (Some((_, tx)), None) => Some(tx.clone()),
            _ => None,
        }
    }

    /// Like [`find`](Self::find), but while port-0 requests are pending,
    /// waits (at most [`PENDING_ROUTE_WAIT`]) for one of them to route the
    /// port.
    ///
    /// The server replies to `tcpip-forward` before it can open a channel
    /// for that port, and the session loop handles messages in order, so the
    /// reply has already been handed to the requesting task when the channel
    /// open arrives. That task only has to run to insert the route, so the
    /// wait is short in practice.
    async fn find_when_routed(
        &self,
        addr: &str,
        port: u32,
    ) -> Option<mpsc::Sender<ForwardedConnection>> {
        let deadline = Instant::now() + PENDING_ROUTE_WAIT;
        let mut pending = self.pending.subscribe();
        loop {
            // Read the count before looking: a request inserts its route
            // before it stops being pending, so a count of 0 read first
            // means every route that was coming is visible to `find`.
            let waiting = *pending.borrow_and_update();
            if let Some(tx) = self.find(addr, port) {
                return Some(tx);
            }
            if waiting == 0 {
                return None;
            }
            match timeout_at(deadline, pending.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => return self.find(addr, port),
            }
        }
    }

    /// Called from the session's handler for a `forwarded-tcpip` channel
    /// open. Never waits: the channel is queued for its listener, or
    /// rejected when there is no listener or its backlog is full. When no
    /// listener matches but a port-0 forward is still being set up, the
    /// decision moves to a task that waits for that route.
    pub(crate) async fn route(
        self: &Arc<Self>,
        channel: Channel<Msg>,
        connected: (&str, u32),
        originator: (&str, u32),
        reply: russh::client::ChannelOpenHandle,
    ) {
        let originator = (originator.0.to_owned(), originator.1);
        if let Lookup::Now(found) = self.lookup_now(connected.0, connected.1) {
            self.deliver(found, channel, originator, reply).await;
            return;
        }
        let this = self.clone();
        let (addr, port) = (connected.0.to_owned(), connected.1);
        tokio::spawn(async move {
            let found = this.find_when_routed(&addr, port).await;
            this.deliver(found, channel, originator, reply).await;
        });
    }

    /// The decision `route` can take without waiting. The pending count is
    /// read **before** looking (as in `find_when_routed`): a request inserts
    /// its route before it stops being pending, so if the count read first
    /// is 0, every route that was coming is visible to the lookup. Read the
    /// other way round, a route inserted between the two steps would be
    /// missed and the connection rejected.
    fn lookup_now(&self, addr: &str, port: u32) -> Lookup {
        let waiting = *self.pending.borrow();
        #[cfg(test)]
        if let Some(hook) = self
            .lookup_hook
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            hook(self);
        }
        match self.find(addr, port) {
            Some(tx) => Lookup::Now(Some(tx)),
            None if waiting == 0 => Lookup::Now(None),
            None => Lookup::Wait,
        }
    }

    /// Hands the channel to `tx`'s listener, or rejects it (no listener, or
    /// its backlog is full).
    async fn deliver(
        &self,
        tx: Option<mpsc::Sender<ForwardedConnection>>,
        channel: Channel<Msg>,
        originator: (String, u32),
        reply: russh::client::ChannelOpenHandle,
    ) {
        let permit = tx.and_then(|tx| tx.try_reserve_owned().ok());
        let (Some(permit), Some(granter)) = (permit, self.granter.get()) else {
            debug!("rejecting forwarded connection");
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return;
        };
        reply.accept().await;
        permit.send(ForwardedConnection {
            stream: ChannelStream::spawn(channel, granter.clone(), None),
            originator_address: originator.0,
            originator_port: originator.1,
        });
    }
}

/// How a pump treats extended data (stderr).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Extended {
    /// Merge it into the output (shells).
    Merge,
    /// Drop it and grant its window back (byte streams).
    Discard,
}

/// Why [`pump_inbound`] returned.
#[derive(Debug, PartialEq, Eq)]
enum InboundEnd {
    /// The server finished sending (EOF or close) and everything was
    /// delivered.
    Eof,
    /// The channel went away without EOF or close (the connection ended)
    /// and everything received was delivered.
    Lost,
    /// The consumer went away.
    ConsumerGone,
    /// The server sent past its window.
    Overrun,
}

/// Moves one channel's inbound data from russh to `out`, withholding the
/// window: russh's queue is always drained at once (the session loop never
/// waits on us), the bytes wait in a buffer capped at the window, and window
/// is granted back only for bytes handed to `out`.
///
/// An exit status the server reports on the way is stored in `exit_status`.
async fn pump_inbound(
    read: &mut ChannelReadHalf,
    window: (&WindowGranter, ChannelId),
    out: &mpsc::Sender<Bytes>,
    extended: Extended,
    exit_status: &mut Option<u32>,
) -> InboundEnd {
    let (granter, id) = window;
    let mut buf = InboundBuffer::default();
    // Bytes handed on whose window has not been granted back yet.
    let mut credit: u32 = 0;
    let mut eof = false;
    // The channel's message stream ended without EOF or close.
    let mut lost = false;
    loop {
        if eof && buf.is_empty() {
            return if lost {
                InboundEnd::Lost
            } else {
                InboundEnd::Eof
            };
        }
        if credit >= GRANT_THRESHOLD {
            // Never waits (priority queue). Fails only once the session is
            // gone, which the read side sees next.
            if let Err(e) = granter.adjust_window(id, credit) {
                debug!(error = %e, "window adjust failed");
            }
            credit = 0;
        }
        tokio::select! {
            msg = read.wait(), if !eof => match msg {
                Some(ChannelMsg::Data { data }) => {
                    if !buf.push(data) {
                        return InboundEnd::Overrun;
                    }
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => match extended {
                    Extended::Merge => {
                        if !buf.push(data) {
                            return InboundEnd::Overrun;
                        }
                    }
                    Extended::Discard => credit = credit.saturating_add(len_u32(&data)),
                },
                Some(ChannelMsg::Eof | ChannelMsg::Close) => eof = true,
                Some(ChannelMsg::ExitStatus { exit_status: status }) => *exit_status = Some(status),
                None => {
                    eof = true;
                    lost = true;
                }
                Some(_) => {}
            },
            permit = out.reserve(), if !buf.is_empty() => match permit {
                Ok(permit) => {
                    if let Some(chunk) = buf.pop() {
                        credit = credit.saturating_add(len_u32(&chunk));
                        permit.send(chunk);
                    }
                }
                Err(_) => return InboundEnd::ConsumerGone,
            },
            // The consumer dropped its receiver while nothing is pending.
            () = out.closed(), if buf.is_empty() => return InboundEnd::ConsumerGone,
        }
    }
}

fn len_u32(b: &[u8]) -> u32 {
    u32::try_from(b.len()).unwrap_or(u32::MAX)
}

/// A channel's received but undelivered bytes, capped at the window.
/// Packets are queued as received (no copy); small ones are coalesced so the
/// per-entry overhead stays small whatever packet sizes the server uses.
#[derive(Default)]
struct InboundBuffer {
    chunks: VecDeque<Bytes>,
    /// Small packets being coalesced; never reallocated (flushed first).
    small: BytesMut,
    len: usize,
}

impl InboundBuffer {
    fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends `data`; false if that would exceed the window (a protocol
    /// violation by the server).
    fn push(&mut self, data: Bytes) -> bool {
        if self.len + data.len() > CHANNEL_WINDOW as usize {
            warn!("ssh server sent past the channel window; closing the channel");
            return false;
        }
        self.len += data.len();
        if data.len() >= SMALL_PACKET {
            self.flush_small();
            self.chunks.push_back(data);
        } else if !data.is_empty() {
            if self.small.len() + data.len() > COALESCE_TO {
                self.flush_small();
            }
            if self.small.is_empty() {
                self.small.reserve(COALESCE_TO);
            }
            self.small.extend_from_slice(&data);
        }
        true
    }

    fn flush_small(&mut self) {
        if !self.small.is_empty() {
            self.chunks
                .push_back(std::mem::take(&mut self.small).freeze());
        }
    }

    /// The oldest bytes, in arrival order.
    fn pop(&mut self) -> Option<Bytes> {
        if self.chunks.is_empty() {
            self.flush_small();
        }
        let chunk = self.chunks.pop_front()?;
        self.len -= chunk.len();
        Some(chunk)
    }
}

/// How long a finished shell waits for the server's `exit-status` after its
/// EOF (OpenSSH sends it between EOF and close).
const EXIT_STATUS_WAIT: Duration = Duration::from_millis(500);

/// How long a shell whose channel vanished waits for the connection to say
/// why (the handler records the reason as the session ends).
const DISCONNECT_REASON_WAIT: Duration = Duration::from_secs(1);

/// After a failed write: how long to wait for the connection's end reason
/// (shorter: the connection may still be up).
const WRITE_FAILURE_REASON_WAIT: Duration = Duration::from_millis(250);

/// The shell side of a [`shell_pump`]: output, control, and how it ended.
struct ShellIo {
    out: mpsc::Sender<Bytes>,
    ctl: mpsc::Receiver<ControlMsg>,
    end: oneshot::Sender<ShellEnd>,
}

/// Drives a shell channel: output with per-channel flow control, and input,
/// resize and close from its [`ShellControl`]. Either side finishing ends
/// the shell, and the [`ShellEnd`] says how. Holds the connection for as
/// long as the shell lives.
async fn shell_pump(conn: ConnRef, channel: Channel<Msg>, granter: WindowGranter, io: ShellIo) {
    let ShellIo { out, ctl, end } = io;
    let (mut read, write) = channel.split();
    let window = (&granter, write.id());
    let mut exit_status = None;
    let finished = tokio::select! {
        inbound = pump_inbound(&mut read, window, &out, Extended::Merge, &mut exit_status) => {
            debug!(?inbound, "shell output finished");
            Ok(inbound)
        }
        written = write_loop(&write, ctl) => written.map(|()| InboundEnd::ConsumerGone),
    };
    let shell_end = match finished {
        Ok(InboundEnd::Eof) => {
            if exit_status.is_none() {
                exit_status = await_exit_status(&mut read).await;
            }
            ShellEnd::Exited { exit_status }
        }
        Ok(InboundEnd::Lost) => ShellEnd::Lost(
            disconnect_reason(&conn, DISCONNECT_REASON_WAIT)
                .await
                .unwrap_or_else(|| "connection closed".into()),
        ),
        Ok(InboundEnd::Overrun) => {
            ShellEnd::Lost("the server sent more than the channel window".into())
        }
        // Closed locally (or no one is listening): nothing went wrong.
        Ok(InboundEnd::ConsumerGone) => ShellEnd::Exited { exit_status: None },
        // A write usually fails because the connection went away: say why
        // it did (the race between the failed write and the inbound end
        // must not decide the message), else the write error.
        Err(e) => {
            warn!(error = %e, "write to ssh channel failed");
            ShellEnd::Lost(
                disconnect_reason(&conn, WRITE_FAILURE_REASON_WAIT)
                    .await
                    .unwrap_or_else(|| format!("write failed: {e}")),
            )
        }
    };
    // Before `out` closes, so the consumer sees it with the end of output.
    let _ = end.send(shell_end);
    // `out` and the control receiver go away here: the output receiver sees
    // EOF and late write/resize calls return `SshError::Closed`.
    drop(out);
    drop(read);
    let _ = write.close().await;
    drop(conn);
}

/// After a shell's EOF: the exit status, if the server sends one before
/// closing the channel (bounded by [`EXIT_STATUS_WAIT`]).
async fn await_exit_status(read: &mut ChannelReadHalf) -> Option<u32> {
    let wait = async {
        loop {
            match read.wait().await {
                Some(ChannelMsg::ExitStatus { exit_status }) => return Some(exit_status),
                Some(ChannelMsg::Close) | None => return None,
                Some(_) => {}
            }
        }
    };
    tokio::time::timeout(EXIT_STATUS_WAIT, wait)
        .await
        .ok()
        .flatten()
}

/// Why `conn`'s session ended, waiting up to `wait` (for the connection to
/// close) for the handler to record it.
async fn disconnect_reason(conn: &ConnRef, wait: Duration) -> Option<String> {
    let recorded = || {
        conn.disconnect
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    };
    if recorded().is_none() {
        let mut closed = conn.closed.clone();
        let _ = tokio::time::timeout(wait, closed.wait_for(|c| *c)).await;
    }
    recorded()
}

/// Drives a byte-stream channel until its [`ChannelStream`] is dropped (or
/// the channel closes). After the server's EOF the channel stays open for
/// our writes.
async fn stream_pump(
    channel: Channel<Msg>,
    granter: WindowGranter,
    out: mpsc::Sender<Bytes>,
    mut gone: oneshot::Receiver<()>,
) {
    let (mut read, write) = channel.split();
    let window = (&granter, write.id());
    // Byte streams have no exit status.
    let mut no_status = None;
    let end = tokio::select! {
        end = pump_inbound(&mut read, window, &out, Extended::Discard, &mut no_status) => Some(end),
        _ = &mut gone => None,
    };
    drop(out);
    if end == Some(InboundEnd::Eof) {
        // Half-closed: keep draining (Close, requests) until the stream is
        // dropped or the channel goes away.
        loop {
            tokio::select! {
                _ = &mut gone => break,
                msg = read.wait() => if msg.is_none() { break },
            }
        }
    }
    drop(read);
    let _ = write.close().await;
}

/// Waits for the server's reply to a channel request sent with
/// `want_reply`; a refusal is `RequestDenied`. Call it only where no channel
/// data can arrive first (it would be dropped).
async fn request_reply(channel: &mut Channel<Msg>) -> Result<(), russh::Error> {
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => return Ok(()),
            Some(ChannelMsg::Failure) => return Err(russh::Error::RequestDenied),
            Some(ChannelMsg::Eof | ChannelMsg::Close) | None => {
                return Err(russh::Error::Disconnect);
            }
            Some(ChannelMsg::Data { .. } | ChannelMsg::ExtendedData { .. }) => {
                warn!("channel data before the request reply was dropped");
            }
            Some(_) => {}
        }
    }
}

/// Closes a channel whose setup failed, so it does not stay half open on
/// the shared connection. Spawned: the close request may have to wait for
/// the outbound queue, and the caller is returning an error.
fn close_unused(channel: Channel<Msg>) {
    tokio::spawn(async move {
        if let Err(e) = channel.close().await {
            debug!(error = %e, "closing a channel after failed setup");
        }
    });
}

/// Apply control messages. Returns when the shell should stop: `Ok` for a
/// Close, the error for a write failure. While a write waits for window
/// space, Close and Resize are still honoured and later writes are queued
/// (bounded) to preserve order.
async fn write_loop(
    write: &ChannelWriteHalf<Msg>,
    mut ctl: mpsc::Receiver<ControlMsg>,
) -> Result<(), russh::Error> {
    let mut pending: VecDeque<Bytes> = VecDeque::new();
    let mut ctl_open = true;
    loop {
        let Some(next) = pending.pop_front() else {
            if !ctl_open {
                // All control handles dropped: the read side ends the shell.
                std::future::pending::<()>().await;
            }
            match ctl.recv().await {
                Some(ControlMsg::Write(b)) => pending.push_back(b),
                Some(ControlMsg::Resize { cols, rows }) => resize(write, cols, rows).await,
                Some(ControlMsg::Close) => return Ok(()),
                None => ctl_open = false,
            }
            continue;
        };
        let mut sending = pin!(write.data_bytes(next));
        loop {
            tokio::select! {
                r = &mut sending => {
                    r?;
                    break;
                }
                m = ctl.recv(), if ctl_open && pending.len() < MAX_PENDING_WRITES => match m {
                    Some(ControlMsg::Write(b)) => pending.push_back(b),
                    Some(ControlMsg::Resize { cols, rows }) => resize(write, cols, rows).await,
                    Some(ControlMsg::Close) => return Ok(()),
                    None => ctl_open = false,
                },
            }
        }
    }
}

async fn resize(write: &ChannelWriteHalf<Msg>, cols: u16, rows: u16) {
    if let Err(e) = write
        .window_change(u32::from(cols), u32::from(rows), 0, 0)
        .await
    {
        warn!(error = %e, "window change failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A channel whose setup fails is closed, not left half open: sshd
    /// allows 10 sessions per connection (`MaxSessions`), so 15 failed
    /// subsystem requests would otherwise use them all up.
    #[tokio::test]
    async fn failed_channel_setup_closes_the_channel() {
        if std::env::var("TETHER_SSH_FIXTURE").as_deref() != Ok("1") {
            eprintln!("skipping: set TETHER_SSH_FIXTURE=1 (and run tests/fixtures/up.sh)");
            return;
        }
        let params = ConnectParams {
            host: "127.0.0.1".into(),
            port: 2222,
            username: "tether".into(),
            auth: crate::AuthMethod::Password(secrecy::SecretString::from("tether-test")),
            term: "xterm-256color".into(),
            cols: 80,
            rows: 24,
            connect_timeout: Duration::from_secs(15),
            keepalive: None,
            env: Vec::new(),
        };
        let conn = SshConnection::connect(
            &params,
            Arc::new(crate::AcceptAll),
            Arc::new(crate::NoPrompter),
        )
        .await
        .expect("connect");
        for _ in 0..15 {
            let err = conn
                .open_subsystem("tether-no-such-subsystem")
                .await
                .expect_err("unknown subsystem");
            assert!(matches!(err, SshError::Channel(_)), "got {err:?}");
        }
        let shell = conn.open_shell("xterm-256color", 80, 24).await;
        assert!(shell.is_ok(), "sessions leaked: {:?}", shell.err());
    }

    #[test]
    fn buffer_is_capped_at_the_window() {
        let mut buf = InboundBuffer::default();
        let chunk = Bytes::from(vec![0u8; 32 * 1024]);
        for _ in 0..(CHANNEL_WINDOW as usize / chunk.len()) {
            assert!(buf.push(chunk.clone()));
        }
        assert_eq!(buf.len, CHANNEL_WINDOW as usize);
        assert!(
            !buf.push(Bytes::from_static(b"x")),
            "one byte past the window"
        );
        assert_eq!(buf.len, CHANNEL_WINDOW as usize);
    }

    /// Checks `chunk` is the next part of what was sent.
    fn take_next(sent: &mut Vec<u8>, chunk: &[u8]) {
        assert_eq!(&sent[..chunk.len()], chunk);
        sent.drain(..chunk.len());
    }

    #[test]
    fn buffer_keeps_order_and_coalesces_small_packets() {
        let mut buf = InboundBuffer::default();
        let mut sent = Vec::new();
        for i in 0..5000usize {
            // A mix of tiny, mid-size and full-size packets.
            let n = [1usize, 7, 900, 1024, 5000, 32 * 1024][i % 6];
            let data: Vec<u8> = (0..n).map(|j| (i + j) as u8).collect();
            if buf.len + n > CHANNEL_WINDOW as usize {
                while let Some(c) = buf.pop() {
                    take_next(&mut sent, &c);
                }
            }
            sent.extend_from_slice(&data);
            assert!(buf.push(Bytes::from(data)));
            // Small packets are coalesced: a small entry is always followed
            // by a large one, so at most half the entries are small.
            assert!(
                buf.chunks
                    .iter()
                    .zip(buf.chunks.iter().skip(1))
                    .all(|(a, b)| a.len() >= SMALL_PACKET || b.len() >= SMALL_PACKET)
            );
        }
        while let Some(c) = buf.pop() {
            take_next(&mut sent, &c);
        }
        assert!(sent.is_empty(), "{} bytes never came out", sent.len());
        assert!(buf.is_empty());
    }

    /// A forwarded connection for a server-picked port can arrive before
    /// the requesting task has routed it; the lookup waits for the route.
    #[tokio::test]
    async fn port_zero_route_is_found_once_routed() {
        let routes = Arc::new(ForwardRoutes::default());
        let (tx, _rx) = mpsc::channel(1);
        let pending = routes.begin_pending();
        let lookup = tokio::spawn({
            let routes = routes.clone();
            async move { routes.find_when_routed("127.0.0.1", 40123).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !lookup.is_finished(),
            "waits while a port-0 forward is pending"
        );
        routes.insert(("127.0.0.1".into(), 40123), tx.clone());
        drop(pending);
        let found = tokio::time::timeout(Duration::from_secs(1), lookup)
            .await
            .expect("lookup ends once routed")
            .expect("lookup task");
        assert!(found.is_some_and(|t| t.same_channel(&tx)));
    }

    /// The requesting task routes the port and stops being pending right
    /// between the two steps of the fast-path lookup. The connection must
    /// still find its listener (or wait for it), never be rejected.
    #[test]
    fn fast_path_reads_pending_before_looking() {
        let routes = ForwardRoutes::default();
        let (tx, _rx) = mpsc::channel(1);
        routes.pending.send_modify(|n| *n += 1);
        let routed = tx.clone();
        *routes.lookup_hook.lock().unwrap() = Some(Box::new(move |r: &ForwardRoutes| {
            r.insert(("127.0.0.1".into(), 40125), routed);
            r.pending.send_modify(|n| *n -= 1);
        }));
        match routes.lookup_now("127.0.0.1", 40125) {
            Lookup::Now(Some(found)) => assert!(found.same_channel(&tx)),
            Lookup::Wait => {}
            Lookup::Now(None) => panic!("rejected a connection whose route was just inserted"),
        }
        assert!(routes.lookup_hook.lock().unwrap().is_none(), "hook ran");
    }

    #[test]
    fn fast_path_rejects_only_when_nothing_is_pending() {
        let routes = ForwardRoutes::default();
        assert!(matches!(
            routes.lookup_now("127.0.0.1", 1),
            Lookup::Now(None)
        ));
        let _pending = routes.begin_pending();
        assert!(matches!(routes.lookup_now("127.0.0.1", 1), Lookup::Wait));
    }

    #[tokio::test]
    async fn port_zero_wait_ends_when_nothing_is_pending() {
        let routes = ForwardRoutes::default();
        // Nothing pending: no wait at all.
        assert!(routes.find_when_routed("127.0.0.1", 40124).await.is_none());
        // A pending request that fails (never routes) ends the wait.
        let pending = routes.begin_pending();
        let fail = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(pending);
        };
        let (found, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(routes.find_when_routed("127.0.0.1", 40124), fail)
        })
        .await
        .expect("the wait ends with the request");
        assert!(found.is_none());
        assert_eq!(*routes.pending.borrow(), 0);
    }

    #[test]
    fn routes_match_exactly_then_by_unique_port() {
        let routes = ForwardRoutes::default();
        let (a, _ra) = mpsc::channel(1);
        let (b, _rb) = mpsc::channel(1);
        routes.insert(("localhost".into(), 8080), a.clone());
        assert!(routes.find("localhost", 8080).unwrap().same_channel(&a));
        assert!(routes.find("127.0.0.1", 8080).unwrap().same_channel(&a));
        assert!(routes.find("localhost", 8081).is_none());

        routes.insert(("0.0.0.0".into(), 8080), b.clone());
        assert!(routes.find("0.0.0.0", 8080).unwrap().same_channel(&b));
        assert!(routes.find("127.0.0.1", 8080).is_none(), "ambiguous");

        // Removing only removes a route that is still ours.
        routes.remove(&("localhost".into(), 8080), &b);
        assert!(routes.find("localhost", 8080).is_some());
        routes.remove(&("localhost".into(), 8080), &a);
        // Now the only listener on the port.
        assert!(routes.find("localhost", 8080).unwrap().same_channel(&b));
        routes.remove(&("0.0.0.0".into(), 8080), &b);
        assert!(routes.find("0.0.0.0", 8080).is_none());
    }
}
