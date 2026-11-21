import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockApi, type MockTetherApi } from "../api/mock";

const backend = vi.hoisted(() => ({ mock: null as unknown as MockTetherApi }));
vi.mock("../api", () => ({
  api: new Proxy({}, { get: (_, key) => Reflect.get(backend.mock, key) }),
}));
// The platform is fixed at load in the product; the tests choose it per case.
const platform = vi.hoisted(() => ({ mobile: false, android: false }));
vi.mock("../platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../platform")>()),
  get IS_MOBILE() {
    return platform.mobile;
  },
  get IS_ANDROID() {
    return platform.android;
  },
}));

import { applyKeepAlive, followKeepAlive, KeepAliveSetting } from "./KeepAliveSetting";
import { SettingsPage } from "./SettingsPage";

const LABEL = "Keep sessions alive in the background";
const HELP =
  "Uses location in the background so iOS keeps Termlyx running while a session or port forward is open. Uses more battery. Termlyx never stores or sends your location.";
const DENIED = "Location permission was not given, so Termlyx can't stay running in the background.";

afterEach(() => {
  cleanup();
  platform.mobile = false;
  platform.android = false;
});
beforeEach(() => {
  localStorage.clear();
  backend.mock = createMockApi();
});

const toggle = () => screen.findByRole("checkbox", { name: LABEL });

describe("KeepAliveSetting", () => {
  it("is off by default and explains the battery and location use", async () => {
    render(<KeepAliveSetting />);
    const box = (await toggle()) as HTMLInputElement;
    await waitFor(() => expect(box.disabled).toBe(false));
    expect(box.checked).toBe(false);
    expect(screen.getByText(HELP)).toBeTruthy();
  });

  it("turns on after location permission is given, and is kept for the device", async () => {
    render(<KeepAliveSetting />);
    const box = (await toggle()) as HTMLInputElement;
    await waitFor(() => expect(box.disabled).toBe(false));
    fireEvent.click(box);
    await waitFor(() => expect(box.checked).toBe(true));
    expect(backend.mock.control.locationAsks()).toBe(1);
    expect((await backend.mock.keepAliveStatus()).enabled).toBe(true);
    expect(localStorage.getItem("tether.keepalive")).toBe("on");
    expect(screen.queryByText(DENIED)).toBeNull();

    fireEvent.click(box);
    await waitFor(() => expect(box.checked).toBe(false));
    expect((await backend.mock.keepAliveStatus()).enabled).toBe(false);
    expect(localStorage.getItem("tether.keepalive")).toBeNull();
  });

  it("turns back off with a note when permission is denied", async () => {
    backend.mock.control.answerLocationWith("deny");
    render(<KeepAliveSetting />);
    const box = (await toggle()) as HTMLInputElement;
    await waitFor(() => expect(box.disabled).toBe(false));
    fireEvent.click(box);
    expect(await screen.findByText(DENIED)).toBeTruthy();
    expect(box.checked).toBe(false);
    expect(localStorage.getItem("tether.keepalive")).toBeNull();
    expect((await backend.mock.keepAliveStatus()).enabled).toBe(false);
  });

  it("follows the backend: permission withdrawn or Allow Once ended unchecks it with the note", async () => {
    backend.mock.control.setLocationPermission("granted");
    render(<KeepAliveSetting />);
    const box = (await toggle()) as HTMLInputElement;
    await waitFor(() => expect(box.disabled).toBe(false));
    fireEvent.click(box);
    await waitFor(() => expect(box.checked).toBe(true));
    expect(localStorage.getItem("tether.keepalive")).toBe("on");

    // "Allow Once" ends: iOS puts the permission back to not asked.
    act(() => backend.mock.control.setLocationPermission("notDetermined"));
    await waitFor(() => expect(box.checked).toBe(false));
    expect(screen.getByText(DENIED)).toBeTruthy();
    expect(localStorage.getItem("tether.keepalive")).toBeNull();
  });

  it("shows what the backend has in force", async () => {
    await backend.mock.keepAliveSet(true);
    render(<KeepAliveSetting />);
    const box = (await toggle()) as HTMLInputElement;
    await waitFor(() => expect(box.checked).toBe(true));
  });
});

describe("applyKeepAlive (start-up)", () => {
  it("does nothing on a device that never turned it on", async () => {
    const set = vi.spyOn(backend.mock, "keepAliveSet");
    await applyKeepAlive(backend.mock);
    expect(set).not.toHaveBeenCalled();
  });

  it("turns it back on for a device that chose it, permission already given", async () => {
    localStorage.setItem("tether.keepalive", "on");
    backend.mock.control.setLocationPermission("granted");
    await applyKeepAlive(backend.mock);
    expect((await backend.mock.keepAliveStatus()).enabled).toBe(true);
    expect(localStorage.getItem("tether.keepalive")).toBe("on");
    expect(backend.mock.control.locationAsks()).toBe(0);
  });

  it("never asks for permission at start-up: not asked (Allow Once ended) forgets the choice", async () => {
    localStorage.setItem("tether.keepalive", "on");
    const set = vi.spyOn(backend.mock, "keepAliveSet");
    await applyKeepAlive(backend.mock);
    expect(set).not.toHaveBeenCalled();
    expect(backend.mock.control.locationAsks()).toBe(0);
    expect((await backend.mock.keepAliveStatus()).enabled).toBe(false);
    expect(localStorage.getItem("tether.keepalive")).toBeNull();
  });

  it("forgets the choice once permission is denied", async () => {
    localStorage.setItem("tether.keepalive", "on");
    backend.mock.control.setLocationPermission("denied");
    await applyKeepAlive(backend.mock);
    expect(backend.mock.control.locationAsks()).toBe(0);
    expect(localStorage.getItem("tether.keepalive")).toBeNull();
  });

  it("keeps the choice when the backend fails", async () => {
    localStorage.setItem("tether.keepalive", "on");
    await applyKeepAlive({
      keepAliveStatus: () => Promise.reject(new Error("not ready")),
      keepAliveSet: () => Promise.reject(new Error("not ready")),
    });
    expect(localStorage.getItem("tether.keepalive")).toBe("on");
  });
});

describe("followKeepAlive", () => {
  it("keeps the stored choice in step with the backend, Settings open or not", async () => {
    backend.mock.control.setLocationPermission("granted");
    const unlisten = await followKeepAlive(backend.mock);
    await backend.mock.keepAliveSet(true);
    expect(localStorage.getItem("tether.keepalive")).toBe("on");
    backend.mock.control.setLocationPermission("denied");
    expect(localStorage.getItem("tether.keepalive")).toBeNull();
    unlisten();
  });
});

describe("SettingsPage", () => {
  it("offers keep-alive on iOS only", async () => {
    await backend.mock.vaultCreate("correct horse battery");
    render(<SettingsPage />);
    await screen.findByRole("heading", { name: "Security" });
    expect(screen.queryByRole("checkbox", { name: LABEL })).toBeNull();
    cleanup();

    platform.mobile = true;
    render(<SettingsPage />);
    expect(await toggle()).toBeTruthy();
  });
});

describe("on Android", () => {
  const ANDROID_HELP =
    "Shows a notification while a session or port forward is open, so Android keeps Termlyx running in the background. Uses more battery.";
  beforeEach(() => {
    platform.mobile = true;
    platform.android = true;
    // Android's backend always reports the permission as given.
    backend.mock.control.setLocationPermission("granted");
  });

  it("is on by default, with no permission or location words", async () => {
    await applyKeepAlive(backend.mock);
    render(<KeepAliveSetting />);
    const box = (await toggle()) as HTMLInputElement;
    await waitFor(() => expect(box.checked).toBe(true));
    expect(screen.getByText(ANDROID_HELP)).toBeTruthy();
    expect(screen.queryByText(/location|permission/i)).toBeNull();
    expect(backend.mock.control.locationAsks()).toBe(0);
  });

  it("remembers a choice to turn it off", async () => {
    await applyKeepAlive(backend.mock);
    render(<KeepAliveSetting />);
    const box = (await toggle()) as HTMLInputElement;
    await waitFor(() => expect(box.checked).toBe(true));
    fireEvent.click(box);
    await waitFor(() => expect(box.checked).toBe(false));
    expect(localStorage.getItem("tether.keepalive")).toBe("off");

    // The next start leaves it off.
    backend.mock = createMockApi();
    backend.mock.control.setLocationPermission("granted");
    await applyKeepAlive(backend.mock);
    expect((await backend.mock.keepAliveStatus()).enabled).toBe(false);
  });

  it("is in Settings", async () => {
    await backend.mock.vaultCreate("correct horse battery");
    render(<SettingsPage />);
    expect(await toggle()).toBeTruthy();
  });
});
