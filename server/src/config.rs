//! Server configuration: environment variables, with an optional
//! `{data_dir}/server.toml` supplying values the environment does not set.
//!
//! | Variable | Meaning | Default |
//! | --- | --- | --- |
//! | `TERMLYX_DATA` | data directory (database, TLS material) | `$XDG_DATA_HOME/termlyx-server`, else `$HOME/.local/share/termlyx-server`, else `/data` |
//! | `TERMLYX_BIND` | listen address | `0.0.0.0:8443` |
//! | `TERMLYX_TLS` | `self-signed`, `files` or `off` | `self-signed` |
//! | `TERMLYX_TLS_CERT`, `TERMLYX_TLS_KEY` | PEM paths for `files` | |
//! | `TERMLYX_NAMES` | comma list of DNS names for the certificate | `termlyx`, `localhost` |
//! | `TERMLYX_IPS` | comma list of IPs for the certificate | none |
//! | `TERMLYX_URL` | public URL put into full codes | `https://{first name}` if names are set, else none |
//! | `TERMLYX_ALLOW_PLAIN` | allow `off` on a non-loopback address | `false` |
//! | `TERMLYX_BEHIND_PROXY` | `self-signed` behind a TLS proxy: accept short codes redeemed without the pin | `false` |
//!
//! Every variable is also read as `TETHER_X` when `TERMLYX_X` is unset.
//!
//! `server.toml` uses the lowercase keys `bind`, `tls`, `tls_cert`, `tls_key`,
//! `names`, `ips`, `url`, `allow_plain_remote`, `behind_proxy`. The data directory itself can
//! only come from the environment.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// How the server terminates TLS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsMode {
    /// Generate and persist a long-lived self-signed certificate; clients pin
    /// its public key.
    SelfSigned,
    /// Use a certificate chain and private key supplied by the operator.
    Files {
        /// PEM certificate chain, leaf first.
        cert: PathBuf,
        /// PEM private key.
        key: PathBuf,
    },
    /// Plain HTTP, for running behind the operator's own reverse proxy.
    Off,
}

/// A configuration problem, reported before anything is started.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A value could not be parsed.
    #[error("invalid {name}: {reason}")]
    Invalid {
        /// Variable or key name.
        name: &'static str,
        /// What is wrong with it.
        reason: String,
    },
    /// `server.toml` exists but cannot be read.
    #[error("cannot read {path}: {reason}")]
    File {
        /// The config file.
        path: PathBuf,
        /// The underlying error.
        reason: String,
    },
    /// Plain HTTP on a non-loopback address without explicit permission.
    #[error(
        "TLS is off but {0} is not a loopback address; bind 127.0.0.1 behind a reverse proxy, \
         or set TERMLYX_ALLOW_PLAIN=1 to accept the risk"
    )]
    PlainRemote(SocketAddr),
}

/// Server settings. Build with [`Config::from_env`], or [`Config::new`] in
/// tests.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding the database and TLS material.
    pub data_dir: PathBuf,
    /// Address to listen on.
    pub bind: SocketAddr,
    /// TLS mode.
    pub tls: TlsMode,
    /// DNS names for the certificate's subject alternative names.
    pub names: Vec<String>,
    /// IP addresses for the certificate's subject alternative names.
    pub ips: Vec<IpAddr>,
    /// URL clients reach the server at; `Some` only when configured.
    pub public_url: Option<String>,
    /// Allow `TlsMode::Off` on a non-loopback address.
    pub allow_plain_remote: bool,
    /// A TLS proxy fronts a `SelfSigned` server, so clients see the proxy's
    /// certificate and redeem short codes unpinned. Without it such proofs
    /// are refused: clients reaching this server directly always pin.
    pub behind_proxy: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    bind: Option<String>,
    tls: Option<String>,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    names: Option<Vec<String>>,
    ips: Option<Vec<String>>,
    url: Option<String>,
    allow_plain_remote: Option<bool>,
    behind_proxy: Option<bool>,
}

fn invalid(name: &'static str, reason: impl ToString) -> ConfigError {
    ConfigError::Invalid {
        name,
        reason: reason.to_string(),
    }
}

fn parse_bool(name: &'static str, v: &str) -> Result<bool, ConfigError> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" | "" => Ok(false),
        other => Err(invalid(name, format!("`{other}` is not a boolean"))),
    }
}

fn split_list(v: &str) -> Vec<String> {
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

impl Config {
    /// Defaults with the given data directory: self-signed TLS on
    /// `0.0.0.0:8443`, names `termlyx` and `localhost`.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            bind: SocketAddr::from(([0, 0, 0, 0], 8443)),
            tls: TlsMode::SelfSigned,
            names: vec!["termlyx".into(), "localhost".into()],
            ips: vec![],
            public_url: None,
            allow_plain_remote: false,
            behind_proxy: false,
        }
    }

    /// Read the process environment, then `{data_dir}/server.toml`.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Like [`Config::from_env`] with an injectable variable lookup.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        // `TERMLYX_X`, falling back to `TETHER_X`.
        let get = |k: &str| {
            get(k).or_else(|| {
                k.strip_prefix("TERMLYX_")
                    .and_then(|r| get(&format!("TETHER_{r}")))
            })
        };
        let data_dir = get("TERMLYX_DATA").map(PathBuf::from).unwrap_or_else(|| {
            let non_empty = |k| get(k).filter(|v: &String| !v.is_empty());
            if let Some(x) = non_empty("XDG_DATA_HOME") {
                PathBuf::from(x).join("termlyx-server")
            } else if let Some(h) = non_empty("HOME") {
                PathBuf::from(h).join(".local/share/termlyx-server")
            } else {
                "/data".into()
            }
        });
        let file = Self::read_file(&data_dir)?;

        // Environment first, then the file, then the default.
        let bind = get("TERMLYX_BIND")
            .or(file.bind)
            .unwrap_or_else(|| "0.0.0.0:8443".into());
        let bind: SocketAddr = bind.parse().map_err(|e| invalid("TERMLYX_BIND", e))?;

        let names = match get("TERMLYX_NAMES") {
            Some(v) => split_list(&v),
            None => file.names.unwrap_or_default(),
        };
        let explicit_names = !names.is_empty();
        let names = if names.is_empty() {
            vec!["termlyx".to_string(), "localhost".to_string()]
        } else {
            names
        };

        let ips = match get("TERMLYX_IPS") {
            Some(v) => split_list(&v),
            None => file.ips.unwrap_or_default(),
        };
        let ips = ips
            .iter()
            .map(|s| {
                s.parse::<IpAddr>()
                    .map_err(|e| invalid("TERMLYX_IPS", format!("{s}: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let cert = get("TERMLYX_TLS_CERT").map(PathBuf::from).or(file.tls_cert);
        let key = get("TERMLYX_TLS_KEY").map(PathBuf::from).or(file.tls_key);
        let tls = match get("TERMLYX_TLS")
            .or(file.tls)
            .unwrap_or_else(|| "self-signed".into())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "self-signed" | "self_signed" | "selfsigned" => TlsMode::SelfSigned,
            "off" => TlsMode::Off,
            "files" => match (cert, key) {
                (Some(cert), Some(key)) => TlsMode::Files { cert, key },
                _ => {
                    return Err(invalid(
                        "TERMLYX_TLS",
                        "`files` needs TERMLYX_TLS_CERT and TERMLYX_TLS_KEY",
                    ));
                }
            },
            other => {
                return Err(invalid(
                    "TERMLYX_TLS",
                    format!("`{other}` is not one of self-signed, files, off"),
                ));
            }
        };

        let allow_plain_remote = match get("TERMLYX_ALLOW_PLAIN") {
            Some(v) => parse_bool("TERMLYX_ALLOW_PLAIN", &v)?,
            None => file.allow_plain_remote.unwrap_or(false),
        };

        let behind_proxy = match get("TERMLYX_BEHIND_PROXY") {
            Some(v) => parse_bool("TERMLYX_BEHIND_PROXY", &v)?,
            None => file.behind_proxy.unwrap_or(false),
        };

        // Explicit names keep the legacy `https://{first name}` default so
        // existing deployments keep making the same codes.
        let public_url = get("TERMLYX_URL")
            .or(file.url)
            .or_else(|| explicit_names.then(|| format!("https://{}", names[0])));

        let cfg = Self {
            data_dir,
            bind,
            tls,
            names,
            ips,
            public_url,
            allow_plain_remote,
            behind_proxy,
        };
        cfg.check()?;
        Ok(cfg)
    }

    fn read_file(data_dir: &Path) -> Result<FileConfig, ConfigError> {
        let path = data_dir.join("server.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| ConfigError::File {
                path,
                reason: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileConfig::default()),
            Err(e) => Err(ConfigError::File {
                path,
                reason: e.to_string(),
            }),
        }
    }

    /// Reject unsafe combinations: with TLS `off`, the bind address must be
    /// loopback unless `allow_plain_remote` is set.
    pub fn check(&self) -> Result<(), ConfigError> {
        if self.tls == TlsMode::Off && !self.allow_plain_remote && !self.bind.ip().is_loopback() {
            return Err(ConfigError::PlainRemote(self.bind));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c =
            Config::from_lookup(lookup(&[("TERMLYX_DATA", dir.path().to_str().unwrap())])).unwrap();
        assert_eq!(c.bind, "0.0.0.0:8443".parse().unwrap());
        assert_eq!(c.tls, TlsMode::SelfSigned);
        assert_eq!(c.names, ["termlyx", "localhost"]);
        assert_eq!(c.public_url, None);
        assert!(!c.allow_plain_remote);
        assert!(!c.behind_proxy);
        let c = Config::from_lookup(lookup(&[
            ("TERMLYX_DATA", dir.path().to_str().unwrap()),
            ("TERMLYX_BEHIND_PROXY", "1"),
        ]))
        .unwrap();
        assert!(c.behind_proxy);
        assert_eq!(
            Config::from_lookup(lookup(&[("HOME", "/h")]))
                .unwrap()
                .data_dir,
            PathBuf::from("/h/.local/share/termlyx-server")
        );
        assert_eq!(
            Config::from_lookup(lookup(&[("HOME", "/h"), ("XDG_DATA_HOME", "/x")]))
                .unwrap()
                .data_dir,
            PathBuf::from("/x/termlyx-server")
        );
        assert_eq!(
            Config::from_lookup(|_| None).unwrap().data_dir,
            PathBuf::from("/data")
        );
    }

    #[test]
    fn explicit_names_keep_legacy_url_default() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::from_lookup(lookup(&[
            ("TETHER_DATA", dir.path().to_str().unwrap()),
            ("TETHER_NAMES", "tether.lan"),
        ]))
        .unwrap();
        assert_eq!(c.public_url.as_deref(), Some("https://tether.lan"));
    }

    #[test]
    fn empty_names_mean_no_public_url() {
        // compose.yml passes `TETHER_NAMES: ${TETHER_NAMES:-}`, i.e. empty when unset.
        let dir = tempfile::tempdir().unwrap();
        let c = Config::from_lookup(lookup(&[
            ("TETHER_DATA", dir.path().to_str().unwrap()),
            ("TETHER_NAMES", ""),
        ]))
        .unwrap();
        assert_eq!(c.public_url, None);
        assert_eq!(c.names, ["termlyx", "localhost"]);
    }

    #[test]
    fn termlyx_names_win_over_tether() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_str().unwrap();
        let c = Config::from_lookup(lookup(&[
            ("TERMLYX_DATA", d),
            ("TETHER_DATA", "/nowhere"),
            ("TERMLYX_NAMES", "new.example"),
            ("TETHER_NAMES", "old.example"),
        ]))
        .unwrap();
        assert_eq!(c.names, ["new.example"]);
        assert_eq!(c.data_dir, dir.path());
    }

    #[test]
    fn tether_names_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::from_lookup(lookup(&[
            ("TETHER_DATA", dir.path().to_str().unwrap()),
            ("TETHER_NAMES", "old.example"),
            ("TETHER_URL", "https://old.example"),
            ("TETHER_TLS", "files"),
            ("TETHER_TLS_CERT", "/c.pem"),
            ("TETHER_TLS_KEY", "/k.pem"),
        ]))
        .unwrap();
        assert_eq!(c.names, ["old.example"]);
        assert_eq!(c.public_url.as_deref(), Some("https://old.example"));
        assert_eq!(
            c.tls,
            TlsMode::Files {
                cert: "/c.pem".into(),
                key: "/k.pem".into()
            }
        );
    }

    #[test]
    fn env_values_and_lists() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::from_lookup(lookup(&[
            ("TETHER_DATA", dir.path().to_str().unwrap()),
            ("TETHER_BIND", "127.0.0.1:9000"),
            ("TETHER_NAMES", "sync.example.com, tether.lan,"),
            ("TETHER_IPS", "192.168.1.8"),
            ("TETHER_TLS", "off"),
        ]))
        .unwrap();
        assert_eq!(c.names, ["sync.example.com", "tether.lan"]);
        assert_eq!(c.ips, ["192.168.1.8".parse::<IpAddr>().unwrap()]);
        assert_eq!(c.tls, TlsMode::Off);
        assert_eq!(
            c.public_url.as_deref(),
            Some("https://sync.example.com")
        );
    }

    #[test]
    fn file_fills_gaps_but_env_wins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("server.toml"),
            "bind = \"127.0.0.1:1111\"\nnames = [\"from-file\"]\nurl = \"https://file.example\"\n",
        )
        .unwrap();
        let c = Config::from_lookup(lookup(&[
            ("TETHER_DATA", dir.path().to_str().unwrap()),
            ("TETHER_BIND", "127.0.0.1:2222"),
        ]))
        .unwrap();
        assert_eq!(c.bind.port(), 2222, "env beats file");
        assert_eq!(c.names, ["from-file"], "file beats default");
        assert_eq!(c.public_url.as_deref(), Some("https://file.example"));
    }

    #[test]
    fn rejects_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_str().unwrap();
        for bad in [
            vec![("TETHER_DATA", d), ("TETHER_TLS", "plaintext")],
            vec![("TETHER_DATA", d), ("TETHER_TLS", "files")],
            vec![("TETHER_DATA", d), ("TETHER_BIND", "nonsense")],
            vec![("TETHER_DATA", d), ("TETHER_IPS", "not-an-ip")],
            vec![("TETHER_DATA", d), ("TETHER_ALLOW_PLAIN", "maybe")],
            // TLS off on all interfaces without permission.
            vec![("TETHER_DATA", d), ("TETHER_TLS", "off")],
        ] {
            assert!(Config::from_lookup(lookup(&bad)).is_err(), "{bad:?}");
        }
        std::fs::write(dir.path().join("server.toml"), "bogus = 1").unwrap();
        assert!(Config::from_lookup(lookup(&[("TETHER_DATA", d)])).is_err());
    }

    #[test]
    fn off_on_loopback_or_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_str().unwrap();
        assert!(
            Config::from_lookup(lookup(&[
                ("TETHER_DATA", d),
                ("TETHER_TLS", "off"),
                ("TETHER_BIND", "127.0.0.1:8080")
            ]))
            .is_ok()
        );
        assert!(
            Config::from_lookup(lookup(&[
                ("TETHER_DATA", d),
                ("TETHER_TLS", "off"),
                ("TETHER_ALLOW_PLAIN", "1")
            ]))
            .is_ok()
        );
    }
}
