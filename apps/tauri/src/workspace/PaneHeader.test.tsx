import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PaneHeader } from "./PaneHeader";

beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

function setup(props: Partial<Parameters<typeof PaneHeader>[0]> = {}) {
  const onActivate = vi.fn();
  const run = vi.fn();
  const onSwap = vi.fn();
  const view = render(
    <PaneHeader
      paneId="p1"
      label="web"
      active={false}
      maximized={null}
      menu={[{ label: "Close pane", run }]}
      onActivate={onActivate}
      onRestore={() => {}}
      onSwap={onSwap}
      {...props}
    />,
  );
  return { onActivate, run, onSwap, header: view.container.querySelector(".pane-header") as HTMLElement };
}

/** jsdom has no PointerEvent: a plain event with the fields the header reads. */
function pointer(type: string, el: Element, init: { pointerType?: string; clientX?: number; clientY?: number }) {
  const e = new Event(type, { bubbles: true, cancelable: true });
  Object.assign(e, { pointerType: "touch", clientX: 0, clientY: 0, ...init });
  fireEvent(el, e);
}
const hold = (ms: number) => act(() => void vi.advanceTimersByTime(ms));

describe("PaneHeader menu", () => {
  it("opens from the button and activates the pane first", () => {
    const { onActivate } = setup();
    fireEvent.click(screen.getByRole("button", { name: "Pane menu for web" }));
    expect(screen.getByRole("menu", { name: "Pane menu" })).toBeTruthy();
    expect(onActivate).toHaveBeenCalledTimes(1);
  });

  it("opens on a right-click", () => {
    const { header, onActivate } = setup();
    fireEvent.contextMenu(header, { clientX: 30, clientY: 20 });
    expect(screen.getByRole("menu")).toBeTruthy();
    expect(onActivate).toHaveBeenCalledTimes(1);
  });

  it("opens when a still 500 ms touch hold lifts, and the click that ends it does nothing", () => {
    const { header } = setup({ maximized: { count: 2 }, onRestore: vi.fn() });
    pointer("pointerdown", header, { clientX: 10, clientY: 10 });
    hold(499);
    pointer("pointerup", header, {});
    expect(screen.queryByRole("menu")).toBeNull();

    pointer("pointerdown", header, { clientX: 10, clientY: 10 });
    hold(500);
    // Held, not lifted: the finger may still turn it into a drag.
    expect(screen.queryByRole("menu")).toBeNull();
    pointer("pointerup", header, {});
    expect(screen.getByRole("menu")).toBeTruthy();
    const restore = vi.fn();
    // The trailing click lands on a button of the header: swallowed.
    const button = screen.getByRole("button", { name: "Restore" });
    button.addEventListener("click", restore);
    fireEvent.click(button);
    expect(restore).not.toHaveBeenCalled();
  });

  it("does not open when the finger lifts early or drifts past 8 px", () => {
    const { header } = setup();
    pointer("pointerdown", header, { clientX: 10, clientY: 10 });
    hold(300);
    pointer("pointerup", header, {});
    hold(500);
    expect(screen.queryByRole("menu")).toBeNull();

    pointer("pointerdown", header, { clientX: 10, clientY: 10 });
    pointer("pointermove", header, { clientX: 10, clientY: 20 });
    hold(600);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("a hold then a move past 8 px drags instead of opening the menu, and swaps with the pane it lifts over", () => {
    const { header, onSwap } = setup();
    const panel = document.createElement("div");
    panel.className = "tab-panel";
    const other = document.createElement("div");
    other.dataset.paneId = "p2";
    panel.append(other);
    document.body.append(panel);
    panel.append(header.parentElement!);
    document.elementFromPoint = () => other;
    pointer("pointerdown", header, { clientX: 10, clientY: 10 });
    hold(500);
    pointer("pointermove", header, { clientX: 14, clientY: 10 });
    expect(other.hasAttribute("data-drop-target")).toBe(false);
    pointer("pointermove", header, { clientX: 40, clientY: 10 });
    expect(other.dataset.dropTarget).toBe("true");
    pointer("pointerup", header, { clientX: 40, clientY: 10 });
    expect(onSwap).toHaveBeenCalledWith("p2");
    expect(other.hasAttribute("data-drop-target")).toBe(false);
    expect(screen.queryByRole("menu")).toBeNull();
    panel.remove();
    // @ts-expect-error jsdom has no elementFromPoint.
    delete document.elementFromPoint;
  });

  it("a drag that ends over no pane, or is cancelled, swaps nothing", () => {
    const { header, onSwap } = setup();
    document.elementFromPoint = () => null;
    pointer("pointerdown", header, { clientX: 10, clientY: 10 });
    hold(500);
    pointer("pointermove", header, { clientX: 40, clientY: 10 });
    pointer("pointerup", header, { clientX: 40, clientY: 10 });
    pointer("pointerdown", header, { clientX: 10, clientY: 10 });
    hold(500);
    pointer("pointermove", header, { clientX: 40, clientY: 10 });
    pointer("pointercancel", header, {});
    expect(onSwap).not.toHaveBeenCalled();
    expect(screen.queryByRole("menu")).toBeNull();
    // @ts-expect-error jsdom has no elementFromPoint.
    delete document.elementFromPoint;
  });

  it("ignores a held mouse", () => {
    const { header } = setup();
    pointer("pointerdown", header, { pointerType: "mouse" });
    hold(600);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("says Maximized with the pane count next to a Restore button", () => {
    const onRestore = vi.fn();
    const { header } = setup({ maximized: { count: 3 }, onRestore, broadcast: { count: 2 } });
    expect(header.textContent).toContain("web · Maximized · 3 panes");
    expect(header.textContent).toContain("Typing goes to 2 panes");
    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    expect(onRestore).toHaveBeenCalledTimes(1);
  });
});
