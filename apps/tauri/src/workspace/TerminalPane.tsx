import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import { api } from "../api";
import { IS_ANDROID, IS_MOBILE } from "../platform";
import { useHostKeyPending } from "../prompts/pending";
import { useTerminalSettings } from "../settings/store";
import { attachHighlights } from "../terminal/highlight";
import { KeyBar } from "../terminal/KeyBar";
import { sendSnippet, snippetTextOf } from "../snippets/send";
import { SnippetEditor } from "../snippets/SnippetEditor";
import { inMenu, RowMenu, type MenuEntry } from "../ui/RowMenu";
import { openSidePanel } from "./sidePanelPref";
import { TouchSelection } from "../terminal/TouchSelection";
import { NO_MODS, applyTyped, type Mods } from "../terminal/keyMap";
import { useShell, type ShellTerm } from "../terminal/useShell";
import { useSoftKeyboard } from "../terminal/useSoftKeyboard";
import { attachImeInput } from "../terminal/imeInput";
import { attachTouchScroll, terminalSink } from "../terminal/touchScroll";
import { findTheme, getTheme, type Theme } from "../themes";
import type { PaneSpec, TargetedPane } from "./model";
import { PaneHeader, type PaneHeaderControls } from "./PaneHeader";
import { usePaneDrop } from "./paneDrag";
import { paneStatus, type PaneStatus } from "./PhoneTerminals";
import { SessionStatus } from "./SessionStatus";
import type { PaneControls } from "./Workspace";
import { bracketedPasteText } from "./broadcast";
import { attachSafePaste, safePasteText } from "./safePaste";
import { terminalKeyHandler } from "./clipboardKeys";
import { useAppActive } from "./useAppActive";
import { useHostHeader } from "./useHostHeader";
import { useReconnect } from "./useReconnect";
import { userInputGate } from "./userInput";
import { useTmuxMemory } from "./useTmuxMemory";

/** The mock-backed test build uses xterm's DOM renderer (no GPU, readable rows). */
// shortcut: DOM renderer on Android (WebGL garbled on the emulator GPU); revisit if the friend's phone feels slow
const USE_WEBGL = import.meta.env.VITE_TETHER_API !== "mock" && !IS_ANDROID;

/**
 * Attributes for xterm's hidden textarea: nothing on the software keyboard
 * may rewrite what is typed (xterm sets the first three itself; kept here so
 * they stay set).
 */
const TEXTAREA_ATTRS = { autocorrect: "off", autocapitalize: "off", spellcheck: "false", autocomplete: "off" };


/** Terminal look a host's effective settings ask for (colour scheme naming a theme, font). */
interface HostLook {
  theme?: Theme;
  fontFamily?: string;
  fontSize?: number;
}

export function useHostLook(pane: Pick<PaneSpec, "target"> | null): HostLook {
  const [look, setLook] = useState<HostLook>({});
  const hostId = pane?.target?.kind === "host" ? pane.target.hostId : null;
  useEffect(() => {
    if (!hostId) {
      setLook({});
      return;
    }
    let live = true;
    api.effectiveSettings(hostId).then(
      (s) => {
        if (!live) return;
        setLook({
          theme: findTheme(s.colorScheme.value),
          fontFamily: s.fontFamily.value?.trim() || undefined,
          fontSize: s.fontSize.value ?? undefined,
        });
      },
      (e: unknown) => console.debug("effective settings unavailable", e),
    );
    return () => {
      live = false;
    };
  }, [hostId]);
  return look;
}

interface TerminalPaneProps {
  pane: TargetedPane;
  /** The pane's stable DOM home, moved between layout slots (see Workspace). */
  host: HTMLElement;
  active: boolean;
  /** The pane's name on screen ("web (2)"). */
  label: string;
  /** The pane shares its tab with others: it gets a header and focus styling. */
  inSplit: boolean;
  /** Whether the pane's tab is on screen. */
  visible: boolean;
  /** The vault is locked: a dropped session waits for the user instead of reconnecting. */
  locked: boolean;
  onActivate: () => void;
  onClose: () => void;
  /** The header's menu and maximize state (shown with `inSplit`). */
  header: PaneHeaderControls;
  /** The tab broadcasts typing: set to the number of panes it goes to (the pane is outlined and says so). */
  broadcast: { count: number } | null;
  /** What was typed here while broadcasting, to be sent to the tab's other panes too (only called while `active`). */
  onBroadcast: (data: string) => void;
  /** Where the session stands, on every change (`null` as the pane goes). */
  onStatus?: (status: PaneStatus | null) => void;
  /** The pane's menu actions, once mounted (`null` as the pane goes). */
  onControls?: (controls: PaneControls | null) => void;
}

/**
 * One terminal: an xterm bound to a session on the pane's target, with the
 * session's status over it. Rendered into `host` through a portal so that
 * re-arranging splits never remounts it (which would end the session).
 */
export function TerminalPane({
  pane,
  host,
  active,
  label,
  inSplit,
  visible,
  locked,
  onActivate,
  onClose,
  header,
  broadcast,
  onBroadcast,
  onStatus,
  onControls,
}: TerminalPaneProps) {
  const termEl = useRef<HTMLDivElement>(null);
  const stageEl = useRef<HTMLDivElement>(null);
  const [term, setTerm] = useState<Terminal | null>(null);
  const fitRef = useRef<() => void>(() => {});
  const [attempt, setAttempt] = useState(0);
  const settings = useTerminalSettings();
  const look = useHostLook(pane);
  const theme = look.theme ?? getTheme(settings.themeId);
  const fontFamily = look.fontFamily ?? settings.fontFamily;
  const fontSize = look.fontSize ?? settings.fontSize;

  useEffect(() => {
    const el = termEl.current;
    if (!el) return;
    // Text too close to its background is lifted to 4.5:1 in every theme (the readability floor).
    // Decorations (highlight.ts) are xterm "proposed" API.
    const t = new Terminal({
      cursorBlink: true,
      fontFamily,
      fontSize,
      minimumContrastRatio: 4.5,
      allowProposedApi: true,
      theme: { ...theme.terminal },
    });
    const fit = new FitAddon();
    t.loadAddon(fit);
    t.open(el);
    // Keyboard and menu pastes go through the sanitizer too, not xterm's own paste-event handler.
    const detachPaste = attachSafePaste(el, t);
    for (const [name, value] of Object.entries(TEXTAREA_ATTRS)) t.textarea?.setAttribute(name, value);
    // The palette and workspace shortcuts are not terminal input (the window handler acts on them); copy/paste keys.
    t.attachCustomKeyEventHandler(terminalKeyHandler(t));

    if (USE_WEBGL) {
      // GPU renderer when available; the DOM renderer is the fallback on error or context loss.
      try {
        const webgl = new WebglAddon();
        webgl.onContextLoss(() => webgl.dispose());
        t.loadAddon(webgl);
      } catch (err) {
        console.warn("WebGL renderer unavailable, using DOM renderer", err);
      }
    }

    // A hidden pane (another tab, the hosts page) has no size: fit it when it shows.
    const safeFit = () => {
      if (el.offsetWidth > 0 && el.offsetHeight > 0) fit.fit();
    };
    fitRef.current = safeFit;
    safeFit();
    // fit() changes cols/rows, which fires onResize -> resize (useShell). Debounced so a
    // drag does not flood the session with intermediate sizes.
    let timer: ReturnType<typeof setTimeout> | undefined;
    const observer = new ResizeObserver(() => {
      clearTimeout(timer);
      timer = setTimeout(safeFit, 50);
    });
    observer.observe(el);
    setTerm(t);
    return () => {
      clearTimeout(timer);
      observer.disconnect();
      detachPaste();
      fitRef.current = () => {};
      setTerm(null);
      t.dispose();
    };
    // Created once; options are updated below.
  }, []);

  // iOS: a finger drag scrolls (xterm itself scrolls with the wheel only).
  useEffect(() => {
    const el = termEl.current;
    if (!IS_MOBILE || !term || !el) return;
    return attachTouchScroll(el, terminalSink(term));
  }, [term]);

  // Android: each letter goes out as the software keyboard composes it, not a whole word at its end.
  useEffect(() => {
    const el = termEl.current;
    if (!IS_ANDROID || !term?.textarea || !el) return;
    return attachImeInput(el, term.textarea, (data) => term.input(data, true));
  }, [term]);

  // IP addresses in the theme's highlight colour (display only); redrawn on a theme change.
  useEffect(() => {
    if (!term || !settings.highlightIps) return;
    const h = attachHighlights(term, theme);
    return () => h.dispose();
  }, [term, theme, settings.highlightIps]);

  useEffect(() => {
    if (!term) return;
    term.options.theme = { ...theme.terminal };
    term.options.fontFamily = fontFamily;
    term.options.fontSize = fontSize;
    fitRef.current();
  }, [term, theme, fontFamily, fontSize]);

  // xterm measures cells once: re-measure when the bundled face finishes loading.
  useEffect(() => {
    if (!term) return;
    let live = true;
    void document.fonts?.load('14px "JetBrains Mono"').then(() => {
      if (!live) return;
      const f = term.options.fontFamily ?? "monospace";
      term.options.fontFamily = "monospace";
      term.options.fontFamily = f;
      fitRef.current();
    });
    return () => {
      live = false;
    };
  }, [term]);

  useEffect(() => {
    if (term && active && visible) {
      fitRef.current();
      term.focus();
    }
  }, [term, active, visible]);

  // A host tab that was in a tmux session reattaches to it on a reconnect (set with each new attempt).
  const [reattach, setReattach] = useState<string | null>(null);
  const target = useMemo(
    () => (pane.target.kind === "host" && reattach ? { ...pane.target, reattachTmux: reattach } : pane.target),
    [pane.target, reattach],
  );
  // Retrying that has stopped abandons the session under way (Cancel mid-attempt, the vault locking).
  const [halted, setHalted] = useState(false);
  const [sessionId, setSessionId] = useState<string | null>(null);
  // iOS: the key bar's sticky ctrl/alt also apply to the next key typed on the software keyboard.
  const [mods, setModsState] = useState<Mods>(NO_MODS);
  const modsRef = useRef<Mods>(NO_MODS);
  const setMods = (m: Mods) => {
    modsRef.current = m;
    setModsState(m);
  };
  // Set while the key bar's own bytes go through xterm (their modifiers are already applied).
  const fromBar = useRef(false);
  // iOS: where broadcast taps what the session is given (after the sticky modifiers), while this pane broadcasts.
  const tap = useRef<((data: string) => void) | null>(null);
  const shellTerm = useMemo<ShellTerm | null>(() => {
    if (!term || !IS_MOBILE) return term;
    return {
      get cols() {
        return term.cols;
      },
      get rows() {
        return term.rows;
      },
      write: (data, callback) => term.write(data, callback),
      onResize: (listener) => term.onResize(listener),
      onData: (listener) =>
        term.onData((data) => {
          if (fromBar.current) {
            listener(data);
            tap.current?.(data);
            return;
          }
          const typed = applyTyped(data, modsRef.current, term.modes.applicationCursorKeysMode);
          if (typed.mods !== modsRef.current) setMods(typed.mods);
          listener(typed.bytes);
          tap.current?.(typed.bytes);
        }),
    };
  }, [term]);
  // Typing, paste, the key bar and snippets all come out of `onData`, along with the terminal's own replies to
  // a remote program's queries and its mouse reports; only the user's input is broadcast (see userInputGate).
  // Only the focused pane of a broadcasting tab forwards it; the others get it through `inject`, which is not
  // a typing event they re-send.
  const sendBroadcast = useRef(onBroadcast);
  sendBroadcast.current = onBroadcast;
  const broadcasting = broadcast !== null && active;
  useEffect(() => {
    if (!term || !broadcasting) return;
    const gate = userInputGate(term);
    if (!gate) return;
    const send = (data: string) => {
      if (gate.take(data)) sendBroadcast.current(data);
    };
    if (IS_MOBILE) {
      // `shellTerm` wraps onData there: tap its output so the modifiers are applied once, here.
      tap.current = send;
      return () => {
        tap.current = null;
        gate.dispose();
      };
    }
    const sub = term.onData(send);
    return () => {
      sub.dispose();
      gate.dispose();
    };
  }, [term, broadcasting]);
  const state = useShell(shellTerm, halted ? null : target, { attempt, onOpen: setSessionId });
  const { active: appActive, inactiveSince } = useAppActive();
  const tracked = pane.target.kind === "host" && state.status === "open" ? sessionId : null;
  const remembered = useTmuxMemory(tracked, appActive, locked);
  const newAttempt = (tmux: string | null) => {
    term?.write("\r\n\x1b[2m[reconnecting]\x1b[0m\r\n");
    setReattach(tmux);
    setAttempt((n) => n + 1);
  };
  const { reconnect, now, reconnectNow, cancel } = useReconnect({
    state,
    active: appActive,
    inactiveSince,
    locked,
    onAttempt: () => {
      newAttempt(remembered);
      setHalted(false);
    },
    onStop: () => setHalted(true),
  });
  const hostInfo = useHostHeader(pane);
  const waitingForYou = useHostKeyPending(hostInfo?.host ?? null, hostInfo?.port ?? 0);

  // After a Retry/Reconnect (its button had the focus) the terminal takes the keys again once it is open.
  const open = state.status === "open";
  useEffect(() => {
    if (term && open && active && visible) term.focus();
  }, [term, open, active, visible]);

  // Retry (a first connect that failed) reopens at once, without tmux; a dropped session goes through the reconnect schedule.
  const retry = () => (reconnect.kind === "idle" ? newAttempt(null) : reconnectNow());

  // The key bar: iOS, a software keyboard up, and this terminal taking its keys.
  const keyboard = useSoftKeyboard();
  const [termFocused, setTermFocused] = useState(false);
  useEffect(() => {
    const ta = term?.textarea;
    if (!ta) return;
    const sync = () => setTermFocused(document.activeElement === ta);
    sync();
    ta.addEventListener("focus", sync);
    ta.addEventListener("blur", sync);
    return () => {
      ta.removeEventListener("focus", sync);
      ta.removeEventListener("blur", sync);
    };
  }, [term]);
  const showKeyBar = IS_MOBILE && keyboard.open && active && visible && termFocused && !!term;
  // A modifier left on does not outlive the keyboard.
  useEffect(() => {
    if (!showKeyBar && modsRef.current !== NO_MODS) setMods(NO_MODS);
  }, [showKeyBar]);
  // Arrows follow the application cursor mode, which the session's output switches.
  const [appCursor, setAppCursor] = useState(false);
  useEffect(() => {
    if (!term || !showKeyBar) return;
    const sync = () => setAppCursor(term.modes.applicationCursorKeysMode);
    sync();
    const sub = term.onWriteParsed(sync);
    return () => sub.dispose();
  }, [term, showKeyBar]);
  const sendFromBar = (bytes: string) => {
    if (!term) return;
    fromBar.current = true;
    try {
      term.input(bytes, true);
    } finally {
      fromBar.current = false;
    }
  };
  // iOS Paste (the long-press menu, the session menu): the clipboard as one paste, bracketed when the
  // app asked for it, and without the key bar's sticky modifiers. The text goes straight to the terminal.
  const paste = () => {
    void api.readClipboard().then((text) => {
      if (!text || !term) return;
      fromBar.current = true;
      try {
        term.paste(safePasteText(text));
      } finally {
        fromBar.current = false;
      }
    });
  };
  const copy = (text: string) => {
    void api.writeClipboard(text);
  };
  // Save as snippet: the editor, its command filled from the selection.
  const [saving, setSaving] = useState<string | null>(null);
  const saveSnippet = (selection: string) => setSaving(snippetTextOf(selection));
  // Desktop: right-click on the terminal is Copy, Paste and, with a selection, Save as snippet.
  const [textMenu, setTextMenu] = useState<{ x: number; y: number; selection: string } | null>(null);
  const textMenuItems = (sel: string): MenuEntry[] => [
    { label: "Copy", run: () => copy(sel), disabled: !sel },
    ["Paste", paste],
    ...(sel ? [["Save as snippet", () => saveSnippet(sel)] as MenuEntry] : []),
  ];

  // What the phone layout's top bar shows and asks of this pane.
  const status = paneStatus(state, reconnect);
  const report = useRef(onStatus);
  report.current = onStatus;
  useEffect(() => {
    report.current?.(status);
  }, [status]);
  useEffect(() => () => report.current?.(null), []);
  const menu = useRef<PaneControls>({ reconnect: () => {}, sendSnippet: () => {}, paste: () => {}, inject: () => {}, focus: () => {} });
  menu.current = {
    // A dropped session goes through the reconnect schedule (at once); any other starts over, back in its tmux session.
    reconnect: () => {
      if (reconnect.kind === "idle") newAttempt(remembered);
      else if (reconnect.kind !== "trying") reconnectNow();
    },
    sendSnippet: (snippet) => term && sendSnippet(term, snippet),
    focus: () => term?.focus(),
    paste,
    // Another pane's broadcast: as typed here, but its modifiers were applied there (as for the key bar).
    // A paste arrives wrapped for the source's mode: paste its text, wrapped (or not) for this one's.
    inject: (data) => {
      if (!term) return;
      fromBar.current = true;
      try {
        const pasted = bracketedPasteText(data);
        if (pasted !== null) term.paste(safePasteText(pasted));
        else term.input(data, true);
      } finally {
        fromBar.current = false;
      }
    },
  };
  const controls = useRef(onControls);
  controls.current = onControls;
  useEffect(() => {
    controls.current?.({
      reconnect: () => menu.current.reconnect(),
      sendSnippet: (snippet) => menu.current.sendSnippet(snippet),
      paste: () => menu.current.paste(),
      inject: (data) => menu.current.inject(data),
      focus: () => menu.current.focus(),
    });
    return () => controls.current?.(null);
  }, []);

  const style: CSSProperties = { background: theme.terminal.background };
  const drop = usePaneDrop(pane.id, header.onSwap);
  return createPortal(
    <div
      className="terminal-pane"
      data-pane-id={pane.id}
      data-active={active ? "true" : "false"}
      data-split={inSplit ? "true" : "false"}
      data-broadcast={broadcast ? "true" : undefined}
      data-terminal-theme={theme.id}
      data-font-size={fontSize}
      data-status={state.status}
      style={style}
      onMouseDownCapture={onActivate}
      onFocusCapture={onActivate}
      {...drop}
    >
      {inSplit && <PaneHeader paneId={pane.id} label={label} active={active} broadcast={broadcast} onActivate={onActivate} {...header} />}
      {/* The terminal and its session card: the card stays above the key bar (and so above the software keyboard). */}
      <div className="terminal-stage" ref={stageEl}>
        <div
          className="terminal-host"
          data-touch-scroll={IS_MOBILE ? "true" : undefined}
          ref={termEl}
          onContextMenu={
            IS_MOBILE
              ? undefined
              : (e) => {
                  e.preventDefault();
                  if (!inMenu(e.target)) setTextMenu({ x: e.clientX, y: e.clientY, selection: term?.getSelection() ?? "" });
                }
          }
        />
        {textMenu && (
          <RowMenu
            owner={`terminal-text-${pane.id}`}
            label="Terminal text"
            anchor={textMenu}
            items={textMenuItems(textMenu.selection)}
            onClose={() => {
              setTextMenu(null);
              term?.focus();
            }}
          />
        )}
        {IS_MOBILE && term && termEl.current && stageEl.current && (
          <TouchSelection
            term={term}
            host={termEl.current}
            stage={stageEl.current}
            onPaste={paste}
            onCopy={copy}
            onSaveSnippet={saveSnippet}
          />
        )}
        <SessionStatus
          state={state}
          header={hostInfo?.header ?? null}
          reconnect={reconnect}
          now={now}
          takeFocus={active && visible}
          waitingForYou={waitingForYou}
          onReconnect={retry}
          onStopReconnect={cancel}
          onCancel={onClose}
          onClose={onClose}
        />
      </div>
      {saving !== null && (
        <SnippetEditor
          initial={{ text: saving }}
          onClose={() => {
            setSaving(null);
            term?.focus();
          }}
        />
      )}
      {showKeyBar && (
        <KeyBar
          onSend={sendFromBar}
          onSnippets={() => openSidePanel("snippets")}
          onHideKeyboard={() => term?.blur()}
          appCursor={appCursor}
          mods={mods}
          onModsChange={setMods}
        />
      )}
    </div>,
    host,
  );
}
