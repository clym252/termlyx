import { cleanup, createEvent, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConnSettings, Group, Host, Tag } from "../api/types";
import { FolderCard } from "./FolderCard";
import { HostCard } from "./HostCard";

afterEach(() => cleanup());

const none: ConnSettings = {
  port: null,
  identityId: null,
  keepaliveSecs: null,
  termType: null,
  charset: null,
  colorScheme: null,
  fontFamily: null,
  fontSize: null,
  env: null,
};
const host: Host = {
  id: "h1",
  groupId: null,
  label: "nas",
  address: "10.0.0.5",
  tagIds: [],
  settings: none,
  forwardRuleIds: [],
};
const tag = (n: number): Tag => ({ id: `t${n}`, name: `tag${n}`, color: "#6aa7ff" });

function renderHost(over: { tags?: Tag[] } = {}) {
  const handlers = { onConnect: vi.fn(), onEdit: vi.fn(), onDelete: vi.fn() };
  render(<HostCard host={host} subtitle="admin@10.0.0.5" tags={over.tags ?? []} editing={false} {...handlers} />);
  return handlers;
}

describe("HostCard", () => {
  it("host_card_click_connects_once", () => {
    const { onConnect } = renderHost();
    const main = screen.getByRole("button", { name: "nas" });
    fireEvent.click(main, { detail: 1 });
    fireEvent.click(main, { detail: 2 });
    expect(onConnect).toHaveBeenCalledTimes(1);
  });

  it("connects on keyboard activation, which clicks with detail 0", () => {
    const { onConnect } = renderHost();
    fireEvent.click(screen.getByRole("button", { name: "nas" }), { detail: 0 });
    expect(onConnect).toHaveBeenCalledTimes(1);
  });

  it("host_card_edit_button", () => {
    const { onConnect, onEdit } = renderHost();
    fireEvent.click(screen.getByRole("button", { name: "Edit host nas" }));
    expect(onEdit).toHaveBeenCalledTimes(1);
    expect(onConnect).not.toHaveBeenCalled();
  });

  it("host_card_context_menu", () => {
    const { onDelete } = renderHost();
    fireEvent.contextMenu(screen.getByRole("button", { name: "nas" }));
    const menu = screen.getByRole("menu");
    expect(within(menu).getAllByRole("menuitem").map((i) => i.textContent)).toEqual([
      "Connect",
      "Edit host",
      "Delete host",
    ]);
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Delete host" }));
    expect(onDelete).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("opens the menu from the keyboard", () => {
    renderHost();
    fireEvent.keyDown(screen.getByRole("button", { name: "nas" }), { key: "F10", shiftKey: true });
    expect(screen.getByRole("menu")).toBeTruthy();
  });

  it("ignores a right-click or the menu key inside its open menu", () => {
    renderHost();
    fireEvent.contextMenu(screen.getByRole("button", { name: "nas" }), { clientX: 10, clientY: 20 });
    const menu = screen.getByRole("menu");
    expect(menu.style.left).toBe("10px");
    const item = within(menu).getByRole("menuitem", { name: "Edit host" });
    const rightClick = fireEvent.contextMenu(item, { clientX: 300, clientY: 400 });
    fireEvent.keyDown(item, { key: "ContextMenu" });
    // Not moved, and the browser's own menu still suppressed.
    expect(screen.getByRole("menu").style.left).toBe("10px");
    expect(rightClick).toBe(false);
  });

  it("host_card_tags_overflow", () => {
    renderHost({ tags: [1, 2, 3, 4, 5].map(tag) });
    expect(document.querySelectorAll(".tag-pill")).toHaveLength(3);
    expect(screen.getByText("+2")).toBeTruthy();
  });

  it("shows the subtitle and the badge initials, and the path only when given", () => {
    const { rerender } = render(
      <HostCard host={host} subtitle="admin@10.0.0.5" tags={[]} editing onConnect={vi.fn()} onEdit={vi.fn()} onDelete={vi.fn()} />,
    );
    expect(screen.getByText("admin@10.0.0.5")).toBeTruthy();
    expect(screen.getByText("NA")).toBeTruthy();
    expect(document.querySelector(".host-card")?.getAttribute("data-editing")).toBe("true");
    expect(document.querySelector(".host-card-path")).toBeNull();
    rerender(
      <HostCard host={host} subtitle="x" tags={[]} path="Lab / Rack" editing={false} onConnect={vi.fn()} onEdit={vi.fn()} onDelete={vi.fn()} />,
    );
    expect(screen.getByText("Lab / Rack")).toBeTruthy();
  });
});

const group: Group = { id: "g1", parentId: null, name: "Lab", settings: none };

function renderFolder(count: number) {
  const handlers = {
    onOpen: vi.fn(),
    onNewSubgroup: vi.fn(),
    onEdit: vi.fn(),
    onDelete: vi.fn(),
    onDropHost: vi.fn(),
  };
  render(<FolderCard group={group} count={count} {...handlers} />);
  return handlers;
}

describe("FolderCard", () => {
  it("folder_card_menu_and_count", () => {
    renderFolder(1);
    expect(screen.getByText("1 host")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Actions for Lab" }));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((i) => i.textContent)).toEqual([
      "Open",
      "New subgroup",
      "Edit group",
      "Delete group",
    ]);
  });

  it("pluralises the count", () => {
    renderFolder(3);
    expect(screen.getByText("3 hosts")).toBeTruthy();
  });

  it("opens on click and from the right-click menu", () => {
    const { onOpen, onEdit } = renderFolder(0);
    fireEvent.click(screen.getByRole("button", { name: "Lab" }));
    expect(onOpen).toHaveBeenCalledTimes(1);
    fireEvent.contextMenu(screen.getByRole("button", { name: "Lab" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Edit group" }));
    expect(onEdit).toHaveBeenCalledTimes(1);
  });

  it("accepts a dropped host", () => {
    const { onDropHost } = renderFolder(0);
    const card = document.querySelector(".folder-card") as HTMLElement;
    const data = new Map([["application/x-tether-host", "h9"]]);
    const dataTransfer = { types: [...data.keys()], getData: (t: string) => data.get(t) ?? "", dropEffect: "" };
    fireEvent.dragOver(card, { dataTransfer });
    expect(card.getAttribute("data-drop")).toBe("true");
    fireEvent.drop(card, { dataTransfer });
    expect(onDropHost).toHaveBeenCalledWith("h9");
    expect(card.getAttribute("data-drop")).toBeNull();
  });

  it("keeps the drop highlight while the drag moves over its own children", () => {
    renderFolder(0);
    const card = document.querySelector(".folder-card") as HTMLElement;
    const dataTransfer = { types: ["application/x-tether-host"], getData: () => "", dropEffect: "" };
    // jsdom has no DragEvent, so `relatedTarget` is set on the event by hand.
    const leaveTo = (target: Element) => {
      const event = createEvent.dragLeave(card, { dataTransfer });
      Object.defineProperty(event, "relatedTarget", { value: target });
      fireEvent(card, event);
    };
    fireEvent.dragOver(card, { dataTransfer });
    leaveTo(screen.getByText("Lab"));
    expect(card.getAttribute("data-drop")).toBe("true");
    leaveTo(document.body);
    expect(card.getAttribute("data-drop")).toBeNull();
  });
});
