//! TLS for the sync client: certificate pinning, and recognising what went
//! wrong when a connection fails.
//!
//! # Trust model
//!
//! A self-hosted server usually has a self-signed certificate, so there is no
//! CA to trust. The join code carries the SHA-256 of the certificate's
//! SubjectPublicKeyInfo (the *pin*); with a pin the connection is accepted if
//! and only if the server's end-entity certificate has exactly that key. The
//! pin **is** the server's identity: the certificate's names, validity
//! period and issuer are deliberately not checked (a renewed certificate on
//! the same key keeps working, and the LAN address may change). WebPKI is
//! never consulted when a pin is set. The TLS handshake signatures are still
//! verified, with the crypto provider's algorithms, so the server must hold
//! the private key. A different key fails with
//! [`SyncError::CertificateChanged`]. Without a pin the normal WebPKI roots
//! (Mozilla's, via `webpki-roots`) and hostname checks apply.

use std::error::Error;
use std::fmt;
use std::io;
use std::sync::{Arc, Mutex};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, aws_lc_rs};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, OtherError, RootCertStore,
    SignatureScheme,
};
use sha2::{Digest, Sha256};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::client::{Client, FIRST_CONTACT_PATIENCE};
use crate::error::SyncError;

/// SHA-256 of a server certificate's SubjectPublicKeyInfo (DER).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Pin([u8; 32]);

impl Pin {
    /// A pin from its 32 bytes (the value in a join code).
    pub fn new(bytes: [u8; 32]) -> Pin {
        Pin(bytes)
    }

    /// The 32 pin bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The pin of a DER certificate; `None` if it does not parse.
    pub fn of_certificate(cert_der: &[u8]) -> Option<Pin> {
        let (_, cert) = X509Certificate::from_der(cert_der).ok()?;
        Some(Pin(
            Sha256::digest(cert.tbs_certificate.subject_pki.raw).into()
        ))
    }
}

impl From<[u8; 32]> for Pin {
    fn from(bytes: [u8; 32]) -> Self {
        Pin(bytes)
    }
}

impl fmt::Debug for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A pin is public (it is in every join code), but a short prefix is
        // enough for logs.
        write!(
            f,
            "Pin({:02x}{:02x}{:02x}{:02x}..)",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

/// Marker carried inside the rustls error so a mismatch can be told apart
/// from every other TLS failure further up the stack.
#[derive(Debug)]
struct PinMismatch;

impl fmt::Display for PinMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("server certificate changed")
    }
}

impl Error for PinMismatch {}

#[derive(Debug)]
struct PinVerifier {
    pin: Pin,
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match Pin::of_certificate(end_entity.as_ref()) {
            Some(p) if p == self.pin => Ok(ServerCertVerified::assertion()),
            Some(_) => Err(rustls::Error::InvalidCertificate(CertificateError::Other(
                OtherError(Arc::new(PinMismatch)),
            ))),
            None => Err(rustls::Error::InvalidCertificate(
                CertificateError::BadEncoding,
            )),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// The rustls client configuration for talking to the sync server (TLS 1.2
/// and 1.3, aws-lc-rs, HTTP/1.1 only).
///
/// With a `pin`, only the certificate with that key is accepted and WebPKI is
/// never consulted (see the module docs). Without one, certificates must
/// chain to the Mozilla root set.
pub fn client_config(pin: Option<Pin>) -> ClientConfig {
    match pin {
        Some(pin) => finish(
            builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(PinVerifier {
                    pin,
                    algs: aws_lc_rs::default_provider().signature_verification_algorithms,
                }))
                .with_no_client_auth(),
        ),
        None => webpki_config(RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        }),
    }
}

/// What [`probe`] saw of a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// `None` when the certificate passed the normal WebPKI check (connect
    /// unpinned); otherwise the leaf's pin, to connect pinned with.
    pub pin: Option<Pin>,
}

/// Accepts any certificate, after recording whether WebPKI trusts it and the
/// leaf's pin. Only for [`probe`], whose one request carries nothing secret.
#[derive(Debug)]
struct CapturingVerifier {
    webpki: Arc<WebPkiServerVerifier>,
    /// `Some(None)`: WebPKI passed; `Some(Some(pin))`: it did not.
    seen: Mutex<Option<Option<Pin>>>,
}

impl ServerCertVerifier for CapturingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let pin = Pin::of_certificate(end_entity.as_ref()).ok_or(
            rustls::Error::InvalidCertificate(CertificateError::BadEncoding),
        )?;
        let trusted = self
            .webpki
            .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
            .is_ok();
        *self.seen.lock().unwrap_or_else(|p| p.into_inner()) = Some((!trusted).then_some(pin));
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}

/// First look at a typed server address (`https://host:port`): one
/// `GET /v1/health`, accepting whatever certificate is shown, to learn
/// whether it passes the normal WebPKI check (spec "TLS"). Nothing secret is
/// sent. [`SyncError::Unreachable`] when nothing answers,
/// `Protocol("not a Termlyx server")` when the answer is not a healthy
/// Termlyx server's.
pub async fn probe(url: &str) -> Result<Probe, SyncError> {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let webpki = WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(aws_lc_rs::default_provider()),
    )
    .build()
    .map_err(|_| SyncError::Protocol("could not set up TLS".into()))?;
    let verifier = Arc::new(CapturingVerifier {
        webpki,
        seen: Mutex::new(None),
    });
    let config = finish(
        builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier.clone())
            .with_no_client_auth(),
    );
    let client = Client::with_tls(url, None, config)?;
    if !FIRST_CONTACT_PATIENCE.is_zero() {
        client.reach(FIRST_CONTACT_PATIENCE).await?;
    }
    let not_termlyx = || SyncError::Protocol("not a Termlyx server".into());
    let body = match client.health().await {
        Ok(body) => body,
        Err(e @ (SyncError::Unreachable(_) | SyncError::UpdateClient)) => return Err(e),
        Err(_) => return Err(not_termlyx()),
    };
    let healthy = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .is_some_and(|v| v.get("status").and_then(|s| s.as_str()) == Some("ok"));
    if !healthy {
        return Err(not_termlyx());
    }
    // Plain loopback `http://` never shows a certificate: nothing to pin.
    let pin = verifier
        .seen
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .flatten();
    Ok(Probe { pin })
}

/// The unpinned configuration: normal WebPKI path and hostname validation
/// against `roots`.
fn webpki_config(roots: RootCertStore) -> ClientConfig {
    finish(
        builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

fn builder() -> rustls::ConfigBuilder<ClientConfig, rustls::WantsVerifier> {
    ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("aws-lc-rs supports the default protocol versions")
}

fn finish(mut config: ClientConfig) -> ClientConfig {
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
}

/// Calls `f` on `err` and on each error it wraps, until `f` returns `Some`.
/// Looks inside `io::Error` too: its `source()` skips the error it holds,
/// which is where tokio-rustls puts the TLS error.
fn find_in_chain<T>(
    err: &(dyn Error + 'static),
    f: &mut dyn FnMut(&(dyn Error + 'static)) -> Option<T>,
) -> Option<T> {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(found) = f(e) {
            return Some(found);
        }
        if let Some(inner) = e
            .downcast_ref::<io::Error>()
            .and_then(io::Error::get_ref)
            .and_then(|inner| find_in_chain(inner, f))
        {
            return Some(inner);
        }
        cur = e.source();
    }
    None
}

/// Turns a transport failure (from reqwest or tungstenite) into the matching
/// [`SyncError`]: a pin mismatch is [`SyncError::CertificateChanged`],
/// everything else is [`SyncError::Unreachable`] with a short reason that
/// never contains a URL.
pub(crate) fn classify(err: &(dyn Error + 'static), timed_out: bool) -> SyncError {
    let mismatch = find_in_chain(err, &mut |e| match e.downcast_ref::<rustls::Error>() {
        Some(rustls::Error::InvalidCertificate(CertificateError::Other(other))) => {
            other.0.downcast_ref::<PinMismatch>().map(|_| ())
        }
        _ => None,
    });
    if mismatch.is_some() {
        return SyncError::CertificateChanged;
    }
    let io_kind = find_in_chain(err, &mut |e| {
        e.downcast_ref::<io::Error>().map(io::Error::kind)
    });
    if timed_out || io_kind == Some(io::ErrorKind::TimedOut) {
        return SyncError::Unreachable("timed out".into());
    }
    // A TLS failure that is not a pin mismatch (unknown issuer, bad
    // handshake): the rustls message names no host and carries no secret.
    if let Some(tls) = find_in_chain(err, &mut |e| {
        e.downcast_ref::<rustls::Error>().map(ToString::to_string)
    }) {
        return SyncError::Unreachable(format!("TLS: {tls}"));
    }
    // A name that does not resolve: hyper's "dns error", or std's lookup
    // failure (WebSocket). Their causes can name the host, so not shown.
    let dns = find_in_chain(err, &mut |e| {
        let m = e.to_string();
        (m == "dns error" || m.starts_with("failed to lookup address")).then_some(())
    });
    if dns.is_some() {
        return SyncError::Unreachable("DNS lookup failed".into());
    }
    match io_kind {
        Some(kind) if kind != io::ErrorKind::Other => SyncError::Unreachable(kind.to_string()),
        _ => SyncError::Unreachable("could not connect".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
    use rustls::crypto::aws_lc_rs::sign::any_supported_type;
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::server::{ClientHello, ResolvesServerCert};
    use rustls::sign::{CertifiedKey, Signer, SigningKey};
    use rustls::version::{TLS12, TLS13};
    use rustls::{
        ClientConnection, ServerConfig, ServerConnection, SignatureAlgorithm,
        SupportedProtocolVersion,
    };

    /// What a misbehaving server does to its handshake signature.
    #[derive(Clone, Copy, Debug)]
    enum Fault {
        None,
        /// Corrupts the signature bytes.
        FlipByte,
        /// Claims another (advertised, supported) scheme than the key signed with.
        ClaimScheme(SignatureScheme),
    }

    #[derive(Debug)]
    struct FaultyKey {
        inner: Arc<dyn SigningKey>,
        fault: Fault,
    }

    impl SigningKey for FaultyKey {
        fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
            Some(Box::new(FaultySigner {
                inner: self.inner.choose_scheme(offered)?,
                fault: self.fault,
            }))
        }
        fn algorithm(&self) -> SignatureAlgorithm {
            self.inner.algorithm()
        }
    }

    #[derive(Debug)]
    struct FaultySigner {
        inner: Box<dyn Signer>,
        fault: Fault,
    }

    impl Signer for FaultySigner {
        fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
            let mut sig = self.inner.sign(message)?;
            if let Fault::FlipByte = self.fault {
                let last = sig.len() - 1;
                sig[last] ^= 1;
            }
            Ok(sig)
        }
        fn scheme(&self) -> SignatureScheme {
            match self.fault {
                Fault::ClaimScheme(s) => s,
                _ => self.inner.scheme(),
            }
        }
    }

    #[derive(Debug)]
    struct Fixed(Arc<CertifiedKey>);

    impl ResolvesServerCert for Fixed {
        fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
            Some(self.0.clone())
        }
    }

    fn signing_key(key: &KeyPair) -> Arc<dyn SigningKey> {
        let der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        any_supported_type(&der).expect("supported key")
    }

    /// A self-signed certificate for `name` and its key.
    fn self_signed(name: &str) -> (CertificateDer<'static>, KeyPair) {
        let key = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec![name.to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert.der().clone(), key)
    }

    fn server_config(
        chain: Vec<CertificateDer<'static>>,
        key: Arc<dyn SigningKey>,
        version: &'static SupportedProtocolVersion,
    ) -> ServerConfig {
        ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
            .with_protocol_versions(&[version])
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(Fixed(Arc::new(CertifiedKey::new(chain, key)))))
    }

    /// Runs a TLS handshake in memory. `Err((side, error))` names who gave up.
    fn handshake(
        client: ClientConfig,
        server: ServerConfig,
        name: &'static str,
    ) -> Result<rustls::ProtocolVersion, (&'static str, rustls::Error)> {
        let mut client =
            ClientConnection::new(Arc::new(client), ServerName::try_from(name).unwrap()).unwrap();
        let mut server = ServerConnection::new(Arc::new(server)).unwrap();
        for _ in 0..16 {
            let mut to_server = Vec::new();
            client.write_tls(&mut to_server).unwrap();
            if !to_server.is_empty() {
                server.read_tls(&mut &to_server[..]).unwrap();
                server.process_new_packets().map_err(|e| ("server", e))?;
            }
            let mut to_client = Vec::new();
            server.write_tls(&mut to_client).unwrap();
            if !to_client.is_empty() {
                client.read_tls(&mut &to_client[..]).unwrap();
                client.process_new_packets().map_err(|e| ("client", e))?;
            }
            if !client.is_handshaking() && !server.is_handshaking() {
                return Ok(client.protocol_version().unwrap());
            }
        }
        panic!("handshake did not finish");
    }

    const VERSIONS: [(&SupportedProtocolVersion, rustls::ProtocolVersion); 2] = [
        (&TLS13, rustls::ProtocolVersion::TLSv1_3),
        (&TLS12, rustls::ProtocolVersion::TLSv1_2),
    ];

    /// Asserts the *client* refused the server.
    fn assert_client_refused(r: Result<rustls::ProtocolVersion, (&str, rustls::Error)>) {
        match r {
            Err(("client", _)) => {}
            other => panic!("the pinned client must refuse this server, got {other:?}"),
        }
    }

    #[test]
    fn pinned_client_accepts_exactly_the_pinned_key() {
        let (cert, key) = self_signed("anything.example");
        let pin = Pin::of_certificate(cert.as_ref()).unwrap();
        for (version, expect) in VERSIONS {
            // Any name: the pin is the identity.
            let got = handshake(
                client_config(Some(pin)),
                server_config(vec![cert.clone()], signing_key(&key), version),
                "whatever.invalid",
            );
            assert_eq!(got.unwrap(), expect);
        }
    }

    #[test]
    fn reasons_name_the_failure_without_the_host() {
        /// hyper-util's connect error: "dns error" over the resolver's cause.
        #[derive(Debug)]
        struct Dns(io::Error);
        impl std::fmt::Display for Dns {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("dns error")
            }
        }
        impl Error for Dns {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                Some(&self.0)
            }
        }
        let unreachable = |r: &str| SyncError::Unreachable(r.into());
        let dns = Dns(io::Error::other("nodename nor servname provided: nas.lan"));
        assert_eq!(classify(&dns, false), unreachable("DNS lookup failed"));
        let lookup = io::Error::other("failed to lookup address information: nas.lan");
        assert_eq!(classify(&lookup, false), unreachable("DNS lookup failed"));
        let refused = io::Error::from(io::ErrorKind::ConnectionRefused);
        assert_eq!(classify(&refused, false), unreachable("connection refused"));
        assert_eq!(
            classify(&refused, false).to_string(),
            "server unreachable (connection refused)"
        );
        assert_eq!(classify(&refused, true), unreachable("timed out"));
    }

    #[test]
    fn another_certificate_is_a_pin_mismatch() {
        let (cert, key) = self_signed("localhost");
        let pin = Pin::of_certificate(cert.as_ref()).unwrap();
        let (other, other_key) = self_signed("localhost");
        for (version, _) in VERSIONS {
            let Err(("client", err)) = handshake(
                client_config(Some(pin)),
                server_config(vec![other.clone()], signing_key(&other_key), version),
                "localhost",
            ) else {
                panic!("a different certificate must be refused");
            };
            assert_eq!(classify(&err, false), SyncError::CertificateChanged);
        }
        let _ = key;
    }

    /// The attack the signature checks exist for: present the real
    /// certificate (public, so the pin matches) without holding its key.
    #[test]
    fn replayed_certificate_without_its_key_is_refused() {
        let (cert, _real_key) = self_signed("localhost");
        let pin = Pin::of_certificate(cert.as_ref()).unwrap();
        let attacker = KeyPair::generate().unwrap();
        for (version, _) in VERSIONS {
            assert_client_refused(handshake(
                client_config(Some(pin)),
                server_config(vec![cert.clone()], signing_key(&attacker), version),
                "localhost",
            ));
        }
    }

    #[test]
    fn corrupted_handshake_signature_is_refused() {
        let (cert, key) = self_signed("localhost");
        let pin = Pin::of_certificate(cert.as_ref()).unwrap();
        for (version, _) in VERSIONS {
            let faulty = Arc::new(FaultyKey {
                inner: signing_key(&key),
                fault: Fault::FlipByte,
            });
            assert_client_refused(handshake(
                client_config(Some(pin)),
                server_config(vec![cert.clone()], faulty, version),
                "localhost",
            ));
        }
    }

    #[test]
    fn signature_under_the_wrong_scheme_is_refused() {
        let (cert, key) = self_signed("localhost");
        let pin = Pin::of_certificate(cert.as_ref()).unwrap();
        for scheme in [
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
        ] {
            for (version, _) in VERSIONS {
                let faulty = Arc::new(FaultyKey {
                    inner: signing_key(&key),
                    fault: Fault::ClaimScheme(scheme),
                });
                assert_client_refused(handshake(
                    client_config(Some(pin)),
                    server_config(vec![cert.clone()], faulty, version),
                    "localhost",
                ));
            }
        }
        // Control: the same setup without the fault is accepted.
        let honest = Arc::new(FaultyKey {
            inner: signing_key(&key),
            fault: Fault::None,
        });
        handshake(
            client_config(Some(pin)),
            server_config(vec![cert], honest, &TLS13),
            "localhost",
        )
        .unwrap();
    }

    /// Without a pin the normal WebPKI rules hold: a certificate that chains
    /// to a trusted root but is valid for another name is refused.
    #[test]
    fn unpinned_client_refuses_a_certificate_for_another_name() {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);
        let leaf_key = KeyPair::generate().unwrap();
        let leaf = CertificateParams::new(vec!["other.example".to_owned()])
            .unwrap()
            .signed_by(&leaf_key, &issuer)
            .unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        let client = || webpki_config(roots.clone());
        let server =
            |version| server_config(vec![leaf.der().clone()], signing_key(&leaf_key), version);

        for (version, expect) in VERSIONS {
            // Control: the right name is accepted.
            assert_eq!(
                handshake(client(), server(version), "other.example").unwrap(),
                expect
            );
            // Another name is not.
            let Err(("client", err)) = handshake(client(), server(version), "sync.example.com")
            else {
                panic!("a certificate for another name must be refused");
            };
            assert!(
                matches!(
                    err,
                    rustls::Error::InvalidCertificate(
                        CertificateError::NotValidForName
                            | CertificateError::NotValidForNameContext { .. }
                    )
                ),
                "{err:?}"
            );
        }
        // The real config (Mozilla roots) trusts none of this.
        assert!(handshake(client_config(None), server(&TLS13), "other.example").is_err());
    }
}
