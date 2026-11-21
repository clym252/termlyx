import { useEffect, useRef, useState } from "react";
import { api as appApi } from "../api";
import type { TetherApi } from "../api/types";

/** How often an open session is asked which tmux session it is in. */
export const TMUX_POLL_MS = 15_000;

/**
 * The tmux session the pane's shell was last known to be in, for reattaching
 * after a drop (`null`: none known). Asks `sessionTmux` every
 * `TMUX_POLL_MS` while `sessionId` is open and the app `active`, and once as
 * the page goes to the background (a suspended app's connection may never
 * come back). A failed ask keeps the last answer, and so does a drop
 * (`sessionId` null): the memory outlives the session it came from, until the
 * next session answers. While the vault is `locked` it never asks (nobody
 * can type into the pane) and keeps what it knew.
 */
export function useTmuxMemory(
  sessionId: string | null,
  active: boolean,
  locked: boolean,
  api: Pick<TetherApi, "sessionTmux"> = appApi,
): string | null {
  const [remembered, setRemembered] = useState<string | null>(null);
  const apiRef = useRef(api);
  apiRef.current = api;
  // The ask of the current session, so the timer and the page hiding never overlap.
  const asking = useRef(false);
  const lockedRef = useRef(locked);
  lockedRef.current = locked;
  const ask = useRef<() => void>(() => {});

  useEffect(() => {
    if (!sessionId) return;
    let live = true;
    asking.current = false;
    ask.current = () => {
      if (asking.current) return;
      asking.current = true;
      apiRef.current
        .sessionTmux(sessionId)
        .then(
          (name) => {
            if (live) setRemembered(name);
          },
          (e: unknown) => console.debug("tmux session not known now", e),
        )
        .finally(() => {
          if (live) asking.current = false;
        });
    };
    const onHide = () => {
      if (document.visibilityState === "hidden" && !lockedRef.current) ask.current();
    };
    document.addEventListener("visibilitychange", onHide);
    return () => {
      live = false;
      ask.current = () => {};
      document.removeEventListener("visibilitychange", onHide);
    };
  }, [sessionId]);

  useEffect(() => {
    if (!sessionId || !active || locked) return;
    const id = setInterval(() => ask.current(), TMUX_POLL_MS);
    return () => clearInterval(id);
  }, [sessionId, active, locked]);

  return remembered;
}
