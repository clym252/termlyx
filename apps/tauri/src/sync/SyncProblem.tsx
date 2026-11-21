import type { Problem } from "./status";

/**
 * A sync failure in the step where it happened: the backend's own plain
 * message, plus advice where there is some. Severe ones (a changed server
 * certificate) are a warning box, not a line of red text.
 */
export function SyncProblem({ problem }: { problem: Problem }) {
  if (problem.severe) {
    return (
      <div role="alert" className="warning sync-warning">
        <strong>{problem.message}</strong>
        {problem.hint && <p>{problem.hint}</p>}
      </div>
    );
  }
  return (
    <p role="alert" className="form-error">
      {problem.message}
      {problem.hint && ` ${problem.hint}`}
    </p>
  );
}
