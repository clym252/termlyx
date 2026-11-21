import { APP_NAME } from "../app/brand";

/** The vault failed to open at startup (Task 12): say why, offer nothing destructive. */
export function VaultUnavailable({ error, onRetry }: { error: string | null; onRetry: () => void }) {
  return (
    <main className="center-screen">
      <section className="card form" aria-labelledby="unavailable-title">
        <h1 id="unavailable-title">Your vault could not be opened</h1>
        <p role="alert" className="form-error">
          {error ?? "The vault file could not be read."}
        </p>
        <p className="muted">
          Nothing was changed. The vault file has been left exactly as it was; check that it is
          intact and that no other copy of {APP_NAME} is using it, then try again.
        </p>
        <button type="button" className="primary" onClick={onRetry}>
          Try again
        </button>
      </section>
    </main>
  );
}
