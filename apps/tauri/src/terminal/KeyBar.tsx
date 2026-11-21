import { useEffect, useRef, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import { nextKeyBarMode, useKeyBarMode, type KeyBarMode } from "./keyBarPref";
import { COMPACT_ROW, KEY_ROWS, NO_MODS, consumeOnce, keyBytes, nextMods, type BarKey, type Mods } from "./keyMap";

/** Arrow keys repeat while held: the first repeat after this long, then one per `REPEAT_MS`. */
export const REPEAT_DELAY_MS = 400;
export const REPEAT_MS = 60;

const REPEATS = new Set<BarKey>(["left", "up", "down", "right"]);

/** What each key shows, and its accessible name where that differs. */
const FACES: Record<BarKey, { text: string; name?: string }> = {
  esc: { text: "esc", name: "Esc" },
  ctrl: { text: "ctrl", name: "Ctrl" },
  alt: { text: "alt", name: "Alt" },
  tab: { text: "tab", name: "Tab" },
  "|": { text: "|" },
  "~": { text: "~" },
  "/": { text: "/" },
  "-": { text: "-" },
  snippets: { text: "{ }", name: "Snippets" },
  home: { text: "home", name: "Home" },
  end: { text: "end", name: "End" },
  pgup: { text: "pgup", name: "Page Up" },
  pgdn: { text: "pgdn", name: "Page Down" },
  left: { text: "←", name: "Left" },
  up: { text: "↑", name: "Up" },
  down: { text: "↓", name: "Down" },
  right: { text: "→", name: "Right" },
};

interface KeyBarProps {
  /** Bytes for the session, modifiers already applied. */
  onSend(bytes: string): void;
  onSnippets(): void;
  /** The hide-keyboard key: the terminal gives up the focus, so the software keyboard (and this bar) go away. */
  onHideKeyboard(): void;
  /** xterm's application cursor keys mode (`modes.applicationCursorKeysMode`). */
  appCursor: boolean;
  /** The sticky modifiers; held by the caller, which also applies them to typed keys. */
  mods: Mods;
  onModsChange(mods: Mods): void;
}

/** What the size toggle is called in each mode: what a press does next. */
const TOGGLE_NAME: Record<Exclude<KeyBarMode, "hidden">, string> = { two: "One row", one: "Hide key bar" };

/** Chevrons closing in: the bar gets smaller. */
const SHRINK_ICON = (
  <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true" focusable="false">
    <path d="M4 2.5 8 6l4-3.5M4 13.5 8 10l4 3.5" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);
/** Chevrons opening out: the bar comes back. */
const GROW_ICON = (
  <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false">
    <path d="M4 6 8 2.5 12 6M4 10l4 3.5 4-3.5" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

/**
 * Terminal keys above the software keyboard (iOS): two rows, one row of the
 * everyday keys, or hidden behind a small pill (the size toggle steps
 * through these; the choice is kept per device). Keys act on press, like a
 * keyboard, and never take the focus: the terminal keeps it, so the
 * software keyboard stays up.
 */
export function KeyBar({ onSend, onSnippets, onHideKeyboard, appCursor, mods, onModsChange }: KeyBarProps) {
  const [mode, setMode] = useKeyBarMode();
  const repeat = useRef<{ stop(): void } | null>(null);
  const stopRepeat = () => {
    repeat.current?.stop();
    repeat.current = null;
  };
  useEffect(() => stopRepeat, []);

  const press = (key: BarKey) => {
    if (key === "ctrl" || key === "alt") {
      onModsChange(nextMods(mods, key));
      return;
    }
    if (key === "snippets") {
      onSnippets();
      return;
    }
    const bytes = keyBytes(key, mods, appCursor);
    onSend(bytes);
    onModsChange(consumeOnce(mods));
    if (!REPEATS.has(key)) return;
    // The modifiers the press had apply to every repeat.
    stopRepeat();
    let interval: ReturnType<typeof setInterval> | undefined;
    const delay = setTimeout(() => {
      onSend(bytes);
      interval = setInterval(() => onSend(bytes), REPEAT_MS);
    }, REPEAT_DELAY_MS);
    repeat.current = {
      stop: () => {
        clearTimeout(delay);
        clearInterval(interval);
      },
    };
  };

  const onPointerDown = (key: BarKey) => (e: ReactPointerEvent<HTMLButtonElement>) => {
    // Keeps the focus (and the software keyboard) on the terminal.
    e.preventDefault();
    if (e.button !== 0) return;
    e.currentTarget.setPointerCapture?.(e.pointerId);
    // Pressed look at once: iOS shows :active late, or never, once touchstart is cancelled.
    e.currentTarget.setAttribute("data-down", "");
    press(key);
  };
  const onPointerEnd = (e: ReactPointerEvent<HTMLButtonElement>) => {
    e.currentTarget.removeAttribute("data-down");
    stopRepeat();
  };

  // iOS moves the focus on the click that follows a touch; a cancelled touchstart has none.
  // React's touch listeners are passive, hence the native one (on the bar or, hidden, the pill's strip).
  const bar = useRef<HTMLDivElement>(null);
  const hidden = mode === "hidden";
  useEffect(() => {
    const el = bar.current;
    if (!el) return;
    const cancel = (e: TouchEvent) => e.preventDefault();
    el.addEventListener("touchstart", cancel, { passive: false });
    return () => el.removeEventListener("touchstart", cancel);
  }, [hidden]);

  const resize = () => {
    stopRepeat();
    const next = nextKeyBarMode(mode);
    // No modifier stays on out of sight.
    if (next === "hidden" && (mods.ctrl !== "off" || mods.alt !== "off")) onModsChange(NO_MODS);
    setMode(next);
  };

  /** A bar button that is not a terminal key: acts on press, keeps the focus, works from assistive tech. */
  const action = (name: string, face: ReactNode, act: () => void, className: string) => (
    <button
      key={name}
      type="button"
      tabIndex={-1}
      className={className}
      aria-label={name}
      title={name}
      onPointerDown={(e) => {
        e.preventDefault();
        if (e.button === 0) act();
      }}
      onMouseDown={(e) => e.preventDefault()}
      onClick={(e) => {
        if (e.detail === 0) act();
      }}
    >
      {face}
    </button>
  );

  if (hidden) {
    return (
      <div ref={bar} className="key-bar-peek">
        {action(
          "Show key bar",
          <span className="key-bar-pill">
            {GROW_ICON}
            <span aria-hidden="true">keys</span>
          </span>,
          resize,
          "key-bar-show",
        )}
      </div>
    );
  }

  const toggle = action(TOGGLE_NAME[mode], SHRINK_ICON, resize, "key key-action key-size");
  const hideKeyboard = action("Hide keyboard", "⌄", onHideKeyboard, "key key-action key-hide");
  const rows: { keys: readonly BarKey[]; end: ReactNode[] }[] =
    mode === "two"
      ? [
          { keys: KEY_ROWS[0], end: [toggle] },
          { keys: KEY_ROWS[1], end: [hideKeyboard] },
        ]
      : [{ keys: COMPACT_ROW, end: [toggle, hideKeyboard] }];

  return (
    <div ref={bar} className="key-bar" data-mode={mode} role="toolbar" aria-label="Terminal keys">
      {rows.map(({ keys: row, end }, i) => (
        <div key={i} className="key-row">
          {row.map((key) => {
            const face = FACES[key];
            const mod = key === "ctrl" || key === "alt" ? mods[key] : undefined;
            return (
              <button
                key={key}
                type="button"
                tabIndex={-1}
                className="key"
                data-key={key}
                data-mod={mod}
                aria-label={face.name}
                aria-pressed={mod === undefined ? undefined : mod !== "off"}
                onPointerDown={onPointerDown(key)}
                onPointerUp={onPointerEnd}
                onPointerCancel={onPointerEnd}
                onLostPointerCapture={onPointerEnd}
                onMouseDown={(e) => e.preventDefault()}
                // Assistive tech activates with a click and no press before it (detail 0).
                onClick={(e) => {
                  if (e.detail === 0) {
                    press(key);
                    stopRepeat();
                  }
                }}
              >
                {face.text}
              </button>
            );
          })}
          {end}
        </div>
      ))}
    </div>
  );
}
