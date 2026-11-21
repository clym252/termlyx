import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockApi, type MockSeed, type MockTetherApi } from "../api/mock";
import type { QuickUnlockKind } from "../api/types";

const backend = vi.hoisted(() => ({ mock: null as unknown as MockTetherApi }));
vi.mock("../api", () => ({
  api: new Proxy({}, { get: (_, key) => Reflect.get(backend.mock, key) }),
}));

import { UnlockScreen, resetAutoPromptForTests } from "./UnlockScreen";

const CHANGED = "Windows Hello changed; quick unlock was turned off";

function setup(kind: QuickUnlockKind, seed: MockSeed = {}) {
  backend.mock = createMockApi({
    quickUnlockKind: kind,
    lockedVault: { password: "correct horse battery", quickUnlock: true },
    ...seed,
  });
  const onUnlocked = vi.fn();
  render(<UnlockScreen onUnlocked={onUnlocked} quickUnlock quickUnlockKind={kind} />);
  return onUnlocked;
}

beforeEach(() => resetAutoPromptForTests());
afterEach(() => cleanup());

describe("UnlockScreen: Windows Hello", () => {
  it("labels the button and asks by itself once", async () => {
    const onUnlocked = setup("windowsHello");
    expect(screen.getByRole("button", { name: "Unlock with Windows Hello" })).toBeTruthy();
    await waitFor(() => expect(onUnlocked).toHaveBeenCalledTimes(1));
    expect(backend.mock.control.quickUnlockCalls()).toBe(1);
  });

  it("auto attempt stays quiet when Windows Hello is unavailable", async () => {
    setup("windowsHello", {
      quickUnlockFailure: {
        message: "Set up a Windows Hello PIN in Windows Settings to use quick unlock.",
        code: "biometryUnavailable",
      },
    });
    await waitFor(() => expect(backend.mock.control.quickUnlockCalls()).toBe(1));
    await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("Master password")));
    expect(screen.queryByRole("alert")).toBeNull();

    // Asking by hand says why.
    backend.mock.control.failQuickUnlock(
      "unlock",
      "Set up a Windows Hello PIN in Windows Settings to use quick unlock.",
      "biometryUnavailable",
    );
    fireEvent.click(screen.getByRole("button", { name: "Unlock with Windows Hello" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Set up a Windows Hello PIN in Windows Settings to use quick unlock.");
  });

  it("declining Windows Hello is not an error", async () => {
    setup("windowsHello", { quickUnlockFailure: { message: "Windows Hello was not confirmed", code: "presenceDenied" } });
    await waitFor(() => expect(backend.mock.control.quickUnlockCalls()).toBe(1));
    await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("Master password")));
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("says Windows Hello changed and drops the button", async () => {
    setup("windowsHello", { quickUnlockFailure: { message: CHANGED, code: "biometryChanged" } });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(
      "Windows Hello changed, so quick unlock was turned off. Unlock with your master password, then turn it on again in Settings → Security.",
    );
    expect(screen.queryByRole("button", { name: "Unlock with Windows Hello" })).toBeNull();
  });

  it("does not ask by itself again on a later unlock screen", async () => {
    setup("windowsHello", { quickUnlockFailure: { message: "Windows Hello was not confirmed", code: "presenceDenied" } });
    await waitFor(() => expect(backend.mock.control.quickUnlockCalls()).toBe(1));
    cleanup();
    render(<UnlockScreen onUnlocked={() => {}} quickUnlock quickUnlockKind="windowsHello" />);
    expect(backend.mock.control.quickUnlockCalls()).toBe(1);
  });
});

describe("UnlockScreen: Touch ID is unchanged", () => {
  it("labels the button and keeps the backend's changed message", async () => {
    setup("touchId", { quickUnlockFailure: { message: CHANGED, code: "biometryChanged" } });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(CHANGED);
    expect(screen.queryByRole("button", { name: "Unlock with Touch ID" })).toBeNull();
  });

  it("offers the Touch ID button", () => {
    backend.mock = createMockApi({ quickUnlockKind: "touchId", lockedVault: { password: "x", quickUnlock: true } });
    render(<UnlockScreen onUnlocked={() => {}} quickUnlock quickUnlockKind="touchId" />);
    expect(screen.getByRole("button", { name: "Unlock with Touch ID" })).toBeTruthy();
  });
});

describe("UnlockScreen: Face ID (iOS)", () => {
  it("labels the button and asks by itself once", async () => {
    const onUnlocked = setup("faceId");
    expect(screen.getByRole("button", { name: "Unlock with Face ID" })).toBeTruthy();
    await waitFor(() => expect(onUnlocked).toHaveBeenCalledTimes(1));
    expect(backend.mock.control.quickUnlockCalls()).toBe(1);
  });

  it("declining Face ID is not an error and keeps the button", async () => {
    setup("faceId", { quickUnlockFailure: { message: "Face ID was not confirmed", code: "presenceDenied" } });
    await waitFor(() => expect(backend.mock.control.quickUnlockCalls()).toBe(1));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("button", { name: "Unlock with Face ID" })).toBeTruthy();
  });

  it("changed faces: the backend's reason shows and the button goes", async () => {
    const changed = "Face ID changed; quick unlock was turned off";
    setup("faceId", { quickUnlockFailure: { message: changed, code: "biometryChanged" } });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(changed);
    expect(screen.queryByRole("button", { name: "Unlock with Face ID" })).toBeNull();
  });
});

describe("UnlockScreen: Android biometric unlock", () => {
  it("labels the button and asks by itself once", async () => {
    const onUnlocked = setup("androidBiometric");
    expect(screen.getByRole("button", { name: "Unlock with biometrics" })).toBeTruthy();
    await waitFor(() => expect(onUnlocked).toHaveBeenCalledTimes(1));
    expect(backend.mock.control.quickUnlockCalls()).toBe(1);
  });

  it("cancelling is not an error and keeps the button", async () => {
    setup("androidBiometric", {
      quickUnlockFailure: { message: "Biometric unlock was not confirmed", code: "presenceDenied" },
    });
    await waitFor(() => expect(backend.mock.control.quickUnlockCalls()).toBe(1));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("button", { name: "Unlock with biometrics" })).toBeTruthy();
  });

  it("changed biometrics: the backend's reason shows and the button goes", async () => {
    const changed = "Biometrics changed; quick unlock was turned off";
    setup("androidBiometric", { quickUnlockFailure: { message: changed, code: "biometryChanged" } });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(changed);
    expect(screen.queryByRole("button", { name: "Unlock with biometrics" })).toBeNull();
  });
});
