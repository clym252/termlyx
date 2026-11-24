import { fileURLToPath } from "node:url";

const site = fileURLToPath(new URL("..", import.meta.url));

export default {
  testDir: fileURLToPath(new URL(".", import.meta.url)),
  testMatch: "behaviour.spec.mjs",
  use: { baseURL: "http://127.0.0.1:4173" },
  webServer: {
    command: "pnpm build && python3 -m http.server 4173 --bind 127.0.0.1 -d dist",
    cwd: site,
    url: "http://127.0.0.1:4173",
    reuseExistingServer: false,
  },
};
