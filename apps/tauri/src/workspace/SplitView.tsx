import { Fragment, useEffect, useLayoutEffect, useRef } from "react";
import { Group, Panel, Separator, useGroupRef, type Layout as GroupLayout } from "react-resizable-panels";
import { panesOf, type LayoutNode } from "./model";

/** Where a pane's DOM home is shown: the element is moved in, never re-created. */
function PaneSlot({ el }: { el: HTMLElement }) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const slot = ref.current;
    if (!slot) return;
    slot.appendChild(el);
    return () => {
      if (el.parentNode === slot) slot.removeChild(el);
    };
  }, [el]);
  return <div className="pane-slot" ref={ref} />;
}

function nodeKey(node: LayoutNode): string {
  return node.kind === "pane" ? node.pane.id : node.id;
}

/**
 * What each split's panels were last sized to, by split id, one size per slot (a child's index): a swap
 * trades the panes in the slots and leaves the geometry. It outlives the groups (a zoom unmounts them).
 */
type Sizes = Map<string, number[]>;

type Split = Extract<LayoutNode, { kind: "split" }>;

function splitIds(node: LayoutNode): string[] {
  return node.kind === "pane" ? [] : [node.id, ...node.children.flatMap(splitIds)];
}

/** The saved slot sizes of a split as a library layout, unless its slot count has changed since. */
function slotLayout(ids: string[], slots: number[] | undefined): GroupLayout | undefined {
  return slots && slots.length === ids.length ? Object.fromEntries(ids.map((id, i) => [id, slots[i]])) : undefined;
}

function SplitGroup({
  node,
  paneElement,
  sizes,
}: {
  node: Split;
  paneElement: (paneId: string) => HTMLElement;
  sizes: Sizes;
}) {
  const group = useGroupRef();
  const ids = node.children.map(nodeKey);
  const idsRef = useRef(ids);
  idsRef.current = ids;
  // Panels are keyed by pane, so a swap would carry each pane's size to its new slot: the group remounts on a new
  // ordering (see key), and its defaultLayout is by slot (see Sizes).
  const slots = sizes.get(node.id);
  // The library's own double-click resets to a panel's defaultSize, and these have none.
  const equalize = () => {
    const equal = ids.map(() => 100 / ids.length);
    sizes.set(node.id, equal);
    group.current?.setLayout(slotLayout(ids, equal)!);
  };
  return (
    <Group
      orientation={node.orientation}
      className="split-group"
      // The library caches a layout per panel ordering for the group's life, and that wins over defaultLayout: a
      // swap, a resize and a swap back would restore the first ordering's stale sizes. A new ordering is a new group
      // (the pane elements only move, so no session ends). A new split id (a preset applied again) is one too:
      // it starts from equal sizes, not the old group's.
      key={`${node.id}:${ids.join(",")}`}
      groupRef={group}
      defaultLayout={slotLayout(ids, slots)}
      onLayoutChanged={(layout) => {
        const cur = idsRef.current;
        if (cur.every((id) => id in layout)) sizes.set(node.id, cur.map((id) => layout[id]));
      }}
    >
      {node.children.map((child, i) => (
        <Fragment key={nodeKey(child)}>
          {i > 0 && <Separator className="split-handle" disableDoubleClick onDoubleClick={equalize} />}
          <Panel id={nodeKey(child)} minSize={60} className="split-panel">
            <Layout node={child} paneElement={paneElement} sizes={sizes} />
          </Panel>
        </Fragment>
      ))}
    </Group>
  );
}

/** The nested resizable groups of `node`, each pane's element in its slot. */
function Layout({ node, paneElement, sizes }: { node: LayoutNode; paneElement: (paneId: string) => HTMLElement; sizes: Sizes }) {
  if (node.kind === "pane") return <PaneSlot el={paneElement(node.pane.id)} />;
  return <SplitGroup node={node} paneElement={paneElement} sizes={sizes} />;
}

/**
 * A tab's split layout: nested resizable groups, with each pane's element
 * (from `paneElement`) placed in its slot. With `zoomed` (a pane of the tab)
 * that pane fills the tab and the others wait in a hidden stash: their
 * elements stay in the DOM, so their sessions go on. A zoom remounts the
 * groups, so the sizes the user dragged are kept here and restored with them.
 * Double-clicking a divider evens out all the panels of its group.
 */
export function SplitView({
  node,
  paneElement,
  zoomed,
}: {
  node: LayoutNode;
  paneElement: (paneId: string) => HTMLElement;
  zoomed: string | null;
}) {
  const sizes = useRef<Sizes>(new Map());
  // Splits that are gone (a pane closed, a preset applied) take their sizes with them.
  useEffect(() => {
    const live = new Set(splitIds(node));
    for (const id of sizes.current.keys()) if (!live.has(id)) sizes.current.delete(id);
  }, [node]);
  const panes = panesOf(node);
  if (!zoomed || !panes.some((p) => p.id === zoomed)) return <Layout node={node} paneElement={paneElement} sizes={sizes.current} />;
  return (
    <>
      <PaneSlot key={zoomed} el={paneElement(zoomed)} />
      <div className="zoom-stash" hidden>
        {panes
          .filter((p) => p.id !== zoomed)
          .map((p) => (
            <PaneSlot key={p.id} el={paneElement(p.id)} />
          ))}
      </div>
    </>
  );
}
