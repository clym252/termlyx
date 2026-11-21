//! Application services for Tether.
//!
//! * Session manager: batches terminal output into frames and applies UI-ack
//!   backpressure on top of a [`core_ssh::SshTransport`].
//! * [`vault`]: the encrypted local vault ([`vault::VaultService`]).
//! * [`import`]: turns an `~/.ssh/config` into a vault import plan.
//! * Prompts: [`PromptBridge`] carries questions for the user (host keys,
//!   passwords, passphrases, keyboard-interactive) to the UI and back, and
//!   [`VaultHostKeyVerifier`] checks host keys against the vault's known
//!   hosts.
//! * Transfers: [`TransferQueue`] uploads and downloads files over a
//!   [`core_ssh::SftpClient`] with progress, cancel, resume and conflict
//!   policies.
//! * Forwards: [`ForwardManager`] runs local, remote and dynamic (SOCKS5)
//!   port-forward rules on an [`core_ssh::SshConnection`] and reports each
//!   rule's [`ForwardStatus`].

mod batcher;
mod forwards;
pub mod import;
mod known_hosts;
// Local terminals: desktop only (iOS and Android apps cannot spawn a shell).
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod local_pty;
mod prompts;
mod save_password;
mod session;
mod transfers;
pub mod vault;

use std::time::Duration;

pub use forwards::{ForwardEvent, ForwardManager, ForwardStatus, RuleHandle};
pub use known_hosts::VaultHostKeyVerifier;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub use local_pty::{LocalShell, LocalShellTransport, local_shell, local_shells};
pub use prompts::{
    EmitPrompt, PROMPT_TIMEOUT, Prompt, PromptBridge, PromptResponse, SaveTarget, WithdrawPrompt,
};
pub use save_password::SavingPrompter;
pub use session::{OutputSink, SessionId, SessionManager, ShellTarget};
pub use transfers::{
    CHUNK_SIZE, ConflictPolicy, Direction, JobId, PROGRESS_INTERVAL, TransferEvent, TransferJob,
    TransferQueue,
};

/// How often buffered output is flushed to the sink as a frame.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// Upper bound on the size of a single frame handed to the sink.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
/// Reading from the remote pauses while this many bytes (or more) have been
/// delivered to the sink but not yet acknowledged via [`SessionManager::ack`].
pub const UNACKED_HIGH_WATERMARK: usize = 4 * 1024 * 1024;
