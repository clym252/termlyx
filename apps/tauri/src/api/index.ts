/**
 * The UI's backend API. `api` is the Tauri implementation, or the in-memory
 * mock when the app is built with `VITE_TETHER_API=mock` (Playwright).
 *
 * Commands reject with an `ApiError` (`{code, message, refs}`). Secrets the
 * UI passes in (passwords, passphrases, key text) go straight to Rust,
 * where they deserialize into `SecretString`; callers should not keep them
 * in React state.
 */

import { Channel, invoke } from "@tauri-apps/api/core";
import { getVersion, onBackButtonPress } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Format, cancel, checkPermissions, requestPermissions, scan } from "@tauri-apps/plugin-barcode-scanner";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open } from "@tauri-apps/plugin-dialog";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { IS_MOBILE, IS_WINDOWS, UPDATES_SUPPORTED } from "../platform";
import { createMockApi, type MockSeed } from "./mock";
import {
  EVENTS,
  SESSION_ID_HEADER,
  type ConnectStep,
  type ForwardEvent,
  type KeepAliveStatus,
  type PromptEvent,
  type PromptWithdrawn,
  type SessionClosed,
  type SessionHandlers,
  type SyncStatus,
  type TetherApi,
  type TransferEventPayload,
  type Unlisten,
  type VaultLocked,
} from "./types";

export * from "./types";
export { createMockApi, type MockControls, type MockSeed, type MockTetherApi } from "./mock";

/** Output frames arrive as ArrayBuffer (raw IPC); accept the other shapes defensively. */
export function toBytes(frame: ArrayBuffer | Uint8Array | number[]): Uint8Array {
  if (frame instanceof Uint8Array) return frame;
  if (frame instanceof ArrayBuffer) return new Uint8Array(frame);
  return Uint8Array.from(frame);
}

function sessionChannels(handlers: SessionHandlers) {
  const onOutput = new Channel<ArrayBuffer>();
  onOutput.onmessage = (frame) => handlers.onOutput(toBytes(frame));
  const onClosed = new Channel<SessionClosed>();
  onClosed.onmessage = (end) => handlers.onClosed(end?.reason ?? null, end?.exitStatus ?? null);
  const onProgress = new Channel<ConnectStep>();
  onProgress.onmessage = (step) => handlers.onProgress?.(step);
  return { onOutput, onClosed, onProgress };
}

function subscribe<T>(event: string, cb: (payload: T) => void): Promise<Unlisten> {
  return listen<T>(event, (e) => cb(e.payload));
}

/** The real backend: Tauri commands and events. */
/** `mobile`: the clipboard goes through the native plugin (registered on iOS and Android only, `lib.rs`) rather than the browser's. */
export function createTauriApi(mobile: boolean = IS_MOBILE): TetherApi {
  // The release the last check found (its handle on the Rust side), for install.
  let found: Update | null = null;
  return {
    app: { version: () => getVersion() },
    updates: {
      // The plugins are registered on desktop only (lib.rs); updates are released for macOS and Windows.
      supported: UPDATES_SUPPORTED,
      async check() {
        const next = await check();
        void found?.close().catch(() => {});
        found = next;
        return next && { version: next.version, notes: next.body ?? "", date: next.date ?? null };
      },
      async install(onProgress) {
        if (!found) throw new Error("no update to install");
        let total = 0;
        let got = 0;
        await found.download((e) => {
          if (e.event === "Started") total = e.data.contentLength ?? 0;
          else if (e.event === "Progress" && total > 0) {
            got += e.data.chunkLength;
            onProgress?.(Math.min(100, Math.floor((got * 100) / total)));
          } else if (e.event === "Finished") onProgress?.(100);
        });
        // Windows: the installer ends the process (no RunEvent::Exit), so the sync engine stops first.
        if (IS_WINDOWS) await invoke("sync_pause", { paused: true });
        try {
          await found.install();
        } catch (e) {
          if (IS_WINDOWS) await invoke("sync_pause", { paused: false }).catch(() => {});
          throw e;
        }
        await relaunch();
      },
    },
    vaultStatus: () => invoke("vault_status"),
    vaultCreate: (password) => invoke("vault_create", { password }),
    vaultUnlock: (password) => invoke("vault_unlock", { password }),
    vaultUnlockRecovery: (recoveryKey, newPassword) =>
      invoke("vault_unlock_recovery", { recoveryKey, newPassword }),
    vaultLock: () => invoke("vault_lock"),
    vaultEnableQuickUnlock: () => invoke("vault_enable_quick_unlock"),
    vaultDisableQuickUnlock: () => invoke("vault_disable_quick_unlock"),
    vaultQuickUnlock: () => invoke("vault_quick_unlock"),
    setAutoLock: (minutes) => invoke("set_auto_lock", { minutes }),
    appFront: (front) => invoke("app_front", { front }),
    setDeviceFallback: (kind) => invoke("set_device_fallback", { kind }),

    listEntities: (kind) => invoke("list_entities", { kind }),
    upsertEntity: (kind, input) => invoke("upsert_entity", { kind, json: input }),
    deleteEntity: (kind, id) => invoke("delete_entity", { kind, id }),
    deleteGroup: (id, policy) => invoke("delete_group", { id, policy }),
    setOwnLogin: (login) => invoke("set_own_login", { login }),
    effectiveSettings: (hostId) => invoke("effective_settings", { hostId }),
    importSshConfig: (path) => invoke("import_ssh_config", { path }),
    keyGenerate: (algo, label, passphrase) =>
      invoke("key_generate", { algo, label, passphrase: passphrase ?? null }),
    keyImport: (text, passphrase, label) =>
      invoke("key_import", { text, passphrase: passphrase ?? null, label: label ?? null }),
    keyExportPrivate: (id, password) => invoke("key_export_private", { id, password }),
    keySavePrivate: (id, password) => invoke("key_save_private", { id, password }),
    knownHostsImport: (path) => invoke("known_hosts_import", { path }),
    pickFile: (options) => open({ multiple: false, directory: false, ...options }),
    scanQr: async () => {
      try {
        // Asks on first use (the system prompt); a refusal ends the scan quietly.
        let state = await checkPermissions();
        if (state !== "granted") state = await requestPermissions();
        if (state !== "granted") return null;
        // Windowed: the camera sits behind the page, so the page can offer Cancel.
        const scanned = await scan({ formats: [Format.QRCode], windowed: true });
        return scanned.content;
      } catch {
        // Cancelled, or no camera. The error text is not shown: it could carry the scan.
        return null;
      }
    },
    cancelScan: () => cancel().catch(() => undefined),
    // The text is never logged, nor the error (it could quote it).
    readClipboard: async () => {
      try {
        const text = mobile ? await readText() : await navigator.clipboard.readText();
        return text || null;
      } catch {
        // Empty, not text, or the paste was refused.
        return null;
      }
    },
    writeClipboard: async (text) => {
      try {
        await (mobile ? writeText(text) : navigator.clipboard.writeText(text));
        return true;
      } catch {
        return false;
      }
    },

    openSession: (target, size, handlers) =>
      invoke("open_session", { target, cols: size.cols, rows: size.rows, ...sessionChannels(handlers) }),
    openShell: (params, handlers) => invoke("open_shell", { params, ...sessionChannels(handlers) }),
    write: (sessionId, data) =>
      invoke("write_shell", data, { headers: { [SESSION_ID_HEADER]: sessionId } }),
    resize: (sessionId, cols, rows) => invoke("resize_shell", { id: sessionId, cols, rows }),
    ack: (sessionId, bytes) => invoke("ack_shell", { id: sessionId, bytes }),
    close: (sessionId) => invoke("close_shell", { id: sessionId }),
    closeAllSessions: () => invoke("close_all_sessions"),
    activeWork: () => invoke("active_work"),
    sessionTmux: (sessionId) => invoke("session_tmux", { id: sessionId }),
    localShells: () => invoke("local_shells"),

    sftpOpen: (hostId) => invoke("sftp_open", { hostId }),
    sftpClose: (hostId) => invoke("sftp_close", { hostId }),
    sftpList: (hostId, path) => invoke("sftp_list", { hostId, path }),
    sftpMkdir: (hostId, path) => invoke("sftp_mkdir", { hostId, path }),
    sftpRename: (hostId, from, to) => invoke("sftp_rename", { hostId, from, to }),
    sftpRemove: (hostId, path, isDir) => invoke("sftp_remove", { hostId, path, isDir }),
    sftpChmod: (hostId, path, mode) => invoke("sftp_chmod", { hostId, path, mode }),
    localList: (path) => invoke("local_list", { path }),
    localMkdir: (path) => invoke("local_mkdir", { path }),
    localResolve: (path) => invoke("local_resolve", { path }),
    localDrives: () => invoke("local_drives"),
    localImport: (path) => invoke("local_import", { path }),
    importFromDevice: (dir) => invoke("local_import_from_device", { dir }),
    saveToDevice: (path) => invoke("local_save_to_device", { path }),
    sftpRealpath: (hostId, path) => invoke("sftp_realpath", { hostId, path }),
    transferEnqueue: (hostId, job) => invoke("transfer_enqueue", { hostId, job }),
    transferCancel: (jobId) => invoke("transfer_cancel", { jobId }),

    forwardStart: (ruleId) => invoke("forward_start", { ruleId }),
    forwardStop: (ruleId) => invoke("forward_stop", { ruleId }),
    forwardStatuses: () => invoke("forward_statuses"),

    keepAliveSet: (enabled) => invoke("keepalive_set", { enabled }),
    keepAliveStatus: () => invoke("keepalive_status"),
    onKeepAlive: (cb) => subscribe<KeepAliveStatus>(EVENTS.keepAlive, cb),

    promptRespond: (id, response) => invoke("prompt_respond", { id, response }),

    onPrompt: (cb) => subscribe<PromptEvent>(EVENTS.prompt, cb),
    onPromptWithdrawn: (cb) => subscribe<PromptWithdrawn>(EVENTS.promptWithdrawn, cb),
    onTransfer: (cb) => subscribe<TransferEventPayload>(EVENTS.transfer, cb),
    onForward: (cb) => subscribe<ForwardEvent>(EVENTS.forward, cb),
    onVaultLocked: (cb) => subscribe<VaultLocked>(EVENTS.vaultLocked, cb),
    onVaultChanged: (cb) => subscribe<unknown>(EVENTS.vaultChanged, () => cb()),

    sync: {
      status: () => invoke("sync_status"),
      parseCode: (code) => invoke("sync_parse_code", { code }),
      checkServer: (address, code) => invoke("sync_check_server", { address, code }),
      enable: (code, username, password, recovery, address) =>
        invoke("sync_enable", { code, username, password, recovery, address }),
      join: (code, username, password, replaceExisting) =>
        invoke("sync_join", { code, username, password, replaceExisting }),
      recover: (code, username, recoveryKey, newPassword) =>
        invoke("sync_recover", { code, username, recoveryKey, newPassword }),
      now: () => invoke("sync_now"),
      signIn: (password) => invoke("sync_sign_in", { password }),
      turnOff: () => invoke("sync_turn_off"),
      linkCode: (address) => invoke("sync_link_code", { address }),
      devices: () => invoke("sync_devices"),
      renameDevice: (id, name) => invoke("sync_rename_device", { id, name }),
      revokeDevice: (id) => invoke("sync_revoke_device", { id }),
      onStatus: (cb) => subscribe<SyncStatus>(EVENTS.syncStatus, cb),
    },
    conflicts: {
      list: () => invoke("conflicts_list"),
      resolve: (id, resolution) => invoke("conflicts_resolve", { id, resolution }),
      reveal: (id, side, field) => invoke("conflicts_reveal", { id, side, field }),
    },
    vault: {
      changePassword: (oldPassword, newPassword) =>
        invoke("vault_change_password", { old: oldPassword, new: newPassword }),
    },

    onCloseRequested: (cb) => getCurrentWindow().onCloseRequested(cb),
    destroyWindow: () => getCurrentWindow().destroy(),

    async onBackButton(cb) {
      const listener = await onBackButtonPress(() => cb());
      return () => void listener.unregister();
    },
    moveToBackground: () => invoke("move_to_background"),
  };
}

function createAppApi(): TetherApi {
  if (import.meta.env.VITE_TETHER_API !== "mock") return createTauriApi();
  const seed = (window as { __tetherMockSeed?: MockSeed }).__tetherMockSeed;
  const mock = createMockApi(seed);
  // Playwright plays the backend's part through it (e2e/helpers.ts).
  Object.assign(window, { __tetherMock: mock });
  return mock;
}

/** The app-wide API instance. */
export const api: TetherApi = createAppApi();
