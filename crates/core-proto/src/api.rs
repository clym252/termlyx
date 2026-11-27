//! Request and response bodies. See the crate docs for conventions.
//!
//! Ids (`vaultId`, record ids, device ids) are strings holding UUIDs; the
//! server treats them as opaque. `hlc` is the fixed-width `Display` form of
//! `core_model::Hlc`. Timestamps (`createdAt`, `lastSeen`) are Unix
//! milliseconds.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Debug stand-in for a secret value.
struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Debug stand-in that shows only how many bytes a sealed value holds.
struct Len(usize);

impl fmt::Debug for Len {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} bytes>", self.0)
    }
}

/// Serde adapter for byte fields: base64, standard alphabet, padded.
mod b64 {
    use data_encoding::BASE64;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        BASE64
            .decode(text.as_bytes())
            .map_err(|e| D::Error::custom(format!("invalid base64: {e}")))
    }
}

/// Serde adapter for optional byte fields.
mod b64_opt {
    use data_encoding::BASE64;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(b) => s.serialize_str(&BASE64.encode(b)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|text| {
                BASE64
                    .decode(text.as_bytes())
                    .map_err(|e| D::Error::custom(format!("invalid base64: {e}")))
            })
            .transpose()
    }
}

/// Argon2id parameters and salt, as stored with the account. The sender is not
/// trusted: the receiving client bounds-checks `m_kib`/`t`/`p` before deriving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KdfWire {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
    /// The 16-byte salt.
    #[serde(with = "b64")]
    pub salt: Vec<u8>,
}

/// Device description sent at register/login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub name: String,
    pub platform: String,
}

/// A device as listed to its account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub created_at: i64,
    pub last_seen: i64,
    /// True for the device making the request.
    pub current: bool,
}

/// `PATCH /v1/devices/{id}`: rename a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameDeviceReq {
    pub name: String,
}

/// Response to `POST /v1/link`: an add-device code for another device.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkResp {
    /// The `tether1-...` add-device code (kind link).
    pub code: String,
    /// When the code stops working, Unix milliseconds.
    pub expires_at: i64,
}

/// The account's wrapped key material, exactly as held in the local `account`
/// table (see `docs/crypto.md`). All byte fields are opaque to the server.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountMaterial {
    pub vault_id: String,
    pub key_version: u32,
    pub kdf: KdfWire,
    #[serde(with = "b64")]
    pub account_public: Vec<u8>,
    #[serde(with = "b64")]
    pub secret_by_kek: Vec<u8>,
    #[serde(with = "b64")]
    pub secret_by_recovery: Vec<u8>,
    #[serde(with = "b64")]
    pub sealed_vault_key: Vec<u8>,
}

/// `POST /v1/prelogin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreloginReq {
    pub username: String,
}

/// Response to prelogin: the KDF parameters to derive the AuthKey with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreloginResp {
    pub kdf: KdfWire,
}

/// `POST /v1/register`: create an account with an invite.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterReq {
    /// The invite's 32-byte token.
    #[serde(with = "b64")]
    pub invite_token: Vec<u8>,
    pub username: String,
    #[serde(with = "b64")]
    pub auth_key: Vec<u8>,
    pub device: DeviceInfo,
    pub material: AccountMaterial,
    #[serde(with = "b64")]
    pub recovery_auth_key: Vec<u8>,
    /// The HLC node the client already uses; the server keeps it if free.
    pub node_hint: u16,
    /// Proof of a short invite code, sent instead of `invite_token` (which is
    /// then empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_proof: Option<CodeProof>,
}

/// `POST /v1/code/verify`: prove knowledge of a short code without sending it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeVerifyReq {
    /// 16 bytes, `ShortCode::id`.
    #[serde(with = "b64")]
    pub code_id: Vec<u8>,
    /// 32 random bytes.
    #[serde(with = "b64")]
    pub client_nonce: Vec<u8>,
    /// Whether the client bound the proof to the server's SPKI pin.
    pub pinned: bool,
    /// Unpinned only: the typed address's `host:port` the proof is bound to
    /// (`short_code::host_binding`). Absent from older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// The server's proof that it knows the code's key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeVerifyResp {
    #[serde(with = "b64")]
    pub server_proof: Vec<u8>,
}

/// A short code redeemed in `RegisterReq`: the client's proof, never the code.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeProof {
    #[serde(with = "b64")]
    pub code_id: Vec<u8>,
    #[serde(with = "b64")]
    pub client_nonce: Vec<u8>,
    pub pinned: bool,
    /// As `CodeVerifyReq::host`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(with = "b64")]
    pub proof: Vec<u8>,
}

impl fmt::Debug for CodeProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodeProof")
            .field("pinned", &self.pinned)
            .finish_non_exhaustive()
    }
}

/// `POST /v1/login`. `link_token` is the 32-byte add-device token when the
/// device is joining through a link code.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginReq {
    pub username: String,
    #[serde(with = "b64")]
    pub auth_key: Vec<u8>,
    pub device: DeviceInfo,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "b64_opt")]
    pub link_token: Option<Vec<u8>>,
}

/// `POST /v1/recover-login`: log in with the recovery auth key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverReq {
    pub username: String,
    #[serde(with = "b64")]
    pub recovery_auth_key: Vec<u8>,
    pub device: DeviceInfo,
}

/// Session tokens issued by register, login, recover-login and refresh.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenResp {
    pub access_token: String,
    pub refresh_token: String,
    /// Access token lifetime in seconds.
    pub expires_in: u64,
    pub device_id: String,
    /// HLC node id the server assigned to this device.
    pub node: u16,
    /// Server epoch id; changes when the server is restored from a backup.
    pub epoch: String,
    /// Per-device secret (32 random bytes, base64url without padding), issued
    /// once when the device is created (register or link-code login) and empty
    /// otherwise. A later password re-login must present it in
    /// `X-Tether-Device-Secret`; the client stores it next to the refresh token.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_secret: String,
}

/// `POST /v1/refresh`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshReq {
    pub refresh_token: String,
}

/// `GET /v1/account`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountResp {
    pub material: AccountMaterial,
    /// Increases on every password change or recovery reset.
    pub account_rev: i64,
}

/// Change the master password: re-wrapped account secret under the new KEK.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordChangeReq {
    #[serde(with = "b64")]
    pub old_auth_key: Vec<u8>,
    #[serde(with = "b64")]
    pub new_auth_key: Vec<u8>,
    pub kdf: KdfWire,
    #[serde(with = "b64")]
    pub secret_by_kek: Vec<u8>,
    /// The `accountRev` the client based the change on.
    pub account_rev_base: i64,
}

/// Reset the master password with the recovery key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryResetReq {
    #[serde(with = "b64")]
    pub recovery_auth_key: Vec<u8>,
    #[serde(with = "b64")]
    pub new_auth_key: Vec<u8>,
    pub kdf: KdfWire,
    #[serde(with = "b64")]
    pub secret_by_kek: Vec<u8>,
}

/// `PUT /v1/account/recovery`: replace the recovery key. The signed-in device
/// proves the master password with the current `authKey` (checked like a
/// sign-in, wrong keys count toward the lockout), then sends the new recovery
/// auth key (the server stores a verifier of it) and the account secret
/// re-wrapped under the new recovery key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryUpdateReq {
    #[serde(with = "b64")]
    pub auth_key: Vec<u8>,
    #[serde(with = "b64")]
    pub recovery_auth_key: Vec<u8>,
    #[serde(with = "b64")]
    pub secret_by_recovery: Vec<u8>,
}

/// One sealed record as the server stores and relays it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRow {
    pub id: String,
    pub kind: String,
    pub key_version: u32,
    pub hlc: String,
    /// Server revision; absent on rows a client pushes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<i64>,
    /// Position in the vault's change log; absent on rows a client pushes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
    pub deleted: bool,
    #[serde(with = "b64")]
    pub envelope: Vec<u8>,
}

/// `GET /v1/sync/changes`: one page of the vault's change log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesResp {
    pub changes: Vec<WireRow>,
    /// Cursor to pass as `since` for the next page.
    pub next: i64,
    pub more: bool,
    pub epoch: String,
}

/// One row to push and the server revision it was edited from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushItem {
    pub row: WireRow,
    /// `None` for a row the server has never seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_rev: Option<i64>,
}

/// `POST /v1/sync/push`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushReq {
    pub items: Vec<PushItem>,
}

/// Per-item outcome of a push, in request order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PushResult {
    /// Accepted at `rev`, logged at `seq`.
    Ok { id: String, rev: i64, seq: i64 },
    /// The server's current row differs from `baseRev`; nothing was written.
    Conflict { id: String, current: WireRow },
}

/// Response to a push.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResp {
    pub results: Vec<PushResult>,
}

/// A message on the WebSocket event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    /// The vault log advanced to `seq`.
    Changed { seq: i64 },
    /// The account material changed (password change or recovery reset).
    AccountChanged { account_rev: i64 },
    /// This device was revoked; the server closes the socket next.
    Revoked,
}

/// Machine-readable error class, `snake_case` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    Revoked,
    Forbidden,
    NotFound,
    Conflict,
    Gone,
    ClockSkew,
    RateLimited,
    UpdateClient,
    UpdateServer,
    InviteInvalid,
    UsernameTaken,
    PayloadTooLarge,
    Internal,
}

/// Body of every non-2xx response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// Plain-language message, safe to show to the user.
    pub message: String,
}

// Manual `Debug` impls: secrets print as `[REDACTED]`, wrapped key material and
// envelopes as their length, so request structs are safe to log.

impl fmt::Debug for AccountMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountMaterial")
            .field("vault_id", &self.vault_id)
            .field("key_version", &self.key_version)
            .field("kdf", &self.kdf)
            .field("account_public", &self.account_public)
            .field("secret_by_kek", &Len(self.secret_by_kek.len()))
            .field("secret_by_recovery", &Len(self.secret_by_recovery.len()))
            .field("sealed_vault_key", &Len(self.sealed_vault_key.len()))
            .finish()
    }
}

impl fmt::Debug for RegisterReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisterReq")
            .field("invite_token", &Redacted)
            .field("username", &self.username)
            .field("auth_key", &Redacted)
            .field("device", &self.device)
            .field("material", &self.material)
            .field("recovery_auth_key", &Redacted)
            .field("node_hint", &self.node_hint)
            .field("code_proof", &self.code_proof)
            .finish()
    }
}

impl fmt::Debug for LoginReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginReq")
            .field("username", &self.username)
            .field("auth_key", &Redacted)
            .field("device", &self.device)
            .field("link_token", &self.link_token.as_ref().map(|_| Redacted))
            .finish()
    }
}

impl fmt::Debug for RecoverReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoverReq")
            .field("username", &self.username)
            .field("recovery_auth_key", &Redacted)
            .field("device", &self.device)
            .finish()
    }
}

impl fmt::Debug for TokenResp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenResp")
            .field("access_token", &Redacted)
            .field("refresh_token", &Redacted)
            .field("expires_in", &self.expires_in)
            .field("device_id", &self.device_id)
            .field("node", &self.node)
            .field("device_secret", &Redacted)
            .field("epoch", &self.epoch)
            .finish()
    }
}

impl fmt::Debug for LinkResp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkResp")
            .field("code", &Redacted)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl fmt::Debug for RefreshReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshReq")
            .field("refresh_token", &Redacted)
            .finish()
    }
}

impl fmt::Debug for PasswordChangeReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordChangeReq")
            .field("old_auth_key", &Redacted)
            .field("new_auth_key", &Redacted)
            .field("kdf", &self.kdf)
            .field("secret_by_kek", &Len(self.secret_by_kek.len()))
            .field("account_rev_base", &self.account_rev_base)
            .finish()
    }
}

impl fmt::Debug for RecoveryResetReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoveryResetReq")
            .field("recovery_auth_key", &Redacted)
            .field("new_auth_key", &Redacted)
            .field("kdf", &self.kdf)
            .field("secret_by_kek", &Len(self.secret_by_kek.len()))
            .finish()
    }
}

impl fmt::Debug for RecoveryUpdateReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoveryUpdateReq")
            .field("auth_key", &Redacted)
            .field("recovery_auth_key", &Redacted)
            .field("secret_by_recovery", &Len(self.secret_by_recovery.len()))
            .finish()
    }
}

impl fmt::Debug for WireRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireRow")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("key_version", &self.key_version)
            .field("hlc", &self.hlc)
            .field("rev", &self.rev)
            .field("seq", &self.seq)
            .field("deleted", &self.deleted)
            .field("envelope", &Len(self.envelope.len()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row() -> WireRow {
        WireRow {
            id: "0190aaaa-0000-7000-8000-000000000001".into(),
            kind: "host".into(),
            key_version: 1,
            hlc: "0000018f0000000000000001-0001".into(),
            rev: Some(3),
            seq: Some(9),
            deleted: false,
            envelope: vec![1, 2, 3, 4],
        }
    }

    #[test]
    fn register_req_without_proof_serializes_as_before() {
        let mut v = serde_json::to_value(RegisterReq {
            invite_token: vec![1],
            username: "u".into(),
            auth_key: vec![2],
            device: DeviceInfo {
                name: "n".into(),
                platform: "p".into(),
            },
            material: AccountMaterial {
                kdf: KdfWire {
                    m_kib: 1,
                    t: 1,
                    p: 1,
                    salt: vec![0],
                },
                account_public: vec![1],
                secret_by_kek: vec![2],
                secret_by_recovery: vec![3],
                sealed_vault_key: vec![4],
                vault_id: "v".into(),
                key_version: 1,
            },
            recovery_auth_key: vec![6],
            node_hint: 1,
            code_proof: None,
        })
        .unwrap();
        assert!(v.get("codeProof").is_none());
        // With a proof it round-trips as camelCase base64.
        let proof = CodeProof {
            code_id: vec![1; 16],
            client_nonce: vec![2; 32],
            pinned: true,
            host: None,
            proof: vec![3; 32],
        };
        v["codeProof"] = serde_json::to_value(&proof).unwrap();
        assert_eq!(
            v["codeProof"]["codeId"],
            data_encoding::BASE64.encode(&[1; 16])
        );
        assert_eq!(v["codeProof"]["pinned"], true);
        assert!(v["codeProof"].get("host").is_none(), "absent, as older clients send it");
        let back: RegisterReq = serde_json::from_value(v).unwrap();
        assert_eq!(back.code_proof, Some(proof));
    }

    #[test]
    fn wire_types_json_shape() {
        let conflict = PushResult::Conflict {
            id: "a".into(),
            current: row(),
        };
        assert_eq!(
            serde_json::to_value(&conflict).unwrap(),
            json!({
                "type": "conflict",
                "id": "a",
                "current": {
                    "id": "0190aaaa-0000-7000-8000-000000000001",
                    "kind": "host",
                    "keyVersion": 1,
                    "hlc": "0000018f0000000000000001-0001",
                    "rev": 3,
                    "seq": 9,
                    "deleted": false,
                    "envelope": "AQIDBA==",
                },
            })
        );
        let ok = PushResult::Ok {
            id: "a".into(),
            rev: 4,
            seq: 10,
        };
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            json!({"type": "ok", "id": "a", "rev": 4, "seq": 10})
        );
        assert_eq!(
            serde_json::to_value(Event::Changed { seq: 12 }).unwrap(),
            json!({"type": "changed", "seq": 12})
        );
        assert_eq!(
            serde_json::to_value(Event::AccountChanged { account_rev: 2 }).unwrap(),
            json!({"type": "account_changed", "accountRev": 2})
        );
        assert_eq!(
            serde_json::to_value(Event::Revoked).unwrap(),
            json!({"type": "revoked"})
        );
        assert_eq!(
            serde_json::to_value(ErrorBody {
                code: ErrorCode::UpdateClient,
                message: "update Tether".into(),
            })
            .unwrap(),
            json!({"code": "update_client", "message": "update Tether"})
        );
        // Everything parses back.
        let back: PushResult =
            serde_json::from_value(serde_json::to_value(&conflict).unwrap()).unwrap();
        assert_eq!(back, conflict);
    }

    #[test]
    fn request_json_shape() {
        let kdf = KdfWire {
            m_kib: 65536,
            t: 3,
            p: 4,
            salt: vec![9; 16],
        };
        let material = AccountMaterial {
            vault_id: "v".into(),
            key_version: 1,
            kdf: kdf.clone(),
            account_public: vec![1],
            secret_by_kek: vec![2],
            secret_by_recovery: vec![3],
            sealed_vault_key: vec![4],
        };
        let reg = RegisterReq {
            invite_token: vec![0xAA; 3],
            username: "alice".into(),
            auth_key: vec![5],
            device: DeviceInfo {
                name: "Mac".into(),
                platform: "macos".into(),
            },
            material,
            recovery_auth_key: vec![6],
            node_hint: 7,
            code_proof: None,
        };
        let v = serde_json::to_value(&reg).unwrap();
        assert_eq!(v["inviteToken"], "qqqq");
        assert_eq!(v["authKey"], "BQ==");
        assert_eq!(v["recoveryAuthKey"], "Bg==");
        assert_eq!(v["nodeHint"], 7);
        assert_eq!(
            v["material"]["kdf"],
            json!({"mKib": 65536, "t": 3, "p": 4, "salt": "CQkJCQkJCQkJCQkJCQkJCQ=="})
        );
        assert_eq!(v["material"]["secretByKek"], "Ag==");
        assert_eq!(serde_json::from_value::<RegisterReq>(v).unwrap(), reg);

        // linkToken is optional and omitted when absent.
        let login = LoginReq {
            username: "u".into(),
            auth_key: vec![1],
            device: DeviceInfo {
                name: "n".into(),
                platform: "p".into(),
            },
            link_token: None,
        };
        assert!(
            serde_json::to_value(&login)
                .unwrap()
                .get("linkToken")
                .is_none()
        );
        let parsed: LoginReq = serde_json::from_value(json!({
            "username": "u", "authKey": "AQ==",
            "device": {"name": "n", "platform": "p"}
        }))
        .unwrap();
        assert_eq!(parsed, login);

        // Rows pushed for the first time carry no rev/seq.
        let item = PushItem {
            row: WireRow {
                rev: None,
                seq: None,
                ..row()
            },
            base_rev: None,
        };
        let v = serde_json::to_value(&item).unwrap();
        assert!(v["row"].get("rev").is_none() && v["baseRev"].is_null());
        assert_eq!(serde_json::from_value::<PushItem>(v).unwrap(), item);

        // Device management bodies.
        assert_eq!(
            serde_json::to_value(LinkResp {
                code: "tether1-x".into(),
                expires_at: 5
            })
            .unwrap(),
            json!({"code": "tether1-x", "expiresAt": 5})
        );
        assert_eq!(
            serde_json::from_value::<RenameDeviceReq>(json!({"name": "Laptop"})).unwrap(),
            RenameDeviceReq {
                name: "Laptop".into()
            }
        );

        // Invalid base64 is a deserialization error, not a panic.
        assert!(serde_json::from_value::<RefreshReq>(json!({"refreshToken": 1})).is_err());
        assert!(
            serde_json::from_value::<PreloginResp>(
                json!({"kdf": {"mKib": 1, "t": 1, "p": 1, "salt": "not base64!"}})
            )
            .is_err()
        );
    }

    #[test]
    fn debug_redacts_secrets() {
        // Distinctive bytes: 0xC4 0xFE repeated, so any leaked decimal, hex or
        // base64 rendering would contain one of these.
        let canary = vec![0xC4, 0xFE, 0xC4, 0xFE];
        let b64 = data_encoding::BASE64.encode(&canary);
        let text = "CANARY-TOKEN-xyz";
        let device = DeviceInfo {
            name: "Mac".into(),
            platform: "macos".into(),
        };
        let material = AccountMaterial {
            vault_id: "v".into(),
            key_version: 1,
            kdf: KdfWire {
                m_kib: 1,
                t: 1,
                p: 1,
                salt: vec![9; 16],
            },
            account_public: vec![1],
            secret_by_kek: canary.clone(),
            secret_by_recovery: canary.clone(),
            sealed_vault_key: canary.clone(),
        };
        let dumps = [
            format!("{material:?}"),
            format!(
                "{:?}",
                RegisterReq {
                    invite_token: canary.clone(),
                    username: "alice".into(),
                    auth_key: canary.clone(),
                    device: device.clone(),
                    material: material.clone(),
                    recovery_auth_key: canary.clone(),
                    node_hint: 1,
                    code_proof: None,
                }
            ),
            format!(
                "{:?}",
                LoginReq {
                    username: "alice".into(),
                    auth_key: canary.clone(),
                    device: device.clone(),
                    link_token: Some(canary.clone()),
                }
            ),
            format!(
                "{:?}",
                RecoverReq {
                    username: "alice".into(),
                    recovery_auth_key: canary.clone(),
                    device,
                }
            ),
            format!(
                "{:?}",
                TokenResp {
                    access_token: text.into(),
                    refresh_token: text.into(),
                    expires_in: 900,
                    device_id: "d".into(),
                    node: 1,
                    epoch: "e".into(),
                    device_secret: text.into(),
                }
            ),
            format!(
                "{:?}",
                RefreshReq {
                    refresh_token: text.into()
                }
            ),
            format!(
                "{:?}",
                PasswordChangeReq {
                    old_auth_key: canary.clone(),
                    new_auth_key: canary.clone(),
                    kdf: material.kdf.clone(),
                    secret_by_kek: canary.clone(),
                    account_rev_base: 1,
                }
            ),
            format!(
                "{:?}",
                RecoveryResetReq {
                    recovery_auth_key: canary.clone(),
                    new_auth_key: canary.clone(),
                    kdf: material.kdf.clone(),
                    secret_by_kek: canary.clone(),
                }
            ),
            format!(
                "{:?}",
                RecoveryUpdateReq {
                    auth_key: canary.clone(),
                    recovery_auth_key: canary.clone(),
                    secret_by_recovery: canary.clone(),
                }
            ),
            format!(
                "{:?}",
                WireRow {
                    envelope: canary.clone(),
                    ..row()
                }
            ),
            // Wrappers that contain the above delegate to them.
            format!(
                "{:?}",
                AccountResp {
                    material: material.clone(),
                    account_rev: 1
                }
            ),
            format!(
                "{:?}",
                PushResp {
                    results: vec![PushResult::Conflict {
                        id: "a".into(),
                        current: WireRow {
                            envelope: canary.clone(),
                            ..row()
                        },
                    }],
                }
            ),
        ];
        for d in &dumps {
            for needle in [text, b64.as_str(), "196", "254", "c4", "C4", "0xc4"] {
                assert!(!d.contains(needle), "{needle:?} leaked in {d}");
            }
        }
        // Non-secret fields stay visible; secrets are marked.
        assert!(dumps[1].contains("alice") && dumps[1].contains("[REDACTED]"));
        assert!(dumps[0].contains("<4 bytes>"));
        assert!(dumps[8].contains("<4 bytes>"), "RecoveryUpdateReq");
        assert!(dumps[9].contains("<4 bytes>"), "WireRow");
    }
}
