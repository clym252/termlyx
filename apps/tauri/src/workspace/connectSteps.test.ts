import { describe, expect, it } from "vitest";
import { STEP_LABELS, STEPS, failureLine, stepMarks } from "./connectSteps";

describe("stepMarks", () => {
  it("marks_nothing_before_the_first_step", () => {
    expect(stepMarks(null, false)).toEqual({ tcp: "todo", hostKey: "todo", auth: "todo", shell: "todo" });
  });

  it("marks_earlier_steps_done_and_the_reported_one_current", () => {
    expect(stepMarks("auth", false)).toEqual({ tcp: "done", hostKey: "done", auth: "current", shell: "todo" });
  });

  it("marks_the_failed_step_and_leaves_later_ones_hollow", () => {
    expect(stepMarks("tcp", true)).toEqual({ tcp: "failed", hostKey: "todo", auth: "todo", shell: "todo" });
    expect(stepMarks("shell", true)).toEqual({ tcp: "done", hostKey: "done", auth: "done", shell: "failed" });
  });

  it("follows_a_step_that_goes_backwards", () => {
    // The backend's reconnect-once path reports shell, then tcp again.
    expect(stepMarks("tcp", false)).toEqual({ tcp: "current", hostKey: "todo", auth: "todo", shell: "todo" });
  });

  it("has_a_label_for_every_step", () => {
    expect(STEPS.map((s) => STEP_LABELS[s])).toEqual([
      "Reaching host",
      "Checking host key",
      "Signing in",
      "Opening shell",
    ]);
  });
});

describe("failureLine", () => {
  it("joins_the_step_and_the_reason", () => {
    expect(failureLine("auth", "password rejected")).toBe("Signing in — password rejected");
  });

  it("words_the_cores_timeout_with_its_limit", () => {
    expect(failureLine("tcp", "timed out")).toBe("Reaching host — timed out after 15 s");
    expect(failureLine("tcp", "connect failed: timed out")).toBe("Reaching host — timed out after 15 s");
  });

  it("leaves_other_reasons_alone", () => {
    expect(failureLine("tcp", "connection refused (os error 111)")).toBe(
      "Reaching host — connection refused (os error 111)",
    );
  });

  it("is_just_the_reason_when_no_step_was_reached", () => {
    expect(failureLine(null, "host not found")).toBe("host not found");
  });
});
