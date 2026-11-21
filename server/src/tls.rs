//! TLS material: the persisted self-signed certificate, operator-supplied
//! files, and the SPKI pin.
//!
//! The pin is the SHA-256 of the certificate's SubjectPublicKeyInfo, so a
//! certificate reissued on the same key keeps the pin clients already hold.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, ring as ring_provider};
use rustls::pki_types::{ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::config::{Config, TlsMode};

/// Self-signed certificates are valid for twenty years.
const VALIDITY_DAYS: i64 = 365 * 20 + 5;
/// A self-signed certificate with less than this left is reissued.
const RENEW_BEFORE_DAYS: i64 = 30;

/// TLS setup failures.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// `TETHER_TLS=off` has no certificate.
    #[error("TLS is off; there is no certificate")]
    Off,
    /// Reading or writing a TLS file failed.
    #[error("{path}: {reason}")]
    Io {
        /// The file concerned.
        path: String,
        /// The underlying error.
        reason: String,
    },
    /// A PEM file or certificate could not be parsed.
    #[error("invalid TLS material: {0}")]
    Invalid(String),
}

fn io_err(path: &Path, e: impl ToString) -> TlsError {
    TlsError::Io {
        path: path.display().to_string(),
        reason: e.to_string(),
    }
}

/// A certificate chain with its private key.
pub struct TlsMaterial {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    pin: [u8; 32],
    names: Vec<String>,
}

// No `Debug`: it holds a private key.

impl TlsMaterial {
    fn new(
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<Self, TlsError> {
        let leaf = chain
            .first()
            .ok_or_else(|| TlsError::Invalid("empty certificate chain".into()))?;
        let (_, cert) = X509Certificate::from_der(leaf.as_ref())
            .map_err(|e| TlsError::Invalid(format!("certificate: {e}")))?;
        Ok(Self {
            pin: Sha256::digest(cert.tbs_certificate.subject_pki.raw).into(),
            names: cert_names(&cert),
            chain,
            key,
        })
    }

    /// SHA-256 of the leaf certificate's SubjectPublicKeyInfo DER.
    pub fn spki_pin(&self) -> [u8; 32] {
        self.pin
    }

    /// DNS names and IP addresses in the leaf certificate's SAN extension.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// A rustls server configuration (ring provider, TLS 1.2 and 1.3,
    /// HTTP/1.1 only).
    pub fn server_config(&self) -> Result<Arc<ServerConfig>, TlsError> {
        let provider = Arc::new(ring_provider::default_provider());
        let mut cfg = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| TlsError::Invalid(e.to_string()))?
            .with_no_client_auth()
            .with_single_cert(self.chain.clone(), self.key.clone_key())
            .map_err(|e| TlsError::Invalid(e.to_string()))?;
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(cfg))
    }
}

/// The SHA-256 of a DER certificate's SubjectPublicKeyInfo, `None` if it does
/// not parse.
pub fn spki_pin_of_cert(cert_der: &[u8]) -> Option<[u8; 32]> {
    let (_, cert) = X509Certificate::from_der(cert_der).ok()?;
    Some(Sha256::digest(cert.tbs_certificate.subject_pki.raw).into())
}

/// The pin of the certificate the server serves, read from disk without
/// creating anything (unlike [`load_or_create`]): `{data}/tls/cert.pem` in
/// self-signed mode, the operator's certificate in `files` mode, `None` when
/// TLS is off. Used by `tether-server health` to trust exactly the local cert.
pub fn local_pin(cfg: &Config) -> Result<Option<[u8; 32]>, TlsError> {
    let path = match &cfg.tls {
        TlsMode::Off => return Ok(None),
        TlsMode::Files { cert, .. } => cert.clone(),
        TlsMode::SelfSigned => cfg.data_dir.join("tls").join("cert.pem"),
    };
    let leaf = CertificateDer::pem_file_iter(&path)
        .map_err(|e| io_err(&path, e))?
        .next()
        .ok_or_else(|| TlsError::Invalid(format!("{}: no certificate", path.display())))?
        .map_err(|e| io_err(&path, e))?;
    spki_pin_of_cert(leaf.as_ref())
        .map(Some)
        .ok_or_else(|| TlsError::Invalid(format!("{}: unparsable certificate", path.display())))
}

/// A rustls client configuration that accepts exactly the certificate whose
/// SubjectPublicKeyInfo hashes to `pin` (any name, any validity). No WebPKI.
pub fn pinned_client_config(pin: [u8; 32]) -> Arc<ClientConfig> {
    let provider = Arc::new(ring_provider::default_provider());
    let verifier = PinVerifier {
        pin,
        algs: provider.signature_verification_algorithms,
    };
    Arc::new(
        ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth(),
    )
}

#[derive(Debug)]
struct PinVerifier {
    pin: [u8; 32],
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
        match spki_pin_of_cert(end_entity.as_ref()) {
            Some(p) if p == self.pin => Ok(ServerCertVerified::assertion()),
            _ => Err(rustls::Error::General("server certificate changed".into())),
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

fn cert_names(cert: &X509Certificate<'_>) -> Vec<String> {
    let mut out = vec![];
    for ext in cert.extensions() {
        if let ParsedExtension::SubjectAlternativeName(san) = ext.parsed_extension() {
            for n in &san.general_names {
                match n {
                    GeneralName::DNSName(d) => out.push((*d).to_string()),
                    GeneralName::IPAddress(b) => match b.len() {
                        4 => out.push(std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string()),
                        16 => {
                            let mut a = [0u8; 16];
                            a.copy_from_slice(b);
                            out.push(std::net::Ipv6Addr::from(a).to_string());
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }
    out
}

/// Load (or, for `SelfSigned`, create) the server's TLS material.
///
/// * `SelfSigned`: `{data}/tls/{key.pem,cert.pem}`. A missing key is generated
///   (P-256). A missing, expiring or name-mismatched certificate is reissued on
///   the existing key, so the pin never changes while the key lives.
/// * `Files`: the operator's PEM files, read as given; nothing is written.
/// * `Off`: [`TlsError::Off`].
pub fn load_or_create(cfg: &Config) -> Result<TlsMaterial, TlsError> {
    match &cfg.tls {
        TlsMode::Off => Err(TlsError::Off),
        TlsMode::Files { cert, key } => load_files(cert, key),
        TlsMode::SelfSigned => load_or_create_self_signed(cfg),
    }
}

fn load_files(cert: &Path, key: &Path) -> Result<TlsMaterial, TlsError> {
    let chain = CertificateDer::pem_file_iter(cert)
        .map_err(|e| io_err(cert, e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io_err(cert, e))?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| io_err(key, e))?;
    TlsMaterial::new(chain, key)
}

fn load_or_create_self_signed(cfg: &Config) -> Result<TlsMaterial, TlsError> {
    let dir = cfg.data_dir.join("tls");
    let key_path = dir.join("key.pem");
    let cert_path = dir.join("cert.pem");

    let key_pair = if key_path.exists() {
        let pem = fs::read_to_string(&key_path).map_err(|e| io_err(&key_path, e))?;
        KeyPair::from_pem(&pem)
            .map_err(|e| TlsError::Invalid(format!("{}: {e}", key_path.display())))?
    } else {
        let kp = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
            .map_err(|e| TlsError::Invalid(e.to_string()))?;
        create_private_dir(&dir)?;
        write_private(&key_path, kp.serialize_pem().as_bytes())?;
        kp
    };

    if !cert_is_current(&cert_path, cfg) {
        let cert = issue(cfg, &key_pair)?;
        create_private_dir(&dir)?;
        write_private(&cert_path, cert.as_bytes())?;
    }

    load_files(&cert_path, &key_path)
}

/// True when `cert_path` holds a certificate that covers every configured name
/// and IP and has more than [`RENEW_BEFORE_DAYS`] left.
fn cert_is_current(cert_path: &Path, cfg: &Config) -> bool {
    let Ok(pem) = fs::read(cert_path) else {
        return false;
    };
    let Ok(der) = CertificateDer::from_pem_slice(&pem) else {
        return false;
    };
    let Ok((_, cert)) = X509Certificate::from_der(der.as_ref()) else {
        return false;
    };
    let have = cert_names(&cert);
    let covers = cfg
        .names
        .iter()
        .cloned()
        .chain(cfg.ips.iter().map(|i| i.to_string()));
    for want in covers {
        if !have.iter().any(|h| h.eq_ignore_ascii_case(&want)) {
            return false;
        }
    }
    let renew_at = time::OffsetDateTime::now_utc() + time::Duration::days(RENEW_BEFORE_DAYS);
    cert.validity().not_after.to_datetime() > renew_at
}

fn issue(cfg: &Config, key: &KeyPair) -> Result<String, TlsError> {
    let mut params = CertificateParams::default();
    for n in &cfg.names {
        let dns = n
            .clone()
            .try_into()
            .map_err(|e| TlsError::Invalid(format!("name `{n}`: {e}")))?;
        params.subject_alt_names.push(SanType::DnsName(dns));
    }
    for ip in &cfg.ips {
        params.subject_alt_names.push(SanType::IpAddress(*ip));
    }
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Tether sync server");
    params.distinguished_name = dn;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(VALIDITY_DAYS);
    let cert = params
        .self_signed(key)
        .map_err(|e| TlsError::Invalid(e.to_string()))?;
    Ok(cert.pem())
}

#[cfg(unix)]
pub(crate) fn create_private_dir(dir: &Path) -> Result<(), TlsError> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| io_err(dir, e))
}

#[cfg(not(unix))]
pub(crate) fn create_private_dir(dir: &Path) -> Result<(), TlsError> {
    fs::create_dir_all(dir).map_err(|e| io_err(dir, e))
}

/// Write `bytes` to `path` with mode 0600, replacing any existing file
/// atomically (temp file in the same directory, then rename).
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), TlsError> {
    let tmp = path.with_extension("pem.tmp");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp).map_err(|e| io_err(&tmp, e))?;
    f.write_all(bytes).map_err(|e| io_err(&tmp, e))?;
    f.sync_all().map_err(|e| io_err(&tmp, e))?;
    drop(f);
    fs::rename(&tmp, path).map_err(|e| io_err(path, e))
}
