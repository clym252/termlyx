import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import type { Group, Host, Id } from "../api/types";
import { ConfirmDialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { useRemembered } from "../ui/remembered";
import { IS_MOBILE } from "../platform";
import { Breadcrumb } from "./Breadcrumb";
import {
  breadcrumb,
  childGroups,
  filterActive,
  filterHosts,
  hostSubtitle,
  hostsAt,
  resolveCurrentGroup,
} from "./browse";
import { DeleteGroupDialog } from "./DeleteGroupDialog";
import { FolderCard } from "./FolderCard";
import { GroupEditor } from "./GroupEditor";
import { HostCard } from "./HostCard";
import { HostEditor } from "./HostEditor";
import { ImportSshConfigDialog } from "./ImportSshConfigDialog";
import { groupPath, hostCounts } from "./model";
import { TagFilter } from "./TagFilter";
import { useHostsData } from "./useHostsData";

type Editing = { mode: "new"; key: number } | { mode: "edit"; id: Id } | null;
type GroupDialog =
  | { mode: "new"; parentId: Id | null }
  | { mode: "edit"; id: Id }
  | { mode: "delete"; id: Id }
  | null;

interface HostsPageProps {
  /**
   * Whether the page is the one showing. It stays mounted while hidden, so its group, filter and
   * editor (with unsaved edits) are still there on return; it reloads when it shows again.
   */
  visible: boolean;
  /** Open this host in the editor (global search). */
  openHost: { id: Id; nonce: number } | null;
  onHostOpened: () => void;
  /** Opens a terminal on the host. */
  onConnect: (host: Host) => void;
  /** Bumped to open the new-host form from outside the page (the phone layout's ＋). */
  newHostRequest?: number;
}

/** The ids from the top level down to `id`, recorded on entering it (the fallback if it is deleted). */
const trailIds = (groups: Group[], id: Id | null) => breadcrumb(groups, id).map((g) => g.id);

/**
 * One group at a time: its subgroups as folder cards, then its own hosts as
 * cards, under a breadcrumb back up. A filter (text or tags) swaps both for
 * every matching host below the current group. The host editor is a side panel over the right edge.
 */
export function HostsPage({ visible, openHost, onHostOpened, onConnect, newHostRequest = 0 }: HostsPageProps) {
  const { data, error, reload } = useHostsData();
  // The group being shown (`null`: the top level) and the trail it was entered through.
  const [current, setCurrent] = useState<Id | null>(null);
  const [ancestry, setAncestry] = useState<Id[]>([]);
  const [filterText, setFilterText] = useState("");
  const [tagFilter, setTagFilter] = useState<ReadonlySet<Id>>(new Set());
  const [editing, setEditing] = useState<Editing>(null);
  const [groupDialog, setGroupDialog] = useState<GroupDialog>(null);
  const [deletingHost, setDeletingHost] = useState<Host | null>(null);
  const [importing, setImporting] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const newKey = useRef(0);
  const pageRef = useRef<HTMLElement>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const newHostRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLElement>(null);
  // Set when an edit is being opened: the panel takes focus once it shows.
  const focusPanel = useRef(false);
  const prevEditing = useRef<Editing>(null);

  // Changes made on other pages (keychain, import, ...) are not announced; read them on return.
  const wasVisible = useRef(visible);
  useEffect(() => {
    if (visible && !wasVisible.current) void reload();
    wasVisible.current = visible;
  }, [visible, reload]);

  // A host picked in global search: go to its group, drop the filters, open it.
  // Waits for the data, which the group comes from.
  const openedNonce = useRef<number | null>(null);
  const hostOpened = useRef(onHostOpened);
  hostOpened.current = onHostOpened;
  useEffect(() => {
    if (!openHost || !data || openedNonce.current === openHost.nonce) return;
    openedNonce.current = openHost.nonce;
    const groupId = data.hosts.find((h) => h.id === openHost.id)?.groupId ?? null;
    setCurrent(groupId);
    setAncestry(trailIds(data.groups, groupId));
    setFilterText("");
    setTagFilter(new Set());
    focusPanel.current = true;
    setEditing({ mode: "edit", id: openHost.id });
    hostOpened.current();
  }, [openHost, data]);

  const groups = useMemo(() => data?.groups ?? [], [data]);
  const hosts = useMemo(() => data?.hosts ?? [], [data]);
  const tags = useMemo(() => data?.tags ?? [], [data]);
  const identities = useMemo(() => data?.identities ?? [], [data]);

  // A current group deleted (here or by sync) falls back to its nearest surviving ancestor.
  const currentId = resolveCurrentGroup(groups, current, ancestry);
  const trail = useMemo(() => breadcrumb(groups, currentId), [groups, currentId]);
  // While the current group exists, keep its recorded trail up to date (it may be moved, here or by sync),
  // so a later deletion falls back to its parent of the moment.
  useEffect(() => {
    if (current === null || currentId !== current) return;
    const ids = trail.map((g) => g.id);
    setAncestry((prev) => (prev.length === ids.length && prev.every((id, i) => id === ids[i]) ? prev : ids));
  }, [current, currentId, trail]);
  // An open editor survives its record being deleted on another device: it keeps the last version
  // and what was typed (saving then follows the backend's merge rules).
  const editingHost =
    useRemembered(
      editing?.mode === "edit" ? editing.id : null,
      editing?.mode === "edit" ? hosts.find((h) => h.id === editing.id) : undefined,
    ) ?? null;

  const panelOpen = editing !== null && (editing.mode === "new" || editingHost !== null);

  // Focus follows the editor: into the panel when an edit opens (a new host focuses its Label itself),
  // and back to whatever opened it when it closes: the card's ✎, or New host. If that no longer
  // exists (the host was deleted), the page heading.
  useEffect(() => {
    const prev = prevEditing.current;
    prevEditing.current = editing;
    if (editing) {
      if (focusPanel.current && editing.mode === "edit") panelRef.current?.focus();
      focusPanel.current = false;
    } else if (prev) {
      const opener =
        prev.mode === "new"
          ? newHostRef.current
          : pageRef.current?.querySelector<HTMLElement>(`[data-host-id="${CSS.escape(prev.id)}"]`);
      (opener ?? headingRef.current)?.focus();
    }
  }, [editing]);

  const filtering = filterActive(filterText, tagFilter);
  const results = useMemo(
    () => (filtering ? filterHosts(hosts, groups, identities, currentId, filterText, tagFilter) : []),
    [filtering, hosts, groups, identities, currentId, filterText, tagFilter],
  );
  // Never the current group itself (a group that is its own parent, from sync).
  const folders = useMemo(
    () => childGroups(groups, currentId).filter((g) => g.id !== currentId),
    [groups, currentId],
  );
  const here = useMemo(() => hostsAt(hosts, currentId), [hosts, currentId]);
  const counts = useMemo(() => hostCounts(groups, hosts), [groups, hosts]);

  function navigate(id: Id | null) {
    setCurrent(id);
    setAncestry(id === null ? [] : trailIds(groups, id));
  }

  function toggleTag(id: Id) {
    setTagFilter((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  }

  function clearFilter() {
    setFilterText("");
    setTagFilter(new Set());
  }

  /** Moves a host (dropped on a folder or breadcrumb segment) to `groupId`, keeping everything else. */
  async function moveHost(hostId: Id, groupId: Id | null) {
    const h = hosts.find((x) => x.id === hostId);
    if (!h || h.groupId === groupId) return;
    try {
      await api.upsertEntity("host", {
        id: h.id,
        label: h.label,
        address: h.address,
        groupId,
        tagIds: h.tagIds,
        settings: h.settings,
      });
      await reload();
    } catch (e) {
      setActionError(describeError(e));
    }
  }

  async function deleteHost(host: Host) {
    try {
      await api.deleteEntity("host", host.id);
      setDeletingHost(null);
      await reload();
      // After the reload, so the focus does not go back to a card that is about to go.
      setEditing((e) => (e?.mode === "edit" && e.id === host.id ? null : e));
    } catch (e) {
      setActionError(describeError(e));
      setDeletingHost(null);
    }
  }

  const newHost = () => setEditing({ mode: "new", key: ++newKey.current });
  const handledNewHost = useRef(newHostRequest);
  useEffect(() => {
    if (newHostRequest === handledNewHost.current) return;
    handledNewHost.current = newHostRequest;
    setEditing({ mode: "new", key: ++newKey.current });
  }, [newHostRequest]);
  const newGroup = () => setGroupDialog({ mode: "new", parentId: currentId });

  const groupTarget = groupDialog && groupDialog.mode !== "delete" ? groupDialog : null;
  const editGroup = useRemembered(
    groupDialog?.mode === "edit" ? groupDialog.id : null,
    groupDialog?.mode === "edit" ? groups.find((g) => g.id === groupDialog.id) : undefined,
  );
  const deleteGroup = groupDialog?.mode === "delete" ? groups.find((g) => g.id === groupDialog.id) : undefined;

  function hostCard(h: Host, path?: string) {
    return (
      <li key={h.id}>
        <HostCard
          host={h}
          subtitle={hostSubtitle(h, groups, identities)}
          tags={tags.filter((t) => h.tagIds.includes(t.id))}
          path={path}
          editing={editing?.mode === "edit" && editing.id === h.id}
          onConnect={() => onConnect(h)}
          onEdit={() => {
            focusPanel.current = true;
            setEditing({ mode: "edit", id: h.id });
          }}
          onDelete={() => setDeletingHost(h)}
        />
      </li>
    );
  }

  function folderView() {
    if (folders.length === 0 && here.length === 0) {
      return currentId === null ? (
        <p className="muted empty">No hosts yet. Use New host to add one.</p>
      ) : (
        <div className="hosts-empty">
          <p className="muted">This group is empty.</p>
          <div className="row">
            <button type="button" onClick={newHost}>
              New host
            </button>
            <button type="button" onClick={newGroup}>
              New group
            </button>
          </div>
        </div>
      );
    }
    return (
      <>
        {folders.length > 0 && (
          <div className="hosts-section">
            <h2 className="section-head">Groups</h2>
            <ul className="card-grid" aria-label="Groups">
              {folders.map((g) => (
                <li key={g.id}>
                  <FolderCard
                    group={g}
                    count={counts.get(g.id) ?? 0}
                    onOpen={() => navigate(g.id)}
                    onNewSubgroup={() => setGroupDialog({ mode: "new", parentId: g.id })}
                    onEdit={() => setGroupDialog({ mode: "edit", id: g.id })}
                    onDelete={() => setGroupDialog({ mode: "delete", id: g.id })}
                    onDropHost={(hostId) => void moveHost(hostId, g.id)}
                  />
                </li>
              ))}
            </ul>
          </div>
        )}
        {here.length > 0 && (
          <div className="hosts-section">
            <h2 className="section-head">Hosts</h2>
            <ul className="card-grid" aria-label="Hosts">
              {here.map((h) => hostCard(h, currentId === null ? groupPath(groups, h.groupId) : undefined))}
            </ul>
          </div>
        )}
      </>
    );
  }

  function filterView() {
    return (
      <>
        <div className="section-head-row">
          <h2 className="section-head">Hosts</h2>
          <button type="button" className="link" onClick={clearFilter}>
            Clear filter
          </button>
        </div>
        {results.length === 0 ? (
          <p className="muted empty">No hosts match the filter.</p>
        ) : (
          <ul className="card-grid" aria-label="Hosts">
            {results.map((h) => hostCard(h, groupPath(groups, h.groupId)))}
          </ul>
        )}
      </>
    );
  }

  return (
    <section
      ref={pageRef}
      className={panelOpen ? "page hosts-page with-editor" : "page hosts-page"}
      aria-labelledby="hosts-title"
    >
      <header className="page-head hosts-top">
        <h1 id="hosts-title" ref={headingRef} tabIndex={-1}>
          Hosts
        </h1>
        <div className="hosts-tools">
          <input
            type="search"
            className="hosts-filter"
            aria-label="Filter hosts"
            autoCapitalize="off"
            autoCorrect="off"
            placeholder="Filter…"
            value={filterText}
            onChange={(e) => setFilterText(e.target.value)}
          />
          <TagFilter tags={tags} selected={tagFilter} onToggle={toggleTag} onClear={() => setTagFilter(new Set())} />
          {/* iOS has no ~/.ssh to import from, so the import is desktop-only. */}
          {!IS_MOBILE && (
            <button type="button" onClick={() => setImporting(true)}>
              Import SSH config…
            </button>
          )}
          <button type="button" onClick={newGroup} disabled={!data}>
            New group
          </button>
          <button type="button" className="primary hosts-new-host" ref={newHostRef} onClick={newHost}>
            New host
          </button>
        </div>
      </header>

      {(error || actionError) && (
        <p role="alert" className="form-error">
          {error ?? actionError}
        </p>
      )}

      {!data ? (
        !error && (
          <p role="status" className="muted">
            Loading…
          </p>
        )
      ) : (
        <>
          <Breadcrumb
            trail={trail}
            onNavigate={navigate}
            onDropHost={(hostId, groupId) => void moveHost(hostId, groupId)}
            actions={
              currentId === null
                ? null
                : [
                    ["New subgroup", () => setGroupDialog({ mode: "new", parentId: currentId })],
                    ["Edit group", () => setGroupDialog({ mode: "edit", id: currentId })],
                    ["Delete group", () => setGroupDialog({ mode: "delete", id: currentId })],
                  ]
            }
          />
          <div className="hosts-layout">
            <div className="host-list-pane">{filtering ? filterView() : folderView()}</div>
          </div>
        </>
      )}

      {data && editing && panelOpen && (
        // Not a modal: the cards behind stay usable. Escape closes it, unless a dialog (a confirm) owns the key,
        // an input method is composing, or a save is in flight (it would reopen the panel when it lands).
        <aside
          ref={panelRef}
          className="editor-panel"
          aria-label="Host editor"
          tabIndex={-1}
          onKeyDown={(e) => {
            if (e.key !== "Escape" || e.defaultPrevented || document.querySelector("dialog[open]")) return;
            if (e.nativeEvent.isComposing || e.currentTarget.querySelector("[data-busy]")) return;
            e.preventDefault();
            setEditing(null);
          }}
        >
          <HostEditor
            key={editing.mode === "new" ? `new-${editing.key}` : editing.id}
            host={editing.mode === "edit" ? editingHost : null}
            defaultGroupId={currentId}
            groups={groups}
            tags={tags}
            identities={identities}
            onSaved={async () => {
              await reload(); // before closing, so the card does not flash old values
              setEditing(null);
            }}
            onCancel={() => setEditing(null)}
            onDelete={setDeletingHost}
          />
        </aside>
      )}

      {data && groupTarget && (groupTarget.mode === "new" || editGroup) && (
        <GroupEditor
          target={
            groupTarget.mode === "new"
              ? { mode: "new", parentId: groupTarget.parentId }
              : { mode: "edit", group: editGroup! }
          }
          groups={groups}
          identities={identities}
          onClose={() => setGroupDialog(null)}
          onSaved={() => {
            // Stay where we are: a new group shows up as a folder here.
            setGroupDialog(null);
            void reload();
          }}
        />
      )}

      {data && deleteGroup && (
        <DeleteGroupDialog
          group={deleteGroup}
          groups={groups}
          hosts={hosts}
          onClose={() => setGroupDialog(null)}
          onDeleted={() => {
            setGroupDialog(null);
            if (deleteGroup.id === currentId) {
              const parent = deleteGroup.parentId;
              navigate(parent !== null && groups.some((g) => g.id === parent) ? parent : null);
            }
            void reload();
          }}
        />
      )}

      {importing && (
        <ImportSshConfigDialog onClose={() => setImporting(false)} onImported={() => void reload()} />
      )}

      {deletingHost && (
        <ConfirmDialog
          title={`Delete host ${deletingHost.label}`}
          confirmLabel="Delete"
          danger
          onConfirm={() => void deleteHost(deletingHost)}
          onCancel={() => setDeletingHost(null)}
        >
          <p>
            Delete <strong>{deletingHost.label}</strong> and its port forwarding rules? This cannot be
            undone.
          </p>
        </ConfirmDialog>
      )}
    </section>
  );
}
