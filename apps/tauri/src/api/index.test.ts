import { beforeEach, describe, expect, it, vi } from "vitest";

const channels: Array<{ onmessage: (m: unknown) => void }> = [];
const listeners = new Map<string, (e: { payload: unknown }) => void>();

vi.mock("@tauri-apps/api/core", () => {
  class Channel {
    onmessage: (m: unknown) => void = () => {};
    constructor() {
      channels.push(this);
    }
  }
  return { Channel, invoke: vi.fn() };
});

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    listeners.set(name, cb);
    return () => listeners.delete(name);
  }),
}));

const backListeners: Array<() => void> = [];
const unregister = vi.fn();
vi.mock("@tauri-apps/api/app", () => ({
  onBackButtonPress: vi.fn(async (cb: () => void) => {
    backListeners.push(cb);
    return { unregister };
  }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({ readText: vi.fn(), writeText: vi.fn() }));
vi.mock("@tauri-apps/plugin-barcode-scanner", () => ({
  Format: { QRCode: "QR_CODE" },
  checkPermissions: vi.fn(),
  requestPermissions: vi.fn(),
  scan: vi.fn(),
  cancel: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn() }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import { cancel, checkPermissions, requestPermissions, scan } from "@tauri-apps/plugin-barcode-scanner";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open } from "@tauri-apps/plugin-dialog";
import { createTauriApi } from "./index";

const invokeMock = vi.mocked(invoke);
const openMock = vi.mocked(open);
const scanner = {
  check: vi.mocked(checkPermissions),
  request: vi.mocked(requestPermissions),
  scan: vi.mocked(scan),
  cancel: vi.mocked(cancel),
};

beforeEach(() => {
  vi.mocked(relaunch).mockClear();
  channels.length = 0;
  listeners.clear();
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
});

describe("createTauriApi", () => {
  it("write_sends_raw_bytes_and_session_header", async () => {
    const api = createTauriApi();
    const bytes = new Uint8Array([0, 0xff, 0x1b]);
    await api.write("sess-9", bytes);
    expect(invokeMock).toHaveBeenCalledWith("write_shell", bytes, {
      headers: { "tether-session-id": "sess-9" },
    });
  });

  it("open_session_wires_output_closed_and_progress_channels", async () => {
    invokeMock.mockResolvedValueOnce({ sessionId: "s1" });
    const api = createTauriApi();
    const out: Uint8Array[] = [];
    const closed: [string | null, number | null][] = [];
    const steps: string[] = [];
    const opened = await api.openSession(
      { kind: "host", hostId: "h1" },
      { cols: 100, rows: 30 },
      {
        onOutput: (b) => out.push(b),
        onClosed: (r, status) => closed.push([r, status]),
        onProgress: (step) => steps.push(step),
      },
    );
    expect(opened).toEqual({ sessionId: "s1" });
    const [cmd, args] = invokeMock.mock.calls[0];
    expect(cmd).toBe("open_session");
    expect(args).toMatchObject({ target: { kind: "host", hostId: "h1" }, cols: 100, rows: 30 });
    const [output, onClosed, onProgress] = channels;
    output.onmessage(new Uint8Array([104, 105]).buffer);
    onClosed.onmessage({ reason: null, exitStatus: 3 });
    onProgress.onmessage("hostKey");
    expect(out).toEqual([new Uint8Array([104, 105])]);
    expect(closed).toEqual([[null, 3]]);
    expect(steps).toEqual(["hostKey"]);
    expect(args).toHaveProperty("onProgress");
  });

  it("commands_use_camel_case_argument_names", async () => {
    const api = createTauriApi();
    await api.vaultUnlockRecovery("rk", "pw");
    await api.upsertEntity("tag", { name: "t", color: "#fff" });
    await api.sftpRemove("h", "/a b", true);
    await api.transferCancel("j");
    expect(invokeMock.mock.calls).toEqual([
      ["vault_unlock_recovery", { recoveryKey: "rk", newPassword: "pw" }],
      ["upsert_entity", { kind: "tag", json: { name: "t", color: "#fff" } }],
      ["sftp_remove", { hostId: "h", path: "/a b", isDir: true }],
      ["transfer_cancel", { jobId: "j" }],
    ]);
  });

  it("sync_conflict_and_password_commands_use_the_rust_argument_names", async () => {
    const api = createTauriApi();
    await api.sync.enable("c", "alice", "pw", { new: true });
    await api.sync.enable("c", "alice", "pw", { new: true }, "nas.local");
    await api.sync.checkServer("nas.local", "c");
    await api.sync.linkCode("nas.local");
    await api.sync.join("c", "alice", "pw", true);
    await api.sync.recover("c", "alice", "rk", "np");
    await api.sync.renameDevice("d", "n");
    await api.sync.revokeDevice("d");
    await api.sync.parseCode("c");
    await api.sync.now();
    await api.sync.signIn("pw");
    await api.conflicts.resolve("x", { fields: ["label"] });
    await api.conflicts.reveal("x", "other", "password");
    await api.vault.changePassword("old", "new");
    expect(invokeMock.mock.calls).toEqual([
      ["sync_enable", { code: "c", username: "alice", password: "pw", recovery: { new: true } }],
      [
        "sync_enable",
        { code: "c", username: "alice", password: "pw", recovery: { new: true }, address: "nas.local" },
      ],
      ["sync_check_server", { address: "nas.local", code: "c" }],
      ["sync_link_code", { address: "nas.local" }],
      ["sync_join", { code: "c", username: "alice", password: "pw", replaceExisting: true }],
      ["sync_recover", { code: "c", username: "alice", recoveryKey: "rk", newPassword: "np" }],
      ["sync_rename_device", { id: "d", name: "n" }],
      ["sync_revoke_device", { id: "d" }],
      ["sync_parse_code", { code: "c" }],
      ["sync_now"],
      ["sync_sign_in", { password: "pw" }],
      ["conflicts_resolve", { id: "x", resolution: { fields: ["label"] } }],
      ["conflicts_reveal", { id: "x", side: "other", field: "password" }],
      ["vault_change_password", { old: "old", new: "new" }],
    ]);
  });

  it("auto_lock_commands_use_the_rust_argument_names", async () => {
    const api = createTauriApi();
    invokeMock.mockResolvedValueOnce(undefined).mockResolvedValueOnce(undefined).mockResolvedValueOnce(true);
    await api.setAutoLock(30);
    await api.setAutoLock(null);
    expect(await api.appFront(true)).toBe(true);
    expect(invokeMock.mock.calls).toEqual([
      ["set_auto_lock", { minutes: 30 }],
      ["set_auto_lock", { minutes: null }],
      ["app_front", { front: true }],
    ]);
  });

  it("device_fallback_uses_the_rust_name", async () => {
    const api = createTauriApi();
    invokeMock.mockResolvedValueOnce(undefined);
    await api.setDeviceFallback("iPad");
    expect(invokeMock.mock.calls).toEqual([["set_device_fallback", { kind: "iPad" }]]);
  });

  it("keepalive_commands_use_the_rust_names", async () => {
    const api = createTauriApi();
    const status = { enabled: true, authorized: true, running: false };
    invokeMock.mockResolvedValueOnce(status).mockResolvedValueOnce(status);
    expect(await api.keepAliveSet(true)).toEqual(status);
    expect(await api.keepAliveStatus()).toEqual(status);
    expect(invokeMock.mock.calls).toEqual([["keepalive_set", { enabled: true }], ["keepalive_status"]]);
    const seen: unknown[] = [];
    await api.onKeepAlive((e) => seen.push(e));
    listeners.get("keepalive")!({ payload: status });
    expect(seen).toEqual([status]);
  });

  it("sync_status_and_vault_locked_events_deliver_their_payloads", async () => {
    const api = createTauriApi();
    const status: unknown[] = [];
    const locks: unknown[] = [];
    await api.sync.onStatus((e) => status.push(e));
    await api.onVaultLocked((e) => locks.push(e));
    listeners.get("sync-status")!({ payload: { account: null, status: { state: "off" } } });
    listeners.get("vault-locked")!({ payload: { reason: "passwordChanged" } });
    expect(status).toEqual([{ account: null, status: { state: "off" } }]);
    expect(locks).toEqual([{ reason: "passwordChanged" }]);
  });

  it("vault_changed_event_calls_back_without_a_payload", async () => {
    const api = createTauriApi();
    const calls: unknown[][] = [];
    await api.onVaultChanged((...args) => calls.push(args));
    listeners.get("vault-changed")!({ payload: {} });
    expect(calls).toEqual([[]]);
  });

  it("back_button_registers_a_listener_and_move_to_background_is_the_plugin_command", async () => {
    const api = createTauriApi();
    let presses = 0;
    const off = await api.onBackButton(() => presses++);
    backListeners[0]!();
    expect(presses).toBe(1);
    off();
    expect(unregister).toHaveBeenCalledTimes(1);
    await api.moveToBackground();
    expect(invokeMock.mock.calls).toEqual([["move_to_background"]]);
  });

  it("pick_file_asks_for_one_file_and_passes_the_path_through", async () => {
    const api = createTauriApi();
    openMock.mockResolvedValueOnce("/home/me/.ssh/known_hosts");
    expect(await api.pickFile({ title: "Import" })).toBe("/home/me/.ssh/known_hosts");
    expect(openMock).toHaveBeenCalledWith({ multiple: false, directory: false, title: "Import" });
    openMock.mockResolvedValueOnce(null);
    expect(await api.pickFile()).toBeNull();
  });

  it("scan_qr_asks_for_the_camera_once_then_scans_qr_codes_only", async () => {
    const api = createTauriApi();
    scanner.check.mockResolvedValueOnce("prompt");
    scanner.request.mockResolvedValueOnce("granted");
    scanner.scan.mockResolvedValueOnce({ content: "tether1-link-x", format: "QR_CODE", bounds: null } as never);
    expect(await api.scanQr()).toBe("tether1-link-x");
    expect(scanner.request).toHaveBeenCalledTimes(1);
    // Windowed, so the page can show a Cancel button over the camera.
    expect(scanner.scan).toHaveBeenCalledWith({ formats: ["QR_CODE"], windowed: true });

    scanner.request.mockClear();
    scanner.check.mockResolvedValueOnce("granted");
    scanner.scan.mockResolvedValueOnce({ content: "again", format: "QR_CODE", bounds: null } as never);
    expect(await api.scanQr()).toBe("again");
    expect(scanner.request).not.toHaveBeenCalled();
    // Nothing goes through the Tether command layer.
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("scan_qr_is_null_when_the_camera_is_refused_or_the_scan_is_cancelled", async () => {
    const api = createTauriApi();
    scanner.scan.mockClear();
    scanner.check.mockResolvedValueOnce("denied");
    scanner.request.mockResolvedValueOnce("denied");
    expect(await api.scanQr()).toBeNull();
    expect(scanner.scan).not.toHaveBeenCalled();

    scanner.check.mockResolvedValueOnce("granted");
    scanner.scan.mockRejectedValueOnce(new Error("cancelled"));
    expect(await api.scanQr()).toBeNull();

    scanner.cancel.mockRejectedValueOnce(new Error("nothing to cancel"));
    await expect(api.cancelScan()).resolves.toBeUndefined();
  });

  it("clipboard_goes_through_the_native_plugin_on_ios", async () => {
    const api = createTauriApi(true);
    vi.mocked(readText).mockResolvedValueOnce("echo hi");
    expect(await api.readClipboard()).toBe("echo hi");
    // Empty, or not text (the plugin rejects): null, nothing thrown.
    vi.mocked(readText).mockResolvedValueOnce("");
    expect(await api.readClipboard()).toBeNull();
    vi.mocked(readText).mockRejectedValueOnce(new Error("not text"));
    expect(await api.readClipboard()).toBeNull();
    vi.mocked(writeText).mockResolvedValueOnce(undefined);
    expect(await api.writeClipboard("ls")).toBe(true);
    expect(writeText).toHaveBeenCalledWith("ls");
    vi.mocked(writeText).mockRejectedValueOnce(new Error("refused"));
    expect(await api.writeClipboard("ls")).toBe(false);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("clipboard_uses_the_browser_clipboard_off_ios", async () => {
    const api = createTauriApi(false);
    const clip = { readText: vi.fn().mockResolvedValue("pwd"), writeText: vi.fn().mockResolvedValue(undefined) };
    Object.defineProperty(navigator, "clipboard", { value: clip, configurable: true });
    vi.mocked(readText).mockClear();
    expect(await api.readClipboard()).toBe("pwd");
    expect(await api.writeClipboard("x")).toBe(true);
    expect(clip.writeText).toHaveBeenCalledWith("x");
    expect(readText).not.toHaveBeenCalled();
  });

  it("events_deliver_payloads", async () => {
    const api = createTauriApi();
    const seen: string[] = [];
    const off = await api.onPromptWithdrawn((e) => seen.push(e.id));
    listeners.get("prompt-withdrawn")!({ payload: { id: "p1" } });
    expect(seen).toEqual(["p1"]);
    off();
    expect(listeners.has("prompt-withdrawn")).toBe(false);
  });

  it("updates_report_download_progress_then_relaunch", async () => {
    const api = await apiOn("macos");
    expect(api.updates.supported).toBe(true);
    await expect(api.updates.install()).rejects.toThrow();
    const update = fakeUpdate();
    vi.mocked(check).mockResolvedValueOnce(update as never);
    expect(await api.updates.check()).toEqual({ version: "0.2.0", notes: "Notes", date: null });
    const seen: number[] = [];
    await api.updates.install((p) => seen.push(p));
    expect(seen).toEqual([25, 100, 100]);
    expect(update.install).toHaveBeenCalledTimes(1);
    expect(relaunch).toHaveBeenCalledTimes(1);
    // macOS: RunEvent::Exit stops sync on relaunch; nothing extra.
    expect(invokeMock).not.toHaveBeenCalled();
    vi.mocked(check).mockResolvedValueOnce(null);
    expect(await api.updates.check()).toBeNull();
    expect(update.close).toHaveBeenCalledTimes(1);
  });

  it("updates_are_supported_on_macos_and_windows_only", async () => {
    for (const [os, supported] of [
      ["macos", true],
      ["windows", true],
      ["linux", false],
      ["ios", false],
      ["android", false],
    ] as const) {
      expect((await apiOn(os)).updates.supported, os).toBe(supported);
    }
  });

  it("updates_stop_sync_before_the_windows_installer_and_resume_if_it_fails", async () => {
    const api = await apiOn("windows");
    const update = fakeUpdate();
    vi.mocked(check).mockResolvedValueOnce(update as never);
    await api.updates.check();
    update.install.mockImplementationOnce(async () => {
      expect(invokeMock.mock.calls).toEqual([["sync_pause", { paused: true }]]);
      throw new Error("installer refused");
    });
    await expect(api.updates.install()).rejects.toThrow("installer refused");
    expect(invokeMock.mock.calls).toEqual([
      ["sync_pause", { paused: true }],
      ["sync_pause", { paused: false }],
    ]);
    expect(relaunch).not.toHaveBeenCalled();
  });
});

type DownloadEv = { event: string; data?: { contentLength?: number; chunkLength?: number } };

/** An `Update` whose download reports 200 bytes in two chunks. */
function fakeUpdate() {
  return {
    version: "0.2.0",
    body: "Notes",
    date: undefined,
    close: vi.fn().mockResolvedValue(undefined),
    download: vi.fn(async (cb: (e: DownloadEv) => void) => {
      cb({ event: "Started", data: { contentLength: 200 } });
      cb({ event: "Progress", data: { chunkLength: 50 } });
      cb({ event: "Progress", data: { chunkLength: 150 } });
      cb({ event: "Finished" });
    }),
    install: vi.fn(async () => {}),
  };
}

/** `createTauriApi` from a fresh module on a backend that reports `os` (the platform is fixed at load). */
async function apiOn(os: string) {
  window.__TETHER_OS__ = os;
  vi.resetModules();
  try {
    return (await import("./index")).createTauriApi(false);
  } finally {
    delete window.__TETHER_OS__;
  }
}
