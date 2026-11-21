import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import type { SessionTarget, Snippet } from "../api/types";
import {
  EMPTY_WORKSPACE,
  activeTab,
  effectiveZoom,
  hasTarget,
  paneLabel,
  panesOf,
  reduce,
  stableOrder,
  tabTitle,
  type Orientation,
  type Preset,
  type Tab,
  type WorkspaceState,
} from "./model";
import { broadcastTargets } from "./broadcast";
import { focusPane } from "./focusPane";
import { Launcher, LauncherPane } from "./LauncherPane";
import { layoutMenu, type LayoutMenuActions } from "./layoutMenu";
import { LOCAL_TITLE, localTarget } from "./local";
import type { PaneStatus } from "./PhoneTerminals";
import { SHORTCUT_LABELS, matchShortcut, type WorkspaceCommand } from "./shortcuts";
import { SidePanel } from "./SidePanel";
import { getSidePanel, setSidePanel, useSidePanel } from "./sidePanelPref";
import { SplitView } from "./SplitView";
import { Tabs, tabButtonId, tabPanelId } from "./Tabs";
import { TerminalPane } from "./TerminalPane";

let counter = 0;
function uid(): string {
  counter += 1;
  return typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : `id-${Date.now()}-${counter}`;
}

/** What the phone layout's session menu asks of a pane. */
export interface PaneControls {
  /** A new session on the pane's target (reattaching its tmux session), whatever the current one is doing. */
  reconnect(): void;
  /** A snippet into the session, as a paste (and Enter if it runs). */
  sendSnippet(snippet: Snippet): void;
  /** The clipboard's text into the session, as a paste. */
  paste(): void;
  /** Text into the session as if typed here: another pane's broadcast. */
  inject(data: string): void;
  /** Keyboard focus into the terminal. */
  focus(): void;
}

/**
 * The workspace state and its operations (ids are made here, so the reducer
 * stays pure), and what the panes report for the phone layout: each one's
 * status and its menu actions.
 */
export function useWorkspace() {
  const [state, dispatch] = useReducer(reduce, EMPTY_WORKSPACE);
  const [statuses, setStatuses] = useState<ReadonlyMap<string, PaneStatus>>(new Map());
  const controls = useRef(new Map<string, PaneControls>());
  const actions = useMemo(
    () => ({
      reportStatus: (paneId: string, status: PaneStatus | null) =>
        setStatuses((prev) => {
          if ((prev.get(paneId) ?? null) === status) return prev;
          const next = new Map(prev);
          if (status) next.set(paneId, status);
          else next.delete(paneId);
          return next;
        }),
      setPaneControls: (paneId: string, c: PaneControls | null) => {
        if (c) controls.current.set(paneId, c);
        else controls.current.delete(paneId);
      },
      paneControls: (paneId: string): PaneControls | undefined => controls.current.get(paneId),
      openTab: (target: SessionTarget, title: string) =>
        dispatch({ type: "openTab", tabId: uid(), paneId: uid(), target, title }),
      newTab: () => dispatch({ type: "newTab", tabId: uid() }),
      launch: (tabId: string, target: SessionTarget, title: string) =>
        dispatch({ type: "launch", tabId, paneId: uid(), target, title }),
      /** `target` undefined: the active pane's own; null: a launcher pane. */
      split: (orientation: Orientation, target?: SessionTarget | null, title?: string) =>
        dispatch({ type: "split", orientation, paneId: uid(), target, title }),
      launchPane: (paneId: string, target: SessionTarget, title: string) =>
        dispatch({ type: "launchPane", paneId, target, title }),
      toggleZoom: (paneId?: string) => dispatch({ type: "toggleZoom", paneId }),
      toggleBroadcast: (tabId: string) => dispatch({ type: "toggleBroadcast", tabId }),
      swapPanes: (a: string, b: string) => dispatch({ type: "swapPanes", a, b }),
      movePaneToTab: (paneId: string) => dispatch({ type: "movePaneToTab", paneId, tabId: uid() }),
      applyPreset: (preset: Preset) =>
        dispatch({ type: "applyPreset", preset, paneIds: [uid(), uid(), uid(), uid()] }),
      closePane: (paneId?: string) => dispatch({ type: "closePane", paneId }),
      closeTab: (tabId: string) => dispatch({ type: "closeTab", tabId }),
      selectTab: (tabId: string) => dispatch({ type: "selectTab", tabId }),
      renameTab: (tabId: string, title: string) => dispatch({ type: "renameTab", tabId, title }),
      moveTab: (tabId: string, toIndex: number) => dispatch({ type: "moveTab", tabId, toIndex }),
      focusPane: (paneId: string) => dispatch({ type: "focusPane", paneId }),
    }),
    [],
  );
  return { state, statuses, ...actions };
}

export type WorkspaceApi = ReturnType<typeof useWorkspace>;

/** The pane next to `from` in `direction`, by on-screen position (ties: the first in layout order). */
function neighbour(panes: HTMLElement[], from: HTMLElement, direction: "left" | "right" | "up" | "down") {
  const a = from.getBoundingClientRect();
  const ac = { x: a.left + a.width / 2, y: a.top + a.height / 2 };
  let best: HTMLElement | null = null;
  let bestScore = Infinity;
  for (const el of panes) {
    if (el === from) continue;
    const r = el.getBoundingClientRect();
    const c = { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    const ahead =
      direction === "left"
        ? r.right <= a.left + 1
        : direction === "right"
          ? r.left >= a.right - 1
          : direction === "up"
            ? r.bottom <= a.top + 1
            : r.top >= a.bottom - 1;
    if (!ahead) continue;
    const horizontal = direction === "left" || direction === "right";
    const along = horizontal ? Math.abs(c.x - ac.x) : Math.abs(c.y - ac.y);
    const across = horizontal ? Math.abs(c.y - ac.y) : Math.abs(c.x - ac.x);
    const score = along + across * 2;
    if (score < bestScore) {
      bestScore = score;
      best = el;
    }
  }
  return best;
}

interface WorkspaceProps {
  ws: WorkspaceApi;
  /** The workspace is on screen (not a sidebar page). */
  visible: boolean;
  /** Shortcuts are live (false while the vault is locked). */
  enabled: boolean;
  /** Bring the workspace on screen (a shortcut opened a tab). */
  onShow: () => void;
  /**
   * The phone layout: the active tab fills the screen, with no tab strip
   * (sessions are switched from the drawer's Terminals) and no splits.
   */
  phone?: boolean;
  /** "Split right…" / "Split down…": pick a host for the pane the split makes. */
  onPickHost: (orientation: Orientation) => void;
  /** Set while the sidebar is hidden: ☰ leads the tab bar and opens it as a drawer. */
  onShowSidebar?: () => void;
}

/**
 * Terminal tabs with split panes. Every pane of every tab stays mounted
 * (its session lives as long as the pane); hidden tabs are just not shown.
 */
export function Workspace({ ws, visible, enabled, onShow, phone = false, onPickHost, onShowSidebar }: WorkspaceProps) {
  const { state } = ws;
  const panelRefs = useRef(new Map<string, HTMLElement>());

  // A stable element per pane: the layout moves it between slots.
  const elements = useRef(new Map<string, HTMLElement>());
  const paneElement = useCallback((paneId: string) => {
    let el = elements.current.get(paneId);
    if (!el) {
      el = document.createElement("div");
      el.className = "pane-home";
      elements.current.set(paneId, el);
    }
    return el;
  }, []);
  const allPanes = useMemo(() => state.tabs.flatMap((t) => panesOf(t.root).map((p) => ({ tab: t, pane: p }))), [state]);
  useEffect(() => {
    const live = new Set(allPanes.map((p) => p.pane.id));
    for (const id of [...elements.current.keys()]) if (!live.has(id)) elements.current.delete(id);
  }, [allPanes]);

  // Panels keep one DOM order however the strip is arranged: moving a panel
  // would reload the terminals in it. React matches them by key, so the
  // strip's order is only ever in the strip.
  const panelOrder = useRef<string[]>([]);
  panelOrder.current = stableOrder(panelOrder.current, state.tabs.map((t) => t.id));
  const panelTabs = panelOrder.current.map((id) => state.tabs.find((t) => t.id === id)!);

  const stateRef = useRef<WorkspaceState>(state);
  stateRef.current = state;
  const statusesRef = useRef(ws.statuses);
  statusesRef.current = ws.statuses;
  const { paneControls } = ws;
  const panel = useSidePanel();
  // Snippets go to the active tab's focused pane.
  const shown = activeTab(state);
  const shownPane = (shown && panesOf(shown.root).find((p) => p.id === shown.activePane && hasTarget(p))) ?? null;
  // Opening or closing the panel beside the terminals leaves the keys with the terminal.
  const panelWasOpen = useRef(panel.open);
  const shownPaneId = shownPane?.id;
  useEffect(() => {
    if (phone || panelWasOpen.current === panel.open) return;
    panelWasOpen.current = panel.open;
    if (shownPaneId) paneControls(shownPaneId)?.focus();
  }, [panel.open, phone, shownPaneId, paneControls]);
  // The phone's sheet is opened afresh each time: never on its own at start.
  useEffect(() => {
    if (phone && getSidePanel().open) setSidePanel({ ...getSidePanel(), open: false });
  }, [phone]);
  // Typing in `paneId` of a broadcasting tab, also into the tab's other open panes.
  const broadcastFrom = useCallback(
    (paneId: string, data: string) => {
      const tab = stateRef.current.tabs.find((t) => panesOf(t.root).some((p) => p.id === paneId));
      if (!tab?.broadcast) return;
      for (const id of broadcastTargets(tab, paneId, statusesRef.current)) paneControls(id)?.inject(data);
    },
    [paneControls],
  );
  const run = useCallback(
    (cmd: WorkspaceCommand): boolean => {
      switch (cmd.kind) {
        case "localTerminal":
          ws.openTab(localTarget(), LOCAL_TITLE);
          onShow();
          return true;
        case "newTab":
          ws.newTab();
          onShow();
          return true;
        case "close":
          if (!visible || !activeTab(stateRef.current)) return false;
          ws.closePane();
          return true;
        case "split":
          if (phone || !visible || !activeTab(stateRef.current)) return false;
          ws.split(cmd.orientation);
          return true;
        case "zoom": {
          const tab = activeTab(stateRef.current);
          if (phone || !visible || !tab?.root) return false;
          ws.toggleZoom();
          return true;
        }
        case "broadcast": {
          const tab = activeTab(stateRef.current);
          if (phone || !visible || !tab?.root) return false;
          ws.toggleBroadcast(tab.id);
          return true;
        }
        case "focus": {
          const tab = activeTab(stateRef.current);
          if (phone || !visible || !tab?.activePane) return false;
          // A zoomed tab keeps its other panes stashed (no rects), and focusing one would end the zoom.
          if (effectiveZoom(tab, phone)) return true;
          const panel = panelRefs.current.get(tab.id);
          const panes = panel ? [...panel.querySelectorAll<HTMLElement>("[data-pane-id]")] : [];
          const from = panes.find((el) => el.dataset.paneId === tab.activePane);
          const to = from && neighbour(panes, from, cmd.direction);
          if (to?.dataset.paneId) ws.focusPane(to.dataset.paneId);
          return true;
        }
      }
    },
    [ws, visible, onShow, phone],
  );

  // The Layout button's and the pane menus' entries; all act on the active pane unless given one.
  // A menu action leaves focus on <body> (its button is gone) unless the active pane changed: give it back
  // to the active pane (its terminal, or a launcher's first host), once the action has been rendered.
  const refocus = () =>
    setTimeout(() => {
      const t = activeTab(stateRef.current);
      if (t?.activePane) focusPane(t.activePane, paneControls);
    }, 0);
  const menuActions = (tab: Tab | undefined, paneId?: string): LayoutMenuActions => {
    const then =
      <A extends unknown[]>(f: (...a: A) => void) =>
      (...a: A) => {
        f(...a);
        refocus();
      };
    return {
      split: then((o) => ws.split(o)),
      // Not refocused: the palette it opens takes the keyboard, and a refocus would steal it back.
      splitPick: onPickHost,
      preset: then(ws.applyPreset),
      zoom: then(() => ws.toggleZoom(paneId)),
      broadcast: then(() => tab && ws.toggleBroadcast(tab.id)),
      moveToTab: then(() => paneId && ws.movePaneToTab(paneId)),
      close: then(() => ws.closePane(paneId)),
    };
  };
  const menuContext = (tab: Tab | undefined) => ({
    hasRoot: !!tab?.root,
    paneCount: panesOf(tab?.root ?? null).length,
    zoomed: !!tab && effectiveZoom(tab, false) !== null,
    broadcast: !!tab?.broadcast,
    labels: SHORTCUT_LABELS,
  });

  useEffect(() => {
    if (!enabled) return;
    // Capture phase: before xterm's own key handling, so these never reach a shell.
    const onKey = (e: KeyboardEvent) => {
      const cmd = matchShortcut(e);
      if (!cmd) return;
      // A modal (prompt, confirmation) owns the keyboard.
      if (document.querySelector("dialog[open]")) return;
      if (run(cmd)) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [enabled, run]);

  return (
    <section className="workspace" hidden={!visible} aria-label="Terminals">
      {!phone && (
        <Tabs
          panelOpen={panel.open}
          onTogglePanel={() => setSidePanel({ ...panel, open: !panel.open })}
          tabs={state.tabs}
          activeTab={state.activeTab}
          onSelect={ws.selectTab}
          onClose={ws.closeTab}
          onRename={ws.renameTab}
          onMove={ws.moveTab}
          onNew={ws.newTab}
          onShowSidebar={onShowSidebar}
          layoutMenu={layoutMenu(menuContext(activeTab(state)), menuActions(activeTab(state)), "tab")}
        />
      )}
      <div className="workspace-body">
      <div className="workspace-tabs">
      {panelTabs.map((tab) => (
        <div
          key={tab.id}
          // No tab strip on a phone: each panel is a region named after its session.
          {...(phone
            ? { role: "region", "aria-label": tabTitle(tab) }
            : { role: "tabpanel", "aria-labelledby": tabButtonId(tab.id) })}
          id={tabPanelId(tab.id)}
          className="tab-panel"
          hidden={tab.id !== state.activeTab}
          ref={(el) => {
            if (el) panelRefs.current.set(tab.id, el);
            else panelRefs.current.delete(tab.id);
          }}
        >
          {tab.root ? (
            <SplitView node={tab.root} paneElement={paneElement} zoomed={effectiveZoom(tab, phone)} />
          ) : (
            <Launcher onPick={(target, title) => ws.launch(tab.id, target, title)} />
          )}
        </div>
      ))}
      </div>
      {panel.open && (
        <SidePanel
          pane={shownPane}
          canSend={shownPane !== null && ws.statuses.get(shownPane.id) === "open"}
          send={(snippet) => {
            if (!shownPane) return;
            paneControls(shownPane.id)?.sendSnippet(snippet);
            // Not on a phone: the focus would raise the keyboard, which puts the panel away.
            if (!phone) paneControls(shownPane.id)?.focus();
          }}
          phone={phone}
        />
      )}
      </div>
      {allPanes.map(({ tab, pane }) => {
        const zoom = effectiveZoom(tab, phone);
        // A phone shows one pane of a split tab, with no header; a zoomed pane elsewhere keeps its own.
        const inSplit = tab.root?.kind === "split" && !phone;
        const common = {
          host: paneElement(pane.id),
          active: tab.activePane === pane.id,
          inSplit,
          onActivate: () => ws.focusPane(pane.id),
          onClose: () => ws.closePane(pane.id),
          header: {
            maximized: zoom === pane.id ? { count: panesOf(tab.root).length } : null,
            menu: layoutMenu(menuContext(tab), menuActions(tab, pane.id), "pane"),
            onRestore: () => {
              ws.toggleZoom(pane.id);
              refocus();
            },
            onSwap: (other: string) => {
              ws.swapPanes(pane.id, other);
              refocus();
            },
          },
        };
        // A phone shows one pane of a tab, so there is nobody to broadcast to.
        const broadcast =
          tab.broadcast && !phone
            ? {
                count: panesOf(tab.root).filter((p) => hasTarget(p) && ws.statuses.get(p.id) === "open").length,
              }
            : null;
        return hasTarget(pane) ? (
          <TerminalPane
            key={pane.id}
            pane={pane}
            {...common}
            label={paneLabel(pane)}
            // A zoom hides the other panes of the tab: no fit or focus while they are stashed.
            visible={visible && tab.id === state.activeTab && (!zoom || zoom === pane.id)}
            locked={!enabled}
            broadcast={broadcast}
            onBroadcast={(data) => broadcastFrom(pane.id, data)}
            onStatus={(status) => ws.reportStatus(pane.id, status)}
            onControls={(c) => ws.setPaneControls(pane.id, c)}
          />
        ) : (
          <LauncherPane
            key={pane.id}
            pane={pane}
            {...common}
            onPick={(target, title) => ws.launchPane(pane.id, target, title)}
          />
        );
      })}
    </section>
  );
}
