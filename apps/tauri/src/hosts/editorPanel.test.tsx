import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockApi, type MockTetherApi } from "../api/mock";

// The page runs against the in-memory backend; a test can hold a save in flight.
const backend = vi.hoisted(() => ({ mock: null as unknown as MockTetherApi }));
vi.mock("../api", () => ({
  api: new Proxy({}, { get: (_, key) => Reflect.get(backend.mock, key) }),
}));

import { HostsPage } from "./HostsPage";

const mock = () => backend.mock;
const flush = () => act(async () => {});
const panel = () => screen.queryByRole("complementary", { name: "Host editor" });

beforeEach(async () => {
  backend.mock = createMockApi();
  await mock().vaultCreate("a long enough password");
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function renderPage() {
  return render(<HostsPage visible openHost={null} onHostOpened={() => {}} onConnect={() => {}} />);
}

/** Opens the editor on host "web" and starts a save that stays in flight until `finish()`. */
async function saveInFlight() {
  await mock().upsertEntity("host", { label: "web", address: "10.0.0.1" });
  const realUpsert = mock().upsertEntity.bind(mock());
  let finish: () => void = () => {};
  const held = new Promise<void>((resolve) => (finish = resolve));
  vi.spyOn(mock(), "upsertEntity").mockImplementation(async (kind, input) => {
    await held;
    return realUpsert(kind, input);
  });
  renderPage();
  fireEvent.click(await screen.findByRole("button", { name: "Edit host web" }));
  fireEvent.change(await screen.findByLabelText("Label"), { target: { value: "web-renamed" } });
  fireEvent.click(screen.getByRole("button", { name: "Save host" }));
  await flush();
  return finish;
}

describe("host editor panel", () => {
  it("stays open while a save is running, then closes", async () => {
    const finish = await saveInFlight();
    const close = screen.getByRole("button", { name: "Close editor" }) as HTMLButtonElement;
    expect(close.disabled).toBe(true);
    fireEvent.click(close);
    fireEvent.keyDown(screen.getByLabelText("Label"), { key: "Escape" });
    expect(panel()).not.toBeNull();

    await act(async () => finish());
    // The save finished: the editor closes by itself.
    await waitFor(() => expect(panel()).toBeNull());
    expect(await screen.findByRole("button", { name: "web-renamed" })).not.toBeNull();
  });

  it("ignores the Escape that ends an IME composition", async () => {
    await mock().upsertEntity("host", { label: "web", address: "10.0.0.1" });
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Edit host web" }));
    const label = await screen.findByLabelText("Label");
    fireEvent.keyDown(label, { key: "Escape", isComposing: true });
    fireEvent.keyDown(label, { key: "Process" });
    expect(panel()).not.toBeNull();
    fireEvent.keyDown(label, { key: "Escape" });
    expect(panel()).toBeNull();
  });

  it("closes_after_saving_an_edit_and_shows_the_change_on_the_card", async () => {
    await mock().upsertEntity("host", { label: "web", address: "10.0.0.1" });
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Edit host web" }));
    fireEvent.change(await screen.findByLabelText("Label"), { target: { value: "web-renamed" } });
    fireEvent.change(screen.getByLabelText("Address"), { target: { value: "10.0.0.9" } });
    fireEvent.click(screen.getByRole("button", { name: "Save host" }));

    await waitFor(() => expect(panel()).toBeNull());
    expect(await screen.findByRole("button", { name: "web-renamed" })).not.toBeNull();
    expect(screen.getByText(/10\.0\.0\.9/)).not.toBeNull();
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("closes_after_saving_a_new_host", async () => {
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "New host" }));
    fireEvent.change(await screen.findByLabelText("Label"), { target: { value: "fresh" } });
    fireEvent.change(screen.getByLabelText("Address"), { target: { value: "10.0.0.5" } });
    fireEvent.click(screen.getByRole("button", { name: "Save host" }));
    await waitFor(() => expect(panel()).toBeNull());
    expect(await screen.findByRole("button", { name: "fresh" })).not.toBeNull();
  });

  it("stays_open_with_the_alert_when_the_save_fails", async () => {
    await mock().upsertEntity("host", { label: "web", address: "10.0.0.1" });
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Edit host web" }));
    vi.spyOn(mock(), "upsertEntity").mockRejectedValue(new Error("disk full"));
    fireEvent.change(await screen.findByLabelText("Label"), { target: { value: "web-2" } });
    fireEvent.click(screen.getByRole("button", { name: "Save host" }));
    expect((await screen.findByRole("alert")).textContent).toContain("disk full");
    expect(panel()).not.toBeNull();
  });

  it("retry_after_a_failed_own_login_save_updates_the_same_new_host", async () => {
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "New host" }));
    fireEvent.change(await screen.findByLabelText("Label"), { target: { value: "fresh" } });
    fireEvent.change(screen.getByLabelText("Address"), { target: { value: "10.0.0.5" } });
    fireEvent.change(screen.getByRole("combobox", { name: "Identity" }), { target: { value: "own" } });
    fireEvent.change(screen.getByLabelText("Username"), { target: { value: "root" } });
    vi.spyOn(mock(), "setOwnLogin").mockRejectedValueOnce(new Error("keychain locked"));
    fireEvent.click(screen.getByRole("button", { name: "Save host" }));
    expect((await screen.findByRole("alert")).textContent).toContain("keychain locked");
    expect(panel()).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Save host" }));
    await waitFor(() => expect(panel()).toBeNull());
    const hosts = await mock().listEntities("host");
    expect(hosts).toHaveLength(1);
    const own = (await mock().listEntities("identity")).find((i) => i.owner === hosts[0].id);
    expect(own?.username).toBe("root");
    expect(hosts[0].settings.identityId).toBe(own?.id);
  });

  it("retry_after_a_failed_own_login_save_updates_the_same_new_group", async () => {
    HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
      this.setAttribute("open", "");
    };
    renderPage();
    fireEvent.click((await screen.findAllByRole("button", { name: "New group" }))[0]);
    fireEvent.change(await screen.findByLabelText("Name"), { target: { value: "lab" } });
    fireEvent.change(screen.getByRole("combobox", { name: "Identity" }), { target: { value: "own" } });
    fireEvent.change(screen.getByLabelText("Username"), { target: { value: "root" } });
    vi.spyOn(mock(), "setOwnLogin").mockRejectedValueOnce(new Error("keychain locked"));
    fireEvent.click(screen.getByRole("button", { name: "Save group" }));
    expect((await screen.findByRole("alert")).textContent).toContain("keychain locked");

    fireEvent.click(screen.getByRole("button", { name: "Save group" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Save group" })).toBeNull());
    expect(await mock().listEntities("group")).toHaveLength(1);
  });

  it("closes_the_group_editor_after_saving_a_group", async () => {
    // jsdom has no modal dialogs.
    HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
      this.setAttribute("open", "");
    };
    renderPage();
    fireEvent.click((await screen.findAllByRole("button", { name: "New group" }))[0]);
    fireEvent.change(await screen.findByLabelText("Name"), { target: { value: "lab" } });
    fireEvent.click(screen.getByRole("button", { name: "Save group" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Save group" })).toBeNull());
    expect((await mock().listEntities("group")).map((g) => g.name)).toEqual(["lab"]);
  });
});

describe("connection settings without Override", () => {
  const field = (name: string) => screen.getByRole("combobox", { name }) as HTMLSelectElement;
  const port = () => screen.getByRole("spinbutton", { name: "Port" }) as HTMLInputElement;
  const stored = async () => (await mock().listEntities("host")).find((h) => h.label === "web")!.settings;
  // Saving closes the editor.
  const save = async () => {
    fireEvent.click(screen.getByRole("button", { name: "Save host" }));
    await waitFor(() => expect(panel()).toBeNull());
  };
  const reopen = async () => {
    fireEvent.click(await screen.findByRole("button", { name: "Edit host web" }));
    await screen.findByLabelText("Label");
  };
  async function openWeb(settings: Record<string, unknown> = {}) {
    await mock().upsertEntity("host", { label: "web", address: "10.0.0.1", settings });
    renderPage();
    await reopen();
  }

  it("no override checkboxes", async () => {
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "New host" }));
    await screen.findByLabelText("Label");
    expect(screen.queryAllByRole("checkbox", { name: /override/i })).toEqual([]);
  });

  it("identity select is enabled and inherits by default", async () => {
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "New host" }));
    await screen.findByLabelText("Label");
    const identity = field("Identity");
    expect(identity.disabled).toBe(false);
    expect(identity.selectedOptions[0].textContent).toMatch(/^Same as default/);
  });

  it("picking a theme sets it; inherit clears it", async () => {
    await openWeb();
    fireEvent.change(field("Color scheme"), { target: { value: "nord" } });
    expect(field("Color scheme").selectedOptions[0].textContent).toBe("Nord");
    await save();
    expect((await stored()).colorScheme).toBe("nord");
    await reopen();
    const inherit = [...field("Color scheme").options].find((o) => o.text.startsWith("Same as"))!;
    fireEvent.change(field("Color scheme"), { target: { value: inherit.value } });
    await save();
    expect((await stored()).colorScheme).toBeNull();
  });

  it("port typed then cleared inherits", async () => {
    await openWeb();
    expect(port().disabled).toBe(false);
    expect(port().placeholder).toBe("22");
    fireEvent.change(port(), { target: { value: "2222" } });
    await save();
    expect((await stored()).port).toBe(2222);
    await reopen();
    fireEvent.change(port(), { target: { value: "" } });
    await save();
    expect((await stored()).port).toBeNull();
    await reopen();
    fireEvent.change(port(), { target: { value: "70000" } });
    fireEvent.click(screen.getByRole("button", { name: "Save host" }));
    expect(await screen.findByText("Port must be a whole number from 1 to 65535.")).not.toBeNull();
    expect(port().getAttribute("aria-invalid")).toBe("true");
  });

  it("text that is not a number is an error, not a silent inherit", async () => {
    await openWeb({ port: 2222 });
    const upsert = vi.spyOn(mock(), "upsertEntity");
    // What WebKit and Firefox report for "22a" typed into a number field.
    Object.defineProperty(port(), "validity", { value: { badInput: true }, configurable: true });
    fireEvent.change(port(), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "Save host" }));
    expect(await screen.findByText("Port must be a whole number from 1 to 65535.")).not.toBeNull();
    expect(upsert).not.toHaveBeenCalled();
    expect((await stored()).port).toBe(2222);
  });

  it("theme and font unset everywhere say they follow the app", async () => {
    await openWeb();
    expect(field("Color scheme").selectedOptions[0].textContent).toBe("Same as app (Termlyx Dark)");
    expect(field("Font family").selectedOptions[0].textContent).toBe("Same as app (JetBrains Mono)");
  });

  it("charset survives a save", async () => {
    await openWeb({ charset: "ISO-8859-1" });
    expect(screen.queryByLabelText(/charset/i)).toBeNull();
    await save();
    expect((await stored()).charset).toBe("ISO-8859-1");
  });

  it("custom term type survives", async () => {
    await openWeb({ termType: "linux" });
    expect(field("Terminal type").selectedOptions[0].textContent).toBe("linux (custom)");
    await save();
    expect((await stored()).termType).toBe("linux");
  });

  it("a theme stored by its old name stays selected and is kept", async () => {
    await openWeb({ colorScheme: "Tether Light" });
    expect(field("Color scheme").selectedOptions[0].textContent).toBe("Termlyx Light");
    await save();
    expect((await stored()).colorScheme).toBe("Tether Light");
  });

  it("a typed font stack is kept", async () => {
    await openWeb({ fontFamily: "Iosevka, monospace" });
    expect(field("Font family").selectedOptions[0].textContent).toBe("Iosevka (custom)");
    await save();
    expect((await stored()).fontFamily).toBe("Iosevka, monospace");
  });

  it("picking own login under an inherited own login starts this host's own", async () => {
    const group = await mock().upsertEntity("group", { name: "Prod" });
    await mock().setOwnLogin({ ownerId: group.id, username: "alice" });
    const login = (await mock().listEntities("identity")).find((i) => i.owner === group.id)!;
    await mock().upsertEntity("group", { id: group.id, name: "Prod", settings: { identityId: login.id } });
    await mock().upsertEntity("host", { label: "web", address: "10.0.0.1", groupId: group.id });
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Prod" }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit host web" }));
    await screen.findByLabelText("Label");
    const identity = field("Identity");
    expect(identity.selectedOptions[0].textContent).toBe("Same as group Prod (Prod login (alice))");
    // The group's login is inherited, never offered to link.
    expect([...identity.options].map((o) => o.value)).not.toContain(login.id);
    expect([...identity.options].map((o) => o.text)).not.toContain("No identity");
    fireEvent.change(identity, { target: { value: "own" } });
    expect((screen.getByLabelText("Username") as HTMLInputElement).value).toBe("alice");
  });
});
