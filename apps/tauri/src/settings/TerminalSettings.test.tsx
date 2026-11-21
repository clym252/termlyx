import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { LocalShell } from "../api/types";
import { createMockApi, type MockTetherApi } from "../api/mock";

const backend = vi.hoisted(() => ({ mock: null as unknown as MockTetherApi }));
vi.mock("../api", () => ({
  api: new Proxy({}, { get: (_, key) => Reflect.get(backend.mock, key) }),
}));

// The platform is fixed at load in the product; the tests choose it per case.
const platform = vi.hoisted(() => ({ mobile: false }));
vi.mock("../platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../platform")>()),
  get IS_MOBILE() {
    return platform.mobile;
  },
}));

import { TerminalSettings } from "./TerminalSettings";
import { getTerminalSettings, updateTerminalSettings } from "./store";

const THREE: LocalShell[] = [
  { id: "powershell", label: "Windows PowerShell" },
  { id: "cmd", label: "Command Prompt" },
  { id: "wsl:Ubuntu", label: "WSL: Ubuntu" },
];

beforeEach(() => {
  localStorage.clear();
  updateTerminalSettings({ localShellId: null });
});
afterEach(() => {
  cleanup();
  platform.mobile = false;
});

describe("TerminalSettings: local shell", () => {
  it("is_hidden_on_ios_and_the_shells_are_not_even_asked_for", async () => {
    platform.mobile = true;
    backend.mock = createMockApi({ localShells: THREE });
    const asked = vi.spyOn(backend.mock, "localShells");
    render(<TerminalSettings />);
    await screen.findByLabelText("Font size");
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByLabelText("Local shell")).toBeNull();
    expect(asked).not.toHaveBeenCalled();
  });

  it("is hidden when there is only one shell", async () => {
    backend.mock = createMockApi();
    render(<TerminalSettings />);
    await screen.findByLabelText("Font size");
    // Let the shell list arrive before concluding nothing shows.
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByLabelText("Local shell")).toBeNull();
  });

  it("lists every shell, the first as the default", async () => {
    backend.mock = createMockApi({ localShells: THREE });
    render(<TerminalSettings />);
    const select = (await screen.findByLabelText("Local shell")) as HTMLSelectElement;
    expect([...select.options].map((o) => o.textContent)).toEqual([
      "Windows PowerShell (default)",
      "Command Prompt",
      "WSL: Ubuntu",
    ]);
    expect(select.value).toBe("powershell");
  });

  it("saves the chosen shell, and picking the default clears it", async () => {
    backend.mock = createMockApi({ localShells: THREE });
    render(<TerminalSettings />);
    const select = (await screen.findByLabelText("Local shell")) as HTMLSelectElement;
    fireEvent.change(select, { target: { value: "wsl:Ubuntu" } });
    expect(getTerminalSettings().localShellId).toBe("wsl:Ubuntu");
    fireEvent.change(select, { target: { value: "powershell" } });
    expect(getTerminalSettings().localShellId).toBeNull();
  });

  it("shows a saved shell that has since gone as unavailable, and keeps it until changed", async () => {
    updateTerminalSettings({ localShellId: "pwsh" });
    backend.mock = createMockApi({ localShells: THREE });
    render(<TerminalSettings />);
    const select = (await screen.findByLabelText("Local shell")) as HTMLSelectElement;
    expect(select.value).toBe("pwsh");
    expect(screen.getByRole("option", { name: "pwsh (not available)" })).toBeTruthy();
  });
});

describe("TerminalSettings: IP highlighting", () => {
  it("toggles the highlight setting", async () => {
    backend.mock = createMockApi();
    updateTerminalSettings({ highlightIps: true });
    render(<TerminalSettings />);
    const box = screen.getByRole("checkbox", { name: "Highlight IP addresses" });
    expect((box as HTMLInputElement).checked).toBe(true);
    fireEvent.click(box);
    expect(getTerminalSettings().highlightIps).toBe(false);
  });
});
