import { useState, type DragEvent } from "react";
import type { Group, Id } from "../api/types";
import { RowMenu, inMenu } from "../ui/RowMenu";
import { HOST_DRAG_TYPE } from "./dnd";

export interface FolderCardProps {
  group: Group;
  /** Hosts in the group, subgroups included. */
  count: number;
  onOpen: () => void;
  onNewSubgroup: () => void;
  onEdit: () => void;
  onDelete: () => void;
  /** A host card was dropped on the folder. */
  onDropHost: (hostId: Id) => void;
}

/** A group as a card: click steps inside, "⋯" or right-click opens its actions, a dropped host card moves into it. */
export function FolderCard(p: FolderCardProps) {
  const { group } = p;
  // `anchor` is where a right-click happened; the "⋯" menu hangs under the card instead.
  const [menu, setMenu] = useState<{ anchor?: { x: number; y: number } } | null>(null);
  const [dropping, setDropping] = useState(false);
  const carriesHost = (e: DragEvent) => e.dataTransfer.types.includes(HOST_DRAG_TYPE);

  return (
    <div
      className="folder-card"
      data-drop={dropping ? "true" : undefined}
      onContextMenu={(e) => {
        e.preventDefault();
        if (!inMenu(e.target)) setMenu({ anchor: { x: e.clientX, y: e.clientY } });
      }}
      onDragOver={(e) => {
        if (!carriesHost(e)) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        setDropping(true);
      }}
      onDragLeave={(e) => {
        // Moving onto the card's own name or button is not leaving it.
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDropping(false);
      }}
      onDrop={(e) => {
        setDropping(false);
        const hostId = e.dataTransfer.getData(HOST_DRAG_TYPE);
        if (!hostId) return;
        e.preventDefault();
        p.onDropHost(hostId);
      }}
    >
      <button type="button" className="folder-card-main" aria-label={group.name} onClick={p.onOpen}>
        <svg className="folder-glyph" viewBox="0 0 24 24" width="22" height="22" aria-hidden="true">
          <path
            d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"
            fill="currentColor"
          />
        </svg>
        <span className="folder-card-text">
          <span className="folder-card-name">{group.name}</span>
          <span className="folder-card-count">{p.count === 1 ? "1 host" : `${p.count} hosts`}</span>
        </span>
      </button>
      <button
        type="button"
        className="folder-card-more"
        aria-label={`Actions for ${group.name}`}
        aria-haspopup="menu"
        aria-expanded={menu !== null}
        data-menu-trigger={group.id}
        onClick={() => setMenu((m) => (m ? null : {}))}
      >
        ⋯
      </button>
      {menu && (
        <RowMenu
          owner={group.id}
          label={group.name}
          anchor={menu.anchor}
          onClose={() => setMenu(null)}
          items={[
            ["Open", p.onOpen],
            ["New subgroup", p.onNewSubgroup],
            ["Edit group", p.onEdit],
            ["Delete group", p.onDelete],
          ]}
        />
      )}
    </div>
  );
}
