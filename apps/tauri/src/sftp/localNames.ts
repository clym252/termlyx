/** Device names Windows will not let a file or folder have, with or without an extension. */
const RESERVED = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/i;
/** Characters a Windows name can't have: `<>:"/\|?*` and the control characters. */
// eslint-disable-next-line no-control-regex
const FORBIDDEN = /[<>:"/\\|?*\u0000-\u001f]/;

/** Whether Windows reserves `name`. `CON.txt` and `con.tar.gz` are reserved too: only what is before the first dot counts. */
function isReserved(name: string): boolean {
  return RESERVED.test(name.split(".")[0].trimEnd());
}

/**
 * Why `name` can't be a file or folder name on this computer, or `null`.
 * Windows rules only (`windows`): characters it forbids, a trailing dot or
 * space, device names like `CON`. Elsewhere the pane's own checks are all
 * there is, so this returns `null`.
 */
export function validateLocalName(name: string, windows: boolean): string | null {
  if (!windows) return null;
  if (FORBIDDEN.test(name)) return "A name can't contain < > : \" / \\ | ? or *.";
  if (name.endsWith(".") || name.endsWith(" ")) return "A name can't end with a dot or a space.";
  if (isReserved(name)) return "Windows reserves that name.";
  return null;
}

/**
 * `name`, a remote file or folder name, as a name Windows accepts (the same
 * rules `validateLocalName` enforces): each forbidden or control character
 * becomes `_`, trailing dots and spaces go (`_` if nothing is left), and a
 * reserved device name gets a `_` in front (`con.txt` becomes `_con.txt`).
 * A name that is already legal comes back unchanged.
 */
export function windowsLocalName(name: string): string {
  const cleaned = name.replace(new RegExp(FORBIDDEN, "g"), "_").replace(/[. ]+$/, "");
  if (cleaned === "") return "_";
  return isReserved(cleaned) ? `_${cleaned}` : cleaned;
}
