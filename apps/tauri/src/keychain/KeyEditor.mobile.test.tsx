import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createMockApi } from "../api/mock";

// Before platform.ts loads: this run is an iPhone.
vi.hoisted(() => {
  (window as { __TETHER_OS__?: string }).__TETHER_OS__ = "ios";
});
vi.mock("../api", () => ({ api: createMockApi() }));

import { api } from "../api";
import type { MockTetherApi } from "../api/mock";
import { SECRET_CLIPBOARD_MS } from "../ui/clipboard";
import { KeyEditor } from "./KeyEditor";

const mock = api as MockTetherApi;

beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it("copies after the master password and clears the clipboard a minute later", async () => {
  await mock.vaultCreate("master");
  const key = await mock.keyGenerate("ed25519", "phone");
  vi.useFakeTimers({ shouldAdvanceTime: true });
  render(<KeyEditor sshKey={key} onClose={() => {}} onSaved={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: "Export private key…" }));
  fireEvent.change(screen.getByLabelText("Master password"), {
    target: { value: "master" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Copy private key" }));
  expect((await screen.findByRole("status")).textContent).toContain("cleared in a minute");
  expect(mock.control.clipboard()).toContain("PRIVATE KEY");

  await act(async () => {
    await vi.advanceTimersByTimeAsync(SECRET_CLIPBOARD_MS);
  });
  expect(mock.control.clipboard() ?? "").toBe("");
});

it("leaves the clipboard alone when something else was copied since", async () => {
  const { clearClipboardLater } = await import("../ui/clipboard");
  vi.useFakeTimers();
  await mock.writeClipboard("secret");
  clearClipboardLater("secret");
  mock.control.setClipboard("something else");
  await vi.advanceTimersByTimeAsync(SECRET_CLIPBOARD_MS);
  expect(mock.control.clipboard()).toBe("something else");
});
