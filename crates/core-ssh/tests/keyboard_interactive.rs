//! Against an in-process russh server (no fixture needed):
//! keyboard-interactive authentication with a real challenge/response
//! exchange (the docker fixture's sshd has no PAM, so it never sends
//! prompts), and host-key negotiation with a server holding several host
//! keys.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use core_ssh::{
    AcceptAll, AuthMethod, AuthPrompter, ConnectParams, HostKeyDecision, HostKeyInfo,
    HostKeyVerifier, RusshTransport, SshError, SshTransport,
};
use russh::keys::key::safe_rng;
use russh::keys::{Algorithm, EcdsaCurve, PrivateKey};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Response, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use secrecy::SecretString;
use tokio::net::TcpListener;

const NAME: &str = "tether-kbd";
const INSTRUCTIONS: &str = "Answer both questions.";
const EXPECTED: [&str; 2] = ["tether-test", "424242"];

/// What the server saw, for assertions.
#[derive(Default)]
struct Seen {
    kbd_responses: Vec<Vec<String>>,
    password_attempts: usize,
}

#[derive(Clone)]
struct KbdServer {
    seen: Arc<Mutex<Seen>>,
}

impl server::Handler for KbdServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        self.seen.lock().unwrap().password_attempts += 1;
        Ok(if password == EXPECTED[0] {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let Some(response) = response else {
            return Ok(Auth::Partial {
                name: Cow::Borrowed(NAME),
                instructions: Cow::Borrowed(INSTRUCTIONS),
                prompts: Cow::Owned(vec![
                    (Cow::Borrowed("Password: "), false),
                    (Cow::Borrowed("Code: "), true),
                ]),
            });
        };
        let answers: Vec<String> = response
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .collect();
        let ok = answers == EXPECTED;
        self.seen.lock().unwrap().kbd_responses.push(answers);
        Ok(if ok { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, &b"kbd-shell-ready\r\n"[..])
    }
}

fn host_key(algorithm: Algorithm) -> PrivateKey {
    PrivateKey::random(&mut safe_rng(), algorithm).unwrap()
}

/// Serves one SSH connection on an ephemeral port with these host keys.
async fn serve_once(seen: Arc<Mutex<Seen>>, keys: Vec<PrivateKey>) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Arc::new(server::Config {
        keys,
        methods: MethodSet::from(&[MethodKind::KeyboardInteractive, MethodKind::Password][..]),
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        if let Ok(running) = server::run_stream(config, stream, KbdServer { seen }).await {
            let _ = running.await;
        }
    });
    port
}

fn params(port: u16) -> ConnectParams {
    ConnectParams {
        host: "127.0.0.1".into(),
        port,
        username: "tether".into(),
        auth: AuthMethod::KeyboardInteractive,
        term: "xterm-256color".into(),
        cols: 80,
        rows: 24,
        connect_timeout: Duration::from_secs(5),
        keepalive: None,
        env: Vec::new(),
    }
}

/// `(name, instructions, prompts)` of one keyboard-interactive round.
type Challenge = (String, String, Vec<(String, bool)>);

/// Answers keyboard-interactive prompts with fixed answers and records the
/// challenge it was shown. Password and passphrase prompts are refused.
struct FakePrompter {
    answers: Vec<&'static str>,
    challenges: Mutex<Vec<Challenge>>,
    other_prompts: Mutex<usize>,
}

impl FakePrompter {
    fn new(answers: &[&'static str]) -> Self {
        Self {
            answers: answers.to_vec(),
            challenges: Mutex::new(Vec::new()),
            other_prompts: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AuthPrompter for FakePrompter {
    async fn password(&self, _user: &str, _host: &str) -> Option<SecretString> {
        *self.other_prompts.lock().unwrap() += 1;
        None
    }

    async fn passphrase(&self, _key_label: &str) -> Option<SecretString> {
        *self.other_prompts.lock().unwrap() += 1;
        None
    }

    async fn keyboard_interactive(
        &self,
        name: &str,
        instructions: &str,
        prompts: &[(String, bool)],
    ) -> Option<Vec<SecretString>> {
        self.challenges
            .lock()
            .unwrap()
            .push((name.into(), instructions.into(), prompts.to_vec()));
        Some(
            self.answers
                .iter()
                .map(|a| SecretString::from(*a))
                .collect(),
        )
    }
}

#[tokio::test]
async fn keyboard_interactive_answers_flow() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let port = serve_once(seen.clone(), vec![host_key(Algorithm::Ed25519)]).await;
    let prompter = Arc::new(FakePrompter::new(&EXPECTED));

    let mut shell = RusshTransport
        .open_shell_with_prompter(params(port), Arc::new(AcceptAll), prompter.clone())
        .await
        .expect("keyboard-interactive login");

    let challenges = prompter.challenges.lock().unwrap().clone();
    assert_eq!(
        challenges,
        vec![(
            NAME.to_string(),
            INSTRUCTIONS.to_string(),
            vec![
                ("Password: ".to_string(), false),
                ("Code: ".to_string(), true)
            ],
        )]
    );
    assert_eq!(*prompter.other_prompts.lock().unwrap(), 0);
    {
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.kbd_responses,
            vec![EXPECTED.map(String::from).to_vec()]
        );
        assert_eq!(seen.password_attempts, 0);
    }

    let out = tokio::time::timeout(Duration::from_secs(5), shell.output.recv())
        .await
        .expect("shell output")
        .expect("shell open");
    assert_eq!(&out[..], b"kbd-shell-ready\r\n");
    shell.control.close().await;
}

/// Wrong answers fail the login. The user was already asked, so the
/// transport does not fall back to a second (password) prompt.
#[tokio::test]
async fn keyboard_interactive_wrong_answers_fail_without_password_fallback() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let port = serve_once(seen.clone(), vec![host_key(Algorithm::Ed25519)]).await;
    let prompter = Arc::new(FakePrompter::new(&["nope", "000000"]));

    let err = RusshTransport
        .open_shell_with_prompter(params(port), Arc::new(AcceptAll), prompter.clone())
        .await
        .err()
        .expect("should fail");
    assert!(matches!(err, SshError::AuthFailed), "got {err:?}");
    assert_eq!(prompter.challenges.lock().unwrap().len(), 1);
    assert_eq!(*prompter.other_prompts.lock().unwrap(), 0);
    assert_eq!(seen.lock().unwrap().password_attempts, 0);
}

/// A prompter that gives up (`None`) is `AuthFailed`, with nothing sent.
#[tokio::test]
async fn keyboard_interactive_unanswered_is_auth_failed() {
    struct Silent;
    #[async_trait]
    impl AuthPrompter for Silent {
        async fn password(&self, _: &str, _: &str) -> Option<SecretString> {
            None
        }
        async fn passphrase(&self, _: &str) -> Option<SecretString> {
            None
        }
        async fn keyboard_interactive(
            &self,
            _: &str,
            _: &str,
            _: &[(String, bool)],
        ) -> Option<Vec<SecretString>> {
            None
        }
    }

    let seen = Arc::new(Mutex::new(Seen::default()));
    let port = serve_once(seen.clone(), vec![host_key(Algorithm::Ed25519)]).await;
    let err = RusshTransport
        .open_shell_with_prompter(params(port), Arc::new(AcceptAll), Arc::new(Silent))
        .await
        .err()
        .expect("should fail");
    assert!(matches!(err, SshError::AuthFailed), "got {err:?}");
    assert!(seen.lock().unwrap().kbd_responses.is_empty());
}

/// Claims to know `known` for every host, records the key types it was
/// shown, and answers `decision`.
struct KnownTypes {
    known: Vec<String>,
    decision: HostKeyDecision,
    shown: Mutex<Vec<String>>,
}

impl KnownTypes {
    fn new(known: &[&str], decision: HostKeyDecision) -> Arc<Self> {
        Arc::new(Self {
            known: known.iter().map(|s| s.to_string()).collect(),
            decision,
            shown: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl HostKeyVerifier for KnownTypes {
    async fn verify(&self, _host: &str, _port: u16, key: &HostKeyInfo) -> HostKeyDecision {
        self.shown.lock().unwrap().push(key.algorithm.clone());
        self.decision
    }

    async fn known_algorithms(&self, _host: &str, _port: u16) -> Vec<String> {
        self.known.clone()
    }
}

fn password_params(port: u16) -> ConnectParams {
    ConnectParams {
        auth: AuthMethod::Password(SecretString::from(EXPECTED[0])),
        ..params(port)
    }
}

/// A server with an ed25519 and an ECDSA host key presents the one the
/// verifier already trusts, not just the transport's default favourite.
#[tokio::test]
async fn known_host_key_types_are_requested_first() {
    let keys = || {
        vec![
            host_key(Algorithm::Ed25519),
            host_key(Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            }),
        ]
    };
    for (known, expected) in [
        (&[][..], "ssh-ed25519"),
        (&["ecdsa-sha2-nistp256"][..], "ecdsa-sha2-nistp256"),
        (&["ssh-ed25519"][..], "ssh-ed25519"),
    ] {
        let port = serve_once(Arc::default(), keys()).await;
        let verifier = KnownTypes::new(known, HostKeyDecision::Accept);
        let shell = RusshTransport
            .open_shell(password_params(port), verifier.clone())
            .await
            .expect("open_shell");
        assert_eq!(
            *verifier.shown.lock().unwrap(),
            vec![expected],
            "known: {known:?}"
        );
        shell.control.close().await;
    }
}

/// A verifier that cannot check keys (vault locked) refuses the connection
/// with its own error, distinct from a user's rejection.
#[tokio::test]
async fn known_host_store_unavailable_is_its_own_error() {
    for (decision, unverifiable) in [
        (HostKeyDecision::Unverifiable, true),
        (HostKeyDecision::Reject, false),
    ] {
        let port = serve_once(Arc::default(), vec![host_key(Algorithm::Ed25519)]).await;
        let err = RusshTransport
            .open_shell(password_params(port), KnownTypes::new(&[], decision))
            .await
            .err()
            .expect("must not connect");
        if unverifiable {
            assert!(matches!(err, SshError::HostKeyUnverifiable), "got {err:?}");
            assert_eq!(
                err.to_string(),
                "vault locked: host keys cannot be verified"
            );
        } else {
            assert!(matches!(err, SshError::HostKeyRejected), "got {err:?}");
        }
    }
}
