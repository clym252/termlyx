import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockApi, type MockTetherApi } from "../api/mock";

const backend = vi.hoisted(() => ({ mock: null as unknown as MockTetherApi }));
vi.mock("../api", () => ({
  api: new Proxy({}, { get: (_, key) => Reflect.get(backend.mock, key) }),
}));
const platform = vi.hoisted(() => ({ mobile: true }));
vi.mock("../platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../platform")>()),
  get IS_MOBILE() {
    return platform.mobile;
  },
}));

import { SELF_HOST_DOCS_URL } from "../app/brand";
import { Dialog } from "../ui/Dialog";
import { SyncFlow } from "./SyncFlow";

const LINK = "tether1-link-demo";
const code = () => screen.getByLabelText(/invite or add-device code/i) as HTMLInputElement;
const scanButton = () => screen.queryByRole("button", { name: "Scan QR code" });

beforeEach(() => {
  // jsdom has no modal <dialog>; remember which ones were opened that way.
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
    this.setAttribute("data-modal", "");
  };
  backend.mock = createMockApi();
  platform.mobile = true;
});
afterEach(() => {
  cleanup();
  document.documentElement.removeAttribute("data-scanning");
});

describe("SyncFlow Scan QR code", () => {
  it("is offered on iOS only", () => {
    const { unmount } = render(<SyncFlow context="firstRun" onClose={() => {}} onDone={() => {}} />);
    expect(scanButton()).not.toBeNull();
    unmount();
    platform.mobile = false;
    render(<SyncFlow context="firstRun" onClose={() => {}} onDone={() => {}} />);
    expect(scanButton()).toBeNull();
  });

  it("fills the code field with what was scanned and goes on to the server and fingerprint", async () => {
    backend.mock.control.setScan(LINK);
    render(<SyncFlow context="firstRun" onClose={() => {}} onDone={() => {}} />);
    fireEvent.click(scanButton()!);
    await waitFor(() => expect(screen.getByTestId("fingerprint")).toBeTruthy());
    expect(code().value).toBe(LINK);
    expect(screen.getByText("https://sync.mock.test")).toBeTruthy();
  });

  it("leaves the field empty and says nothing is wrong when the user cancels", async () => {
    let finish: (text: string | null) => void = () => {};
    backend.mock.scanQr = () => new Promise((resolve) => (finish = resolve));
    const cancel = vi.fn(async () => finish(null));
    backend.mock.cancelScan = cancel;
    render(<SyncFlow context="firstRun" onClose={() => {}} onDone={() => {}} />);
    fireEvent.click(scanButton()!);
    // While the camera is open the page is see-through and offers Cancel.
    await waitFor(() => expect(document.documentElement.dataset.scanning).toBe("true"));
    fireEvent.click(screen.getByRole("button", { name: "Cancel scan" }));
    await waitFor(() => expect(document.documentElement.dataset.scanning).toBeUndefined());
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(code().value).toBe("");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText(/camera/i)).toBeNull();
  });

  it("explains a scan that could not start (camera denied) and keeps paste available", async () => {
    backend.mock.control.setScan(null);
    render(<SyncFlow context="firstRun" onClose={() => {}} onDone={() => {}} />);
    fireEvent.click(scanButton()!);
    expect((await screen.findByRole("alert")).textContent).toMatch(/camera/i);
    expect(code().value).toBe("");
    fireEvent.change(code(), { target: { value: LINK } });
    expect(code().value).toBe(LINK);
  });

  it("does not keep the scanned text anywhere but the field", async () => {
    backend.mock.control.setScan("tether1-invite-demo");
    render(<SyncFlow context="firstRun" onClose={() => {}} onDone={() => {}} />);
    fireEvent.click(scanButton()!);
    // An invite is refused on this screen: the field keeps what was scanned, the rest of the page does not.
    await screen.findByRole("alert");
    expect(JSON.stringify({ ...localStorage, ...sessionStorage })).not.toContain("invite-demo");
    expect(window.location.href).not.toContain("invite-demo");
  });

  it("hosted in a dialog (Settings), the camera prompt is its own modal dialog above it, and Cancel works", async () => {
    let finish: (text: string | null) => void = () => {};
    backend.mock.scanQr = () => new Promise((resolve) => (finish = resolve));
    backend.mock.cancelScan = vi.fn(async () => finish(null));
    render(
      <Dialog title="Connect to server">
        <SyncFlow context="settings" onClose={() => {}} onDone={() => {}} />
      </Dialog>,
    );
    const host = screen.getByRole("dialog", { name: "Connect to server", hidden: true });
    fireEvent.click(scanButton()!);
    const overlay = await screen.findByRole("dialog", { name: "Scan QR code", hidden: true });
    // Its own modal in the top layer: an open modal makes everything outside it inert, so it
    // must not be a child of the host dialog's page content nor a plain element in <body>.
    expect(overlay.tagName).toBe("DIALOG");
    expect(overlay.hasAttribute("data-modal")).toBe(true);
    expect(host.contains(overlay)).toBe(false);
    expect(overlay.parentElement).toBe(document.body);
    expect(document.documentElement.dataset.scanning).toBe("true");

    // Escape (the dialog's cancel event) cancels the scan, like the button.
    fireEvent(overlay, new Event("cancel", { cancelable: true }));
    await waitFor(() => expect(document.documentElement.dataset.scanning).toBeUndefined());
    expect(screen.queryByRole("dialog", { name: "Scan QR code", hidden: true })).toBeNull();
    expect(backend.mock.cancelScan).toHaveBeenCalledTimes(1);
    // The form is usable again.
    expect(scanButton()).toHaveProperty("disabled", false);
  });

  it("ends the scan when the vault locks, so the unlock screen is not hidden", async () => {
    let finish: (text: string | null) => void = () => {};
    backend.mock.scanQr = () => new Promise((resolve) => (finish = resolve));
    const cancel = vi.fn(async () => finish(null));
    backend.mock.cancelScan = cancel;
    render(<SyncFlow context="settings" onClose={() => {}} onDone={() => {}} />);
    // A lock with no scan running asks nothing of the camera.
    await backend.mock.vaultLock();
    expect(cancel).not.toHaveBeenCalled();

    fireEvent.click(scanButton()!);
    await waitFor(() => expect(document.documentElement.dataset.scanning).toBe("true"));
    await backend.mock.vaultLock();
    await waitFor(() => expect(document.documentElement.dataset.scanning).toBeUndefined());
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("dialog", { name: "Scan QR code", hidden: true })).toBeNull();
    // Ended by the lock, not by a camera fault: nothing to explain after the unlock.
    expect(screen.queryByRole("alert")).toBeNull();
    expect(code().value).toBe("");
  });

  it("closes the camera when the form goes away mid-scan, and only then", async () => {
    backend.mock.scanQr = () => new Promise(() => {});
    const cancel = vi.fn(async () => {});
    backend.mock.cancelScan = cancel;
    const first = render(<SyncFlow context="firstRun" onClose={() => {}} onDone={() => {}} />);
    first.unmount();
    expect(cancel).not.toHaveBeenCalled();

    const second = render(<SyncFlow context="firstRun" onClose={() => {}} onDone={() => {}} />);
    fireEvent.click(scanButton()!);
    await waitFor(() => expect(document.documentElement.dataset.scanning).toBe("true"));
    second.unmount();
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(document.documentElement.dataset.scanning).toBeUndefined();
  });
});

describe("SyncFlow Connect to server (setup code and address)", () => {
  const SHORT = "TLX-ABCD-EFGH-JKMN-PQRS";
  const address = () => screen.queryByLabelText("Server address") as HTMLInputElement | null;
  const open = () => {
    platform.mobile = false;
    render(<SyncFlow context="settings" onClose={() => {}} onDone={() => {}} />);
  };
  const codeField = () => screen.getByLabelText("Setup, invite or add-device code") as HTMLInputElement;
  const next = () => fireEvent.click(screen.getByRole("button", { name: "Continue" }));

  it("short_code_asks_for_address", async () => {
    open();
    expect(address()).toBeNull();
    fireEvent.change(codeField(), { target: { value: SHORT } });
    next();
    await waitFor(() => expect(address()).not.toBeNull());
    expect(address()!.placeholder).toBe("192.168.1.20 or nas.local");
    // The code step stays until the address is typed; then the server and fingerprint are confirmed.
    expect(screen.queryByTestId("fingerprint")).toBeNull();
    fireEvent.change(address()!, { target: { value: "192.168.1.20" } });
    next();
    expect((await screen.findByTestId("fingerprint")).textContent).toMatch(/^sha256:/);
    expect(screen.getByText("https://192.168.1.20:8443")).toBeTruthy();
  });

  const cases: [string, string][] = [
    ["unreachable", "Can't reach 192.168.1.20:8443. Is the server running, and is this device on the same network?"],
    ["protocol", "That address answers, but it is not a Termlyx server. Check the address and port."],
    ["invalidCode", "That code is not valid on this server. Get the setup code from the server's log, or a new invite."],
    ["serverUnverified", "Couldn't verify this server. Make sure the address is your Termlyx server, not another device."],
    [
      "certificateChanged",
      "This server's certificate changed since you connected. If you reinstalled the server, connect again with a new code.",
    ],
    ["invalidAddress", "That server address isn't valid. Use an address like 192.168.1.20, nas.local or nas.local:8443."],
  ];
  it.each(cases)("check_server_errors_show_spec_text: %s", async (errorCode, text) => {
    open();
    fireEvent.change(codeField(), { target: { value: SHORT } });
    next();
    await waitFor(() => expect(address()).not.toBeNull());
    fireEvent.change(address()!, { target: { value: "192.168.1.20:8443" } });
    backend.mock.control.failNextSync("checkServer", errorCode as never, "raw backend text");
    next();
    expect((await screen.findByRole("alert")).textContent).toContain(text);
    expect(screen.queryByTestId("fingerprint")).toBeNull();
  });

  it("hides the address field again when a full code follows a short one", async () => {
    open();
    fireEvent.change(codeField(), { target: { value: SHORT } });
    next();
    await waitFor(() => expect(address()).not.toBeNull());
    fireEvent.change(codeField(), { target: { value: "tether1-invite-demo" } });
    next();
    await screen.findByTestId("fingerprint");
    expect(address()).toBeNull();
  });

  it("without a fingerprint, says plain HTTP only for an http:// server", async () => {
    open();
    backend.mock.control.setCodeInfo("tether1-public", {
      kind: "invite",
      server: "https://termlyx.example.com",
      fingerprint: null,
    });
    fireEvent.change(codeField(), { target: { value: "tether1-public" } });
    next();
    expect(await screen.findByText("This server uses a publicly trusted certificate.")).toBeTruthy();
    expect(screen.queryByText(/plain HTTP/)).toBeNull();
    cleanup();

    open();
    backend.mock.control.setCodeInfo("tether1-loopback", {
      kind: "invite",
      server: "http://127.0.0.1:8080",
      fingerprint: null,
    });
    fireEvent.change(codeField(), { target: { value: "tether1-loopback" } });
    next();
    expect(await screen.findByText(/reached over plain HTTP/)).toBeTruthy();
    expect(screen.queryByText("This server uses a publicly trusted certificate.")).toBeNull();
  });

  it("shows the self-host guide address with a Copy button, in Settings only", async () => {
    const write = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { value: { writeText: write }, configurable: true });
    open();
    expect(screen.getByText(SELF_HOST_DOCS_URL)).toBeTruthy();
    expect(screen.queryByRole("link")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(write).toHaveBeenCalledWith(SELF_HOST_DOCS_URL));
    cleanup();
    render(<SyncFlow context="firstRun" onClose={() => {}} onDone={() => {}} />);
    expect(screen.queryByText(SELF_HOST_DOCS_URL)).toBeNull();
  });
});
