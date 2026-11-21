//! The port-forwarding engine: local (`-L`), remote (`-R`) and dynamic
//! (`-D`, SOCKS5) forwards over one [`SshConnection`].
//!
//! [`Forward::start`] binds (or asks the server to bind) and returns the
//! bound address; [`Forward::serve`] then accepts connections until the SSH
//! connection closes; [`Forward::shutdown`] stops it.
//!
//! - **Local**: each accepted TCP connection opens a `direct-tcpip` channel
//!   to the destination (as seen from the server). If the channel cannot be
//!   opened, just that client socket is closed.
//! - **Dynamic**: each accepted connection speaks SOCKS5 (see `socks5.rs`;
//!   the handshake is bounded by [`SOCKS_HANDSHAKE_TIMEOUT`]); the target is
//!   opened as a `direct-tcpip` channel, domain names unresolved.
//! - **Remote**: the server listens (`tcpip-forward`); each
//!   `forwarded-tcpip` channel is connected to the local destination.
//!
//! Every per-connection task is owned by the [`Forward`] (a `JoinSet`), at
//! most [`MAX_CONNECTIONS`] at once: shutting it down closes the listener
//! and ends every copy in flight (and waits until the OS lets the port be
//! bound again), and dropping it aborts them. Data is copied both ways with half-close: when
//! one side finishes sending, the other side's write half is shut down (a
//! TCP FIN, or an SSH channel EOF) and the other direction keeps going until
//! it finishes too.
//!
//! All channels share the connection, so the known limitation in
//! `connection.rs` applies: while one channel's outbound data waits on the
//! server's window, writes and channel opens of the others wait too.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::connection::{ChannelStream, RemoteListener, SshConnection};
use crate::error::SshError;
use crate::socks5::{self, Reply};

/// Bound on a SOCKS client's handshake (greeting and request), and on
/// writing our reply.
pub const SOCKS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on connecting to a remote forward's local destination.
pub const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Copy buffer per direction.
const COPY_BUF: usize = 32 * 1024;
/// Connections (including SOCKS handshakes) one forward handles at once.
/// At the cap, the forward stops accepting until one ends: further local
/// clients wait in the kernel's listen backlog, and further remote ones in
/// the listener's queue (then the server's connection is rejected).
pub const MAX_CONNECTIONS: usize = 256;
/// Upper bound on waiting for the OS to release a closed listener's port;
/// see `wait_port_released`.
pub const PORT_RELEASE_WAIT: Duration = Duration::from_secs(1);
const PORT_RELEASE_POLL: Duration = Duration::from_millis(10);
/// Pause after a failed `accept` (for example out of file descriptors), so
/// the loop does not spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// What to forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardSpec {
    /// Listen on `bind_addr:bind_port` locally; connect to
    /// `dest_host:dest_port` from the server.
    Local {
        bind_addr: String,
        bind_port: u16,
        dest_host: String,
        dest_port: u16,
    },
    /// The server listens on `bind_addr:bind_port`; connect to
    /// `dest_host:dest_port` locally.
    Remote {
        bind_addr: String,
        bind_port: u16,
        dest_host: String,
        dest_port: u16,
    },
    /// A SOCKS5 proxy on `bind_addr:bind_port` locally; targets are
    /// connected from the server.
    Dynamic { bind_addr: String, bind_port: u16 },
}

/// Why a forward could not start, or stopped by itself.
#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    /// The local bind address is taken (`EADDRINUSE`).
    #[error("address in use")]
    AddrInUse,
    /// Any other failure to listen locally.
    #[error("cannot listen on {addr}: {source}")]
    Bind { addr: String, source: io::Error },
    /// The server refused (or did not answer) the remote forward request.
    #[error("remote forward refused: {0}")]
    Remote(SshError),
    /// The SSH connection is gone.
    #[error("connection closed")]
    Closed,
}

/// What listens, and where its connections go.
enum Kind {
    Local {
        listener: TcpListener,
        dest_host: String,
        dest_port: u16,
    },
    Dynamic {
        listener: TcpListener,
    },
    Remote {
        listener: RemoteListener,
        dest_host: String,
        dest_port: u16,
    },
}

/// A running forward; see the module docs.
pub struct Forward {
    conn: SshConnection,
    kind: Kind,
    bound: SocketAddr,
    tasks: JoinSet<()>,
    /// One permit per connection; see [`MAX_CONNECTIONS`].
    slots: Arc<Semaphore>,
}

impl std::fmt::Debug for Forward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Forward")
            .field("bound", &self.bound)
            .field("connections", &self.tasks.len())
            .finish_non_exhaustive()
    }
}

impl Forward {
    /// Binds the forward. Local and dynamic forwards listen on
    /// `bind_addr:bind_port` (an empty address means `127.0.0.1`, `*` means
    /// all interfaces, as in OpenSSH); remote forwards send `tcpip-forward`.
    /// Port 0 picks a free port; [`bound`](Self::bound) reports it.
    pub async fn start(conn: SshConnection, spec: &ForwardSpec) -> Result<Self, ForwardError> {
        if conn.is_closed() {
            return Err(ForwardError::Closed);
        }
        let (kind, bound) = match spec {
            ForwardSpec::Local {
                bind_addr,
                bind_port,
                dest_host,
                dest_port,
            } => {
                let (listener, bound) = bind_local(bind_addr, *bind_port).await?;
                let kind = Kind::Local {
                    listener,
                    dest_host: dest_host.clone(),
                    dest_port: *dest_port,
                };
                (kind, bound)
            }
            ForwardSpec::Dynamic {
                bind_addr,
                bind_port,
            } => {
                let (listener, bound) = bind_local(bind_addr, *bind_port).await?;
                (Kind::Dynamic { listener }, bound)
            }
            ForwardSpec::Remote {
                bind_addr,
                bind_port,
                dest_host,
                dest_port,
            } => {
                let listener = match conn.tcpip_forward(bind_addr, *bind_port).await {
                    Ok(l) => l,
                    Err(SshError::Closed) => return Err(ForwardError::Closed),
                    Err(e) => return Err(ForwardError::Remote(e)),
                };
                // The server reports the port, not the address.
                let port = u16::try_from(listener.port()).unwrap_or(*bind_port);
                let bound = SocketAddr::new(remote_bind_ip(bind_addr), port);
                let kind = Kind::Remote {
                    listener,
                    dest_host: dest_host.clone(),
                    dest_port: *dest_port,
                };
                (kind, bound)
            }
        };
        Ok(Self {
            conn,
            kind,
            bound,
            tasks: JoinSet::new(),
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        })
    }

    /// Lowers the connection cap (tests).
    #[cfg(test)]
    fn with_limit(mut self, limit: usize) -> Self {
        self.slots = Arc::new(Semaphore::new(limit));
        self
    }

    /// Where the forward listens. For a remote forward this is the port the
    /// server listens on, with the requested bind address when it is an IP
    /// literal (`localhost` is shown as 127.0.0.1, anything else as
    /// 0.0.0.0).
    pub fn bound(&self) -> SocketAddr {
        self.bound
    }

    /// Connections being forwarded right now.
    pub fn connections(&self) -> usize {
        self.tasks.len()
    }

    /// Accepts and forwards connections until the SSH connection closes;
    /// returns [`ForwardError::Closed`] then. At most [`MAX_CONNECTIONS`]
    /// are handled at once. Cancel-safe: dropping the future leaves the
    /// forward intact (call [`shutdown`](Self::shutdown) next).
    pub async fn serve(&mut self) -> ForwardError {
        let Self {
            conn,
            kind,
            tasks,
            slots,
            ..
        } = self;
        let closed = conn.close_signal().wait();
        tokio::pin!(closed);
        loop {
            tokio::select! {
                () = &mut closed => return ForwardError::Closed,
                // Reap finished connections so the set stays small.
                Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                    if let Err(e) = done
                        && e.is_panic()
                    {
                        warn!("a forwarded connection's task panicked");
                    }
                }
                // Accept only with a free slot; the slot goes with the
                // connection's task. (Both steps are cancel-safe.)
                (slot, next) = async {
                    let slot = slots.clone().acquire_owned().await;
                    (slot, next_connection(conn, kind).await)
                } => match next {
                    Next::Spawn(task) => {
                        tasks.spawn(async move {
                            let _slot = slot;
                            task.await;
                        });
                    }
                    Next::Retry => {}
                    Next::Closed => return ForwardError::Closed,
                },
            }
        }
    }

    /// Stops the forward: stops listening (a remote forward is cancelled on
    /// the server, waiting at most the connection's setup timeout for the
    /// reply), then ends every connection in flight and waits for their
    /// tasks. For a local listener it then waits (at most
    /// [`PORT_RELEASE_WAIT`]) until the OS lets the port be bound again, so
    /// when it returns, the local port is free.
    pub async fn shutdown(self) {
        let Self {
            conn,
            kind,
            mut tasks,
            bound,
            ..
        } = self;
        match kind {
            Kind::Local { listener, .. } | Kind::Dynamic { listener } => {
                drop(listener);
                tasks.shutdown().await;
                wait_port_released(bound).await;
            }
            Kind::Remote { listener, .. } => {
                if let Err(e) = listener.cancel().await {
                    debug!(error = %e, "cancel-tcpip-forward failed");
                }
                tasks.shutdown().await;
            }
        }
        drop(conn);
    }
}

type ConnectionTask = Pin<Box<dyn Future<Output = ()> + Send>>;

enum Next {
    /// A new connection, to be run by the forward.
    Spawn(ConnectionTask),
    /// A transient accept error; try again.
    Retry,
    /// The SSH connection is gone.
    Closed,
}

/// Waits for the next connection. Cancel-safe (both accepts are).
async fn next_connection(conn: &SshConnection, kind: &mut Kind) -> Next {
    match kind {
        Kind::Local {
            listener,
            dest_host,
            dest_port,
        } => match accept_tcp(listener).await {
            Some((sock, peer)) => Next::Spawn(Box::pin(local_connection(
                conn.clone(),
                sock,
                peer,
                dest_host.clone(),
                *dest_port,
            ))),
            None => Next::Retry,
        },
        Kind::Dynamic { listener } => match accept_tcp(listener).await {
            Some((sock, peer)) => Next::Spawn(Box::pin(socks_connection(conn.clone(), sock, peer))),
            None => Next::Retry,
        },
        Kind::Remote {
            listener,
            dest_host,
            dest_port,
        } => match listener.accept().await {
            Some(fc) => Next::Spawn(Box::pin(remote_connection(
                fc.stream,
                dest_host.clone(),
                *dest_port,
            ))),
            None => Next::Closed,
        },
    }
}

/// The next client, or `None` after a failed accept. Accept errors on a
/// bound listener are per connection or resource exhaustion, so the caller
/// keeps listening; the pause keeps the loop from spinning.
async fn accept_tcp(listener: &TcpListener) -> Option<(TcpStream, SocketAddr)> {
    match listener.accept().await {
        Ok((sock, peer)) => {
            let _ = sock.set_nodelay(true);
            Some((sock, peer))
        }
        Err(e) => {
            warn!(error = %e, "accept failed");
            tokio::time::sleep(ACCEPT_BACKOFF).await;
            None
        }
    }
}

/// Waits until `addr` can be bound again (with `SO_REUSEADDR` on unix, as
/// our listeners are; Windows listeners are exclusive instead, see
/// [`bind_local`]), at most [`PORT_RELEASE_WAIT`].
///
/// Every socket of ours on the port is closed by now, but macOS can still
/// refuse the bind (`EADDRINUSE`) for a moment while a just-closed
/// connection on it is in a closing state (seen in testing: a server-side
/// socket in FIN_WAIT, no file descriptor left, bind succeeding a few
/// hundred milliseconds later). Without this wait, a rule restarted on the
/// same port right after a stop could fail with "address in use".
async fn wait_port_released(addr: SocketAddr) {
    let deadline = tokio::time::Instant::now() + PORT_RELEASE_WAIT;
    loop {
        let probe = match addr {
            SocketAddr::V4(_) => TcpSocket::new_v4(),
            SocketAddr::V6(_) => TcpSocket::new_v6(),
        };
        let free = probe.and_then(|s| {
            // Not on Windows: there `SO_REUSEADDR` lets a bind take over a
            // port that is in use, so the probe would always "succeed".
            #[cfg(not(windows))]
            s.set_reuseaddr(true)?;
            s.bind(addr)
        });
        match free {
            Ok(()) => return,
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {}
            // Anything else will not get better by waiting.
            Err(e) => return debug!(error = %e, %addr, "port release probe failed"),
        }
        if tokio::time::Instant::now() >= deadline {
            return debug!(%addr, "port still in use after the forward closed");
        }
        tokio::time::sleep(PORT_RELEASE_POLL).await;
    }
}

/// One local-forward client: open the channel, copy.
async fn local_connection(
    conn: SshConnection,
    sock: TcpStream,
    peer: SocketAddr,
    host: String,
    port: u16,
) {
    match conn.open_direct_tcpip(&host, port, peer).await {
        Ok(channel) => pipe(sock, channel).await,
        // Dropping `sock` closes just this client.
        Err(e) => debug!(error = %e, %peer, "direct-tcpip open failed"),
    }
}

/// One remote-forward connection: connect locally, copy.
async fn remote_connection(stream: ChannelStream, host: String, port: u16) {
    match timeout(
        LOCAL_CONNECT_TIMEOUT,
        TcpStream::connect((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(sock)) => {
            let _ = sock.set_nodelay(true);
            pipe(sock, stream).await;
        }
        // Dropping `stream` closes the channel.
        Ok(Err(e)) => debug!(error = %e, %host, port, "local connect failed"),
        Err(_) => debug!(%host, port, "local connect timed out"),
    }
}

/// Listens locally. `""` means loopback and `"*"` all interfaces, as in
/// OpenSSH; anything else is an address or a name (resolved; the first
/// address that binds wins).
async fn bind_local(bind_addr: &str, port: u16) -> Result<(TcpListener, SocketAddr), ForwardError> {
    let host = match bind_addr {
        "" => "127.0.0.1",
        "*" => "0.0.0.0",
        other => other,
    };
    let bind_err = |source: io::Error| {
        if source.kind() == io::ErrorKind::AddrInUse {
            ForwardError::AddrInUse
        } else {
            ForwardError::Bind {
                addr: format!("{host}:{port}"),
                source,
            }
        }
    };
    let listener = listen(host, port).await.map_err(bind_err)?;
    let bound = listener.local_addr().map_err(bind_err)?;
    Ok((listener, bound))
}

#[cfg(not(windows))]
async fn listen(host: &str, port: u16) -> io::Result<TcpListener> {
    // tokio sets `SO_REUSEADDR` here, so a quick rebind after a stop works.
    TcpListener::bind((host, port)).await
}

/// Windows: `SO_REUSEADDR` would let another process (or a second rule)
/// bind the same port, so claim it exclusively before binding; an occupied
/// port then fails with "address in use" as on unix. Like
/// `TcpListener::bind`, the first resolved address that binds wins.
#[cfg(windows)]
async fn listen(host: &str, port: u16) -> io::Result<TcpListener> {
    let mut last = None;
    for addr in tokio::net::lookup_host((host, port)).await? {
        match listen_exclusive(addr) {
            Ok(listener) => return Ok(listener),
            Err(e) => last = Some(e),
        }
    }
    Err(last
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no address to listen on")))
}

#[cfg(windows)]
fn listen_exclusive(addr: SocketAddr) -> io::Result<TcpListener> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        SO_EXCLUSIVEADDRUSE, SOCKET_ERROR, SOL_SOCKET, WSAGetLastError, setsockopt,
    };

    let socket = match addr {
        SocketAddr::V4(_) => TcpSocket::new_v4()?,
        SocketAddr::V6(_) => TcpSocket::new_v6()?,
    };
    let on: u32 = 1;
    // SAFETY: the socket is open and owned by `socket` for the call; the
    // option value is a live `u32` and the length passed is its size.
    let rc = unsafe {
        setsockopt(
            socket.as_raw_socket() as usize,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            std::ptr::from_ref(&on).cast(),
            std::mem::size_of::<u32>() as i32,
        )
    };
    if rc == SOCKET_ERROR {
        // SAFETY: plain FFI call, no arguments; reads this thread's Winsock
        // error, so it must come right after the failed call.
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    // Against an exclusive listener Windows answers a bind from a socket that
    // set `SO_REUSEADDR` with WSAEACCES, not WSAEADDRINUSE. For us both mean
    // the port is taken. WSAEACCES is also what a bind to an excluded or
    // reserved port range (Hyper-V, WinNAT, WSL) gets, so those ports read
    // "address in use" too; they are not usable either.
    socket.bind(addr).map_err(|e| match e.kind() {
        io::ErrorKind::PermissionDenied => io::Error::from(io::ErrorKind::AddrInUse),
        _ => e,
    })?;
    // tokio's `TcpListener::bind` uses the same backlog.
    socket.listen(1024)
}

/// The address a remote forward's status shows; see [`Forward::bound`].
fn remote_bind_ip(bind_addr: &str) -> IpAddr {
    match bind_addr {
        "localhost" => Ipv4Addr::LOCALHOST.into(),
        other => other
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .unwrap_or(Ipv4Addr::UNSPECIFIED.into()),
    }
}

/// One dynamic-forward client: SOCKS5 handshake, open the target, reply,
/// copy.
async fn socks_connection(conn: SshConnection, mut sock: TcpStream, peer: SocketAddr) {
    let target = match timeout(SOCKS_HANDSHAKE_TIMEOUT, socks5::handshake(&mut sock)).await {
        Ok(Ok(target)) => target,
        Ok(Err(e)) => {
            debug!(error = %e, %peer, "socks handshake failed");
            return;
        }
        Err(_) => {
            debug!(%peer, "socks handshake timed out");
            return;
        }
    };
    let (host, port) = target.host_port();
    let (code, channel) = match conn.open_direct_tcpip(&host, port, peer).await {
        Ok(channel) => (Reply::Succeeded, Some(channel)),
        Err(e) => {
            debug!(error = %e, %host, port, "socks target could not be opened");
            (open_failure_reply(&e), None)
        }
    };
    // The address the client reached us on, as the proxy's bound address.
    let bound = sock
        .local_addr()
        .unwrap_or(SocketAddr::from(([0, 0, 0, 0], 0)));
    match timeout(
        SOCKS_HANDSHAKE_TIMEOUT,
        socks5::reply(&mut sock, code, bound),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return debug!(error = %e, %peer, "socks reply failed"),
        Err(_) => return debug!(%peer, "socks reply timed out"),
    }
    if let Some(channel) = channel {
        pipe(sock, channel).await;
    }
}

/// The SOCKS reply for a `direct-tcpip` open that failed.
fn open_failure_reply(e: &SshError) -> Reply {
    match e {
        // The server's reason code only survives as text.
        SshError::Channel(msg) if msg.contains("AdministrativelyProhibited") => Reply::NotAllowed,
        SshError::Channel(msg) if msg.contains("ConnectFailed") => Reply::HostUnreachable,
        SshError::Channel(msg) if msg.contains("timed out") => Reply::HostUnreachable,
        _ => Reply::GeneralFailure,
    }
}

/// Copies both ways until both directions finished, with half-close (see
/// the module docs). An error in either direction ends both.
async fn pipe<A, B>(mut a: A, mut b: B)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    match tokio::io::copy_bidirectional_with_sizes(&mut a, &mut b, COPY_BUF, COPY_BUF).await {
        Ok((out, back)) => debug!(out, back, "forwarded connection finished"),
        Err(e) => debug!(error = %e, "forwarded connection ended with an error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn bind_conflict_is_addr_in_use() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let err = bind_local("127.0.0.1", port).await.unwrap_err();
        assert!(matches!(err, ForwardError::AddrInUse), "{err:?}");
        assert_eq!(err.to_string(), "address in use");
    }

    #[tokio::test]
    async fn other_bind_errors_name_the_address() {
        // TEST-NET-1: not a local address.
        let err = bind_local("192.0.2.1", 0).await.unwrap_err();
        assert!(matches!(err, ForwardError::Bind { .. }), "{err:?}");
        assert!(
            err.to_string()
                .starts_with("cannot listen on 192.0.2.1:0: "),
            "{err}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn port_release_wait_is_bounded() {
        // Free: returns at once.
        let (l, bound) = bind_local("127.0.0.1", 0).await.unwrap();
        drop(l);
        let start = tokio::time::Instant::now();
        wait_port_released(bound).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        // Still listening (someone else's): gives up after the bound.
        let (_l, bound) = bind_local("127.0.0.1", 0).await.unwrap();
        let start = tokio::time::Instant::now();
        wait_port_released(bound).await;
        let waited = start.elapsed();
        assert!(
            waited >= PORT_RELEASE_WAIT && waited < PORT_RELEASE_WAIT + Duration::from_millis(100),
            "{waited:?}"
        );
    }

    /// Our listeners must not share a port with a `SO_REUSEADDR` socket,
    /// which on Windows can take over a port in use. Note: on current
    /// Windows 11 a plain (tokio) listener already refuses this bind from
    /// the same user, so this passes with or without `SO_EXCLUSIVEADDRUSE`
    /// (checked on the VM by removing the setsockopt). The option is
    /// defence in depth (it also covers other accounts' sockets and older
    /// Windows); no VM-observable case separates the two.
    #[cfg(windows)]
    #[tokio::test]
    async fn reuseaddr_socket_cannot_take_a_forward_port() {
        let (_l, bound) = bind_local("127.0.0.1", 0).await.unwrap();
        let sock = TcpSocket::new_v4().unwrap();
        sock.set_reuseaddr(true).unwrap();
        // Windows reports WSAEACCES here, WSAEADDRINUSE on some versions.
        let err = sock.bind(bound).unwrap_err();
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied
            ),
            "{err:?}"
        );
    }

    /// The reverse: a port another process holds with `SO_REUSEADDR` is
    /// still "address in use" for us, not a permission error.
    #[cfg(windows)]
    #[tokio::test]
    async fn port_held_with_reuseaddr_is_addr_in_use() {
        let holder = TcpSocket::new_v4().unwrap();
        holder.set_reuseaddr(true).unwrap();
        holder.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let listener = holder.listen(1).unwrap();
        let port = listener.local_addr().unwrap().port();
        let err = bind_local("127.0.0.1", port).await.unwrap_err();
        assert!(matches!(err, ForwardError::AddrInUse), "{err:?}");
    }

    /// A rule restarted on its port right after a stop: our side closes the
    /// accepted connection and then the listener (as a stop does), and the
    /// port must come back within the probe's bound. Guards the Windows
    /// `SO_EXCLUSIVEADDRUSE` listener against a lingering closed connection.
    #[tokio::test]
    async fn port_is_rebindable_after_our_side_closes() {
        let (listener, bound) = bind_local("127.0.0.1", 0).await.unwrap();
        let client = TcpStream::connect(bound).await.unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        drop(accepted);
        drop(listener);
        wait_port_released(bound).await;
        let (_again, rebound) = bind_local("127.0.0.1", bound.port()).await.unwrap();
        assert_eq!(rebound, bound);
        drop(client);
    }

    #[tokio::test]
    async fn empty_bind_address_is_loopback() {
        let (_l, bound) = bind_local("", 0).await.unwrap();
        assert_eq!(bound.ip(), IpAddr::from(Ipv4Addr::LOCALHOST));
        assert_ne!(bound.port(), 0);
    }

    #[test]
    fn remote_bind_ip_display() {
        assert_eq!(
            remote_bind_ip("localhost"),
            IpAddr::from(Ipv4Addr::LOCALHOST)
        );
        assert_eq!(
            remote_bind_ip("127.0.0.1"),
            IpAddr::from(Ipv4Addr::LOCALHOST)
        );
        assert_eq!(remote_bind_ip("[::1]"), "::1".parse::<IpAddr>().unwrap());
        assert_eq!(remote_bind_ip(""), IpAddr::from(Ipv4Addr::UNSPECIFIED));
        assert_eq!(
            remote_bind_ip("example.org"),
            IpAddr::from(Ipv4Addr::UNSPECIFIED)
        );
    }

    #[test]
    fn open_failures_map_to_socks_replies() {
        use russh::ChannelOpenFailure as F;
        let channel = |f| SshError::Channel(russh::Error::ChannelOpenFailure(f).to_string());
        assert_eq!(
            open_failure_reply(&channel(F::AdministrativelyProhibited)),
            Reply::NotAllowed
        );
        assert_eq!(
            open_failure_reply(&channel(F::ConnectFailed)),
            Reply::HostUnreachable
        );
        assert_eq!(
            open_failure_reply(&SshError::Channel("timed out".into())),
            Reply::HostUnreachable
        );
        assert_eq!(open_failure_reply(&SshError::Closed), Reply::GeneralFailure);
    }

    /// At the cap, the forward holds further accepts until a connection
    /// ends. Needs the fixture (a forward needs an SSH connection).
    #[tokio::test]
    async fn connections_over_the_cap_wait_for_a_slot() {
        if std::env::var("TETHER_SSH_FIXTURE").as_deref() != Ok("1") {
            eprintln!("skipping: set TETHER_SSH_FIXTURE=1 (and run tests/fixtures/up.sh)");
            return;
        }
        let params = crate::ConnectParams {
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
        let spec = ForwardSpec::Dynamic {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
        };
        let mut forward = Forward::start(conn, &spec)
            .await
            .expect("start")
            .with_limit(2);
        let bound = forward.bound();
        let serving = tokio::spawn(async move {
            forward.serve().await;
        });

        // Two silent SOCKS clients take both slots (their handshakes wait).
        let silent = TcpStream::connect(bound).await.unwrap();
        let _silent2 = TcpStream::connect(bound).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        // A third connects (the kernel's backlog) but is not served...
        let mut third = TcpStream::connect(bound).await.unwrap();
        third.write_all(&[5, 1, 0]).await.unwrap();
        let mut method = [0u8; 2];
        assert!(
            timeout(Duration::from_millis(500), third.read_exact(&mut method))
                .await
                .is_err(),
            "served beyond the cap"
        );
        // ...until a slot frees up.
        drop(silent);
        timeout(Duration::from_secs(5), third.read_exact(&mut method))
            .await
            .expect("served once a slot is free")
            .unwrap();
        assert_eq!(method, [5, 0]);
        serving.abort();
    }

    /// Half-close: one side's EOF reaches the other as a shutdown, and the
    /// other direction keeps flowing until it ends too.
    #[tokio::test]
    async fn pipe_carries_half_close_both_ways() {
        let (mut client, a) = tokio::io::duplex(1024);
        let (b, mut server) = tokio::io::duplex(1024);
        let piping = tokio::spawn(pipe(a, b));

        client.write_all(b"request").await.unwrap();
        client.shutdown().await.unwrap();
        let mut got = Vec::new();
        server.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"request", "the server sees the client's EOF");

        // The server can still answer after the client's half-close.
        server.write_all(b"response").await.unwrap();
        server.shutdown().await.unwrap();
        let mut got = Vec::new();
        client.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"response");
        tokio::time::timeout(Duration::from_secs(1), piping)
            .await
            .expect("pipe ends once both directions are done")
            .unwrap();
    }
}
