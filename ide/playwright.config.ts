// Conformance, not smoke: every test is named for a claim in a design document.
//
// The suite this replaced (`scripts/ui-smoke.mjs`) proved that the application
// works. It could not tell anyone whether the application does what the
// documents say, and the gap turned out to be large -- of 39 functional
// requirements in the PRD, 7 are built and observable here. So the tests are
// grouped by the document they answer to, `test.fixme` marks a documented claim
// that is not implemented, and a reporter writes the table.

import { defineConfig } from "@playwright/test";

const URL = process.env.GEARBOX_STUDIO_URL ?? "http://127.0.0.1:3000/";

export default defineConfig({
  testDir: "tests",

  // **`tests/wedge/` is somebody else's server.** Those specs need a backend
  // whose engine is a wedging proxy and whose cap is eight seconds, and they get
  // one from `playwright.wedge.config.ts`. Collected here they would run against
  // this server, where nothing is ever withheld, and report a timeout mechanism
  // they never reached.
  // UX acceptance owns a temporary workspace and its own server configuration.
  testIgnore: ["wedge/**", "ux/**"],

  // One Theia backend, one engine process, and `catalogue/load` is stateful:
  // two workers would race over the same server. Serial also keeps the
  // conformance report in document order, which is how it is meant to be read.
  workers: 1,
  fullyParallel: false,

  // A conformance run that reports "flaky" has reported nothing. Anything
  // genuinely racy has to be written so that it either observes the claim or
  // says it could not, never so that a rerun changes the answer.
  retries: 0,

  // Theia's first paint against a real gear tree is seconds, not milliseconds,
  // and the staged-loading tests deliberately watch a whole load.
  timeout: 120_000,
  expect: { timeout: 15_000 },

  reporter: [["list"], ["./tests/report/conformance-reporter.ts"]],

  globalSetup: "./tests/global-setup.ts",
  // Checked at both ends: a write that lands after the last per-test hook is
  // invisible to it and would otherwise surface as a refusal to start the *next*
  // run, a day later and in a different file.
  globalTeardown: "./tests/global-teardown.ts",

  use: {
    baseURL: URL,
    viewport: { width: 1600, height: 1000 },
    // Screenshots only on failure: a conformance failure is usually "the thing
    // is not there", and a picture of what was there instead is the fastest
    // way to tell that from "the selector moved".
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },

  webServer: {
    // `npm run verify` used to assume someone had already started the app by
    // hand, which is why a failed run and an unstarted one looked the same.
    command: "npm run start:browser",
    url: URL,
    // **Reused, and that costs one thing worth knowing: a backend change needs
    // the server killed.** The frontend is served from disk, so `npm run build`
    // plus a reload picks it up -- and `global-setup` refuses a bundle older than
    // its sources. The *backend* JS is loaded into this process at startup, so a
    // reused server keeps running the code it started with. Measured the hard
    // way: a node-side field that was being sent looked like an engine that
    // ignored it, through three layers of correct code.
    reuseExistingServer: true,
    timeout: 180_000,
    stdout: "ignore",
    stderr: "pipe",
  },
});
