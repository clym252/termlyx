import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RowMenu, type MenuEntry } from "./RowMenu";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

/** Gives every element the size of the menu (jsdom does no layout). */
function menuSize(width: number, height: number) {
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
    DOMRect.fromRect({ x: 0, y: 0, width, height }),
  );
}

const items: [string, () => void][] = [["Open", () => {}]];

describe("RowMenu", () => {
  it("sits at its anchor when it fits", () => {
    menuSize(150, 100);
    render(<RowMenu owner="h1" label="nas" items={items} onClose={() => {}} anchor={{ x: 40, y: 60 }} />);
    const menu = screen.getByRole("menu");
    expect([menu.style.left, menu.style.top]).toEqual(["40px", "60px"]);
  });

  it("stays inside the window near its right and bottom edges", () => {
    menuSize(150, 100);
    const { innerWidth: w, innerHeight: h } = window;
    render(<RowMenu owner="h1" label="nas" items={items} onClose={() => {}} anchor={{ x: w - 20, y: h - 10 }} />);
    const menu = screen.getByRole("menu");
    // Shifted left to fit, flipped above the anchor.
    expect(menu.style.left).toBe(`${w - 150 - 4}px`);
    expect(menu.style.top).toBe(`${h - 10 - 100}px`);
  });
});

describe("RowMenu focus", () => {
  const two: [string, () => void][] = [
    ["Open", () => {}],
    ["Edit", () => {}],
  ];

  it("keeps the focused item when the parent re-renders with a new onClose", () => {
    const { rerender } = render(<RowMenu owner="h1" label="nas" items={two} onClose={() => {}} />);
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Open" }));
    const edit = screen.getByRole("menuitem", { name: "Edit" });
    edit.focus();
    rerender(<RowMenu owner="h1" label="nas" items={two} onClose={() => {}} />);
    expect(document.activeElement).toBe(edit);
  });

  it("closes through the latest onClose", () => {
    const first = vi.fn();
    const latest = vi.fn();
    const { rerender } = render(<RowMenu owner="h1" label="nas" items={two} onClose={first} />);
    rerender(<RowMenu owner="h1" label="nas" items={two} onClose={latest} />);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(first).not.toHaveBeenCalled();
    expect(latest).toHaveBeenCalledTimes(1);
  });
});

describe("RowMenu entries", () => {
  it("marks a disabled entry, skips it with the arrow keys and ignores its click", () => {
    const run = vi.fn();
    const onClose = vi.fn();
    const items: MenuEntry[] = [
      { label: "Off", run, disabled: true },
      ["First", () => {}],
      "separator",
      { label: "Second", run: () => {} },
    ];
    render(<RowMenu owner="h1" label="nas" items={items} onClose={onClose} />);
    const off = screen.getByRole("menuitem", { name: "Off" });
    expect(off.getAttribute("aria-disabled")).toBe("true");
    // Focus starts on the first entry that can be chosen.
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "First" }));
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Second" }));
    // Wraps past the disabled entry.
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "First" }));
    fireEvent.click(off);
    expect(run).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("separator")).toBeTruthy();
  });

  it("shows a check and a shortcut hint, and runs then closes on a choice", () => {
    const run = vi.fn();
    const onClose = vi.fn();
    render(
      <RowMenu
        owner="h1"
        label="nas"
        items={[{ label: "Broadcast input", run, checked: true, hint: "⌥⌘B" }]}
        onClose={onClose}
      />,
    );
    const item = screen.getByRole("menuitemcheckbox", { name: /Broadcast input/ });
    expect(item.getAttribute("aria-checked")).toBe("true");
    expect(item.querySelector("kbd")?.textContent).toBe("⌥⌘B");
    fireEvent.click(item);
    expect(run).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("RowMenu keys and focus", () => {
  const three: MenuEntry[] = [
    ["A", () => {}],
    { label: "B", run: () => {}, disabled: true },
    ["C", () => {}],
    ["D", () => {}],
  ];

  it("lands on the last entry with ArrowUp and the first with ArrowDown when focus is off the entries", () => {
    render(<RowMenu owner="h1" label="nas" items={three} onClose={() => {}} />);
    const menu = screen.getByRole("menu");
    (document.activeElement as HTMLElement).blur();
    fireEvent.keyDown(menu, { key: "ArrowUp" });
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "D" }));
    (document.activeElement as HTMLElement).blur();
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "A" }));
  });

  it("gives focus back to its trigger on Escape", () => {
    render(
      <>
        <button type="button" data-menu-trigger="h1">
          more
        </button>
        <RowMenu owner="h1" label="nas" items={three} onClose={() => {}} />
      </>,
    );
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "A" }));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "more" }));
  });
});
