import { defineConfig, devices } from "@playwright/test";

const port = Number(process.env.PORT ?? 8737);

export default defineConfig({
  testDir: "./tests",
  timeout: 180_000,
  reporter: "list",
  workers: 1,
  use: { baseURL: `http://localhost:${port}` },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
  ],
  webServer: {
    command: "node serve.mjs",
    url: `http://localhost:${port}/index.html`,
    reuseExistingServer: false,
    env: { PORT: String(port) },
  },
});
