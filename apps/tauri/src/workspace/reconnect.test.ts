import { describe, expect, it } from "vitest";
import {
  RETRY_DELAYS_MS,
  isFatalFailure,
  reconnectReducer,
  type ReconnectEvent,
  type ReconnectState,
} from "./reconnect";

const idle: ReconnectState = { kind: "idle" };

/** Runs events through the reducer from `idle`. */
function run(...events: ReconnectEvent[]): ReconnectState {
  return events.reduce(reconnectReducer, idle);
}
const lost = (now: number, whileActive = true): ReconnectEvent => ({ type: "lost", whileActive, now });
const failed = (now: number, fatal = false): ReconnectEvent => ({ type: "failed", now, fatal });

describe("reconnectReducer", () => {
  it("lost_while_active_schedules_first_try_after_1s", () => {
    expect(run(lost(5000))).toEqual({ kind: "waiting", attempt: 1, dueAt: 6000 });
  });

  it("a_tick_before_the_due_time_keeps_waiting_and_at_it_starts_the_attempt", () => {
    const waiting = run(lost(0));
    expect(reconnectReducer(waiting, { type: "tick", now: 999 })).toBe(waiting);
    expect(reconnectReducer(waiting, { type: "tick", now: 1000 })).toEqual({ kind: "trying", attempt: 1 });
  });

  it("delays_follow_1_2_4_8_15", () => {
    expect(RETRY_DELAYS_MS).toEqual([1000, 2000, 4000, 8000, 15000]);
    let s: ReconnectState = run(lost(0));
    let now = 0;
    const waited: number[] = [];
    for (let n = 1; n <= 5; n++) {
      expect(s.kind).toBe("waiting");
      if (s.kind !== "waiting") return;
      expect(s.attempt).toBe(n);
      waited.push(s.dueAt - now);
      now = s.dueAt;
      s = reconnectReducer(s, { type: "tick", now });
      expect(s).toEqual({ kind: "trying", attempt: n });
      s = reconnectReducer(s, failed(now));
    }
    expect(waited).toEqual([1000, 2000, 4000, 8000, 15000]);
    expect(s).toEqual({ kind: "gaveUp" });
  });

  it("five_failures_give_up", () => {
    let s: ReconnectState = run(lost(0));
    for (let n = 1; n <= 5; n++) {
      s = reconnectReducer(s, { type: "reconnectNow" });
      expect(s).toEqual({ kind: "trying", attempt: n });
      s = reconnectReducer(s, failed(0));
    }
    expect(s).toEqual({ kind: "gaveUp" });
  });

  it("lost_in_background_waits_for_tap", () => {
    const s = run(lost(0, false));
    expect(s).toEqual({ kind: "manual" });
    expect(reconnectReducer(s, { type: "tick", now: 60_000 })).toBe(s);
    expect(reconnectReducer(s, { type: "reconnectNow" })).toEqual({ kind: "trying", attempt: 1 });
  });

  it("cancel_stops_retrying", () => {
    expect(run(lost(0), { type: "cancel" })).toEqual({ kind: "gaveUp" });
    expect(run(lost(0), { type: "reconnectNow" }, { type: "cancel" })).toEqual({ kind: "gaveUp" });
    // Whatever the cancelled attempt reports afterwards is ignored.
    const s = run(lost(0), { type: "reconnectNow" }, { type: "cancel" }, failed(10));
    expect(s).toEqual({ kind: "gaveUp" });
  });

  it("host_key_failure_is_fatal_and_stops", () => {
    expect(run(lost(0), { type: "reconnectNow" }, failed(0, true))).toEqual({ kind: "gaveUp" });
    expect(isFatalFailure("hostKey", "hostKeyRejected")).toBe(true);
    expect(isFatalFailure("hostKey", undefined)).toBe(true);
    expect(isFatalFailure("auth", "authFailed")).toBe(true);
    expect(isFatalFailure(null, "hostKeyRejected")).toBe(true);
    expect(isFatalFailure("tcp", "connect")).toBe(false);
    expect(isFatalFailure("auth", "connect")).toBe(false);
    expect(isFatalFailure(null, "connect")).toBe(false);
    expect(isFatalFailure("tcp", undefined)).toBe(false);
  });

  it("locked_during_wait_becomes_manual", () => {
    expect(run(lost(0), { type: "locked" })).toEqual({ kind: "manual" });
    expect(run(lost(0), { type: "reconnectNow" }, { type: "locked" })).toEqual({ kind: "manual" });
    // An attempt that was already under way fails or opens without waking the countdown.
    expect(run(lost(0), { type: "reconnectNow" }, { type: "locked" }, failed(0))).toEqual({ kind: "manual" });
  });

  it("opened_returns_to_idle_and_a_later_drop_starts_over", () => {
    const s = run(lost(0), { type: "reconnectNow" }, failed(0), { type: "reconnectNow" }, { type: "opened" });
    expect(s).toEqual(idle);
    expect(reconnectReducer(s, lost(100))).toEqual({ kind: "waiting", attempt: 1, dueAt: 1100 });
  });

  it("reconnect_now_from_gave_up_starts_at_attempt_1", () => {
    expect(run(lost(0), { type: "cancel" }, { type: "reconnectNow" })).toEqual({ kind: "trying", attempt: 1 });
  });

  it("reconnect_now_while_waiting_keeps_the_attempt_number", () => {
    const s = run(lost(0), { type: "reconnectNow" }, failed(0), { type: "reconnectNow" });
    expect(s).toEqual({ kind: "trying", attempt: 2 });
  });

  it("a_first_connect_failure_leaves_idle_alone", () => {
    expect(reconnectReducer(idle, failed(0))).toBe(idle);
    expect(reconnectReducer(idle, { type: "tick", now: 1 })).toBe(idle);
    expect(reconnectReducer(idle, { type: "cancel" })).toBe(idle);
    expect(reconnectReducer(idle, { type: "locked" })).toBe(idle);
  });

  it("a_drop_during_an_attempt_counts_as_that_attempt_failing", () => {
    const trying = run(lost(0), { type: "reconnectNow" });
    expect(reconnectReducer(trying, lost(5))).toEqual({ kind: "waiting", attempt: 2, dueAt: 2005 });
    // The fifth attempt dropping gives up; it never starts over.
    let s: ReconnectState = run(lost(0));
    for (let n = 1; n <= 5; n++) {
      s = reconnectReducer(s, { type: "reconnectNow" });
      s = reconnectReducer(s, lost(0));
    }
    expect(s).toEqual({ kind: "gaveUp" });
  });

  it("a_drop_during_an_attempt_in_the_background_waits_for_the_user", () => {
    const trying = run(lost(0), { type: "reconnectNow" });
    expect(reconnectReducer(trying, lost(5, false))).toEqual({ kind: "manual" });
  });
});
