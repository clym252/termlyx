//! Saving a password typed at a connect prompt: [`SavingPrompter`] asks
//! through the [`PromptBridge`] with a save offer and keeps the answer the
//! user asked to save, so the caller can store it once authentication has
//! succeeded (and only then).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use core_ssh::AuthPrompter;
use secrecy::SecretString;

use crate::prompts::{Prompt, PromptBridge, PromptResponse, SaveTarget};

/// An [`AuthPrompter`] for one connection to a vault host. Password prompts,
/// and keyboard-interactive rounds with a single hidden prompt, carry the
/// save offer (when there is one); an answer the user asked to save is kept
/// for [`take_saved`](Self::take_saved). Passphrases are never offered.
pub struct SavingPrompter {
    bridge: Arc<PromptBridge>,
    offer: Option<SaveTarget>,
    saved: Mutex<Option<SecretString>>,
}

impl SavingPrompter {
    /// `offer`: where a saved password would go, or `None` to offer nothing.
    pub fn new(bridge: Arc<PromptBridge>, offer: Option<SaveTarget>) -> Arc<Self> {
        Arc::new(Self {
            bridge,
            offer,
            saved: Mutex::new(None),
        })
    }

    /// The last answer the user asked to save, once. Call it only after
    /// authentication succeeded.
    pub fn take_saved(&self) -> Option<SecretString> {
        self.slot().take()
    }

    /// Records the answer to a prompt that offered saving: kept if the user
    /// asked to save it, else an earlier kept one is forgotten. Prompts
    /// without the offer (an OTP round after the password) leave it alone.
    fn keep(&self, offered: bool, answer: Option<&SecretString>) {
        if offered {
            *self.slot() = answer.cloned();
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<SecretString>> {
        // Holds no invariant a panic could break.
        self.saved.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[async_trait]
impl AuthPrompter for SavingPrompter {
    async fn password(&self, user: &str, host: &str) -> Option<SecretString> {
        let offered = self.offer.is_some();
        let p = Prompt::Password {
            user: user.to_owned(),
            host: host.to_owned(),
            save_to: self.offer.clone(),
        };
        match self.bridge.ask(p).await {
            PromptResponse::Secret(s) => {
                self.keep(offered, None);
                Some(s)
            }
            PromptResponse::SaveSecret(s) => {
                self.keep(offered, Some(&s));
                Some(s)
            }
            _ => None,
        }
    }

    async fn passphrase(&self, key_label: &str) -> Option<SecretString> {
        self.bridge.passphrase(key_label).await
    }

    async fn keyboard_interactive(
        &self,
        name: &str,
        instructions: &str,
        prompts: &[(String, bool)],
    ) -> Option<Vec<SecretString>> {
        let single_hidden = matches!(prompts, [(_, false)]);
        let offer = self.offer.clone().filter(|_| single_hidden);
        let offered = offer.is_some();
        let p = Prompt::KeyboardInteractive {
            name: name.to_owned(),
            instructions: instructions.to_owned(),
            prompts: prompts.to_vec(),
            save_to: offer,
        };
        let (answers, save) = match self.bridge.ask(p).await {
            PromptResponse::Answers(a) => (a, false),
            PromptResponse::SaveAnswers(a) => (a, true),
            _ => return None,
        };
        if answers.len() != prompts.len() {
            return None;
        }
        self.keep(offered, answers.first().filter(|_| save));
        Some(answers)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, Weak};

    use core_ssh::AuthPrompter;
    use secrecy::{ExposeSecret, SecretString};

    use super::*;
    use crate::prompts::{Prompt, PromptBridge, PromptResponse, SaveTarget};

    /// A bridge whose stand-in user answers each prompt with `answer`,
    /// recording the prompts.
    fn bridge(
        answer: fn(&Prompt) -> PromptResponse,
    ) -> (Arc<PromptBridge>, Arc<Mutex<Vec<Prompt>>>) {
        let seen: Arc<Mutex<Vec<Prompt>>> = Arc::default();
        let me: Arc<Mutex<Weak<PromptBridge>>> = Arc::new(Mutex::new(Weak::new()));
        let b = Arc::new(PromptBridge::new({
            let seen = seen.clone();
            let me = me.clone();
            Arc::new(move |id: String, p: Prompt| {
                let r = answer(&p);
                seen.lock().unwrap().push(p);
                if let Some(b) = me.lock().unwrap().upgrade() {
                    b.respond(&id, r);
                }
                Ok(())
            })
        }));
        *me.lock().unwrap() = Arc::downgrade(&b);
        (b, seen)
    }

    fn target() -> SaveTarget {
        SaveTarget {
            identity_label: "alice".into(),
            is_new: false,
        }
    }

    #[tokio::test]
    async fn saving_prompter_stashes_only_when_asked() {
        let (b, seen) = bridge(|_| PromptResponse::Secret(SecretString::from("typed")));
        let p = SavingPrompter::new(b, Some(target()));
        assert_eq!(
            p.password("alice", "web").await.unwrap().expose_secret(),
            "typed"
        );
        assert!(p.take_saved().is_none());
        assert!(matches!(
            &seen.lock().unwrap()[0],
            Prompt::Password { save_to: Some(t), .. } if *t == target()
        ));

        let (b, _) = bridge(|_| PromptResponse::SaveSecret(SecretString::from("keep")));
        let p = SavingPrompter::new(b, Some(target()));
        assert_eq!(
            p.password("alice", "web").await.unwrap().expose_secret(),
            "keep"
        );
        assert_eq!(p.take_saved().unwrap().expose_secret(), "keep");
        assert!(p.take_saved().is_none(), "taken once");

        // Without an offer, nothing is offered and a save answer is not kept.
        let (b, seen) = bridge(|_| PromptResponse::SaveSecret(SecretString::from("keep")));
        let p = SavingPrompter::new(b, None);
        assert!(p.password("alice", "web").await.is_some());
        assert!(p.take_saved().is_none());
        assert!(matches!(
            &seen.lock().unwrap()[0],
            Prompt::Password { save_to: None, .. }
        ));
    }

    #[tokio::test]
    async fn single_hidden_kbd_prompt_is_offered_and_saved() {
        let (b, seen) = bridge(|_| PromptResponse::SaveAnswers(vec![SecretString::from("pw")]));
        let p = SavingPrompter::new(b, Some(target()));
        let a = p
            .keyboard_interactive("", "", &[("Password: ".into(), false)])
            .await
            .unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(p.take_saved().unwrap().expose_secret(), "pw");
        assert!(matches!(
            &seen.lock().unwrap()[0],
            Prompt::KeyboardInteractive {
                save_to: Some(_),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn multi_prompt_kbd_not_offered() {
        let (b, seen) = bridge(|_| {
            PromptResponse::SaveAnswers(vec![SecretString::from("pw"), SecretString::from("123")])
        });
        let p = SavingPrompter::new(b, Some(target()));
        let prompts = [
            ("Password: ".to_string(), false),
            ("Code: ".to_string(), true),
        ];
        assert!(p.keyboard_interactive("", "", &prompts).await.is_some());
        assert!(p.take_saved().is_none());
        assert!(matches!(
            &seen.lock().unwrap()[0],
            Prompt::KeyboardInteractive { save_to: None, .. }
        ));

        // A single prompt that echoes (a username, an OTP) is not a password.
        let (b, seen) = bridge(|_| PromptResponse::SaveAnswers(vec![SecretString::from("x")]));
        let p = SavingPrompter::new(b, Some(target()));
        assert!(
            p.keyboard_interactive("", "", &[("Code: ".into(), true)])
                .await
                .is_some()
        );
        assert!(p.take_saved().is_none());
        assert!(matches!(
            &seen.lock().unwrap()[0],
            Prompt::KeyboardInteractive { save_to: None, .. }
        ));
    }
}
