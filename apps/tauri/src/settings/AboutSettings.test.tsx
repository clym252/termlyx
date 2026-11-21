import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockApi } from "../api/mock";

vi.mock("../api", () => ({ api: createMockApi() }));

import { api } from "../api";
import type { MockTetherApi } from "../api/mock";
import { AboutSettings } from "./AboutSettings";

const mock = api as MockTetherApi;

beforeEach(() => {
  // jsdom has no <dialog> showModal
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
  localStorage.clear();
  mock.control.setUpdate(null);
  mock.control.failUpdateInstall(null);
  // jsdom is no macOS or Windows; the cases that need another answer set it.
  mock.updates.supported = true;
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const checkButton = () => screen.getByRole("button", { name: "Check for updates" });

describe("AboutSettings", () => {
  it("shows the app version", async () => {
    render(<AboutSettings />);
    expect(await screen.findByText("Termlyx 0.1.0")).toBeTruthy();
  });

  it("opens the third-party licenses", async () => {
    render(<AboutSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Third-party licenses" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog.textContent).toContain("JetBrains Mono");
    expect(dialog.textContent).toContain("russh");
  });

  it("offers an available update", async () => {
    mock.control.setUpdate({ version: "0.2.0", notes: "Fixes", date: null });
    render(<AboutSettings />);
    fireEvent.click(checkButton());
    expect(await screen.findByText("Version 0.2.0 is available")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Update now" })).toBeTruthy();
  });

  it("says when it is up to date", async () => {
    render(<AboutSettings />);
    fireEvent.click(checkButton());
    expect(await screen.findByText("Termlyx is up to date.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Update now" })).toBeNull();
  });

  it("says when the check failed", async () => {
    mock.control.setUpdate("fail");
    render(<AboutSettings />);
    fireEvent.click(checkButton());
    expect(await screen.findByText("Couldn't check for updates.")).toBeTruthy();
  });

  it("says a failed install in a short message, the detail only in the debug log", async () => {
    const debug = vi.spyOn(console, "debug").mockImplementation(() => {});
    mock.control.setUpdate({ version: "0.2.0", notes: "", date: null });
    mock.control.failUpdateInstall("error sending request for url (https://cdn.example/u.zip?sig=SECRET)");
    render(<AboutSettings />);
    fireEvent.click(checkButton());
    fireEvent.click(await screen.findByRole("button", { name: "Update now" }));
    expect(await screen.findByText("Couldn't install the update.")).toBeTruthy();
    expect(document.body.textContent).not.toContain("error sending request");
    expect(debug).toHaveBeenCalledTimes(1);
    expect(String(debug.mock.calls[0])).toContain("https://cdn.example/u.zip");
    expect(String(debug.mock.calls[0])).not.toContain("SECRET");
  });

  it("keeps the automatic check choice", async () => {
    render(<AboutSettings />);
    const box = screen.getByRole("checkbox", { name: "Automatically check for updates" }) as HTMLInputElement;
    expect(box.checked).toBe(true);
    fireEvent.click(box);
    expect(box.checked).toBe(false);
    expect(localStorage.getItem("tether.update-check")).toBe("false");
    cleanup();
    render(<AboutSettings />);
    expect((screen.getByRole("checkbox", { name: "Automatically check for updates" }) as HTMLInputElement).checked).toBe(
      false,
    );
  });

  it("shows no update controls where updates are not supported", async () => {
    mock.updates.supported = false;
    render(<AboutSettings />);
    await screen.findByText("Termlyx 0.1.0");
    expect(screen.queryByRole("button", { name: "Check for updates" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "Automatically check for updates" })).toBeNull();
  });
});
