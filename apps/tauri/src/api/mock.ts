/**
 * In-memory TetherApi for Vitest and Playwright (`VITE_TETHER_API=mock`).
 *
 * Mirrors the Rust command layer's observable rules where cheap: a vault
 * that must be created and unlocked (fake password check), entity CRUD
 * with the vault's reference rules (a referenced identity/key/group is not
 * deleted and the error lists its users; deleting a tag strips it from
 * hosts; deleting a host deletes its forward rules; group delete policies),
 * effective settings with sources, and no secret in any returned value.
 *
 * Sync is simulated too: `sync.*` keeps a fake account (server, username,
 * devices, status) with the real commands' rules (a wrong password or
 * recovery key, `vaultExists`, add-device codes, `syncOff` while sync is off),
 * and `control` plays the engine and the server: set the status, add devices
 * and conflicts, change the password "on another device".
 *
 * `control` lets tests play the backend's part: ask prompts, push session
 * output, emit transfer and forward events.
 */

import type {
  ActiveWork,
  ApiError,
  ChildPolicy,
  CodeInfo,
  Conflict,
  ConflictSide,
  ConnectStep,
  ConnSettings,
  Device,
  EffectiveSettings,
  EnableError,
  EnableResult,
  EntityInputOf,
  EntityKind,
  EntityOf,
  ErrorCode,
  ForwardEvent,
  ForwardRule,
  Snippet,
  ForwardStatus,
  Group,
  Host,
  HostKeyInfo,
  Id,
  KeepAliveStatus,
  Identity,
  KeyAlgo,
  KnownHost,
  LocalEntry,
  LocalShell,
  PickFileOptions,
  Prompt,
  PromptEvent,
  PromptResponse,
  PromptWithdrawn,
  RemoteEntry,
  Resolved,
  SessionHandlers,
  SessionTarget,
  Source,
  SshKey,
  Tag,
  TetherApi,
  CloseRequest,
  TransferEventPayload,
  TransferJob,
  Unlisten,
  QuickUnlockKind,
  RecoveryChoice,
  Resolution,
  SyncAccount,
  SyncState,
  SyncStatus,
  UpdateInfo,
  VaultLocked,
} from "./types";
import { isApiError } from "./types";
import { UPDATES_SUPPORTED } from "../platform";

/** Test-side handles on the mock backend. */
export interface MockControls {
  /** Asks the UI (a `prompt` event) and resolves with its answer; a withdrawn prompt resolves "reject". */
  ask(prompt: Prompt): Promise<PromptResponse>;
  /** Withdraws an unanswered prompt (`prompt-withdrawn`), like a timeout. */
  withdraw(id: string): void;
  /** Pushes terminal output into a session. */
  output(sessionId: string, bytes: Uint8Array): void;
  /**
   * Ends a session from the remote side: `reason` null (the default) is the
   * shell exiting (with `exitStatus`), a string a lost connection.
   */
  endSession(sessionId: string, reason?: string | null, exitStatus?: number | null): void;
  /**
   * The next `openSession` throws (code `connect`) with this message, like a
   * host that cannot be reached. With `atStep`, the steps up to and including
   * it are reported first (a login refused at `auth`).
   */
  failNextOpenSession(message: string, atStep?: ConnectStep): void;
  /**
   * The next `openSession` stops after reporting `tcp` until the returned
   * handle `release`s it (it then goes on and resolves) or `fail`s it (it
   * throws, code `connect`), to test the UI mid-connect.
   */
  holdNextOpenSession(): { release(): void; fail(message: string): void };
  /** Bytes the UI wrote to a session, in order. */
  written(sessionId: string): Uint8Array[];
  /** The `shellId` of every local session opened so far, in order (`null`: the default shell). */
  openedLocalShells(): (string | null)[];
  /** The target of every `openSession` call so far, in order (failed ones included). */
  openTargets(): SessionTarget[];
  /**
   * The tmux session `sessionTmux` reports for a session from now on (`null`:
   * none), as if the user had started or left tmux in it. A host session
   * opened with `reattachTmux` starts in that session.
   */
  setTmux(sessionId: string, name: string | null): void;
  emitTransfer(payload: TransferEventPayload): void;
  emitForward(event: ForwardEvent): void;
  /** The next `forwardStart` of `ruleId` fails with this error (e.g. "address in use"). */
  failNextForward(ruleId: Id, error: string): void;
  /**
   * The next `forwardStart` of `ruleId` throws (code `connect`) before any
   * status, like a host that refuses the login.
   */
  rejectNextForwardStart(ruleId: Id, message: string): void;
  /** Contents of a "local file" for the import commands. */
  setFile(path: string, text: string): void;
  /** The files the next `importFromDevice` picks (default none: a cancelled picker); used up by that call. */
  setDevicePick(names: string[]): void;
  /** The next `importFromDevice` waits (its picker open) until the returned handle `release`s it. */
  holdNextDevicePick(): { release(): void };
  /** The next `saveToDevice` is cancelled in the picker. */
  cancelNextSave(): void;
  /** The paths `saveToDevice` wrote to the device, in order. */
  savedToDevice(): string[];
  /** What the next native file dialog (`pickFile`, `keySavePrivate`) resolves with: a path, or `null` for a cancelled dialog (the default). */
  setPick(path: string | null): void;
  /** The options of every `pickFile` call so far. */
  pickCalls(): PickFileOptions[];
  /** What the next `scanQr` resolves with: the scanned text, or `null` for a cancelled or refused camera (the default). */
  setScan(text: string | null): void;
  /** How many times `scanQr` was called. */
  scanCalls(): number;
  /** Sets the clipboard's text (`null`: empty) for `readClipboard`. */
  setClipboard(text: string | null): void;
  /** The clipboard's text (`null`: empty), as `writeClipboard` left it. */
  clipboard(): string | null;
  /**
   * The next quick-unlock `enable` or `unlock` fails with this error (code
   * `keychain` unless given). `biometryChanged` also turns quick unlock off,
   * like the backend.
   */
  failQuickUnlock(op: "enable" | "unlock", message: string, code?: ErrorCode): void;
  /** How many times `vaultQuickUnlock` was called. */
  quickUnlockCalls(): number;
  /**
   * Moves the auto-lock's clock forward, like time spent away while the app
   * was suspended: no timer runs, so a lock that is due happens on the next
   * `appFront(true)`.
   */
  advanceClock(ms: number): void;
  /**
   * Delivers the auto-lock's `vault-locked` event this long after
   * `appFront(true)` answers (default at once): the real event and the
   * command's answer travel separately, so the UI must not wait for the event.
   */
  delayLockEvents(ms: number): void;
  /** The auto-lock limit last set with `setAutoLock` (`null`: never). */
  autoLockMinutes(): number | null;
  /** What `setDeviceFallback` was last given (`null`: never). */
  deviceFallback(): "iPhone" | "iPad" | null;
  /** What the user answers when keep-alive asks for location permission (default allow). */
  answerLocationWith(answer: "allow" | "deny"): void;
  /** How many times keep-alive asked for location permission. */
  locationAsks(): number;
  /**
   * The location permission changes outside Tether (iOS Settings, "Allow
   * Once" ending: `notDetermined`). Anything but granted turns keep-alive
   * off, like the backend; a change is sent as a `keepalive` event.
   */
  setLocationPermission(permission: "notDetermined" | "granted" | "denied"): void;
  /** Adds a remote file or directory to a host's fake SFTP tree. */
  addRemote(hostId: Id, entry: RemoteEntry): void;
  /** Adds a local file or directory to the fake local file system (`~` is `MOCK_HOME`). */
  addLocal(entry: LocalEntry): void;
  /** Every transfer enqueued so far, in order (finished ones included). */
  enqueued(): { jobId: string; hostId: Id; job: TransferJob }[];
  /**
   * The host key the "server" at `address:port` presents. Once set,
   * opening a session on a host with that address checks it against the
   * known hosts like the real backend: unknown asks `hostKeyUnknown`, a
   * different key asks `hostKeyChanged`; an accept with `save` stores it
   * (replacing the old entries), anything else but accept refuses the
   * session (`hostKeyRejected`).
   */
  setServerKey(address: string, port: number, algorithm: string, publicKey: string): HostKeyInfo;
  /** Ids of the prompts still waiting for an answer, oldest first. */
  pendingPrompts(): string[];
  /**
   * The user asks to close the window: runs the `onCloseRequested`
   * handlers like Tauri (awaiting each); the window closes unless one
   * called `preventDefault`.
   */
  requestClose(): Promise<void>;
  /** Whether the window was closed (`destroyWindow`, or an unprevented close request). */
  windowClosed(): boolean;
  /** Android's system Back: runs the `onBackButton` handlers. */
  backButton(): void;
  /** How many times the app was sent to the background (`moveToBackground`). */
  backgrounded(): number;

  /**
   * Sets the engine's state and emits `sync-status` (like the engine would:
   * offline, an error, syncing with progress). Works while sync is off too;
   * `account` stays whatever the account is.
   */
  setSyncStatus(status: SyncState): void;
  /** How many times `sync.now` was called. */
  syncNowCalls(): number;
  /** Another device of the account (listed by `sync.devices`). Unset fields get defaults. */
  addDevice(device: Partial<Device> & { id: string }): Device;
  /** What `sync.parseCode` says about `code` (codes not set here: any `tether1-` text; `link` in it makes an add-device code). */
  setCodeInfo(code: string, info: CodeInfo): void;
  /**
   * The account on the "server": who can sign in, with which password and
   * recovery key, and which vault it syncs (`vaultId`; another vault than
   * this device's unless given, see `localVaultId`). `enable` and `join`
   * check against it; `null` forgets it.
   */
  setServerAccount(
    account: { username: string; password: string; recoveryKey: string; vaultId?: string } | null,
  ): void;
  /** The next call of this sync command rejects with `{code, message}` (one attempt). */
  failNextSync(op: SyncOp, code: ErrorCode, message: string): void;
  /**
   * The next `sync.enable` fails after the server registered the account, like a credential store
   * failing at the end: the rejection carries the new recovery key (when "I lost it" made one),
   * and sync is on but must sign in again.
   */
  failNextEnableAfterRegister(code: ErrorCode, message: string): void;
  /**
   * Another device changed the master password. Unless `stale`, this device
   * took it over at once: it locks and emits `vault-locked` with reason
   * `passwordChanged`. A `stale` device (it was offline) heard nothing: it
   * still opens with the old password, and a failed unlock with the new one
   * refreshes from the server and succeeds.
   */
  changePasswordElsewhere(newPassword: string, options?: { stale?: boolean }): void;
  /**
   * Adds a conflict. `current` and `other` are the record in the vault's
   * JSON names with real secret values (`password`, `private_openssh`,
   * `passphrase` are masked in `conflicts.list`, revealed by
   * `conflicts.reveal`); `null` is a deletion. `changedFields` is derived.
   */
  addConflict(conflict: {
    kind: EntityKind;
    recordId?: Id;
    current: Record<string, unknown> | null;
    other: Record<string, unknown> | null;
    detectedAt?: number;
  }): Conflict;
  /** Every `conflicts.resolve` so far, in order. */
  resolvedConflicts(): { id: Id; resolution: Resolution }[];
  /** The id of this device's vault (null before it exists). */
  localVaultId(): string | null;
  /**
   * A record added or changed on another device that sync just pulled in:
   * it is stored like `upsertEntity` would and `vault-changed` is emitted,
   * without a page having asked for anything.
   */
  remoteUpsert<K extends EntityKind>(kind: K, input: EntityInputOf<K>): EntityOf<K>;
  /** A record deleted on another device, pulled in by sync (`vault-changed` follows). */
  remoteDelete(kind: EntityKind, id: Id): void;
  /** Emits `vault-changed` alone (how often the UI reads again is up to it). */
  emitVaultChanged(): void;
  /** How many times `vault-changed` was emitted so far. */
  vaultChangedCount(): number;
  /** What `updates.check` finds from now on: a release, none (`null`, the default), or `"fail"` (it rejects, like no network). */
  setUpdate(update: UpdateInfo | null | "fail"): void;
  /** How many times `updates.check` was called. */
  updateChecks(): number;
  /** How many times `updates.install` was called. */
  updateInstalls(): number;
  /** `updates.install` rejects with this message from now on (`null`: it succeeds, the default). */
  failUpdateInstall(message: string | null): void;
}

/** The sync commands `failNextSync` can fail. */
export type SyncOp =
  | "parseCode"
  | "checkServer"
  | "enable"
  | "join"
  | "recover"
  | "now"
  | "signIn"
  | "turnOff"
  | "linkCode"
  | "devices"
  | "renameDevice"
  | "revokeDevice"
  | "changePassword";

/** A server code that parses in the mock (any `tether1-` text does, `link` in it making an add-device code; `TLX-` text is a short code). */
export const MOCK_SERVER = "https://sync.mock.test";
/** The fingerprint the mock shows for its server's codes. */
export const MOCK_FINGERPRINT =
  "sha256:0f1e2d3c4b5a69788796a5b4c3d2e1f00123456789abcdeffedcba9876543210";
/** The id of the mock's own device. */
export const MOCK_DEVICE_ID = "0190b6f6-7d3c-7000-8000-00000000d0d0";

/** Record fields the vault treats as secret, per kind (Rust: `secret_fields`). */
const SECRET_FIELDS: Partial<Record<EntityKind, readonly string[]>> = {
  identity: ["password"],
  ssh_key: ["private_openssh", "passphrase"],
};

/** What `~` expands to in the mock's local file system. */
export const MOCK_HOME = "/home/mock";
/** The login directory of the mock's SFTP servers (`.`); it always exists. */
export const MOCK_REMOTE_HOME = "/home/deploy";

/** `path` with `.`, `..`, empty and repeated components resolved, relative to `base`. */
function canonical(base: string, path: string): string {
  const parts: string[] = [];
  for (const part of (path.startsWith("/") ? path : `${base}/${path}`).split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") parts.pop();
    else parts.push(part);
  }
  return `/${parts.join("/")}`;
}

export type MockTetherApi = TetherApi & { control: MockControls };

function apiError(code: ErrorCode, message: string, refs: string[] = []): ApiError {
  return { code, message, refs };
}

const APP_DEFAULTS: ConnSettings = {
  port: 22,
  identityId: null,
  keepaliveSecs: 30,
  termType: "xterm-256color",
  charset: "UTF-8",
  colorScheme: null,
  fontFamily: null,
  fontSize: null,
  env: null,
};

const EMPTY_SETTINGS: ConnSettings = {
  port: null,
  identityId: null,
  keepaliveSecs: null,
  termType: null,
  charset: null,
  colorScheme: null,
  fontFamily: null,
  fontSize: null,
  env: null,
};

/** Marks the body of a fake encrypted private key. */
const ENCRYPTED = "mock-encrypted:";

const KEY_ALGOS: Record<KeyAlgo, string> = {
  ed25519: "ssh-ed25519",
  ecdsaP256: "ecdsa-sha2-nistp256",
  ecdsaP384: "ecdsa-sha2-nistp384",
  ecdsaP521: "ecdsa-sha2-nistp521",
  rsa2048: "rsa-sha2-512",
  rsa4096: "rsa-sha2-512",
};

/** Stored identity: the password stays in here. */
interface IdentityRecord {
  id: Id;
  label: string;
  username: string;
  password: string | null;
  keyId: Id | null;
  owner: Id | null;
}

/** Stored key: the private part stays in here. */
interface KeyRecord {
  id: Id;
  label: string;
  algorithm: string;
  privateOpenssh: string;
  passphrase: string | null;
  publicOpenssh: string;
  fingerprint: string;
}

interface SessionRecord {
  handlers: SessionHandlers;
  title: string;
  hostId: Id | null;
  written: Uint8Array[];
  tmux: string | null;
}

interface TransferRecord {
  hostId: Id;
  job: TransferJob;
}

const clone = <T>(v: T): T => structuredClone(v);

const CONNECT_STEPS: readonly ConnectStep[] = ["tcp", "hostKey", "auth", "shell"];

const SETTINGS_FIELDS = Object.keys(EMPTY_SETTINGS);

/** The fields each entity input accepts (Rust: `deny_unknown_fields` on the DTOs). */
const INPUT_FIELDS: Record<EntityKind, readonly string[]> = {
  group: ["id", "parentId", "name", "settings"],
  host: ["id", "groupId", "label", "address", "tagIds", "settings", "forwardRuleIds"],
  tag: ["id", "name", "color"],
  identity: ["id", "label", "username", "password", "keyId"],
  ssh_key: ["id", "label"],
  known_host: [
    "id",
    "hostPattern",
    "port",
    "algorithm",
    "publicKey",
    "addedAt",
    "source",
    "fingerprint",
  ],
  forward_rule: [
    "id",
    "hostId",
    "label",
    "kind",
    "bindAddr",
    "bindPort",
    "destHost",
    "destPort",
    "autostart",
  ],
  snippet: ["id", "label", "text", "run"],
};

/** Like serde's `deny_unknown_fields`: an unknown field is an `invalid` error, never silently dropped. */
function refuseUnknownFields(kind: EntityKind, input: unknown) {
  if (typeof input !== "object" || input === null || Array.isArray(input)) {
    throw apiError("invalid", `${kind}: expected an object`);
  }
  const obj = input as Record<string, unknown>;
  const allowed = INPUT_FIELDS[kind];
  for (const key of Object.keys(obj)) {
    if (!allowed.includes(key)) throw apiError("invalid", `${kind}: unknown field \`${key}\``);
  }
  const settings = obj.settings;
  if (allowed.includes("settings") && settings != null) {
    if (typeof settings !== "object" || Array.isArray(settings)) {
      throw apiError("invalid", `${kind}: settings must be an object`);
    }
    for (const key of Object.keys(settings)) {
      if (!SETTINGS_FIELDS.includes(key)) {
        throw apiError("invalid", `${kind}: unknown settings field \`${key}\``);
      }
    }
  }
}

/** A small stable hash for fake fingerprints. */
function hash(text: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(16).padStart(8, "0");
}

function parentOf(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  const i = trimmed.lastIndexOf("/");
  return i <= 0 ? "/" : trimmed.slice(0, i);
}

/**
 * State the mock starts with, for tests that need it before the page's
 * first render (Playwright sets `window.__tetherMockSeed` in an init script).
 */
export interface MockSeed {
  quickUnlockKind?: QuickUnlockKind;
  /** The shells `localShells` offers, default first. Defaults to one (macOS-like) shell. */
  localShells?: LocalShell[];
  /** The roots `localDrives` lists (a Windows-like machine). Defaults to `["/"]`. */
  localDrives?: string[];
  /** A vault that already exists and is locked. */
  lockedVault?: { password: string; quickUnlock: boolean };
  /** The first quick unlock fails like `failQuickUnlock("unlock", ...)`. */
  quickUnlockFailure?: { message: string; code: ErrorCode };
  /**
   * Sync is already on (with `lockedVault`: that vault syncs). The account
   * defaults to `alice` on `MOCK_SERVER`, state idle, one device (this one).
   */
  sync?: { server?: string; username?: string; status?: SyncState; recoveryKey?: string };
  /** The first this many `setAutoLock` calls fail (`internal`), like a backend that is not ready. */
  autoLockFailures?: number;
  /** What the user answers when keep-alive asks for location permission (default allow). */
  locationAnswer?: "allow" | "deny";
  /** The location permission at start (default not asked yet). */
  locationPermission?: "notDetermined" | "granted" | "denied";
  /** What `updates.check` finds, as `setUpdate`. */
  update?: UpdateInfo | null | "fail";
}

export function createMockApi(seed: MockSeed = {}): MockTetherApi {
  let counter = 0;
  const newId = (): Id => {
    counter += 1;
    return `0190b6f6-7d3c-7000-8000-${counter.toString(16).padStart(12, "0")}`;
  };

  // vault
  let password: string | null = seed.lockedVault?.password ?? null;
  let recoveryKey: string | null = password === null ? null : "MOCK-SEEDED";
  let unlocked = false;
  /** This device's vault id (a join of the same vault keeps the records). */
  let vaultId: string | null = password === null ? null : "mock-seeded-vault";
  let quickUnlock = seed.lockedVault?.quickUnlock ?? false;
  const quickUnlockKind: QuickUnlockKind = seed.quickUnlockKind ?? "none";
  let quickUnlockCalls = 0;
  // auto-lock (the backend's autolock.rs), on a clock tests can move
  let clockOffset = 0;
  const clockNow = () => Date.now() + clockOffset;
  let autoLockMinutes: number | null = null;
  let deviceFallback: "iPhone" | "iPad" | null = null;
  let leftFrontAt: number | null = null;
  let lockEventDelay = 0;
  let autoLockFailures = seed.autoLockFailures ?? 0;
  const localShells: LocalShell[] = seed.localShells ?? [{ id: "default", label: "zsh" }];
  const localDrives = seed.localDrives ?? ["/"];
  const openedLocalShells: (string | null)[] = [];
  const openTargets: SessionTarget[] = [];
  let update: UpdateInfo | null | "fail" = seed.update ?? null;
  let updateChecks = 0;
  let updateInstalls = 0;
  let installFailure: string | null = null;

  // sync
  interface ServerAccount {
    username: string;
    password: string;
    recoveryKey: string;
    /** The vault the account syncs. */
    vaultId: string;
  }
  let serverAccount: ServerAccount | null = null;
  let syncAccount: SyncAccount | null = null;
  let syncState: SyncState = { state: "off" };
  let devices: Device[] = [];
  let syncNowCalls = 0;
  let linkCounter = 0;
  const codeInfos = new Map<string, CodeInfo>();
  const syncFailures = new Map<SyncOp, { code: ErrorCode; message: string }>();
  let lateEnableFailure: { code: ErrorCode; message: string } | null = null;
  const conflictRecords = new Map<Id, { view: Conflict; raw: { current: Record<string, unknown> | null; other: Record<string, unknown> | null } }>();
  const resolvedConflicts: { id: Id; resolution: Resolution }[] = [];

  const groups = new Map<Id, Group>();
  const hosts = new Map<Id, Host>();
  const tags = new Map<Id, Tag>();
  const identities = new Map<Id, IdentityRecord>();
  const keys = new Map<Id, KeyRecord>();
  const knownHosts = new Map<Id, KnownHost>();
  const rules = new Map<Id, ForwardRule>();
  const snippets = new Map<Id, Snippet>();

  // runtime
  const sessions = new Map<string, SessionRecord>();
  const transfers = new Map<string, TransferRecord>();
  const sftpOpen = new Set<Id>();
  const remote = new Map<Id, Map<string, RemoteEntry>>();
  const local = new Map<string, LocalEntry>();
  const enqueued: { jobId: string; hostId: Id; job: TransferJob }[] = [];
  const forwardStatus = new Map<Id, ForwardStatus>();
  const forwardFailures = new Map<Id, string>();
  const forwardRejections = new Map<Id, string>();
  const sessionFailures: { message: string; atStep?: ConnectStep }[] = [];
  const sessionHolds: { release(): void; fail(message: string): void; wait: Promise<void> }[] = [];

  // background keep-alive (the backend's keepalive.rs, as on iOS): asked once, answered at once
  let keepAliveOn = false;
  let locationPermission: "notDetermined" | "granted" | "denied" = seed.locationPermission ?? "notDetermined";
  let locationAnswer = seed.locationAnswer ?? "allow";
  let locationAsks = 0;
  let keepAliveReported: string | null = null;
  /** Sends the status as a `keepalive` event when it changed (the mock does not resend on session changes). */
  const reportKeepAlive = () => {
    const status = keepAliveStatus();
    const key = JSON.stringify(status);
    if (key === keepAliveReported) return;
    keepAliveReported = key;
    emit("keepalive", status);
  };
  const keepAliveStatus = (): KeepAliveStatus => {
    const forwards = [...forwardStatus.values()].filter((s) => s.state === "starting" || s.state === "active");
    const authorized = locationPermission === "granted";
    return {
      enabled: keepAliveOn,
      authorized,
      running: keepAliveOn && authorized && sessions.size + forwards.length > 0,
    };
  };
  const files = new Map<string, string>();
  let pickedFile: string | null = null;
  let devicePick: string[] = [];
  let devicePickHold: Promise<void> | null = null;
  let saveCancelled = false;
  const savedToDevice: string[] = [];
  const pickCalls: PickFileOptions[] = [];
  let scannedText: string | null = null;
  let scanCalls = 0;
  let clipboardText: string | null = null;
  const quickUnlockFailures = new Map<"enable" | "unlock", { message: string; code: ErrorCode }>();
  if (seed.quickUnlockFailure) quickUnlockFailures.set("unlock", seed.quickUnlockFailure);
  const pendingPrompts = new Map<string, (r: PromptResponse) => void>();
  const serverKeys = new Map<string, HostKeyInfo>();
  const closeHandlers = new Set<(e: CloseRequest) => void | Promise<void>>();
  let windowClosed = false;
  let backgrounded = 0;

  function ask(prompt: Prompt): Promise<PromptResponse> {
    const id = `prompt-${newId()}`;
    const answer = new Promise<PromptResponse>((resolve) => pendingPrompts.set(id, resolve));
    emit("prompt", { id, prompt });
    return answer;
  }

  const listeners = new Map<string, Set<(payload: unknown) => void>>();
  function emit(event: string, payload: unknown) {
    for (const cb of [...(listeners.get(event) ?? [])]) cb(clone(payload));
  }
  function on<T>(event: string, cb: (payload: T) => void): Promise<Unlisten> {
    const set = listeners.get(event) ?? new Set();
    listeners.set(event, set);
    const wrapped = cb as (payload: unknown) => void;
    set.add(wrapped);
    return Promise.resolve(() => {
      set.delete(wrapped);
    });
  }

  // -- sync ---------------------------------------------------------------

  const thisDevice = (): Device => ({
    id: MOCK_DEVICE_ID,
    // Like the backend: the host name on a desktop, the fallback the app sent on iOS.
    name: deviceFallback ?? "This Mac",
    platform: deviceFallback === null ? "macos" : "ios",
    createdAt: 1_700_000_000_000,
    lastSeen: Date.now(),
    current: true,
  });

  function syncStatus(): SyncStatus {
    return clone({ account: syncAccount, status: syncState });
  }

  let vaultChanged = 0;
  /** Sync pulled changes into the vault. */
  function emitVaultChanged() {
    vaultChanged += 1;
    emit("vault-changed", {});
  }

  function setSyncState(state: SyncState) {
    syncState = state;
    emit("sync-status", syncStatus());
  }

  /** What a sync cycle looks like from outside: syncing, then idle. */
  function runSync() {
    if (syncState.state === "error" && syncState.kind === "revoked") return;
    const total = hosts.size + groups.size + identities.size + keys.size;
    setSyncState({ state: "syncing", done: 0, total });
    setSyncState({ state: "idle", lastSync: Date.now() });
  }

  function failSync(op: SyncOp) {
    const failure = syncFailures.get(op);
    if (!failure) return;
    syncFailures.delete(op);
    throw apiError(failure.code, failure.message);
  }

  function requireSync(): SyncAccount {
    if (!syncAccount) throw apiError("syncOff", "sync is off");
    return syncAccount;
  }

  /** A typed address as `https://host:port` (port 8443 unless typed), like `core_proto::parse_address`. */
  function mockAddress(text: string): string {
    const m = /^(?:https:\/\/)?([\w.-]+)(?::(\d{1,5}))?\/?$/i.exec(text.trim());
    if (!m) throw apiError("invalidAddress", "that server address is not valid");
    return `https://${m[1]}:${m[2] ?? "8443"}`;
  }

  /** What a short code and a typed address make once the server has answered the proof. */
  function checkedServer(address: string): CodeInfo {
    return { kind: "invite", server: mockAddress(address), fingerprint: MOCK_FINGERPRINT };
  }

  function parseCode(text: string): CodeInfo {
    const code = text.trim();
    const known = codeInfos.get(code);
    if (known) return clone(known);
    if (/^tlx-[0-9a-z-]+$/i.test(code)) return { kind: "short", server: "", fingerprint: null };
    if (!/^tether1-\S+$/i.test(code)) {
      throw apiError("invalidCode", "invalid code: not a Termlyx code");
    }
    return {
      kind: /link/i.test(code) ? "link" : "invite",
      server: MOCK_SERVER,
      fingerprint: MOCK_FINGERPRINT,
    };
  }

  function startSync(info: CodeInfo, username: string) {
    syncAccount = { server: info.server, username, deviceId: MOCK_DEVICE_ID };
    devices = [thisDevice(), ...devices.filter((d) => !d.current)];
    runSync();
  }

  /** The password the account has now; the same lock a device gets when it hears. */
  function changePasswordElsewhere(newPassword: string, stale: boolean) {
    if (serverAccount) serverAccount.password = newPassword;
    if (stale) return;
    password = newPassword;
    unlocked = false;
    emit("vault-locked", { reason: "passwordChanged" } satisfies VaultLocked);
  }

  function conflictRecord(id: Id) {
    const found = conflictRecords.get(id);
    if (!found) throw notFound("conflict", id);
    return found;
  }

  const maskSecrets = (kind: EntityKind, record: Record<string, unknown> | null) => {
    if (record === null) return null;
    const masked = clone(record);
    for (const field of SECRET_FIELDS[kind] ?? []) {
      if (masked[field] != null) masked[field] = { hasSecret: true };
    }
    return masked;
  };

  if (seed.sync) {
    const username = seed.sync.username ?? "alice";
    serverAccount = {
      username,
      password: password ?? "",
      recoveryKey: seed.sync.recoveryKey ?? recoveryKey ?? "MOCK-SEEDED",
      vaultId: vaultId ?? "mock-seeded-vault",
    };
    startSync({ kind: "link", server: seed.sync.server ?? MOCK_SERVER, fingerprint: null }, username);
    syncState = seed.sync.status ?? { state: "idle", lastSync: Date.now() };
  }

  function requireVault() {
    if (password === null) throw apiError("locked", "vault is locked");
    if (!unlocked) throw apiError("locked", "vault is locked");
  }

  function notFound(kind: string, id: Id): ApiError {
    return apiError("notFound", `${kind} ${id} not found`, [id]);
  }

  function requireRef<T>(map: Map<Id, T>, id: Id | null | undefined) {
    if (id != null && !map.has(id)) {
      throw apiError("missingRef", `referenced record ${id} does not exist`, [id]);
    }
  }

  const settingsOf = (s?: Partial<ConnSettings>): ConnSettings => ({ ...EMPTY_SETTINGS, ...s });

  const identityDto = (i: IdentityRecord): Identity => ({
    id: i.id,
    label: i.label,
    username: i.username,
    hasPassword: i.password !== null,
    keyId: i.keyId,
    owner: i.owner,
  });

  /** Like the backend: a Keychain identity or the record's own login, never another record's. */
  function requireLogin(owner: Id, identityId: Id | null) {
    requireRef(identities, identityId);
    const found = identityId === null ? undefined : identities.get(identityId);
    if (found?.owner && found.owner !== owner) {
      throw apiError("invalid", `identity ${identityId} is another record's own login`, [identityId!]);
    }
  }

  /** Like the backend's tidy: own logins nothing links go; the others take their owner's name. */
  function pruneOwnLogins() {
    for (const ident of [...identities.values()]) {
      if (ident.owner === null) continue;
      const owner = hosts.get(ident.owner) ?? groups.get(ident.owner);
      if (owner?.settings.identityId === ident.id) {
        // Named after its owner, also after a rename.
        ident.label = "label" in owner ? owner.label : owner.name;
        continue;
      }
      const used = [...hosts.values(), ...groups.values()].some((r) => r.settings.identityId === ident.id);
      if (!used) identities.delete(ident.id);
    }
  }

  const keyDto = (k: KeyRecord): SshKey => ({
    id: k.id,
    label: k.label,
    algorithm: k.algorithm,
    publicOpenssh: k.publicOpenssh,
    fingerprint: k.fingerprint,
    hasPassphrase: k.passphrase !== null,
    certificate: null,
  });

  /** Forward rule ids of a host: the given order for ids that are its rules, then the rest. */
  function ruleIdsOf(hostId: Id, preferred: Id[] = []): Id[] {
    const owned = [...rules.values()].filter((r) => r.hostId === hostId).map((r) => r.id);
    const ordered = preferred.filter((id, i) => owned.includes(id) && preferred.indexOf(id) === i);
    return [...ordered, ...owned.filter((id) => !ordered.includes(id))];
  }

  function syncHostRules(hostId: Id) {
    const host = hosts.get(hostId);
    if (host) host.forwardRuleIds = ruleIdsOf(hostId, host.forwardRuleIds);
  }

  function groupChain(leaf: Id | null): Id[] {
    const chain: Id[] = [];
    const seen = new Set<Id>();
    for (let id = leaf; id !== null; ) {
      const g = groups.get(id);
      if (!g) break;
      if (seen.has(id)) throw apiError("cycle", "group hierarchy would contain a cycle");
      seen.add(id);
      chain.push(id);
      id = g.parentId;
    }
    return chain.reverse();
  }

  function list<K extends EntityKind>(kind: K): EntityOf<K>[] {
    const all: Record<EntityKind, () => unknown[]> = {
      group: () => [...groups.values()],
      host: () => [...hosts.values()],
      tag: () => [...tags.values()],
      identity: () => [...identities.values()].map(identityDto),
      ssh_key: () => [...keys.values()].map(keyDto),
      known_host: () => [...knownHosts.values()],
      forward_rule: () => [...rules.values()],
      snippet: () => [...snippets.values()],
    };
    const make = all[kind];
    if (!make) throw apiError("invalid", `unknown entity kind "${kind}"`);
    return clone(make()) as EntityOf<K>[];
  }

  function upsert<K extends EntityKind>(kind: K, input: EntityInputOf<K>): EntityOf<K> {
    refuseUnknownFields(kind, input);
    const id = (input as { id?: Id }).id ?? newId();
    switch (kind) {
      case "group": {
        const i = input as EntityInputOf<"group">;
        const g: Group = {
          id,
          parentId: i.parentId ?? null,
          name: i.name,
          settings: settingsOf(i.settings),
        };
        if (g.parentId === id) throw apiError("cycle", "group hierarchy would contain a cycle");
        requireRef(groups, g.parentId);
        requireLogin(id, g.settings.identityId);
        const previous = groups.get(id);
        groups.set(id, g);
        try {
          groupChain(id);
        } catch (e) {
          if (previous) groups.set(id, previous);
          else groups.delete(id);
          throw e;
        }
        pruneOwnLogins();
        return clone(g) as EntityOf<K>;
      }
      case "host": {
        const i = input as EntityInputOf<"host">;
        const h: Host = {
          id,
          groupId: i.groupId ?? null,
          label: i.label,
          address: i.address,
          tagIds: [...(i.tagIds ?? [])],
          settings: settingsOf(i.settings),
          forwardRuleIds: ruleIdsOf(id, i.forwardRuleIds ?? []),
        };
        requireRef(groups, h.groupId);
        requireLogin(id, h.settings.identityId);
        h.tagIds.forEach((t) => requireRef(tags, t));
        hosts.set(id, h);
        pruneOwnLogins();
        return clone(h) as EntityOf<K>;
      }
      case "tag": {
        const i = input as EntityInputOf<"tag">;
        const t: Tag = { id, name: i.name, color: i.color };
        tags.set(id, t);
        return clone(t) as EntityOf<K>;
      }
      case "identity": {
        const i = input as EntityInputOf<"identity">;
        requireRef(keys, i.keyId);
        const previous = identities.get(id);
        const record: IdentityRecord = {
          id,
          label: i.label,
          username: i.username,
          password: i.password === undefined ? (previous?.password ?? null) : i.password,
          keyId: i.keyId ?? null,
          owner: previous?.owner ?? null,
        };
        identities.set(id, record);
        return identityDto(record) as EntityOf<K>;
      }
      case "ssh_key": {
        const i = input as EntityInputOf<"ssh_key">;
        const key = keys.get(i.id);
        if (!key) throw notFound("ssh_key", i.id);
        key.label = i.label;
        return keyDto(key) as EntityOf<K>;
      }
      case "known_host": {
        const i = input as EntityInputOf<"known_host">;
        const k: KnownHost = {
          id,
          hostPattern: i.hostPattern,
          port: i.port,
          algorithm: i.algorithm,
          publicKey: i.publicKey,
          addedAt: i.addedAt || Math.floor(Date.now() / 1000),
          source: i.source ?? "prompt",
          fingerprint: `SHA256:${hash(i.publicKey)}`,
        };
        knownHosts.set(id, k);
        return clone(k) as EntityOf<K>;
      }
      case "forward_rule": {
        const i = input as EntityInputOf<"forward_rule">;
        if (!hosts.has(i.hostId)) {
          throw apiError("missingRef", `referenced record ${i.hostId} does not exist`, [i.hostId]);
        }
        const previous = rules.get(id);
        const r: ForwardRule = {
          id,
          hostId: i.hostId,
          label: i.label,
          kind: i.kind,
          bindAddr: i.bindAddr,
          bindPort: i.bindPort,
          destHost: i.destHost ?? null,
          destPort: i.destPort ?? null,
          autostart: i.autostart ?? false,
        };
        rules.set(id, r);
        if (previous && previous.hostId !== r.hostId) syncHostRules(previous.hostId);
        syncHostRules(r.hostId);
        return clone(r) as EntityOf<K>;
      }
      case "snippet": {
        const i = input as EntityInputOf<"snippet">;
        const sn: Snippet = { id, label: i.label.trim(), text: i.text, run: i.run ?? false };
        snippets.set(id, sn);
        return clone(sn) as EntityOf<K>;
      }
      default:
        throw apiError("invalid", `unknown entity kind "${kind as string}"`);
    }
  }

  function deleteRulesOf(hostId: Id) {
    for (const r of [...rules.values()]) if (r.hostId === hostId) rules.delete(r.id);
  }

  function remove(kind: EntityKind, id: Id) {
    const maps: Record<EntityKind, Map<Id, unknown>> = {
      group: groups,
      host: hosts,
      tag: tags,
      identity: identities,
      ssh_key: keys,
      known_host: knownHosts,
      forward_rule: rules,
      snippet: snippets,
    };
    const map = maps[kind];
    if (!map) throw apiError("invalid", `unknown entity kind "${kind}"`);
    if (!map.has(id)) throw notFound(kind, id);
    switch (kind) {
      case "group": {
        const children = [
          ...[...groups.values()].filter((g) => g.parentId === id).map((g) => g.id),
          ...[...hosts.values()].filter((h) => h.groupId === id).map((h) => h.id),
        ];
        if (children.length) throw apiError("referenced", "group has children", children);
        break;
      }
      case "identity": {
        const users = [
          ...[...hosts.values()].filter((h) => h.settings.identityId === id).map((h) => h.id),
          ...[...groups.values()].filter((g) => g.settings.identityId === id).map((g) => g.id),
        ];
        if (users.length) throw apiError("referenced", `still referenced by ${users.length} record(s)`, users);
        break;
      }
      case "ssh_key": {
        const users = [...identities.values()].filter((i) => i.keyId === id).map((i) => i.id);
        if (users.length) throw apiError("referenced", `still referenced by ${users.length} record(s)`, users);
        break;
      }
      case "tag":
        for (const h of hosts.values()) h.tagIds = h.tagIds.filter((t) => t !== id);
        break;
      case "host":
        deleteRulesOf(id);
        break;
      case "forward_rule": {
        const hostId = rules.get(id)!.hostId;
        rules.delete(id);
        syncHostRules(hostId);
        return;
      }
    }
    map.delete(id);
  }

  function subtree(root: Id): Set<Id> {
    const found = new Set([root]);
    let grew = true;
    while (grew) {
      grew = false;
      for (const g of groups.values()) {
        if (g.parentId !== null && found.has(g.parentId) && !found.has(g.id)) {
          found.add(g.id);
          grew = true;
        }
      }
    }
    return found;
  }

  function effective(hostId: Id): EffectiveSettings {
    const host = hosts.get(hostId);
    if (!host) throw notFound("host", hostId);
    const layers: [Source, ConnSettings][] = [
      [{ kind: "default" }, APP_DEFAULTS],
      ...groupChain(host.groupId).map(
        (gid): [Source, ConnSettings] => [{ kind: "group", groupId: gid }, groups.get(gid)!.settings],
      ),
      [{ kind: "host" }, host.settings],
    ];
    const pick = <F extends keyof ConnSettings>(field: F): Resolved<ConnSettings[F]> => {
      for (let i = layers.length - 1; i >= 0; i--) {
        const value = layers[i][1][field];
        if (value !== null && value !== undefined) return clone({ value, source: layers[i][0] });
      }
      return { value: null as ConnSettings[F], source: { kind: "default" } };
    };
    const port = pick("port");
    return {
      port: { value: port.value ?? 22, source: port.source },
      identityId: pick("identityId"),
      keepaliveSecs: pick("keepaliveSecs"),
      termType: pick("termType"),
      charset: pick("charset"),
      colorScheme: pick("colorScheme"),
      fontFamily: pick("fontFamily"),
      fontSize: pick("fontSize"),
      env: pick("env"),
    };
  }

  /**
   * Stores a fake key. Like the backend, a key with a passphrase is
   * "encrypted" (its body carries a passphrase check) and keeps the
   * passphrase; one without is stored without.
   */
  function storeKey(label: string, algorithm: string, body: string, passphrase: string | null) {
    const id = newId();
    const stored = passphrase === null ? body : `${ENCRYPTED}${hash(passphrase)}:${body}`;
    const record: KeyRecord = {
      id,
      label,
      algorithm,
      privateOpenssh: `-----BEGIN OPENSSH PRIVATE KEY-----\n${stored}\n-----END OPENSSH PRIVATE KEY-----\n`,
      passphrase,
      publicOpenssh: `${algorithm} AAAA${hash(body)} ${label}`,
      fingerprint: `SHA256:${hash(body)}`,
    };
    keys.set(id, record);
    return keyDto(record);
  }

  function readFile(path: string): string {
    const text = files.get(path);
    if (text === undefined) throw apiError("io", `could not read ${path}`);
    return text;
  }

  function session(id: string): SessionRecord {
    const s = sessions.get(id);
    if (!s) throw apiError("closed", "session closed");
    return s;
  }

  /** The real backend's host-key policy (core-api known_hosts), for hosts with a `setServerKey`. */
  async function verifyHostKey(address: string, port: number) {
    const key = serverKeys.get(`${address}:${port}`);
    if (!key) return;
    const entries = [...knownHosts.values()].filter((k) => k.hostPattern === address && k.port === port);
    if (entries.some((k) => `${k.algorithm} ${k.publicKey}` === key.openssh)) return;
    const shown = entries.find((k) => k.algorithm === key.algorithm) ?? entries[0];
    const answer = await ask(
      shown
        ? {
            kind: "hostKeyChanged",
            host: address,
            port,
            old: {
              algorithm: shown.algorithm,
              sha256Fingerprint: shown.fingerprint,
              openssh: `${shown.algorithm} ${shown.publicKey}`,
            },
            new: clone(key),
          }
        : { kind: "hostKeyUnknown", host: address, port, key: clone(key) },
    );
    if (typeof answer !== "object" || !("accept" in answer)) {
      throw apiError("hostKeyRejected", "host key rejected");
    }
    if (answer.accept.save) {
      upsert("known_host", {
        hostPattern: address,
        port,
        algorithm: key.algorithm,
        publicKey: key.openssh.slice(key.algorithm.length + 1),
      });
      entries.forEach((k) => knownHosts.delete(k.id));
    }
  }

  function endSession(id: string, reason: string | null, exitStatus: number | null = null) {
    const s = sessions.get(id);
    if (!s) return;
    sessions.delete(id);
    s.handlers.onClosed(reason, exitStatus);
  }

  function requireSftp(hostId: Id): Map<string, RemoteEntry> {
    if (!sftpOpen.has(hostId)) throw apiError("notOpen", "SFTP is not open for this host");
    const tree = remote.get(hostId) ?? new Map<string, RemoteEntry>();
    remote.set(hostId, tree);
    return tree;
  }

  function finishTransfer(payload: TransferEventPayload) {
    if (payload.event.kind !== "progress") transfers.delete(payload.event.job);
    emit("transfer", payload);
  }

  function failQuickUnlock(op: "enable" | "unlock") {
    const failure = quickUnlockFailures.get(op);
    quickUnlockFailures.delete(op);
    if (failure === undefined) return;
    if (failure.code === "biometryChanged") quickUnlock = false;
    throw apiError(failure.code, failure.message);
  }

  /** Another running rule that listens on the same address (remote rules listen on the server, not here). */
  function bindConflict(rule: ForwardRule): boolean {
    if (rule.kind === "remote") return false;
    return [...rules.values()].some((other) => {
      const state = forwardStatus.get(other.id)?.state;
      return (
        other.id !== rule.id &&
        other.kind !== "remote" &&
        (state === "active" || state === "starting") &&
        other.bindPort === rule.bindPort &&
        other.bindAddr === rule.bindAddr
      );
    });
  }

  function setForward(ruleId: Id, status: ForwardStatus) {
    forwardStatus.set(ruleId, status);
    emit("forward", { ruleId, status });
  }

  /** `sync.enable` without the `newRecoveryKey: null` the API method adds to plain rejections. */
  async function enableSync(
    code: string,
    username: string,
    pw: string,
    recovery: RecoveryChoice,
    address?: string,
  ): Promise<EnableResult> {
    failSync("enable");
    const info = address === undefined ? parseCode(code) : checkedServer(address);
    // The vault's own answers, as the backend gives them before anything else.
    if (password === null) throw apiError("notInitialized", "vault has not been created");
    if (!unlocked) throw apiError("locked", "vault is locked");
    if (syncAccount) throw apiError("syncOn", "sync is already on");
    if (info.kind !== "invite") {
      throw apiError("invalidCode", "invalid code: this is an add-device code; it joins a synced vault instead");
    }
    if (pw !== password) throw apiError("wrongPassword", "wrong password");
    let newRecoveryKey: string | null = null;
    if ("existing" in recovery) {
      if (recovery.existing !== recoveryKey) throw apiError("wrongRecoveryKey", "wrong recovery key");
    } else {
      newRecoveryKey = `MOCK-${hash(pw + newId())}-${hash(newId())}`;
      recoveryKey = newRecoveryKey;
    }
    serverAccount = {
      username: username.trim(),
      password: pw,
      recoveryKey: recoveryKey ?? "",
      vaultId: vaultId ?? `vault-${newId()}`,
    };
    const late = lateEnableFailure;
    lateEnableFailure = null;
    if (late) {
      // The server has the account (and the new key) but this device failed to finish: sync is on
      // without a session, so the engine asks to sign in again.
      syncAccount = { server: info.server, username: username.trim(), deviceId: MOCK_DEVICE_ID };
      setSyncState({ state: "error", kind: "signInAgain", message: "not authorized; sign in again" });
      throw { ...apiError(late.code, late.message), newRecoveryKey } satisfies EnableError;
    }
    startSync(info, username.trim());
    return { newRecoveryKey };
  }

  const api: MockTetherApi = {
    app: { version: async () => "0.1.0" },
    updates: {
      // As the real app: updates on macOS and Windows only.
      supported: UPDATES_SUPPORTED,
      async check() {
        updateChecks += 1;
        if (update === "fail") throw new Error("network unreachable");
        return update;
      },
      async install(onProgress) {
        updateInstalls += 1;
        for (const pct of [0, 50, 100]) onProgress?.(pct);
        if (installFailure !== null) throw new Error(installFailure);
      },
    },
    async vaultStatus() {
      return {
        state: password === null ? "uninitialized" : unlocked ? "unlocked" : "locked",
        quickUnlockEnabled: quickUnlock,
        quickUnlockKind,
        error: null,
      };
    },
    async vaultCreate(pw) {
      if (password !== null) throw apiError("alreadyInitialized", "vault already exists");
      password = pw;
      recoveryKey = `MOCK-${hash(pw + newId())}-${hash(newId())}`;
      vaultId = `vault-${newId()}`;
      unlocked = true;
      return { recoveryKey };
    },
    async vaultUnlock(pw) {
      if (password === null) throw apiError("notInitialized", "vault has not been created");
      if (pw !== password) {
        // A device that missed a password change asks the server (when it can).
        const refreshed =
          syncAccount !== null && syncState.state !== "offline" && serverAccount?.password === pw;
        if (!refreshed) throw apiError("wrongPassword", "wrong master password");
        password = pw;
      }
      unlocked = true;
      // Sync waiting to sign in again does so with the password just typed.
      if (syncState.state === "error" && syncState.kind === "signInAgain" && serverAccount?.password === pw) {
        runSync();
      }
    },
    async vaultUnlockRecovery(key, newPassword) {
      if (password === null) throw apiError("notInitialized", "vault has not been created");
      if (key !== recoveryKey) throw apiError("wrongRecoveryKey", "wrong recovery key");
      // With sync on this changes the password everywhere.
      if (syncAccount && serverAccount) serverAccount.password = newPassword;
      password = newPassword;
      unlocked = true;
    },
    async vaultLock() {
      // Like the backend: locking a locked or uncreated vault just succeeds.
      unlocked = false;
      emit("vault-locked", { reason: "manual" } satisfies VaultLocked);
    },
    async vaultEnableQuickUnlock() {
      requireVault();
      failQuickUnlock("enable");
      quickUnlock = true;
    },
    async vaultDisableQuickUnlock() {
      if (password === null) throw apiError("notInitialized", "vault has not been created");
      quickUnlock = false;
    },
    async setDeviceFallback(kind) {
      if (kind !== "iPhone" && kind !== "iPad") throw apiError("invalid", "the device is an iPhone or an iPad");
      deviceFallback = kind;
    },
    async setAutoLock(minutes) {
      if (autoLockFailures > 0) {
        autoLockFailures -= 1;
        throw apiError("internal", "auto-lock is not ready");
      }
      if (minutes !== null && (!Number.isInteger(minutes) || minutes <= 0)) {
        throw apiError("invalid", "auto-lock needs at least one minute");
      }
      autoLockMinutes = minutes;
    },
    async appFront(front) {
      if (!front) {
        // A second report while away keeps the first time.
        leftFrontAt ??= clockNow();
        return !unlocked;
      }
      const since = leftFrontAt;
      leftFrontAt = null;
      if (unlocked && since !== null && autoLockMinutes !== null && clockNow() - since >= autoLockMinutes * 60_000) {
        unlocked = false;
        const send = () => emit("vault-locked", { reason: "idle" } satisfies VaultLocked);
        if (lockEventDelay > 0) setTimeout(send, lockEventDelay);
        else send();
      }
      return !unlocked;
    },
    async vaultQuickUnlock() {
      quickUnlockCalls += 1;
      if (password === null) throw apiError("notInitialized", "vault has not been created");
      if (!quickUnlock) throw apiError("quickUnlockNotEnabled", "quick unlock is not enabled");
      failQuickUnlock("unlock");
      unlocked = true;
    },

    async listEntities(kind) {
      requireVault();
      return list(kind);
    },
    async upsertEntity(kind, input) {
      requireVault();
      return upsert(kind, input);
    },
    async deleteEntity(kind, id) {
      requireVault();
      remove(kind, id);
      pruneOwnLogins();
    },
    async setOwnLogin(login) {
      requireVault();
      for (const key of Object.keys(login)) {
        if (!["ownerId", "username", "password"].includes(key)) {
          throw apiError("invalid", `OwnLoginInput: unknown field \`${key}\``);
        }
      }
      const owner = hosts.get(login.ownerId) ?? groups.get(login.ownerId);
      if (!owner) throw apiError("missingRef", `referenced record ${login.ownerId} does not exist`, [login.ownerId]);
      const linked = identities.get(owner.settings.identityId ?? "");
      const existing = linked?.owner === login.ownerId ? linked : undefined;
      const record: IdentityRecord = {
        id: existing?.id ?? newId(),
        label: "label" in owner ? owner.label : owner.name,
        username: login.username,
        password: login.password === undefined ? (existing?.password ?? null) : login.password,
        keyId: null,
        owner: login.ownerId,
      };
      identities.set(record.id, record);
      owner.settings = { ...owner.settings, identityId: record.id };
      pruneOwnLogins();
      return clone(identityDto(record));
    },
    async deleteGroup(id, policy: ChildPolicy) {
      requireVault();
      const group = groups.get(id);
      if (!group) throw notFound("group", id);
      if (policy === "moveToParent") {
        for (const g of groups.values()) if (g.parentId === id) g.parentId = group.parentId;
        for (const h of hosts.values()) if (h.groupId === id) h.groupId = group.parentId;
      } else {
        const doomed = subtree(id);
        for (const h of [...hosts.values()]) {
          if (h.groupId !== null && doomed.has(h.groupId)) {
            deleteRulesOf(h.id);
            hosts.delete(h.id);
          }
        }
        for (const g of doomed) groups.delete(g);
      }
      groups.delete(id);
      pruneOwnLogins();
    },
    async effectiveSettings(hostId) {
      requireVault();
      return effective(hostId);
    },
    async importSshConfig(path) {
      requireVault();
      const text = readFile(path);
      const report = { hostsAdded: 0, identitiesAdded: 0, keysAdded: 0, keyFailures: [], skipped: [] };
      type Block = { alias: string; opts: Map<string, string> };
      const blocks: Block[] = [];
      for (const raw of text.split("\n")) {
        const line = raw.trim();
        if (!line || line.startsWith("#")) continue;
        const [key, ...rest] = line.split(/\s+/);
        const value = rest.join(" ");
        if (key.toLowerCase() === "host") blocks.push({ alias: value, opts: new Map() });
        else blocks[blocks.length - 1]?.opts.set(key.toLowerCase(), value);
      }
      const existing = new Set([...hosts.values()].map((h) => h.label));
      for (const b of blocks) {
        if (/[*?!]/.test(b.alias)) continue;
        if (b.opts.has("proxyjump") || b.opts.has("proxycommand")) {
          (report.skipped as { name: string; reason: string }[]).push({
            name: b.alias,
            reason: "ProxyJump not supported until Phase 4",
          });
          continue;
        }
        if (existing.has(b.alias)) {
          (report.skipped as { name: string; reason: string }[]).push({
            name: b.alias,
            reason: "a host with this label exists",
          });
          continue;
        }
        let identityId: Id | null = null;
        const user = b.opts.get("user");
        if (user) {
          identityId = newId();
          identities.set(identityId, { id: identityId, label: user, username: user, password: null, keyId: null, owner: null });
          report.identitiesAdded++;
        }
        const id = newId();
        const port = b.opts.get("port");
        hosts.set(id, {
          id,
          groupId: null,
          label: b.alias,
          address: b.opts.get("hostname") ?? b.alias,
          tagIds: [],
          settings: settingsOf({ port: port ? Number(port) : null, identityId }),
          forwardRuleIds: [],
        });
        report.hostsAdded++;
      }
      return report;
    },
    async keyGenerate(algo, label, passphrase) {
      requireVault();
      return storeKey(label, KEY_ALGOS[algo], `mock-${algo}-${newId()}`, passphrase ?? null);
    },
    async keyImport(text, passphrase, label) {
      requireVault();
      const match = /-----BEGIN ([A-Z ]*)PRIVATE KEY-----\n([\s\S]*?)\n-----END/.exec(text);
      if (!match) throw apiError("key", "could not parse key: not a recognised private key");
      let body = match[2];
      // Encrypted input needs its passphrase, which is then kept; for an
      // unencrypted key a passphrase is ignored (as the backend does).
      const marked = body.startsWith(ENCRYPTED);
      const encrypted = marked || match[1].includes("ENCRYPTED");
      if (encrypted && !passphrase) {
        throw apiError("needsPassphrase", "key is encrypted and needs a passphrase");
      }
      if (marked) {
        const [check, ...rest] = body.slice(ENCRYPTED.length).split(":");
        if (check !== hash(passphrase!)) throw apiError("badPassphrase", "wrong passphrase");
        body = rest.join(":");
      }
      const keep = encrypted ? passphrase! : null;
      return storeKey(label || "Imported key", "ssh-ed25519", body, keep);
    },
    async keyExportPrivate(id, pw) {
      requireVault();
      if (pw !== password) throw apiError("wrongPassword", "wrong password");
      const key = keys.get(id);
      if (!key) throw notFound("ssh_key", id);
      return { privateOpenssh: key.privateOpenssh };
    },
    async keySavePrivate(id, pw) {
      requireVault();
      if (pw !== password) throw apiError("wrongPassword", "wrong password");
      const key = keys.get(id);
      if (!key) throw notFound("ssh_key", id);
      if (pickedFile !== null) files.set(pickedFile, key.privateOpenssh);
      return pickedFile;
    },
    async pickFile(options) {
      pickCalls.push(clone(options ?? {}));
      return pickedFile;
    },
    async scanQr() {
      scanCalls += 1;
      return scannedText;
    },
    async cancelScan() {},
    async readClipboard() {
      return clipboardText || null;
    },
    async writeClipboard(text) {
      clipboardText = text;
      return true;
    },
    async knownHostsImport(path) {
      requireVault();
      const text = readFile(path);
      let added = 0;
      let alreadyKnown = 0;
      for (const raw of text.split("\n")) {
        const line = raw.trim();
        if (!line || line.startsWith("#") || line.startsWith("@")) continue;
        const [patterns, algorithm, publicKey] = line.split(/\s+/);
        if (!publicKey) continue;
        for (const pattern of patterns.split(",")) {
          const m = /^\[(.+)\]:(\d+)$/.exec(pattern);
          const hostPattern = (m ? m[1] : pattern).toLowerCase();
          const port = m ? Number(m[2]) : 22;
          const dup = [...knownHosts.values()].some(
            (k) =>
              k.hostPattern === hostPattern &&
              k.port === port &&
              k.algorithm === algorithm &&
              k.publicKey === publicKey,
          );
          if (dup) {
            alreadyKnown++;
            continue;
          }
          upsert("known_host", { hostPattern, port, algorithm, publicKey, source: "import" });
          added++;
        }
      }
      return { added, alreadyKnown };
    },

    async openSession(target, _size, handlers) {
      openTargets.push(clone(target));
      const failure = sessionFailures.shift();
      const hold = sessionHolds.shift();
      // One macrotask apart, so a UI sees each step before the next.
      const report = async (step: ConnectStep) => {
        handlers.onProgress?.(step);
        await new Promise<void>((resolve) => setTimeout(resolve, 0));
      };
      const failAt = async (at: ConnectStep | undefined, message: string): Promise<never> => {
        if (at) {
          for (const step of CONNECT_STEPS.slice(0, CONNECT_STEPS.indexOf(at) + 1)) await report(step);
        }
        throw apiError("connect", message);
      };
      if (failure !== undefined) await failAt(failure.atStep, failure.message);
      let title = "Local shell";
      let hostId: Id | null = null;
      if (target.kind === "host") {
        requireVault();
        const host = hosts.get(target.hostId);
        if (!host) throw notFound("host", target.hostId);
        await report("tcp");
        await hold?.wait;
        await report("hostKey");
        // No identity: the backend uses the local login name and prompts (R31).
        const settings = effective(host.id);
        await verifyHostKey(host.address, settings.port.value);
        await report("auth");
        title = host.label;
        hostId = host.id;
      } else {
        const shellId = target.shellId ?? null;
        if (shellId !== null && !localShells.some((s) => s.id === shellId)) {
          throw apiError("shell", "That shell is no longer available.");
        }
        openedLocalShells.push(shellId);
        if (target.cwd) title = `Local: ${target.cwd}`;
      }
      await report("shell");
      const sessionId = newId();
      const tmux = target.kind === "host" ? (target.reattachTmux ?? null) : null;
      sessions.set(sessionId, { handlers, title, hostId, written: [], tmux });
      return { sessionId };
    },
    async openShell(params, handlers) {
      const sessionId = newId();
      sessions.set(sessionId, {
        handlers,
        title: `${params.username}@${params.host}`,
        hostId: null,
        written: [],
        tmux: null,
      });
      return { sessionId };
    },
    async write(sessionId, data) {
      session(sessionId).written.push(data.slice());
    },
    async resize(sessionId) {
      session(sessionId);
    },
    async ack() {},
    async close(sessionId) {
      endSession(sessionId, null);
    },
    async closeAllSessions() {
      const ids = [...sessions.keys()];
      ids.forEach((id) => endSession(id, null));
      return ids.length;
    },
    async sessionTmux(sessionId) {
      return session(sessionId).tmux;
    },
    async localShells(): Promise<LocalShell[]> {
      return clone(localShells);
    },
    async activeWork(): Promise<ActiveWork> {
      return {
        sessions: [...sessions.entries()].map(([id, s]) => ({ id, title: s.title, hostId: s.hostId })),
        transfers: [...transfers.entries()].map(([jobId, t]) => ({
          jobId,
          hostId: t.hostId,
          direction: t.job.direction,
          local: t.job.local,
          remote: t.job.remote,
        })),
        forwards: [...forwardStatus.entries()]
          .filter(([, status]) => status.state === "starting" || status.state === "active")
          .map(([ruleId, status]) => ({ ruleId, hostId: rules.get(ruleId)?.hostId ?? "", status: clone(status) })),
      };
    },

    async sftpOpen(hostId) {
      requireVault();
      if (!hosts.has(hostId)) throw notFound("host", hostId);
      sftpOpen.add(hostId);
    },
    async sftpClose(hostId) {
      sftpOpen.delete(hostId);
      for (const [jobId, t] of [...transfers]) {
        if (t.hostId === hostId) finishTransfer({ hostId, event: { kind: "cancelled", job: jobId } });
      }
    },
    async sftpList(hostId, path) {
      const tree = requireSftp(hostId);
      const dir = path.replace(/\/+$/, "") || "/";
      const entry = tree.get(dir);
      // A symlink lists like a directory when it has children (the mock has no link targets).
      const linkToDir =
        entry?.isSymlink === true && [...tree.values()].some((e) => parentOf(e.path) === dir);
      if (dir !== "/" && dir !== MOCK_REMOTE_HOME && !entry?.isDir && !linkToDir) {
        throw apiError("sftp", `sftp: listing ${path}: no such file`);
      }
      return [...tree.values()]
        .filter((e) => parentOf(e.path) === dir)
        .sort((a, b) => a.name.localeCompare(b.name))
        .map(clone);
    },
    async sftpMkdir(hostId, path) {
      const tree = requireSftp(hostId);
      if (tree.has(path)) throw apiError("sftp", `sftp: mkdir ${path}: already exists`);
      const name = path.slice(path.lastIndexOf("/") + 1);
      tree.set(path, { name, path, isDir: true, size: 0, mtime: 0, mode: 0o40755, isSymlink: false });
    },
    async sftpRename(hostId, from, to) {
      const tree = requireSftp(hostId);
      const entry = tree.get(from);
      if (!entry) throw apiError("sftp", `sftp: rename ${from}: no such file`);
      tree.delete(from);
      tree.set(to, { ...entry, path: to, name: to.slice(to.lastIndexOf("/") + 1) });
    },
    async sftpRemove(hostId, path, isDir) {
      const tree = requireSftp(hostId);
      const entry = tree.get(path);
      if (!entry || entry.isDir !== isDir) throw apiError("sftp", `sftp: remove ${path}: no such file`);
      if (isDir && [...tree.values()].some((e) => parentOf(e.path) === path)) {
        throw apiError("sftp", `sftp: remove ${path}: directory not empty`);
      }
      tree.delete(path);
    },
    async sftpChmod(hostId, path, mode) {
      const tree = requireSftp(hostId);
      const entry = tree.get(path);
      if (!entry) throw apiError("sftp", `sftp: chmod ${path}: no such file`);
      entry.mode = (entry.mode & ~0o7777) | (mode & 0o7777);
    },
    async sftpRealpath(hostId, path) {
      requireSftp(hostId);
      return canonical(MOCK_REMOTE_HOME, path);
    },
    async localResolve(path) {
      return path === "~" || path.startsWith("~/") ? MOCK_HOME + path.slice(1) : path;
    },
    async localList(path) {
      const expanded = path === "~" || path.startsWith("~/") ? MOCK_HOME + path.slice(1) : path;
      const dir = expanded.replace(/\/+$/, "") || "/";
      if (dir !== "/" && dir !== MOCK_HOME && !local.get(dir)?.isDir) {
        throw apiError("io", `listing ${dir}: No such file or directory (os error 2)`);
      }
      const byName = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);
      return [...local.values()]
        .filter((e) => parentOf(e.path) === dir)
        .sort(
          (a, b) =>
            Number(b.isDir) - Number(a.isDir) ||
            byName(a.name.toLowerCase(), b.name.toLowerCase()) ||
            byName(a.name, b.name),
        )
        .map(clone);
    },
    async localDrives() {
      return [...localDrives];
    },
    async localImport(path) {
      const source = decodeURIComponent(path.replace(/^file:\/\//, ""));
      const text = files.get(source);
      if (text === undefined) throw apiError("invalid", "that is not a file the picker handed over");
      files.delete(source);
      const base = source.slice(source.lastIndexOf("/") + 1);
      const dot = base.lastIndexOf(".");
      const [stem, ext] = dot > 0 ? [base.slice(0, dot), base.slice(dot)] : [base, ""];
      let name = base;
      for (let n = 2; local.has(`${MOCK_HOME}/${name}`); n++) name = `${stem} ${n}${ext}`;
      const dest = `${MOCK_HOME}/${name}`;
      local.set(dest, { name, path: dest, isDir: false, size: text.length, mtime: 0, mode: 0o100644, isSymlink: false });
      return dest;
    },
    async importFromDevice(dir) {
      const hold = devicePickHold;
      devicePickHold = null;
      await hold;
      const picked = devicePick;
      devicePick = [];
      const names: string[] = [];
      for (const base of picked) {
        const dot = base.lastIndexOf(".");
        const [stem, ext] = dot > 0 ? [base.slice(0, dot), base.slice(dot)] : [base, ""];
        let name = base;
        for (let n = 2; local.has(`${dir}/${name}`); n++) name = `${stem} (${n})${ext}`;
        const dest = `${dir}/${name}`;
        local.set(dest, { name, path: dest, isDir: false, size: 0, mtime: 0, mode: 0o100644, isSymlink: false });
        names.push(name);
      }
      return names;
    },
    async saveToDevice(path) {
      if (saveCancelled) {
        saveCancelled = false;
        return false;
      }
      savedToDevice.push(path);
      return true;
    },
    async localMkdir(path) {
      for (let dir = path.replace(/\/+$/, ""); dir && dir !== "/"; dir = parentOf(dir)) {
        if (!local.has(dir)) {
          const name = dir.slice(dir.lastIndexOf("/") + 1);
          local.set(dir, { name, path: dir, isDir: true, size: 0, mtime: 0, mode: 0o40755, isSymlink: false });
        }
      }
    },
    async transferEnqueue(hostId, job) {
      requireSftp(hostId);
      const jobId = newId();
      enqueued.push({ jobId, hostId, job: clone(job) });
      transfers.set(jobId, { hostId, job: clone(job) });
      return { jobId };
    },
    async transferCancel(jobId) {
      const t = transfers.get(jobId);
      if (!t) return false;
      finishTransfer({ hostId: t.hostId, event: { kind: "cancelled", job: jobId } });
      return true;
    },

    async forwardStart(ruleId) {
      requireVault();
      const rule = rules.get(ruleId);
      if (!rule) throw notFound("forward rule", ruleId);
      const current = forwardStatus.get(ruleId);
      if (current?.state === "active" || current?.state === "starting") return clone(current);
      const rejected = forwardRejections.get(ruleId);
      forwardRejections.delete(ruleId);
      if (rejected !== undefined) throw apiError("connect", rejected);
      setForward(ruleId, { state: "starting" });
      const failure = forwardFailures.get(ruleId) ?? (bindConflict(rule) ? "address in use" : undefined);
      forwardFailures.delete(ruleId);
      const status: ForwardStatus = failure
        ? { state: "error", detail: failure }
        : { state: "active", detail: { bound: `${rule.bindAddr}:${rule.bindPort}` } };
      setForward(ruleId, status);
      return clone(status);
    },
    async forwardStop(ruleId) {
      if (forwardStatus.has(ruleId)) setForward(ruleId, { state: "stopped" });
    },
    async forwardStatuses() {
      return [...forwardStatus.entries()].map(([ruleId, status]) => ({
        ruleId,
        hostId: rules.get(ruleId)?.hostId ?? "",
        status: clone(status),
      }));
    },

    async keepAliveSet(enabled) {
      if (enabled && locationPermission === "notDetermined") {
        locationAsks += 1;
        locationPermission = locationAnswer === "allow" ? "granted" : "denied";
      }
      keepAliveOn = enabled && locationPermission === "granted";
      reportKeepAlive();
      return keepAliveStatus();
    },
    async keepAliveStatus() {
      return keepAliveStatus();
    },
    onKeepAlive: (cb) => on<KeepAliveStatus>("keepalive", cb),

    async promptRespond(id, response) {
      const resolve = pendingPrompts.get(id);
      pendingPrompts.delete(id);
      resolve?.(clone(response));
    },

    onPrompt: (cb) => on<PromptEvent>("prompt", cb),
    onPromptWithdrawn: (cb) => on<PromptWithdrawn>("prompt-withdrawn", cb),
    onTransfer: (cb) => on<TransferEventPayload>("transfer", cb),
    onForward: (cb) => on<ForwardEvent>("forward", cb),
    onVaultLocked: (cb) => on<VaultLocked>("vault-locked", cb),
    onVaultChanged: (cb) => on<unknown>("vault-changed", () => cb()),

    sync: {
      async status() {
        return syncStatus();
      },
      async parseCode(code) {
        failSync("parseCode");
        return parseCode(code);
      },
      async checkServer(address, code) {
        failSync("checkServer");
        const info = checkedServer(address);
        if (parseCode(code).kind !== "short") throw apiError("invalidCode", "invalid code: not a short code");
        return info;
      },
      async enable(code, username, pw, recovery, address) {
        try {
          return await enableSync(code, username, pw, recovery, address);
        } catch (e) {
          // Like the backend, every rejection is an EnableError.
          throw isApiError(e) && !("newRecoveryKey" in e) ? { ...e, newRecoveryKey: null } : e;
        }
      },
      async join(code, username, pw, replaceExisting) {
        failSync("join");
        const info = parseCode(code);
        if (info.kind !== "link") {
          throw apiError("invalidCode", "invalid code: this is an invite code; joining a synced vault needs an add-device code");
        }
        // The backend signs in first: an unknown account and a wrong password look the same.
        if (serverAccount && (serverAccount.username !== username.trim() || serverAccount.password !== pw)) {
          throw apiError("wrongPassword", "wrong password");
        }
        const exists = password !== null;
        // This device's own vault is reconnected and keeps its records; another one is replaced.
        const same = exists && serverAccount !== null && serverAccount.vaultId === vaultId;
        if (exists && !same && !replaceExisting) throw apiError("vaultExists", "this device already has a vault");
        let backup: string | null = null;
        if (exists && !same) {
          backup = "/home/mock/Library/vault.sqlite.before-join-2026-10-21";
          for (const m of [groups, hosts, tags, identities, keys, knownHosts, rules]) m.clear();
          conflictRecords.clear();
          quickUnlock = false;
        }
        password = pw;
        recoveryKey = serverAccount?.recoveryKey ?? `MOCK-${hash(pw + newId())}-${hash(newId())}`;
        unlocked = true;
        serverAccount ??= { username: username.trim(), password: pw, recoveryKey, vaultId: `vault-${newId()}` };
        vaultId = serverAccount.vaultId;
        startSync(info, username.trim());
        return { backup, reconnected: same };
      },
      async recover(code, username, key, newPassword) {
        failSync("recover");
        const info = parseCode(code);
        if (!serverAccount || serverAccount.username !== username.trim() || serverAccount.recoveryKey !== key) {
          throw apiError("wrongRecoveryKey", "wrong recovery key");
        }
        changePasswordElsewhere(newPassword, !syncAccount);
        return { needsDeviceCode: true, server: info.server };
      },
      async now() {
        failSync("now");
        requireSync();
        syncNowCalls += 1;
        runSync();
      },
      async signIn(pw) {
        failSync("signIn");
        requireSync();
        if (serverAccount && serverAccount.password !== pw) throw apiError("wrongPassword", "wrong password");
        runSync();
      },
      async turnOff() {
        failSync("turnOff");
        syncAccount = null;
        devices = [];
        setSyncState({ state: "off" });
      },
      async linkCode(address) {
        failSync("linkCode");
        requireSync();
        const server = address === undefined ? syncAccount!.server : mockAddress(address);
        linkCounter += 1;
        const code = `tether1-link-mock-${linkCounter}`;
        codeInfos.set(code, { kind: "link", server, fingerprint: MOCK_FINGERPRINT });
        return { code, server, expiresAt: Date.now() + 10 * 60 * 1000 };
      },
      async devices() {
        failSync("devices");
        requireSync();
        // This device's name can change after sync started (the iOS fallback arrives at start-up).
        return clone(devices.map((d) => (d.current ? { ...d, ...thisDevice(), lastSeen: d.lastSeen } : d)));
      },
      async renameDevice(id, name) {
        failSync("renameDevice");
        requireSync();
        const device = devices.find((d) => d.id === id);
        if (!device) throw notFound("device", id);
        device.name = name;
      },
      async revokeDevice(id) {
        failSync("revokeDevice");
        requireSync();
        const device = devices.find((d) => d.id === id);
        if (!device) throw notFound("device", id);
        devices = devices.filter((d) => d.id !== id);
        if (device.current) setSyncState({ state: "error", kind: "revoked", message: "device revoked" });
      },
      onStatus: (cb) => on<SyncStatus>("sync-status", cb),
    },

    conflicts: {
      async list() {
        requireVault();
        return clone([...conflictRecords.values()].map((c) => c.view));
      },
      async resolve(id, resolution) {
        requireVault();
        const { view } = conflictRecord(id);
        if ("fields" in resolution) {
          const known = new Set([...Object.keys(view.current ?? {}), ...Object.keys(view.other ?? {})]);
          const unknown = resolution.fields.find((f) => !known.has(f));
          if (unknown !== undefined) throw apiError("invalid", `${view.kind}: unknown field \`${unknown}\``);
        }
        conflictRecords.delete(id);
        resolvedConflicts.push({ id, resolution: clone(resolution) });
      },
      async reveal(id, side: ConflictSide, field) {
        requireVault();
        const { view, raw } = conflictRecord(id);
        if (!(SECRET_FIELDS[view.kind] ?? []).includes(field)) throw apiError("invalid", "not a secret field");
        const record = raw[side];
        if (record === null) throw apiError("invalid", "that version is a deletion");
        const value = record[field];
        if (typeof value !== "string") throw apiError("invalid", "no secret is stored in that field");
        return value;
      },
    },

    vault: {
      async changePassword(oldPassword, newPassword) {
        failSync("changePassword");
        if (password === null) throw apiError("notInitialized", "vault has not been created");
        if (oldPassword !== password) throw apiError("wrongPassword", "wrong password");
        if (syncAccount) {
          // With sync on the server hears of it first.
          if (syncState.state === "offline") throw apiError("unreachable", "server unreachable");
          if (serverAccount) serverAccount.password = newPassword;
        }
        password = newPassword;
      },
    },

    async onCloseRequested(cb) {
      closeHandlers.add(cb);
      return () => {
        closeHandlers.delete(cb);
      };
    },
    async destroyWindow() {
      windowClosed = true;
    },

    onBackButton: (cb) => on<null>("back", () => cb()),
    async moveToBackground() {
      backgrounded++;
    },

    control: {
      ask,
      withdraw(id) {
        const resolve = pendingPrompts.get(id);
        if (!resolve) return;
        pendingPrompts.delete(id);
        resolve("reject");
        emit("prompt-withdrawn", { id });
      },
      output(sessionId, bytes) {
        session(sessionId).handlers.onOutput(bytes.slice());
      },
      endSession(sessionId, reason = null, exitStatus = null) {
        endSession(sessionId, reason, exitStatus);
      },
      openedLocalShells() {
        return [...openedLocalShells];
      },
      openTargets() {
        return clone(openTargets);
      },
      setTmux(sessionId, name) {
        session(sessionId).tmux = name;
      },
      written(sessionId) {
        return sessions.get(sessionId)?.written.map((b) => b.slice()) ?? [];
      },
      emitTransfer: finishTransfer,
      emitForward(event) {
        setForward(event.ruleId, event.status);
      },
      failNextOpenSession(message, atStep) {
        sessionFailures.push({ message, atStep });
      },
      holdNextOpenSession() {
        let release!: () => void;
        let fail!: (message: string) => void;
        const wait = new Promise<void>((resolve, reject) => {
          release = resolve;
          fail = (message) => reject(apiError("connect", message));
        });
        // A held open that is never failed must not raise an unhandled rejection.
        wait.catch(() => {});
        sessionHolds.push({ release, fail, wait });
        return { release, fail };
      },
      failNextForward(ruleId, error) {
        forwardFailures.set(ruleId, error);
      },
      rejectNextForwardStart(ruleId, message) {
        forwardRejections.set(ruleId, message);
      },
      setFile(path, text) {
        files.set(path, text);
      },
      setPick(path) {
        pickedFile = path;
      },
      setDevicePick(names) {
        devicePick = [...names];
      },
      holdNextDevicePick() {
        let release!: () => void;
        devicePickHold = new Promise<void>((resolve) => (release = resolve));
        return { release };
      },
      cancelNextSave() {
        saveCancelled = true;
      },
      savedToDevice() {
        return [...savedToDevice];
      },
      pickCalls() {
        return clone(pickCalls);
      },
      setScan(text) {
        scannedText = text;
      },
      scanCalls() {
        return scanCalls;
      },
      setClipboard(text) {
        clipboardText = text;
      },
      clipboard() {
        return clipboardText;
      },
      failQuickUnlock(op, message, code = "keychain") {
        quickUnlockFailures.set(op, { message, code });
      },
      advanceClock(ms) {
        clockOffset += ms;
      },
      delayLockEvents(ms) {
        lockEventDelay = ms;
      },
      autoLockMinutes() {
        return autoLockMinutes;
      },
      deviceFallback() {
        return deviceFallback;
      },
      answerLocationWith(answer) {
        locationAnswer = answer;
      },
      locationAsks() {
        return locationAsks;
      },
      setLocationPermission(permission) {
        locationPermission = permission;
        if (permission !== "granted") keepAliveOn = false;
        reportKeepAlive();
      },
      quickUnlockCalls() {
        return quickUnlockCalls;
      },
      addRemote(hostId, entry) {
        const tree = remote.get(hostId) ?? new Map<string, RemoteEntry>();
        remote.set(hostId, tree);
        tree.set(entry.path, clone(entry));
      },
      addLocal(entry) {
        local.set(entry.path, clone(entry));
      },
      enqueued() {
        return clone(enqueued);
      },
      setServerKey(address, port, algorithm, publicKey) {
        const key: HostKeyInfo = {
          algorithm,
          sha256Fingerprint: `SHA256:${hash(publicKey)}`,
          openssh: `${algorithm} ${publicKey}`,
        };
        serverKeys.set(`${address}:${port}`, key);
        return clone(key);
      },
      pendingPrompts() {
        return [...pendingPrompts.keys()];
      },
      async requestClose() {
        let prevented = false;
        const event: CloseRequest = {
          preventDefault: () => {
            prevented = true;
          },
        };
        for (const cb of [...closeHandlers]) await cb(event);
        if (!prevented) windowClosed = true;
      },
      windowClosed() {
        return windowClosed;
      },
      backButton() {
        emit("back", null);
      },
      backgrounded() {
        return backgrounded;
      },
      setSyncStatus: setSyncState,
      syncNowCalls() {
        return syncNowCalls;
      },
      addDevice(device) {
        const added: Device = {
          name: "Another device",
          platform: "linux",
          createdAt: 1_700_000_000_000,
          lastSeen: Date.now(),
          current: false,
          ...device,
        };
        devices = [...devices.filter((d) => d.id !== added.id), added];
        return clone(added);
      },
      setCodeInfo(code, info) {
        codeInfos.set(code.trim(), clone(info));
      },
      setServerAccount(account) {
        serverAccount = account === null ? null : { vaultId: `vault-${newId()}`, ...account };
      },
      localVaultId() {
        return vaultId;
      },
      failNextEnableAfterRegister(code, message) {
        lateEnableFailure = { code, message };
      },
      failNextSync(op, code, message) {
        syncFailures.set(op, { code, message });
      },
      changePasswordElsewhere(newPassword, options = {}) {
        changePasswordElsewhere(newPassword, options.stale ?? false);
      },
      addConflict(input) {
        const raw = { current: clone(input.current), other: clone(input.other) };
        const keys = new Set([...Object.keys(raw.current ?? {}), ...Object.keys(raw.other ?? {})]);
        const changedFields = [...keys]
          .filter((k) => JSON.stringify(raw.current?.[k]) !== JSON.stringify(raw.other?.[k]))
          .sort();
        const view: Conflict = {
          id: newId(),
          recordId: input.recordId ?? ((input.current ?? input.other)?.id as Id | undefined) ?? newId(),
          kind: input.kind,
          current: maskSecrets(input.kind, raw.current),
          other: maskSecrets(input.kind, raw.other),
          changedFields,
          detectedAt: input.detectedAt ?? Math.floor(Date.now() / 1000),
        };
        conflictRecords.set(view.id, { view, raw });
        return clone(view);
      },
      resolvedConflicts() {
        return clone(resolvedConflicts);
      },
      remoteUpsert(kind, input) {
        const record = upsert(kind, input);
        emitVaultChanged();
        return record;
      },
      remoteDelete(kind, id) {
        remove(kind, id);
        pruneOwnLogins();
        emitVaultChanged();
      },
      emitVaultChanged,
      vaultChangedCount() {
        return vaultChanged;
      },
      setUpdate(next) {
        update = next;
      },
      updateChecks: () => updateChecks,
      updateInstalls: () => updateInstalls,
      failUpdateInstall(message) {
        installFailure = message;
      },
    },
  };
  return api;
}
