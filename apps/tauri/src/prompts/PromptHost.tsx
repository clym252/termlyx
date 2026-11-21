import { useCallback, useEffect, useState } from "react";
import { api as appApi } from "../api";
import type { PromptEvent, PromptResponse, TetherApi, Unlisten } from "../api/types";
import { ConflictDialog } from "../sftp/ConflictDialog";
import { transferStore } from "../sftp/transfers";
import { HostKeyDialog } from "./HostKeyDialog";
import { KbdInteractiveDialog } from "./KbdInteractiveDialog";
import { publishPending } from "./pending";
import { SecretDialog } from "./SecretDialog";

/**
 * The answer a prompt already has without asking: a transfer conflict whose
 * batch had "apply to all" chosen. Everything else is asked. A remembered
 * Resume holds only where it can work (the existing file is smaller than
 * the incoming one); otherwise the user is asked, since resuming onto a
 * file that is not smaller would report a different file as transferred.
 */
export function standingAnswer(e: PromptEvent): PromptResponse | null {
  if (e.prompt.kind !== "transferConflict") return null;
  const policy = transferStore.autoAnswer(e.prompt.jobId);
  if (!policy) return null;
  if (policy === "resume" && e.prompt.existingSize >= e.prompt.incomingSize) return null;
  return { conflict: policy };
}

/**
 * Answers the backend's prompts (`prompt` events) one at a time, oldest
 * first: host keys, passphrases, passwords, keyboard-interactive, and the
 * file conflicts of transfers (wherever the user is in the app). A prompt
 * the backend withdraws (it timed out or its connection went away) closes
 * its dialog. Every dialog's dismiss path answers reject (a conflict: skip).
 */
export function PromptHost({ api = appApi }: { api?: TetherApi }) {
  const [queue, setQueue] = useState<PromptEvent[]>([]);

  const answer = useCallback(
    (id: string, response: PromptResponse) => {
      api.promptRespond(id, response).catch((e: unknown) => console.debug("prompt_respond failed", e));
    },
    [api],
  );

  useEffect(() => {
    let live = true;
    const offs: Unlisten[] = [];
    const keep = (off: Unlisten) => (live ? offs.push(off) : off());
    void api
      .onPrompt((e) => {
        const standing = standingAnswer(e);
        if (standing) answer(e.id, standing);
        else setQueue((q) => (q.some((p) => p.id === e.id) ? q : [...q, e]));
      })
      .then(keep);
    void api.onPromptWithdrawn((w) => setQueue((q) => q.filter((p) => p.id !== w.id))).then(keep);
    return () => {
      live = false;
      offs.forEach((off) => off());
    };
  }, [api, answer]);

  // "Apply to all" also settles conflicts of the batch that are already waiting.
  useEffect(() => {
    const settled = queue.flatMap((e) => {
      const standing = standingAnswer(e);
      return standing ? [{ id: e.id, standing }] : [];
    });
    if (settled.length === 0) return;
    for (const s of settled) answer(s.id, s.standing);
    setQueue((q) => q.filter((p) => !settled.some((s) => s.id === p.id)));
  }, [queue, answer]);

  // What is still waiting, for the cards that say so (see pending.ts).
  useEffect(() => {
    publishPending(queue);
    return () => publishPending([]);
  }, [queue]);

  const respond = useCallback(
    (id: string, response: PromptResponse) => {
      setQueue((q) => q.filter((p) => p.id !== id));
      answer(id, response);
    },
    [answer],
  );

  const current = queue[0];
  if (!current) return null;
  const onRespond = (r: PromptResponse) => respond(current.id, r);
  const { prompt } = current;
  switch (prompt.kind) {
    case "hostKeyUnknown":
    case "hostKeyChanged":
      return <HostKeyDialog key={current.id} prompt={prompt} onRespond={onRespond} />;
    case "passphrase":
    case "password":
      return <SecretDialog key={current.id} prompt={prompt} onRespond={onRespond} />;
    case "keyboardInteractive":
      return <KbdInteractiveDialog key={current.id} prompt={prompt} onRespond={onRespond} />;
    case "transferConflict":
      return <ConflictDialog key={current.id} prompt={prompt} onRespond={onRespond} />;
  }
}
