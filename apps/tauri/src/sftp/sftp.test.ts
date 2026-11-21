import { describe, expect, it } from "vitest";
import { createMockApi, MOCK_HOME, MOCK_REMOTE_HOME, type MockTetherApi } from "../api/mock";
import type { LocalEntry, RemoteEntry } from "../api/types";
import { standingAnswer } from "../prompts/PromptHost";
import { parseOctalMode } from "./dialogs";
import {
  baseName,
  escapeName,
  formatBytes,
  formatMode,
  driveEntries,
  isDriveRoot,
  joinPath,
  normalizePath,
  parentPath,
  shownPath,
  sortEntries,
  THIS_PC,
  typedPath,
} from "./paths";
import { MAX_DEPTH, planTransfer, startTransfer } from "./plan";
import { groupTransfers, TransferStore, transferStore } from "./transfers";

const entry = (path: string, over: Partial<RemoteEntry> = {}): LocalEntry & RemoteEntry => ({
  name: path.slice(path.lastIndexOf("/") + 1),
  path,
  isDir: false,
  size: 1,
  mtime: 1_700_000_000,
  mode: 0o100644,
  isSymlink: false,
  ...over,
});
const dir = (path: string) => entry(path, { isDir: true, size: 0, mode: 0o40755 });

describe("paths", () => {
  it("join_and_parent_keep_odd_names_verbatim", () => {
    expect(joinPath("remote", "/", "a b")).toBe("/a b");
    expect(joinPath("remote", "/srv/", 'q"uote')).toBe('/srv/q"uote');
    expect(joinPath("remote", "/srv", "line\nbreak")).toBe("/srv/line\nbreak");
    expect(joinPath("local", "C:\\Users\\me", "x y")).toBe("C:\\Users\\me\\x y");
    expect(joinPath("local", "/home/me", "日本語")).toBe("/home/me/日本語");
    expect(parentPath("remote", "/srv/www/")).toBe("/srv");
    expect(parentPath("remote", "/srv")).toBe("/");
    expect(parentPath("remote", "/")).toBeNull();
    expect(parentPath("local", "C:\\Users\\me")).toBe("C:\\Users");
    expect(parentPath("local", "C:\\Users")).toBe("C:\\");
    expect(parentPath("local", "C:\\")).toBeNull();
    expect(parentPath("local", "~")).toBeNull();
    expect(baseName("/srv/a b/")).toBe("a b");
    expect(baseName("C:\\x\\y.txt")).toBe("y.txt");
  });

  it("is_drive_root_only_for_a_bare_drive", () => {
    expect(isDriveRoot("C:\\")).toBe(true);
    expect(isDriveRoot("d:\\")).toBe(true);
    expect(isDriveRoot("C:/")).toBe(true);
    expect(isDriveRoot("C:")).toBe(true);
    expect(isDriveRoot("C:\\Users")).toBe(false);
    expect(isDriveRoot("C:\\Users\\")).toBe(false);
    expect(isDriveRoot("/")).toBe(false);
    expect(isDriveRoot("~")).toBe(false);
    expect(isDriveRoot("")).toBe(false);
    expect(isDriveRoot("\\\\server\\share\\")).toBe(false);
  });

  it("drive_entries_are_folders_named_by_their_root", () => {
    expect(driveEntries(["C:\\", "D:\\"])).toEqual([
      { name: "C:\\", path: "C:\\", isDir: true, size: 0, mtime: 0, mode: null, isSymlink: false },
      { name: "D:\\", path: "D:\\", isDir: true, size: 0, mtime: 0, mode: null, isSymlink: false },
    ]);
    // Going up from a drive root is the This PC list; from there, nowhere.
    expect(parentPath("local", THIS_PC)).toBeNull();
  });

  it("normalize_path_tidies_separators_and_dots", () => {
    expect(normalizePath("remote", "//srv//www/")).toBe("/srv/www");
    expect(normalizePath("remote", "/")).toBe("/");
    expect(normalizePath("remote", "///")).toBe("/");
    expect(normalizePath("remote", "/a/./b/../c/")).toBe("/a/c");
    expect(normalizePath("remote", "/../x")).toBe("/x");
    expect(normalizePath("remote", "a//b")).toBe("a/b");
    expect(normalizePath("remote", "../a")).toBe("../a");
    expect(normalizePath("remote", "")).toBe(".");
    expect(normalizePath("remote", "/srv/line\nbreak /")).toBe("/srv/line\nbreak ");
    expect(normalizePath("local", "/home/me/")).toBe("/home/me");
    expect(normalizePath("local", "C:\\Users\\me\\")).toBe("C:\\Users\\me");
    expect(normalizePath("local", "C:\\")).toBe("C:\\");
    // Done events and the pane compare equal after normalising.
    expect(normalizePath("remote", parentPath("remote", "/srv//notes.txt")!)).toBe("/srv");
  });

  it("escape_name_shows_line_breaks", () => {
    expect(escapeName("a\nb\tc\r")).toBe("a\\nb\\tc\\r");
    expect(escapeName("bell\u0007")).toBe("bell\\x07");
    expect(escapeName("plain ünï 日本")).toBe("plain ünï 日本");
  });

  it("formats_sizes_and_modes", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1023)).toBe("1023 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(5 * 1024 ** 3)).toBe("5.0 GB");
    expect(formatMode(0o100644)).toBe("-rw-r--r--");
    expect(formatMode(0o40755)).toBe("drwxr-xr-x");
    expect(formatMode(0o120777)).toBe("lrwxrwxrwx");
    expect(formatMode(0o104755)).toBe("-rwsr-xr-x");
    expect(formatMode(0o41777)).toBe("drwxrwxrwt");
    expect(formatMode(0)).toBe("—");
    expect(formatMode(null)).toBe("—");
  });

  it("sorts_folders_first_then_by_key", () => {
    const list = [
      entry("/a/b2.txt", { size: 30 }),
      entry("/a/B10.txt", { size: 10 }),
      dir("/a/zdir"),
      entry("/a/b1.txt", { size: 20 }),
    ];
    const names = (key: "name" | "size", d: "asc" | "desc") => sortEntries(list, key, d).map((e) => e.name);
    expect(names("name", "asc")).toEqual(["zdir", "b1.txt", "b2.txt", "B10.txt"]);
    expect(names("name", "desc")).toEqual(["zdir", "B10.txt", "b2.txt", "b1.txt"]);
    expect(names("size", "asc")).toEqual(["zdir", "B10.txt", "b1.txt", "b2.txt"]);
    expect(names("size", "desc")).toEqual(["zdir", "b2.txt", "b1.txt", "B10.txt"]);
  });

  it("parses_octal_modes", () => {
    expect(parseOctalMode("644")).toBe(0o644);
    expect(parseOctalMode(" 0755 ")).toBe(0o755);
    expect(parseOctalMode("4755")).toBe(0o4755);
    for (const bad of ["", "64", "999", "abc", "77777", "6 4 4", "-644"]) expect(parseOctalMode(bad)).toBeNull();
  });
});

async function setup(): Promise<{ api: MockTetherApi; hostId: string }> {
  const api = createMockApi();
  await api.vaultCreate("pw");
  const host = await api.upsertEntity("host", { label: "h", address: "a" });
  await api.sftpOpen(host.id);
  return { api, hostId: host.id };
}

describe("planTransfer", () => {
  it("upload_expands_a_folder_into_one_job_per_file", async () => {
    const { api, hostId } = await setup();
    const root = `${MOCK_HOME}/my docs`;
    for (const e of [
      dir(root),
      entry(`${root}/a "1".txt`),
      entry(`${root}/line\nbreak.txt`),
      dir(`${root}/sub dir`),
      entry(`${root}/sub dir/日本語.txt`),
      dir(`${root}/sub dir/empty`),
    ]) {
      api.control.addLocal(e);
    }
    api.control.addRemote(hostId, dir("/srv"));
    const plan = await planTransfer(api, hostId, "upload", [dir(root)], "/srv");
    expect(plan.problems).toEqual([]);
    // Files of a folder come before its sub-folders; paths are used as they are.
    expect(plan.jobs).toEqual([
      { local: `${root}/a "1".txt`, remote: '/srv/my docs/a "1".txt' },
      { local: `${root}/line\nbreak.txt`, remote: "/srv/my docs/line\nbreak.txt" },
      { local: `${root}/sub dir/日本語.txt`, remote: "/srv/my docs/sub dir/日本語.txt" },
    ]);
    // Every folder, even an empty one, exists at the destination.
    expect((await api.sftpList(hostId, "/srv")).map((e) => e.name)).toEqual(["my docs"]);
    expect((await api.sftpList(hostId, "/srv/my docs")).map((e) => e.name)).toEqual(["sub dir"]);
    expect((await api.sftpList(hostId, "/srv/my docs/sub dir")).map((e) => e.name)).toEqual(["empty"]);
  });

  it("download_creates_local_folders_and_reuses_existing_ones", async () => {
    const { api, hostId } = await setup();
    for (const e of [
      dir("/data"),
      entry("/data/x y.bin"),
      dir("/data/inner"),
      entry("/data/inner/z.txt"),
    ]) {
      api.control.addRemote(hostId, e);
    }
    api.control.addLocal(dir(`${MOCK_HOME}/dl`));
    api.control.addLocal(dir(`${MOCK_HOME}/dl/data`));
    const plan = await planTransfer(api, hostId, "download", [dir("/data")], `${MOCK_HOME}/dl`);
    expect(plan.problems).toEqual([]);
    expect(plan.jobs).toEqual([
      { local: `${MOCK_HOME}/dl/data/x y.bin`, remote: "/data/x y.bin" },
      { local: `${MOCK_HOME}/dl/data/inner/z.txt`, remote: "/data/inner/z.txt" },
    ]);
    expect((await api.localList(`${MOCK_HOME}/dl/data`)).map((e) => e.name)).toEqual(["inner"]);
  });

  it("windows_download_maps_remote_names_to_legal_local_names", async () => {
    const { api, hostId } = await setup();
    for (const e of [
      dir("/srv"),
      entry("/srv/a:b.txt"),
      entry("/srv/what?.log"),
      entry("/srv/con.txt"),
      entry("/srv/trailing. "),
      entry("/srv/Zoë Lee;1.txt"),
      dir("/srv/x|y"),
      entry("/srv/x|y/NUL"),
    ]) {
      api.control.addRemote(hostId, e);
    }
    api.control.addLocal(dir(`${MOCK_HOME}/dl`));
    const plan = await planTransfer(api, hostId, "download", [dir("/srv")], `${MOCK_HOME}/dl`, true);
    expect(plan.problems).toEqual([]);
    // The remote side keeps its names; only the local ones change.
    expect(plan.jobs).toEqual([
      { local: `${MOCK_HOME}/dl/srv/a_b.txt`, remote: "/srv/a:b.txt" },
      { local: `${MOCK_HOME}/dl/srv/_con.txt`, remote: "/srv/con.txt" },
      { local: `${MOCK_HOME}/dl/srv/trailing`, remote: "/srv/trailing. " },
      { local: `${MOCK_HOME}/dl/srv/what_.log`, remote: "/srv/what?.log" },
      { local: `${MOCK_HOME}/dl/srv/Zoë Lee;1.txt`, remote: "/srv/Zoë Lee;1.txt" },
      { local: `${MOCK_HOME}/dl/srv/x_y/_NUL`, remote: "/srv/x|y/NUL" },
    ]);
    // The folder was made under its local name.
    expect((await api.localList(`${MOCK_HOME}/dl/srv`)).map((e) => e.name)).toEqual(["x_y"]);
  });

  it("windows_download_of_a_single_odd_file_uses_the_mapped_name", async () => {
    const { api, hostId } = await setup();
    api.control.addRemote(hostId, entry("/a:b.txt"));
    const plan = await planTransfer(api, hostId, "download", [entry("/a:b.txt")], `${MOCK_HOME}/dl`, true);
    expect(plan.jobs).toEqual([{ local: `${MOCK_HOME}/dl/a_b.txt`, remote: "/a:b.txt" }]);
  });

  it("names_are_untouched_off_windows_and_for_uploads", async () => {
    const { api, hostId } = await setup();
    api.control.addRemote(hostId, entry("/a:b.txt"));
    const down = await planTransfer(api, hostId, "download", [entry("/a:b.txt")], `${MOCK_HOME}/dl`, false);
    expect(down.jobs).toEqual([{ local: `${MOCK_HOME}/dl/a:b.txt`, remote: "/a:b.txt" }]);
    // The local side's names are as they are; the remote accepts them all.
    api.control.addLocal(entry(`${MOCK_HOME}/con.txt`));
    api.control.addRemote(hostId, dir("/srv"));
    const up = await planTransfer(api, hostId, "upload", [entry(`${MOCK_HOME}/con.txt`)], "/srv", true);
    expect(up.jobs).toEqual([{ local: `${MOCK_HOME}/con.txt`, remote: "/srv/con.txt" }]);
  });

  it("windows_names_that_map_to_one_local_name_become_two_jobs_for_that_file", async () => {
    const { api, hostId } = await setup();
    for (const e of [dir("/srv"), entry("/srv/a:b"), entry("/srv/a_b")]) api.control.addRemote(hostId, e);
    const plan = await planTransfer(api, hostId, "download", [dir("/srv")], `${MOCK_HOME}/dl`, true);
    // Same destination: the transfer's own conflict handling (ask) decides at run time.
    expect(plan.jobs.map((j) => j.local)).toEqual([`${MOCK_HOME}/dl/srv/a_b`, `${MOCK_HOME}/dl/srv/a_b`]);
    expect(plan.jobs.map((j) => j.remote).sort()).toEqual(["/srv/a:b", "/srv/a_b"]);
  });

  it("existing_remote_folder_is_reused", async () => {
    const { api, hostId } = await setup();
    api.control.addLocal(dir(`${MOCK_HOME}/d`));
    api.control.addLocal(entry(`${MOCK_HOME}/d/f.txt`));
    api.control.addRemote(hostId, dir("/srv"));
    api.control.addRemote(hostId, dir("/srv/d"));
    api.control.addRemote(hostId, entry("/srv/d/keep.txt"));
    const plan = await planTransfer(api, hostId, "upload", [dir(`${MOCK_HOME}/d`)], "/srv");
    expect(plan.problems).toEqual([]);
    expect(plan.jobs).toEqual([{ local: `${MOCK_HOME}/d/f.txt`, remote: "/srv/d/f.txt" }]);
    expect((await api.sftpList(hostId, "/srv/d")).map((e) => e.name)).toEqual(["keep.txt"]);
  });

  it("links_to_folders_inside_a_folder_are_not_followed", async () => {
    const { api, hostId } = await setup();
    api.control.addRemote(hostId, dir("/data"));
    api.control.addRemote(hostId, entry("/data/real.txt"));
    // A remote link reports its own attributes; one with children leads to a folder.
    api.control.addRemote(hostId, entry("/data/loop", { isSymlink: true, mode: 0o120777 }));
    api.control.addRemote(hostId, entry("/data/loop/inside.txt"));
    api.control.addRemote(hostId, entry("/data/file-link", { isSymlink: true, mode: 0o120777 }));
    api.control.addLocal(dir(`${MOCK_HOME}/out`));
    const plan = await planTransfer(api, hostId, "download", [dir("/data")], `${MOCK_HOME}/out`);
    expect(plan.jobs.map((j) => j.remote)).toEqual(["/data/file-link", "/data/real.txt"]);
    expect(plan.problems).toHaveLength(1);
    expect(plan.problems[0]).toContain("/data/loop");

    // Dragged itself, a link to a folder is a folder.
    const top = await planTransfer(
      api,
      hostId,
      "download",
      [entry("/data/loop", { isSymlink: true, mode: 0o120777 })],
      `${MOCK_HOME}/out`,
    );
    expect(top.jobs).toEqual([{ local: `${MOCK_HOME}/out/loop/inside.txt`, remote: "/data/loop/inside.txt" }]);
  });

  it("sockets_and_pipes_are_left_out_and_reported", async () => {
    const { api, hostId } = await setup();
    api.control.addLocal(dir(`${MOCK_HOME}/d`));
    api.control.addLocal(entry(`${MOCK_HOME}/d/ok.txt`));
    api.control.addLocal(entry(`${MOCK_HOME}/d/pipe`, { mode: 0o010644 }));
    api.control.addRemote(hostId, dir("/srv"));
    const plan = await planTransfer(
      api,
      hostId,
      "upload",
      [dir(`${MOCK_HOME}/d`), entry(`${MOCK_HOME}/d/pipe`, { mode: 0o140755 })],
      "/srv",
    );
    expect(plan.jobs).toEqual([{ local: `${MOCK_HOME}/d/ok.txt`, remote: "/srv/d/ok.txt" }]);
    expect(plan.problems).toHaveLength(2);
    expect(plan.problems.every((p) => p.includes("not a regular file"))).toBe(true);
  });

  it("unreadable_folder_is_reported_and_the_rest_goes_on", async () => {
    const { api, hostId } = await setup();
    api.control.addLocal(dir(`${MOCK_HOME}/gone`));
    api.control.addRemote(hostId, dir("/srv"));
    api.control.addLocal(entry(`${MOCK_HOME}/ok.txt`));
    // The folder entry exists in the dragged list but was removed meanwhile.
    const plan = await planTransfer(
      api,
      hostId,
      "upload",
      [dir(`${MOCK_HOME}/missing`), entry(`${MOCK_HOME}/ok.txt`)],
      "/srv",
    );
    expect(plan.jobs).toEqual([{ local: `${MOCK_HOME}/ok.txt`, remote: "/srv/ok.txt" }]);
    expect(plan.problems).toHaveLength(1);
    expect(plan.problems[0]).toContain("Could not read");
  });

  it("stops_at_the_depth_limit", async () => {
    const { api, hostId } = await setup();
    let path = `${MOCK_HOME}/deep`;
    api.control.addLocal(dir(path));
    for (let i = 0; i < MAX_DEPTH + 2; i++) {
      path += "/d";
      api.control.addLocal(dir(path));
      api.control.addLocal(entry(`${path}/f.txt`));
    }
    api.control.addRemote(hostId, dir("/srv"));
    const plan = await planTransfer(api, hostId, "upload", [dir(`${MOCK_HOME}/deep`)], "/srv");
    expect(plan.jobs).toHaveLength(MAX_DEPTH);
    expect(plan.problems).toHaveLength(1);
  });
});

describe("startTransfer and the transfer store", () => {
  it("jobs_are_queued_with_policy_ask_in_one_batch", async () => {
    const { api, hostId } = await setup();
    const store = new TransferStore();
    api.control.addLocal(entry(`${MOCK_HOME}/a.txt`));
    api.control.addLocal(entry(`${MOCK_HOME}/b c.txt`));
    api.control.addRemote(hostId, dir("/srv"));
    const result = await startTransfer(
      api,
      store,
      hostId,
      "upload",
      [entry(`${MOCK_HOME}/a.txt`), entry(`${MOCK_HOME}/b c.txt`)],
      "/srv",
    );
    expect(result).toEqual({ queued: 2, problems: [] });
    const queued = api.control.enqueued();
    expect(queued.map((q) => q.job)).toEqual([
      { direction: "upload", local: `${MOCK_HOME}/a.txt`, remote: "/srv/a.txt", policy: "ask" },
      { direction: "upload", local: `${MOCK_HOME}/b c.txt`, remote: "/srv/b c.txt", policy: "ask" },
    ]);
    const items = store.getSnapshot();
    expect(items.map((i) => [i.name, i.status])).toEqual([
      ["a.txt", "queued"],
      ["b c.txt", "queued"],
    ]);
    expect(new Set(items.map((i) => i.batchId)).size).toBe(1);
    expect(items[0].batchId).not.toBeNull();
  });

  it("windows_download_rows_show_the_local_name", async () => {
    const { api, hostId } = await setup();
    const store = new TransferStore();
    api.control.addRemote(hostId, dir("/srv"));
    api.control.addRemote(hostId, entry("/srv/a:b.txt"));
    api.control.addRemote(hostId, dir("/srv/p?q"));
    api.control.addRemote(hostId, entry("/srv/p?q/c.txt"));
    api.control.addLocal(dir(`${MOCK_HOME}/dl`));
    const odd = entry("/srv/a:b.txt");
    const result = await startTransfer(api, store, hostId, "download", [odd], `${MOCK_HOME}/dl`, true);
    expect(result).toEqual({ queued: 1, problems: [] });
    expect(api.control.enqueued().map((q) => q.job)).toEqual([
      { direction: "download", local: `${MOCK_HOME}/dl/a_b.txt`, remote: "/srv/a:b.txt", policy: "ask" },
    ]);
    expect(store.getSnapshot().map((i) => i.name)).toEqual(["a_b.txt"]);
    // A dragged folder is labelled by its local name too.
    const folder = await startTransfer(api, store, hostId, "download", [dir("/srv/p?q")], `${MOCK_HOME}/dl`, true);
    expect(folder.queued).toBe(1);
    const batch = store.getSnapshot()[1].batchId!;
    expect(store.batchLabel(batch)).toBe("p_q");
  });

  it("cancelling_while_queuing_stops_the_rest", async () => {
    for (const how of ["batch", "all"] as const) {
      const { api, hostId } = await setup();
      const store = new TransferStore();
      const files = ["a", "b", "c", "d", "e"].map((n) => entry(`${MOCK_HOME}/${n}.txt`));
      for (const f of files) api.control.addLocal(f);
      api.control.addRemote(hostId, dir("/srv"));
      // The user cancels once the second file is queued.
      const enqueue = api.transferEnqueue.bind(api);
      api.transferEnqueue = async (h, job) => {
        const r = await enqueue(h, job);
        if (api.control.enqueued().length === 2) {
          if (how === "batch") store.cancelBatch(store.getSnapshot()[0].batchId!);
          else store.stopQueuing();
        }
        return r;
      };
      const result = await startTransfer(api, store, hostId, "upload", files, "/srv");
      expect(result, how).toEqual({ queued: 2, problems: [] });
      expect(api.control.enqueued()).toHaveLength(2);
    }
  });

  it("queue_failure_is_reported_once", async () => {
    const { api, hostId } = await setup();
    const store = new TransferStore();
    await api.sftpClose(hostId);
    const result = await startTransfer(api, store, hostId, "upload", [entry(`${MOCK_HOME}/a.txt`)], "/srv");
    expect(result.queued).toBe(0);
    expect(result.problems).toHaveLength(1);
    expect(result.problems[0]).toContain("Could not queue");
  });

  it("events_update_rows_even_when_they_arrive_before_the_job_is_registered", () => {
    const store = new TransferStore();
    const hostId = "h1";
    store.apply({ hostId, event: { kind: "progress", job: "j1", bytes: 5, total: 10 } });
    store.register({ jobId: "j1", hostId, batchId: "b", direction: "download", local: "/l/x y", remote: "/r/x y" });
    expect(store.getSnapshot()).toMatchObject([{ name: "x y", status: "running", bytes: 5, total: 10, batchId: "b" }]);
    store.apply({ hostId, event: { kind: "done", job: "j1", dest: "/l/x" } });
    // Nothing follows a final event.
    store.apply({ hostId, event: { kind: "progress", job: "j1", bytes: 1, total: 10 } });
    expect(store.getSnapshot()[0]).toMatchObject({ status: "done", dest: "/l/x", bytes: 10 });
    expect(store.hasLive(hostId)).toBe(false);
  });

  it("done_events_reach_listeners_and_clear_finished_keeps_live_rows", () => {
    const store = new TransferStore();
    const done: string[] = [];
    store.onDone((d) => done.push(d.dest));
    store.register({ jobId: "a", hostId: "h", batchId: null, direction: "upload", local: "/l/a", remote: "/r/a" });
    store.register({ jobId: "b", hostId: "h", batchId: null, direction: "upload", local: "/l/b", remote: "/r/b" });
    store.apply({ hostId: "h", event: { kind: "done", job: "a", dest: "/r/a (1)" } });
    store.apply({ hostId: "h", event: { kind: "progress", job: "b", bytes: 1, total: 2 } });
    expect(done).toEqual(["/r/a (1)"]);
    expect(store.hasLive("h")).toBe(true);
    store.clearFinished();
    expect(store.getSnapshot().map((i) => i.jobId)).toEqual(["b"]);
  });

  it("apply_to_all_answers_the_batch_only", () => {
    const store = new TransferStore();
    const meta = (jobId: string, batchId: string) => ({
      jobId,
      hostId: "h",
      batchId,
      direction: "upload" as const,
      local: `/l/${jobId}`,
      remote: `/r/${jobId}`,
    });
    store.register(meta("a", "batch1"));
    store.register(meta("b", "batch1"));
    store.register(meta("c", "batch2"));
    expect(store.autoAnswer("b")).toBeNull();
    store.rememberForBatch("a", "skip");
    expect(store.autoAnswer("b")).toBe("skip");
    expect(store.autoAnswer("c")).toBeNull();
    expect(store.autoAnswer("unknown")).toBeNull();

    // A choice made before the job's batch is known applies once it is.
    store.rememberForBatch("d", "overwrite");
    store.register(meta("d", "batch3"));
    store.register(meta("e", "batch3"));
    expect(store.autoAnswer("e")).toBe("overwrite");

    // A batch with no rows left forgets its answer.
    store.apply({ hostId: "h", event: { kind: "done", job: "a", dest: "/r/a" } });
    store.apply({ hostId: "h", event: { kind: "done", job: "b", dest: "/r/b" } });
    store.clearFinished();
    store.register(meta("f", "batch1"));
    expect(store.autoAnswer("f")).toBeNull();
  });
});

describe("transfer groups and cancel all", () => {
  const meta = (jobId: string, batchId: string | null, local = `/l/${jobId}`) => ({
    jobId,
    hostId: "h",
    batchId,
    direction: "upload" as const,
    local,
    remote: `/r/${jobId}`,
  });

  it("groups_batches_of_many", () => {
    const store = new TransferStore();
    store.nameBatch("b", "photos");
    store.register(meta("a1", "b"));
    store.register(meta("s1", "single"));
    store.register(meta("a2", "b"));
    store.register(meta("n1", null));
    store.apply({ hostId: "h", event: { kind: "done", job: "a1", dest: "/r/a1" } });
    const rows = groupTransfers(store.getSnapshot(), (b) => store.batchLabel(b));
    expect(rows.map((r) => (r.kind === "batch" ? `batch:${r.label}:${r.items.length}` : `job:${r.item.jobId}`))).toEqual([
      "batch:photos:2",
      "job:s1",
      "job:n1",
    ]);
    const batch = rows[0];
    expect(batch.kind === "batch" && batch.direction).toBe("upload");
    // An unnamed batch is named by its size.
    store.register(meta("c1", "c"));
    store.register(meta("c2", "c"));
    const all = groupTransfers(store.getSnapshot(), (b) => store.batchLabel(b));
    const unnamed = all[all.length - 1];
    expect(unnamed?.kind === "batch" && unnamed.label).toBe("2 files");
  });

  it("live_job_ids", () => {
    const store = new TransferStore();
    store.register(meta("a", "b"));
    store.register(meta("b", "b"));
    store.register(meta("c", null));
    store.apply({ hostId: "h", event: { kind: "progress", job: "b", bytes: 1, total: 2 } });
    store.apply({ hostId: "h", event: { kind: "cancelled", job: "c" } });
    expect(store.liveJobIds()).toEqual(["a", "b"]);
    expect(store.liveJobIds("b")).toEqual(["a", "b"]);
    expect(store.liveJobIds("other")).toEqual([]);
  });
});

describe("conflict answers that apply to a whole batch", () => {
  const conflict = (jobId: string, existingSize: number, incomingSize: number) => ({
    id: `p-${jobId}-${existingSize}`,
    prompt: { kind: "transferConflict" as const, jobId, path: "/r/x", existingSize, incomingSize },
  });
  const meta = (jobId: string, batchId: string) => ({
    jobId,
    hostId: "h",
    batchId,
    direction: "upload" as const,
    local: `/l/${jobId}`,
    remote: `/r/${jobId}`,
  });

  it("a_remembered_resume_is_not_applied_where_it_cannot_resume", () => {
    transferStore.register(meta("batch-job-a", "batch-resume"));
    transferStore.register(meta("batch-job-b", "batch-resume"));
    expect(standingAnswer(conflict("batch-job-b", 1, 2))).toBeNull();
    transferStore.rememberForBatch("batch-job-a", "resume");
    expect(standingAnswer(conflict("batch-job-b", 1, 2))).toEqual({ conflict: "resume" });
    // Not smaller than the incoming file: asked, not answered.
    expect(standingAnswer(conflict("batch-job-b", 2, 2))).toBeNull();
    expect(standingAnswer(conflict("batch-job-b", 5, 2))).toBeNull();
    // Other choices hold whatever the sizes.
    transferStore.rememberForBatch("batch-job-a", "skip");
    expect(standingAnswer(conflict("batch-job-b", 5, 2))).toEqual({ conflict: "skip" });
    transferStore.rememberForBatch("batch-job-a", "overwrite");
    expect(standingAnswer(conflict("batch-job-b", 5, 2))).toEqual({ conflict: "overwrite" });
    // Other prompts and unknown jobs are never answered.
    expect(standingAnswer(conflict("nobody", 1, 2))).toBeNull();
    expect(standingAnswer({ id: "x", prompt: { kind: "passphrase", keyLabel: "k" } })).toBeNull();
    transferStore.apply({ hostId: "h", event: { kind: "cancelled", job: "batch-job-a" } });
    transferStore.apply({ hostId: "h", event: { kind: "cancelled", job: "batch-job-b" } });
    transferStore.clearFinished();
  });

  it("a_done_before_registration_is_announced_with_the_real_direction", () => {
    const store = new TransferStore();
    const done: { direction: string; dest: string }[] = [];
    store.onDone((d) => done.push(d));
    store.apply({ hostId: "h", event: { kind: "progress", job: "j", bytes: 1, total: 2 } });
    store.apply({ hostId: "h", event: { kind: "done", job: "j", dest: "/l/dl/x" } });
    // Nothing yet: the placeholder's direction would be a guess.
    expect(done).toEqual([]);
    store.register({ ...meta("j", "b"), direction: "download" });
    expect(done).toEqual([{ direction: "download", hostId: "h", dest: "/l/dl/x" }]);
    // A registered job's done is announced once, at once.
    store.register(meta("k", "b"));
    store.apply({ hostId: "h", event: { kind: "done", job: "k", dest: "/r/k" } });
    expect(done).toHaveLength(2);
  });
});

describe("paths that reach jobs are resolved", () => {
  it("mock_resolves_home_and_server_paths", async () => {
    const api = createMockApi();
    await api.vaultCreate("pw");
    const host = await api.upsertEntity("host", { label: "h", address: "a" });
    await api.sftpOpen(host.id);
    expect(await api.localResolve("~")).toBe(MOCK_HOME);
    expect(await api.localResolve("~/x y")).toBe(`${MOCK_HOME}/x y`);
    expect(await api.localResolve("/a/~b")).toBe("/a/~b");
    expect(await api.sftpRealpath(host.id, ".")).toBe(MOCK_REMOTE_HOME);
    expect(await api.sftpRealpath(host.id, "sub/../x")).toBe(`${MOCK_REMOTE_HOME}/x`);
    expect(await api.sftpRealpath(host.id, "/a//b/../c/")).toBe("/a/c");
  });
});

describe("local file system in the mock", () => {
  it("lists_folders_first_and_expands_the_home_shortcut", async () => {
    const api = createMockApi();
    api.control.addLocal(entry(`${MOCK_HOME}/b.txt`));
    api.control.addLocal(dir(`${MOCK_HOME}/zdir`));
    api.control.addLocal(entry(`${MOCK_HOME}/A.txt`));
    expect((await api.localList("~")).map((e) => e.name)).toEqual(["zdir", "A.txt", "b.txt"]);
    await expect(api.localList("/nowhere")).rejects.toMatchObject({ code: "io" });
    await api.localMkdir("/x/y z");
    expect((await api.localList("/x")).map((e) => e.name)).toEqual(["y z"]);
  });

  it("lists_the_root_unless_a_seed_has_drives", async () => {
    expect(await createMockApi().localDrives()).toEqual(["/"]);
    expect(await createMockApi({ localDrives: ["C:\\", "D:\\"] }).localDrives()).toEqual(["C:\\", "D:\\"]);
  });
});

describe("shownPath / typedPath (a pane confined to a root)", () => {
  const root = "/private/var/mobile/Containers/Data/Application/ABC/Documents";

  it("shows the root as / and folders under it from there", () => {
    expect(shownPath(root, root)).toBe("/");
    expect(shownPath(`${root}/Projects/a`, root)).toBe("/Projects/a");
    expect(shownPath(`${root}x/y`, root)).toBe(`${root}x/y`);
    expect(shownPath("/home/alice", undefined)).toBe("/home/alice");
  });

  it("reads typed paths as under the root", () => {
    expect(typedPath("/", root)).toBe(root);
    expect(typedPath("/Projects", root)).toBe(`${root}/Projects`);
    expect(typedPath("Projects/a", root)).toBe(`${root}/Projects/a`);
    expect(typedPath("~/x", root)).toBe("~/x");
    expect(typedPath("/etc", undefined)).toBe("/etc");
  });
});
