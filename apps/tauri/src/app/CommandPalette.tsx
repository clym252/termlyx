import { useCallback, useEffect, useId, useMemo, useState, type KeyboardEvent } from "react";
import { api } from "../api";
import type { Group, Host, Tag } from "../api/types";
import { CMD_KEYS, IS_MOBILE } from "../platform";
import { useVaultChanged } from "../sync/vaultChanged";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import {
  paletteGroups,
  type ActionId,
  type PaletteItem,
  type PaletteMode,
  type PaletteSource,
  type TerminalItem,
} from "./paletteItems";

export type How = "open" | "split" | "details";

interface CommandPaletteProps {
  mode: PaletteMode;
  source: Omit<PaletteSource, "hosts" | "tags" | "groups">;
  /** The palette is going away: before a pick is reported, and for a cancel. */
  onClose: () => void;
  /** Escape or a click outside (after `onClose`): nothing was picked. */
  onCancel: () => void;
  /** `how`: Enter, Mod+Enter, Alt+Enter. A pick in pick mode is always "open". */
  onHost: (host: Host, how: How) => void;
  /** `how` as for a host, on the pane's target: "split" opens it beside the current pane, "details" shows its host. */
  onTerminal: (item: TerminalItem, how: How) => void;
  onAction: (id: ActionId) => void;
}

// What the keys are called on this platform, for the footer.
const MOD_NAME = CMD_KEYS ? "⌘" : "Ctrl+";
const ALT_NAME = CMD_KEYS ? "⌥" : "Alt+";

/**
 * The command palette (Cmd+K, Ctrl+Shift+K): open terminals, hosts and actions
 * behind one input. Enter on a host connects, Mod+Enter splits, Alt+Enter opens
 * its details; on an open terminal Enter switches to it and the others act on
 * what it opened (Alt+Enter does nothing where that is not a host). Picking a
 * host mode (`pickHost`) lists only what a new pane can open.
 * It closes itself before it reports a pick, so a pick may open the palette again.
 */
export function CommandPalette({ mode, source, onClose, onCancel, onHost, onTerminal, onAction }: CommandPaletteProps) {
  const [data, setData] = useState<{ hosts: Host[]; tags: Tag[]; groups: Group[] } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const listId = useId();
  const picking = mode.kind === "pickHost";

  const load = useCallback(async () => {
    try {
      const [hosts, tags, groups] = await Promise.all([
        api.listEntities("host"),
        api.listEntities("tag"),
        api.listEntities("group"),
      ]);
      setData({ hosts, tags, groups });
      setError(null);
    } catch (e) {
      setError(describeError(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);
  useVaultChanged(load);

  const groups = useMemo(
    () => (data ? paletteGroups(query, { ...source, ...data }, mode) : []),
    [data, query, source, mode],
  );
  const items = useMemo(() => groups.flatMap((g) => g.items), [groups]);
  const current = Math.min(active, Math.max(items.length - 1, 0));
  const optionId = (item: PaletteItem) => `${listId}-${item.key}`;
  const activeItem = items[current];

  // Arrowing past the visible part of a long list.
  useEffect(() => {
    if (activeItem) document.getElementById(optionId(activeItem))?.scrollIntoView?.({ block: "nearest" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeItem?.key]);

  function run(item: PaletteItem, how: How) {
    // Details of a terminal that is not a host's (local, a launcher): nothing to show, and the palette stays.
    if (item.kind === "terminal" && how === "details" && item.target?.kind !== "host") return;
    onClose();
    if (item.kind === "terminal") onTerminal(item, how);
    else if (item.kind === "host") onHost(item.host, picking ? "open" : how);
    else onAction(item.id);
  }

  function onKeyDown(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive(Math.min(current + 1, items.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive(Math.max(current - 1, 0));
    } else if (e.key === "Enter" && activeItem) {
      e.preventDefault();
      const mod = CMD_KEYS ? e.metaKey : e.ctrlKey;
      run(activeItem, mod ? "split" : e.altKey ? "details" : "open");
    }
  }

  return (
    <Dialog
      title={picking ? "Open in new pane" : "Search"}
      onClose={() => {
        onClose();
        onCancel();
      }}
    >
      <div className="dialog-content">
        <input
          role="combobox"
          aria-label="Search"
          aria-expanded="true"
          aria-controls={listId}
          aria-activedescendant={activeItem ? optionId(activeItem) : undefined}
          aria-autocomplete="list"
          placeholder="Hosts, open terminals or actions"
          autoComplete="off"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          autoFocus
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setActive(0);
          }}
          onKeyDown={onKeyDown}
        />
        {error && (
          <p role="alert" className="form-error">
            {error}
          </p>
        )}
        <div id={listId} role="listbox" aria-label="Results" className="search-results">
          {groups.map((g) => (
            <div key={g.title} role="group" aria-label={g.title}>
              <div className="search-group" aria-hidden="true">
                {g.title}
              </div>
              {g.items.map((item) => {
                const selected = item === activeItem;
                return (
                  <div
                    key={item.key}
                    id={optionId(item)}
                    role="option"
                    aria-selected={selected}
                    className={selected ? "search-item active" : "search-item"}
                    onMouseMove={() => setActive(items.indexOf(item))}
                    onClick={() => run(item, "open")}
                  >
                    <Row item={item} picking={picking} onDetails={() => run(item, "details")} />
                  </div>
                );
              })}
            </div>
          ))}
        </div>
        {data && items.length === 0 && <p className="muted">{picking ? "No hosts match." : "Nothing matches."}</p>}
        {!picking && (
          <p className="muted small search-keys">
            {`↩ connect · ${MOD_NAME}↩ split right · ${ALT_NAME}↩ details`}
          </p>
        )}
      </div>
    </Dialog>
  );
}

function Row({ item, picking, onDetails }: { item: PaletteItem; picking: boolean; onDetails: () => void }) {
  switch (item.kind) {
    case "terminal":
      return (
        <>
          <span className="host-label">{item.label}</span>
          <span className="muted small">{item.detail}</span>
        </>
      );
    case "host":
      return (
        <>
          <span className="host-label">{item.host.label}</span>
          <span className="host-address">{item.host.address}</span>
          {IS_MOBILE && !picking ? (
            // No Alt+Enter on a touch screen. (Picking a host for a new pane has no details: a tap opens it.)
            <span className="search-detail">
              <span className="muted small">{item.detail}</span>
              <button
                type="button"
                className="small"
                onClick={(e) => {
                  e.stopPropagation();
                  onDetails();
                }}
              >
                Details
              </button>
            </span>
          ) : (
            <span className="muted small">{item.detail}</span>
          )}
        </>
      );
    case "action":
      return (
        <>
          <span className="host-label">{item.label}</span>
          {item.hint && <kbd>{item.hint}</kbd>}
        </>
      );
  }
}
