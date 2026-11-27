/**
 * Types of the Tauri command layer (src-tauri/src/{commands,vault_cmds,
 * sftp_cmds,forward_cmds,prompt_cmds}.rs). Field names are camelCase, as
 * serde sends them. `bindings.json` lists every DTO's fields; Rust's
 * `cmd_dto_roundtrip` and `contract.test.ts` both check against it, and
 * `DTO_FIELDS` below must name every field of its type (checked by tsc).
 *
 * No output type carries a secret, except the deliberate ones, each shown
 * to the user: `VaultCreated.recoveryKey` and `EnableResult.newRecoveryKey`
 * (once), `KeyExport.privateOpenssh` (explicit export), `LinkCode.code`
 * (single-use add-device code) and `conflicts.reveal` (one secret field the
 * user asked to see).
 */

export type Id = string;

// -- errors -------------------------------------------------------------

export type ErrorCode =
  | "invalid"
  | "notFound"
  | "internal"
  | "io"
  | "referenced"
  | "missingRef"
  | "idConflict"
  | "cycle"
  | "locked"
  | "notInitialized"
  | "alreadyInitialized"
  | "wrongPassword"
  | "wrongRecoveryKey"
  | "quickUnlockNotEnabled"
  | "keychain"
  | "biometryUnavailable"
  | "biometryChanged"
  | "presenceDenied"
  | "corrupt"
  | "storage"
  | "crypto"
  | "vaultUnavailable"
  | "noUsername"
  | "notOpen"
  | "connect"
  | "authFailed"
  | "hostKeyRejected"
  | "hostKeyUnverifiable"
  | "hostKeyEncoding"
  | "channel"
  | "sftp"
  | "closed"
  | "notOwner"
  // sessionTmux: the host's connection is connecting or opening a channel
  | "busy"
  | "needsPassphrase"
  | "badPassphrase"
  | "key"
  | "shell"
  // local files on iOS: a path outside Tether's folder; a command this platform lacks
  | "outsideFolder"
  | "unsupported"
  // sync (Rust: sync_cmds::cmd_error)
  | "certificateChanged"
  | "revoked"
  | "updateRequired"
  | "updateServer"
  | "unreachable"
  | "unauthorized"
  | "invalidCode"
  | "invalidAddress"
  | "protocol"
  | "serverUnverified"
  | "weakKdf"
  | "clockSkew"
  | "rateLimited"
  | "usernameTaken"
  | "conflict"
  | "forbidden"
  | "vaultExists"
  | "server"
  | "syncOff"
  | "syncOn"
  | "syncUnavailable"
  | "needsDeviceCode";

/** What every command rejects with. `refs`: ids involved (for `referenced`, the records still using the target). */
export interface ApiError {
  code: ErrorCode;
  message: string;
  refs: string[];
}

export function isApiError(e: unknown): e is ApiError {
  return (
    typeof e === "object" &&
    e !== null &&
    typeof (e as ApiError).code === "string" &&
    typeof (e as ApiError).message === "string" &&
    Array.isArray((e as ApiError).refs)
  );
}

// -- vault --------------------------------------------------------------

export type VaultState = "uninitialized" | "locked" | "unlocked" | "unavailable";

/** What quick unlock asks for: nothing extra, Touch ID (macOS, iOS), Face ID (iOS) or Windows Hello. */
export type QuickUnlockKind = "none" | "touchId" | "faceId" | "windowsHello" | "androidBiometric";

export interface VaultStatus {
  state: VaultState;
  quickUnlockEnabled: boolean;
  quickUnlockKind: QuickUnlockKind;
  /** Why the vault is unavailable (it failed to open at startup). */
  error: string | null;
}

export interface VaultCreated {
  /** Shown to the user once; stored nowhere. */
  recoveryKey: string;
}

/** Why the vault locked (the `vault-locked` event). */
export type LockReason = "manual" | "passwordChanged" | "idle";

export interface VaultLocked {
  /**
   * `passwordChanged`: another device changed the master password; unlock with the new one.
   * `idle`: auto-lock (Tether was away from the front for the chosen time).
   */
  reason: LockReason;
}

/**
 * `null` (or omitted) inherits from the level above. Applied to
 * connections: port, identityId, keepaliveSecs (0 = off), termType and env
 * (the server may refuse names its AcceptEnv does not allow). `charset` is
 * informational only: terminals are always UTF-8. colorScheme, fontFamily
 * and fontSize are for the terminal view. Unknown fields are refused.
 */
export interface ConnSettings {
  port: number | null;
  identityId: Id | null;
  keepaliveSecs: number | null;
  termType: string | null;
  charset: string | null;
  colorScheme: string | null;
  fontFamily: string | null;
  fontSize: number | null;
  env: [string, string][] | null;
}

export interface Group {
  id: Id;
  parentId: Id | null;
  name: string;
  settings: ConnSettings;
}

export interface Host {
  id: Id;
  groupId: Id | null;
  label: string;
  address: string;
  tagIds: Id[];
  settings: ConnSettings;
  /** Derived by the vault from the host's forward rules. */
  forwardRuleIds: Id[];
}

export interface Tag {
  id: Id;
  name: string;
  color: string;
}

export interface Identity {
  id: Id;
  label: string;
  username: string;
  hasPassword: boolean;
  keyId: Id | null;
  /** The host or group whose own login this is (hidden from the Keychain); `null` for a Keychain identity. */
  owner: Id | null;
}

/** A host's or group's own login. `password`: omitted keeps it, `null` clears it, a string sets it. */
export interface OwnLoginInput {
  ownerId: Id;
  username: string;
  password?: string | null;
}

export interface SshKey {
  id: Id;
  label: string;
  algorithm: string;
  publicOpenssh: string;
  fingerprint: string;
  hasPassphrase: boolean;
  certificate: string | null;
}

export type KnownHostSource = "prompt" | "import";

export interface KnownHost {
  id: Id;
  hostPattern: string;
  port: number;
  algorithm: string;
  publicKey: string;
  /** Unix seconds. */
  addedAt: number;
  source: KnownHostSource;
  fingerprint: string;
}

export type ForwardKind = "local" | "remote" | "dynamic";

export interface ForwardRule {
  id: Id;
  hostId: Id;
  label: string;
  kind: ForwardKind;
  bindAddr: string;
  bindPort: number;
  destHost: string | null;
  destPort: number | null;
  autostart: boolean;
}

/** A saved command for the terminal. `run`: press Enter after pasting it. */
export interface Snippet {
  id: Id;
  label: string;
  text: string;
  run: boolean;
}

/** Record kinds as the vault names them. */
export type EntityKind =
  | "group"
  | "host"
  | "tag"
  | "identity"
  | "ssh_key"
  | "known_host"
  | "forward_rule"
  | "snippet";

export interface EntityMap {
  group: Group;
  host: Host;
  tag: Tag;
  identity: Identity;
  ssh_key: SshKey;
  known_host: KnownHost;
  forward_rule: ForwardRule;
  snippet: Snippet;
}

export type EntityOf<K extends EntityKind> = EntityMap[K];

/** Creating or updating an identity. `password`: omitted keeps it, `null` clears it, a string sets it. */
export interface IdentityInput {
  id?: Id;
  label: string;
  username: string;
  password?: string | null;
  keyId?: Id | null;
}

/** Keys are created with keyGenerate/keyImport; only the label is editable. */
export interface SshKeyInput {
  id: Id;
  label: string;
}

type Optional<T, K extends keyof T> = Omit<T, K> & Partial<Pick<T, K>>;

export interface EntityInputMap {
  group: Optional<Omit<Group, "settings">, "id" | "parentId"> & {
    settings?: Partial<ConnSettings>;
  };
  host: Optional<Omit<Host, "settings">, "id" | "groupId" | "tagIds" | "forwardRuleIds"> & {
    settings?: Partial<ConnSettings>;
  };
  tag: Optional<Tag, "id">;
  identity: IdentityInput;
  ssh_key: SshKeyInput;
  known_host: Optional<Omit<KnownHost, "fingerprint">, "id" | "addedAt" | "source">;
  forward_rule: Optional<ForwardRule, "id" | "destHost" | "destPort" | "autostart">;
  snippet: Optional<Snippet, "id" | "run">;
}

export type EntityInputOf<K extends EntityKind> = EntityInputMap[K];

export type ChildPolicy = "moveToParent" | "deleteRecursive";

export type Source = { kind: "default" } | { kind: "group"; groupId: Id } | { kind: "host" };

export interface Resolved<T> {
  value: T;
  source: Source;
}

export interface EffectiveSettings {
  port: Resolved<number>;
  identityId: Resolved<Id | null>;
  keepaliveSecs: Resolved<number | null>;
  termType: Resolved<string | null>;
  charset: Resolved<string | null>;
  colorScheme: Resolved<string | null>;
  fontFamily: Resolved<string | null>;
  fontSize: Resolved<number | null>;
  env: Resolved<[string, string][] | null>;
}

export interface KeyFailure {
  path: string;
  needsPassphrase: boolean;
  message: string;
}

export interface SkippedEntry {
  name: string;
  reason: string;
}

export interface ImportReport {
  hostsAdded: number;
  identitiesAdded: number;
  keysAdded: number;
  keyFailures: KeyFailure[];
  skipped: SkippedEntry[];
}

export interface KnownHostsImportReport {
  added: number;
  alreadyKnown: number;
}

export type KeyAlgo = "ed25519" | "ecdsaP256" | "ecdsaP384" | "ecdsaP521" | "rsa2048" | "rsa4096";

/** The one deliberate private-key egress. */
export interface KeyExport {
  privateOpenssh: string;
}

// -- sessions -----------------------------------------------------------

/**
 * What to open a session on. `reattachTmux` (a reconnecting host tab that was
 * in that tmux session) attaches the new shell to it, or gives the login
 * shell when it is gone.
 */
export type SessionTarget =
  | { kind: "host"; hostId: Id; reattachTmux?: string | null }
  | { kind: "local"; cwd?: string | null; shellId?: string | null };

/** A shell a local terminal can run. Which program it starts stays in the backend. */
export interface LocalShell {
  id: string;
  label: string;
}

export interface TermSize {
  cols: number;
  rows: number;
}

export interface OpenedSession {
  sessionId: string;
}

/**
 * A step of connecting a session, in order. A new SSH connection reports all
 * four; a pooled connection or a local shell only `shell`.
 */
export type ConnectStep = "tcp" | "hostKey" | "auth" | "shell";

export interface SessionHandlers {
  /** A batch of raw terminal output; ack it once written (`ack`). */
  onOutput(bytes: Uint8Array): void;
  /**
   * The session ended. Called once. `reason` is null for a normal end (the
   * remote shell exited, with `exitStatus` when the server reported one, or
   * the session was closed); a string means the connection was lost.
   */
  onClosed(reason: string | null, exitStatus: number | null): void;
  /**
   * The connect step now running. Called before `openSession` resolves; a
   * failed open leaves it on the step that failed. A reconnect after a dead
   * pooled connection repeats steps (`shell`, then `tcp` again, ...).
   */
  onProgress?(step: ConnectStep): void;
}

/** What the backend sends on a session's closed channel (see `SessionHandlers.onClosed`). */
export interface SessionClosed {
  reason: string | null;
  exitStatus: number | null;
}

/** DEV connect form only: direct parameters, one connection per shell. */
export interface ConnectParams {
  host: string;
  port: number;
  username: string;
  password?: string | null;
  privateKeyPem?: string | null;
  passphrase?: string | null;
  cols: number;
  rows: number;
}

export interface ActiveSession {
  id: string;
  title: string;
  hostId: Id | null;
}

export type Direction = "upload" | "download";

export interface ActiveTransfer {
  jobId: string;
  hostId: Id;
  direction: Direction;
  local: string;
  remote: string;
}

/** The calling window's live sessions and running transfers, and the app's running forwards (quit confirmation). */
export interface ActiveWork {
  sessions: ActiveSession[];
  transfers: ActiveTransfer[];
  /** Starting or active port forwards (app-wide). */
  forwards: ForwardStatusEntry[];
}

// -- sftp & transfers ----------------------------------------------------

export interface RemoteEntry {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  /** Unix seconds; 0 if unknown. */
  mtime: number;
  /** st_mode (type and permission bits); 0 if unknown. */
  mode: number;
  isSymlink: boolean;
}

/** An entry of a local directory (`local_list`). For a symlink, `isDir`/`size`/`mode` are its target's. */
export interface LocalEntry {
  name: string;
  /** The listed directory joined with `name`. */
  path: string;
  isDir: boolean;
  size: number;
  /** Unix seconds; 0 if unknown. */
  mtime: number;
  /** st_mode (type and permission bits); null where the platform has none. */
  mode: number | null;
  isSymlink: boolean;
}

export type ConflictPolicy = "overwrite" | "skip" | "rename" | "resume" | "ask";

export interface TransferJob {
  direction: Direction;
  local: string;
  remote: string;
  policy: ConflictPolicy;
}

export interface EnqueuedTransfer {
  jobId: string;
}

export type TransferEvent =
  | { kind: "progress"; job: string; bytes: number; total: number }
  | { kind: "done"; job: string; dest: string }
  | { kind: "skipped"; job: string }
  | { kind: "cancelled"; job: string }
  | { kind: "failed"; job: string; error: string };

export interface TransferEventPayload {
  hostId: Id;
  event: TransferEvent;
}

// -- forwards -----------------------------------------------------------

export type ForwardStatus =
  | { state: "starting" }
  | { state: "active"; detail: { bound: string } }
  | { state: "error"; detail: string }
  | { state: "stopped" };

export interface ForwardEvent {
  ruleId: Id;
  status: ForwardStatus;
}

export interface ForwardStatusEntry {
  ruleId: Id;
  hostId: Id;
  status: ForwardStatus;
}

// -- background keep-alive (iOS) ------------------------------------------

/** Keep-alive: on, location permission given, and location updates running now (on, permitted, a session or forward open). */
export interface KeepAliveStatus {
  enabled: boolean;
  authorized: boolean;
  running: boolean;
}

// -- prompts ------------------------------------------------------------

export interface HostKeyInfo {
  algorithm: string;
  sha256Fingerprint: string;
  openssh: string;
}

/** Where a password typed at a prompt would be saved: an identity, or a new one of that name. */
export interface SaveTarget {
  identityLabel: string;
  isNew: boolean;
}

export type Prompt =
  | { kind: "hostKeyUnknown"; host: string; port: number; key: HostKeyInfo }
  | { kind: "hostKeyChanged"; host: string; port: number; old: HostKeyInfo; new: HostKeyInfo }
  | { kind: "passphrase"; keyLabel: string }
  | {
      kind: "keyboardInteractive";
      name: string;
      instructions: string;
      /** `[text, echo]` pairs. */
      prompts: [string, boolean][];
      /** Set when the (single, hidden) answer may be saved to the vault. */
      saveTo: SaveTarget | null;
    }
  | { kind: "password"; user: string; host: string; saveTo: SaveTarget | null }
  | {
      kind: "transferConflict";
      jobId: string;
      path: string;
      existingSize: number;
      incomingSize: number;
    };

export interface PromptEvent {
  id: string;
  prompt: Prompt;
}

export interface PromptWithdrawn {
  id: string;
}

export type PromptResponse =
  | { accept: { save: boolean } }
  | "reject"
  | { secret: string }
  /** A password the user asked to save (prompts with `saveTo`). */
  | { saveSecret: string }
  | { answers: string[] }
  | { saveAnswers: string[] }
  | { conflict: ConflictPolicy };

/** Options of the native "open file" dialog. */
export interface PickFileOptions {
  title?: string;
  filters?: { name: string; extensions: string[] }[];
}

// -- sync ---------------------------------------------------------------

/** Where this device syncs to and as whom. */
export interface SyncAccount {
  server: string;
  username: string;
  deviceId: Id;
}

export type SyncErrorKind =
  | "certificateChanged"
  | "revoked"
  | "updateClient"
  | "updateServer"
  | "clockSkew"
  | "signInAgain"
  | "rateLimited"
  | "storage"
  | "vault"
  | "server"
  | "partial";

/** The engine's state. `message` is a plain sentence ("server certificate changed", "device revoked", ...). */
export type SyncState =
  | { state: "off" }
  /** Up to date as of `lastSync` (Unix ms; 0 before the first sync). */
  | { state: "idle"; lastSync: number }
  | { state: "syncing"; done: number; total: number | null }
  /** The server cannot be reached; retrying. */
  | { state: "offline"; reason: string }
  | { state: "error"; kind: SyncErrorKind; message: string };

/** `sync.status()` and the `sync-status` event. `account` is null while sync is off. */
export interface SyncStatus {
  account: SyncAccount | null;
  status: SyncState;
}

/** What a pasted code is, to show before the user confirms. */
export interface CodeInfo {
  /**
   * `invite` creates an account; `link` is an add-device code; `short` is a short code (setup,
   * invite or add-device), which names no server: `server` is "" and `fingerprint` null until
   * `checkServer` verifies the typed address.
   */
  kind: "invite" | "link" | "short";
  server: string;
  /**
   * Character for character what `termlyx-server fingerprint` prints: `sha256:` and 64 lowercase
   * hex digits, no spaces. Show it as is; the user compares the two. Null when the code carries
   * no pin: plain-HTTP loopback servers, or a certificate that passed the public-root check.
   */
  fingerprint: string | null;
}

/** The recovery key step of turning sync on: the existing key's text, or "I lost it". */
export type RecoveryChoice = { existing: string } | { new: true };

export interface EnableResult {
  /** The new recovery key when "I lost it" made one: show it once. */
  newRecoveryKey: string | null;
}

/**
 * What `sync.enable` rejects with: an `ApiError`, plus the new recovery key when the failure came
 * after the server took it ("I lost it"). It is then the only key that works: show it once, as
 * after a success.
 */
export interface EnableError extends ApiError {
  newRecoveryKey: string | null;
}

/** The new recovery key a failed `sync.enable` carries, if any. */
export function recoveryKeyOf(e: unknown): string | null {
  if (!isApiError(e)) return null;
  const key = (e as Partial<EnableError>).newRecoveryKey;
  return typeof key === "string" ? key : null;
}

export interface JoinResult {
  /** Where the replaced vault was kept; null when no vault was replaced. */
  backup: string | null;
  /** This device's own vault was reconnected to sync (it is the synced vault): its records stayed. */
  reconnected: boolean;
}

/** The account has a new password; nothing was written here. The device needs an add-device code to join. */
export interface RecoverResult {
  needsDeviceCode: boolean;
  server: string;
}

export interface LinkCode {
  /** The add-device code (single use). */
  code: string;
  /** The server address the code carries (`https://host:port`). */
  server: string;
  /** Unix ms. */
  expiresAt: number;
}

export interface Device {
  id: string;
  name: string;
  platform: string;
  /** Unix ms. */
  createdAt: number;
  /** Unix ms. */
  lastSeen: number;
  /** This device. */
  current: boolean;
}

/**
 * Two versions of one record. `current` and `other` are the record in the vault's own JSON field
 * names (snake_case: `group_id`, `key_id`, ...), the names `changedFields` and a `{fields}`
 * resolution use. Secret fields read `{hasSecret: true}` (null when unset); a side is null when it
 * is a deletion (or the record is gone).
 */
export interface Conflict {
  id: Id;
  recordId: Id;
  kind: EntityKind;
  current: Record<string, unknown> | null;
  other: Record<string, unknown> | null;
  changedFields: string[];
  /** Unix seconds. */
  detectedAt: number;
}

export type ConflictSide = "current" | "other";

/** Keep the current version, take the other one, or take these fields from the other. */
export type Resolution = { keepCurrent: true } | { useOther: true } | { fields: string[] };

export interface SyncApi {
  status(): Promise<SyncStatus>;
  /** Rejects with `invalidCode` (or `updateRequired` for a code from a newer app). */
  parseCode(code: string): Promise<CodeInfo>;
  /**
   * Checks that the server at the typed `address` knows the short `code`, before anything secret
   * goes to it. Resolves `kind: "invite"` with the normalized `server` and its `fingerprint` (or
   * null). Rejects with `invalidAddress`, `unreachable`, `protocol` (not a Termlyx server),
   * `invalidCode`, `serverUnverified`, `certificateChanged` or `updateServer`.
   */
  checkServer(address: string, code: string): Promise<CodeInfo>;
  /**
   * Turns sync on for the unlocked vault with an invite code; uploads in the background
   * (`sync-status`). Rejects with an `EnableError`: show its `newRecoveryKey` when it has one.
   */
  enable(
    code: string,
    username: string,
    password: string,
    recovery: RecoveryChoice,
    /** With a short code: the server address typed next to it. */
    address?: string,
  ): Promise<EnableResult>;
  /**
   * Joins a synced vault with an add-device code. When this device's vault is that vault it is
   * reconnected (records kept). Another vault: `vaultExists` unless `replaceExisting`; the first
   * try spends the code, and a retry with the same code after the warning reuses its sign-in.
   */
  join(
    code: string,
    username: string,
    password: string,
    replaceExisting: boolean,
  ): Promise<JoinResult>;
  /** "Forgot password" without a vault here: sets a new account password; then `join` with an add-device code. */
  recover(
    code: string,
    username: string,
    recoveryKey: string,
    newPassword: string,
  ): Promise<RecoverResult>;
  /** Starts a sync and returns at once; watch `onStatus`. */
  now(): Promise<void>;
  /**
   * Signs this device in again with the master password after sync stopped with `signInAgain`
   * (the session is gone, e.g. the server was restored from an older backup); then it syncs.
   * `wrongPassword`; `revoked` or `needsDeviceCode` when only an add-device code can bring it back.
   */
  signIn(password: string): Promise<void>;
  turnOff(): Promise<void>;
  /** With `address` (`host[:port]`) the code names that address instead of the one in use. Rejects `invalidAddress`. */
  linkCode(address?: string): Promise<LinkCode>;
  devices(): Promise<Device[]>;
  renameDevice(id: string, name: string): Promise<void>;
  revokeDevice(id: string): Promise<void>;
  onStatus(cb: (e: SyncStatus) => void): Promise<Unlisten>;
}

export interface ConflictsApi {
  list(): Promise<Conflict[]>;
  resolve(id: Id, resolution: Resolution): Promise<void>;
  /** The value of one secret field (`password`, `private_openssh`, `passphrase`) of one version. */
  reveal(id: Id, side: ConflictSide, field: string): Promise<string>;
}

export interface VaultApi {
  /** Works with sync on or off. With sync on the other devices lock and ask for the new password. */
  changePassword(oldPassword: string, newPassword: string): Promise<void>;
}

// -- the API --------------------------------------------------------------

export type Unlisten = () => void;

/** A request to close the window; `preventDefault` keeps it open. */
export interface CloseRequest {
  preventDefault(): void;
}

/** A newer release than the installed one, from the update manifest. */
export interface UpdateInfo {
  version: string;
  /** Release notes (plain text). */
  notes: string;
  /** Publication time (RFC 3339), if the manifest has one. */
  date: string | null;
}

/** Everything the UI can ask of the backend. Implemented by `index.ts` (Tauri) and `mock.ts` (tests). */
export interface TetherApi {
  app: {
    /** The installed app's version (`X.Y.Z`). */
    version(): Promise<string>;
  };
  /** Signed self-update (desktop only; the updater and process plugins). */
  updates: {
    /** False on iOS and Android: the other two then must not be called. */
    supported: boolean;
    /** The newer release, or `null` when this one is the latest. Rejects when the manifest cannot be read. */
    check(): Promise<UpdateInfo | null>;
    /** Downloads, verifies and installs the release the last `check` found, then relaunches. */
    install(onProgress?: (pct: number) => void): Promise<void>;
  };
  vaultStatus(): Promise<VaultStatus>;
  vaultCreate(password: string): Promise<VaultCreated>;
  vaultUnlock(password: string): Promise<void>;
  vaultUnlockRecovery(recoveryKey: string, newPassword: string): Promise<void>;
  vaultLock(): Promise<void>;
  vaultEnableQuickUnlock(): Promise<void>;
  vaultDisableQuickUnlock(): Promise<void>;
  vaultQuickUnlock(): Promise<void>;
  /** Auto-lock after this many minutes away from the front; `null`: never. Sent at start-up and on change. */
  setAutoLock(minutes: number | null): Promise<void>;
  /**
   * The page went to the background (`false`) or came back (`true`). Answers
   * whether the vault is locked, after any auto-lock the return called for:
   * the UI waits for it before resuming anything.
   */
  appFront(front: boolean): Promise<boolean>;
  /**
   * iOS, at start-up: what this device is called in the sync device list when
   * its host name says nothing (`gethostname` is generic there).
   */
  setDeviceFallback(kind: "iPhone" | "iPad"): Promise<void>;

  listEntities<K extends EntityKind>(kind: K): Promise<EntityOf<K>[]>;
  upsertEntity<K extends EntityKind>(kind: K, input: EntityInputOf<K>): Promise<EntityOf<K>>;
  deleteEntity(kind: EntityKind, id: Id): Promise<void>;
  deleteGroup(id: Id, policy: ChildPolicy): Promise<void>;
  /** Sets the own login of a host or group and links it there; returns the identity. */
  setOwnLogin(login: OwnLoginInput): Promise<Identity>;
  effectiveSettings(hostId: Id): Promise<EffectiveSettings>;
  importSshConfig(path: string): Promise<ImportReport>;
  keyGenerate(algo: KeyAlgo, label: string, passphrase?: string): Promise<SshKey>;
  keyImport(text: string, passphrase?: string, label?: string): Promise<SshKey>;
  /**
   * The private key, after the master password (`wrongPassword` otherwise).
   * Mobile only: the desktop saves it with `keySavePrivate`, so it never
   * reaches the page.
   */
  keyExportPrivate(id: Id, password: string): Promise<KeyExport>;
  /**
   * After the master password, the native save dialog, and the backend
   * writes the private key there (owner-only). The path saved to, or `null`
   * when the dialog was cancelled. Desktop.
   */
  keySavePrivate(id: Id, password: string): Promise<string | null>;
  knownHostsImport(path: string): Promise<KnownHostsImportReport>;
  /** The native "open file" dialog; the chosen path, or `null` when cancelled. */
  pickFile(options?: PickFileOptions): Promise<string | null>;
  /**
   * iOS only: opens the camera to scan one QR code and resolves with its text, or `null` when
   * the user cancelled or the camera is not allowed. The text is a secret (a join code): hand
   * it on and keep it nowhere.
   */
  scanQr(): Promise<string | null>;
  /** Closes the camera of a running `scanQr` (which then resolves `null`). */
  cancelScan(): Promise<void>;
  /**
   * The clipboard's text for the iOS terminal's Paste (UIPasteboard: iOS asks with its own
   * paste prompt), or `null` when it is empty, not text, or refused. The text may be a secret:
   * hand it to the terminal and keep it nowhere. (Desktop copy/paste keys use the browser's
   * clipboard directly.)
   */
  readClipboard(): Promise<string | null>;
  /** Puts `text` on the clipboard (the iOS terminal's Copy); false when that failed. */
  writeClipboard(text: string): Promise<boolean>;

  openSession(target: SessionTarget, size: TermSize, handlers: SessionHandlers): Promise<OpenedSession>;
  /** DEV connect form only; the backend has it only when built with `--features dev-connect`. */
  openShell(params: ConnectParams, handlers: SessionHandlers): Promise<OpenedSession>;
  /** Keystrokes, sent as a raw binary body. */
  write(sessionId: string, data: Uint8Array): Promise<void>;
  resize(sessionId: string, cols: number, rows: number): Promise<void>;
  ack(sessionId: string, bytes: number): Promise<void>;
  close(sessionId: string): Promise<void>;
  /** Closes every session this window opened (call on load); returns how many. */
  closeAllSessions(): Promise<number>;
  activeWork(): Promise<ActiveWork>;
  /**
   * The tmux session a host session's shell is attached to (`null`: none, or
   * not a host session). Rejects when it cannot be told now (the session or
   * its connection gone): that is not "no tmux".
   */
  sessionTmux(sessionId: string): Promise<string | null>;
  /** Shells a local terminal can run, default first (several on Windows, one elsewhere). */
  localShells(): Promise<LocalShell[]>;

  sftpOpen(hostId: Id): Promise<void>;
  sftpClose(hostId: Id): Promise<void>;
  sftpList(hostId: Id, path: string): Promise<RemoteEntry[]>;
  sftpMkdir(hostId: Id, path: string): Promise<void>;
  sftpRename(hostId: Id, from: string, to: string): Promise<void>;
  sftpRemove(hostId: Id, path: string, isDir: boolean): Promise<void>;
  sftpChmod(hostId: Id, path: string, mode: number): Promise<void>;
  /** Lists a local directory (`~` expands to the home directory): directories first, then by name. */
  localList(path: string): Promise<LocalEntry[]>;
  /** `path` with a leading `~` expanded to the home directory (nothing else is touched or checked). */
  localResolve(path: string): Promise<string>;
  /** The server's absolute form of `path`; `.` is the home directory, relative paths resolve against it. */
  sftpRealpath(hostId: Id, path: string): Promise<string>;
  /** Creates a local directory and its parents; an existing one is fine. */
  localMkdir(path: string): Promise<void>;
  /** The roots of the local file systems: drive roots (`C:\\`) on Windows, `["/"]` elsewhere. */
  localDrives(): Promise<string[]>;
  /**
   * iOS: moves a file the file picker produced (`pickFile`: a `file://` URL
   * or a path) into Tether's folder, the local pane's root, and returns its
   * new path. A name already there gets a number (`name 2.ext`).
   */
  localImport(path: string): Promise<string>;
  /**
   * Android: the system file picker (several files); each is copied into
   * `dir`, a folder inside Tether's folder. Resolves the names the copies got
   * (a taken name becomes `name (2).ext`); empty when the picker was cancelled.
   */
  importFromDevice(dir: string): Promise<string[]>;
  /**
   * Android: the system "save as" picker; the file `path` (inside Tether's
   * folder) is written to the place chosen. False when the picker was cancelled.
   */
  saveToDevice(path: string): Promise<boolean>;
  transferEnqueue(hostId: Id, job: TransferJob): Promise<EnqueuedTransfer>;
  transferCancel(jobId: string): Promise<boolean>;

  forwardStart(ruleId: Id): Promise<ForwardStatus>;
  forwardStop(ruleId: Id): Promise<void>;
  forwardStatuses(): Promise<ForwardStatusEntry[]>;

  /**
   * Background keep-alive (iOS; elsewhere turning it on fails `unsupported`).
   * On asks for location permission first if it was never asked, and answers
   * once the user has: without permission it stays off (`enabled: false`).
   */
  keepAliveSet(enabled: boolean): Promise<KeepAliveStatus>;
  keepAliveStatus(): Promise<KeepAliveStatus>;
  /**
   * Keep-alive's status changed (the `keepalive` event): turned on or off,
   * the permission given, withdrawn or lapsed ("Allow Once" ended, which
   * also turns it off), updates started or stopped.
   */
  onKeepAlive(cb: (e: KeepAliveStatus) => void): Promise<Unlisten>;

  promptRespond(id: string, response: PromptResponse): Promise<void>;

  onPrompt(cb: (e: PromptEvent) => void): Promise<Unlisten>;
  onPromptWithdrawn(cb: (e: PromptWithdrawn) => void): Promise<Unlisten>;
  onTransfer(cb: (e: TransferEventPayload) => void): Promise<Unlisten>;
  onForward(cb: (e: ForwardEvent) => void): Promise<Unlisten>;
  onVaultLocked(cb: (e: VaultLocked) => void): Promise<Unlisten>;
  /**
   * Sync changed records in the vault (the `vault-changed` event, no
   * payload): lists that show vault data should read again. Use
   * `useVaultChanged` rather than subscribing directly.
   */
  onVaultChanged(cb: () => void): Promise<Unlisten>;

  sync: SyncApi;
  conflicts: ConflictsApi;
  vault: VaultApi;

  /**
   * The user asked to close this window. The handler is awaited; the window
   * closes afterwards unless it called `preventDefault` (then close it with
   * `destroyWindow`).
   */
  onCloseRequested(cb: (e: CloseRequest) => void | Promise<void>): Promise<Unlisten>;
  /** Closes this window without asking again. */
  destroyWindow(): Promise<void>;

  /**
   * Android's system Back (button or gesture). Once a handler is registered the
   * app no longer closes on Back: the handler decides. Android only.
   */
  onBackButton(cb: () => void): Promise<Unlisten>;
  /** Sends the app to the background, as Home does (Android; sessions keep running). */
  moveToBackground(): Promise<void>;
}

/** Event names (Rust: state::events). */
export const EVENTS = {
  prompt: "prompt",
  promptWithdrawn: "prompt-withdrawn",
  transfer: "transfer",
  forward: "forward",
  vaultLocked: "vault-locked",
  vaultChanged: "vault-changed",
  syncStatus: "sync-status",
  keepAlive: "keepalive",
} as const;

/** Header carrying the session id of a `write_shell` call (Rust: commands::SESSION_ID_HEADER). */
export const SESSION_ID_HEADER = "tether-session-id";

// -- field lists (checked against bindings.json) ---------------------------

type Missing<T, K extends readonly PropertyKey[]> = Exclude<keyof T, K[number]>;

/** `fields<T>()("a", "b")`: tsc rejects a list that misses or invents a field of T. */
function fields<T>() {
  return <const K extends readonly (keyof T)[]>(
    ...keys: K & ([Missing<T, K>] extends [never] ? unknown : { missing: Missing<T, K> })
  ): readonly string[] => keys as readonly string[];
}

type Variant<U, Tag extends string, V> = Extract<U, Record<Tag, V>>;
/** The member of a union that has key `K`. */
type WithKey<U, K extends string> = Extract<U, Record<K, unknown>>;
type ActiveStatus = Variant<ForwardStatus, "state", "active">;

export const DTO_FIELDS: Record<string, readonly string[]> = {
  VaultStatus: fields<VaultStatus>()("state", "quickUnlockEnabled", "quickUnlockKind", "error"),
  VaultCreated: fields<VaultCreated>()("recoveryKey"),
  VaultLocked: fields<VaultLocked>()("reason"),
  ConnSettings: fields<ConnSettings>()(
    "port",
    "identityId",
    "keepaliveSecs",
    "termType",
    "charset",
    "colorScheme",
    "fontFamily",
    "fontSize",
    "env",
  ),
  Group: fields<Group>()("id", "parentId", "name", "settings"),
  Host: fields<Host>()("id", "groupId", "label", "address", "tagIds", "settings", "forwardRuleIds"),
  Tag: fields<Tag>()("id", "name", "color"),
  Snippet: fields<Snippet>()("id", "label", "text", "run"),
  Identity: fields<Identity>()("id", "label", "username", "hasPassword", "keyId", "owner"),
  OwnLoginInput: fields<OwnLoginInput>()("ownerId", "username", "password"),
  IdentityInput: fields<IdentityInput>()("id", "label", "username", "password", "keyId"),
  SshKey: fields<SshKey>()(
    "id",
    "label",
    "algorithm",
    "publicOpenssh",
    "fingerprint",
    "hasPassphrase",
    "certificate",
  ),
  SshKeyInput: fields<SshKeyInput>()("id", "label"),
  KnownHost: fields<KnownHost>()(
    "id",
    "hostPattern",
    "port",
    "algorithm",
    "publicKey",
    "addedAt",
    "source",
    "fingerprint",
  ),
  ForwardRule: fields<ForwardRule>()(
    "id",
    "hostId",
    "label",
    "kind",
    "bindAddr",
    "bindPort",
    "destHost",
    "destPort",
    "autostart",
  ),
  EffectiveSettings: fields<EffectiveSettings>()(
    "port",
    "identityId",
    "keepaliveSecs",
    "termType",
    "charset",
    "colorScheme",
    "fontFamily",
    "fontSize",
    "env",
  ),
  Resolved: fields<Resolved<unknown>>()("value", "source"),
  "Source.default": fields<Variant<Source, "kind", "default">>()("kind"),
  "Source.group": fields<Variant<Source, "kind", "group">>()("kind", "groupId"),
  "Source.host": fields<Variant<Source, "kind", "host">>()("kind"),
  ImportReport: fields<ImportReport>()(
    "hostsAdded",
    "identitiesAdded",
    "keysAdded",
    "keyFailures",
    "skipped",
  ),
  KeyFailure: fields<KeyFailure>()("path", "needsPassphrase", "message"),
  SkippedEntry: fields<SkippedEntry>()("name", "reason"),
  KnownHostsImportReport: fields<KnownHostsImportReport>()("added", "alreadyKnown"),
  KeyExport: fields<KeyExport>()("privateOpenssh"),
  OpenedSession: fields<OpenedSession>()("sessionId"),
  SessionClosed: fields<SessionClosed>()("reason", "exitStatus"),
  "SessionTarget.host": fields<Variant<SessionTarget, "kind", "host">>()("kind", "hostId", "reattachTmux"),
  "SessionTarget.local": fields<Variant<SessionTarget, "kind", "local">>()("kind", "cwd", "shellId"),
  ConnectParams: fields<ConnectParams>()(
    "host",
    "port",
    "username",
    "password",
    "privateKeyPem",
    "passphrase",
    "cols",
    "rows",
  ),
  ActiveWork: fields<ActiveWork>()("sessions", "transfers", "forwards"),
  ActiveSession: fields<ActiveSession>()("id", "title", "hostId"),
  LocalShell: fields<LocalShell>()("id", "label"),
  ActiveTransfer: fields<ActiveTransfer>()("jobId", "hostId", "direction", "local", "remote"),
  RemoteEntry: fields<RemoteEntry>()("name", "path", "isDir", "size", "mtime", "mode", "isSymlink"),
  LocalEntry: fields<LocalEntry>()("name", "path", "isDir", "size", "mtime", "mode", "isSymlink"),
  TransferJob: fields<TransferJob>()("direction", "local", "remote", "policy"),
  EnqueuedTransfer: fields<EnqueuedTransfer>()("jobId"),
  TransferEventPayload: fields<TransferEventPayload>()("hostId", "event"),
  "TransferEvent.progress": fields<Variant<TransferEvent, "kind", "progress">>()(
    "kind",
    "job",
    "bytes",
    "total",
  ),
  "TransferEvent.done": fields<Variant<TransferEvent, "kind", "done">>()("kind", "job", "dest"),
  "TransferEvent.skipped": fields<Variant<TransferEvent, "kind", "skipped">>()("kind", "job"),
  "TransferEvent.cancelled": fields<Variant<TransferEvent, "kind", "cancelled">>()("kind", "job"),
  "TransferEvent.failed": fields<Variant<TransferEvent, "kind", "failed">>()("kind", "job", "error"),
  ForwardEvent: fields<ForwardEvent>()("ruleId", "status"),
  ForwardStatusEntry: fields<ForwardStatusEntry>()("ruleId", "hostId", "status"),
  KeepAliveStatus: fields<KeepAliveStatus>()("enabled", "authorized", "running"),
  "ForwardStatus.starting": fields<Variant<ForwardStatus, "state", "starting">>()("state"),
  "ForwardStatus.active": fields<ActiveStatus>()("state", "detail"),
  "ForwardStatus.active.detail": fields<ActiveStatus["detail"]>()("bound"),
  "ForwardStatus.error": fields<Variant<ForwardStatus, "state", "error">>()("state", "detail"),
  "ForwardStatus.stopped": fields<Variant<ForwardStatus, "state", "stopped">>()("state"),
  PromptEvent: fields<PromptEvent>()("id", "prompt"),
  PromptWithdrawn: fields<PromptWithdrawn>()("id"),
  "Prompt.hostKeyUnknown": fields<Variant<Prompt, "kind", "hostKeyUnknown">>()(
    "kind",
    "host",
    "port",
    "key",
  ),
  "Prompt.hostKeyChanged": fields<Variant<Prompt, "kind", "hostKeyChanged">>()(
    "kind",
    "host",
    "port",
    "old",
    "new",
  ),
  "Prompt.passphrase": fields<Variant<Prompt, "kind", "passphrase">>()("kind", "keyLabel"),
  "Prompt.keyboardInteractive": fields<Variant<Prompt, "kind", "keyboardInteractive">>()(
    "kind",
    "name",
    "instructions",
    "prompts",
    "saveTo",
  ),
  "Prompt.password": fields<Variant<Prompt, "kind", "password">>()("kind", "user", "host", "saveTo"),
  SaveTarget: fields<SaveTarget>()("identityLabel", "isNew"),
  "Prompt.transferConflict": fields<Variant<Prompt, "kind", "transferConflict">>()(
    "kind",
    "jobId",
    "path",
    "existingSize",
    "incomingSize",
  ),
  HostKeyInfo: fields<HostKeyInfo>()("algorithm", "sha256Fingerprint", "openssh"),
  CmdError: fields<ApiError>()("code", "message", "refs"),
  SyncStatus: fields<SyncStatus>()("account", "status"),
  SyncAccount: fields<SyncAccount>()("server", "username", "deviceId"),
  "SyncState.off": fields<Variant<SyncState, "state", "off">>()("state"),
  "SyncState.idle": fields<Variant<SyncState, "state", "idle">>()("state", "lastSync"),
  "SyncState.syncing": fields<Variant<SyncState, "state", "syncing">>()("state", "done", "total"),
  "SyncState.offline": fields<Variant<SyncState, "state", "offline">>()("state", "reason"),
  "SyncState.error": fields<Variant<SyncState, "state", "error">>()("state", "kind", "message"),
  CodeInfo: fields<CodeInfo>()("kind", "server", "fingerprint"),
  "RecoveryChoice.existing": fields<WithKey<RecoveryChoice, "existing">>()("existing"),
  "RecoveryChoice.new": fields<WithKey<RecoveryChoice, "new">>()("new"),
  EnableResult: fields<EnableResult>()("newRecoveryKey"),
  EnableError: fields<EnableError>()("code", "message", "refs", "newRecoveryKey"),
  JoinResult: fields<JoinResult>()("backup", "reconnected"),
  RecoverResult: fields<RecoverResult>()("needsDeviceCode", "server"),
  LinkCode: fields<LinkCode>()("code", "server", "expiresAt"),
  Device: fields<Device>()("id", "name", "platform", "createdAt", "lastSeen", "current"),
  Conflict: fields<Conflict>()(
    "id",
    "recordId",
    "kind",
    "current",
    "other",
    "changedFields",
    "detectedAt",
  ),
  "Resolution.keepCurrent": fields<WithKey<Resolution, "keepCurrent">>()("keepCurrent"),
  "Resolution.useOther": fields<WithKey<Resolution, "useOther">>()("useOther"),
  "Resolution.fields": fields<WithKey<Resolution, "fields">>()("fields"),
};
