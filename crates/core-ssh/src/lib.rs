//! SSH engine for Tether: a transport trait plus a russh-backed implementation,
//! a shared connection with channels (shells, SFTP) and port forwarding.

mod connection;
mod deadline;
mod error;
mod forward;
mod hostkey;
pub mod keys;
mod known_hosts_file;
mod progress;
mod prompter;
mod russh_impl;
mod sftp;
mod socks5;
mod tmux;
mod transport;

pub use connection::{
    ChannelStream, ClosedSignal, ExecOutput, ForwardedConnection, RemoteListener, SftpChannel,
    ShellOptions, SshConnection,
};
pub use error::SshError;
pub use forward::{
    Forward, ForwardError, ForwardSpec, LOCAL_CONNECT_TIMEOUT, MAX_CONNECTIONS, PORT_RELEASE_WAIT,
    SOCKS_HANDSHAKE_TIMEOUT,
};
pub use hostkey::{HostKeyDecision, HostKeyInfo, HostKeyVerifier};
pub use known_hosts_file::{HASHED_PORT, KnownHostEntry, host_matches, parse_known_hosts};
pub use progress::{ConnectProgress, ConnectStep, NoProgress};
pub use prompter::{AuthPrompter, NoPrompter};
pub use russh_impl::RusshTransport;
pub use sftp::{RemoteEntry, RemoteFile, SftpClient};
pub use tmux::{TAB_ENV, parse_tmux_tabs, tmux_reattach_command, tmux_tabs_command};
pub use transport::{
    AuthMethod, ConnectParams, ControlMsg, DEFAULT_KEEPALIVE, ShellControl, ShellEnd, ShellHandle,
    SshTransport,
};

#[cfg(feature = "test-util")]
pub use hostkey::AcceptAll;
