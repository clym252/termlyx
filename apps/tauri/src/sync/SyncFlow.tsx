import { useEffect, useRef, useState, type FormEvent } from "react";
import { createPortal } from "react-dom";
import { api } from "../api";
import { isApiError, recoveryKeyOf, type CodeInfo, type EntityKind, type RecoveryChoice } from "../api/types";
import { IS_IOS, IS_MOBILE } from "../platform";
import { checkNewPassword, field } from "../vault/password";
import { RecoveryKeyDialog } from "../vault/RecoveryKeyDialog";
import { copyText } from "../ui/clipboard";
import { QrScanOverlay } from "./QrScanOverlay";
import { SyncProblem } from "./SyncProblem";
import { problemFromCheckError, problemFromError, problemFromState, type Problem } from "./status";
import { useSyncStatus } from "./store";
import { APP_NAME, SELF_HOST_DOCS_URL } from "../app/brand";

/** Every kind of record the vault holds (what "replaces N records" counts). */
const COUNTED: Record<EntityKind, true> = {
  group: true,
  host: true,
  tag: true,
  identity: true,
  ssh_key: true,
  known_host: true,
  forward_rule: true,
  snippet: true,
};
// A Record keyed by kind, so a new kind cannot be left out of the count.
const RECORD_KINDS = Object.keys(COUNTED) as EntityKind[];

/** Form fields that hold a secret: emptied once they have been used. */
const SECRETS = ["password", "enableKey", "resetKey", "newPassword", "confirm"] as const;

type Path = "enable" | "join" | "recover";

type Step =
  | "code"
  | "confirm"
  | "username"
  | "password"
  | "recovery"
  | "replace"
  | "reset"
  | "deviceCode"
  | "progress"
  | "done";

/** What the progress step is waiting for. */
type Work = "enable" | "upload" | "join" | "reset";

const WORK_TEXT: Record<Work, string> = {
  enable: "Turning on sync…",
  upload: "Uploading your vault…",
  join: "Downloading your vault…",
  reset: "Resetting your password…",
};

/** The file the old vault is kept as when a join replaces it (the backend names it by the UTC date). */
function backupName(): string {
  return `vault.sqlite.before-join-${new Date().toISOString().slice(0, 10)}`;
}

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

interface SyncFlowProps {
  /**
   * `settings`: the vault exists (turn sync on with an invite, or replace it with a synced one
   * through an add-device code). `firstRun`: no vault yet (join one; or reset the password
   * first with the recovery key).
   */
  context: "settings" | "firstRun";
  /** The user backed out. */
  onClose: () => void;
  /** The flow finished and the user chose to leave it. */
  onDone: () => void;
  /** True while a step cannot be interrupted (the host dialog then ignores Escape). */
  onBusy?: (busy: boolean) => void;
}

/**
 * Turning sync on, joining a synced vault, and getting back in after a lost
 * master password, as one form of steps:
 *
 * - invite code: code, server and fingerprint, username, master password,
 *   recovery key, progress, done;
 * - add-device code: the same up to the master password, then download and
 *   done; when this device's vault is the synced one it is reconnected
 *   (records kept), and when another vault would be replaced the backend
 *   says so first and the replace warning comes before the download;
 * - recovery key reset on a device without a vault: code, server, username,
 *   recovery key and new password, then an add-device code, download, done.
 *
 * Secrets (code, passwords, recovery key) live only in the uncontrolled
 * fields. All steps stay mounted (the others hidden) so what was typed
 * survives Back, and the fields are emptied once used.
 */
export function SyncFlow({ context, onClose, onDone, onBusy }: SyncFlowProps) {
  const formRef = useRef<HTMLFormElement>(null);
  const { sync } = useSyncStatus();
  const [step, setStep] = useState<Step>("code");
  const [reset, setReset] = useState(false);
  const [info, setInfo] = useState<CodeInfo | null>(null);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [busy, setBusy] = useState(false);
  const [haveKey, setHaveKey] = useState(false);
  const [work, setWork] = useState<Work>("enable");
  // Held only while its dialog is open (the key has to be rendered somewhere).
  const [newKey, setNewKey] = useState<string | null>(null);
  const [records, setRecords] = useState(0);
  const [backup, setBackup] = useState<string | null>(null);
  const [replaced, setReplaced] = useState(false);
  const [reconnected, setReconnected] = useState(false);
  // A short code was typed: its server address is asked for under it, and `enable` sends it along.
  const [docsCopied, setDocsCopied] = useState<"idle" | "copied" | "failed">("idle");
  const [askAddress, setAskAddress] = useState(false);
  const [viaAddress, setViaAddress] = useState(false);
  // iOS: the camera is open behind the page (QR scan of the code).
  const [scanning, setScanning] = useState(false);
  const scanCancelled = useRef(false);
  const scanningNow = useRef(false);

  // Leaving mid-scan (the dialog closed, the screen changed) must not leave the camera up.
  useEffect(
    () => () => {
      if (scanningNow.current) void api.cancelScan();
    },
    [],
  );

  // A lock (auto-lock on the way back, Lock vault) ends a scan: this flow can stay mounted
  // under the unlock screen, and the see-through scanning page would hide that screen.
  useEffect(() => {
    if (!IS_MOBILE) return;
    let live = true;
    let off: (() => void) | undefined;
    void api
      .onVaultLocked(() => {
        if (!scanningNow.current) return;
        scanCancelled.current = true;
        void api.cancelScan();
      })
      .then((unlisten) => {
        if (live) off = unlisten;
        else unlisten();
      });
    return () => {
      live = false;
      off?.();
    };
  }, []);

  const path: Path = reset ? "recover" : info?.kind === "invite" ? "enable" : "join";
  const working = step === "progress" && work !== "upload";

  useEffect(() => onBusy?.(working || busy || scanning), [working, busy, scanning, onBusy]);

  // Focus the first field of each step (or its main button).
  const firstRender = useRef(true);
  useEffect(() => {
    if (firstRender.current) {
      firstRender.current = false;
      return;
    }
    const section = formRef.current?.querySelector<HTMLElement>(`[data-step="${step}"]`);
    const target =
      section?.querySelector<HTMLElement>("input:not([readonly])") ??
      section?.querySelector<HTMLElement>("button.primary") ??
      section?.querySelector<HTMLElement>("[tabindex='-1']");
    target?.focus();
  }, [step, haveKey]);

  // After enabling, the vault uploads in the background: wait for it.
  useEffect(() => {
    if (step !== "progress" || work !== "upload" || !sync) return;
    const s = sync.status;
    if (s.state === "idle" && s.lastSync > 0) {
      setStep("done");
    } else if (s.state === "error") {
      setProblem(problemFromState(s));
      setStep("done");
    } else if (s.state === "offline") {
      // The server dropped after the account was made: sync is on and keeps retrying.
      setProblem({ message: s.reason, hint: `${APP_NAME} keeps retrying in the background.`, severe: false });
      setStep("done");
    }
  }, [step, work, sync]);

  const read = (name: string): string => (formRef.current ? field(new FormData(formRef.current), name) : "");
  const clear = (...names: readonly string[]) => {
    for (const name of names) {
      const el = formRef.current?.elements.namedItem(name);
      if (el instanceof HTMLInputElement) el.value = "";
    }
  };

  function stop(message: string) {
    setProblem({ message, hint: null, severe: false });
  }

  /** A command failed: show why in the step it belongs to, emptying the field it blames. */
  function failed(e: unknown, fallback: Step) {
    const code = isApiError(e) ? e.code : "";
    let target = fallback;
    if (code === "wrongPassword") {
      target = "password";
      clear("password");
    } else if (code === "wrongRecoveryKey") {
      target = path === "recover" ? "reset" : "recovery";
      clear("enableKey", "resetKey");
      if (path !== "recover") setHaveKey(true);
    } else if (code === "usernameTaken") {
      target = "username";
    } else if (code === "invalidCode" && fallback !== "deviceCode") {
      target = "code";
    }
    setProblem(problemFromError(e, info?.server));
    setStep(target);
  }

  function go(next: Step) {
    setProblem(null);
    setStep(next);
  }

  async function parse() {
    const code = read("code").trim();
    if (!code) return stop("Paste the code you were given.");
    setBusy(true);
    setViaAddress(false);
    try {
      let parsed = await api.sync.parseCode(code);
      // A full code names its server: drop an address field left over from a short code.
      if (parsed.kind !== "short") setAskAddress(false);
      if (parsed.kind === "short") {
        if (context !== "settings") {
          return stop("A short code only connects a new account. Joining needs an add-device code from a device that already syncs.");
        }
        setAskAddress(true);
        const address = read("address");
        if (!address) {
          requestAnimationFrame(() => formRef.current?.querySelector<HTMLInputElement>("[name=address]")?.focus());
          return stop("Enter the address of your server.");
        }
        try {
          parsed = await api.sync.checkServer(address, code);
        } catch (e) {
          return setProblem(problemFromCheckError(e, address));
        }
        setViaAddress(true);
      }
      if (context === "firstRun" && !reset && parsed.kind !== "link") {
        return stop(
          "That is an invite code, which creates a new account. Joining a synced vault needs an add-device code. To turn sync on for a new vault, create it first, then use Settings.",
        );
      }
      setInfo(parsed);
      go("confirm");
    } catch (e) {
      setProblem(problemFromError(e));
    } finally {
      setBusy(false);
    }
  }

  /** iOS: scan the code from the other device's screen, then go on as if it had been pasted. */
  async function scanCode() {
    setProblem(null);
    scanCancelled.current = false;
    scanningNow.current = true;
    setScanning(true);
    let text: string | null = null;
    try {
      text = await api.scanQr();
    } finally {
      scanningNow.current = false;
      setScanning(false);
    }
    if (text === null) {
      // Cancelling is the user's own choice; anything else is the camera being off.
      if (!scanCancelled.current) {
        stop(`Could not open the camera. Allow camera access for ${APP_NAME} in Settings, or paste the code.`);
      }
      return;
    }
    const input = formRef.current?.elements.namedItem("code");
    if (input instanceof HTMLInputElement) input.value = text;
    await parse();
  }

  function cancelScan() {
    scanCancelled.current = true;
    void api.cancelScan();
  }

  async function askReplace() {
    setBusy(true);
    try {
      const lists = await Promise.all(RECORD_KINDS.map((kind) => api.listEntities(kind)));
      setRecords(lists.reduce((n, list) => n + list.length, 0));
      go("replace");
    } catch (e) {
      setProblem(problemFromError(e));
    } finally {
      setBusy(false);
    }
  }

  async function enable(recovery: RecoveryChoice) {
    setProblem(null);
    setWork("enable");
    setStep("progress");
    try {
      const result = await api.sync.enable(
        read("code").trim(),
        read("username").trim(),
        read("password"),
        recovery,
        viaAddress ? read("address") : undefined,
      );
      clear(...SECRETS, "code");
      setWork("upload");
      if (result.newRecoveryKey !== null) setNewKey(result.newRecoveryKey);
    } catch (e) {
      // A failure after the server took a new recovery key: that key is the only one that works
      // now, so it is shown all the same.
      const key = recoveryKeyOf(e);
      if (key !== null) setNewKey(key);
      failed(e, "recovery");
    }
  }

  async function join(code: string, password: string, replace: boolean, fallback: Step) {
    setProblem(null);
    setWork("join");
    setStep("progress");
    try {
      const result = await api.sync.join(code, read("username").trim(), password, replace);
      clear(...SECRETS, "code", "deviceCode");
      setBackup(result.backup);
      setReplaced(replace && result.backup !== null);
      setReconnected(result.reconnected);
      setStep("done");
    } catch (e) {
      if (!replace && isApiError(e) && e.code === "vaultExists") {
        // Another vault would be replaced: nothing changed yet, ask first. The retry reuses this
        // sign-in (the code is spent).
        setStep("password");
        return askReplace();
      }
      failed(e, fallback);
    }
  }

  async function resetPassword() {
    const resetKey = read("resetKey").trim();
    if (!resetKey) return stop("Enter your recovery key.");
    const mismatch = checkNewPassword(read("newPassword"), read("confirm"));
    if (mismatch) return stop(mismatch);
    setProblem(null);
    setWork("reset");
    setStep("progress");
    try {
      const result = await api.sync.recover(read("code").trim(), read("username").trim(), resetKey, read("newPassword"));
      // The new password stays in its field: the join below needs it.
      clear("resetKey", "confirm");
      go(result.needsDeviceCode ? "deviceCode" : "done");
    } catch (e) {
      clear("resetKey");
      failed(e, "reset");
    }
  }

  async function joinAfterReset() {
    const code = read("deviceCode").trim();
    if (!code) return stop("Paste the add-device code.");
    setBusy(true);
    try {
      const parsed = await api.sync.parseCode(code);
      if (parsed.kind !== "link") {
        return stop("That is an invite code. Joining needs an add-device code from a device that is already signed in.");
      }
      if (info && parsed.server !== info.server) return stop("That code is for a different server.");
    } catch (e) {
      setProblem(problemFromError(e));
      return;
    } finally {
      setBusy(false);
    }
    await join(code, read("newPassword"), false, "deviceCode");
  }

  async function advance() {
    setProblem(null);
    switch (step) {
      case "code":
        return parse();
      case "confirm":
        return go("username");
      case "username":
        if (!read("username").trim()) return stop("Enter your username.");
        return go(path === "recover" ? "reset" : "password");
      case "password":
        if (!read("password")) return stop("Enter your master password.");
        if (path === "enable") return go("recovery");
        // On a device with a vault the backend reconnects it if it is the synced vault, and refuses
        // (`vaultExists`, then the replace warning) if it is another one.
        return join(read("code").trim(), read("password"), false, "password");
      case "recovery":
        if (!haveKey) return;
        if (!read("enableKey").trim()) return stop("Enter your recovery key.");
        return enable({ existing: read("enableKey").trim() });
      case "replace":
        return join(read("code").trim(), read("password"), true, "replace");
      case "reset":
        return resetPassword();
      case "deviceCode":
        return joinAfterReset();
      default:
        return;
    }
  }

  function back() {
    const previous: Partial<Record<Step, Step>> = {
      confirm: "code",
      username: "confirm",
      password: "username",
      recovery: "password",
      replace: "password",
      reset: "username",
    };
    const to = previous[step];
    if (to) go(to);
  }

  const shown = (s: Step) => step !== s;
  const fingerprint = info?.fingerprint ?? null;
  const upload = sync?.status.state === "syncing" ? sync.status : null;
  const joined = path !== "enable";

  const error = problem && <SyncProblem problem={problem} />;
  const backButton = (
    <button type="button" onClick={back} disabled={busy}>
      Back
    </button>
  );

  return (
    <>
      <form
        ref={formRef}
        className="form sync-flow"
        aria-label={context === "firstRun" ? "Join a synced vault" : "Connect to server"}
        onSubmit={(e: FormEvent<HTMLFormElement>) => {
          e.preventDefault();
          void advance();
        }}
      >
        <section className="sync-step" data-step="code" hidden={shown("code")}>
          <label className="field">
            <span>{context === "settings" ? "Setup, invite or add-device code" : "Invite or add-device code"}</span>
            <input
              name="code"
              className="mono"
              autoComplete="off"
              autoCapitalize="off"
              spellCheck={false}
              autoFocus={context === "firstRun"}
            />
          </label>
          {askAddress && (
            <label className="field">
              <span>Server address</span>
              <input
                name="address"
                className="mono"
                placeholder="192.168.1.20 or nas.local"
                autoComplete="off"
                autoCapitalize="off"
                spellCheck={false}
              />
            </label>
          )}
          {IS_MOBILE && (
            <button type="button" className="scan-qr" onClick={() => void scanCode()} disabled={busy || scanning}>
              Scan QR code
            </button>
          )}
          <p className="muted small">
            {context === "firstRun"
              ? IS_MOBILE
                ? "Scan the QR code from a device that already syncs (Settings → Sync → Add device), or paste its code."
                : "Paste the add-device code from a device that already syncs (Settings → Sync → Add device)."
              : "An invite code from your server's admin creates your account. An add-device code from another device joins its synced vault: if it is this vault, it is reconnected and keeps its records; another vault would replace this one (you are asked first)."}
          </p>
          {reset && (
              <p className="muted small">
                Resetting your master password with your recovery key. Paste any code from your server.
              </p>
          )}
          {context === "settings" && (
            <p className="muted small">
              Don't have a server yet? Setup guide: <code className="mono">{SELF_HOST_DOCS_URL}</code>{" "}
              <button
                type="button"
                onClick={() => void copyText(SELF_HOST_DOCS_URL).then((ok) => setDocsCopied(ok ? "copied" : "failed"))}
              >
                Copy
              </button>{" "}
              <span role="status">
                {docsCopied === "copied" && "Copied"}
                {docsCopied === "failed" && "Could not copy. Select the address and copy it by hand."}
              </span>
            </p>
          )}
          {step === "code" && error}
          <div className="dialog-actions sync-actions">
            {context === "firstRun" && (
              <button type="button" className="link" onClick={() => setReset((r) => !r)}>
                {reset ? "Never mind, I remember it" : "I forgot my master password"}
              </button>
            )}
            <span className="grow" />
            <button type="button" onClick={onClose} disabled={busy}>
              Cancel
            </button>
            <button type="submit" className="primary" disabled={busy}>
              {busy ? "Checking…" : "Continue"}
            </button>
          </div>
        </section>

        <section className="sync-step" data-step="confirm" hidden={shown("confirm")}>
          <p>
            Server <strong>{info?.server}</strong>
            {fingerprint !== null && (
              <>
                {" "}
                · fingerprint{" "}
                <code className="mono fingerprint fingerprint-value" data-testid="fingerprint">
                  {fingerprint}
                </code>
              </>
            )}
          </p>
          {fingerprint !== null ? (
            <p className="muted small">
              Compare with <code className="mono">termlyx-server fingerprint</code> on your server.
            </p>
          ) : info?.server.startsWith("http://") ? (
            <p className="warning">
              This code carries no server fingerprint (the server is reached over plain HTTP). Continue only on a
              network you trust.
            </p>
          ) : (
            <p className="muted small">This server uses a publicly trusted certificate.</p>
          )}
          <div className="dialog-actions sync-actions">
            {backButton}
            <button type="submit" className="primary">
              Continue
            </button>
          </div>
        </section>

        <section className="sync-step" data-step="username" hidden={shown("username")}>
          <label className="field">
            <span>Username</span>
            <input name="username" autoComplete="username" autoCapitalize="off" spellCheck={false} />
          </label>
          <p className="muted small">
            {path === "enable"
              ? "Choose the name of your sync account on this server."
              : "The username of your sync account on this server."}
          </p>
          {step === "username" && error}
          <div className="dialog-actions sync-actions">
            {backButton}
            <button type="submit" className="primary">
              Continue
            </button>
          </div>
        </section>

        <section className="sync-step" data-step="password" hidden={shown("password")}>
          <label className="field">
            <span>Master password</span>
            <input name="password" type="password" autoComplete="current-password" />
          </label>
          <p className="muted small">
            {path === "enable"
              ? "Your current master password. It never leaves this device; the server gets only a key derived from it."
              : "The master password of your synced vault. It never leaves this device."}
          </p>
          {step === "password" && error}
          <div className="dialog-actions sync-actions">
            {backButton}
            <button type="submit" className="primary" disabled={busy}>
              {path === "enable" ? "Continue" : context === "settings" ? "Continue" : "Download vault"}
            </button>
          </div>
        </section>

        <section className="sync-step" data-step="recovery" hidden={shown("recovery")}>
          <h3>Recovery key</h3>
          <p className="muted">
            Sync sets up your recovery key with the server, so you can get back in on a new device if you forget the
            master password. Enter the key you saved when you created your vault, or make a new one (the old key then
            stops working).
          </p>
          {haveKey ? (
            <label className="field">
              <span>Recovery key</span>
              <input
                name="enableKey"
                className="mono"
                autoComplete="off"
                autoCapitalize="off"
                spellCheck={false}
              />
            </label>
          ) : (
            <div className="row">
              <button type="button" onClick={() => setHaveKey(true)}>
                I have my recovery key
              </button>
              <button type="button" onClick={() => void enable({ new: true })}>
                I lost it — create a new one
              </button>
            </div>
          )}
          {step === "recovery" && error}
          <div className="dialog-actions sync-actions">
            {backButton}
            {haveKey && (
              <>
                <button type="button" onClick={() => void enable({ new: true })}>
                  I lost it — create a new one
                </button>
                <button type="submit" className="primary">
                  Start syncing
                </button>
              </>
            )}
          </div>
        </section>

        <section className="sync-step" data-step="replace" hidden={shown("replace")}>
          <p className="warning">
            This replaces this device's vault ({plural(records, "record", "records")}). A copy is kept as {backupName()}.
          </p>
          <p className="muted small">The copy stays next to the vault file. Nothing is merged: the synced vault takes over.</p>
          {step === "replace" && error}
          <div className="dialog-actions sync-actions">
            <button type="button" onClick={onClose}>
              Cancel
            </button>
            <button type="submit" className="danger">
              Replace vault and join
            </button>
          </div>
        </section>

        {reset && (
        <section className="sync-step" data-step="reset" hidden={shown("reset")}>
          <label className="field">
            <span>Recovery key</span>
            <input name="resetKey" className="mono" autoComplete="off" autoCapitalize="off" spellCheck={false} />
          </label>
          <label className="field">
            <span>New master password</span>
            <input name="newPassword" type="password" autoComplete="new-password" />
          </label>
          <label className="field">
            <span>Confirm new master password</span>
            <input name="confirm" type="password" autoComplete="new-password" />
          </label>
          {step === "reset" && error}
          <div className="dialog-actions sync-actions">
            {backButton}
            <button type="submit" className="primary">
              Reset password
            </button>
          </div>
        </section>
        )}

        <section className="sync-step" data-step="deviceCode" hidden={shown("deviceCode")}>
          <p role="status">
            Your master password was reset. To finish, this device needs an add-device code: get one from a device
            that is signed in (Settings → Sync → Add device), or ask your server's admin.
          </p>
          <label className="field">
            <span>Add-device code</span>
            <input name="deviceCode" className="mono" autoComplete="off" autoCapitalize="off" spellCheck={false} />
          </label>
          {step === "deviceCode" && error}
          <div className="dialog-actions sync-actions">
            <button type="submit" className="primary" disabled={busy}>
              Download vault
            </button>
          </div>
        </section>

        <section className="sync-step" data-step="progress" hidden={shown("progress")}>
          <p role="status" tabIndex={-1}>
            {WORK_TEXT[work]}
            {work === "upload" && upload && upload.total !== null && ` ${upload.done} of ${upload.total}`}
          </p>
          {IS_IOS && work !== "upload" && (
            // The first connection waits for the answer (core-sync `first_contact`).
            <p className="muted small">If iOS asks to find devices on your local network, tap Allow.</p>
          )}
          {work === "upload" && (
            // The command has returned: sync is on and the upload goes on without this dialog.
            <>
              <p className="muted small">You can close this; the upload continues in the background.</p>
              <div className="dialog-actions sync-actions">
                <button type="button" onClick={onDone}>
                  Close
                </button>
              </div>
            </>
          )}
        </section>

        <section className="sync-step" data-step="done" hidden={shown("done")}>
          {joined ? (
            <>
              <p role="status" tabIndex={-1}>
                {reconnected
                  ? "This vault is reconnected to sync. Its records are kept, and what changed while it was not syncing is uploaded now."
                  : "Your vault is synced."}
                {replaced && backup && ` The previous vault was kept as ${backup}.`}
              </p>
              {context === "firstRun" && <p className="muted small">It is unlocked with your master password.</p>}
            </>
          ) : (
            <p role="status" tabIndex={-1}>
              {problem ? "Sync is on, but the first sync did not finish." : "Sync is on. Your vault is up to date on the server."}
            </p>
          )}
          {step === "done" && error}
          <div className="dialog-actions sync-actions">
            <button type="button" className="primary" onClick={onDone}>
              {context === "firstRun" ? `Open ${APP_NAME}` : "Done"}
            </button>
          </div>
        </section>
      </form>
      {scanning && <QrScanOverlay onCancel={cancelScan} />}
      {/* Beside the host dialog in the page, not inside it. */}
      {newKey !== null &&
        createPortal(<RecoveryKeyDialog recoveryKey={newKey} onSaved={() => setNewKey(null)} />, document.body)}
    </>
  );
}
