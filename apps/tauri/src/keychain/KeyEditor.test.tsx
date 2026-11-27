import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockApi } from "../api/mock";

vi.mock("../api", () => ({ api: createMockApi() }));

import { api } from "../api";
import type { MockTetherApi } from "../api/mock";
import type { SshKey } from "../api/types";
import { KeyEditor } from "./KeyEditor";

const mock = api as MockTetherApi;
let key: SshKey;

beforeAll(async () => {
  await mock.vaultCreate("master");
});

beforeEach(async () => {
  // jsdom has no <dialog> showModal
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
  key = await mock.keyGenerate("ed25519", "laptop");
});
afterEach(() => cleanup());

async function exportWith(password: string) {
  fireEvent.click(screen.getByRole("button", { name: "Export private key…" }));
  fireEvent.change(screen.getByLabelText("Master password"), {
    target: { value: password },
  });
  fireEvent.click(screen.getByRole("button", { name: "Save private key…" }));
}

describe("KeyEditor export (desktop)", () => {
  it("asks for the master password, then saves through the backend", async () => {
    mock.control.setPick("/home/me/laptop");
    render(<KeyEditor sshKey={key} onClose={() => {}} onSaved={() => {}} />);

    await exportWith("wrong");
    expect((await screen.findByRole("alert")).textContent).toContain("Wrong master password");
    // The field is cleared after the attempt.
    expect((screen.getByLabelText("Master password") as HTMLInputElement).value).toBe("");

    fireEvent.change(screen.getByLabelText("Master password"), {
      target: { value: "master" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save private key…" }));
    expect((await screen.findByRole("status")).textContent).toContain("Private key saved to /home/me/laptop");
    expect(screen.queryByLabelText("Master password")).toBeNull();
    // Nothing went to the clipboard, and the page never showed the key.
    expect(mock.control.clipboard()).toBeNull();
    expect(document.body.innerHTML).not.toContain("PRIVATE KEY");
  });

  it("a cancelled save dialog keeps the password dialog open", async () => {
    mock.control.setPick(null);
    render(<KeyEditor sshKey={key} onClose={() => {}} onSaved={() => {}} />);
    await exportWith("master");
    await waitFor(() =>
      expect(
        (
          screen.getByRole("button", {
            name: "Save private key…",
          }) as HTMLButtonElement
        ).disabled,
      ).toBe(false),
    );
    expect(screen.queryByRole("status")).toBeNull();
    expect((screen.getByLabelText("Master password") as HTMLInputElement).value).toBe("");
  });
});
