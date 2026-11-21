import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api";
import type { Host, Id, SessionTarget } from "../api/types";
import { clearConflicts, refreshConflicts, useConflicts } from "../conflicts/store";
import { FORM_FACTOR, IS_ANDROID, IS_MOBILE, layoutFor, type FormFactor } from "../platform";
import { ConfirmDialog } from "../ui/Dialog";
import { focusPane } from "../workspace/focusPane";
import { LOCAL_TITLE, localTarget } from "../workspace/local";
import { activeTab, hasTarget, paneLabel, panesOf, tabTitle, type Orientation, type Preset } from "../workspace/model";
import { PhoneTerminals } from "../workspace/PhoneTerminals";
import { SHORTCUT_LABELS, isSidebarToggleKey } from "../workspace/shortcuts";
import { useWorkspace, Workspace } from "../workspace/Workspace";
import { backAction, closeMenu, closeTopDialog, setBackHandler } from "./back";
import { CommandPalette, type How } from "./CommandPalette";
import { Drawer } from "./Drawer";
import { isInTerminal, isPaletteKey } from "./hotkeys";
import type { ActionId, PaletteMode, TerminalItem } from "./paletteItems";
import { PhoneTopBar } from "./PhoneTopBar";
import { UpdateNotice } from "./UpdateNotice";
import { Sidebar } from "./Sidebar";
import { ALL_ROUTES, type RouteContext, type RouteId } from "./routes";
import { lastVisibleSidebarMode, nextSidebarMode, useSidebarMode } from "./sidebarPref";
import { getSidePanel, openSidePanel, setSidePanel } from "../workspace/sidePanelPref";

/** The layout for this device at the window's width; only an iPad's can change (Slide Over, Split View). */
function useLayoutKind(): FormFactor {
  const [width, setWidth] = useState(() => window.innerWidth);
  useEffect(() => {
    if (FORM_FACTOR !== "tablet") return;
    const onResize = () => setWidth(window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  return layoutFor(FORM_FACTOR, width);
}

/**
 * The unlocked app: sidebar, the current page or the terminal workspace,
 * and the command palette.
 *
 * On a phone: a top bar over one page or one full-screen terminal, the
 * sidebar in a drawer (☰ or a swipe from the left edge), and the open
 * sessions listed under the drawer's Terminals (`sessions`).
 *
 * `locked`: the vault locked while terminals may be open. The layout stays
 * mounted (hidden, shortcuts off) so locking never ends sessions silently;
 * Gate shows the unlock screen over it.
 */
export function Layout({ onLocked, locked = false }: { onLocked: () => void; locked?: boolean }) {
  const [route, setRoute] = useState<RouteId>("hosts");
  const [view, setView] = useState<"page" | "terminals" | "sessions">("page");
  const viewRef = useRef(view);
  viewRef.current = view;
  const phone = useLayoutKind() === "phone";
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [newHostRequest, setNewHostRequest] = useState(0);
  const [disconnectingAll, setDisconnectingAll] = useState(false);
  // The open palette; `id` is new for each opening, so a palette opened from a pick starts afresh.
  const [palette, setPalette] = useState<{ mode: PaletteMode; id: number } | null>(null);
  const paletteId = useRef(0);
  // What had the keyboard when the palette opened (not an earlier palette's own input, when a pick opens another).
  const beforePalette = useRef<Element | null>(null);
  const openPalette = useCallback((mode: PaletteMode = { kind: "all" }) => {
    const had = document.activeElement;
    if (!had?.closest("dialog")) beforePalette.current = had;
    setPalette({ mode, id: ++paletteId.current });
  }, []);
  const [openHost, setOpenHost] = useState<{ id: Id; nonce: number } | null>(null);
  // Never reused: the Hosts page stays mounted and ignores a nonce it has already acted on.
  const openNonce = useRef(0);
  const [sidebarMode, setSidebarMode] = useSidebarMode();
  // The sidebar lives in a drawer on a phone, and wherever it is set to Hidden.
  const drawerNav = phone || sidebarMode === "hidden";
  const ws = useWorkspace();
  const { conflicts } = useConflicts();
  const tabCount = ws.state.tabs.length;
  const { openTab } = ws;

  // A palette pick leaves the keyboard on <body>: give it to the active pane once the pick has rendered
  // (a pane that is still mounting takes it itself).
  const wsState = useRef(ws.state);
  wsState.current = ws.state;
  const { paneControls } = ws;
  const focusActivePane = useCallback(
    () =>
      setTimeout(() => {
        const pane = activeTab(wsState.current)?.activePane;
        if (pane) focusPane(pane, paneControls);
      }, 0),
    [paneControls],
  );
  // Escape (or a click outside, or the chord again): nothing was picked, and the dialog's own focus return is
  // lost with it. The terminal gets the keyboard back when it is on screen, else whatever had it before.
  const cancelPalette = useCallback(() => {
    setPalette(null);
    if (viewRef.current === "terminals") focusActivePane();
    else setTimeout(() => (beforePalette.current as HTMLElement | null)?.focus?.(), 0);
  }, [focusActivePane]);

  // The last tab closed: back to the page.
  useEffect(() => {
    if (tabCount === 0) setView("page");
  }, [tabCount]);

  // The session list is the phone's tab strip: an iPad widening out of Slide Over shows the tabs instead.
  // Locking, or leaving the drawer layouts, closes the drawer.
  useEffect(() => {
    if (!phone) setView((v) => (v === "sessions" ? "terminals" : v));
    if (!drawerNav || locked) setDrawerOpen(false);
  }, [phone, drawerNav, locked]);

  // Conflicts for the badge: read when the vault opens, dropped when it locks.
  useEffect(() => {
    if (locked) clearConflicts();
    else void refreshConflicts();
  }, [locked]);

  useEffect(() => {
    if (locked) return;
    const onKey = (e: KeyboardEvent) => {
      if (!isPaletteKey(e, isInTerminal(e.target))) return;
      e.preventDefault();
      if (palette) cancelPalette();
      else openPalette();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [locked, palette, openPalette, cancelPalette]);

  // The sidebar's own button flips full and rail; the chord cycles through hidden too.
  const toggleSidebar = useCallback(() => setSidebarMode(sidebarMode === "rail" ? "full" : "rail"), [sidebarMode, setSidebarMode]);
  const cycleSidebar = useCallback(() => setSidebarMode(nextSidebarMode(sidebarMode)), [sidebarMode, setSidebarMode]);

  useEffect(() => {
    if (locked) return;
    const onKey = (e: KeyboardEvent) => {
      if (!isSidebarToggleKey(e)) return;
      // A modal (prompt, confirmation) owns the keyboard.
      if (document.querySelector("dialog[open]")) return;
      e.preventDefault();
      cycleSidebar();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [locked, cycleSidebar]);

  const lock = useCallback(async () => {
    try {
      await api.vaultLock();
    } finally {
      onLocked();
    }
  }, [onLocked]);

  const connect = useCallback(
    (target: SessionTarget, title: string) => {
      openTab(target, title);
      setView("terminals");
    },
    [openTab],
  );
  const showTerminals = useCallback(() => setView("terminals"), []);
  const navigate = (id: RouteId) => {
    setRoute(id);
    setView("page");
    setDrawerOpen(false);
  };

  // What the palette's picks do.
  const splitWith = (orientation: Orientation, target: SessionTarget, title: string) => {
    ws.split(orientation, target, title);
    showTerminals();
    focusActivePane();
  };
  const showHost = (hostId: Id) => {
    navigate("hosts");
    setOpenHost({ id: hostId, nonce: ++openNonce.current });
  };
  // A new session on `target`: beside the current pane when asked and there is one to split, else its own tab.
  const openTarget = (target: SessionTarget, title: string, how: "open" | "split") => {
    if (how === "split" && !phone && activeTab(ws.state)?.root) splitWith("horizontal", target, title);
    else connect(target, title);
  };
  const onPaletteHost = (host: Host, how: How) => {
    const target: SessionTarget = { kind: "host", hostId: host.id };
    if (palette?.mode.kind === "pickHost") return splitWith(palette.mode.orientation, target, host.label);
    if (how === "details") showHost(host.id);
    else openTarget(target, host.label, how);
  };
  // Enter switches to the terminal; the others act on what its pane opened, as for a host. The palette keeps
  // Alt+Enter on a terminal that is not a host's to itself.
  const onPaletteTerminal = (item: TerminalItem, how: How) => {
    if (how === "details" && item.target?.kind === "host") return showHost(item.target.hostId);
    if (how === "split" && item.target) return openTarget(item.target, item.title, "split");
    ws.selectTab(item.tabId);
    if (item.paneId) ws.focusPane(item.paneId);
    showTerminals();
    focusActivePane();
  };
  const onPaletteAction = (id: ActionId) => {
    if (id.startsWith("go:")) return navigate(id.slice(3) as RouteId);
    if (id.startsWith("preset:")) {
      ws.applyPreset(id.slice(7) as Preset);
    } else if (id === "localTerminal" && palette?.mode.kind === "pickHost") {
      return splitWith(palette.mode.orientation, localTarget(), LOCAL_TITLE);
    } else {
      switch (id) {
        case "newTab":
          ws.newTab();
          break;
        case "localTerminal":
          ws.openTab(localTarget(), LOCAL_TITLE);
          break;
        case "splitRight":
          ws.split("horizontal");
          break;
        case "splitDown":
          ws.split("vertical");
          break;
        case "splitRightPick":
          return openPalette({ kind: "pickHost", orientation: "horizontal" });
        case "splitDownPick":
          return openPalette({ kind: "pickHost", orientation: "vertical" });
        case "zoom":
          ws.toggleZoom();
          break;
        case "broadcast": {
          const tab = activeTab(ws.state);
          if (tab) ws.toggleBroadcast(tab.id);
          break;
        }
        case "closePane":
          ws.closePane();
          break;
        case "newHost":
          navigate("hosts");
          return setNewHostRequest((n) => n + 1);
        case "hideSidebar":
          return setSidebarMode("hidden");
        case "showSidebar":
          return setSidebarMode(lastVisibleSidebarMode());
        case "panel:snippets":
          openSidePanel("snippets");
          break;
        case "panel:themes":
          openSidePanel("themes");
          break;
        case "lock":
          return void lock();
      }
    }
    showTerminals();
    focusActivePane();
  };
  // Android's Back (`App` registers the press): one step back, never closing the app or a session.
  const stepBack = useRef(() => {});
  stepBack.current = () => {
    const action = locked
      ? "background"
      : backAction({
          menuOpen: document.querySelector('[role="menu"]') !== null,
          dialogOpen: document.querySelector("dialog[open]") !== null,
          panelOpen: phone && view === "terminals" && getSidePanel().open,
          drawerOpen,
          terminalShown: view === "terminals",
          // The Terminals list is a page of its own, like Settings.
          route: view === "sessions" ? "terminals" : route,
        });
    if (action === "closeMenu") closeMenu();
    else if (action === "closeDialog") closeTopDialog();
    else if (action === "closePanel") setSidePanel({ ...getSidePanel(), open: false });
    else if (action === "closeDrawer") setDrawerOpen(false);
    else if (action === "toHosts") navigate("hosts");
    else void api.moveToBackground();
  };
  useEffect(() => {
    if (!IS_ANDROID) return;
    setBackHandler(() => stepBack.current());
    return () => setBackHandler(null);
  }, []);

  // The sidebar's pages, as it lists them.
  const paletteRoutes = ALL_ROUTES.filter((r) => !r.hidden).map((r) => ({ id: r.id, label: r.label }));

  const current = ALL_ROUTES.find((r) => r.id === route) ?? ALL_ROUTES[0];
  const routeContext = (visible: boolean): RouteContext => ({
    visible,
    openHost,
    onHostOpened: () => setOpenHost(null),
    connect,
    newHostRequest,
  });

  const sidebar = (
    <Sidebar
      routes={ALL_ROUTES}
      current={view === "page" ? current.id : null}
      terminals={{
        count: tabCount,
        shown: view !== "page",
        onShow: () => {
          setView(phone ? "sessions" : "terminals");
          setDrawerOpen(false);
        },
      }}
      conflictCount={conflicts?.length ?? 0}
      mode={phone || sidebarMode === "hidden" ? "full" : sidebarMode}
      onToggleMode={drawerNav ? undefined : toggleSidebar}
      onNavigate={navigate}
      onSearch={() => {
        setDrawerOpen(false);
        openPalette();
      }}
      onLock={() => {
        setDrawerOpen(false);
        void lock();
      }}
    />
  );

  // The phone's top bar: the shown terminal's, or the page's (the session list's is "Terminals").
  const shownTab = activeTab(ws.state);
  const shownPane = shownTab?.activePane ?? null;
  // A split tab shows its focused pane alone: the bar names that pane.
  const focused = shownTab?.root?.kind === "split" ? panesOf(shownTab.root).find((p) => p.id === shownPane) : undefined;
  const shownPaneLabel = focused ? paneLabel(focused) : null;
  // Hidden sidebar on a wider screen: the phone's page bar (☰ and the name); terminals put ☰ in the tab bar.
  const hiddenBar =
    !phone && sidebarMode === "hidden" && view === "page" ? (
      <PhoneTopBar kind="page" title={current.label} onMenu={() => setDrawerOpen(true)} />
    ) : null;
  const topBar = !phone ? hiddenBar : view === "terminals" && shownTab ? (
    <PhoneTopBar
      kind="terminal"
      label={shownPaneLabel ?? tabTitle(shownTab)}
      onBack={() => navigate("hosts")}
      actions={[
        // A split tab shows one pane: closing the session ends that pane, not the others (the drawer closes a tab).
        ["Close session", () => (focused ? ws.closePane(focused.id) : ws.closeTab(shownTab.id))],
        ["Reconnect", () => shownPane && ws.paneControls(shownPane)?.reconnect()],
        ["Paste", () => shownPane && ws.paneControls(shownPane)?.paste()],
        ["Snippets", () => openSidePanel("snippets")],
        ["Themes", () => openSidePanel("themes")],
        ["Disconnect all", () => setDisconnectingAll(true)],
      ]}
    />
  ) : (
    <PhoneTopBar
      kind="page"
      title={view === "sessions" ? "Terminals" : current.label}
      onMenu={() => setDrawerOpen(true)}
      onNewHost={view === "page" && current.id === "hosts" ? () => setNewHostRequest((n) => n + 1) : undefined}
    />
  );

  return (
    <div className={phone ? "shell layout-phone" : "shell"} hidden={locked}>
      {!drawerNav && sidebar}
      {/* The open drawer is modal: nothing behind it takes the keyboard focus (Tab on a hardware keyboard). */}
      <main className={!phone && sidebarMode === "hidden" ? "main sidebar-hidden" : "main"} inert={drawerNav && drawerOpen && !locked}>
        {topBar}
        {/* Kept-mounted pages hold their state while hidden; locking unmounts them, dropping it. */}
        {!locked &&
          ALL_ROUTES.filter((r) => r.keepMounted).map((r) => {
            const shown = view === "page" && current.id === r.id;
            return (
              <div key={r.id} className="page-slot" hidden={!shown}>
                {r.render(routeContext(shown))}
              </div>
            );
          })}
        {view === "page" && !current.keepMounted && current.render(routeContext(true))}
        {view === "sessions" && (
          <PhoneTerminals
            sessions={ws.state.tabs.map((t) => {
              const pane = t.activePane;
              const status = pane ? (ws.statuses.get(pane) ?? null) : null;
              const panes =
                t.root?.kind === "split"
                  ? panesOf(t.root).map((p) => ({
                      paneId: p.id,
                      label: paneLabel(p),
                      status: hasTarget(p) ? (ws.statuses.get(p.id) ?? null) : null,
                      active: p.id === t.activePane,
                    }))
                  : undefined;
              return { tabId: t.id, label: tabTitle(t), status, panes };
            })}
            onShow={(tabId) => {
              ws.selectTab(tabId);
              setView("terminals");
            }}
            onShowPane={(tabId, paneId) => {
              ws.selectTab(tabId);
              ws.focusPane(paneId);
              setView("terminals");
            }}
            onClose={ws.closeTab}
            onClosePane={(paneId) => ws.closePane(paneId)}
          />
        )}
        <Workspace
          ws={ws}
          visible={view === "terminals"}
          enabled={!locked}
          onShow={showTerminals}
          phone={phone}
          onPickHost={(o) => openPalette({ kind: "pickHost", orientation: o })}
          onShowSidebar={!phone && sidebarMode === "hidden" ? () => setDrawerOpen(true) : undefined}
        />
      </main>
      {drawerNav && (
        <Drawer open={drawerOpen && !locked} onOpen={() => !locked && setDrawerOpen(true)} onClose={() => setDrawerOpen(false)}>
          {sidebar}
        </Drawer>
      )}
      {disconnectingAll && (
        <ConfirmDialog
          title="Disconnect all"
          confirmLabel="Disconnect all"
          danger
          onConfirm={() => {
            setDisconnectingAll(false);
            for (const t of ws.state.tabs) ws.closeTab(t.id);
          }}
          onCancel={() => setDisconnectingAll(false)}
        >
          <p>Close {tabCount === 1 ? "the open session" : `all ${tabCount} open sessions`}?</p>
        </ConfirmDialog>
      )}
      {!locked && <UpdateNotice />}
      {palette && !locked && (
        <CommandPalette
          key={palette.id}
          mode={palette.mode}
          source={{
            workspace: ws.state,
            touch: IS_MOBILE,
            phone,
            sidebarHidden: sidebarMode === "hidden",
            labels: SHORTCUT_LABELS,
            routes: paletteRoutes,
          }}
          onClose={() => setPalette(null)}
          onCancel={cancelPalette}
          onHost={onPaletteHost}
          onTerminal={onPaletteTerminal}
          onAction={onPaletteAction}
        />
      )}
    </div>
  );
}
