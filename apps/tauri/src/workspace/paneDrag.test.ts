import { afterEach, describe, expect, it } from "vitest";
import { paneUnder } from "./paneDrag";

afterEach(() => {
  document.body.innerHTML = "";
  // @ts-expect-error jsdom has no elementFromPoint; the tests stub it.
  delete document.elementFromPoint;
});

/** A tab panel with panes `ids`, each with a child (what a point usually lands on), and an outside pane. */
function setup(ids: string[]) {
  const panel = document.createElement("div");
  for (const id of ids) {
    const pane = document.createElement("div");
    pane.dataset.paneId = id;
    pane.innerHTML = `<span class="inner"></span>`;
    panel.appendChild(pane);
  }
  const outside = document.createElement("div");
  outside.dataset.paneId = "elsewhere";
  document.body.append(panel, outside);
  return { panel, outside, inner: (id: string) => panel.querySelector(`[data-pane-id="${id}"] .inner`) as Element };
}
const hit = (el: Element | null) => {
  document.elementFromPoint = () => el;
};

describe("paneUnder", () => {
  it("pane_under_finds_other_pane", () => {
    const { panel, inner } = setup(["a", "b"]);
    hit(inner("b"));
    expect(paneUnder(10, 10, panel, "a")).toBe("b");
  });

  it("pane_under_ignores_excluded_and_outside", () => {
    const { panel, outside, inner } = setup(["a", "b"]);
    hit(inner("a"));
    expect(paneUnder(10, 10, panel, "a")).toBeNull();
    hit(outside);
    expect(paneUnder(10, 10, panel, "a")).toBeNull();
    hit(panel);
    expect(paneUnder(10, 10, panel, "a")).toBeNull();
    hit(null);
    expect(paneUnder(10, 10, panel, "a")).toBeNull();
  });
});
