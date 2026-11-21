import type { ConnectStep } from "../api/types";

/** The steps of opening a session, in the order a first connect reports them. */
export const STEPS: readonly ConnectStep[] = ["tcp", "hostKey", "auth", "shell"];

export const STEP_LABELS: Record<ConnectStep, string> = {
  tcp: "Reaching host",
  hostKey: "Checking host key",
  auth: "Signing in",
  shell: "Opening shell",
};

export type StepMark = "done" | "current" | "failed" | "todo";

/**
 * How each step shows, from the last one the backend reported (`null`: none
 * yet) and whether the open has failed there. Only the last report counts,
 * so a step that goes backwards (a reconnect after a dead pooled connection
 * starts over at `tcp`) simply moves the marker back.
 */
export function stepMarks(current: ConnectStep | null, failed: boolean): Record<ConnectStep, StepMark> {
  const at = current === null ? -1 : STEPS.indexOf(current);
  const marks = {} as Record<ConnectStep, StepMark>;
  STEPS.forEach((step, i) => {
    marks[step] = i < at ? "done" : i === at ? (failed ? "failed" : "current") : "todo";
  });
  return marks;
}

/** The core's connect timeout (`connect_timeout` in core-ssh), in seconds. */
const CONNECT_TIMEOUT_SECS = 15;

/** The failed step's line: its name and why. With no step reported, the reason alone. */
export function failureLine(step: ConnectStep | null, error: string): string {
  const reason = /^(connect failed:\s*)?timed out$/i.test(error.trim())
    ? `timed out after ${CONNECT_TIMEOUT_SECS} s`
    : error;
  return step === null ? reason : `${STEP_LABELS[step]} — ${reason}`;
}
