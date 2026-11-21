//! A saved (stored) password against an in-process russh server: used as
//! the answer to a keyboard-interactive-only server's password prompt, and
//! when it is refused the human is asked instead of failing outright.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use core_ssh::{
    AcceptAll, AuthMethod, AuthPrompter, ConnectParams, RusshTransport, SshError, SshTransport,
};
use russh::keys::key::safe_rng;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Response, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use secrecy::SecretString;
use tokio::net::TcpListener;

const RIGHT: &str = "right-password";
const CODE: &str = "123456";

#[derive(Default)]
struct Seen {
    passwords: Vec<String>,
    kbd_responses: Vec<Vec<String>>,
}

#[derive(Clone)]
struct Server {
    seen: Arc<Mutex<Seen>>,
    /// Keyboard-interactive asks the password, then (whatever it was) a
    /// code in a second round, like PAM with an OTP module.
    otp: bool,
    /// The password answered in the first round (OTP mode).
    pending: Option<String>,
}

impl server::Handler for Server {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        self.seen.lock().unwrap().passwords.push(password.into());
        Ok(if password == RIGHT {
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
                name: Cow::Borrowed(""),
                instructions: Cow::Borrowed(""),
                prompts: Cow::Owned(vec![(Cow::Borrowed("Password: "), false)]),
            });
        };
        let answers: Vec<String> = response
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .collect();
        self.seen
            .lock()
            .unwrap()
            .kbd_responses
            .push(answers.clone());
        if self.otp {
            return Ok(match self.pending.take() {
                None => {
                    self.pending = answers.first().cloned();
                    Auth::Partial {
                        name: Cow::Borrowed(""),
                        instructions: Cow::Borrowed(""),
                        prompts: Cow::Owned(vec![(Cow::Borrowed("Code: "), true)]),
                    }
                }
                Some(pw) if pw == RIGHT && answers == [CODE] => Auth::Accept,
                Some(_) => Auth::reject(),
            });
        }
        let ok = answers == [RIGHT];
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
        session.channel_success(channel)
    }
}

/// Serves one connection offering `methods`.
async fn serve_once(seen: Arc<Mutex<Seen>>, methods: &[MethodKind]) -> u16 {
    serve(seen, methods, false).await
}

async fn serve(seen: Arc<Mutex<Seen>>, methods: &[MethodKind], otp: bool) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Arc::new(server::Config {
        keys: vec![PrivateKey::random(&mut safe_rng(), Algorithm::Ed25519).unwrap()],
        methods: MethodSet::from(methods),
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        if let Ok(running) = server::run_stream(
            config,
            stream,
            Server {
                seen,
                otp,
                pending: None,
            },
        )
        .await
        {
            let _ = running.await;
        }
    });
    port
}

fn saved(port: u16, password: &str) -> ConnectParams {
    ConnectParams {
        host: "127.0.0.1".into(),
        port,
        username: "tether".into(),
        auth: AuthMethod::Password(SecretString::from(password.to_owned())),
        term: "xterm-256color".into(),
        cols: 80,
        rows: 24,
        connect_timeout: Duration::from_secs(5),
        keepalive: None,
        env: Vec::new(),
    }
}

/// Answers every question with `answer` (or nothing), counting them.
struct Human {
    answer: Option<&'static str>,
    asked: Mutex<Vec<&'static str>>,
}

impl Human {
    fn new(answer: Option<&'static str>) -> Arc<Self> {
        Arc::new(Self {
            answer,
            asked: Mutex::new(Vec::new()),
        })
    }
    fn asked(&self) -> Vec<&'static str> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl AuthPrompter for Human {
    async fn password(&self, _: &str, _: &str) -> Option<SecretString> {
        self.asked.lock().unwrap().push("password");
        self.answer.map(SecretString::from)
    }
    async fn passphrase(&self, _: &str) -> Option<SecretString> {
        self.asked.lock().unwrap().push("passphrase");
        None
    }
    async fn keyboard_interactive(
        &self,
        _: &str,
        _: &str,
        prompts: &[(String, bool)],
    ) -> Option<Vec<SecretString>> {
        self.asked.lock().unwrap().push("kbd");
        // A code prompt gets the code; anything else the password.
        self.answer.map(|a| {
            prompts
                .iter()
                .map(|(text, _)| {
                    SecretString::from(if text.starts_with("Code") { CODE } else { a })
                })
                .collect()
        })
    }
}

async fn connect(params: ConnectParams, human: Arc<Human>) -> Result<(), SshError> {
    let shell = RusshTransport
        .open_shell_with_prompter(params, Arc::new(AcceptAll), human)
        .await?;
    shell.control.close().await;
    Ok(())
}

#[tokio::test]
async fn saved_password_answers_kbd_only_server() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let port = serve_once(seen.clone(), &[MethodKind::KeyboardInteractive]).await;
    let human = Human::new(None);
    connect(saved(port, RIGHT), human.clone())
        .await
        .expect("login with the saved password");
    assert!(human.asked().is_empty());
    assert_eq!(
        seen.lock().unwrap().kbd_responses,
        vec![vec![RIGHT.to_string()]]
    );
}

#[tokio::test]
async fn stale_saved_password_falls_back_to_prompt() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let port = serve_once(
        seen.clone(),
        &[MethodKind::Password, MethodKind::KeyboardInteractive],
    )
    .await;
    let human = Human::new(Some(RIGHT));
    connect(saved(port, "stale"), human.clone())
        .await
        .expect("human answer logs in");
    assert_eq!(human.asked(), vec!["kbd"]);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.passwords, vec!["stale".to_string()]);
    assert_eq!(seen.kbd_responses, vec![vec![RIGHT.to_string()]]);
}

#[tokio::test]
async fn stale_saved_password_on_kbd_only_server_falls_back_to_prompt() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let port = serve_once(seen.clone(), &[MethodKind::KeyboardInteractive]).await;
    let human = Human::new(Some(RIGHT));
    connect(saved(port, "stale"), human.clone())
        .await
        .expect("human answer logs in");
    assert_eq!(human.asked(), vec!["kbd"]);
    assert_eq!(
        seen.lock().unwrap().kbd_responses,
        vec![vec!["stale".to_string()], vec![RIGHT.to_string()]]
    );
}

#[tokio::test]
async fn stale_saved_password_without_answer_fails() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let port = serve_once(
        seen.clone(),
        &[MethodKind::Password, MethodKind::KeyboardInteractive],
    )
    .await;
    let human = Human::new(None);
    let err = connect(saved(port, "stale"), human.clone())
        .await
        .unwrap_err();
    assert!(matches!(err, SshError::AuthFailed), "got {err:?}");
    assert_eq!(human.asked(), vec!["kbd"]);
}

/// Password then OTP in two rounds: the stale saved password answers the
/// first, the human the code; refused, the human is asked for both, so a
/// changed password can be typed (and saved) again.
#[tokio::test]
async fn stale_saved_password_with_otp_round_asks_again() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let port = serve(seen.clone(), &[MethodKind::KeyboardInteractive], true).await;
    let human = Human::new(Some(RIGHT));
    connect(saved(port, "stale"), human.clone())
        .await
        .expect("human answers log in");
    assert_eq!(human.asked(), vec!["kbd", "kbd", "kbd"]);
    assert_eq!(
        seen.lock().unwrap().kbd_responses,
        vec![
            vec!["stale".to_string()],
            vec![CODE.to_string()],
            vec![RIGHT.to_string()],
            vec![CODE.to_string()],
        ]
    );
}
