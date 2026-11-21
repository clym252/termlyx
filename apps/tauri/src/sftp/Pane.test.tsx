import type { ComponentProps } from "react";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { Pane, type PaneBackend } from "./Pane";
import { driveEntries, THIS_PC, type FileEntry, type Side } from "./paths";

// The platform is fixed at load in the product; the tests choose it per case.
const platform = vi.hoisted(() => ({ windows: false }));
vi.mock("../platform", () => ({
  get IS_WINDOWS() {
    return platform.windows;
  },
  get IS_MAC() {
    return false;
  },
}));

// jsdom has no modal <dialog>.
beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function close() {
    this.removeAttribute("open");
  };
});
afterEach(() => {
  cleanup();
  platform.windows = false;
});

const folder = (path: string): FileEntry => ({
  name: path.slice(path.lastIndexOf("\\") + 1),
  path,
  isDir: true,
  size: 0,
  mtime: 0,
  mode: null,
  isSymlink: false,
});

/** A Windows-like local file system: two drives and a user folder with a space and a non-ASCII name. */
function windowsBackend(): PaneBackend & { listed: string[] } {
  const tree: Record<string, FileEntry[]> = {
    "C:\\": [folder("C:\\Users")],
    "C:\\Users": [folder("C:\\Users\\Zoë Lee")],
    "C:\\Users\\Zoë Lee": [folder("C:\\Users\\Zoë Lee\\Existing")],
    "D:\\": [],
  };
  const listed: string[] = [];
  return {
    listed,
    resolve: async (path) => path,
    list: async (path) => {
      listed.push(path);
      if (path === THIS_PC) return driveEntries(["C:\\", "D:\\"]);
      const entries = tree[path];
      if (!entries) throw new Error(`no such folder ${path}`);
      return entries;
    },
    mkdir: async () => {},
  };
}

function mount(side: Side, backend: PaneBackend, initialPath: string, extra: Partial<ComponentProps<typeof Pane>> = {}) {
  render(
    <Pane
      side={side}
      title={side === "local" ? "This computer" : "Server"}
      backend={backend}
      initialPath={initialPath}
      refreshToken={0}
      onPathChange={vi.fn()}
      transferLabel="Upload"
      onTransfer={vi.fn()}
      onDragEntries={vi.fn()}
      acceptsDrop={() => false}
      onDropEntries={vi.fn()}
      {...extra}
    />,
  );
}

const pathBox = () => screen.getByRole("textbox", { name: "Path" }) as HTMLInputElement;

describe("Pane: Windows local folders", () => {
  beforeEach(() => {
    platform.windows = true;
  });

  it("goes up from a drive root to the This PC list and opens a drive from it", async () => {
    const backend = windowsBackend();
    mount("local", backend, "C:\\Users");
    await screen.findByRole("rowheader", { name: "Zoë Lee" });

    fireEvent.click(screen.getByRole("button", { name: "Up" }));
    await screen.findByRole("rowheader", { name: "Users" });
    expect(pathBox().value).toBe("C:\\");

    fireEvent.click(screen.getByRole("button", { name: "Up" }));
    await screen.findByRole("rowheader", { name: "D:\\" });
    expect(pathBox().value).toBe(THIS_PC);
    expect(screen.getByRole("rowheader", { name: "C:\\" })).toBeTruthy();
    // Nothing is above the drive list, and nothing can be made or sent from it.
    expect((screen.getByRole("button", { name: "Up" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "New folder" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Upload" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.queryByRole("button", { name: THIS_PC })).toBeNull();

    fireEvent.doubleClick(screen.getByRole("rowheader", { name: "D:\\" }));
    await waitFor(() => expect(pathBox().value).toBe("D:\\"));
  });

  it("jumps to This PC from the button, and reloads the list in place", async () => {
    const backend = windowsBackend();
    mount("local", backend, "C:\\Users\\Zoë Lee");
    await screen.findByRole("rowheader", { name: "Existing" });
    fireEvent.click(screen.getByRole("button", { name: THIS_PC }));
    await screen.findByRole("rowheader", { name: "C:\\" });
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(backend.listed.filter((p) => p === THIS_PC).length).toBe(2));
  });

  it("holds new folder names to Windows rules, case-insensitively for duplicates", async () => {
    mount("local", windowsBackend(), "C:\\Users\\Zoë Lee");
    await screen.findByRole("rowheader", { name: "Existing" });
    fireEvent.click(screen.getByRole("button", { name: "New folder" }));
    const dialog = await screen.findByRole("dialog");
    const field = within(dialog).getByLabelText("Folder name");
    const create = within(dialog).getByRole("button", { name: "Create" });
    const tryName = async (name: string, message: string) => {
      fireEvent.change(field, { target: { value: name } });
      fireEvent.click(create);
      expect((await within(dialog).findByRole("alert")).textContent).toBe(message);
    };
    await tryName("a:b", "A name can't contain < > : \" / \\ | ? or *.");
    await tryName("nope.", "A name can't end with a dot or a space.");
    await tryName("con.txt", "Windows reserves that name.");
    await tryName("existing", "Something with that name is already here.");
  });

  it("takes Windows mode from the platform, not from the path", async () => {
    // A Windows machine whose pane shows a Unix-shaped path (WSL-style) still gets the Windows rules.
    const backend: PaneBackend = { resolve: async (p) => p, list: async () => [], mkdir: vi.fn(async () => {}) };
    mount("local", backend, "/mnt/c");
    await screen.findByText("This folder is empty.");
    expect(screen.getByRole("button", { name: THIS_PC })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "New folder" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Folder name"), { target: { value: "con" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Create" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe("Windows reserves that name.");
    expect(backend.mkdir).not.toHaveBeenCalled();
  });

  it("has no This PC off Windows, even for a drive-shaped path", async () => {
    platform.windows = false;
    const backend: PaneBackend = { resolve: async (p) => p, list: async () => [], mkdir: vi.fn(async () => {}) };
    mount("local", backend, "C:\\Users");
    await screen.findByText("This folder is empty.");
    expect(screen.queryByRole("button", { name: THIS_PC })).toBeNull();
    // Up from a drive root is just the parent path logic: nothing above it.
    cleanup();
    mount("local", backend, "C:\\");
    await screen.findByText("This folder is empty.");
    expect((screen.getByRole("button", { name: "Up" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("leaves other names alone off Windows", async () => {
    platform.windows = false;
    const backend: PaneBackend = {
      resolve: async (p) => p,
      list: async () => [],
      mkdir: vi.fn(async () => {}),
    };
    mount("local", backend, "/home/me");
    await screen.findByText("This folder is empty.");
    expect(screen.queryByRole("button", { name: THIS_PC })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "New folder" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Folder name"), { target: { value: "con" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Create" }));
    await waitFor(() => expect(backend.mkdir).toHaveBeenCalledWith("/home/me/con"));
  });

  it("never applies Windows rules to the remote pane", async () => {
    const backend: PaneBackend = { resolve: async (p) => p, list: async () => [], mkdir: vi.fn(async () => {}) };
    mount("remote", backend, "C:\\odd");
    await screen.findByText("This folder is empty.");
    expect(screen.queryByRole("button", { name: THIS_PC })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "New folder" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Folder name"), { target: { value: "a:b" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Create" }));
    await waitFor(() => expect(backend.mkdir).toHaveBeenCalled());
  });
});

describe("Pane: a root it cannot leave (iOS, Tether's folder)", () => {
  const entry = (path: string, isDir = true): FileEntry => ({ ...folder(path), name: path.slice(path.lastIndexOf("/") + 1), isDir });
  const backend = (): PaneBackend => ({
    resolve: async (p) => p,
    list: async (p) => (p === "/docs" ? [entry("/docs/Projects"), entry("/docs/a.txt", false)] : []),
    mkdir: async () => {},
  });

  it("stops Up at the root, but not below it", async () => {
    mount("local", backend(), "/docs", { root: "/docs", title: "On My iPhone › Tether" });
    await screen.findByRole("rowheader", { name: "Projects" });
    expect(screen.getByRole("heading", { name: "On My iPhone › Tether" })).toBeTruthy();
    expect((screen.getByRole("button", { name: "Up" }) as HTMLButtonElement).disabled).toBe(true);

    expect(pathBox().value).toBe("/");

    fireEvent.doubleClick(screen.getByRole("rowheader", { name: "Projects" }));
    await waitFor(() => expect(pathBox().value).toBe("/Projects"));
    const up = screen.getByRole("button", { name: "Up" }) as HTMLButtonElement;
    expect(up.disabled).toBe(false);
    fireEvent.click(up);
    await waitFor(() => expect(pathBox().value).toBe("/"));
    expect((screen.getByRole("button", { name: "Up" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("shows the extra toolbar buttons it is given", async () => {
    const onImport = vi.fn();
    mount("local", backend(), "/docs", {
      root: "/docs",
      actions: <button type="button" onClick={onImport}>Import from Files</button>,
    });
    await screen.findByRole("rowheader", { name: "Projects" });
    fireEvent.click(within(screen.getByRole("toolbar", { name: "Local actions" })).getByRole("button", { name: "Import from Files" }));
    expect(onImport).toHaveBeenCalledOnce();
  });
});
