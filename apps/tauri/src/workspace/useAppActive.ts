import { useEffect, useState } from "react";

/**
 * How long after the page shows again a drop still counts as one that
 * happened in the background: the dead connection of a suspended app is
 * noticed when it wakes, a moment after the visibility event.
 */
export const SETTLE_MS = 2000;

export interface AppActivity {
  /** The page is in front: the window is shown (desktop) or the app is active (iOS WKWebView reports it the same way). */
  active: boolean;
  /**
   * When the page last went to the background (`Date.now()`), kept for
   * `SETTLE_MS` after it returns; `null` otherwise.
   */
  inactiveSince: number | null;
}

const visible = () => document.visibilityState === "visible";

/**
 * Told of every move to (`true`) or from the front, before any page reacts
 * (Gate: `api.appFront`, which runs the auto-lock). While the check of a
 * return runs, every `useAppActive` stays inactive, so nothing resumes (no
 * reconnect attempt starts) before the backend has locked the vault if it
 * had to. The check handles its own failure (Gate: unknown counts as locked
 * until the vault's status answers); one that throws all the same resumes.
 */
export type FrontCheck = (front: boolean) => Promise<void>;

type Subscriber = (front: boolean, checked: Promise<void> | null) => void;

let frontCheck: FrontCheck | null = null;
/** The check of the latest return while it runs. */
let returning: Promise<void> | null = null;
const subscribers = new Set<Subscriber>();
let listening = false;

/** The one `visibilitychange` listener: the check first, then the hooks. */
function onVisibilityChange() {
  const front = visible();
  let checked: Promise<void> | null = null;
  if (frontCheck) {
    const run = frontCheck(front).catch(() => {});
    if (front) {
      const mine = run.then(() => {
        if (returning === mine) returning = null;
      });
      returning = checked = mine;
    } else {
      returning = null;
    }
  }
  for (const s of [...subscribers]) s(front, checked);
}

function listen() {
  if (listening) return;
  document.addEventListener("visibilitychange", onVisibilityChange);
  listening = true;
}

function stopIfUnused() {
  if (!listening || subscribers.size > 0 || frontCheck !== null) return;
  document.removeEventListener("visibilitychange", onVisibilityChange);
  listening = false;
}

/** Installs the front check (one at a time); returns its removal. */
export function setFrontCheck(check: FrontCheck): () => void {
  frontCheck = check;
  listen();
  return () => {
    if (frontCheck === check) frontCheck = null;
    stopIfUnused();
  };
}

/** Whether the app is in front, from `document.visibilityState` (and the front check, on a return). */
export function useAppActive(): AppActivity {
  const [activity, setActivity] = useState<AppActivity>(() => ({
    active: visible() && returning === null,
    inactiveSince: visible() && returning === null ? null : Date.now(),
  }));

  useEffect(() => {
    let settle: ReturnType<typeof setTimeout> | undefined;
    let live = true;
    let moves = 0;
    const onChange: Subscriber = (front, checked) => {
      clearTimeout(settle);
      const move = ++moves;
      if (!front) {
        setActivity((a) => (a.active ? { active: false, inactiveSince: Date.now() } : a));
        return;
      }
      const resume = () => {
        // Hidden again (or gone) while the check ran: that move decides.
        if (!live || move !== moves) return;
        setActivity((a) => ({ active: true, inactiveSince: a.inactiveSince }));
        settle = setTimeout(() => setActivity({ active: true, inactiveSince: null }), SETTLE_MS);
      };
      if (checked) void checked.then(resume);
      else resume();
    };
    subscribers.add(onChange);
    listen();
    // Mounted while a return is being checked: wait for it like the others.
    if (returning) onChange(true, returning);
    return () => {
      live = false;
      clearTimeout(settle);
      subscribers.delete(onChange);
      stopIfUnused();
    };
  }, []);

  return activity;
}
