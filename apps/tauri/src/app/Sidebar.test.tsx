import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../sync/SyncStatusDot", () => ({
  SyncStatusDot: ({ compact }: { compact?: boolean }) => <span data-testid="sync-stub" data-compact={String(!!compact)} />,
}));

import { Sidebar } from "./Sidebar";
import type { RouteDef } from "./routes";
import type { SidebarMode } from "./sidebarPref";

const ROUTES: RouteDef[] = [
  { id: "hosts", label: "Hosts", render: () => null },
  { id: "keychain", label: "Keychain", render: () => null },
  { id: "forwarding", label: "Port Forwarding", render: () => null },
  { id: "knownHosts", label: "Known Hosts", render: () => null },
  { id: "sftp", label: "SFTP", render: () => null },
  { id: "settings", label: "Settings", render: () => null },
  { id: "conflicts", label: "Conflicts", hidden: true, render: () => null },
];
const LABELS = ["Hosts", "Keychain", "Port Forwarding", "Known Hosts", "SFTP", "Settings"];

function renderSidebar(
  mode: Exclude<SidebarMode, "hidden">,
  extra: { count?: number; conflicts?: number; onToggleMode?: () => void; inDrawer?: boolean } = {},
) {
  const onToggleMode = extra.inDrawer ? undefined : (extra.onToggleMode ?? vi.fn());
  const onNavigate = vi.fn();
  render(
    <Sidebar
      routes={ROUTES}
      current="hosts"
      terminals={{ count: extra.count ?? 0, shown: false, onShow: vi.fn() }}
      conflictCount={extra.conflicts ?? 0}
      mode={mode}
      onToggleMode={onToggleMode}
      onNavigate={onNavigate}
      onSearch={vi.fn()}
      onLock={vi.fn()}
    />,
  );
  return { onToggleMode, onNavigate };
}

afterEach(() => cleanup());

describe("Sidebar in full mode", () => {
  it("shows the word labels", () => {
    renderSidebar("full");
    const nav = screen.getByRole("navigation", { name: "Main" });
    for (const label of LABELS) expect(within(nav).getByRole("button", { name: label }).textContent).toBe(label);
    expect(screen.getByRole("button", { name: "Lock vault" }).textContent).toBe("Lock vault");
    expect(screen.getByRole("button", { name: "Collapse sidebar" })).toBeTruthy();
  });
});

describe("Sidebar in the phone layout's drawer", () => {
  it("lists every page with no collapse control and no shortcut hint", () => {
    renderSidebar("full", { inDrawer: true, count: 2 });
    const nav = screen.getByRole("navigation", { name: "Main" });
    for (const label of LABELS) expect(within(nav).getByRole("button", { name: label }).textContent).toBe(label);
    expect(within(nav).getByRole("button", { name: /^Terminals ?\(2\)$/ })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Collapse sidebar" })).toBeNull();
    expect(screen.getByRole("button", { name: "Search" }).querySelector("kbd")).toBeNull();
  });
});

describe("Sidebar in rail mode", () => {
  it("keeps every nav button's accessible name and tooltip, with no visible word", () => {
    renderSidebar("rail");
    const nav = screen.getByRole("navigation", { name: "Main" });
    for (const label of LABELS) {
      const b = within(nav).getByRole("button", { name: label });
      expect(b.getAttribute("title")).toBe(label);
      expect(b.textContent).toBe("");
      expect(b.querySelector("svg")).not.toBeNull();
    }
    const lock = screen.getByRole("button", { name: "Lock vault" });
    expect(lock.textContent).toBe("");
    expect(lock.getAttribute("title")).toBe("Lock vault");
    const search = screen.getByRole("button", { name: "Search" });
    expect(search.textContent).toBe("");
  });

  it("marks the current page", () => {
    renderSidebar("rail");
    expect(screen.getByRole("button", { name: "Hosts" }).getAttribute("aria-current")).toBe("page");
    expect(screen.getByRole("button", { name: "Keychain" }).getAttribute("aria-current")).toBeNull();
  });

  it("shows the terminal count as a badge, the name still carrying it", () => {
    renderSidebar("rail", { count: 3 });
    const b = screen.getByRole("button", { name: "Terminals (3)" });
    expect(b.textContent).toBe("3");
  });

  it("disables Terminals with no tabs, and shows no badge", () => {
    renderSidebar("rail");
    const b = screen.getByRole("button", { name: "Terminals" }) as HTMLButtonElement;
    expect(b.disabled).toBe(true);
    expect(b.textContent).toBe("");
  });

  it("keeps the conflict badge as a count", () => {
    renderSidebar("rail", { conflicts: 2 });
    const badge = screen.getByTestId("conflict-badge");
    expect(badge.getAttribute("aria-label")).toBe("2 conflicts");
    expect(badge.textContent).toBe("2");
  });

  it("makes the sync indicator compact", () => {
    renderSidebar("rail");
    expect(screen.getByTestId("sync-stub").getAttribute("data-compact")).toBe("true");
  });

  it("navigates from an icon", () => {
    const { onNavigate } = renderSidebar("rail");
    fireEvent.click(screen.getByRole("button", { name: "Keychain" }));
    expect(onNavigate).toHaveBeenCalledWith("keychain");
  });
});

describe("Sidebar toggle", () => {
  it("is named for what it does in each mode and calls onToggleMode", () => {
    const full = renderSidebar("full");
    fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
    expect(full.onToggleMode).toHaveBeenCalledTimes(1);
    cleanup();
    const rail = renderSidebar("rail");
    fireEvent.click(screen.getByRole("button", { name: "Expand sidebar" }));
    expect(rail.onToggleMode).toHaveBeenCalledTimes(1);
  });

  it("puts the shortcut in its tooltip", () => {
    renderSidebar("full");
    expect(screen.getByRole("button", { name: "Collapse sidebar" }).getAttribute("title")).toMatch(/^Collapse sidebar \(.+\)$/);
  });

  it("marks the sidebar's mode for styling", () => {
    renderSidebar("rail");
    expect(document.querySelector("aside.sidebar")?.getAttribute("data-mode")).toBe("rail");
  });
});
