import type { ReactNode } from "react";
import type { RouteId } from "./routes";

/** What the sidebar draws an icon for: every route, plus the controls that are not routes. */
export type NavIconName = RouteId | "terminals" | "search" | "lock" | "collapse" | "expand" | "layout" | "sidePanel";

/*
 * Stroke icons drawn for Tether on a 24-unit grid: round caps and joins,
 * `currentColor`, so they follow the item's colour (muted, accent when current).
 * A Record keyed by route id, so a new route cannot ship without one.
 */
const PATHS: Record<NavIconName, ReactNode> = {
  // A rack unit: two stacked boxes with a status light each.
  hosts: (
    <>
      <rect x="4" y="4" width="16" height="6.5" rx="1.5" />
      <rect x="4" y="13.5" width="16" height="6.5" rx="1.5" />
      <path d="M7.5 7.25h.01M7.5 16.75h.01" />
    </>
  ),
  // Curly braces around a dot: a saved bit of text to drop in.
  snippets: (
    <>
      <path d="M9 4.5H8a2 2 0 0 0-2 2v3a2.5 2.5 0 0 1-2 2.5 2.5 2.5 0 0 1 2 2.5v3a2 2 0 0 0 2 2h1" />
      <path d="M15 4.5h1a2 2 0 0 1 2 2v3a2.5 2.5 0 0 0 2 2.5 2.5 2.5 0 0 0-2 2.5v3a2 2 0 0 1-2 2h-1" />
      <path d="M12 12h.01" />
    </>
  ),
  // A key: round bow, shaft, two teeth.
  keychain: (
    <>
      <circle cx="8" cy="15" r="3.5" />
      <path d="M10.5 12.5 19 4M16 7l2.5 2.5M13.5 9.5l2 2" />
    </>
  ),
  // A path from a source dot that bends and ends in an arrow.
  forwarding: (
    <>
      <circle cx="5.5" cy="6" r="1.75" />
      <path d="M5.5 8v4.5a3 3 0 0 0 3 3H19M16 12l3 3.5-3 3.5" />
    </>
  ),
  // A shield with a tick: servers you have already vouched for.
  knownHosts: (
    <>
      <path d="M12 3.5 5 6v5.5c0 4.2 2.9 7.2 7 9 4.1-1.8 7-4.8 7-9V6z" />
      <path d="m9 12 2.2 2.2L15.5 10" />
    </>
  ),
  // A folder with an arrow lifting out of it.
  sftp: (
    <>
      <path d="M3.5 7.5v-1a1.5 1.5 0 0 1 1.5-1.5h4l2 2.5H19a1.5 1.5 0 0 1 1.5 1.5v9A1.5 1.5 0 0 1 19 19.5H5A1.5 1.5 0 0 1 3.5 18z" />
      <path d="M12 16v-5M9.75 13.25 12 11l2.25 2.25" />
    </>
  ),
  // Three sliders.
  settings: (
    <>
      <path d="M4 7h7M15 7h5M4 12h3M11 12h9M4 17h9M17 17h3" />
      <circle cx="13" cy="7" r="2" />
      <circle cx="9" cy="12" r="2" />
      <circle cx="15" cy="17" r="2" />
    </>
  ),
  // One line forking into two: two versions of a record.
  conflicts: (
    <>
      <path d="M12 20v-6M12 14 6.5 8M12 14l5.5-6" />
      <path d="M6.5 11V8h3M17.5 11V8h-3" />
    </>
  ),
  // A prompt: caret and cursor line.
  devTerminal: <path d="m6 8 4 4-4 4M12.5 16.5H18" />,
  // A window with a prompt in it.
  terminals: (
    <>
      <rect x="3.5" y="4.5" width="17" height="15" rx="2" />
      <path d="m7.5 10 3 2.5-3 2.5M13 15h3.5" />
    </>
  ),
  search: (
    <>
      <circle cx="10.5" cy="10.5" r="5.5" />
      <path d="m15 15 5 5" />
    </>
  ),
  // A padlock.
  lock: (
    <>
      <rect x="5" y="11" width="14" height="9" rx="2" />
      <path d="M8 11V8a4 4 0 0 1 8 0v3M12 14.5v2" />
    </>
  ),
  // A frame cut into three panes: one tall, two stacked.
  layout: (
    <>
      <rect x="3.5" y="4.5" width="17" height="15" rx="2" />
      <path d="M11 4.5v15M11 12h9.5" />
    </>
  ),
  // A window with a column on its right: the side panel.
  sidePanel: (
    <>
      <rect x="3.5" y="4.5" width="17" height="15" rx="2" />
      <path d="M14.5 4.5v15M16.75 8.5h1.5M16.75 11.5h1.5" />
    </>
  ),
  collapse: <path d="m14.5 6-6 6 6 6" />,
  expand: <path d="m9.5 6 6 6-6 6" />,
};

/** A decorative icon: the button around it carries the name. */
export function NavIcon({ name }: { name: NavIconName }) {
  return (
    <svg
      className="nav-icon"
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {PATHS[name]}
    </svg>
  );
}

/** The tab bar's Layout button icon. */
export const LayoutIcon = () => <NavIcon name="layout" />;
