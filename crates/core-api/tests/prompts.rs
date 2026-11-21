//! `PromptBridge`: routing answers to the right question, the 90 s timeout,
//! the `AuthPrompter` mapping and the wire format the UI sees.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use core_api::{PROMPT_TIMEOUT, Prompt, PromptBridge, PromptResponse};
use core_ssh::{AuthPrompter, HostKeyInfo};
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::mpsc;

/// A bridge whose emitted prompts arrive on a channel, like the UI would see
/// them.
fn channel_bridge() -> (Arc<PromptBridge>, mpsc::UnboundedReceiver<(String, Prompt)>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let bridge = PromptBridge::new(Arc::new(move |id, prompt| {
        let _ = tx.send((id, prompt));
        Ok(())
    }));
    (Arc::new(bridge), rx)
}

fn password_prompt() -> Prompt {
    Prompt::Password {
        user: "tether".into(),
        host: "example.test".into(),
        save_to: None,
    }
}

/// Lets every other task on the (current-thread, paused-clock) runtime run.
async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn prompt_timeout_rejects() {
    assert_eq!(PROMPT_TIMEOUT, Duration::from_secs(90));
    let (bridge, mut rx) = channel_bridge();
    let asking = {
        let bridge = bridge.clone();
        tokio::spawn(async move { bridge.ask(password_prompt()).await })
    };
    let (id, _) = rx.recv().await.expect("prompt emitted");

    tokio::time::advance(Duration::from_secs(89)).await;
    settle().await;
    assert!(!asking.is_finished(), "rejected before 90 s");

    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert!(asking.is_finished(), "still waiting at 90 s");
    assert!(matches!(asking.await.unwrap(), PromptResponse::Reject));

    // A late answer is ignored (the question is gone) and does not panic.
    bridge.respond(&id, PromptResponse::Accept { save: true });
}

#[tokio::test(start_paused = true)]
async fn prompt_timeout_maps_to_none_for_auth_prompter() {
    let (bridge, mut rx) = channel_bridge();
    let asking = {
        let bridge = bridge.clone();
        tokio::spawn(async move { bridge.password("tether", "example.test").await })
    };
    rx.recv().await.expect("prompt emitted");
    tokio::time::advance(PROMPT_TIMEOUT).await;
    assert!(asking.await.unwrap().is_none());
}

#[tokio::test]
async fn prompt_answers_route_by_id() {
    let (bridge, mut rx) = channel_bridge();
    let first = {
        let bridge = bridge.clone();
        tokio::spawn(async move { bridge.ask(password_prompt()).await })
    };
    let (id1, _) = rx.recv().await.unwrap();
    let second = {
        let bridge = bridge.clone();
        tokio::spawn(async move {
            bridge
                .ask(Prompt::Passphrase {
                    key_label: "work key".into(),
                })
                .await
        })
    };
    let (id2, p2) = rx.recv().await.unwrap();
    assert_ne!(id1, id2);
    assert_eq!(
        p2,
        Prompt::Passphrase {
            key_label: "work key".into()
        }
    );

    bridge.respond("no-such-id", PromptResponse::Reject);
    bridge.respond(&id2, PromptResponse::Secret(SecretString::from("two")));
    bridge.respond(&id1, PromptResponse::Secret(SecretString::from("one")));

    let PromptResponse::Secret(s1) = first.await.unwrap() else {
        panic!("first: wrong response");
    };
    let PromptResponse::Secret(s2) = second.await.unwrap() else {
        panic!("second: wrong response");
    };
    assert_eq!((s1.expose_secret(), s2.expose_secret()), ("one", "two"));
}

/// The emit callback may answer synchronously (the question is registered
/// before it is emitted, and no lock is held while emitting).
#[tokio::test]
async fn prompt_answered_inside_emit() {
    let cell: Arc<Mutex<Option<std::sync::Weak<PromptBridge>>>> = Arc::default();
    let bridge = Arc::new(PromptBridge::new({
        let cell = cell.clone();
        Arc::new(move |id, _prompt| {
            let bridge = cell.lock().unwrap().as_ref().and_then(|w| w.upgrade());
            bridge
                .expect("bridge")
                .respond(&id, PromptResponse::Accept { save: false });
            Ok(())
        })
    }));
    *cell.lock().unwrap() = Some(Arc::downgrade(&bridge));
    let r = tokio::time::timeout(Duration::from_secs(5), bridge.ask(password_prompt()))
        .await
        .expect("answered");
    assert!(matches!(r, PromptResponse::Accept { save: false }));
}

/// A prompt the UI never received cannot be answered: reject at once rather
/// than after the 90 s timeout.
#[tokio::test(start_paused = true)]
async fn prompt_emit_failure_rejects_at_once() {
    let emitted: Arc<Mutex<Vec<String>>> = Arc::default();
    let bridge = PromptBridge::new({
        let emitted = emitted.clone();
        Arc::new(move |id, _prompt| {
            emitted.lock().unwrap().push(id);
            Err("window closed".to_string())
        })
    });
    let start = tokio::time::Instant::now();
    assert!(matches!(
        bridge.ask(password_prompt()).await,
        PromptResponse::Reject
    ));
    assert!(bridge.password("tether", "example.test").await.is_none());
    // With the clock paused, any wait would have auto-advanced it.
    assert_eq!(start.elapsed(), Duration::ZERO);
    assert_eq!(emitted.lock().unwrap().len(), 2);
    // The questions are gone: a late answer is ignored.
    let first = emitted.lock().unwrap()[0].clone();
    bridge.respond(&first, PromptResponse::Accept { save: true });
}

type Emitted = mpsc::UnboundedReceiver<(String, Prompt)>;
type Withdrawn = Arc<Mutex<Vec<String>>>;

/// A bridge that records emitted prompts and withdrawn prompt ids.
fn withdraw_bridge() -> (Arc<PromptBridge>, Emitted, Withdrawn) {
    let (tx, rx) = mpsc::unbounded_channel();
    let withdrawn: Arc<Mutex<Vec<String>>> = Arc::default();
    let bridge = PromptBridge::new(Arc::new(move |id, prompt| {
        let _ = tx.send((id, prompt));
        Ok(())
    }))
    .on_withdrawn({
        let withdrawn = withdrawn.clone();
        Arc::new(move |id| withdrawn.lock().unwrap().push(id))
    });
    (Arc::new(bridge), rx, withdrawn)
}

/// A prompt that times out is withdrawn, so the UI can close its dialog.
#[tokio::test(start_paused = true)]
async fn prompt_withdrawn_on_timeout() {
    let (bridge, mut rx, withdrawn) = withdraw_bridge();
    let asking = {
        let bridge = bridge.clone();
        tokio::spawn(async move { bridge.ask(password_prompt()).await })
    };
    let (id, _) = rx.recv().await.unwrap();
    assert!(withdrawn.lock().unwrap().is_empty());
    tokio::time::advance(PROMPT_TIMEOUT).await;
    assert!(matches!(asking.await.unwrap(), PromptResponse::Reject));
    assert_eq!(*withdrawn.lock().unwrap(), vec![id]);
}

/// A prompt whose asker goes away (cancelled transfer, aborted connect) is
/// withdrawn too.
#[tokio::test]
async fn prompt_withdrawn_when_asker_dropped() {
    let (bridge, mut rx, withdrawn) = withdraw_bridge();
    let asking = {
        let bridge = bridge.clone();
        tokio::spawn(async move { bridge.ask(password_prompt()).await })
    };
    let (id, _) = rx.recv().await.unwrap();
    asking.abort();
    let _ = asking.await;
    assert_eq!(*withdrawn.lock().unwrap(), vec![id.clone()]);
    // A late answer is ignored and withdraws nothing more.
    bridge.respond(&id, PromptResponse::Reject);
    assert_eq!(withdrawn.lock().unwrap().len(), 1);
}

/// Answered prompts, and prompts that were never delivered, are not
/// withdrawn.
#[tokio::test]
async fn prompt_not_withdrawn_when_answered_or_undelivered() {
    let (bridge, mut rx, withdrawn) = withdraw_bridge();
    let asking = {
        let bridge = bridge.clone();
        tokio::spawn(async move { bridge.ask(password_prompt()).await })
    };
    let (id, _) = rx.recv().await.unwrap();
    bridge.respond(&id, PromptResponse::Reject);
    asking.await.unwrap();
    assert!(withdrawn.lock().unwrap().is_empty());

    let failed: Arc<Mutex<Vec<String>>> = Arc::default();
    let bridge = PromptBridge::new(Arc::new(|_, _| Err("no window".to_string()))).on_withdrawn({
        let failed = failed.clone();
        Arc::new(move |id| failed.lock().unwrap().push(id))
    });
    assert!(matches!(
        bridge.ask(password_prompt()).await,
        PromptResponse::Reject
    ));
    assert!(failed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn prompt_responses_map_onto_auth_prompter() {
    let (bridge, mut rx) = channel_bridge();
    let b = bridge.clone();
    let answer = tokio::spawn(async move {
        // password: a secret is the answer.
        let (id, p) = rx.recv().await.unwrap();
        assert_eq!(
            p,
            Prompt::Password {
                user: "tether".into(),
                host: "example.test".into(),
                save_to: None,
            }
        );
        b.respond(&id, PromptResponse::Secret(SecretString::from("pw")));
        // passphrase: anything but a secret is a refusal.
        let (id, _) = rx.recv().await.unwrap();
        b.respond(&id, PromptResponse::Accept { save: true });
        // keyboard-interactive: one answer per prompt.
        let (id, p) = rx.recv().await.unwrap();
        assert_eq!(
            p,
            Prompt::KeyboardInteractive {
                name: "n".into(),
                instructions: "i".into(),
                prompts: vec![("Password: ".into(), false), ("Code: ".into(), true)],
                save_to: None,
            }
        );
        b.respond(
            &id,
            PromptResponse::Answers(vec![SecretString::from("a"), SecretString::from("b")]),
        );
        // ... and the wrong number of answers is a refusal.
        let (id, _) = rx.recv().await.unwrap();
        b.respond(&id, PromptResponse::Answers(vec![SecretString::from("a")]));
        // Reject is a refusal.
        let (id, _) = rx.recv().await.unwrap();
        b.respond(&id, PromptResponse::Reject);
    });

    let pw = bridge.password("tether", "example.test").await.unwrap();
    assert_eq!(pw.expose_secret(), "pw");
    assert!(bridge.passphrase("k").await.is_none());
    let prompts = vec![
        ("Password: ".to_string(), false),
        ("Code: ".to_string(), true),
    ];
    let answers = bridge
        .keyboard_interactive("n", "i", &prompts)
        .await
        .unwrap();
    let answers: Vec<&str> = answers.iter().map(|a| a.expose_secret()).collect();
    assert_eq!(answers, ["a", "b"]);
    assert!(
        bridge
            .keyboard_interactive("n", "i", &prompts)
            .await
            .is_none()
    );
    assert!(bridge.password("tether", "example.test").await.is_none());
    answer.await.unwrap();
}

#[test]
fn prompt_serializes_camel_case() {
    let key = HostKeyInfo {
        algorithm: "ssh-ed25519".into(),
        sha256_fingerprint: "SHA256:abc".into(),
        openssh: "ssh-ed25519 AAAA".into(),
    };
    let json = serde_json::to_value(Prompt::HostKeyChanged {
        host: "h".into(),
        port: 22,
        old: key.clone(),
        new: key,
    })
    .unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "kind": "hostKeyChanged",
            "host": "h",
            "port": 22,
            "old": {"algorithm": "ssh-ed25519", "sha256Fingerprint": "SHA256:abc", "openssh": "ssh-ed25519 AAAA"},
            "new": {"algorithm": "ssh-ed25519", "sha256Fingerprint": "SHA256:abc", "openssh": "ssh-ed25519 AAAA"},
        })
    );
    let json = serde_json::to_value(Prompt::Passphrase {
        key_label: "k".into(),
    })
    .unwrap();
    assert_eq!(
        json,
        serde_json::json!({"kind": "passphrase", "keyLabel": "k"})
    );
}

#[test]
fn prompt_response_deserializes_secrets_without_leaking_them() {
    let r: PromptResponse = serde_json::from_str(r#"{"accept":{"save":true}}"#).unwrap();
    assert!(matches!(r, PromptResponse::Accept { save: true }));
    let r: PromptResponse = serde_json::from_str(r#""reject""#).unwrap();
    assert!(matches!(r, PromptResponse::Reject));
    let r: PromptResponse = serde_json::from_str(r#"{"secret":"hunter2"}"#).unwrap();
    let shown = format!("{r:?}");
    let PromptResponse::Secret(s) = r else {
        panic!("not a secret");
    };
    assert_eq!(s.expose_secret(), "hunter2");
    assert!(!shown.contains("hunter2"), "Debug leaked: {shown}");
    let r: PromptResponse = serde_json::from_str(r#"{"answers":["a1","b2"]}"#).unwrap();
    assert!(!format!("{r:?}").contains("a1"));
    assert!(matches!(r, PromptResponse::Answers(ref v) if v.len() == 2));
}
