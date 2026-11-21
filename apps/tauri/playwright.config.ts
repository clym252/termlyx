import { defineConfig, devices } from "@playwright/test";
import process from "node:process";

// Fixed, unusual port (override with TETHER_E2E_PORT): the Tauri dev server owns 1420.
// The server is never reused: it must be the mock-backed one started below.
const port = Number(process.env.TETHER_E2E_PORT ?? 14200);

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  reporter: "list",
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: `pnpm exec vite --host 127.0.0.1 --port ${port} --strictPort`,
    url: `http://127.0.0.1:${port}`,
    reuseExistingServer: false,
    env: { VITE_TETHER_API: "mock" },
    timeout: 60_000,
  },
});
