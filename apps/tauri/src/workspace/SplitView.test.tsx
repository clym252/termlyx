import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import type { LayoutNode, PaneSpec } from "./model";
import { SplitView } from "./SplitView";

beforeAll(() => {
  // react-resizable-panels measures its group with a ResizeObserver.
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
});
afterEach(cleanup);

const spec = (id: string): PaneSpec => ({ id, target: { kind: "local", shellId: null }, title: id, ordinal: 1 });
const leaf = (id: string): LayoutNode => ({ kind: "pane", pane: spec(id) });
const TREE: LayoutNode = { kind: "split", id: "s1", orientation: "horizontal", children: [leaf("p1"), leaf("p2"), leaf("p3")] };

function homes() {
  const els = new Map<string, HTMLElement>();
  const paneElement = (id: string) => {
    let el = els.get(id);
    if (!el) {
      el = document.createElement("div");
      el.className = "pane-home";
      el.dataset.home = id;
      els.set(id, el);
    }
    return el;
  };
  return { els, paneElement };
}

describe("SplitView", () => {
  it("split_view_zoom_stash_keeps_elements", () => {
    const { els, paneElement } = homes();
    const { container, rerender } = render(<SplitView node={TREE} paneElement={paneElement} zoomed={null} />);
    for (const el of els.values()) expect(el.isConnected).toBe(true);

    // Zooming moves the elements; none is re-created or dropped.
    const before = new Map(els);
    rerender(<SplitView node={TREE} paneElement={paneElement} zoomed="p2" />);
    const visible = [...container.querySelectorAll<HTMLElement>(".pane-slot")].filter((s) => !s.closest(".zoom-stash"));
    expect(visible).toHaveLength(1);
    expect(visible[0].contains(els.get("p2")!)).toBe(true);
    for (const id of ["p1", "p3"]) {
      const el = els.get(id)!;
      expect(el.isConnected).toBe(true);
      expect(el.closest(".zoom-stash")).not.toBeNull();
    }
    for (const [id, el] of before) expect(els.get(id)).toBe(el);

    // And back: every element is in a slot of the layout again.
    rerender(<SplitView node={TREE} paneElement={paneElement} zoomed={null} />);
    expect(container.querySelector(".zoom-stash")).toBeNull();
    for (const el of els.values()) expect(el.isConnected).toBe(true);
  });

  it("split_view_ignores_zoom_on_a_pane_not_in_the_tree", () => {
    const { els, paneElement } = homes();
    const { container } = render(<SplitView node={TREE} paneElement={paneElement} zoomed="gone" />);
    expect(container.querySelector(".zoom-stash")).toBeNull();
    for (const el of els.values()) expect(el.isConnected).toBe(true);
  });
});
