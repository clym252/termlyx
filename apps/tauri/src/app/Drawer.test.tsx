import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Drawer } from "./Drawer";

function renderDrawer(open: boolean) {
  const onOpen = vi.fn();
  const onClose = vi.fn();
  const view = render(
    <Drawer open={open} onOpen={onOpen} onClose={onClose}>
      <button type="button">Hosts</button>
    </Drawer>,
  );
  return { onOpen, onClose, view };
}

/** A one-finger touch at (x, y) on `target`. */
const touch = (x: number, y = 200) => ({ touches: [{ clientX: x, clientY: y }], changedTouches: [{ clientX: x, clientY: y }] });

afterEach(() => cleanup());

describe("Drawer", () => {
  it("shows its content only while open", () => {
    const { view } = renderDrawer(false);
    expect(screen.queryByRole("dialog", { name: "Menu" })).toBeNull();
    view.rerender(
      <Drawer open onOpen={vi.fn()} onClose={vi.fn()}>
        <button type="button">Hosts</button>
      </Drawer>,
    );
    expect(screen.getByRole("dialog", { name: "Menu" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Hosts" })).toBeTruthy();
  });

  it("closes on a tap outside the panel", () => {
    const { onClose } = renderDrawer(true);
    fireEvent.click(screen.getByTestId("drawer-backdrop"));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("does not close on a tap inside the panel", () => {
    const { onClose } = renderDrawer(true);
    fireEvent.click(screen.getByRole("button", { name: "Hosts" }));
    expect(onClose).not.toHaveBeenCalled();
  });

  it("closes on Escape", () => {
    const { onClose } = renderDrawer(true);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("ignores Escape while closed", () => {
    const { onClose } = renderDrawer(false);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
  });

  it("opens on a swipe in from the left edge", () => {
    const { onOpen } = renderDrawer(false);
    fireEvent.touchStart(window, touch(10));
    fireEvent.touchMove(window, touch(50));
    expect(onOpen).not.toHaveBeenCalled();
    fireEvent.touchMove(window, touch(90));
    expect(onOpen).toHaveBeenCalledTimes(1);
    // One swipe opens once.
    fireEvent.touchMove(window, touch(140));
    fireEvent.touchEnd(window, touch(140));
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("does not open on a swipe that starts away from the edge", () => {
    const { onOpen } = renderDrawer(false);
    fireEvent.touchStart(window, touch(40));
    fireEvent.touchMove(window, touch(200));
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("does not open on a mostly vertical drag from the edge", () => {
    const { onOpen } = renderDrawer(false);
    fireEvent.touchStart(window, touch(10, 100));
    fireEvent.touchMove(window, touch(80, 400));
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("closes on a swipe back to the left", () => {
    const { onClose } = renderDrawer(true);
    const panel = screen.getByRole("dialog", { name: "Menu" });
    fireEvent.touchStart(panel, touch(200));
    fireEvent.touchMove(panel, touch(130));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
