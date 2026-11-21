import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const bus = vi.hoisted(() => ({
  subscribed: 0,
  unsubscribed: 0,
  cb: null as null | (() => void),
}));

vi.mock("../api", () => ({
  api: {
    onVaultChanged: vi.fn(async (cb: () => void) => {
      bus.subscribed += 1;
      bus.cb = cb;
      return () => {
        bus.unsubscribed += 1;
      };
    }),
    listEntities: vi.fn(async (kind: string) => (kind === "host" ? [{ id: "h1", label: "web" }] : [])),
  },
}));

import { api } from "../api";
import { useHostsData } from "../hosts/useHostsData";
import { useVaultChanged } from "./vaultChanged";

const fire = () => act(async () => bus.cb?.());
const flush = () => act(async () => {});

beforeEach(() => {
  bus.subscribed = 0;
  bus.unsubscribed = 0;
  bus.cb = null;
  vi.mocked(api.listEntities).mockClear();
});
afterEach(() => cleanup());

describe("useVaultChanged", () => {
  it("shares one subscription between components and drops it with the last", async () => {
    const a = renderHook(() => useVaultChanged(() => {}));
    const b = renderHook(() => useVaultChanged(() => {}));
    await flush();
    expect(bus.subscribed).toBe(1);

    a.unmount();
    expect(bus.unsubscribed).toBe(0);
    b.unmount();
    expect(bus.unsubscribed).toBe(1);

    renderHook(() => useVaultChanged(() => {}));
    await flush();
    expect(bus.subscribed).toBe(2);
  });

  it("drops a subscription that became ready after everyone left", async () => {
    const { unmount } = renderHook(() => useVaultChanged(() => {}));
    unmount();
    await flush();
    expect(bus.subscribed).toBe(1);
    expect(bus.unsubscribed).toBe(1);
  });

  it("calls every mounted component, and not an unmounted one", async () => {
    const a = vi.fn();
    const b = vi.fn();
    renderHook(() => useVaultChanged(a));
    const second = renderHook(() => useVaultChanged(b));
    await flush();
    a.mockClear(); // the catch-up read when the subscription became ready
    b.mockClear();
    await fire();
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1);
    second.unmount();
    await fire();
    expect(a).toHaveBeenCalledTimes(2);
    expect(b).toHaveBeenCalledTimes(1);
  });

  it("calls the latest callback", async () => {
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = renderHook(({ cb }) => useVaultChanged(cb), { initialProps: { cb: first } });
    await flush();
    first.mockClear();
    rerender({ cb: second });
    await fire();
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });

  it("coalesces a burst into the running reload plus one more", async () => {
    let release: (() => void) | null = null;
    let runs = 0;
    const reload = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          runs += 1;
          release = resolve;
        }),
    );
    renderHook(() => useVaultChanged(reload));
    await flush();
    await act(async () => release?.()); // the catch-up read ends
    await flush();
    runs = 0;

    await fire(); // starts a read
    await fire();
    await fire();
    await fire(); // three more while it runs
    expect(runs).toBe(1);

    await act(async () => release?.()); // the first read ends: one more starts
    expect(runs).toBe(2);
    await act(async () => release?.());
    await flush();
    expect(runs).toBe(2);

    await fire(); // idle again: a new event reads again
    expect(runs).toBe(3);
    await act(async () => release?.());
  });

  it("reads once when the subscription becomes ready (a change while it started is not missed)", async () => {
    const reload = vi.fn();
    renderHook(() => useVaultChanged(reload));
    expect(reload).not.toHaveBeenCalled();
    await flush();
    expect(reload).toHaveBeenCalledTimes(1);
    // A second component on the live subscription does not trigger another.
    renderHook(() => useVaultChanged(reload));
    await flush();
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("keeps going after a reload that throws", async () => {
    const reload = vi.fn().mockRejectedValueOnce(new Error("boom")).mockResolvedValue(undefined);
    renderHook(() => useVaultChanged(reload));
    await flush(); // the catch-up read throws
    await fire();
    expect(reload).toHaveBeenCalledTimes(2);
  });
});

describe("useHostsData", () => {
  it("reads again when sync changes the vault", async () => {
    const { result } = renderHook(() => useHostsData());
    await waitFor(() => expect(result.current.data).not.toBeNull());
    expect(result.current.data?.hosts).toEqual([{ id: "h1", label: "web" }]);

    vi.mocked(api.listEntities).mockImplementation(async (kind: string) =>
      kind === "host"
        ? ([
            { id: "h1", label: "web" },
            { id: "h2", label: "from another device" },
          ] as never)
        : [],
    );
    await fire();
    await waitFor(() => expect(result.current.data?.hosts.map((h) => h.id)).toEqual(["h1", "h2"]));
  });
});
