import { APP_NAME } from "./brand";

/** Rounded-square terminal icon: accent chevron, light underscore. */
export function AppMark({ size = 22 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 26 26" aria-hidden="true" focusable="false">
      <rect x="1" y="1" width="24" height="24" rx="7" fill="none" stroke="var(--accent)" strokeWidth="2" />
      <path d="M7 9l4 4-4 4" stroke="var(--accent)" strokeWidth="2.2" fill="none" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M13 17h6" stroke="var(--text)" strokeWidth="2.2" strokeLinecap="round" />
    </svg>
  );
}

export function Wordmark({ compact = false, size }: { compact?: boolean; size?: number }) {
  return (
    <div className="wordmark" role="img" aria-label={APP_NAME}>
      <AppMark size={size} />
      {!compact && <span className="wordmark-text">{APP_NAME.toUpperCase()}</span>}
    </div>
  );
}
