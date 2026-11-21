use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use secrecy::SecretString;
use tokio::sync::{mpsc, oneshot};

use crate::connection::SshConnection;
use crate::error::SshError;
use crate::hostkey::HostKeyVerifier;
use crate::progress::ConnectProgress;
use crate::prompter::AuthPrompter;

pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    /// Terminal type, e.g. `xterm-256color`.
    pub term: String,
    pub cols: u16,
    pub rows: u16,
    /// Bounds the network waits of each connect phase: TCP connect, key
    /// exchange, and authentication (server replies); on an
    /// [`SshConnection`] it also bounds each channel's setup. Typically 15 s.
    ///
    /// Human waits do not count: while [`HostKeyVerifier::verify`] or an
    /// [`AuthPrompter`] call is outstanding the deadline is suspended, and it
    /// resumes (with the time left) once the call returns. Verifiers and
    /// prompters bound their own waits.
    pub connect_timeout: Duration,
    /// Interval between keepalive messages on the connection. `None` uses
    /// [`DEFAULT_KEEPALIVE`]; a zero interval turns keepalives off.
    pub keepalive: Option<Duration>,
    /// Environment variables requested, in order, before every shell
    /// started on the connection (`env` channel requests, sent without
    /// asking for a reply). Servers accept only names their `AcceptEnv`
    /// allows (OpenSSH accepts none by default); a refusal is ignored.
    pub env: Vec<(String, String)>,
}

/// Keepalive interval when [`ConnectParams::keepalive`] is `None`.
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(30);

impl ConnectParams {
    /// The keepalive interval for the russh client config: the default when
    /// unset, none at all for a zero interval.
    pub fn keepalive_interval(&self) -> Option<Duration> {
        match self.keepalive {
            None => Some(DEFAULT_KEEPALIVE),
            Some(d) if d.is_zero() => None,
            Some(d) => Some(d),
        }
    }
}

impl std::fmt::Debug for ConnectParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectParams")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("term", &self.term)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("connect_timeout", &self.connect_timeout)
            .field("keepalive", &self.keepalive)
            // Names only: values may be sensitive.
            .field(
                "env",
                &self.env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

pub enum AuthMethod {
    Password(SecretString),
    /// A private key in any format `decode_secret_key` reads. If it is
    /// encrypted and `passphrase` is `None`, the passphrase is asked for via
    /// [`AuthPrompter::passphrase`].
    PrivateKey {
        pem: SecretString,
        passphrase: Option<SecretString>,
    },
    /// No stored secret: ask. Uses keyboard-interactive (each challenge goes
    /// to [`AuthPrompter::keyboard_interactive`]) when the server offers it,
    /// and otherwise, or when the server fails keyboard-interactive without
    /// asking anything, a password from [`AuthPrompter::password`].
    KeyboardInteractive,
}

#[async_trait]
pub trait SshTransport: Send + Sync {
    /// Opens a shell with no one to ask for missing secrets
    /// (see [`NoPrompter`](crate::NoPrompter)).
    async fn open_shell(
        &self,
        p: ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
    ) -> Result<ShellHandle, SshError>;

    /// Opens a shell, asking `prompter` for secrets the [`AuthMethod`] does
    /// not carry. The default ignores `prompter` and calls
    /// [`open_shell`](Self::open_shell) (for transports that never prompt,
    /// e.g. test fakes).
    async fn open_shell_with_prompter(
        &self,
        p: ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
        prompter: Arc<dyn AuthPrompter>,
    ) -> Result<ShellHandle, SshError> {
        let _ = prompter;
        self.open_shell(p, v).await
    }

    /// Connects and authenticates, for opening several channels (shells,
    /// SFTP, forwards) on one connection; asks `prompter` for secrets the
    /// [`AuthMethod`] does not carry. `p.term`, `p.cols` and `p.rows` are
    /// unused. The default reports that the transport cannot do this (for
    /// transports that only open shells, e.g. test fakes).
    async fn connect(
        &self,
        p: &ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
        prompter: Arc<dyn AuthPrompter>,
    ) -> Result<SshConnection, SshError> {
        let _ = (p, v, prompter);
        Err(SshError::Channel(
            "connect unsupported by this transport".into(),
        ))
    }

    /// [`connect`](Self::connect) that reports each connect step to
    /// `progress`. The default reports nothing (for transports with no
    /// steps to report, e.g. test fakes).
    async fn connect_with_progress(
        &self,
        p: &ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
        prompter: Arc<dyn AuthPrompter>,
        progress: Arc<dyn ConnectProgress>,
    ) -> Result<SshConnection, SshError> {
        let _ = progress;
        self.connect(p, v, prompter).await
    }
}

/// Messages from a [`ShellControl`] to the task driving the channel.
#[derive(Debug)]
pub enum ControlMsg {
    Write(Bytes),
    Resize { cols: u16, rows: u16 },
    Close,
}

/// How a shell ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEnd {
    /// The shell finished: the remote side closed the channel (or the shell
    /// was closed locally). `exit_status` is the remote command's status
    /// when the server reported one.
    Exited { exit_status: Option<u32> },
    /// The connection or the channel failed (a transport error, a
    /// disconnect, a protocol violation), with a human-readable reason.
    Lost(String),
}

/// An open interactive shell. `output` yields remote bytes and closes (returns
/// `None`) on EOF or disconnect; `end` says which, and is resolved before
/// `output` closes.
pub struct ShellHandle {
    pub output: mpsc::Receiver<Bytes>,
    pub control: ShellControl,
    /// How the shell ended. A sender dropped without a value (shells that do
    /// not report it) means a normal end without an exit status.
    pub end: oneshot::Receiver<ShellEnd>,
}

impl ShellHandle {
    pub(crate) fn new(
        output: mpsc::Receiver<Bytes>,
        control: ShellControl,
        end: oneshot::Receiver<ShellEnd>,
    ) -> Self {
        Self {
            output,
            control,
            end,
        }
    }

    /// Build a handle from raw parts: a constructor for shells that are not
    /// driven by russh (the local PTY shell, fakes in tests). `output` must
    /// close when the shell ends; `control` messages are applied by whoever
    /// owns the matching receiver. The shell reports a normal end unless
    /// [`with_end`](Self::with_end) gives it a way to say otherwise.
    pub fn from_parts(output: mpsc::Receiver<Bytes>, control: ShellControl) -> Self {
        let (_, end) = oneshot::channel();
        Self::new(output, control, end)
    }

    /// Replaces how the end is reported: send a [`ShellEnd`] on the matching
    /// sender before closing `output`.
    pub fn with_end(mut self, end: oneshot::Receiver<ShellEnd>) -> Self {
        self.end = end;
        self
    }
}

#[derive(Clone)]
pub struct ShellControl {
    tx: mpsc::Sender<ControlMsg>,
}

impl ShellControl {
    pub(crate) fn new(tx: mpsc::Sender<ControlMsg>) -> Self {
        Self { tx }
    }

    /// Build a control handle backed by a channel you own, for shells that are
    /// not driven by russh (the local PTY shell, fakes in tests).
    pub fn from_sender(tx: mpsc::Sender<ControlMsg>) -> Self {
        Self::new(tx)
    }

    pub async fn write(&self, b: Bytes) -> Result<(), SshError> {
        self.send(ControlMsg::Write(b)).await
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshError> {
        self.send(ControlMsg::Resize { cols, rows }).await
    }

    /// Ask the shell to close. Idempotent; a no-op if already closed.
    pub async fn close(&self) {
        let _ = self.tx.send(ControlMsg::Close).await;
    }

    async fn send(&self, msg: ControlMsg) -> Result<(), SshError> {
        self.tx.send(msg).await.map_err(|_| SshError::Closed)
    }
}

#[cfg(test)]
mod params_tests {
    use super::*;

    fn params(keepalive: Option<Duration>) -> ConnectParams {
        ConnectParams {
            host: "h".into(),
            port: 22,
            username: "u".into(),
            auth: AuthMethod::KeyboardInteractive,
            term: "xterm-256color".into(),
            cols: 80,
            rows: 24,
            connect_timeout: Duration::from_secs(15),
            keepalive,
            env: vec![("LANG".into(), "secret-ish-value".into())],
        }
    }

    #[test]
    fn keepalive_interval_defaults_and_zero_disables() {
        assert_eq!(params(None).keepalive_interval(), Some(DEFAULT_KEEPALIVE));
        assert_eq!(
            params(Some(Duration::from_secs(10))).keepalive_interval(),
            Some(Duration::from_secs(10))
        );
        assert_eq!(params(Some(Duration::ZERO)).keepalive_interval(), None);
    }

    #[test]
    fn debug_shows_env_names_not_values() {
        let shown = format!("{:?}", params(None));
        assert!(
            shown.contains("LANG") && !shown.contains("secret-ish-value"),
            "{shown}"
        );
    }
}

#[cfg(all(test, feature = "test-util"))]
mod tests {
    use super::*;

    fn fake() -> (mpsc::Sender<Bytes>, mpsc::Receiver<ControlMsg>, ShellHandle) {
        let (out_tx, out_rx) = mpsc::channel(4);
        let (ctl_tx, ctl_rx) = mpsc::channel(4);
        let handle = ShellHandle::from_parts(out_rx, ShellControl::from_sender(ctl_tx));
        (out_tx, ctl_rx, handle)
    }

    #[tokio::test]
    async fn from_parts_wires_output_and_control() {
        let (out_tx, mut ctl_rx, mut handle) = fake();

        out_tx.send(Bytes::from_static(b"hi")).await.unwrap();
        assert_eq!(
            handle.output.recv().await.unwrap(),
            Bytes::from_static(b"hi")
        );

        handle
            .control
            .write(Bytes::from_static(b"ls\n"))
            .await
            .unwrap();
        handle.control.resize(100, 30).await.unwrap();
        handle.control.close().await;
        assert!(matches!(ctl_rx.recv().await, Some(ControlMsg::Write(b)) if &b[..] == b"ls\n"));
        assert!(matches!(
            ctl_rx.recv().await,
            Some(ControlMsg::Resize {
                cols: 100,
                rows: 30
            })
        ));
        assert!(matches!(ctl_rx.recv().await, Some(ControlMsg::Close)));

        drop(out_tx);
        assert!(handle.output.recv().await.is_none());
    }

    #[tokio::test]
    async fn control_returns_closed_after_receiver_dropped() {
        let (_out_tx, ctl_rx, handle) = fake();
        drop(ctl_rx);
        assert!(matches!(
            handle.control.write(Bytes::from_static(b"x")).await,
            Err(SshError::Closed)
        ));
        assert!(matches!(
            handle.control.resize(80, 24).await,
            Err(SshError::Closed)
        ));
        handle.control.close().await; // must not panic
    }
}
