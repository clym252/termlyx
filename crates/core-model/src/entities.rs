//! Vault entities. Every type here derives serde so it can be sealed into an
//! encrypted envelope by a later crate; none of them is ever written to disk
//! or a log in plaintext.

use std::fmt;
use std::str::FromStr;

pub use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::Hlc;

/// Entity id (UUIDv7).
pub type Id = uuid::Uuid;

/// Serde helpers for [`SecretString`] fields.
///
/// `secrecy` deliberately provides no `Serialize` for secrets, so secret
/// fields opt in with `#[serde(serialize_with = "secret_serde::serialize")]`
/// (or `serialize_opt` for `Option<SecretString>`). `Deserialize` comes from
/// secrecy's `serde` feature.
///
/// **Warning:** these expose the secret in plaintext. Entity serialization
/// exists only to feed the bytes straight into an encrypted envelope (the
/// crypto crate) and the output must never be logged, printed or stored
/// unencrypted.
pub mod secret_serde {
    use secrecy::{ExposeSecret, SecretString};
    use serde::Serializer;

    /// Serialize a secret as a plain string.
    pub fn serialize<S: Serializer>(secret: &SecretString, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(secret.expose_secret())
    }

    /// Serialize an optional secret as a string or `null`.
    pub fn serialize_opt<S: Serializer>(
        secret: &Option<SecretString>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match secret {
            Some(secret) => s.serialize_some(secret.expose_secret()),
            None => s.serialize_none(),
        }
    }
}

/// Version of the set of record kinds this build understands: 1 is the
/// Phase 1 kinds, 2 adds [`Kind::Conflict`], 3 adds [`Kind::Snippet`]. Bump it
/// whenever a kind is added, so a device can tell that a peer may hold records
/// it cannot read.
pub const KIND_SET_VERSION: u32 = 3;

/// Fields of a record that this build does not know, kept verbatim so that
/// saving a record written by a newer version does not drop them.
///
/// Flattened into every entity (`#[serde(flatten, default)] extra`). `Debug`
/// prints only the key names: the values may be secrets.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Extra(pub serde_json::Map<String, serde_json::Value>);

impl fmt::Debug for Extra {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

impl Extra {
    /// Whether no unknown field is held.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The kind of a vault record; part of the encryption AAD, so the strings are stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Group,
    Host,
    Tag,
    Identity,
    SshKey,
    KnownHost,
    ForwardRule,
    Preference,
    /// The losing version of a record two devices edited at once; see [`Conflict`].
    Conflict,
    Snippet,
}

impl Kind {
    /// Stable snake_case name (same as the serde representation).
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Group => "group",
            Kind::Host => "host",
            Kind::Tag => "tag",
            Kind::Identity => "identity",
            Kind::SshKey => "ssh_key",
            Kind::KnownHost => "known_host",
            Kind::ForwardRule => "forward_rule",
            Kind::Preference => "preference",
            Kind::Conflict => "conflict",
            Kind::Snippet => "snippet",
        }
    }
}

/// The text is not a known [`Kind`] name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown record kind: {0:?}")]
pub struct ParseKindError(pub String);

impl FromStr for Kind {
    type Err = ParseKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "group" => Kind::Group,
            "host" => Kind::Host,
            "tag" => Kind::Tag,
            "identity" => Kind::Identity,
            "ssh_key" => Kind::SshKey,
            "known_host" => Kind::KnownHost,
            "forward_rule" => Kind::ForwardRule,
            "preference" => Kind::Preference,
            "conflict" => Kind::Conflict,
            "snippet" => Kind::Snippet,
            other => return Err(ParseKindError(other.to_owned())),
        })
    }
}

/// Connection settings that can be set at any level (app defaults, group,
/// host). `None` means "inherit from the level above".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnSettings {
    pub port: Option<u16>,
    pub identity_id: Option<Id>,
    pub keepalive_secs: Option<u32>,
    pub term_type: Option<String>,
    pub charset: Option<String>,
    pub color_scheme: Option<String>,
    pub font_family: Option<String>,
    pub font_size: Option<u16>,
    /// Environment variables to request, in order.
    pub env: Option<Vec<(String, String)>>,
}

/// A folder of hosts; groups nest through `parent_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: Id,
    pub parent_id: Option<Id>,
    pub name: String,
    pub settings: ConnSettings,
    /// Fields written by a newer version, preserved on save.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// A saved connection target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub id: Id,
    pub group_id: Option<Id>,
    pub label: String,
    pub address: String,
    pub tag_ids: Vec<Id>,
    pub settings: ConnSettings,
    pub forward_rule_ids: Vec<Id>,
    /// Fields written by a newer version, preserved on save.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// A coloured label attached to hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: Id,
    pub name: String,
    pub color: String,
    /// Fields written by a newer version, preserved on save.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// Login credentials: a username plus a password and/or a key reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub id: Id,
    pub label: String,
    pub username: String,
    #[serde(serialize_with = "secret_serde::serialize_opt")]
    pub password: Option<SecretString>,
    pub key_id: Option<Id>,
    /// The host or group whose own login this is (set in its editor, hidden
    /// from the Keychain), or `None` for a Keychain identity. Not a checked
    /// reference: the owner's `settings.identity_id` points back here, and
    /// `VaultService::tidy_own_logins` removes it once that link is gone.
    #[serde(default)]
    pub owner: Option<Id>,
    /// Fields written by a newer version, preserved on save.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// An SSH key pair stored in the vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshKey {
    pub id: Id,
    pub label: String,
    pub algorithm: String,
    #[serde(serialize_with = "secret_serde::serialize")]
    pub private_openssh: SecretString,
    #[serde(serialize_with = "secret_serde::serialize_opt")]
    pub passphrase: Option<SecretString>,
    pub public_openssh: String,
    pub certificate: Option<String>,
    /// Fields written by a newer version, preserved on save.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// Where a trusted host key came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownHostSource {
    /// The user accepted it in a first-connect prompt.
    Prompt,
    /// Imported from an OpenSSH `known_hosts` file.
    Import,
}

/// A trusted host public key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownHost {
    pub id: Id,
    pub host_pattern: String,
    pub port: u16,
    pub algorithm: String,
    pub public_key: String,
    /// Unix seconds when the key was first trusted.
    pub added_at: i64,
    pub source: KnownHostSource,
    /// Fields written by a newer version, preserved on save.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// Direction of a port forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardKind {
    /// `-L`: listen locally, connect from the remote side.
    Local,
    /// `-R`: listen on the remote side, connect locally.
    Remote,
    /// `-D`: local SOCKS proxy.
    Dynamic,
}

/// A saved port-forward belonging to one host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardRule {
    pub id: Id,
    pub host_id: Id,
    pub label: String,
    pub kind: ForwardKind,
    pub bind_addr: String,
    pub bind_port: u16,
    /// Destination for `Local`/`Remote`; unused for `Dynamic`.
    pub dest_host: Option<String>,
    pub dest_port: Option<u16>,
    pub autostart: bool,
    /// Fields written by a newer version, preserved on save.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// A saved command sent to a terminal from the snippets list.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub id: Id,
    pub label: String,
    /// What is pasted into the session; may span lines.
    pub text: String,
    /// Press Enter after pasting.
    #[serde(default)]
    pub run: bool,
    /// Fields written by a newer version, preserved on save.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// The text may hold a password typed into a command: shown by length only.
impl fmt::Debug for Snippet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snippet")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("text_len", &self.text.len())
            .field("run", &self.run)
            .finish()
    }
}

/// The losing version of a record that two devices changed at the same time.
///
/// Sync keeps the newer version and stores the other one here, still sealed
/// in its original envelope, until the user keeps or dismisses it, so a
/// conflict never loses a version (passwords included).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub id: Id,
    /// Id of the record that lost.
    pub record_id: Id,
    /// Kind of the record that lost.
    pub record_kind: Kind,
    /// Vault key version the losing envelope is sealed under.
    pub key_version: u32,
    /// Version stamp of the losing envelope.
    pub hlc: Hlc,
    /// Whether the losing version was a deletion.
    pub deleted: bool,
    /// The losing envelope, byte for byte (base64 in JSON).
    #[serde(with = "base64_bytes")]
    pub envelope: Vec<u8>,
    /// Unix seconds when the conflict was detected.
    pub detected_at: i64,
    /// Fields written by a newer version, preserved on save.
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// Serde helper: a byte vector as a standard base64 string.
mod base64_bytes {
    use data_encoding::BASE64;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        BASE64
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn kind_strings_round_trip_and_match_serde() {
        let all = [
            (Kind::Group, "group"),
            (Kind::Host, "host"),
            (Kind::Tag, "tag"),
            (Kind::Identity, "identity"),
            (Kind::SshKey, "ssh_key"),
            (Kind::KnownHost, "known_host"),
            (Kind::ForwardRule, "forward_rule"),
            (Kind::Preference, "preference"),
            (Kind::Conflict, "conflict"),
            (Kind::Snippet, "snippet"),
        ];
        for (kind, s) in all {
            assert_eq!(kind.as_str(), s);
            assert_eq!(s.parse::<Kind>().unwrap(), kind);
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{s}\""));
        }
        assert!("nope".parse::<Kind>().is_err());
    }

    #[test]
    fn secrets_round_trip_and_debug_is_redacted() {
        let ident = Identity {
            id: Id::now_v7(),
            label: "prod".into(),
            username: "root".into(),
            password: Some(SecretString::from("hunter2-canary")),
            key_id: None,
            owner: None,
            extra: Default::default(),
        };
        assert!(!format!("{ident:?}").contains("hunter2-canary"));
        let json = serde_json::to_string(&ident).unwrap();
        let back: Identity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.password.unwrap().expose_secret(), "hunter2-canary");
        assert_eq!(back.username, "root");

        let none = Identity {
            password: None,
            ..ident
        };
        let back: Identity = serde_json::from_str(&serde_json::to_string(&none).unwrap()).unwrap();
        assert!(back.password.is_none());
    }

    #[test]
    fn ssh_key_round_trips_with_unicode_label() {
        let key = SshKey {
            id: Id::now_v7(),
            label: "ключ 🔑 鍵".into(),
            algorithm: "ssh-ed25519".into(),
            private_openssh: SecretString::from("PRIVATE"),
            passphrase: Some(SecretString::from("pp")),
            public_openssh: "ssh-ed25519 AAAA".into(),
            certificate: None,
            extra: Default::default(),
        };
        let back: SshKey = serde_json::from_str(&serde_json::to_string(&key).unwrap()).unwrap();
        assert_eq!(back.label, key.label);
        assert_eq!(back.private_openssh.expose_secret(), "PRIVATE");
        assert_eq!(back.passphrase.unwrap().expose_secret(), "pp");
    }

    fn sample_host() -> Host {
        Host {
            id: Id::now_v7(),
            group_id: None,
            label: "h".into(),
            address: "example.org".into(),
            tag_ids: vec![],
            settings: ConnSettings::default(),
            forward_rule_ids: vec![],
            extra: Extra::default(),
        }
    }

    #[test]
    fn unknown_fields_round_trip() {
        let mut value = serde_json::to_value(sample_host()).unwrap();
        value["future_field"] = serde_json::json!({"a": 1});
        let host: Host = serde_json::from_value(value).unwrap();
        assert_eq!(host.extra.0["future_field"], serde_json::json!({"a": 1}));
        let again = serde_json::to_value(&host).unwrap();
        assert_eq!(again["future_field"], serde_json::json!({"a": 1}));
        // Known fields are not duplicated into `extra`.
        assert!(!host.extra.0.contains_key("label"));
    }

    #[test]
    fn extra_debug_hides_values() {
        let mut extra = Extra::default();
        extra
            .0
            .insert("future_field".into(), "canary-secret".into());
        let text = format!("{extra:?}");
        assert!(text.contains("future_field"));
        assert!(!text.contains("canary-secret"));
    }

    #[test]
    fn conflict_round_trips_with_base64_envelope() {
        let conflict = Conflict {
            id: Id::now_v7(),
            record_id: Id::now_v7(),
            record_kind: Kind::Host,
            key_version: 1,
            hlc: Hlc {
                wall_ms: 7,
                counter: 1,
                node: 2,
            },
            deleted: false,
            envelope: vec![0, 1, 2, 250, 251, 255],
            detected_at: 1_700_000_000,
            extra: Extra::default(),
        };
        let json = serde_json::to_value(&conflict).unwrap();
        assert_eq!(json["envelope"], "AAEC+vv/");
        assert_eq!(json["record_kind"], "host");
        assert_eq!(json["hlc"], "0000000000000007-00000001-0002");
        let back: Conflict = serde_json::from_value(json).unwrap();
        assert_eq!(back, conflict);
    }

    #[test]
    fn kind_set_version_covers_snippet() {
        assert_eq!(KIND_SET_VERSION, 3);
    }

    #[test]
    fn snippet_round_trips_and_debug_hides_text() {
        let snippet = Snippet {
            id: Id::now_v7(),
            label: "Upgrade".into(),
            text: "echo hunter2-canary".into(),
            run: true,
            extra: Extra::default(),
        };
        let debug = format!("{snippet:?}");
        assert!(debug.contains("Upgrade"), "{debug}");
        assert!(!debug.contains("hunter2-canary"), "{debug}");
        let json = serde_json::to_value(&snippet).unwrap();
        assert_eq!(serde_json::from_value::<Snippet>(json).unwrap(), snippet);
        // `run` defaults to false for a record written without it.
        let old: Snippet = serde_json::from_value(serde_json::json!({
            "id": snippet.id, "label": "l", "text": "t"
        }))
        .unwrap();
        assert!(!old.run);
    }
}
