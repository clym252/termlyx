import { useId, useRef, useState } from "react";
import { api } from "../api";
import type { Id, Identity } from "../api/types";
import { IS_MOBILE } from "../platform";

/** The Identity select's value for "Own username & password" before the own login exists. */
export const OWN_LOGIN = "own";

/** How an identity reads in a select: a Keychain identity, or someone's own login. */
export function identityOptionLabel(i: Identity): string {
  return i.owner ? `${i.label} login (${i.username})` : `${i.label} (${i.username})`;
}

/**
 * A host's or group's own username and password while its editor is open.
 * The password field is uncontrolled and read only on save (then cleared).
 */
export function useOwnLogin(ownerId: Id | null, linkedId: Id | null, identities: readonly Identity[]) {
  // The one the owner links, if it is its own; else any it owns (not yet pruned).
  const mine = ownerId ? identities.filter((i) => i.owner === ownerId) : [];
  const owned = mine.find((i) => i.id === linkedId) ?? mine[0];
  const [username, setUsername] = useState(owned?.username ?? "");
  const password = useRef<HTMLInputElement>(null);
  const remove = useRef<HTMLInputElement>(null);

  /** Whether the Identity setting `value` means this record's own login. */
  const isOwn = (value: string | null) => value === OWN_LOGIN || (!!owned && value === owned.id);

  /** Saves the own login on `owner` (just saved) and returns it. */
  async function save(owner: Id): Promise<Identity> {
    const typed = password.current?.value ?? "";
    if (password.current) password.current.value = "";
    const change = typed ? { password: typed } : remove.current?.checked ? { password: null } : {};
    return api.setOwnLogin({ ownerId: owner, username: username.trim(), ...change });
  }

  return { owned, username, setUsername, password, remove, isOwn, save };
}

export type OwnLogin = ReturnType<typeof useOwnLogin>;

/** Username and Password of an own login, under the Identity setting. */
export function OwnLoginFields({ login }: { login: OwnLogin }) {
  const userId = useId();
  const passId = useId();
  const saved = login.owned?.hasPassword ?? false;
  return (
    <div className="own-login">
      <div className="field">
        <label htmlFor={userId}>Username</label>
        <input
          id={userId}
          value={login.username}
          onChange={(e) => login.setUsername(e.target.value)}
          placeholder={IS_MOBILE ? "Login name on the server" : "Your local login name"}
          spellCheck={false}
          autoCapitalize="off"
        />
      </div>
      <div className="field">
        <label htmlFor={passId}>Password</label>
        <input
          id={passId}
          ref={login.password}
          type="password"
          autoComplete="off"
          placeholder={saved ? "Saved — leave blank to keep" : "Not saved (asked when connecting)"}
        />
      </div>
      {saved && (
        <label className="choice">
          <input type="checkbox" ref={login.remove} />
          <span>Remove saved password</span>
        </label>
      )}
    </div>
  );
}
