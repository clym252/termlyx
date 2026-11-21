//! TLS modes, certificate persistence and the SPKI pin.

mod common;

use common::*;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tether_server::config::{Config, TlsMode};
use tether_server::tls;
use x509_parser::prelude::*;

fn sans(cert_pem: &str) -> Vec<String> {
    let der = rustls_pki_types::CertificateDer::from_pem_slice(cert_pem.as_bytes()).unwrap();
    let (_, cert) = X509Certificate::from_der(der.as_ref()).unwrap();
    let mut out = vec![];
    for ext in cert.extensions() {
        if let ParsedExtension::SubjectAlternativeName(san) = ext.parsed_extension() {
            for n in &san.general_names {
                match n {
                    GeneralName::DNSName(d) => out.push(d.to_string()),
                    GeneralName::IPAddress(b) => out.push(format!("{b:?}")),
                    _ => {}
                }
            }
        }
    }
    out
}

use rustls_pki_types::pem::PemObject;

#[tokio::test]
async fn self_signed_cert_persists_and_pin_stable() {
    let srv = spawn_with(|c| {
        c.tls = TlsMode::SelfSigned;
        c.names = vec!["sync.example.com".into()];
        c.ips = vec![
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 8)),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        ];
    })
    .await;
    assert_ne!(srv.pin, [0u8; 32]);
    let dir = srv.state.cfg.data_dir.clone();
    let cert_pem = std::fs::read_to_string(dir.join("tls/cert.pem")).unwrap();
    let names = sans(&cert_pem);
    assert!(
        names.contains(&"sync.example.com".to_string()),
        "{names:?}"
    );
    assert_eq!(names.len(), 3, "{names:?}");

    // Files on disk are private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in ["tls/key.pem", "tls/cert.pem"] {
            let mode = std::fs::metadata(dir.join(f)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{f}");
        }
    }

    let pin = srv.pin;
    let srv = srv.restart().await;
    assert_eq!(srv.pin, pin, "same dir, same key, same pin");
    assert_eq!(
        std::fs::read_to_string(dir.join("tls/cert.pem")).unwrap(),
        cert_pem
    );
    // And the pinned client really connects.
    assert_eq!(srv.get("/v1/health").await.status, 200);
}

#[tokio::test]
async fn changed_names_reissue_the_cert_on_the_same_key() {
    let srv = spawn_with(|c| {
        c.tls = TlsMode::SelfSigned;
        c.names = vec!["a.example".into()];
    })
    .await;
    let pin = srv.pin;
    let dir = srv.state.cfg.data_dir.clone();
    let before = std::fs::read_to_string(dir.join("tls/cert.pem")).unwrap();

    let mut cfg = Config::new(dir.clone());
    cfg.tls = TlsMode::SelfSigned;
    cfg.names = vec!["a.example".into(), "b.example".into()];
    let m = tls::load_or_create(&cfg).unwrap();
    assert_eq!(m.spki_pin(), pin, "renewed on the same key keeps the pin");
    let after = std::fs::read_to_string(dir.join("tls/cert.pem")).unwrap();
    assert_ne!(before, after);
    assert!(sans(&after).contains(&"b.example".to_string()));
}

#[tokio::test]
async fn spawn_tls_serves_health_over_pinned_tls() {
    let srv = spawn_tls().await;
    assert!(srv.url.starts_with("https://127.0.0.1:"));
    let r = srv.get("/v1/health").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.headers.get("x-tether-protocol").unwrap(), "1");
    // A client pinned to some other key refuses the server.
    assert!(
        srv.try_request_with_pin([7u8; 32], "GET", "/v1/health")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn off_mode_refuses_non_loopback_bind() {
    let dir = TempDir::new().unwrap();
    let mut cfg = Config::new(dir.path().to_path_buf());
    cfg.tls = TlsMode::Off;
    cfg.bind = "0.0.0.0:0".parse::<SocketAddr>().unwrap();
    let err = tether_server::start(cfg.clone())
        .await
        .err()
        .expect("must refuse");
    assert!(err.to_string().contains("loopback"), "{err}");
    assert!(cfg.check().is_err());
    // IPv6 loopback is fine, a LAN address is not.
    cfg.bind = "192.168.1.8:8443".parse().unwrap();
    assert!(cfg.check().is_err());
    cfg.bind = "[::1]:8443".parse().unwrap();
    assert!(cfg.check().is_ok());

    // Explicitly allowed (behind the user's own proxy on another host).
    cfg.allow_plain_remote = true;
    cfg.bind = "0.0.0.0:0".parse().unwrap();
    let running = tether_server::start(cfg).await.expect("allow_plain_remote");
    running.shutdown().await;
}

#[tokio::test]
async fn files_mode_loads_given_cert() {
    let dir = TempDir::new().unwrap();
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["files.example".into()]).unwrap();
    params.distinguished_name = rcgen::DistinguishedName::new();
    let cert = params.self_signed(&key).unwrap();
    let cert_path = dir.path().join("my-cert.pem");
    let key_path = dir.path().join("my-key.pem");
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, key.serialize_pem()).unwrap();

    use sha2::{Digest, Sha256};
    let expected: [u8; 32] =
        Sha256::digest(rcgen::PublicKeyData::subject_public_key_info(&key)).into();

    let data = TempDir::new().unwrap();
    let mut cfg = Config::new(data.path().to_path_buf());
    cfg.tls = TlsMode::Files {
        cert: cert_path,
        key: key_path,
    };
    let m = tls::load_or_create(&cfg).unwrap();
    assert_eq!(m.spki_pin(), expected);
    assert!(
        !data.path().join("tls").exists(),
        "files mode generates nothing"
    );

    // A server started from those files presents that key.
    cfg.bind = "127.0.0.1:0".parse().unwrap();
    let running = tether_server::start(cfg).await.unwrap();
    assert_eq!(running.pin, Some(expected));
    running.shutdown().await;

    // Missing files are a clear error.
    let mut bad = Config::new(data.path().to_path_buf());
    bad.tls = TlsMode::Files {
        cert: data.path().join("nope.pem"),
        key: data.path().join("nope-key.pem"),
    };
    assert!(tls::load_or_create(&bad).is_err());
}
