import { useState, type DragEvent } from "react";
import type { Group, Id } from "../api/types";
import { RowMenu } from "../ui/RowMenu";
import { HOST_DRAG_TYPE } from "./dnd";

export interface BreadcrumbProps {
  /** Groups from the top level down to the current one; empty at the top level. */
  trail: Group[];
  /** `null` is "All hosts". */
  onNavigate: (id: Id | null) => void;
  /** A host card was dropped on a segment (`null`: on "All hosts", out of any group). */
  onDropHost: (hostId: Id, groupId: Id | null) => void;
  /** The current group's actions, in a "⋯" after the last segment; `null` for none. */
  actions: [string, () => void][] | null;
}

/**
 * Where the Hosts page is: "All hosts › Homelab › Proxmox". Every segment but the
 * last navigates; "All hosts" and the segments above the current group take
 * dropped host cards ("All hosts" always does, even at the top level).
 */
export function Breadcrumb({ trail, onNavigate, onDropHost, actions }: BreadcrumbProps) {
  const [dropTarget, setDropTarget] = useState<Id | "all" | null>(null);
  const last = trail[trail.length - 1] as Group | undefined;

  function dropProps(target: Id | null) {
    const key = target ?? "all";
    const carriesHost = (e: DragEvent) => e.dataTransfer.types.includes(HOST_DRAG_TYPE);
    return {
      "data-drop": dropTarget === key ? "true" : undefined,
      onDragOver: (e: DragEvent) => {
        if (!carriesHost(e)) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        setDropTarget(key);
      },
      onDragLeave: (e: DragEvent) => {
        if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
        setDropTarget((t) => (t === key ? null : t));
      },
      onDrop: (e: DragEvent) => {
        setDropTarget(null);
        const hostId = e.dataTransfer.getData(HOST_DRAG_TYPE);
        if (!hostId) return;
        e.preventDefault();
        onDropHost(hostId, target);
      },
    };
  }

  return (
    <nav aria-label="Breadcrumb" className="breadcrumb">
      <ol>
        <li>
          <button
            type="button"
            className="crumb"
            aria-current={last ? undefined : "location"}
            onClick={() => onNavigate(null)}
            {...dropProps(null)}
          >
            All hosts
          </button>
        </li>
        {trail.slice(0, -1).map((g) => (
          <li key={g.id}>
            <button type="button" className="crumb" onClick={() => onNavigate(g.id)} {...dropProps(g.id)}>
              {g.name}
            </button>
          </li>
        ))}
        {last && (
          <li key={last.id}>
            <span className="crumb crumb-here" aria-current="location">
              {last.name}
            </span>
            {actions && <CurrentActions group={last} actions={actions} />}
          </li>
        )}
      </ol>
    </nav>
  );
}

/** The current group's "⋯" and its menu; keyed by the group, so a menu never outlives the group it was opened for. */
function CurrentActions({ group, actions }: { group: Group; actions: [string, () => void][] }) {
  const [menu, setMenu] = useState(false);
  return (
    <span className="crumb-actions">
      <button
        type="button"
        className="crumb-more"
        aria-label={`Actions for ${group.name}`}
        aria-haspopup="menu"
        aria-expanded={menu}
        data-menu-trigger={group.id}
        onClick={() => setMenu((m) => !m)}
      >
        ⋯
      </button>
      {menu && <RowMenu owner={group.id} label={group.name} items={actions} onClose={() => setMenu(false)} />}
    </span>
  );
}
