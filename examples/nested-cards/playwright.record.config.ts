import { defineConfig, devices } from "@playwright/test";

/**
 * Records the README's tour. Separate from the test config so a normal
 * `npx playwright test` does not spend time producing a video.
 *
 *   npx playwright test -c playwright.record.config.ts
 *   ./scripts/record.sh      # the above, plus the conversion to WebP
 */
export default defineConfig({
  testDir: "./scripts",
  reporter: [["list"]],
  webServer: {
    command: "./serve.sh",
    url: "http://127.0.0.1:8097/alpha",
    reuseExistingServer: true,
    timeout: 120_000,
  },
  use: { baseURL: "http://127.0.0.1:8097" },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
