/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** `"mock"` builds the UI against the in-memory API (src/api/mock.ts), e.g. for Playwright. */
  readonly VITE_TETHER_API?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
