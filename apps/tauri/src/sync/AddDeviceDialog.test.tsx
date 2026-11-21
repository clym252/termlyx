import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { LinkCode } from "../api/types";

const backend = vi.hoisted(() => ({ linkCode: null as unknown as (address?: string) => Promise<LinkCode> }));
vi.mock("../api", () => ({
  api: { sync: { linkCode: (address?: string) => backend.linkCode(address) } },
}));

import { AddDeviceDialog } from "./AddDeviceDialog";

const CODE = "tether1-link-abc123SECRETxyz";
const qr = () => screen.queryByRole("img", { name: "QR code for this add-device code" });

beforeEach(() => {
  // jsdom has no <dialog> showModal
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
  backend.linkCode = async () => ({ code: CODE, server: "https://192.168.1.20:8443", expiresAt: Date.now() + 600_000 });
});
afterEach(() => cleanup());

describe("AddDeviceDialog QR code", () => {
  it("draws a QR code of the code beside the text, with no copy of the code in the image", async () => {
    render(<AddDeviceDialog onClose={() => {}} />);
    const svg = await screen.findByRole("img", { name: "QR code for this add-device code" });
    expect(svg.tagName.toLowerCase()).toBe("svg");
    // Modules only: the code is not written into the image, and nothing is loaded or linked.
    expect(svg.outerHTML).not.toContain(CODE);
    expect(svg.querySelectorAll("path").length).toBeGreaterThan(0);
    expect(svg.querySelector("image, a, foreignObject, script")).toBeNull();
    expect(screen.getByLabelText("Add-device code")).toHaveProperty("value", CODE);
    expect(screen.getByRole("button", { name: "Copy code" })).toBeTruthy();
  });

  it("is a different picture for a different code", async () => {
    const { unmount } = render(<AddDeviceDialog onClose={() => {}} />);
    const first = (await screen.findByRole("img", { name: "QR code for this add-device code" })).innerHTML;
    unmount();
    backend.linkCode = async () => ({ code: "tether1-link-another-one", server: "https://s:8443", expiresAt: Date.now() + 600_000 });
    render(<AddDeviceDialog onClose={() => {}} />);
    const second = (await screen.findByRole("img", { name: "QR code for this add-device code" })).innerHTML;
    expect(second).not.toBe(first);
  });

  it("is gone with the code when it expires, and with the dialog when it closes", async () => {
    vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
    try {
      const { unmount } = render(<AddDeviceDialog onClose={() => {}} />);
      await act(async () => {});
      expect(qr()).not.toBeNull();
      await act(async () => {
        vi.advanceTimersByTime(601_000);
      });
      expect(screen.getByText("This code has expired.")).toBeTruthy();
      expect(qr()).toBeNull();
      unmount();
      expect(document.body.querySelector("svg")).toBeNull();
      expect(document.body.textContent).not.toContain(CODE);
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the code in no storage", async () => {
    render(<AddDeviceDialog onClose={() => {}} />);
    await screen.findByRole("img", { name: "QR code for this add-device code" });
    expect(JSON.stringify({ ...localStorage })).not.toContain(CODE);
    expect(JSON.stringify({ ...sessionStorage })).not.toContain(CODE);
  });
});

describe("AddDeviceDialog server address", () => {
  it("add_device_address_edit_makes_new_code", async () => {
    const asked: (string | undefined)[] = [];
    backend.linkCode = async (address) => {
      asked.push(address);
      return address
        ? { code: "tether1-link-second", server: `https://${address}:8443`, expiresAt: Date.now() + 600_000 }
        : { code: CODE, server: "https://192.168.1.20:8443", expiresAt: Date.now() + 600_000 };
    };
    render(<AddDeviceDialog onClose={() => {}} />);
    await screen.findByText("https://192.168.1.20:8443");
    expect(screen.getByText(/Server address in this code:/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByLabelText("Server address"), { target: { value: "nas.local" } });
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await screen.findByText("https://nas.local:8443");
    expect(asked).toEqual([undefined, "nas.local"]);
    expect(screen.getByLabelText("Add-device code")).toHaveProperty("value", "tether1-link-second");
    expect(qr()).not.toBeNull();
  });

  it("other Apply failures keep the backend's message, not the setup-code wording", async () => {
    render(<AddDeviceDialog onClose={() => {}} />);
    await screen.findByText("https://192.168.1.20:8443");
    backend.linkCode = async () => {
      throw { code: "unreachable", message: "server unreachable", refs: [] };
    };
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByLabelText("Server address"), { target: { value: "nas.local" } });
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toBe("server unreachable"));
  });

  it("a bad address says so and keeps the code that was showing", async () => {
    render(<AddDeviceDialog onClose={() => {}} />);
    await screen.findByText("https://192.168.1.20:8443");
    backend.linkCode = async () => {
      throw { code: "invalidAddress", message: "raw", refs: [] };
    };
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByLabelText("Server address"), { target: { value: "a b" } });
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("That server address isn't valid."));
    expect(screen.getByLabelText("Add-device code")).toHaveProperty("value", CODE);
  });
});
