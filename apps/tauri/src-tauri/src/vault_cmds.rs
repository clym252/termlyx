//! Vault commands: lifecycle (create, unlock, recovery, lock, quick unlock),
//! entity CRUD, effective settings, ssh_config and known_hosts import, and
//! the keychain (generate, import, export).
//!
//! DTOs are camelCase and separate from the `core_model` entities (which are
//! snake_case and carry secrets). Output DTOs never contain a secret: an
//! identity says `hasPassword`, a key `hasPassphrase`. The only secrets that
//! leave are the one-time recovery key ([`VaultCreated`], and the new one
//! `sync_enable` can make), an explicit key export ([`KeyExport`]), and a
//! revealed conflict field (`conflicts_reveal`, see `sync_cmds`). Secrets in
//! inputs deserialize into `SecretString`.
//!
//! Every command that changes records goes through
//! [`AppState::with_vault_write`], which nudges the sync engine to push.
//!
//! Entity kinds on the wire are the vault's record kinds: `group`, `host`,
//! `tag`, `identity`, `ssh_key`, `known_host`, `forward_rule`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use core_api::import::plan_ssh_config_import;
use core_api::vault::{ChildPolicy, GateKind, PasswordChange, VaultService};
use core_model::{
    ConnSettings, EffectiveSettings, ForwardKind, ForwardRule, Group, Host, Id, Identity, Kind,
    KnownHost, KnownHostSource, Resolved, Snippet, Source, SshKey, Tag,
};
use core_ssh::keys::{self, KeyAlgo, KeyError};
use core_ssh::{AuthMethod, ConnectParams, HostKeyInfo, parse_known_hosts};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use tauri::State;

use crate::state::{AppState, CONNECT_TIMEOUT, CmdError, DEFAULT_TERM, VaultSlot, events, lock};

// ---------------------------------------------------------------------------
// DTOs

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VaultState {
    /// No vault has been created yet.
    Uninitialized,
    Locked,
    Unlocked,
    /// The vault file could not be opened at startup (see `error`).
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatus {
    pub state: VaultState,
    pub quick_unlock_enabled: bool,
    /// What quick unlock asks for on this device (`touchId` on macOS).
    pub quick_unlock_kind: GateKind,
    pub error: Option<String>,
}

/// Why the vault locked, in the `vault-locked` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LockReason {
    /// The user locked it (or the app did, for them).
    Manual,
    /// Another device changed the master password; unlock with the new one.
    PasswordChanged,
    /// Auto-lock: Tether was away from the front for the chosen time.
    Idle,
}

/// Payload of the `vault-locked` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultLocked {
    pub reason: LockReason,
}

/// Result of `vault_create`: the recovery key, shown to the user once and
/// stored nowhere. No `Debug`, so it cannot end up in a log line.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultCreated {
    pub recovery_key: String,
}

/// [`ConnSettings`] on the wire. `null` (or a missing field) inherits.
///
/// Applied to connections: `port`, `identityId`, `keepaliveSecs` (0 turns
/// keepalives off), `termType` and `env` (requested before each shell; the
/// server may refuse names its `AcceptEnv` does not allow). `charset` is
/// informational: only UTF-8 is applied. `colorScheme`, `fontFamily` and
/// `fontSize` are for the terminal view.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct ConnSettingsDto {
    pub port: Option<u16>,
    pub identity_id: Option<Id>,
    pub keepalive_secs: Option<u32>,
    pub term_type: Option<String>,
    pub charset: Option<String>,
    pub color_scheme: Option<String>,
    pub font_family: Option<String>,
    pub font_size: Option<u16>,
    pub env: Option<Vec<(String, String)>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupDto {
    pub id: Id,
    #[serde(default)]
    pub parent_id: Option<Id>,
    pub name: String,
    #[serde(default)]
    pub settings: ConnSettingsDto,
}

/// A host. On upsert `forwardRuleIds` may be omitted: the vault derives it
/// from the rules (only their order is taken from the input).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostDto {
    pub id: Id,
    #[serde(default)]
    pub group_id: Option<Id>,
    pub label: String,
    pub address: String,
    #[serde(default)]
    pub tag_ids: Vec<Id>,
    #[serde(default)]
    pub settings: ConnSettingsDto,
    #[serde(default)]
    pub forward_rule_ids: Vec<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TagDto {
    pub id: Id,
    pub name: String,
    pub color: String,
}

/// A snippet; `run` defaults to false on input.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnippetDto {
    pub id: Id,
    pub label: String,
    pub text: String,
    #[serde(default)]
    pub run: bool,
}

/// The command may hold a password: shown by length only, like `Snippet`.
impl std::fmt::Debug for SnippetDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnippetDto")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("text_len", &self.text.len())
            .field("run", &self.run)
            .finish()
    }
}

/// An identity as shown: whether a password is stored, never the password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityDto {
    pub id: Id,
    pub label: String,
    pub username: String,
    pub has_password: bool,
    pub key_id: Option<Id>,
    /// The host or group whose own login this is; `None` for a Keychain
    /// identity.
    pub owner: Option<Id>,
}

/// Setting a host's or group's own login (`set_own_login`). `password`:
/// omitted keeps the stored one, `null` clears it, a string sets it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnLoginInput {
    pub owner_id: Id,
    pub username: String,
    #[serde(default, deserialize_with = "present")]
    pub password: Option<Option<SecretString>>,
}

impl std::fmt::Debug for OwnLoginInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let password = match &self.password {
            None => "keep",
            Some(None) => "clear",
            Some(Some(_)) => "[REDACTED]",
        };
        f.debug_struct("OwnLoginInput")
            .field("owner_id", &self.owner_id)
            .field("username", &self.username)
            .field("password", &password)
            .finish()
    }
}

/// Creating or updating an identity. `password`: omitted keeps the stored
/// one (none for a new identity), `null` clears it, a string sets it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityInput {
    #[serde(default)]
    pub id: Option<Id>,
    pub label: String,
    pub username: String,
    #[serde(default, deserialize_with = "present")]
    pub password: Option<Option<SecretString>>,
    #[serde(default)]
    pub key_id: Option<Id>,
}

impl std::fmt::Debug for IdentityInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let password = match &self.password {
            None => "keep",
            Some(None) => "clear",
            Some(Some(_)) => "set",
        };
        f.debug_struct("IdentityInput")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("username", &self.username)
            .field("password", &password)
            .field("key_id", &self.key_id)
            .finish()
    }
}

/// A field that is present (possibly `null`), as opposed to omitted.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<SecretString>>, D::Error> {
    Option::<SecretString>::deserialize(d).map(Some)
}

/// A key as shown: public parts only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshKeyDto {
    pub id: Id,
    pub label: String,
    pub algorithm: String,
    pub public_openssh: String,
    /// `SHA256:...` of the public key; empty if it does not parse.
    pub fingerprint: String,
    /// The vault stores the key's passphrase (used when connecting).
    pub has_passphrase: bool,
    pub certificate: Option<String>,
}

/// Editing a stored key: only the label. Keys are created with
/// `key_generate` / `key_import`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SshKeyInput {
    pub id: Id,
    pub label: String,
}

/// A trusted host key. On upsert `id`, `addedAt` (now) and `source`
/// (`"prompt"`, a user decision) may be omitted; `fingerprint` is output
/// only (accepted on input and ignored, so a listed record can be sent
/// back).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnownHostDto {
    pub id: Id,
    pub host_pattern: String,
    pub port: u16,
    pub algorithm: String,
    pub public_key: String,
    #[serde(default)]
    pub added_at: i64,
    /// `"prompt"` or `"import"`.
    #[serde(default = "prompt_source")]
    pub source: String,
    #[serde(default)]
    pub fingerprint: String,
}

fn prompt_source() -> String {
    "prompt".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardRuleDto {
    pub id: Id,
    pub host_id: Id,
    pub label: String,
    /// `"local"`, `"remote"` or `"dynamic"`.
    pub kind: String,
    pub bind_addr: String,
    pub bind_port: u16,
    #[serde(default)]
    pub dest_host: Option<String>,
    #[serde(default)]
    pub dest_port: Option<u16>,
    #[serde(default)]
    pub autostart: bool,
}

/// Which level supplied an effective value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SourceDto {
    Default,
    Group { group_id: Id },
    Host,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedDto<T> {
    pub value: T,
    pub source: SourceDto,
}

/// Effective settings of a host, each value with its source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveSettingsDto {
    pub port: ResolvedDto<u16>,
    pub identity_id: ResolvedDto<Option<Id>>,
    pub keepalive_secs: ResolvedDto<Option<u32>>,
    pub term_type: ResolvedDto<Option<String>>,
    pub charset: ResolvedDto<Option<String>>,
    pub color_scheme: ResolvedDto<Option<String>>,
    pub font_family: ResolvedDto<Option<String>>,
    pub font_size: ResolvedDto<Option<u16>>,
    pub env: ResolvedDto<Option<Vec<(String, String)>>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReportDto {
    pub hosts_added: usize,
    pub identities_added: usize,
    pub keys_added: usize,
    pub key_failures: Vec<KeyFailureDto>,
    /// Hosts and directives that were not imported, with the reason.
    pub skipped: Vec<SkippedEntryDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyFailureDto {
    pub path: String,
    pub needs_passphrase: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedEntryDto {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownHostsImportReport {
    pub added: usize,
    /// Entries already stored (same pattern, port and key), not added again.
    pub already_known: usize,
}

/// The private key, OpenSSH format (encrypted iff it has a passphrase): a
/// deliberate secret egress, like the recovery key and `conflicts_reveal`
/// (the other two). No `Debug`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyExport {
    pub private_openssh: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyAlgoDto {
    Ed25519,
    EcdsaP256,
    EcdsaP384,
    EcdsaP521,
    Rsa2048,
    Rsa4096,
}

impl From<KeyAlgoDto> for KeyAlgo {
    fn from(a: KeyAlgoDto) -> Self {
        match a {
            KeyAlgoDto::Ed25519 => KeyAlgo::Ed25519,
            KeyAlgoDto::EcdsaP256 => KeyAlgo::EcdsaP256,
            KeyAlgoDto::EcdsaP384 => KeyAlgo::EcdsaP384,
            KeyAlgoDto::EcdsaP521 => KeyAlgo::EcdsaP521,
            KeyAlgoDto::Rsa2048 => KeyAlgo::Rsa2048,
            KeyAlgoDto::Rsa4096 => KeyAlgo::Rsa4096,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChildPolicyDto {
    MoveToParent,
    DeleteRecursive,
}

impl From<ChildPolicyDto> for ChildPolicy {
    fn from(p: ChildPolicyDto) -> Self {
        match p {
            ChildPolicyDto::MoveToParent => ChildPolicy::MoveToParent,
            ChildPolicyDto::DeleteRecursive => ChildPolicy::DeleteRecursive,
        }
    }
}

// ---------------------------------------------------------------------------
// Conversions

impl From<ConnSettings> for ConnSettingsDto {
    fn from(s: ConnSettings) -> Self {
        Self {
            port: s.port,
            identity_id: s.identity_id,
            keepalive_secs: s.keepalive_secs,
            term_type: s.term_type,
            charset: s.charset,
            color_scheme: s.color_scheme,
            font_family: s.font_family,
            font_size: s.font_size,
            env: s.env,
        }
    }
}

impl From<ConnSettingsDto> for ConnSettings {
    fn from(s: ConnSettingsDto) -> Self {
        Self {
            port: s.port,
            identity_id: s.identity_id,
            keepalive_secs: s.keepalive_secs,
            term_type: s.term_type,
            charset: s.charset,
            color_scheme: s.color_scheme,
            font_family: s.font_family,
            font_size: s.font_size,
            env: s.env,
        }
    }
}

impl From<Group> for GroupDto {
    fn from(g: Group) -> Self {
        Self {
            id: g.id,
            parent_id: g.parent_id,
            name: g.name,
            settings: g.settings.into(),
        }
    }
}

impl From<GroupDto> for Group {
    fn from(g: GroupDto) -> Self {
        Self {
            id: g.id,
            parent_id: g.parent_id,
            name: g.name,
            settings: g.settings.into(),
            extra: Default::default(),
        }
    }
}

impl From<Host> for HostDto {
    fn from(h: Host) -> Self {
        Self {
            id: h.id,
            group_id: h.group_id,
            label: h.label,
            address: h.address,
            tag_ids: h.tag_ids,
            settings: h.settings.into(),
            forward_rule_ids: h.forward_rule_ids,
        }
    }
}

impl From<HostDto> for Host {
    fn from(h: HostDto) -> Self {
        Self {
            id: h.id,
            group_id: h.group_id,
            label: h.label,
            address: h.address,
            tag_ids: h.tag_ids,
            settings: h.settings.into(),
            forward_rule_ids: h.forward_rule_ids,
            extra: Default::default(),
        }
    }
}

impl From<Tag> for TagDto {
    fn from(t: Tag) -> Self {
        Self {
            id: t.id,
            name: t.name,
            color: t.color,
        }
    }
}

impl From<TagDto> for Tag {
    fn from(t: TagDto) -> Self {
        Self {
            id: t.id,
            name: t.name,
            color: t.color,
            extra: Default::default(),
        }
    }
}

impl From<Snippet> for SnippetDto {
    fn from(s: Snippet) -> Self {
        Self {
            id: s.id,
            label: s.label,
            text: s.text,
            run: s.run,
        }
    }
}

impl From<SnippetDto> for Snippet {
    fn from(s: SnippetDto) -> Self {
        Self {
            id: s.id,
            label: s.label.trim().to_owned(),
            text: s.text,
            run: s.run,
            extra: Default::default(),
        }
    }
}

impl From<&Identity> for IdentityDto {
    fn from(i: &Identity) -> Self {
        Self {
            id: i.id,
            label: i.label.clone(),
            username: i.username.clone(),
            has_password: i.password.is_some(),
            key_id: i.key_id,
            owner: i.owner,
        }
    }
}

impl From<&SshKey> for SshKeyDto {
    fn from(k: &SshKey) -> Self {
        Self {
            id: k.id,
            label: k.label.clone(),
            algorithm: k.algorithm.clone(),
            public_openssh: k.public_openssh.clone(),
            fingerprint: fingerprint(&k.public_openssh),
            has_passphrase: k.passphrase.is_some(),
            certificate: k.certificate.clone(),
        }
    }
}

fn fingerprint(public_line: &str) -> String {
    HostKeyInfo::from_openssh(public_line)
        .map(|i| i.sha256_fingerprint)
        .unwrap_or_default()
}

fn source_str(s: KnownHostSource) -> &'static str {
    match s {
        KnownHostSource::Prompt => "prompt",
        KnownHostSource::Import => "import",
    }
}

impl From<KnownHost> for KnownHostDto {
    fn from(k: KnownHost) -> Self {
        let fingerprint = fingerprint(&format!("{} {}", k.algorithm, k.public_key));
        Self {
            id: k.id,
            host_pattern: k.host_pattern,
            port: k.port,
            algorithm: k.algorithm,
            public_key: k.public_key,
            added_at: k.added_at,
            source: source_str(k.source).to_owned(),
            fingerprint,
        }
    }
}

impl TryFrom<KnownHostDto> for KnownHost {
    type Error = CmdError;

    fn try_from(k: KnownHostDto) -> Result<Self, CmdError> {
        let source = match k.source.as_str() {
            "prompt" => KnownHostSource::Prompt,
            "import" => KnownHostSource::Import,
            other => return Err(CmdError::invalid(format!("unknown source {other:?}"))),
        };
        Ok(Self {
            id: k.id,
            host_pattern: k.host_pattern,
            port: k.port,
            algorithm: k.algorithm,
            public_key: k.public_key,
            added_at: if k.added_at == 0 {
                unix_now()
            } else {
                k.added_at
            },
            source,
            extra: Default::default(),
        })
    }
}

fn forward_kind_str(k: ForwardKind) -> &'static str {
    match k {
        ForwardKind::Local => "local",
        ForwardKind::Remote => "remote",
        ForwardKind::Dynamic => "dynamic",
    }
}

impl From<ForwardRule> for ForwardRuleDto {
    fn from(r: ForwardRule) -> Self {
        Self {
            id: r.id,
            host_id: r.host_id,
            label: r.label,
            kind: forward_kind_str(r.kind).to_owned(),
            bind_addr: r.bind_addr,
            bind_port: r.bind_port,
            dest_host: r.dest_host,
            dest_port: r.dest_port,
            autostart: r.autostart,
        }
    }
}

impl TryFrom<ForwardRuleDto> for ForwardRule {
    type Error = CmdError;

    fn try_from(r: ForwardRuleDto) -> Result<Self, CmdError> {
        let kind = match r.kind.as_str() {
            "local" => ForwardKind::Local,
            "remote" => ForwardKind::Remote,
            "dynamic" => ForwardKind::Dynamic,
            other => return Err(CmdError::invalid(format!("unknown forward kind {other:?}"))),
        };
        Ok(Self {
            id: r.id,
            host_id: r.host_id,
            label: r.label,
            kind,
            bind_addr: r.bind_addr,
            bind_port: r.bind_port,
            dest_host: r.dest_host,
            dest_port: r.dest_port,
            autostart: r.autostart,
            extra: Default::default(),
        })
    }
}

fn source_dto(s: Source) -> SourceDto {
    match s {
        Source::Default => SourceDto::Default,
        Source::Group(group_id) => SourceDto::Group { group_id },
        Source::Host => SourceDto::Host,
    }
}

fn resolved<T>(r: Resolved<T>) -> ResolvedDto<T> {
    ResolvedDto {
        value: r.value,
        source: source_dto(r.source),
    }
}

impl From<EffectiveSettings> for EffectiveSettingsDto {
    fn from(e: EffectiveSettings) -> Self {
        Self {
            port: resolved(e.port),
            identity_id: resolved(e.identity_id),
            keepalive_secs: resolved(e.keepalive_secs),
            term_type: resolved(e.term_type),
            charset: resolved(e.charset),
            color_scheme: resolved(e.color_scheme),
            font_family: resolved(e.font_family),
            font_size: resolved(e.font_size),
            env: resolved(e.env),
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// An entity kind the UI may name.
fn entity_kind(kind: &str) -> Result<Kind, CmdError> {
    match Kind::from_str(kind) {
        Ok(Kind::Preference | Kind::Conflict) | Err(_) => {
            Err(CmdError::invalid(format!("unknown entity kind {kind:?}")))
        }
        Ok(k) => Ok(k),
    }
}

fn to_json<T: Serialize>(v: T) -> Result<Value, CmdError> {
    serde_json::to_value(v).map_err(|e| CmdError::internal(e.to_string()))
}

/// Decodes an upsert input, giving it a fresh id when it has none.
fn input<T: serde::de::DeserializeOwned>(mut json: Value, kind: Kind) -> Result<T, CmdError> {
    if let Some(obj) = json.as_object_mut()
        && obj.get("id").is_none_or(Value::is_null)
        && kind != Kind::SshKey
    {
        obj.insert("id".into(), Value::String(Id::now_v7().to_string()));
    }
    // `from_value` moves strings, so a secret is not copied on the way.
    serde_json::from_value(json)
        .map_err(|e| CmdError::invalid(format!("bad {} input: {e}", kind.as_str())))
}

fn not_found(kind: &str, id: Id) -> CmdError {
    CmdError::not_found(format!("{kind} {id}")).with_ref(id)
}

impl CmdError {
    fn with_ref(mut self, id: Id) -> Self {
        self.refs = vec![id.to_string()];
        self
    }
}

// ---------------------------------------------------------------------------
// AppState operations (what the commands call)

impl AppState {
    pub async fn vault_status(&self) -> VaultStatus {
        if let VaultSlot::Failed(why) = self.vault_slot() {
            return VaultStatus {
                state: VaultState::Unavailable,
                quick_unlock_enabled: false,
                quick_unlock_kind: GateKind::None,
                error: Some(why.clone()),
            };
        }
        let status = self
            .with_vault(|v| {
                let state = if !v.is_initialized() {
                    VaultState::Uninitialized
                } else if v.is_unlocked() {
                    VaultState::Unlocked
                } else {
                    VaultState::Locked
                };
                Ok(VaultStatus {
                    state,
                    quick_unlock_enabled: v.is_initialized() && v.is_quick_unlock_enabled(),
                    quick_unlock_kind: v.quick_unlock_kind(),
                    error: None,
                })
            })
            .await;
        status.unwrap_or_else(|e| VaultStatus {
            state: VaultState::Unavailable,
            quick_unlock_enabled: false,
            quick_unlock_kind: GateKind::None,
            error: Some(e.message),
        })
    }

    /// Creates the vault (Argon2, in a blocking task) and returns the
    /// recovery key, once.
    pub async fn vault_create(&self, password: SecretString) -> Result<VaultCreated, CmdError> {
        let recovery_key = self.with_vault(move |v| Ok(v.create(password)?)).await?;
        self.start_sync().await;
        Ok(VaultCreated { recovery_key })
    }

    /// Unlocks with the master password. With sync on, a wrong password gets
    /// one more chance: this device may have missed a password change, so the
    /// current account key material is fetched from the server and the
    /// unlock retried once (any failure there is the same `wrongPassword`).
    /// After the unlock, if sync stopped with "sign in again", the same
    /// password signs the device back in.
    pub async fn vault_unlock(&self, password: SecretString) -> Result<(), CmdError> {
        // Only a device that syncs needs a second copy (for the retry and the
        // sign-in); it is dropped when this call ends.
        let spare = self
            .sync_running()
            .then(|| SecretString::from(password.expose_secret().to_owned()));
        let copy = |s: &SecretString| SecretString::from(s.expose_secret().to_owned());
        let first = self.with_vault(move |v| Ok(v.unlock(password)?)).await;
        match (first, &spare) {
            (Err(e), Some(spare)) if e.code == "wrongPassword" => {
                match self.unlock_refreshed(copy(spare)).await {
                    Some(retried) => retried?,
                    None => return Err(e),
                }
            }
            (first, _) => first?,
        }
        self.unlocked().await;
        if let Some(spare) = spare {
            self.sign_in_if_waiting(spare).await;
        }
        Ok(())
    }

    /// Resets the password with the recovery key and unlocks. With sync on
    /// this is an account-wide password change (every device asks for the new
    /// password; refused with `syncUnavailable` while the engine is not
    /// running); without it, local only.
    pub async fn vault_unlock_recovery(
        &self,
        recovery_key: SecretString,
        new_password: SecretString,
    ) -> Result<(), CmdError> {
        match self.recover_synced(&recovery_key, &new_password).await {
            Some(done) => done?,
            None => {
                self.with_vault(move |v| {
                    Ok(v.unlock_with_recovery(recovery_key.expose_secret(), new_password)?)
                })
                .await?;
            }
        }
        self.unlocked().await;
        Ok(())
    }

    /// Locks the vault and tells every webview (`vault-locked`).
    pub async fn vault_lock(&self) -> Result<(), CmdError> {
        self.lock_vault(LockReason::Manual).await.map(|_| ())
    }

    /// Locks the vault (open sessions keep running) and sends `vault-locked`
    /// with `reason`. An auto-lock ([`LockReason::Idle`]) of a vault that was
    /// not unlocked says nothing, so it cannot replace the reason the UI
    /// shows (a password changed elsewhere). Returns whether the vault was
    /// unlocked.
    pub(crate) async fn lock_vault(&self, reason: LockReason) -> Result<bool, CmdError> {
        let was_unlocked = self
            .with_vault(|v| {
                let was = v.is_unlocked();
                v.lock();
                Ok(was)
            })
            .await?;
        if was_unlocked || reason != LockReason::Idle {
            self.emit(events::VAULT_LOCKED, &VaultLocked { reason });
        }
        Ok(was_unlocked)
    }

    /// After any successful unlock: the auto-lock starts over, and the sync
    /// engine applies what it pulled while locked and pushes (and starts, if
    /// it was not running).
    async fn unlocked(&self) {
        // Someone is here: whatever absence the auto-lock was timing is over.
        self.autolock().unlocked(crate::autolock::Moment::now());
        self.start_sync().await;
        self.0.sync.unlocked();
    }

    pub async fn vault_enable_quick_unlock(&self) -> Result<(), CmdError> {
        self.with_vault(|v| Ok(v.enable_quick_unlock()?)).await
    }

    pub async fn vault_disable_quick_unlock(&self) -> Result<(), CmdError> {
        self.with_vault(|v| Ok(v.disable_quick_unlock()?)).await
    }

    pub async fn vault_quick_unlock(&self) -> Result<(), CmdError> {
        self.with_vault(|v| Ok(v.quick_unlock()?)).await?;
        self.unlocked().await;
        Ok(())
    }

    /// Every live record of `kind`, as DTOs.
    pub async fn list_entities(&self, kind: &str) -> Result<Vec<Value>, CmdError> {
        let kind = entity_kind(kind)?;
        self.with_vault(move |v| list_dtos(v, kind)).await
    }

    /// Inserts or updates a record from its DTO (an id is assigned when
    /// missing) and returns it as stored.
    pub async fn upsert_entity(&self, kind: &str, json: Value) -> Result<Value, CmdError> {
        let kind = entity_kind(kind)?;
        self.with_vault_write(move |v| upsert_dto(v, kind, json))
            .await
    }

    /// Deletes record `id`. `referenced` (with the users in `refs`) while
    /// other records use it; groups with children need `delete_group`.
    ///
    /// Deleting a host (its rules go with it) or a forward rule stops the
    /// affected running forwards.
    pub async fn delete_entity(&self, kind: &str, id: Id) -> Result<(), CmdError> {
        let kind = entity_kind(kind)?;
        let started = self.started_forwards();
        let orphaned = self
            .with_vault_write(move |v| {
                match kind {
                    Kind::Group => v.delete::<Group>(id),
                    Kind::Host => v.delete::<Host>(id),
                    Kind::Tag => v.delete::<Tag>(id),
                    Kind::Identity => v.delete::<Identity>(id),
                    Kind::SshKey => v.delete::<SshKey>(id),
                    Kind::KnownHost => v.delete::<KnownHost>(id),
                    Kind::ForwardRule => v.delete::<ForwardRule>(id),
                    Kind::Snippet => v.delete::<Snippet>(id),
                    Kind::Preference | Kind::Conflict => unreachable!("rejected by entity_kind"),
                }?;
                // A deleted host or group takes its own login with it.
                v.tidy_own_logins()?;
                Ok(orphaned_forwards(v, started))
            })
            .await?;
        self.stop_forwards(orphaned).await;
        Ok(())
    }

    /// Deletes a group with `policy` for its children. A recursive delete
    /// stops the running forwards of every host it removes.
    pub async fn delete_group(&self, id: Id, policy: ChildPolicyDto) -> Result<(), CmdError> {
        let started = self.started_forwards();
        let orphaned = self
            .with_vault_write(move |v| {
                v.delete_group(id, policy.into())?;
                v.tidy_own_logins()?;
                Ok(orphaned_forwards(v, started))
            })
            .await?;
        self.stop_forwards(orphaned).await;
        Ok(())
    }

    /// `(rule, host)` of every rule started in this run.
    fn started_forwards(&self) -> Vec<(Id, Id)> {
        lock(&self.0.forward_hosts)
            .iter()
            .map(|(rule, host)| (*rule, *host))
            .collect()
    }

    /// Stops each deleted rule, lets its host's connection go once idle, and
    /// forgets it.
    async fn stop_forwards(&self, rules: Vec<Id>) {
        for rule in rules {
            tracing::info!(rule = %rule, "stopping a forward whose rule or host was deleted");
            self.forward_stop(rule).await;
            lock(&self.0.forward_hosts).remove(&rule);
        }
    }

    /// Sets the own login (username and password) of a host or group and
    /// links it there; see `VaultService::set_own_login`.
    pub async fn set_own_login(&self, login: OwnLoginInput) -> Result<IdentityDto, CmdError> {
        self.with_vault_write(move |v| {
            let password = match login.password {
                None => PasswordChange::Keep,
                Some(None) => PasswordChange::Clear,
                Some(Some(p)) => PasswordChange::Set(p),
            };
            let identity = v.set_own_login(login.owner_id, login.username, password)?;
            v.tidy_own_logins()?;
            Ok(IdentityDto::from(&identity))
        })
        .await
    }

    pub async fn effective_settings(&self, host_id: Id) -> Result<EffectiveSettingsDto, CmdError> {
        self.with_vault(move |v| Ok(v.effective_settings(host_id)?.into()))
            .await
    }

    /// Imports an `~/.ssh/config`-style file: hosts, identities and the key
    /// files they name. Keys that do not load are reported, not fatal.
    pub async fn import_ssh_config(&self, path: PathBuf) -> Result<ImportReportDto, CmdError> {
        let path = expand_home(&path, std::env::home_dir().as_deref());
        self.with_vault_write(move |v| {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| CmdError::io(format!("could not read {}: {e}", path.display())))?;
            let existing = v.list::<Host>()?;
            let plan = plan_ssh_config_import(&text, &existing);
            let skipped = plan
                .skipped
                .iter()
                .map(|(name, reason)| SkippedEntryDto {
                    name: name.clone(),
                    reason: reason.clone(),
                })
                .collect();
            let report = v.apply_import(plan, |key_path| {
                let text = std::fs::read_to_string(key_path).map_err(|e| {
                    KeyError::Parse(format!("could not read {}: {e}", key_path.display()))
                })?;
                let text = SecretString::from(text);
                keys::import(text.expose_secret(), None)
            })?;
            Ok(ImportReportDto {
                hosts_added: report.hosts_added,
                identities_added: report.identities_added,
                keys_added: report.keys_added,
                key_failures: report
                    .key_failures
                    .into_iter()
                    .map(|f| KeyFailureDto {
                        path: f.path.display().to_string(),
                        needs_passphrase: f.needs_passphrase,
                        message: f.message,
                    })
                    .collect(),
                skipped,
            })
        })
        .await
    }

    /// Generates a key (in a blocking task: RSA takes a while) and stores
    /// it; `passphrase` encrypts it and is stored with it.
    pub async fn key_generate(
        &self,
        algo: KeyAlgoDto,
        label: String,
        passphrase: Option<SecretString>,
    ) -> Result<SshKeyDto, CmdError> {
        let comment = label.clone();
        let (generated, passphrase) = tauri::async_runtime::spawn_blocking(move || {
            keys::generate(algo.into(), &comment, passphrase.as_ref()).map(|g| (g, passphrase))
        })
        .await
        .map_err(|e| CmdError::internal(format!("key generation failed: {e}")))??;
        self.store_key(SshKey {
            id: Id::now_v7(),
            label,
            algorithm: generated.algorithm,
            private_openssh: generated.private_openssh,
            passphrase,
            public_openssh: generated.public_openssh,
            certificate: None,
            extra: Default::default(),
        })
        .await
    }

    /// Imports a private key (OpenSSH, PKCS#1, SEC1 or PKCS#8 text) and
    /// stores it. An encrypted key needs `passphrase`, which is then stored
    /// with it. `label` defaults to the key's comment.
    pub async fn key_import(
        &self,
        text: SecretString,
        passphrase: Option<SecretString>,
        label: Option<String>,
    ) -> Result<SshKeyDto, CmdError> {
        let (imported, passphrase) = tauri::async_runtime::spawn_blocking(move || {
            let imported = keys::import(text.expose_secret(), passphrase.as_ref())?;
            // The output is encrypted iff the input was; keep the
            // passphrase only then.
            let encrypted = matches!(
                keys::import(imported.private_openssh.expose_secret(), None),
                Err(KeyError::NeedsPassphrase)
            );
            Ok::<_, KeyError>((imported, passphrase.filter(|_| encrypted)))
        })
        .await
        .map_err(|e| CmdError::internal(format!("key import failed: {e}")))??;
        let label = label
            .filter(|l| !l.trim().is_empty())
            .or_else(|| {
                imported
                    .public_openssh
                    .split_whitespace()
                    .nth(2)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "Imported key".to_owned());
        self.store_key(SshKey {
            id: Id::now_v7(),
            label,
            algorithm: imported.algorithm,
            private_openssh: imported.private_openssh,
            passphrase,
            public_openssh: imported.public_openssh,
            certificate: None,
            extra: Default::default(),
        })
        .await
    }

    async fn store_key(&self, key: SshKey) -> Result<SshKeyDto, CmdError> {
        self.with_vault_write(move |v| {
            let dto = SshKeyDto::from(&key);
            v.upsert(key)?;
            Ok(dto)
        })
        .await
    }

    /// The stored private key, for the user to save elsewhere.
    pub async fn key_export_private(&self, id: Id) -> Result<KeyExport, CmdError> {
        self.with_vault(move |v| {
            let key = v
                .get::<SshKey>(id)?
                .ok_or_else(|| not_found("ssh_key", id))?;
            Ok(KeyExport {
                private_openssh: key.private_openssh.expose_secret().to_owned(),
            })
        })
        .await
    }

    /// Imports an OpenSSH `known_hosts` file as trusted keys (source
    /// `import`), skipping entries already stored. All new entries are
    /// written in one vault transaction ([`VaultService::upsert_all`]).
    pub async fn known_hosts_import(
        &self,
        path: PathBuf,
    ) -> Result<KnownHostsImportReport, CmdError> {
        let path = expand_home(&path, std::env::home_dir().as_deref());
        self.with_vault_write(move |v| {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| CmdError::io(format!("could not read {}: {e}", path.display())))?;
            let mut seen: HashSet<(String, u16, String, String)> = v
                .list::<KnownHost>()?
                .into_iter()
                .map(|k| (k.host_pattern, k.port, k.algorithm, k.public_key))
                .collect();
            let mut report = KnownHostsImportReport {
                added: 0,
                already_known: 0,
            };
            let now = unix_now();
            let mut new = Vec::new();
            for e in parse_known_hosts(&text) {
                let key = (
                    e.host_pattern.clone(),
                    e.port,
                    e.algorithm.clone(),
                    e.public_key.clone(),
                );
                if !seen.insert(key) {
                    report.already_known += 1;
                    continue;
                }
                new.push(KnownHost {
                    id: Id::now_v7(),
                    host_pattern: e.host_pattern,
                    port: e.port,
                    algorithm: e.algorithm,
                    public_key: e.public_key,
                    added_at: now,
                    source: KnownHostSource::Import,
                    extra: Default::default(),
                });
            }
            // One transaction: a failure part-way leaves nothing imported.
            report.added = new.len();
            v.upsert_all(new)?;
            Ok(report)
        })
        .await
    }

    /// Connection parameters for vault host `host_id`: address, effective
    /// port, terminal type, keepalive and env, and the effective identity's
    /// credentials (key, else password, else keyboard-interactive / prompted
    /// password). A host without an identity uses keyboard-interactive, and
    /// an empty username is the local login name ([`local_login_name`];
    /// ruling R31). `charset` is not applied here: terminals are UTF-8.
    pub async fn connect_params(
        &self,
        host_id: Id,
        cols: u16,
        rows: u16,
    ) -> Result<ConnectParams, CmdError> {
        self.with_vault(move |v| {
            let host = v
                .get::<Host>(host_id)?
                .ok_or_else(|| not_found("host", host_id))?;
            let eff = v.effective_settings(host_id)?;
            // No identity: what `ssh host` does, the local login name and
            // keyboard-interactive (which falls back to a password prompt).
            let (username, auth) = match eff.identity_id.value {
                None => (String::new(), AuthMethod::KeyboardInteractive),
                Some(identity_id) => {
                    let identity = v
                        .get::<Identity>(identity_id)?
                        .ok_or_else(|| not_found("identity", identity_id))?;
                    let auth = match (identity.key_id, identity.password) {
                        (Some(key_id), _) => {
                            let key = v
                                .get::<SshKey>(key_id)?
                                .ok_or_else(|| not_found("ssh_key", key_id))?;
                            AuthMethod::PrivateKey {
                                pem: key.private_openssh,
                                passphrase: key.passphrase,
                            }
                        }
                        (None, Some(password)) => AuthMethod::Password(password),
                        (None, None) => AuthMethod::KeyboardInteractive,
                    };
                    (identity.username, auth)
                }
            };
            // An empty username (no identity, or an imported `IdentityFile`
            // without `User`) is the local login name, as with `ssh`.
            let username = if username.trim().is_empty() {
                local_login_name()
                    .ok_or_else(|| CmdError::new("noUsername", no_username_message(cfg!(mobile))))?
            } else {
                username
            };
            Ok(ConnectParams {
                host: host.address,
                port: eff.port.value,
                username,
                auth,
                term: eff
                    .term_type
                    .value
                    .unwrap_or_else(|| DEFAULT_TERM.to_owned()),
                cols,
                rows,
                connect_timeout: CONNECT_TIMEOUT,
                // 0 turns keepalives off; unset uses core-ssh's default.
                keepalive: eff
                    .keepalive_secs
                    .value
                    .map(|s| Duration::from_secs(u64::from(s))),
                env: eff.env.value.unwrap_or_default(),
            })
        })
        .await
    }

    /// The label and terminal type of a host, for opening a shell on it.
    pub(crate) async fn host_label_and_term(
        &self,
        host_id: Id,
    ) -> Result<(String, String), CmdError> {
        self.with_vault(move |v| {
            let host = v
                .get::<Host>(host_id)?
                .ok_or_else(|| not_found("host", host_id))?;
            let eff = v.effective_settings(host_id)?;
            Ok((
                host.label,
                eff.term_type
                    .value
                    .unwrap_or_else(|| DEFAULT_TERM.to_owned()),
            ))
        })
        .await
    }
}

/// The local login name `ssh` would use for a host without `User`: `$USER`,
/// else `$LOGNAME`, else `$USERNAME` (Windows). `None` when none is set.
pub fn local_login_name() -> Option<String> {
    login_name_from(|name| std::env::var(name).ok())
}

/// Why a host without a username cannot connect. Phones have no local login
/// name to fall back on, so it says where to set one.
fn no_username_message(mobile: bool) -> &'static str {
    if mobile {
        "No username. Set a username on this host or its identity."
    } else {
        "no username: the local login name is unknown; set one on an identity"
    }
}

/// [`local_login_name`] over an injected environment.
pub fn login_name_from(env: impl Fn(&str) -> Option<String>) -> Option<String> {
    ["USER", "LOGNAME", "USERNAME"]
        .into_iter()
        .filter_map(env)
        .map(|v| v.trim().to_owned())
        .find(|v| !v.is_empty())
}

/// The started rules (of `(rule, host)` pairs) whose rule or host record is
/// gone from the vault. A rule that cannot be checked counts as gone: after
/// a delete, stopping too much beats leaving a listener nobody can see.
fn orphaned_forwards(v: &VaultService, started: Vec<(Id, Id)>) -> Vec<Id> {
    started
        .into_iter()
        .filter(|(rule, host)| {
            !matches!(v.get::<ForwardRule>(*rule), Ok(Some(_)))
                || !matches!(v.get::<Host>(*host), Ok(Some(_)))
        })
        .map(|(rule, _)| rule)
        .collect()
}

fn list_dtos(v: &VaultService, kind: Kind) -> Result<Vec<Value>, CmdError> {
    fn all<T: core_api::vault::Entity, D: Serialize>(
        v: &VaultService,
        f: impl Fn(T) -> D,
    ) -> Result<Vec<Value>, CmdError> {
        v.list::<T>()?.into_iter().map(|e| to_json(f(e))).collect()
    }
    match kind {
        Kind::Group => all::<Group, _>(v, GroupDto::from),
        Kind::Host => all::<Host, _>(v, HostDto::from),
        Kind::Tag => all::<Tag, _>(v, TagDto::from),
        Kind::Identity => all(v, |i: Identity| IdentityDto::from(&i)),
        Kind::SshKey => all(v, |k: SshKey| SshKeyDto::from(&k)),
        Kind::KnownHost => all::<KnownHost, _>(v, KnownHostDto::from),
        Kind::ForwardRule => all::<ForwardRule, _>(v, ForwardRuleDto::from),
        Kind::Snippet => all::<Snippet, _>(v, SnippetDto::from),
        Kind::Preference | Kind::Conflict => unreachable!("rejected by entity_kind"),
    }
}

fn upsert_dto(v: &mut VaultService, kind: Kind, json: Value) -> Result<Value, CmdError> {
    match kind {
        Kind::Group => {
            let g: Group = input::<GroupDto>(json, kind)?.into();
            let id = g.id;
            v.upsert(g)?;
            v.tidy_own_logins()?;
            stored::<Group, _>(v, id, GroupDto::from)
        }
        Kind::Host => {
            let h: Host = input::<HostDto>(json, kind)?.into();
            let id = h.id;
            v.upsert(h)?;
            v.tidy_own_logins()?;
            stored::<Host, _>(v, id, HostDto::from)
        }
        Kind::Tag => {
            let t: Tag = input::<TagDto>(json, kind)?.into();
            let id = t.id;
            v.upsert(t)?;
            stored::<Tag, _>(v, id, TagDto::from)
        }
        Kind::Snippet => {
            let s: Snippet = input::<SnippetDto>(json, kind)?.into();
            let id = s.id;
            v.upsert(s)?;
            stored::<Snippet, _>(v, id, SnippetDto::from)
        }
        Kind::Identity => {
            let i: IdentityInput = input(json, kind)?;
            let id = i.id.expect("input() assigns an id");
            let old = v.get::<Identity>(id)?;
            let password = match i.password {
                Some(set_or_clear) => set_or_clear,
                None => old.as_ref().and_then(|old| old.password.clone()),
            };
            v.upsert(Identity {
                id,
                label: i.label,
                username: i.username,
                password,
                key_id: i.key_id,
                // An own login stays its owner's.
                owner: old.and_then(|old| old.owner),
                extra: Default::default(),
            })?;
            stored(v, id, |i: Identity| IdentityDto::from(&i))
        }
        Kind::SshKey => {
            let k: SshKeyInput = input(json, kind)?;
            let mut key = v
                .get::<SshKey>(k.id)?
                .ok_or_else(|| not_found("ssh_key", k.id))?;
            key.label = k.label;
            v.upsert(key)?;
            stored(v, k.id, |k: SshKey| SshKeyDto::from(&k))
        }
        Kind::KnownHost => {
            let k = KnownHost::try_from(input::<KnownHostDto>(json, kind)?)?;
            let id = k.id;
            v.upsert(k)?;
            stored::<KnownHost, _>(v, id, KnownHostDto::from)
        }
        Kind::ForwardRule => {
            let r = ForwardRule::try_from(input::<ForwardRuleDto>(json, kind)?)?;
            let id = r.id;
            v.upsert(r)?;
            stored::<ForwardRule, _>(v, id, ForwardRuleDto::from)
        }
        Kind::Preference | Kind::Conflict => unreachable!("rejected by entity_kind"),
    }
}

/// Reads record `id` back (as normalised by the vault) as a DTO.
fn stored<T: core_api::vault::Entity, D: Serialize>(
    v: &VaultService,
    id: Id,
    f: impl Fn(T) -> D,
) -> Result<Value, CmdError> {
    let value = v
        .get::<T>(id)?
        .ok_or_else(|| CmdError::internal(format!("record {id} vanished after upsert")))?;
    to_json(f(value))
}

/// `~` or `~/rest` as a path under `home`; anything else is returned as is
/// (also when the home directory is unknown, so the read then fails with the
/// path the user gave). The UI offers `~/.ssh/config` as the default.
pub fn expand_home(path: &std::path::Path, home: Option<&std::path::Path>) -> PathBuf {
    let Some(home) = home else {
        return path.to_path_buf();
    };
    match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// Commands

#[tauri::command]
pub async fn vault_status(state: State<'_, AppState>) -> Result<VaultStatus, CmdError> {
    Ok(state.vault_status().await)
}

#[tauri::command]
pub async fn vault_create(
    state: State<'_, AppState>,
    password: SecretString,
) -> Result<VaultCreated, CmdError> {
    state.vault_create(password).await
}

#[tauri::command]
pub async fn vault_unlock(
    state: State<'_, AppState>,
    password: SecretString,
) -> Result<(), CmdError> {
    state.vault_unlock(password).await
}

#[tauri::command]
pub async fn vault_unlock_recovery(
    state: State<'_, AppState>,
    recovery_key: SecretString,
    new_password: SecretString,
) -> Result<(), CmdError> {
    state
        .vault_unlock_recovery(recovery_key, new_password)
        .await
}

#[tauri::command]
pub async fn vault_lock(state: State<'_, AppState>) -> Result<(), CmdError> {
    state.vault_lock().await
}

#[tauri::command]
pub async fn vault_enable_quick_unlock(state: State<'_, AppState>) -> Result<(), CmdError> {
    state.vault_enable_quick_unlock().await
}

#[tauri::command]
pub async fn vault_disable_quick_unlock(state: State<'_, AppState>) -> Result<(), CmdError> {
    state.vault_disable_quick_unlock().await
}

#[tauri::command]
pub async fn vault_quick_unlock(state: State<'_, AppState>) -> Result<(), CmdError> {
    state.vault_quick_unlock().await
}

#[tauri::command]
pub async fn list_entities(
    state: State<'_, AppState>,
    kind: String,
) -> Result<Vec<Value>, CmdError> {
    state.list_entities(&kind).await
}

#[tauri::command]
pub async fn upsert_entity(
    state: State<'_, AppState>,
    kind: String,
    json: Value,
) -> Result<Value, CmdError> {
    state.upsert_entity(&kind, json).await
}

#[tauri::command]
pub async fn delete_entity(
    state: State<'_, AppState>,
    kind: String,
    id: Id,
) -> Result<(), CmdError> {
    state.delete_entity(&kind, id).await
}

#[tauri::command]
pub async fn delete_group(
    state: State<'_, AppState>,
    id: Id,
    policy: ChildPolicyDto,
) -> Result<(), CmdError> {
    state.delete_group(id, policy).await
}

#[tauri::command]
pub async fn set_own_login(
    state: State<'_, AppState>,
    login: OwnLoginInput,
) -> Result<IdentityDto, CmdError> {
    state.set_own_login(login).await
}

#[tauri::command]
pub async fn effective_settings(
    state: State<'_, AppState>,
    host_id: Id,
) -> Result<EffectiveSettingsDto, CmdError> {
    state.effective_settings(host_id).await
}

#[tauri::command]
pub async fn import_ssh_config(
    state: State<'_, AppState>,
    path: PathBuf,
) -> Result<ImportReportDto, CmdError> {
    state.import_ssh_config(path).await
}

#[tauri::command]
pub async fn key_generate(
    state: State<'_, AppState>,
    algo: KeyAlgoDto,
    label: String,
    passphrase: Option<SecretString>,
) -> Result<SshKeyDto, CmdError> {
    state.key_generate(algo, label, passphrase).await
}

#[tauri::command]
pub async fn key_import(
    state: State<'_, AppState>,
    text: SecretString,
    passphrase: Option<SecretString>,
    label: Option<String>,
) -> Result<SshKeyDto, CmdError> {
    state.key_import(text, passphrase, label).await
}

#[tauri::command]
pub async fn key_export_private(state: State<'_, AppState>, id: Id) -> Result<KeyExport, CmdError> {
    state.key_export_private(id).await
}

#[tauri::command]
pub async fn known_hosts_import(
    state: State<'_, AppState>,
    path: PathBuf,
) -> Result<KnownHostsImportReport, CmdError> {
    state.known_hosts_import(path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn no_username_says_where_to_set_one_on_ios_only() {
        assert_eq!(
            no_username_message(true),
            "No username. Set a username on this host or its identity."
        );
        assert_eq!(
            no_username_message(false),
            "no username: the local login name is unknown; set one on an identity"
        );
    }

    #[test]
    fn expand_home_replaces_a_leading_tilde_only() {
        let home = Path::new("/home/me");
        assert_eq!(
            expand_home(Path::new("~/.ssh/config"), Some(home)),
            Path::new("/home/me/.ssh/config")
        );
        assert_eq!(expand_home(Path::new("~"), Some(home)), home);
        // Not a home reference: `~user`, a tilde inside a name, absolute and relative paths.
        for p in ["~user/x", "/etc/~/x", "a/~/b", "/abs/config", "rel/config"] {
            assert_eq!(expand_home(Path::new(p), Some(home)), Path::new(p), "{p}");
        }
        // No home directory: left alone.
        assert_eq!(
            expand_home(Path::new("~/.ssh/config"), None),
            Path::new("~/.ssh/config")
        );
    }
}
