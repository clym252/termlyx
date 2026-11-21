import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockApi, type MockTetherApi } from "../api/mock";

const backend = vi.hoisted(() => ({ mock: null as unknown as MockTetherApi }));
vi.mock("../api", () => ({
  api: new Proxy({}, { get: (_, key) => Reflect.get(backend.mock, key) }),
}));

import { getTerminalSettings, updateTerminalSettings } from "../settings/store";
import { ThemesTab } from "./ThemesTab";

beforeEach(async () => {
  backend.mock = createMockApi();
  await backend.mock.vaultCreate("pw");
  updateTerminalSettings({ themeId: "tether-dark", highlightIps: true });
});
afterEach(cleanup);

describe("ThemesTab", () => {
  it("lists dark and light themes with the current one checked", () => {
    render(<ThemesTab pane={null} />);
    const dark = screen.getByRole("radiogroup", { name: "Dark" });
    const light = screen.getByRole("radiogroup", { name: "Light" });
    expect(within(dark).getAllByRole("radio")).toHaveLength(20);
    expect(within(light).getAllByRole("radio")).toHaveLength(10);
    expect(screen.getByRole("radio", { name: "Termlyx Dark" }).getAttribute("aria-checked")).toBe("true");
  });

  it("applies a theme on click", () => {
    render(<ThemesTab pane={null} />);
    fireEvent.click(screen.getByRole("radio", { name: "Gruvbox Light" }));
    expect(getTerminalSettings().themeId).toBe("gruvbox-light");
    expect(screen.getByRole("radio", { name: "Gruvbox Light" }).getAttribute("aria-checked")).toBe("true");
  });

  it("draws each swatch in its own theme's colours", () => {
    render(<ThemesTab pane={null} />);
    const swatch = screen.getByRole("radio", { name: "Gruvbox Dark" }).querySelector<HTMLElement>(".theme-swatch")!;
    expect(swatch.style.background).toBe("rgb(40, 40, 40)");
  });

  it("says when the terminal's host sets its own theme", async () => {
    const host = await backend.mock.upsertEntity("host", {
      label: "web-1",
      address: "web1.example",
      settings: { colorScheme: "nord" },
    });
    render(<ThemesTab pane={{ id: "p1", title: "web-1", ordinal: 1, target: { kind: "host", hostId: host.id } }} />);
    await waitFor(() => expect(screen.getByText("This terminal uses Nord from its host settings.")).toBeTruthy());
  });

  it("toggles IP highlighting", () => {
    render(<ThemesTab pane={null} />);
    fireEvent.click(screen.getByRole("checkbox", { name: "Highlight IP addresses" }));
    expect(getTerminalSettings().highlightIps).toBe(false);
  });
});
