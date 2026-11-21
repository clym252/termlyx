import { conflictCountLabel } from "../conflicts/model";
import { IS_ANDROID } from "../platform";
import { SyncStatusDot } from "../sync/SyncStatusDot";
import { SHORTCUT_LABELS } from "../workspace/shortcuts";
import { paletteLabel } from "./hotkeys";
import { useLongPressLabel } from "./useLongPressLabel";
import { NavIcon } from "./navIcons";
import type { RouteDef, RouteId } from "./routes";
import type { SidebarMode } from "./sidebarPref";
import { Wordmark } from "./Wordmark";

interface SidebarProps {
  routes: readonly RouteDef[];
  /** The page shown, or null while the terminals are. */
  current: RouteId | null;
  /** The terminal workspace entry: open tab count, whether it is shown. */
  terminals: { count: number; shown: boolean; onShow: () => void };
  /** Conflicts waiting to be settled; the badge is hidden at 0. */
  conflictCount: number;
  /** Full labels, or an icon rail (labels become tooltips and accessible names). */
  mode: Exclude<SidebarMode, "hidden">;
  /** Absent in the phone layout's drawer: no collapse control and no shortcut hints there. */
  onToggleMode?: () => void;
  onNavigate: (id: RouteId) => void;
  onSearch: () => void;
  onLock: () => void;
}

export function Sidebar({
  routes,
  current,
  terminals,
  conflictCount,
  mode,
  onToggleMode,
  onNavigate,
  onSearch,
  onLock,
}: SidebarProps) {
  const listed = routes.filter((r) => !r.hidden);
  const rail = mode === "rail";
  /** In the rail the icon is all there is to see: the name moves to aria-label and the tooltip. */
  const press = useLongPressLabel();
  /** On touch, resting a finger on a rail icon shows the name (there is no hover). */
  const railName = (name: string) => (rail ? { "aria-label": name, title: name, ...press.bind(name) } : {});
  const toggleName = rail ? "Expand sidebar" : "Collapse sidebar";
  const terminalsName = terminals.count > 0 ? `Terminals (${terminals.count})` : "Terminals";
  const inDrawer = !onToggleMode;
  return (
    <aside className="sidebar" data-mode={mode}>
      <div className="sidebar-head">
        <div className="brand"><Wordmark compact={rail} /></div>
        {onToggleMode && (
          <button
            type="button"
            className="nav-item sidebar-toggle"
            aria-label={toggleName}
            title={`${toggleName} (${SHORTCUT_LABELS.sidebar})`}
            {...(rail ? press.bind(toggleName) : {})}
            onClick={onToggleMode}
          >
            <NavIcon name={rail ? "expand" : "collapse"} />
          </button>
        )}
      </div>
      <button
        type="button"
        className="search-button"
        onClick={onSearch}
        aria-haspopup="dialog"
        {...(rail ? { "aria-label": "Search", title: `Search (${paletteLabel()})`, ...press.bind("Search") } : {})}
      >
        <NavIcon name="search" />
        {!rail && (
          <>
            <span className="nav-label">Search</span>
            {/* Touch-first on Android: "Ctrl+Shift+K" only crowds (and clipped) the tablet sidebar. */}
            {!inDrawer && !IS_ANDROID && <kbd>{paletteLabel()}</kbd>}
          </>
        )}
      </button>
      <nav aria-label="Main">
        <ul>
          {listed.map((r) => (
            <li key={r.id}>
              <button
                type="button"
                className="nav-item"
                aria-current={r.id === current ? "page" : undefined}
                onClick={() => onNavigate(r.id)}
                {...railName(r.label)}
              >
                <NavIcon name={r.id} />
                {!rail && <span className="nav-label">{r.label}</span>}
              </button>
            </li>
          ))}
          <li>
            <button
              type="button"
              className="nav-item terminals-item"
              aria-current={terminals.shown ? "page" : undefined}
              disabled={terminals.count === 0}
              onClick={terminals.onShow}
              {...railName(terminalsName)}
            >
              <NavIcon name="terminals" />
              {!rail && (
                <span className="nav-label">
                  Terminals
                  {terminals.count > 0 && <span className="count"> ({terminals.count})</span>}
                </span>
              )}
              {rail && terminals.count > 0 && <span className="rail-badge">{terminals.count}</span>}
            </button>
          </li>
        </ul>
      </nav>
      <div className="sidebar-foot">
        {conflictCount > 0 && (
          <button
            type="button"
            className="nav-item conflict-badge"
            data-testid="conflict-badge"
            aria-current={current === "conflicts" ? "page" : undefined}
            onClick={() => onNavigate("conflicts")}
            {...railName(conflictCountLabel(conflictCount))}
          >
            <NavIcon name="conflicts" />
            {rail ? <span className="rail-badge">{conflictCount}</span> : <span className="nav-label">{conflictCountLabel(conflictCount)}</span>}
          </button>
        )}
        <SyncStatusDot onOpen={() => onNavigate("settings")} compact={rail} />
        <button type="button" className="nav-item" onClick={onLock} {...railName("Lock vault")}>
          <NavIcon name="lock" />
          {!rail && <span className="nav-label">Lock vault</span>}
        </button>
      </div>
      {/* Fixed, so the rail's clipping does not reach it. */}
      {press.label && (
        <div className="rail-tip" aria-hidden="true" style={{ left: press.label.x, top: press.label.y }}>
          {press.label.name}
        </div>
      )}
    </aside>
  );
}
