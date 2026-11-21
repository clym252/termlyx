/**
 * Passwords and the recovery key are secrets: forms that take them are
 * uncontrolled. They are read from the form at submit, handed to the API,
 * and the inputs cleared; they are never put in React state.
 */

export const MIN_PASSWORD_LENGTH = 8;

/** The text value of a form field, "" if absent. */
export function field(data: FormData, name: string): string {
  const v = data.get(name);
  return typeof v === "string" ? v : "";
}

/** An error for a new password and its confirmation, or null if they are fine. */
export function checkNewPassword(password: string, confirm: string): string | null {
  if (password.length < MIN_PASSWORD_LENGTH) {
    return `The master password must be at least ${MIN_PASSWORD_LENGTH} characters.`;
  }
  if (password !== confirm) return "The passwords do not match.";
  return null;
}
