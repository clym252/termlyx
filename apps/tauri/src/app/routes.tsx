import type { ReactNode } from "react";
import { ConflictsPage } from "../conflicts/ConflictsPage";
import { DevTerminal } from "../dev/DevTerminal";
import { ForwardingPage } from "../forwarding/ForwardingPage";
import { HostsPage } from "../hosts/HostsPage";
import type { Id, SessionTarget } from "../api/types";
import { KeychainPage } from "../keychain/KeychainPage";
import { KnownHostsPage } from "../knownhosts/KnownHostsPage";
import { SettingsPage } from "../settings/SettingsPage";
import { SftpPage } from "../sftp/SftpPage";
import { SnippetsPage } from "../snippets/SnippetsPage";

export type RouteId = "hosts" | "snippets" | "keychain" | "forwarding" | "knownHosts" | "sftp" | "settings" | "conflicts" | "devTerminal";

/** Things a page may be asked to do when it is navigated to. */
export interface RouteContext {
  /** The page is the one showing (always true for a page that is not kept mounted). */
  visible: boolean;
  /** Open this host in the editor (from global search). `nonce` makes repeats distinct. */
  openHost: { id: Id; nonce: number } | null;
  /** The page has acted on `openHost`; forget it so returning to the page does not repeat it. */
  onHostOpened(): void;
  /** Opens a terminal tab on `target` and shows the workspace. */
  connect(target: SessionTarget, title: string): void;
  /** Bumped when the phone layout's ＋ asks Hosts for a new host. */
  newHostRequest: number;
}

export interface RouteDef {
  id: RouteId;
  label: string;
  /** Reached from elsewhere (the conflicts badge), not listed in the sidebar's navigation. */
  hidden?: boolean;
  /**
   * Stays mounted (hidden) while another page or the terminals show, so its place and unsaved edits
   * are kept; dropped when the vault locks.
   */
  keepMounted?: boolean;
  render(ctx: RouteContext): ReactNode;
}

/**
 * The sidebar, in order. Terminals are not a route: the workspace (Layout)
 * stays mounted beside the pages.
 */
export const ROUTES: readonly RouteDef[] = [
  {
    id: "hosts",
    label: "Hosts",
    keepMounted: true,
    render: (ctx) => (
      <HostsPage
        visible={ctx.visible}
        openHost={ctx.openHost}
        onHostOpened={ctx.onHostOpened}
        onConnect={(host) => ctx.connect({ kind: "host", hostId: host.id }, host.label)}
        newHostRequest={ctx.newHostRequest}
      />
    ),
  },
  {
    id: "snippets",
    label: "Snippets",
    render: () => <SnippetsPage />,
  },
  {
    id: "keychain",
    label: "Keychain",
    render: () => <KeychainPage />,
  },
  {
    id: "forwarding",
    label: "Port Forwarding",
    render: () => <ForwardingPage />,
  },
  {
    id: "knownHosts",
    label: "Known Hosts",
    render: () => <KnownHostsPage />,
  },
  {
    id: "sftp",
    label: "SFTP",
    render: () => <SftpPage />,
  },
  {
    id: "settings",
    label: "Settings",
    render: () => <SettingsPage />,
  },
  {
    id: "conflicts",
    label: "Conflicts",
    hidden: true,
    render: () => <ConflictsPage />,
  },
];

/**
 * DEV only: the Phase 0 connect form (direct parameters, no vault), for
 * trying a server before the Keychain page exists, and the throughput bench.
 */
export const DEV_ROUTES: readonly RouteDef[] = import.meta.env.DEV
  ? [{ id: "devTerminal", label: "Dev terminal", render: () => <DevTerminal /> }]
  : [];

export const ALL_ROUTES: readonly RouteDef[] = [...ROUTES, ...DEV_ROUTES];
