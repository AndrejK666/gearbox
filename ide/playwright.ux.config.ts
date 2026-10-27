import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "tests/ux", testMatch: "*.spec.ts", workers: 1, retries: 0,
  timeout: 120_000, expect: { timeout: 20_000 }, reporter: "line",
  outputDir: "test-results/ux",
  use: { channel: process.env.GEARBOX_UX_BROWSER, baseURL: "http://127.0.0.1:3014", viewport: { width: 1600, height: 1000 }, screenshot: "only-on-failure", trace: "retain-on-failure" },
  webServer: { command: "node tests/ux/server.cjs", url: "http://127.0.0.1:3014", reuseExistingServer: false, timeout: 120_000 },
});
