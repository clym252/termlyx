import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockApi, type MockTetherApi } from "../api/mock";

// The page runs against the in-memory backend; `control.remoteUpsert` plays sync pulling a change.
const backend = vi.hoisted(() => ({ mock: null as unknown as MockTetherApi }));
vi.mock("../api", () => ({
  api: new Proxy({}, { get: (_, key) => Reflect.get(backend.mock, key) }),
}));

import { HostsPage } from "./HostsPage";

const mock = () => backend.mock;
const flush = () => act(async () => {});

beforeEach(async () => {
  backend.mock = createMockApi();
  await mock().vaultCreate("a long enough password");
});
afterEach(() => cleanup());

function renderPage() {
  return render(<HostsPage visible openHost={null} onHostOpened={() => {}} onConnect={() => {}} />);
}

describe("hosts page and sync", () => {
  it("lists a host another device added, without a reload by the user", async () => {
    renderPage();
    await flush();
    expect(screen.queryByText("from-laptop")).toBeNull();
    await act(async () => {
      mock().control.remoteUpsert("host", { label: "from-laptop", address: "10.0.0.9" });
    });
    await waitFor(() => expect(screen.getByText("from-laptop")).toBeTruthy());
  });

  it("keeps what was typed in an open host editor when sync changes the vault", async () => {
    const web = await mock().upsertEntity("host", { label: "web", address: "10.0.0.1" });
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Edit host web" }));
    const label = (await screen.findByLabelText("Label")) as HTMLInputElement;
    fireEvent.change(label, { target: { value: "web (typing)" } });

    await act(async () => {
      mock().control.remoteUpsert("host", { id: web.id, label: "web-elsewhere", address: "10.0.0.1" });
    });
    await waitFor(() => expect(screen.getByText("web-elsewhere")).toBeTruthy());
    expect((screen.getByLabelText("Label") as HTMLInputElement).value).toBe("web (typing)");
    // The same input element: the form was not rebuilt.
    expect(screen.getByLabelText("Label")).toBe(label);
  });

  it("keeps the editor open, with its text, when the host is deleted on another device", async () => {
    const web = await mock().upsertEntity("host", { label: "web", address: "10.0.0.1" });
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Edit host web" }));
    const label = (await screen.findByLabelText("Label")) as HTMLInputElement;
    fireEvent.change(label, { target: { value: "still here" } });

    await act(async () => {
      mock().control.remoteDelete("host", web.id);
    });
    await waitFor(() => expect(screen.queryByRole("list", { name: "Hosts" })).toBeNull());
    expect((screen.getByLabelText("Label") as HTMLInputElement).value).toBe("still here");
  });
});
