//! The prompt bridge: questions for the human (host keys, passwords,
//! passphrases, keyboard-interactive challenges) go out through an emit
//! callback (the app turns them into UI events) and come back through
//! [`PromptBridge::respond`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use core_ssh::{AuthPrompter, HostKeyInfo};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::transfers::ConflictPolicy;

/// How long a prompt waits for an answer before it counts as
/// [`PromptResponse::Reject`]. Below OpenSSH's default `LoginGraceTime`
/// (120 s), so the user sees our timeout rather than a server disconnect.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(90);

/// A question for the user. Serializes as `{"kind": "hostKeyUnknown", ...}`
/// with camelCase field names. Carries no secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Prompt {
    /// First connection to this host (or no key of this type known yet).
    HostKeyUnknown {
        host: String,
        port: u16,
        key: HostKeyInfo,
    },
    /// The host presented a different key than the one stored for it: a
    /// blocking warning whose default answer is Reject.
    HostKeyChanged {
        host: String,
        port: u16,
        old: HostKeyInfo,
        new: HostKeyInfo,
    },
    Passphrase {
        key_label: String,
    },
    /// One keyboard-interactive round; `prompts` are `(text, echo)`.
    /// `save_to` is set when the answer may be saved (a single hidden
    /// prompt on a vault host); see [`SaveTarget`].
    KeyboardInteractive {
        name: String,
        instructions: String,
        prompts: Vec<(String, bool)>,
        save_to: Option<SaveTarget>,
    },
    /// A login password. `save_to` is set when it may be saved.
    Password {
        user: String,
        host: String,
        save_to: Option<SaveTarget>,
    },
    /// A transfer's destination already exists (policy
    /// [`ConflictPolicy::Ask`]); answered with [`PromptResponse::Conflict`].
    /// `path` is the destination (an SFTP path for uploads, a local one for
    /// downloads).
    TransferConflict {
        job_id: String,
        path: String,
        existing_size: u64,
        incoming_size: u64,
    },
}

/// Where a password typed at a prompt would be saved: on the host's
/// identity `identity_label`, or (`is_new`) on a new identity of that name
/// linked to the host. Serializes as `{"identityLabel": "...", "isNew": false}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveTarget {
    pub identity_label: String,
    pub is_new: bool,
}

/// The user's answer. Deserializes from `{"accept": {"save": true}}`,
/// `"reject"`, `{"secret": "..."}`, `{"saveSecret": "..."}`,
/// `{"answers": ["...", ...]}`, `{"saveAnswers": ["..."]}` or
/// `{"conflict": "rename"}`; secrets
/// go straight into [`SecretString`] and `Debug` redacts them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PromptResponse {
    /// Trust the host key; `save` stores it as a known host.
    Accept {
        save: bool,
    },
    Reject,
    /// A password or passphrase.
    Secret(SecretString),
    /// A password the user asked to save to the vault (only meaningful for
    /// a prompt with `save_to`; otherwise the same as `Secret`).
    SaveSecret(SecretString),
    /// Keyboard-interactive answers, one per prompt.
    Answers(Vec<SecretString>),
    /// Keyboard-interactive answers the user asked to save (see
    /// `SaveSecret`).
    SaveAnswers(Vec<SecretString>),
    /// How to resolve a [`Prompt::TransferConflict`]. `Ask` counts as
    /// `Skip`.
    Conflict(ConflictPolicy),
}

/// Sends a prompt to the UI. Called with a fresh prompt id. An `Err` (the
/// prompt could not be delivered, so nobody can answer it) makes
/// [`PromptBridge::ask`] return [`PromptResponse::Reject`] at once instead of
/// waiting for the timeout; the message is logged.
pub type EmitPrompt = Arc<dyn Fn(String, Prompt) -> Result<(), String> + Send + Sync>;

/// Tells the UI that a delivered prompt no longer wants an answer (it timed
/// out, or whoever asked gave up), so a dialog still showing it can close.
/// Called with the prompt id; never for a prompt that was answered or that
/// could not be delivered.
pub type WithdrawPrompt = Arc<dyn Fn(String) + Send + Sync>;

/// Routes prompts to the UI and answers back to the waiting caller.
pub struct PromptBridge {
    emit: EmitPrompt,
    withdraw: Option<WithdrawPrompt>,
    timeout: Duration,
    pending: Mutex<HashMap<String, oneshot::Sender<PromptResponse>>>,
}

impl PromptBridge {
    /// A bridge whose prompts time out after [`PROMPT_TIMEOUT`].
    pub fn new(emit: EmitPrompt) -> Self {
        Self::with_timeout(emit, PROMPT_TIMEOUT)
    }

    /// Like [`new`](Self::new) with another timeout (tests, mostly).
    pub fn with_timeout(emit: EmitPrompt, timeout: Duration) -> Self {
        Self {
            emit,
            withdraw: None,
            timeout,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Also report withdrawn prompts (see [`WithdrawPrompt`]).
    pub fn on_withdrawn(mut self, withdraw: WithdrawPrompt) -> Self {
        self.withdraw = Some(withdraw);
        self
    }

    /// Emits `p` and waits for its answer. An unanswered prompt (timeout,
    /// failed emit, or the bridge dropping the question) is
    /// [`PromptResponse::Reject`]. A delivered prompt that ends unanswered
    /// (timeout, or this future dropped) is reported as withdrawn.
    pub async fn ask(&self, p: Prompt) -> PromptResponse {
        let id = uuid::Uuid::now_v7().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending_map().insert(id.clone(), tx);
        // Forget the question however this future ends (answer, timeout or
        // the caller giving up).
        let mut forget = Forget {
            bridge: self,
            id: &id,
            delivered: false,
        };
        // Registered first, so an answer arriving during `emit` is not lost;
        // no lock is held while emitting.
        if let Err(e) = (self.emit)(id.clone(), p) {
            tracing::warn!(prompt_id = %id, error = %e, "prompt could not be delivered; rejecting");
            return PromptResponse::Reject;
        }
        forget.delivered = true;
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) | Err(_) => PromptResponse::Reject,
        }
    }

    /// Answers prompt `prompt_id`. Unknown or expired ids are ignored.
    pub fn respond(&self, prompt_id: &str, r: PromptResponse) {
        let waiting = self.pending_map().remove(prompt_id);
        match waiting {
            Some(tx) => {
                let _ = tx.send(r);
            }
            None => tracing::debug!(prompt_id, "answer for an unknown or expired prompt"),
        }
    }

    fn pending_map(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<PromptResponse>>> {
        // The map stays consistent even if a holder panicked.
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }
}

struct Forget<'a> {
    bridge: &'a PromptBridge,
    id: &'a str,
    /// The UI has the prompt, so it must hear if it goes away unanswered.
    delivered: bool,
}

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        // Still pending means nobody answered it (an answer removes it).
        let unanswered = self.bridge.pending_map().remove(self.id).is_some();
        if unanswered
            && self.delivered
            && let Some(withdraw) = &self.bridge.withdraw
        {
            withdraw(self.id.to_owned());
        }
    }
}

/// The transport's questions, asked through the bridge. Anything but the
/// expected kind of answer (including a timeout) is "no answer".
#[async_trait]
impl AuthPrompter for PromptBridge {
    async fn password(&self, user: &str, host: &str) -> Option<SecretString> {
        let p = Prompt::Password {
            user: user.to_owned(),
            host: host.to_owned(),
            save_to: None,
        };
        match self.ask(p).await {
            PromptResponse::Secret(s) | PromptResponse::SaveSecret(s) => Some(s),
            _ => None,
        }
    }

    async fn passphrase(&self, key_label: &str) -> Option<SecretString> {
        let p = Prompt::Passphrase {
            key_label: key_label.to_owned(),
        };
        match self.ask(p).await {
            PromptResponse::Secret(s) => Some(s),
            _ => None,
        }
    }

    async fn keyboard_interactive(
        &self,
        name: &str,
        instructions: &str,
        prompts: &[(String, bool)],
    ) -> Option<Vec<SecretString>> {
        let p = Prompt::KeyboardInteractive {
            name: name.to_owned(),
            instructions: instructions.to_owned(),
            prompts: prompts.to_vec(),
            save_to: None,
        };
        match self.ask(p).await {
            PromptResponse::Answers(a) | PromptResponse::SaveAnswers(a)
                if a.len() == prompts.len() =>
            {
                Some(a)
            }
            _ => None,
        }
    }
}
