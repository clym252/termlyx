import type { Locator, Page } from "@playwright/test";
import type { LocalEntry, RemoteEntry } from "../src/api/types";
import { expect, test } from "./fixtures";
import { createVault, goTo, seedHost } from "./helpers";

const HOME = "/home/mock";
const REMOTE_HOME = "/home/deploy";
const MTIME = 1_700_000_000;

type Seed = { local?: LocalEntry[]; remote?: RemoteEntry[] };

const dir = (path: string): LocalEntry & RemoteEntry => ({
  name: path.slice(path.lastIndexOf("/") + 1),
  path,
  isDir: true,
  size: 0,
  mtime: MTIME,
  mode: 0o40755,
  isSymlink: false,
});
const file = (path: string, size = 0, mode = 0o100644): LocalEntry & RemoteEntry => ({
  ...dir(path),
  isDir: false,
  size,
  mode,
});
const link = (path: string, size = 0): LocalEntry & RemoteEntry => ({
  ...file(path, size, 0o120777),
  isSymlink: true,
});

/** Fills the mock's local and remote file systems. */
async function seed(page: Page, hostId: string, data: Seed) {
  await page.evaluate(
    ({ hostId, data }) => {
      const control = window.__tetherMock.control;
      for (const e of data.local ?? []) control.addLocal(e);
      for (const e of data.remote ?? []) control.addRemote(hostId, e);
    },
    { hostId, data },
  );
}

const localPane = (page: Page) => page.getByRole("region", { name: "Local files" });
const remotePane = (page: Page) => page.getByRole("region", { name: "Remote files" });
const pathOf = (pane: Locator) => pane.getByRole("textbox", { name: "Path", exact: true });
/** The row of an entry, by its displayed name. */
const rowOf = (pane: Locator, name: string) =>
  pane.getByRole("row").filter({ has: pane.page().getByRole("rowheader", { name, exact: true }) });
const names = (pane: Locator) => pane.getByRole("rowheader").allInnerTexts();
const transfers = (page: Page) => page.getByRole("region", { name: "Transfers" });

/** Opens the SFTP page on the host called `label`. */
async function openSftp(page: Page, label = "web") {
  await goTo(page, "SFTP");
  await expect(page.getByRole("heading", { name: "SFTP", level: 1 })).toBeVisible();
  await page.getByLabel("Host", { exact: true }).selectOption({ label });
  await expect(remotePane(page)).toBeVisible();
  await expect(localPane(page)).toBeVisible();
}

/** The remote entries under `path`, as the mock serves them. */
function remoteNames(page: Page, hostId: string, path: string): Promise<string[]> {
  return page.evaluate(
    async ({ hostId, path }) => (await window.__tetherMock.sftpList(hostId, path)).map((e) => e.name),
    { hostId, path },
  );
}

test("browse_local_and_remote", async ({ page }) => {
  await createVault(page);
  const hostId = await seedHost(page, "web", "10.0.0.1");
  await seed(page, hostId, {
    local: [
      dir(`${HOME}/docs`),
      dir(`${HOME}/empty`),
      file(`${HOME}/notes.txt`, 1234),
      file(`${HOME}/my file.txt`, 5),
    ],
    remote: [
      dir(`${REMOTE_HOME}/projects`),
      file(`${REMOTE_HOME}/projects/todo.txt`, 3),
      dir("/srv"),
      dir("/srv/www"),
      file("/srv/www/app.js", 10),
      link("/srv/current", 5),
      file("/srv/current/index.html", 7),
      file('/srv/report "final".pdf', 300),
      file("/srv/line\nbreak.txt", 10),
      file("/srv/ünï 日本.txt", 20),
    ],
  });

  await openSftp(page);
  const local = localPane(page);
  const remote = remotePane(page);

  // The local pane starts in the home directory, folders first.
  await expect(pathOf(local)).toHaveValue(HOME);
  await expect.poll(() => names(local)).toEqual(["docs", "empty", "my file.txt", "notes.txt"]);
  await expect(rowOf(local, "docs").getByRole("img", { name: "Folder" })).toBeVisible();
  await expect(rowOf(local, "notes.txt")).toContainText("1.2 KB");

  // The remote pane starts in the login directory; the server resolves a relative path.
  await expect(pathOf(remote)).toHaveValue(REMOTE_HOME);
  await expect.poll(() => names(remote)).toEqual(["projects"]);
  await pathOf(remote).fill("projects");
  await pathOf(remote).press("Enter");
  await expect(pathOf(remote)).toHaveValue(`${REMOTE_HOME}/projects`);
  await expect.poll(() => names(remote)).toEqual(["todo.txt"]);

  // Folders open on double-click.
  await pathOf(remote).fill("/");
  await pathOf(remote).press("Enter");
  await expect(pathOf(remote)).toHaveValue("/");
  await expect.poll(() => names(remote)).toEqual(["srv"]);
  await rowOf(remote, "srv").dblclick();
  await expect(pathOf(remote)).toHaveValue("/srv");
  // Odd names are listed as they are, a line break shown as \n.
  await expect
    .poll(() => names(remote))
    .toEqual(["www", "current", "line\\nbreak.txt", 'report "final".pdf', "ünï 日本.txt"]);

  // A symbolic link looks different, and one to a folder opens like a folder.
  await expect(rowOf(remote, "current").getByRole("img", { name: "Symbolic link" })).toBeVisible();
  await rowOf(remote, "current").dblclick();
  await expect(pathOf(remote)).toHaveValue("/srv/current");
  await expect.poll(() => names(remote)).toEqual(["index.html"]);
  await remote.getByRole("button", { name: "Up", exact: true }).click();
  await expect(pathOf(remote)).toHaveValue("/srv");

  // A link to a file is just a file: double-clicking it stays put.
  await seed(page, hostId, { remote: [link("/srv/readme-link", 3)] });
  await remote.getByRole("button", { name: "Refresh", exact: true }).click();
  await rowOf(remote, "readme-link").dblclick();
  await expect(pathOf(remote)).toHaveValue("/srv");
  await expect(remote.getByRole("alert")).toHaveCount(0);

  // Sorting by size; folders stay first.
  const sizeHeader = remote.getByRole("columnheader", { name: "Size" });
  await sizeHeader.getByRole("button", { name: "Size" }).click();
  await expect(sizeHeader).toHaveAttribute("aria-sort", "ascending");
  await expect
    .poll(() => names(remote))
    .toEqual(["www", "readme-link", "current", "line\\nbreak.txt", "ünï 日本.txt", 'report "final".pdf']);
  await sizeHeader.getByRole("button", { name: "Size" }).click();
  await expect(sizeHeader).toHaveAttribute("aria-sort", "descending");
  await expect
    .poll(() => names(remote))
    .toEqual(["www", 'report "final".pdf', "ünï 日本.txt", "line\\nbreak.txt", "current", "readme-link"]);

  // The path bar takes a path; a bad one is reported and changes nothing.
  // Doubled and trailing slashes are tidied, so the bar always shows the plain path.
  await pathOf(remote).fill("//srv//www/");
  await pathOf(remote).press("Enter");
  await expect(pathOf(remote)).toHaveValue("/srv/www");
  await expect.poll(() => names(remote)).toEqual(["app.js"]);
  await pathOf(remote).fill("/nowhere");
  await pathOf(remote).press("Enter");
  await expect(remote.getByRole("alert")).toContainText("no such file");
  await expect(pathOf(remote)).toHaveValue("/srv/www");
  await expect.poll(() => names(remote)).toEqual(["app.js"]);

  // `~` is expanded even where the folder is empty: the bar never shows (or sends) a `~`.
  await pathOf(local).fill("~/empty");
  await pathOf(local).press("Enter");
  await expect(pathOf(local)).toHaveValue(`${HOME}/empty`);
  await expect(local.getByText("This folder is empty.")).toBeVisible();
  await pathOf(local).fill("~");
  await pathOf(local).press("Enter");
  await expect(pathOf(local)).toHaveValue(HOME);

  // Local navigation goes down and up.
  await rowOf(local, "docs").dblclick();
  await expect(pathOf(local)).toHaveValue(`${HOME}/docs`);
  await local.getByRole("button", { name: "Up", exact: true }).click();
  await expect(pathOf(local)).toHaveValue(HOME);
});

test("drag_file_uploads_with_progress", async ({ page }) => {
  await createVault(page);
  const hostId = await seedHost(page, "web", "10.0.0.1");
  await seed(page, hostId, {
    local: [
      file(`${HOME}/notes.txt`, 1024),
      dir(`${HOME}/empty`),
      dir(`${HOME}/my docs`),
      file(`${HOME}/my docs/a "1".txt`, 3),
      dir(`${HOME}/my docs/sub dir`),
      file(`${HOME}/my docs/sub dir/b.txt`, 4),
    ],
    remote: [dir("/srv"), file("/srv/existing.txt", 1)],
  });
  await openSftp(page);
  const local = localPane(page);
  const remote = remotePane(page);
  await pathOf(remote).fill("/srv");
  await pathOf(remote).press("Enter");
  await expect.poll(() => names(remote)).toEqual(["existing.txt"]);

  // Dragging a file onto the remote pane uploads it into the shown folder.
  await rowOf(local, "notes.txt").dragTo(remote);
  const jobs = () => page.evaluate(() => window.__tetherMock.control.enqueued());
  await expect.poll(async () => (await jobs()).length).toBe(1);
  const [upload] = await jobs();
  expect(upload.hostId).toBe(hostId);
  expect(upload.job).toEqual({
    direction: "upload",
    local: `${HOME}/notes.txt`,
    remote: "/srv/notes.txt",
    policy: "ask",
  });

  // It is queued until the backend reports progress.
  const bar = transfers(page);
  const item = bar.getByRole("listitem").filter({ hasText: "notes.txt" });
  await expect(item).toContainText("Queued");
  await page.evaluate(
    ({ hostId, job }) =>
      window.__tetherMock.control.emitTransfer({ hostId, event: { kind: "progress", job, bytes: 512, total: 1024 } }),
    { hostId, job: upload.jobId },
  );
  await expect(item).toContainText("512 B of 1.0 KB (50%)");
  await expect(item.getByRole("progressbar", { name: "notes.txt" })).toHaveJSProperty("value", 512);

  // Done: the remote listing picks the new file up.
  await seed(page, hostId, { remote: [file("/srv/notes.txt", 1024)] });
  await page.evaluate(
    ({ hostId, job }) =>
      window.__tetherMock.control.emitTransfer({ hostId, event: { kind: "done", job, dest: "/srv/notes.txt" } }),
    { hostId, job: upload.jobId },
  );
  await expect(item).toContainText("Done");
  await expect.poll(() => names(remote)).toEqual(["existing.txt", "notes.txt"]);

  // Clear finished removes it from the list.
  await bar.getByRole("button", { name: "Clear finished" }).click();
  await expect(bar.getByRole("listitem")).toHaveCount(0);

  // A folder expands into one job per file, creating the folders it needs.
  await rowOf(local, "my docs").dragTo(remote);
  await expect.poll(async () => (await jobs()).length).toBe(3);
  const folderJobs = (await jobs()).slice(1).map((j) => j.job);
  expect(folderJobs).toEqual([
    { direction: "upload", local: `${HOME}/my docs/a "1".txt`, remote: '/srv/my docs/a "1".txt', policy: "ask" },
    { direction: "upload", local: `${HOME}/my docs/sub dir/b.txt`, remote: "/srv/my docs/sub dir/b.txt", policy: "ask" },
  ]);
  expect(await remoteNames(page, hostId, "/srv")).toContain("my docs");
  expect(await remoteNames(page, hostId, "/srv/my docs")).toEqual(['sub dir']);

  // A folder's jobs are one row; its files show on demand.
  await expect(bar.getByRole("listitem").filter({ hasText: "my docs" })).toContainText("0 of 2 files done");
  await expect(bar.getByRole("listitem").filter({ hasText: "b.txt" })).toHaveCount(0);
  await bar.getByRole("button", { name: "Show files of my docs" }).click();

  // A queued job can be cancelled.
  const second = bar.getByRole("listitem").filter({ hasText: "b.txt" });
  await expect(second).toContainText("Queued");
  await second.getByRole("button", { name: "Cancel b.txt" }).click();
  await expect(second).toContainText("Cancelled");
  await expect(second.getByRole("button", { name: "Cancel b.txt" })).toHaveCount(0);
  const active = await page.evaluate(async () => (await window.__tetherMock.activeWork()).transfers.map((t) => t.remote));
  expect(active).toEqual(['/srv/my docs/a "1".txt']);

  // A failure keeps its reason.
  const [, first] = await jobs();
  await page.evaluate(
    ({ hostId, job }) =>
      window.__tetherMock.control.emitTransfer({ hostId, event: { kind: "failed", job, error: "disk full" } }),
    { hostId, job: first.jobId },
  );
  await expect(bar.getByRole("listitem").filter({ hasText: 'a "1".txt' })).toContainText("Failed: disk full");

  // A download into a folder reached through `~` (empty) gets an absolute local path.
  await pathOf(local).fill("~/empty");
  await pathOf(local).press("Enter");
  await expect(pathOf(local)).toHaveValue(`${HOME}/empty`);
  await rowOf(remote, "existing.txt").click();
  await remote.getByRole("button", { name: "Download" }).click();
  await expect.poll(async () => (await jobs()).length).toBe(4);
  expect((await jobs())[3].job).toEqual({
    direction: "download",
    local: `${HOME}/empty/existing.txt`,
    remote: "/srv/existing.txt",
    policy: "ask",
  });
  expect((await jobs()).every((j) => !j.job.local.includes("~"))).toBe(true);
});

test("windows_download_lands_under_a_legal_local_name", async ({ page }) => {
  // The page reports a Windows `navigator.platform` before any app code runs.
  await page.addInitScript(() => {
    Object.defineProperty(Navigator.prototype, "platform", { get: () => "Win32", configurable: true });
  });
  await createVault(page);
  const hostId = await seedHost(page, "web", "10.0.0.1");
  await seed(page, hostId, {
    local: [dir(`${HOME}/dl`)],
    remote: [dir("/srv"), file("/srv/a:b.txt", 5), file("/srv/ok name.txt", 5)],
  });
  await openSftp(page);
  const local = localPane(page);
  const remote = remotePane(page);
  await pathOf(remote).fill("/srv");
  await pathOf(remote).press("Enter");
  await expect.poll(() => names(remote)).toEqual(["a:b.txt", "ok name.txt"]);
  await pathOf(local).fill(`${HOME}/dl`);
  await pathOf(local).press("Enter");
  await expect(pathOf(local)).toHaveValue(`${HOME}/dl`);

  await rowOf(remote, "a:b.txt").click();
  await remote.getByRole("button", { name: "Download" }).click();
  const jobs = () => page.evaluate(() => window.__tetherMock.control.enqueued());
  await expect.poll(async () => (await jobs()).length).toBe(1);
  expect((await jobs())[0].job).toEqual({
    direction: "download",
    local: `${HOME}/dl/a_b.txt`,
    remote: "/srv/a:b.txt",
    policy: "ask",
  });
  // The transfer list names the file as it is saved here.
  const bar = transfers(page);
  await expect(bar.getByRole("listitem").filter({ hasText: "a_b.txt" })).toContainText("Download");
});

test("cancel_folder_and_cancel_all", async ({ page }) => {
  await createVault(page);
  const hostId = await seedHost(page, "web", "10.0.0.1");
  await seed(page, hostId, {
    local: [
      dir(`${HOME}/photos`),
      file(`${HOME}/photos/a.jpg`, 10),
      file(`${HOME}/photos/b.jpg`, 10),
      file(`${HOME}/photos/c.jpg`, 10),
      file(`${HOME}/notes.txt`, 10),
    ],
    remote: [dir("/srv")],
  });
  await openSftp(page);
  const local = localPane(page);
  const remote = remotePane(page);
  await pathOf(remote).fill("/srv");
  await pathOf(remote).press("Enter");
  const jobs = () => page.evaluate(() => window.__tetherMock.control.enqueued());
  const live = () => page.evaluate(async () => (await window.__tetherMock.activeWork()).transfers.map((t) => t.remote).sort());
  await rowOf(local, "photos").dragTo(remote);
  await expect.poll(async () => (await jobs()).length).toBe(3);
  await rowOf(local, "notes.txt").dragTo(remote);
  await expect.poll(async () => (await jobs()).length).toBe(4);

  const bar = transfers(page);
  const group = bar.getByRole("listitem").filter({ hasText: "photos" });
  await expect(group).toContainText("Upload");
  await expect(group).toContainText("0 of 3 files done");

  // Cancel on the folder row stops its files only.
  await group.getByRole("button", { name: "Cancel photos" }).click();
  await expect.poll(live).toEqual(["/srv/notes.txt"]);
  await expect(group).toContainText("3 cancelled");
  await expect(group.getByRole("button", { name: "Cancel photos" })).toHaveCount(0);

  // Cancel all stops everything left.
  const cancelAll = bar.getByRole("button", { name: "Cancel all" });
  await cancelAll.click();
  await expect.poll(live).toEqual([]);
  await expect(bar.getByRole("listitem").filter({ hasText: "notes.txt" })).toContainText("Cancelled");
  await expect(cancelAll).toBeDisabled();
});

test("conflict_dialog_offers_overwrite_skip_rename_resume", async ({ page }) => {
  await createVault(page);
  const hostId = await seedHost(page, "web", "10.0.0.1");
  await seed(page, hostId, {
    local: [file(`${HOME}/notes.txt`, 100), file(`${HOME}/big.bin`, 200)],
    remote: [dir("/srv")],
  });
  await openSftp(page);
  const local = localPane(page);
  await pathOf(remotePane(page)).fill("/srv");
  await pathOf(remotePane(page)).press("Enter");

  // Two files uploaded together (the keyboard way) make one batch.
  await rowOf(local, "big.bin").click();
  await rowOf(local, "notes.txt").click({ modifiers: ["ControlOrMeta"] });
  await expect(local.getByRole("row", { selected: true })).toHaveCount(2);
  await local.getByRole("button", { name: "Upload" }).click();
  const jobs = () => page.evaluate(() => window.__tetherMock.control.enqueued());
  await expect.poll(async () => (await jobs()).length).toBe(2);
  const [a, b] = (await jobs()).map((j) => j.jobId);

  const dialog = page.getByRole("dialog", { name: "File already exists" });
  const ask = (jobId: string, existingSize: number, incomingSize: number, slot: "__answer" | "__answers") =>
    page.evaluate(
      ({ jobId, existingSize, incomingSize, slot }) => {
        const p = window.__tetherMock.control.ask({
          kind: "transferConflict",
          jobId,
          path: "/srv/notes.txt",
          existingSize,
          incomingSize,
        });
        if (slot === "__answer") window.__answer = p;
        else (window.__answers ??= []).push(p);
      },
      { jobId, existingSize, incomingSize, slot },
    );
  const answer = () => page.evaluate(() => window.__answer);

  // It shows what exists and what comes, and Skip, not Overwrite, has the focus.
  await ask(a, 40, 100, "__answer");
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("/srv/notes.txt");
  await expect(dialog.getByText("40 B", { exact: true })).toBeVisible();
  await expect(dialog.getByText("100 B", { exact: true })).toBeVisible();
  for (const name of ["Overwrite", "Skip", "Rename", "Resume"]) {
    await expect(dialog.getByRole("button", { name, exact: true })).toBeVisible();
  }
  await expect(dialog.getByRole("button", { name: "Skip", exact: true })).toBeFocused();
  // Resume is possible: the existing file is smaller. Rename answers rename.
  await expect(dialog.getByRole("button", { name: "Resume", exact: true })).toBeEnabled();
  await dialog.getByRole("button", { name: "Rename", exact: true }).click();
  expect(await answer()).toEqual({ conflict: "rename" });
  await expect(dialog).toBeHidden();

  // An existing file that is not smaller cannot be resumed; Escape skips.
  await ask("some-other-job", 200, 100, "__answer");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Resume", exact: true })).toBeDisabled();
  await page.keyboard.press("Escape");
  expect(await answer()).toEqual({ conflict: "skip" });
  await expect(dialog).toBeHidden();

  // Overwrite is a deliberate choice that waits a moment before it arms.
  await ask("some-other-job", 40, 100, "__answer");
  await expect(dialog.getByRole("button", { name: "Overwrite", exact: true })).toBeEnabled();
  await dialog.getByRole("button", { name: "Overwrite", exact: true }).click();
  expect(await answer()).toEqual({ conflict: "overwrite" });
  await expect(dialog).toBeHidden();
  await ask("some-other-job", 40, 100, "__answer");
  await dialog.getByRole("button", { name: "Resume", exact: true }).click();
  expect(await answer()).toEqual({ conflict: "resume" });

  // "Apply to all" answers the batch's remaining conflicts without asking.
  await ask(a, 40, 100, "__answer");
  await dialog.getByRole("checkbox", { name: "Apply to all remaining conflicts in this batch" }).check();
  await dialog.getByRole("button", { name: "Skip", exact: true }).click();
  expect(await answer()).toEqual({ conflict: "skip" });
  await ask(b, 10, 200, "__answers");
  expect(await page.evaluate(() => window.__answers![0])).toEqual({ conflict: "skip" });
  await expect(dialog).toBeHidden();

  // Escape or a click outside skips only this file, even with "apply to all" ticked.
  await local.getByRole("button", { name: "Upload" }).click();
  await expect.poll(async () => (await jobs()).length).toBe(4);
  const [, , c, d] = (await jobs()).map((j) => j.jobId);
  await ask(c, 40, 100, "__answer");
  await dialog.getByRole("checkbox", { name: "Apply to all remaining conflicts in this batch" }).check();
  await page.keyboard.press("Escape");
  expect(await answer()).toEqual({ conflict: "skip" });
  await ask(d, 10, 200, "__answer");
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Escape");
  expect(await answer()).toEqual({ conflict: "skip" });
  await expect(dialog).toBeHidden();

  // A remembered Resume holds only where it can work: a file that is not
  // smaller than the incoming one is asked about, not "resumed" into a false success.
  await ask(c, 40, 100, "__answer");
  await dialog.getByRole("checkbox", { name: "Apply to all remaining conflicts in this batch" }).check();
  await dialog.getByRole("button", { name: "Resume", exact: true }).click();
  expect(await answer()).toEqual({ conflict: "resume" });
  await ask(d, 300, 200, "__answer");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Resume", exact: true })).toBeDisabled();
  await page.keyboard.press("Escape");
  expect(await answer()).toEqual({ conflict: "skip" });
  await ask(d, 10, 200, "__answers");
  expect(await page.evaluate(() => window.__answers![window.__answers!.length - 1])).toEqual({ conflict: "resume" });
  await expect(dialog).toBeHidden();

  // A prompt the backend withdraws closes its dialog.
  await ask("unrelated", 1, 2, "__answer");
  await expect(dialog).toBeVisible();
  const [pending] = await page.evaluate(() => window.__tetherMock.control.pendingPrompts());
  await page.evaluate((id) => window.__tetherMock.control.withdraw(id), pending);
  await expect(dialog).toBeHidden();
});

test("rename_mkdir_delete_chmod", async ({ page }) => {
  await createVault(page);
  const hostId = await seedHost(page, "web", "10.0.0.1");
  await seed(page, hostId, {
    remote: [dir("/srv"), file("/srv/old name.txt", 10), dir("/srv/keep"), file("/srv/keep/x", 1), file("/srv/tmp.log", 2)],
  });
  await openSftp(page);
  const remote = remotePane(page);
  await pathOf(remote).fill("/srv");
  await pathOf(remote).press("Enter");
  await expect.poll(() => names(remote)).toEqual(["keep", "old name.txt", "tmp.log"]);

  // Rename.
  await rowOf(remote, "old name.txt").click();
  await remote.getByRole("button", { name: "Rename", exact: true }).click();
  const rename = page.getByRole("dialog", { name: "Rename" });
  await expect(rename.getByLabel("New name")).toHaveValue("old name.txt");
  await rename.getByLabel("New name").fill('new "quoted" name.txt');
  await rename.getByRole("button", { name: "Rename", exact: true }).click();
  await expect(rename).toBeHidden();
  await expect.poll(() => names(remote)).toEqual(["keep", 'new "quoted" name.txt', "tmp.log"]);
  expect(await remoteNames(page, hostId, "/srv")).toContain('new "quoted" name.txt');

  // A name with a slash is refused before anything is sent.
  await rowOf(remote, 'new "quoted" name.txt').click();
  await remote.getByRole("button", { name: "Rename", exact: true }).click();
  await rename.getByLabel("New name").fill("a/b");
  await rename.getByRole("button", { name: "Rename", exact: true }).click();
  await expect(rename.getByRole("alert")).toContainText("slash");
  await rename.getByRole("button", { name: "Cancel" }).click();

  // New folder.
  await remote.getByRole("button", { name: "New folder", exact: true }).click();
  const mkdir = page.getByRole("dialog", { name: "New folder" });
  await mkdir.getByLabel("Folder name").fill("build output");
  await mkdir.getByRole("button", { name: "Create" }).click();
  await expect(mkdir).toBeHidden();
  await expect.poll(() => names(remote)).toEqual(["build output", "keep", 'new "quoted" name.txt', "tmp.log"]);

  // Delete asks first; Cancel changes nothing.
  await rowOf(remote, "tmp.log").click();
  await remote.getByRole("button", { name: "Delete", exact: true }).click();
  const del = page.getByRole("dialog", { name: /^Delete/ });
  await expect(del).toContainText("tmp.log");
  await del.getByRole("button", { name: "Cancel" }).click();
  await expect(rowOf(remote, "tmp.log")).toBeVisible();
  await remote.getByRole("button", { name: "Delete", exact: true }).click();
  await del.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(del).toBeHidden();
  await expect(rowOf(remote, "tmp.log")).toHaveCount(0);

  // An empty folder goes; one with files in it is refused, and stays.
  await rowOf(remote, "build output").click();
  await remote.getByRole("button", { name: "Delete", exact: true }).click();
  await del.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(rowOf(remote, "build output")).toHaveCount(0);
  await rowOf(remote, "keep").click();
  await remote.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(del).toContainText("must be empty");
  await del.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(del.getByRole("alert")).toContainText("not empty");
  await del.getByRole("button", { name: "Cancel" }).click();
  await expect(rowOf(remote, "keep")).toBeVisible();

  // Permissions: octal, validated.
  await rowOf(remote, 'new "quoted" name.txt').click();
  await expect(rowOf(remote, 'new "quoted" name.txt')).toContainText("-rw-r--r--");
  await remote.getByRole("button", { name: "Permissions", exact: true }).click();
  const chmod = page.getByRole("dialog", { name: "Permissions" });
  const mode = chmod.getByLabel("Mode (octal)");
  await expect(mode).toHaveValue("644");
  for (const bad of ["999", "12", "abc", "77777"]) {
    await mode.fill(bad);
    await chmod.getByRole("button", { name: "Apply" }).click();
    await expect(chmod.getByRole("alert")).toContainText("octal");
  }
  await mode.fill("600");
  await chmod.getByRole("button", { name: "Apply" }).click();
  await expect(chmod).toBeHidden();
  await expect(rowOf(remote, 'new "quoted" name.txt')).toContainText("-rw-------");
  const modes = await page.evaluate(
    async ({ hostId }) => (await window.__tetherMock.sftpList(hostId, "/srv")).map((e) => [e.name, e.mode & 0o7777]),
    { hostId },
  );
  expect(modes).toContainEqual(['new "quoted" name.txt', 0o600]);
});

test("hidden_files_toggle", async ({ page }) => {
  await createVault(page);
  const hostId = await seedHost(page, "web", "10.0.0.1");
  await seed(page, hostId, {
    local: [file(`${HOME}/.zshrc`, 1), file(`${HOME}/visible.txt`, 1), dir(`${HOME}/.config`)],
    remote: [dir("/srv"), file("/srv/.env", 1), file("/srv/app.txt", 1), dir("/srv/.git")],
  });
  await openSftp(page);
  const local = localPane(page);
  const remote = remotePane(page);
  await pathOf(remote).fill("/srv");
  await pathOf(remote).press("Enter");

  // Dotfiles are hidden by default, in both panes.
  await expect.poll(() => names(local)).toEqual(["visible.txt"]);
  await expect.poll(() => names(remote)).toEqual(["app.txt"]);

  // The toggle is per pane.
  const showRemote = remote.getByRole("checkbox", { name: "Show hidden files" });
  await expect(showRemote).not.toBeChecked();
  await showRemote.check();
  await expect.poll(() => names(remote)).toEqual([".git", ".env", "app.txt"]);
  await expect.poll(() => names(local)).toEqual(["visible.txt"]);

  const showLocal = local.getByRole("checkbox", { name: "Show hidden files" });
  await showLocal.check();
  await expect.poll(() => names(local)).toEqual([".config", ".zshrc", "visible.txt"]);

  await showRemote.uncheck();
  await expect.poll(() => names(remote)).toEqual(["app.txt"]);
  await expect.poll(() => names(local)).toEqual([".config", ".zshrc", "visible.txt"]);
});
