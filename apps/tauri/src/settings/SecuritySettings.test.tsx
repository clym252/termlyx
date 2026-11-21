import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { QuickUnlockKind } from "../api/types";
import { createMockApi, type MockTetherApi } from "../api/mock";

const backend = vi.hoisted(() => ({ mock: null as unknown as MockTetherApi }));
vi.mock("../api", () => ({
  api: new Proxy({}, { get: (_, key) => Reflect.get(backend.mock, key) }),
}));
// The platform is fixed at load in the product; the tests choose it per case.
const platform = vi.hoisted(() => ({
  ios: false,
  formFactor: "desktop" as "desktop" | "phone" | "tablet",
  deviceNoun: "computer",
}));
vi.mock("../platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../platform")>()),
  get IS_IOS() {
    return platform.ios;
  },
  get FORM_FACTOR() {
    return platform.formFactor;
  },
  get DEVICE_NOUN() {
    return platform.deviceNoun;
  },
}));

import { applyAutoLock, getAutoLockError, noteAutoLockApplied, setAutoLockPref } from "./autoLockPref";
import { SecuritySettings } from "./SecuritySettings";

afterEach(() => {
  cleanup();
  platform.ios = false;
  platform.formFactor = "desktop";
  platform.deviceNoun = "computer";
  // The choice is cached per module: back to the default for the next test.
  setAutoLockPref(null);
  noteAutoLockApplied(null);
});
beforeEach(() => localStorage.clear());

async function open(kind: QuickUnlockKind) {
  backend.mock = createMockApi({ quickUnlockKind: kind });
  await backend.mock.vaultCreate("correct horse battery");
  render(<SecuritySettings />);
}

describe("SecuritySettings: quick unlock wording", () => {
  it("names Windows Hello and explains what it protects", async () => {
    await open("windowsHello");
    const toggle = await screen.findByRole("checkbox", { name: "Unlock with Windows Hello" });
    expect(toggle).toBeTruthy();
    expect(
      screen.getByText(
        "Keeps a key on this PC that only Windows Hello (your PIN, fingerprint or face) can open. Resetting Windows Hello turns it off. The master password still works.",
      ),
    ).toBeTruthy();
  });

  it("keeps the Touch ID wording", async () => {
    await open("touchId");
    await screen.findByRole("checkbox", { name: "Unlock with Touch ID" });
    expect(screen.getByText(/Keeps a key in this Mac's keychain that Termlyx only uses after Touch ID/)).toBeTruthy();
  });

  it("names Face ID on an iPhone", async () => {
    platform.ios = true;
    platform.formFactor = "phone";
    await open("faceId");
    await screen.findByRole("checkbox", { name: "Unlock with Face ID" });
    expect(
      screen.getByText(
        "Keeps a key on this iPhone that Termlyx only uses after Face ID. Changing enrolled faces turns it off. The master password still works.",
      ),
    ).toBeTruthy();
  });

  it("names Touch ID on an iPad", async () => {
    platform.ios = true;
    platform.formFactor = "tablet";
    await open("touchId");
    await screen.findByRole("checkbox", { name: "Unlock with Touch ID" });
    expect(
      screen.getByText(
        "Keeps a key on this iPad that Termlyx only uses after Touch ID. Changing enrolled fingers turns it off. The master password still works.",
      ),
    ).toBeTruthy();
  });

  it("never says keychain on iOS, even without a named sensor", async () => {
    platform.ios = true;
    platform.formFactor = "phone";
    await open("none");
    await screen.findByRole("checkbox", { name: "Unlock with Face ID or Touch ID" });
    expect(
      screen.getByText(
        "Keeps a key on this iPhone that Termlyx only uses after Face ID or Touch ID. The master password still works.",
      ),
    ).toBeTruthy();
    expect(screen.queryByText(/keychain/)).toBeNull();
  });
});

describe("SecuritySettings: Android biometric unlock", () => {
  it("names biometric unlock on a phone", async () => {
    platform.deviceNoun = "phone";
    await open("androidBiometric");
    await screen.findByRole("checkbox", { name: "Biometric unlock" });
    expect(
      screen.getByText(
        "Unlock with a fingerprint or face this phone rates as strong. Changing enrolled fingerprints or faces turns it off.",
      ),
    ).toBeTruthy();
    expect(screen.queryByText(/keychain/)).toBeNull();
  });

  it("says tablet on a tablet", async () => {
    platform.deviceNoun = "tablet";
    await open("androidBiometric");
    await screen.findByRole("checkbox", { name: "Biometric unlock" });
    expect(screen.getByText(/this tablet rates as strong/)).toBeTruthy();
  });
});

describe("SecuritySettings: auto-lock", () => {
  it("offers five choices and starts at Never", async () => {
    await open("touchId");
    const select = (await screen.findByRole("combobox", { name: "Auto-lock" })) as HTMLSelectElement;
    expect([...select.options].map((o) => o.textContent)).toEqual([
      "Never",
      "After 5 minutes",
      "After 15 minutes",
      "After 30 minutes",
      "After 1 hour",
    ]);
    expect(select.selectedOptions[0].textContent).toBe("Never");
  });

  it("saves the choice on this device and tells the backend", async () => {
    await open("touchId");
    const setAutoLock = vi.spyOn(backend.mock, "setAutoLock");
    const select = (await screen.findByRole("combobox", { name: "Auto-lock" })) as HTMLSelectElement;
    fireEvent.change(select, { target: { value: "30" } });
    await waitFor(() => expect(setAutoLock).toHaveBeenCalledWith(30));
    await waitFor(() => expect(localStorage.getItem("tether.auto-lock")).toBe("30"));
    expect(select.selectedOptions[0].textContent).toBe("After 30 minutes");

    fireEvent.change(select, { target: { value: "never" } });
    await waitFor(() => expect(setAutoLock).toHaveBeenLastCalledWith(null));
    await waitFor(() => expect(localStorage.getItem("tether.auto-lock")).toBe("never"));
  });

  it("keeps the old choice when the backend refuses it", async () => {
    await open("touchId");
    vi.spyOn(backend.mock, "setAutoLock").mockRejectedValue({ code: "internal", message: "nope", refs: [] });
    const select = (await screen.findByRole("combobox", { name: "Auto-lock" })) as HTMLSelectElement;
    fireEvent.change(select, { target: { value: "5" } });
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(select.selectedOptions[0].textContent).toBe("Never");
    expect(localStorage.getItem("tether.auto-lock")).toBeNull();
  });

  it("says so when the choice could not be applied at start-up, and tries again", async () => {
    await open("touchId");
    setAutoLockPref(15);
    // Start-up (Gate) failed twice: the backend still runs with Never.
    await applyAutoLock(() => Promise.reject({ code: "internal", message: "backend busy", refs: [] }));
    expect(getAutoLockError()).toBe("backend busy");
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Auto-lock is not on");
    expect(alert.textContent).toContain("backend busy");

    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(backend.mock.control.autoLockMinutes()).toBe(15);
  });
});
