import { useRef, useState, type DragEvent, type KeyboardEvent } from "react";
import { LayoutIcon, NavIcon } from "../app/navIcons";
import { RowMenu, type MenuEntry } from "../ui/RowMenu";
import { tabTitle, type Tab } from "./model";
import { SHORTCUT_LABELS } from "./shortcuts";
import { ShowSidebarButton } from "../app/ShowSidebarButton";

interface TabsProps {
  tabs: Tab[];
  activeTab: string | null;
  onSelect: (tabId: string) => void;
  onClose: (tabId: string) => void;
  onNew: () => void;
  /** Names a tab (a blank name goes back to the automatic one). */
  onRename: (tabId: string, title: string) => void;
  /** Puts a tab at `toIndex` of the new order. */
  onMove: (tabId: string, toIndex: number) => void;
  /** The Layout button's menu (see layoutMenu). */
  layoutMenu: MenuEntry[];
  /** The side panel's (snippets, themes) toggle. */
  panelOpen: boolean;
  onTogglePanel(): void;
  /** Set while the sidebar is hidden: ☰ first in the bar. */
  onShowSidebar?: () => void;
}

/** The drag payload type of a tab (its id), carried within the tab strip. */
export const TAB_DRAG_TYPE = "application/x-tether-tab";

export const tabButtonId = (tabId: string) => `tab-${tabId}`;
export const tabPanelId = (tabId: string) => `tabpanel-${tabId}`;

/**
 * The terminal tab strip: a tab per tab (with its close button), New tab, and
 * the Layout button at the strip's right edge.
 * Double-clicking a tab's title renames it in place: Enter or leaving the
 * field keeps the name, Escape drops the edit. Tabs drag to reorder; a bar
 * on the side of the tab under the pointer shows where the dragged one lands.
 */
export function Tabs({
  tabs,
  activeTab,
  onSelect,
  onClose,
  onNew,
  onRename,
  onMove,
  layoutMenu,
  panelOpen,
  onTogglePanel,
  onShowSidebar,
}: TabsProps) {
  const [layoutOpen, setLayoutOpen] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  // Where a dragged tab would land.
  const [drop, setDrop] = useState<{ tabId: string; side: "before" | "after" } | null>(null);
  // Set once Enter or Escape ended the edit, so the blur that follows (some
  // engines blur a focused field as it is removed) does not end it again.
  const ended = useRef(false);

  const start = (tabId: string) => {
    ended.current = false;
    setEditing(tabId);
  };
  const finish = (tab: Tab, value: string | null) => {
    if (ended.current) return;
    ended.current = true;
    setEditing(null);
    // Leaving the shown name as it was is no rename: an automatic name
    // keeps following the focused pane.
    if (value !== null && value.trim() !== tabTitle(tab)) onRename(tab.id, value);
  };
  const onEditKey = (tab: Tab, e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") finish(tab, e.currentTarget.value);
    else if (e.key === "Escape") {
      // Escape must not also reach a dialog or the terminal.
      e.stopPropagation();
      finish(tab, null);
    }
  };

  const carriesTab = (e: DragEvent) => e.dataTransfer.types.includes(TAB_DRAG_TYPE);
  const sideOf = (e: DragEvent<HTMLElement>) => {
    const box = e.currentTarget.getBoundingClientRect();
    return e.clientX < box.left + box.width / 2 ? "before" : "after";
  };
  const onDropOn = (e: DragEvent<HTMLElement>, target: number) => {
    setDrop(null);
    const tabId = e.dataTransfer.getData(TAB_DRAG_TYPE);
    const from = tabs.findIndex((t) => t.id === tabId);
    if (from < 0) return;
    e.preventDefault();
    // `at` is a slot between tabs; the tab's index once it has left its old place.
    const at = target + (sideOf(e) === "after" ? 1 : 0);
    onMove(tabId, at > from ? at - 1 : at);
  };

  return (
    <div className="tabbar">
      {onShowSidebar && <ShowSidebarButton onShow={onShowSidebar} />}
      <div role="tablist" aria-label="Terminal tabs" className="tablist">
        {tabs.map((tab, index) => {
          const title = tabTitle(tab);
          const selected = tab.id === activeTab;
          const marker = drop?.tabId === tab.id ? drop.side : undefined;
          return (
            <div
              key={tab.id}
              className={selected ? "tab selected" : "tab"}
              // A tab being renamed is a text field: dragging would fight selecting its text.
              draggable={editing !== tab.id}
              data-drop-before={marker === "before" ? "true" : undefined}
              data-drop-after={marker === "after" ? "true" : undefined}
              onDragStart={(e) => {
                e.dataTransfer.setData(TAB_DRAG_TYPE, tab.id);
                e.dataTransfer.effectAllowed = "move";
              }}
              onDragEnd={() => setDrop(null)}
              onDragOver={(e) => {
                if (!carriesTab(e)) return;
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                const side = sideOf(e);
                setDrop((prev) => (prev?.tabId === tab.id && prev.side === side ? prev : { tabId: tab.id, side }));
              }}
              onDragLeave={(e) => {
                // Moving onto the tab's own title or close button is not leaving it.
                if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDrop(null);
              }}
              onDrop={(e) => {
                if (carriesTab(e)) onDropOn(e, index);
              }}
            >
              {editing === tab.id ? (
                <input
                  className="tab-rename"
                  aria-label="Tab name"
                  defaultValue={title}
                  autoFocus
                  onFocus={(e) => e.currentTarget.select()}
                  onKeyDown={(e) => onEditKey(tab, e)}
                  onBlur={(e) => finish(tab, e.currentTarget.value)}
                />
              ) : (
                <button
                  type="button"
                  role="tab"
                  id={tabButtonId(tab.id)}
                  aria-selected={selected}
                  aria-controls={tabPanelId(tab.id)}
                  tabIndex={selected ? 0 : -1}
                  className="tab-title"
                  title={`${title} (double-click to rename)`}
                  onClick={() => onSelect(tab.id)}
                  onDoubleClick={() => start(tab.id)}
                >
                  <span className="tab-label">{title}</span>
                  {tab.broadcast && <span className="tab-broadcast">Broadcast</span>}
                </button>
              )}
              <button
                type="button"
                className="tab-close"
                aria-label={`Close tab ${title}`}
                title={`Close tab (${SHORTCUT_LABELS.close} closes the focused pane)`}
                onClick={() => onClose(tab.id)}
              >
                ×
              </button>
            </div>
          );
        })}
      </div>
      <button type="button" className="tab-new" aria-label="New tab" title={`New tab (${SHORTCUT_LABELS.newTab})`} onClick={onNew}>
        +
      </button>
      <span className="tab-layout-wrap">
        <button
          type="button"
          className="tab-layout"
          aria-label="Layout"
          aria-haspopup="menu"
          aria-expanded={layoutOpen}
          data-menu-trigger="tab-layout"
          title="Layout: split, arrange, maximize, broadcast"
          onClick={() => setLayoutOpen((o) => !o)}
        >
          <LayoutIcon />
        </button>
        {layoutOpen && <RowMenu owner="tab-layout" label="Layout" items={layoutMenu} onClose={() => setLayoutOpen(false)} />}
      </span>
      <button
        type="button"
        className="tab-layout"
        aria-label="Side panel"
        aria-pressed={panelOpen}
        title="Side panel: snippets and themes"
        onClick={onTogglePanel}
      >
        <NavIcon name="sidePanel" />
      </button>
    </div>
  );
}
