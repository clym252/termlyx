import { useState, type CSSProperties, type KeyboardEvent, type MouseEvent } from "react";
import type { Host, Tag } from "../api/types";
import { RowMenu, inMenu } from "../ui/RowMenu";
import { badgeIndex, initials } from "./browse";
import { HOST_DRAG_TYPE } from "./dnd";

const MAX_TAGS = 3;

export interface HostCardProps {
  host: Host;
  /** `user@address[:port]`, from `hostSubtitle`. */
  subtitle: string;
  /** The host's own tags. */
  tags: Tag[];
  /** The host's group path; given where cards from several groups mix (filter results, the top level), empty for an ungrouped host. */
  path?: string;
  /** Whether the host is open in the editor panel. */
  editing: boolean;
  onConnect: () => void;
  onEdit: () => void;
  onDelete: () => void;
}

/**
 * A host as a card: click connects, ✎ edits, right-click (or Shift+F10) opens the
 * menu, and the card can be dragged onto a folder. The root is a div so that its
 * buttons do not nest.
 */
export function HostCard(p: HostCardProps) {
  const { host } = p;
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const n = badgeIndex(host.id) + 1;
  const shown = p.tags.slice(0, MAX_TAGS);

  // A double-click is two clicks (detail 1, then 2) and must open one session; keyboard activation has detail 0.
  const connectOnce = (e: MouseEvent) => {
    if (e.detail <= 1) p.onConnect();
  };

  function onKeyDown(e: KeyboardEvent<HTMLDivElement>) {
    if (e.key !== "ContextMenu" && !(e.key === "F10" && e.shiftKey)) return;
    e.preventDefault();
    if (inMenu(e.target)) return;
    const box = e.currentTarget.getBoundingClientRect();
    setMenu({ x: box.left + 16, y: box.top + box.height / 2 });
  }

  return (
    <div
      className="host-card"
      data-editing={p.editing ? "true" : undefined}
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData(HOST_DRAG_TYPE, host.id);
        e.dataTransfer.effectAllowed = "move";
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        if (!inMenu(e.target)) setMenu({ x: e.clientX, y: e.clientY });
      }}
      onKeyDown={onKeyDown}
    >
      <button type="button" className="host-card-main" aria-label={host.label} onClick={connectOnce}>
        <span
          className="host-badge"
          aria-hidden="true"
          style={{ background: `var(--badge-${n}-bg)`, color: `var(--badge-${n}-fg)` }}
        >
          {initials(host.label)}
        </span>
        <span className="host-card-text">
          <span className="host-card-label">{host.label}</span>
          <span className="host-card-sub">{p.subtitle}</span>
          {p.path && <span className="host-card-path">{p.path}</span>}
          {shown.length > 0 && (
            <span className="host-card-tags">
              {shown.map((t) => (
                <span key={t.id} className="tag-pill" style={{ "--tag-color": t.color } as CSSProperties}>
                  {t.name}
                </span>
              ))}
              {p.tags.length > shown.length && <span className="tag-more">+{p.tags.length - shown.length}</span>}
            </span>
          )}
        </span>
      </button>
      <span className="host-card-actions">
        <button type="button" className="host-card-edit" data-host-id={host.id} aria-label={`Edit host ${host.label}`} title="Edit host" onClick={p.onEdit}>
          ✎
        </button>
        <button type="button" className="host-card-connect" aria-label={`Connect to ${host.label}`} onClick={connectOnce}>
          Connect
        </button>
      </span>
      {menu && (
        <RowMenu
          owner={host.id}
          label={host.label}
          anchor={menu}
          onClose={() => setMenu(null)}
          items={[
            ["Connect", p.onConnect],
            ["Edit host", p.onEdit],
            ["Delete host", p.onDelete],
          ]}
        />
      )}
    </div>
  );
}
