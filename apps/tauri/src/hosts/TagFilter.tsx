import type { CSSProperties } from "react";
import type { Id, Tag } from "../api/types";

/** Toggle buttons for filtering the host list: a host must carry every selected tag. */
export function TagFilter({
  tags,
  selected,
  onToggle,
  onClear,
}: {
  tags: Tag[];
  selected: ReadonlySet<Id>;
  onToggle: (id: Id) => void;
  onClear: () => void;
}) {
  if (tags.length === 0) return null;
  const sorted = [...tags].sort((a, b) => a.name.localeCompare(b.name));
  return (
    <div role="group" aria-label="Filter by tag" className="tag-filter">
      {sorted.map((t) => (
        <button
          key={t.id}
          type="button"
          className="tag-chip"
          aria-pressed={selected.has(t.id)}
          style={{ "--tag-color": t.color } as CSSProperties}
          onClick={() => onToggle(t.id)}
        >
          {t.name}
        </button>
      ))}
      {selected.size > 0 && (
        <button type="button" className="link" onClick={onClear}>
          Clear
        </button>
      )}
    </div>
  );
}
