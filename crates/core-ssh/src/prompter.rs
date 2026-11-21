use async_trait::async_trait;
use secrecy::SecretString;

/// Asks a human for a secret the transport needs during authentication.
///
/// Every method returns `None` when the user cancels, does not answer, or
/// there is nobody to ask; the transport then fails with
/// [`SshError::AuthFailed`](crate::SshError::AuthFailed).
///
/// `ConnectParams::connect_timeout` does not count time spent inside these
/// calls (they wait on a human), so implementations must bound their own
/// wait, e.g. a prompt that gives up after a timeout.
#[async_trait]
pub trait AuthPrompter: Send + Sync {
    /// A login password for `user` on `host`.
    async fn password(&self, user: &str, host: &str) -> Option<SecretString>;

    /// The passphrase of an encrypted private key, described by `key_label`.
    async fn passphrase(&self, key_label: &str) -> Option<SecretString>;

    /// Answers to one keyboard-interactive challenge: one per prompt, in
    /// order. Each prompt is `(text, echo)`; `echo == false` means the
    /// answer is secret and must not be shown as it is typed.
    async fn keyboard_interactive(
        &self,
        name: &str,
        instructions: &str,
        prompts: &[(String, bool)],
    ) -> Option<Vec<SecretString>>;
}

/// An [`AuthPrompter`] with nobody behind it: every question goes
/// unanswered.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPrompter;

#[async_trait]
impl AuthPrompter for NoPrompter {
    async fn password(&self, _user: &str, _host: &str) -> Option<SecretString> {
        None
    }

    async fn passphrase(&self, _key_label: &str) -> Option<SecretString> {
        None
    }

    async fn keyboard_interactive(
        &self,
        _name: &str,
        _instructions: &str,
        _prompts: &[(String, bool)],
    ) -> Option<Vec<SecretString>> {
        None
    }
}
