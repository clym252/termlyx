import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockApi, type MockTetherApi } from "../api/mock";

const backend = vi.hoisted(() => ({ mock: null as unknown as MockTetherApi }));
vi.mock("../api", () => ({
  api: new Proxy({}, { get: (_, key) => Reflect.get(backend.mock, key) }),
}));

import { SnippetsPage } from "./SnippetsPage";

beforeEach(async () => {
  // jsdom has no <dialog> showModal
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
  backend.mock = createMockApi();
  await backend.mock.vaultCreate("pw");
  await backend.mock.upsertEntity("snippet", { label: "Upgrade", text: "sudo apt update && sudo apt upgrade -y" });
  await backend.mock.upsertEntity("snippet", { label: "Disk usage", text: "df -h", run: true });
  await backend.mock.upsertEntity("snippet", { label: "Two lines", text: "echo one\necho two" });
});
afterEach(cleanup);

const list = () => screen.getByRole("list", { name: "Snippets" });
const rows = () => within(list()).getAllByRole("listitem");

describe("SnippetsPage", () => {
  it("lists snippets with the first line of each command", async () => {
    render(<SnippetsPage />);
    await waitFor(() => expect(rows()).toHaveLength(3));
    const two = rows().find((r) => r.textContent?.includes("Two lines"))!;
    const cmd = two.querySelector(".snippet-command")!;
    expect(cmd.textContent).toBe("echo one");
  });

  it("filters by search", async () => {
    render(<SnippetsPage />);
    await waitFor(() => expect(rows()).toHaveLength(3));
    fireEvent.change(screen.getByRole("searchbox", { name: "Search snippets" }), { target: { value: "disk" } });
    expect(rows()).toHaveLength(1);
    expect(rows()[0].textContent).toContain("Disk usage");
  });

  it("creates a snippet; Save waits for a label and a command", async () => {
    render(<SnippetsPage />);
    await waitFor(() => expect(rows()).toHaveLength(3));
    fireEvent.click(screen.getByRole("button", { name: "New snippet" }));
    const dialog = await screen.findByRole("dialog", { name: "New snippet" });
    const save = within(dialog).getByRole("button", { name: "Save" });
    expect(save).toHaveProperty("disabled", true);
    fireEvent.change(within(dialog).getByLabelText("Label"), { target: { value: "Uptime" } });
    expect(save).toHaveProperty("disabled", true);
    fireEvent.change(within(dialog).getByLabelText("Command"), { target: { value: "uptime" } });
    fireEvent.click(within(dialog).getByRole("checkbox", { name: "Run after sending" }));
    fireEvent.click(save);
    await waitFor(() => expect(rows()).toHaveLength(4));
    const made = (await backend.mock.listEntities("snippet")).find((s) => s.label === "Uptime");
    expect(made).toMatchObject({ text: "uptime", run: true });
  });

  it("edits from the row and from its menu", async () => {
    render(<SnippetsPage />);
    await waitFor(() => expect(rows()).toHaveLength(3));
    fireEvent.click(within(list()).getByRole("button", { name: /^Upgrade/ }));
    const dialog = await screen.findByRole("dialog", { name: "Edit snippet" });
    fireEvent.change(within(dialog).getByLabelText("Label"), { target: { value: "Upgrade all" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(list().textContent).toContain("Upgrade all"));

    fireEvent.click(screen.getByRole("button", { name: "Actions for Disk usage" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Edit" }));
    expect(await screen.findByRole("dialog", { name: "Edit snippet" })).toBeTruthy();
  });

  it("deletes after a confirmation", async () => {
    render(<SnippetsPage />);
    await waitFor(() => expect(rows()).toHaveLength(3));
    fireEvent.click(screen.getByRole("button", { name: "Actions for Disk usage" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete" }));
    const dialog = await screen.findByRole("dialog", { name: "Delete snippet?" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(rows()).toHaveLength(2));
    expect(list().textContent).not.toContain("Disk usage");
  });
});
