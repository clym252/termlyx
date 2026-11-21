//! `VaultHostKeyVerifier`: unknown keys prompt (and are saved on request),
//! changed keys prompt a blocking warning that defaults to Reject, and a
//! locked vault never auto-accepts.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use core_api::vault::{MemoryKeyStore, SharedVault, VaultService};
use core_api::{Prompt, PromptBridge, PromptResponse, VaultHostKeyVerifier};
use core_model::{Id, KnownHost, KnownHostSource};
use core_ssh::keys::{KeyAlgo, generate};
use core_ssh::{
    AuthMethod, ConnectParams, HostKeyDecision, HostKeyInfo, HostKeyVerifier, RusshTransport,
    SshError, SshTransport, parse_known_hosts,
};
use secrecy::SecretString;
use tempfile::TempDir;

const FIXTURE_PUB: &str = include_str!("../../../tests/fixtures/keys/id_ed25519_test.pub");

fn fixture_enabled() -> bool {
    let on = std::env::var("TETHER_SSH_FIXTURE").as_deref() == Ok("1");
    if !on {
        eprintln!("skipping: set TETHER_SSH_FIXTURE=1 (and run tests/fixtures/up.sh)");
    }
    on
}

/// A created (so unlocked) vault with cheap KDF parameters.
fn vault() -> (TempDir, SharedVault) {
    let dir = tempfile::tempdir().unwrap();
    let mut v = VaultService::open_for_tests(
        &dir.path().join("vault.sqlite"),
        Box::new(MemoryKeyStore::default()),
    )
    .unwrap();
    v.create(SecretString::from("master")).unwrap();
    (dir, Arc::new(Mutex::new(v)))
}

/// A bridge that records every prompt and answers it with `answer` (or
/// leaves it unanswered when `answer` returns `None`).
struct Scripted {
    bridge: Arc<PromptBridge>,
    seen: Arc<Mutex<Vec<Prompt>>>,
}

fn scripted(
    timeout: Duration,
    answer: impl Fn(&Prompt) -> Option<PromptResponse> + Send + Sync + 'static,
) -> Scripted {
    let seen: Arc<Mutex<Vec<Prompt>>> = Arc::default();
    let me: Arc<Mutex<Weak<PromptBridge>>> = Arc::new(Mutex::new(Weak::new()));
    let bridge = Arc::new(PromptBridge::with_timeout(
        {
            let seen = seen.clone();
            let me = me.clone();
            Arc::new(move |id: String, prompt: Prompt| {
                let reply = answer(&prompt);
                seen.lock().unwrap().push(prompt);
                if let (Some(r), Some(b)) = (reply, me.lock().unwrap().upgrade()) {
                    b.respond(&id, r);
                }
                Ok(())
            })
        },
        timeout,
    ));
    *me.lock().unwrap() = Arc::downgrade(&bridge);
    Scripted { bridge, seen }
}

fn answering(r: fn() -> PromptResponse) -> Scripted {
    scripted(Duration::from_secs(5), move |_| Some(r()))
}

fn fixture_key() -> HostKeyInfo {
    HostKeyInfo::from_openssh(FIXTURE_PUB).unwrap()
}

fn other_ed25519_key() -> HostKeyInfo {
    let k = generate(KeyAlgo::Ed25519, "", None).unwrap();
    HostKeyInfo::from_openssh(&k.public_openssh).unwrap()
}

fn known(host: &str, port: u16, key: &HostKeyInfo, source: KnownHostSource) -> KnownHost {
    let (algorithm, public_key) = key.openssh.split_once(' ').unwrap();
    KnownHost {
        id: Id::now_v7(),
        host_pattern: host.into(),
        port,
        algorithm: algorithm.into(),
        public_key: public_key.into(),
        added_at: 1,
        source,
        extra: Default::default(),
    }
}

fn stored(vault: &SharedVault) -> Vec<KnownHost> {
    vault.lock().unwrap().list::<KnownHost>().unwrap()
}

#[tokio::test]
async fn unknown_key_prompts_and_saves_on_accept() {
    let (_dir, vault) = vault();
    let s = answering(|| PromptResponse::Accept { save: true });
    let verifier = VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone());
    let key = fixture_key();

    let decision = verifier.verify("example.test", 2200, &key).await;
    assert_eq!(decision, HostKeyDecision::Accept);
    assert_eq!(
        *s.seen.lock().unwrap(),
        vec![Prompt::HostKeyUnknown {
            host: "example.test".into(),
            port: 2200,
            key: key.clone(),
        }]
    );
    let saved = stored(&vault);
    assert_eq!(saved.len(), 1);
    let kh = &saved[0];
    assert_eq!(
        (kh.host_pattern.as_str(), kh.port, kh.source),
        ("example.test", 2200, KnownHostSource::Prompt)
    );
    assert_eq!(format!("{} {}", kh.algorithm, kh.public_key), key.openssh);
    assert!(kh.added_at > 1_700_000_000, "added_at is unix seconds");

    // Now known: accepted silently, no second prompt.
    assert_eq!(
        verifier.verify("example.test", 2200, &key).await,
        HostKeyDecision::Accept
    );
    assert_eq!(s.seen.lock().unwrap().len(), 1);
    // Same host, other port: still unknown.
    verifier.verify("example.test", 22, &key).await;
    assert_eq!(s.seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn unknown_key_prompt_accept_once_is_not_saved() {
    let (_dir, vault) = vault();
    let s = answering(|| PromptResponse::Accept { save: false });
    let verifier = VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone());
    let key = fixture_key();
    assert_eq!(
        verifier.verify("example.test", 22, &key).await,
        HostKeyDecision::Accept
    );
    assert!(stored(&vault).is_empty());
    verifier.verify("example.test", 22, &key).await;
    assert_eq!(s.seen.lock().unwrap().len(), 2, "asked again next time");
}

#[tokio::test]
async fn unknown_key_prompt_rejected_or_unanswered_rejects() {
    let (_dir, vault) = vault();
    for s in [
        answering(|| PromptResponse::Reject),
        // A nonsensical answer to a host-key question is a refusal.
        answering(|| PromptResponse::Secret(SecretString::from("x"))),
        // Nobody answers: the bridge times out.
        scripted(Duration::from_millis(50), |_| None),
    ] {
        let verifier = VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone());
        assert_eq!(
            verifier.verify("example.test", 22, &fixture_key()).await,
            HostKeyDecision::Reject
        );
        assert_eq!(s.seen.lock().unwrap().len(), 1);
    }
    assert!(stored(&vault).is_empty());
}

#[tokio::test]
async fn changed_key_prompt_accept_with_save_replaces_stored_key() {
    let (_dir, vault) = vault();
    let old = other_ed25519_key();
    vault
        .lock()
        .unwrap()
        .upsert(known("example.test", 22, &old, KnownHostSource::Import))
        .unwrap();
    let s = answering(|| PromptResponse::Accept { save: true });
    let verifier = VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone());
    let new = fixture_key();

    assert_eq!(
        verifier.verify("example.test", 22, &new).await,
        HostKeyDecision::Accept
    );
    assert_eq!(
        *s.seen.lock().unwrap(),
        vec![Prompt::HostKeyChanged {
            host: "example.test".into(),
            port: 22,
            old: old.clone(),
            new: new.clone(),
        }]
    );
    let saved = stored(&vault);
    assert_eq!(saved.len(), 1, "old key replaced, not kept: {saved:?}");
    assert_eq!(
        format!("{} {}", saved[0].algorithm, saved[0].public_key),
        new.openssh
    );
    assert_eq!(saved[0].source, KnownHostSource::Prompt);
    // The old key is no longer trusted.
    assert_eq!(
        verifier.verify("example.test", 22, &new).await,
        HostKeyDecision::Accept
    );
    verifier.verify("example.test", 22, &old).await;
    assert_eq!(s.seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn changed_key_prompt_accept_once_keeps_stored_key() {
    let (_dir, vault) = vault();
    let old = other_ed25519_key();
    let entry = known("example.test", 22, &old, KnownHostSource::Prompt);
    vault.lock().unwrap().upsert(entry.clone()).unwrap();
    let s = answering(|| PromptResponse::Accept { save: false });
    let verifier = VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone());
    assert_eq!(
        verifier.verify("example.test", 22, &fixture_key()).await,
        HostKeyDecision::Accept
    );
    assert_eq!(stored(&vault), vec![entry]);
}

fn ecdsa_key(algo: KeyAlgo) -> HostKeyInfo {
    let k = generate(algo, "", None).unwrap();
    HostKeyInfo::from_openssh(&k.public_openssh).unwrap()
}

/// A known host presenting a key of another type gets the blocking
/// changed-key warning (default Reject), never the milder unknown prompt.
#[tokio::test]
async fn known_host_with_other_algorithm_prompts_changed() {
    let (_dir, vault) = vault();
    let ecdsa = ecdsa_key(KeyAlgo::EcdsaP256);
    let entry = known("example.test", 22, &ecdsa, KnownHostSource::Prompt);
    vault.lock().unwrap().upsert(entry.clone()).unwrap();
    // Nobody answers: the default is Reject.
    let s = scripted(Duration::from_millis(50), |_| None);
    let verifier = VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone());
    let key = fixture_key();
    assert_eq!(
        verifier.verify("example.test", 22, &key).await,
        HostKeyDecision::Reject
    );
    assert_eq!(
        *s.seen.lock().unwrap(),
        vec![Prompt::HostKeyChanged {
            host: "example.test".into(),
            port: 22,
            old: ecdsa,
            new: key,
        }]
    );
    assert_eq!(stored(&vault), vec![entry]);
}

/// The warning shows the stored key of the presented type when there is
/// one, even if a key of another type was stored first.
#[tokio::test]
async fn known_host_changed_prompt_prefers_same_algorithm_old_key() {
    let (_dir, vault) = vault();
    let ecdsa = ecdsa_key(KeyAlgo::EcdsaP256);
    let old_ed25519 = other_ed25519_key();
    {
        let mut v = vault.lock().unwrap();
        v.upsert(known("example.test", 22, &ecdsa, KnownHostSource::Prompt))
            .unwrap();
        v.upsert(known(
            "example.test",
            22,
            &old_ed25519,
            KnownHostSource::Prompt,
        ))
        .unwrap();
    }
    let s = answering(|| PromptResponse::Reject);
    let verifier = VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone());
    verifier.verify("example.test", 22, &fixture_key()).await;
    assert!(matches!(
        &s.seen.lock().unwrap()[0],
        Prompt::HostKeyChanged { old, .. } if *old == old_ed25519
    ));
}

/// Accepting a changed key with save replaces every key stored for that
/// host and port (of any type); entries for other hosts are untouched.
#[tokio::test]
async fn known_host_changed_accept_save_replaces_all_entries_for_host() {
    let (_dir, vault) = vault();
    let other_host = known(
        "other.test",
        22,
        &ecdsa_key(KeyAlgo::EcdsaP256),
        KnownHostSource::Prompt,
    );
    {
        let mut v = vault.lock().unwrap();
        for key in [ecdsa_key(KeyAlgo::EcdsaP256), ecdsa_key(KeyAlgo::EcdsaP384)] {
            v.upsert(known("example.test", 22, &key, KnownHostSource::Import))
                .unwrap();
        }
        v.upsert(other_host.clone()).unwrap();
    }
    let s = answering(|| PromptResponse::Accept { save: true });
    let verifier = VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone());
    let key = fixture_key();
    assert_eq!(
        verifier.verify("example.test", 22, &key).await,
        HostKeyDecision::Accept
    );
    assert!(matches!(
        s.seen.lock().unwrap()[0],
        Prompt::HostKeyChanged { .. }
    ));
    let saved = stored(&vault);
    assert_eq!(saved.len(), 2, "{saved:?}");
    assert!(saved.contains(&other_host));
    let mine: Vec<_> = saved
        .iter()
        .filter(|k| k.host_pattern == "example.test")
        .collect();
    assert_eq!(mine.len(), 1);
    assert_eq!(
        format!("{} {}", mine[0].algorithm, mine[0].public_key),
        key.openssh
    );
}

#[tokio::test]
async fn known_host_algorithms_are_listed_for_the_host() {
    let (_dir, vault) = vault();
    let ecdsa = ecdsa_key(KeyAlgo::EcdsaP256);
    {
        let mut v = vault.lock().unwrap();
        v.upsert(known("example.test", 22, &ecdsa, KnownHostSource::Prompt))
            .unwrap();
        v.upsert(known(
            "example.test",
            22,
            &ecdsa_key(KeyAlgo::EcdsaP256),
            KnownHostSource::Prompt,
        ))
        .unwrap();
        v.upsert(known(
            "example.test",
            22,
            &fixture_key(),
            KnownHostSource::Prompt,
        ))
        .unwrap();
        v.upsert(known(
            "example.test",
            2200,
            &ecdsa_key(KeyAlgo::EcdsaP384),
            KnownHostSource::Prompt,
        ))
        .unwrap();
    }
    let s = answering(|| PromptResponse::Reject);
    let verifier = VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone());
    assert_eq!(
        verifier.known_algorithms("example.test", 22).await,
        vec!["ecdsa-sha2-nistp256".to_string(), "ssh-ed25519".to_string()]
    );
    assert!(
        verifier
            .known_algorithms("unknown.test", 22)
            .await
            .is_empty()
    );
    vault.lock().unwrap().lock();
    assert!(
        verifier
            .known_algorithms("example.test", 22)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn known_host_imported_hashed_and_bracketed_entries_accept_silently() {
    let (_dir, vault) = vault();
    // `ssh-keygen -H` output for `hashed.example.com` (port 22) and
    // `[hashed.example.com]:2222`, plus a plain bracketed entry.
    let key_line = fixture_key().openssh;
    let file = format!(
        "|1|HaW8/opFzilgDxjQXnStU7xLnlk=|AAFGL7JKn8j+Jgphlt8Nu54tiFk= {key_line}\n\
         |1|7wmNmyEG5VYJiCsJAU3d+Fl/AJ4=|WbqvqfIzLjoRnqeDprqPRum7k14= {key_line}\n\
         [plain.example.com]:2200 {key_line}\n"
    );
    {
        let mut v = vault.lock().unwrap();
        for e in parse_known_hosts(&file) {
            v.upsert(KnownHost {
                id: Id::now_v7(),
                host_pattern: e.host_pattern,
                port: e.port,
                algorithm: e.algorithm,
                public_key: e.public_key,
                added_at: 1,
                source: KnownHostSource::Import,
                extra: Default::default(),
            })
            .unwrap();
        }
    }
    let s = answering(|| PromptResponse::Reject);
    let verifier = VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone());
    let key = fixture_key();
    for (host, port) in [
        ("hashed.example.com", 22),
        ("hashed.example.com", 2222),
        ("plain.example.com", 2200),
    ] {
        assert_eq!(
            verifier.verify(host, port, &key).await,
            HostKeyDecision::Accept,
            "{host}:{port}"
        );
    }
    assert!(s.seen.lock().unwrap().is_empty());
    // A hashed entry for another port does not vouch for this one.
    assert_eq!(
        verifier.verify("hashed.example.com", 2200, &key).await,
        HostKeyDecision::Reject
    );
    // Changed key behind a hashed name: the blocking warning.
    let other = other_ed25519_key();
    verifier.verify("hashed.example.com", 22, &other).await;
    assert!(matches!(
        s.seen.lock().unwrap().last().unwrap(),
        Prompt::HostKeyChanged { old, .. } if old.openssh == key.openssh
    ));
}

#[tokio::test]
async fn known_host_lookup_with_locked_vault_is_unverifiable_without_prompt() {
    let (_dir, vault) = vault();
    let key = fixture_key();
    vault
        .lock()
        .unwrap()
        .upsert(known("example.test", 22, &key, KnownHostSource::Prompt))
        .unwrap();
    vault.lock().unwrap().lock();
    let s = answering(|| PromptResponse::Accept { save: true });
    let verifier = VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone());
    // Even a key the vault knows: we cannot check it, so we never accept,
    // and say why (distinct from the user rejecting it).
    assert_eq!(
        verifier.verify("example.test", 22, &key).await,
        HostKeyDecision::Unverifiable
    );
    assert_eq!(
        verifier.verify("unknown.test", 22, &key).await,
        HostKeyDecision::Unverifiable
    );
    assert!(s.seen.lock().unwrap().is_empty(), "no prompt while locked");
}

#[tokio::test]
async fn known_host_lookup_with_uninitialised_vault_is_unverifiable() {
    let dir = tempfile::tempdir().unwrap();
    let v = VaultService::open_for_tests(
        &dir.path().join("vault.sqlite"),
        Box::new(MemoryKeyStore::default()),
    )
    .unwrap();
    let vault: SharedVault = Arc::new(Mutex::new(v));
    let s = answering(|| PromptResponse::Accept { save: true });
    let verifier = VaultHostKeyVerifier::new(vault, s.bridge.clone());
    assert_eq!(
        verifier.verify("example.test", 22, &fixture_key()).await,
        HostKeyDecision::Unverifiable
    );
    assert!(s.seen.lock().unwrap().is_empty());
}

/// Fixture: a stored wrong key for 127.0.0.1:2222. The connection raises the
/// changed-key warning and, with nobody accepting it, is rejected; the stored
/// key is left alone.
#[tokio::test]
async fn changed_key_prompts_changed_and_defaults_reject() {
    if !fixture_enabled() {
        return;
    }
    let (_dir, vault) = vault();
    let wrong = other_ed25519_key();
    let entry = known("127.0.0.1", 2222, &wrong, KnownHostSource::Prompt);
    vault.lock().unwrap().upsert(entry.clone()).unwrap();
    // Nobody answers; a short timeout stands in for the user walking away.
    let s = scripted(Duration::from_millis(500), |_| None);
    let verifier = Arc::new(VaultHostKeyVerifier::new(vault.clone(), s.bridge.clone()));

    let params = ConnectParams {
        host: "127.0.0.1".into(),
        port: 2222,
        username: "tether".into(),
        auth: AuthMethod::Password(SecretString::from("tether-test")),
        term: "xterm-256color".into(),
        cols: 80,
        rows: 24,
        connect_timeout: Duration::from_secs(15),
        keepalive: None,
        env: Vec::new(),
    };
    let err = RusshTransport
        .open_shell(params, verifier)
        .await
        .err()
        .expect("must not connect");
    assert!(matches!(err, SshError::HostKeyRejected), "got {err:?}");

    let seen = s.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "prompts: {seen:?}");
    let Prompt::HostKeyChanged {
        host,
        port,
        old,
        new,
    } = &seen[0]
    else {
        panic!("expected HostKeyChanged, got {:?}", seen[0]);
    };
    assert_eq!((host.as_str(), *port), ("127.0.0.1", 2222));
    assert_eq!(old, &wrong);
    assert_eq!(new.algorithm, "ssh-ed25519");
    assert_ne!(new.sha256_fingerprint, wrong.sha256_fingerprint);
    assert_eq!(stored(&vault), vec![entry], "stored key untouched");
}
