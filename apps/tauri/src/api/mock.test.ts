import { describe, expect, it } from "vitest";
import { createMockApi } from "./mock";
import type { ConnectStep, SessionHandlers } from "./types";

function recorder() {
  const steps: ConnectStep[] = [];
  const handlers: SessionHandlers = {
    onOutput: () => {},
    onClosed: () => {},
    onProgress: (step) => steps.push(step),
  };
  return { steps, handlers };
}

async function mockWithHost() {
  const api = createMockApi();
  await api.vaultCreate("pw");
  const host = await api.upsertEntity("host", { label: "web", address: "web.test" });
  return { api, hostId: host.id };
}

describe("mock openSession progress", () => {
  it("mock_open_session_reports_all_steps_then_resolves", async () => {
    const { api, hostId } = await mockWithHost();
    const { steps, handlers } = recorder();
    const opened = await api.openSession({ kind: "host", hostId }, { cols: 80, rows: 24 }, handlers);
    expect(opened.sessionId).toBeTruthy();
    expect(steps).toEqual(["tcp", "hostKey", "auth", "shell"]);
  });

  it("mock_open_session_for_a_local_shell_reports_only_shell", async () => {
    const api = createMockApi();
    const { steps, handlers } = recorder();
    await api.openSession({ kind: "local" }, { cols: 80, rows: 24 }, handlers);
    expect(steps).toEqual(["shell"]);
  });

  it("mock_fail_at_auth_reports_up_to_auth_then_throws", async () => {
    const { api, hostId } = await mockWithHost();
    api.control.failNextOpenSession("Permission denied", "auth");
    const { steps, handlers } = recorder();
    await expect(
      api.openSession({ kind: "host", hostId }, { cols: 80, rows: 24 }, handlers),
    ).rejects.toMatchObject({ code: "connect", message: "Permission denied" });
    expect(steps).toEqual(["tcp", "hostKey", "auth"]);
  });

  it("mock_fail_without_a_step_reports_nothing", async () => {
    const { api, hostId } = await mockWithHost();
    api.control.failNextOpenSession("unreachable");
    const { steps, handlers } = recorder();
    await expect(
      api.openSession({ kind: "host", hostId }, { cols: 80, rows: 24 }, handlers),
    ).rejects.toMatchObject({ code: "connect" });
    expect(steps).toEqual([]);
  });

  it("mock_hold_freezes_an_open_until_released_or_failed", async () => {
    const { api, hostId } = await mockWithHost();
    const size = { cols: 80, rows: 24 };

    const held = api.control.holdNextOpenSession();
    const a = recorder();
    const opening = api.openSession({ kind: "host", hostId }, size, a.handlers);
    let settled = false;
    void opening.then(() => (settled = true));
    await new Promise((r) => setTimeout(r, 30));
    expect(settled).toBe(false);
    expect(a.steps).toEqual(["tcp"]);
    held.release();
    await opening;
    expect(a.steps).toEqual(["tcp", "hostKey", "auth", "shell"]);

    const failing = api.control.holdNextOpenSession();
    const b = recorder();
    const second = api.openSession({ kind: "host", hostId }, size, b.handlers);
    const outcome = expect(second).rejects.toMatchObject({ code: "connect", message: "boom" });
    await new Promise((r) => setTimeout(r, 10));
    failing.fail("boom");
    await outcome;
    expect(b.steps).toEqual(["tcp"]);
  });
});
