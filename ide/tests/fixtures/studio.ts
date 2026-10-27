// The two ways this suite opens the application, and why there are two.
//
// Most conformance tests only read what the projection produced, so they can
// share one loaded application: `studio` is worker-scoped and boots once.
// The staged-loading tests cannot -- they have to watch a load happen -- so
// `freshStudio` is test-scoped and gets its own context.
//
// The split is not only about speed. Theia persists its layout, and
// `initializeLayout` runs *only* when there is no saved layout
// (`view-contributions.ts` explains why that is the right behaviour). Playwright
// gives every context an empty localStorage, so the panels open as designed --
// which means this suite depends on never setting `storageState` or otherwise
// reusing a profile. Reusing one would collapse the panels and fail half the
// claims here for a reason that has nothing to do with the claims.

import { join } from "node:path";

import {
  expect,
  test as base,
  type Browser,
  type BrowserContext,
  type Locator,
  type Page,
} from "@playwright/test";

import { noteTest } from "./description-watch";
import { RpcControl } from "./rpc";
import { liveCopies } from "./product-copy";
import { productsDiff, productsStatus, restoreProducts } from "./products-tree";
import { corpusDiff, corpusStatus, restoreCorpus } from "./corpus-files";
import {
  appendWriteTrace,
  flushWriteTraces,
  formatWriteTraces,
  readWriteTraces,
  resetWriteTraces,
  trackWriteTrace,
} from "./write-traces";

/** One sample of the catalogue's DOM, taken while a load is in flight. */
export interface Sample {
  t: number;
  rows: number;
  /**
   * Visible, not merely attached. A collapsed Theia side panel keeps its widget
   * in the DOM, so counting nodes says nothing about what a person sees -- this
   * distinction is what let an earlier version of this check pass against a
   * blank screen.
   */
  visibleRows: number;
  pending: number;
  badges: number;
  /** Ids inside rows only: `.gbx-id` is also the class on a diagnostic code. */
  rowIds: number;
  groups: string[];
  names: string[];
  /** Rows showing "parsing…" or "did not project". */
  waiting: number;
}

export interface Studio {
  page: Page;
  /** Console errors, with the failing URL appended -- see below. */
  consoleErrors: string[];
  /**
   * Console warnings, collected because of exactly one of them: a TextMate
   * grammar that fails to load is a `logger.warn` inside MonacoTextmateService,
   * never an error. Without this the editor falls back to plaintext silently and
   * every other check still passes.
   */
  /** Console warnings — see interface comment. */
  consoleWarnings: string[];
  /**
   * Stack traces from `ProductEditService` at the moment a write is about to
   * happen. Collected separately because the default handler ignored `info`.
   */
  writeTraces: string[];
  /** Every sample taken since before the document had scripts. */
  timeline(): Promise<Sample[]>;
  /** Select a row whose name contains `name`; resolve with the detail text. */
  detailOf(name: string): Promise<string | null>;
  /**
   * The detail panel read as labelled rows rather than as one string.
   *
   * Needed because `textContent` concatenates sibling `div`s with nothing
   * between them: the host gear's panel reads
   * `...selects vendor constructorfabricGTS types...`, and a regex over that
   * picks up the next row's first word. The `<code>` runs are kept separately
   * because that is where a projected identifier lives.
   */
  factsOf(name: string): Promise<Record<string, Fact> | null>;
}

/** One labelled row of the Gear detail panel. */
export interface Fact {
  text: string;
  codes: string[];
}

const SAMPLE_MS = 25;

/**
 * Installed with `addInitScript`, so it starts sampling before Theia's own
 * scripts run.
 *
 * In-page rather than driven from the test: the claim under test is that a row
 * is useful *before* it is complete, and a driver-side poll adds a round trip to
 * every sample. A snapshot taken after the load would pass even if the whole
 * tree had appeared at once, which is the failure this exists to rule out.
 */
function installSampler(): void {
  const samples: unknown[] = [];
  (window as unknown as { __gbxSamples: unknown[] }).__gbxSamples = samples;
  // Scoped to the catalogue widget. `.gbx-row` is shared with the Product view's
  // process and binding rows on purpose -- they are the same kind of thing and
  // should look alike -- which means an unscoped count silently mixes the two.
  // That is not hypothetical: a diagnostics test started passing because the
  // Product panel had rendered a diagnostic the catalogue never produced.
  const q = (sel: string): Element[] =>
    Array.from(document.querySelectorAll(`.gbx-widget-catalogue ${sel}`));
  const text = (e: Element): string => (e.textContent ?? "").trim();
  setInterval(() => {
    const rows = q(".gbx-row");
    samples.push({
      t: Date.now(),
      rows: rows.length,
      visibleRows: rows.filter((r) => (r as HTMLElement).getClientRects().length > 0).length,
      pending: q(".gbx-row.gbx-pending").length,
      badges: q(".gbx-badge").length,
      rowIds: q(".gbx-row .gbx-id").length,
      groups: q(".gbx-group-label").map(text),
      names: q(".gbx-row-name").map(text),
      waiting: q(".gbx-row .gbx-waiting").length,
    });
  }, 25);
}

async function open(
  browser: Browser,
  /**
   * Run against the context before anything navigates.
   *
   * For a websocket route, which has to be installed while the socket is still
   * unopened — the connection this suite cares about is made during the first
   * load, and a route added afterwards sees none of it.
   */
  beforeNavigate?: (context: BrowserContext) => Promise<void>,
): Promise<{ studio: Studio; close: () => Promise<void> }> {
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  await beforeNavigate?.(context);
  const page = await context.newPage();

  const consoleErrors: string[] = [];
  const consoleWarnings: string[] = [];
  const writeTraces: string[] = [];
  page.on("console", (message) => {
    const text = message.text();
    if (message.type() === "info" && text.startsWith("Gearbox: writing")) {
      const args = message.args();
      const work = Promise.resolve(args[1]?.jsonValue())
        .then((stack) => {
          const line = stack ? `${text}\n${String(stack)}` : text;
          writeTraces.push(line);
          appendWriteTrace(line);
        })
        .catch(() => {
          writeTraces.push(text);
          appendWriteTrace(text);
        });
      trackWriteTrace(work);
    }
    if (message.type() === "error") {
      // The URL of a failed request lives in `location()`, not in `text()`: the
      // text is only "Failed to load resource: ... 404". Without the URL there
      // is no way to tell a missing favicon from a missing bundle.
      const url = message.location()?.url ?? "";
      consoleErrors.push(url ? `${message.text()} [${url}]` : message.text());
    }
    if (message.type() === "warning") consoleWarnings.push(message.text());
  });
  page.on("pageerror", (error) => consoleErrors.push(`pageerror: ${error.message}`));

  await page.addInitScript(installSampler);
  await page.goto("/", { waitUntil: "domcontentloaded" });

  const studio: Studio = {
    page,
    consoleErrors,
    consoleWarnings,
    writeTraces,
    timeline: () =>
      page.evaluate(
        () => (window as unknown as { __gbxSamples?: Sample[] }).__gbxSamples ?? [],
      ) as Promise<Sample[]>,
    // **Select, then reveal, and that order is now load-bearing.** The Inspector
    // is gated on `gearbox.hasSelection`: with nothing selected it is not in the
    // palette, because a panel whose whole content is "select something" is worse
    // than an absent one. Revealing first therefore fails at the command, which
    // is the gate working -- so this picks the row first and then asks for the
    // panel that has something to say about it.
    detailOf: async (name: string) => {
      await resetCatalogueView(page);
      const picked = await page.evaluate((wanted) => {
        const row = Array.from(
          document.querySelectorAll(".gbx-widget-catalogue .gbx-row"),
        ).find((r) =>
          r.querySelector(".gbx-row-name")?.textContent?.includes(wanted),
        );
        if (!row) return false;
        (row as HTMLElement).click();
        return true;
      }, name);
      if (!picked) return null;
      await revealDetail(page);
      return page.evaluate(async (wanted) => {
        // The detail widget renders on the store's change event, so poll for the
        // gear's own name to appear rather than sleeping a guessed interval.
        //
        // **Scoped to the Inspector.** The Composition pane renders a
        // `.gbx-detail` too, and once a claim earlier in the run had opened
        // the product with API Gateway in it, an unscoped query read that
        // pane's settings form -- which also says "API Gateway" -- instead
        // of the catalogue facts the claim is about.
        for (let attempt = 0; attempt < 60; attempt += 1) {
          const text =
            document.querySelector(".gbx-widget-inspector .gbx-detail")?.textContent ?? "";
          if (text.includes(wanted)) return text.replace(/\s+/g, " ").trim();
          await new Promise((r) => setTimeout(r, 50));
        }
        return (document.querySelector(".gbx-widget-inspector .gbx-detail")?.textContent ?? "")
          .replace(/\s+/g, " ")
          .trim();
      }, name);
    },
    factsOf: async (name: string) => {
      const found = await studio.detailOf(name);
      if (found === null) return null;
      return page.evaluate(() => {
        const out: Record<string, { text: string; codes: string[] }> = {};
        for (const row of Array.from(document.querySelectorAll(".gbx-widget-inspector .gbx-detail .gbx-kv"))) {
          const spans = row.children;
          const label = (spans[0]?.textContent ?? "").trim();
          const value = spans[1];
          if (label.length === 0 || value === undefined) continue;
          out[label] = {
            text: (value.textContent ?? "").replace(/\s+/g, " ").trim(),
            codes: Array.from(value.querySelectorAll("code")).map((c) =>
              (c.textContent ?? "").trim(),
            ),
          };
        }
        return out;
      });
    },
  };

  return { studio, close: () => context.close() };
}

/** Wait until rows exist, none are pending, and that has held for a beat. */
export async function settled(page: Page): Promise<void> {
  await page.waitForFunction(
    () => {
      const samples = (window as unknown as { __gbxSamples?: Sample[] }).__gbxSamples ?? [];
      const last = samples[samples.length - 1];
      const previous = samples[samples.length - 2];
      return Boolean(
        last &&
          previous &&
          last.rows > 0 &&
          last.pending === 0 &&
          previous.pending === 0 &&
          previous.rows === last.rows,
      );
    },
    undefined,
    { timeout: 90_000, polling: SAMPLE_MS },
  );
  // A second signal, because the catalogue settling says nothing about the rest
  // of the shell: Theia attaches the side and bottom areas after the tree.
  //
  // This waited for `.gbx-inspector` until the Inspector moved to the right
  // panel and stopped opening itself on Home -- it now arrives with the first
  // selection, which is what fills it, so waiting for it here would wait for a
  // selection nobody has made. The Start screen is the honest boot-complete
  // signal for the Home context every session begins in.
  //
  // **Folding the catalogue on Home does not affect this**, which was worth
  // checking rather than assuming: a collapsed side panel keeps its widget
  // rendered and attached -- 14 rows, 49 pixels of tab bar -- so the sampler
  // still counts what it has always counted.
  await page.waitForSelector(".gbx-start", { state: "attached", timeout: 60_000 });
}

/**
 * Open a view through the command palette.
 *
 * F1 is pressed repeatedly because a keypress sent while Theia is still
 * installing its keybindings is simply lost -- a race in driving the UI, not a
 * defect in it, and one that waiting cannot fix because nothing opens without
 * another press. The selector is `.quick-input-widget`, without the `monaco-`
 * prefix Theia used to carry; a selector that never matches is worse than no
 * wait at all, because swallowing the timeout leaves the step passing or failing
 * on timing.
 */
export async function openPalette(page: Page): Promise<void> {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await page.keyboard.press("F1");
    const opened = await page
      .locator(".quick-input-widget")
      .waitFor({ state: "visible", timeout: 1000 })
      .then(() => true, () => false);
    if (opened) return;
  }
  await page.locator(".quick-input-widget").waitFor({ state: "visible" });
}

/**
 * Refuse to drive the application while a modal dialog is up.
 *
 * A leftover dialog is not untidy, it is armed. Theia binds Enter on
 * **`document.body`** while one is open (`@theia/core/lib/browser/dialogs.js:82`)
 * and `handleEnter` accepts unless the event came from a textarea, so the next
 * keystroke any test sends -- to the palette, to an editor, to nothing in
 * particular -- answers it. When the dialog is a description-edit confirmation,
 * that keystroke writes to `product.gdl`. The `studio` page is worker-scoped, so a
 * leftover outlives the test that made it and lands on a stranger.
 *
 * Checked here rather than in an `afterEach`, and the reason is mechanical: this
 * suite's page belongs to the worker-scoped `studio` fixture, and a hook that
 * asked for a page fixture would get a *different*, freshly created one. So the
 * check sits at the two doors every test goes through, which is also where the
 * damage would be done.
 */
async function refuseIfDialogOpen(page: Page, doing: string): Promise<void> {
  const dialogs = page.locator(".dialogBlock");
  if ((await dialogs.count()) === 0) return;
  const titles = await page.locator(".dialogBlock .dialogTitle").allInnerTexts();
  await page.keyboard.press("Escape");
  throw new Error(
    `a modal dialog was open before ${doing}: ${JSON.stringify(titles)}. ` +
      `An earlier test left it, and Enter is bound on document.body while it is up, ` +
      `so the next keystroke would have answered it. It has been dismissed.`,
  );
}

/**
 * What the command palette offers for a query, as labels.
 *
 * Reads rather than runs, because "this command is not reachable" is a claim about
 * the palette itself: `QuickCommandService` filters
 * `CommandRegistry.getAllCommands()` by `isVisible && isEnabled`, so a command
 * that is still registered is still offered no matter what the menus say. Asserting
 * on menus alone missed that for the whole life of the shell policy.
 *
 * Closes the palette on the way out. A palette left open swallows the next test's
 * keystrokes, and the dialog guard above exists because that class of leftover has
 * already cost this suite a stray write.
 */
export async function paletteOffers(page: Page, query: string): Promise<string[]> {
  await openPalette(page);
  await page.keyboard.type(query, { delay: 10 });
  const options = page.locator(`.quick-input-list [role="option"]`);
  // Either something matched or the list is empty; both are answers, and waiting
  // for the first option would turn "nothing is offered" into a timeout.
  await page
    .locator(".quick-input-widget")
    .waitFor({ state: "visible" })
    .catch(() => undefined);
  const labels = (await options.allInnerTexts()).map((text) => text.replace(/\s+/g, " ").trim());
  await page.keyboard.press("Escape");
  await page.locator(".quick-input-widget").waitFor({ state: "hidden" });
  return labels;
}

export async function runCommand(page: Page, label: string): Promise<void> {
  await refuseIfDialogOpen(page, `running "${label}"`);
  await openPalette(page);
  await page.keyboard.type(label, { delay: 20 });
  // Wait for the filtered list to settle on a match before committing, rather
  // than sleeping a guessed interval.
  const options = page.locator(`.quick-input-list [role="option"]`);
  await options.first().waitFor({ state: "visible" });

  // Click an entry that actually *contains* what was typed, rather than pressing
  // Enter on whatever the fuzzy matcher ranked first.
  //
  // Measured: typing `Gearbox Product` offers `Gearbox: Open Product…` **first**
  // and `View: Toggle Gearbox Product` second, because a view toggle's palette
  // label is prefixed. So Enter ran the wrong command -- it opened a second
  // quick-pick -- and the caller waited sixty seconds for a panel nobody had
  // asked for. First-match-wins was a fragility all along: every command added to
  // this application could shift the ranking under every test that reveals a view.
  //
  // Containment, not equality: the label a caller knows is `Gearbox Product`, and
  // the palette renders it inside `View: Toggle …`.
  const matching = options.filter({ hasText: label });
  if ((await matching.count()) > 0) {
    await matching.first().click();
    return;
  }

  // **Not `Enter`.** Pressing it here used to be the fallback, on the theory that
  // the fuzzy matcher had probably ranked the right thing first. Two problems, and
  // the second is why this throws instead.
  //
  // It hides a broken test: a command whose label changed, or one that was never
  // registered, ran *something else* and the failure surfaced sixty seconds later
  // as a panel that never appeared.
  //
  // And it can write to a file. `DialogOverlayService` binds Enter on
  // `document.body` (`@theia/core/lib/browser/dialogs.js:82`), so while any dialog
  // is open an Enter anywhere accepts it -- including a description-edit
  // confirmation left over from an earlier test in this worker-scoped page. That
  // is a candidate mechanism for the stray `use_gear(...)` writes this suite has
  // guarded against three times, and it costs nothing to take away.
  const offered = await options.allInnerTexts();
  throw new Error(
    `no command in the palette contains "${label}". Offered: ${JSON.stringify(offered)}`,
  );
}

/**
 * Bring a view to the front, idempotently.
 *
 * Three Theia behaviours have to be respected at once, and each of them broke a
 * test before this helper existed:
 *
 *   - the view command is a *toggle*, so calling it on the active view closes it;
 *   - `visible` is not `attached` -- a tab that exists but is not current keeps
 *     its widget in the DOM, and with two Gearbox views in the main area that is
 *     the normal case;
 *   - Theia's layout restorer reopens tabs after a reload, so a view may already
 *     be there before anything asks for it.
 *
 * So: check visibility, not presence, and let the toggle activate rather than
 * open. `toggleView` activates a view that is open but not focused, which is
 * exactly what is wanted here.
 */
async function revealView(page: Page, command: string, selector: string): Promise<void> {
  await refuseIfDialogOpen(page, `revealing ${command}`);
  for (let attempt = 0; attempt < 3; attempt += 1) {
    if (await page.locator(selector).first().isVisible()) return;
    // Attached-but-hidden: the widget is in the DOM behind another tab. Click its
    // shell tab; fall back to the command only when the widget does not exist yet.
    //
    // This was briefly reverted to always use the command, on the argument that a
    // tab click moves the tests off the path a person takes. Measured both ways:
    // the command reveals Product when the Graph is on top of it, but across the
    // whole suite it produced widespread `toBeVisible` failures and took 19.6
    // minutes against 3.3. Whatever the mechanism -- a toggle is a toggle, and
    // `runCommand` drives the palette, which is a second stateful thing to get
    // wrong -- the tab click is what Theia reliably listens to.
    //
    // The user's path is not left uncovered by this: `regression.spec.ts` asserts
    // that the toggle *command* reveals an open-but-hidden view, which is the
    // claim a tab click would otherwise hide.
    const tabId = await page.evaluate((sel) => {
      const el = document.querySelector(sel);
      let n: HTMLElement | null = el instanceof HTMLElement ? el : null;
      while (n) {
        if (n.id && document.getElementById(`shell-tab-${n.id}`)) return n.id;
        n = n.parentElement;
      }
      return "";
    }, selector);
    if (tabId !== "") {
      await page.locator(`[id="shell-tab-${tabId}"]`).click();
    } else {
      await runCommand(page, command);
    }
    const shown = await page
      .locator(selector)
      .first()
      .waitFor({ state: "visible", timeout: 15_000 })
      .then(() => true, () => false);
    if (shown) return;
  }

  // **Bounded, and this line is why the suite could go silent.** After three
  // failed attempts this used to wait with no timeout at all -- the config sets
  // no `actionTimeout`, so `waitFor` without one waits forever. A view that never
  // appears then wedged the whole run: the reporter printed nothing further, no
  // test was blamed, and even the test timeout did not surface, because the
  // failure artefacts are captured from the same stuck page.
  //
  // Measured while chasing exactly that: `it renders the cluster graph` calls this
  // for `Gearbox Graph` when the main area holds only `Gearbox Studio` and
  // `Gearbox Product` -- no graph tab, nothing to click, and the command fallback
  // did not bring one either. A missing view has to fail like a missing view.
  const shown = await page
    .locator(selector)
    .first()
    .waitFor({ state: "visible", timeout: 30_000 })
    .then(() => true, () => false);
  if (shown) return;

  const tabs = await page
    .locator(".lm-TabBar li")
    .allInnerTexts()
    .then((all) => all.map((t) => t.trim()).filter((t) => t.length > 0));
  throw new Error(
    `"${command}" never showed \`${selector}\` after three attempts and a 30s wait. ` +
      `Tabs on screen: ${JSON.stringify([...new Set(tabs)])}.`,
  );
}

/**
 * No test may leave a product description changed.
 *
 * `global-setup` checks this once, before the run; this checks it after every
 * test, and the difference is attribution. A description that goes dirty
 * mid-run poisons every later claim that depends on the resolution -- observed
 * twice, as a stray `use_gear("grpc-hub")` and a stray `use_gear("types-registry")`
 * -- and the failures land in files that have nothing to do with the write.
 *
 * The cause has not been found: it did not reproduce across three full runs with
 * a stack trace armed on the only code path that writes. So this does the next
 * best thing rather than pretending otherwise -- it **fails the test that did
 * it**, prints the diff naming the gear, and restores the tree so the rest of the
 * run is still worth reading. Silently restoring would have hidden it again.
 */
// Do not request the worker-scoped `studio` fixture here: staged-loading tests
// use `freshStudio` and are timing-sensitive; pulling `studio` into every test
// would instantiate the shared session for them. Write stacks are also appended
// to the trace file from `open()`, so the file is enough for this guard.
export async function openGraph(page: Page): Promise<void> {
  await revealView(page, "Gearbox Graph", ".gbx-svg");
}

/**
 * Put the Graph away.
 *
 * **A test that reloads with the Graph in front hands the next one a moving
 * shell.** `ShellLayoutRestorer` restores whatever was active, and the Graph is
 * `availableIn: all` with `lifetime: "context-kind"`, so the first reconcile
 * after a boot -- where `prev` is `undefined` -- deliberately keeps it: it is
 * valid on Home. The restorer then activates it, and that activation is bounded
 * by `waitForRevealed`, which polls with no timeout. Measured at 4.3 seconds
 * after `ready`, which is long after `settled()` returns and long after the next
 * test has revealed the Product -- the shell logged
 * `Widget was activated, but did not accept focus after 2000ms: gearbox.graph`
 * and `[data-resolved-profile]` sat hidden behind it for the full minute.
 *
 * None of that is the application misbehaving: a person who reloads with the
 * Graph on screen should get the Graph back. It is a test leaving a screen that
 * takes the room in front of the reload it performs for an unrelated reason.
 *
 * By the tab's close icon rather than the toggle command, for the reason
 * [`revealView`] gives about the palette being a second stateful thing to get
 * wrong. Absent tab is not an error: the caller wants it gone, and it is.
 */
export async function closeGraph(page: Page): Promise<void> {
  const tab = page.locator('[id="shell-tab-gearbox.graph"]');
  if ((await tab.count()) === 0) return;
  await tab.locator(".lm-TabBar-tabCloseIcon").click();
  await page.locator(".gbx-widget-graph").waitFor({ state: "detached", timeout: 30_000 });
}

/**
 * Open the Graph panel and switch it to one of its four views.
 *
 * The switch is a button rather than a Theia tab, so this clicks it and then
 * waits for that view's own root -- waiting on `.gbx-svg` alone would pass
 * against the view that was already showing.
 */
export async function openGraphView(
  page: Page,
  view: "deps" | "contracts" | "applications" | "cluster",
): Promise<void> {
  await openGraph(page);
  await page.locator(`.gbx-widget-graph .gbx-view-tab[data-view="${view}"]`).click();
  await page
    .locator(`.gbx-widget-graph [data-graph="${view}"], .gbx-widget-graph .gbx-empty`)
    .first()
    .waitFor({ state: "visible" });
}

/**
 * Bring the Inspector to the front, and wait for its "why" section.
 *
 * `Gearbox Explain` was a panel of its own until the two bottom panels became one
 * Inspector. The section kept its class, so everything asserted about the
 * explanation still asserts the same markup -- only the tab it lives behind
 * changed.
 */
export async function openExplain(page: Page): Promise<void> {
  await revealInspector(page);
}

/**
 * Open the Problems view and read the Gearbox markers in it.
 *
 * Two Theia behaviours to respect. Clicking the *already current* tab of a
 * bottom-panel view collapses the panel, so the tab is only clicked when
 * Problems is not already showing. And the marker tree is rebuilt
 * asynchronously after `setMarkers`, so the node list is read until it stops
 * changing -- reading once returned a half-updated tree, which looked exactly
 * like markers that had not been replaced.
 */
export async function problems(
  page: Page,
): Promise<{ files: string[]; markers: string[] }> {
  // Opened rather than assumed, since Problems stopped opening itself: on Home
  // there is no resolution to report on, and Conflicts is the domain screen for
  // the array this view also receives (`HiddenProblemsView`). The tab exists once
  // something has asked for it.
  const tab = page.locator("#theia-bottom-content-panel .lm-TabBar-tab", {
    hasText: "Problems",
  });
  if ((await tab.count()) === 0) {
    await runCommand(page, "Problems");
  } else if (!(await tab.evaluate((e) => e.classList.contains("lm-mod-current")))) {
    await tab.click();
  }
  await page.locator(".theia-marker-container").waitFor({ state: "visible", timeout: 30_000 });

  const read = () =>
    page.evaluate(() =>
      Array.from(document.querySelectorAll(".theia-marker-container .theia-TreeNode")).map((e) =>
        (e.textContent ?? "").replace(/\s+/g, " ").trim(),
      ),
    );
  let previous = await read();
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await page.waitForTimeout(250);
    const nodes = await read();
    if (nodes.length === previous.length && nodes.every((n, i) => n === previous[i])) {
      // A file node names a path; a marker node is the message. Split on that
      // rather than on tree depth, which Theia renders with padding rather than
      // with a class.
      return {
        files: nodes.filter((text) => /\.gdl|\.lock|\.rs\b/.test(text)),
        markers: nodes.filter((text) => !/\.gdl|\.lock|\.rs\b/.test(text)),
      };
    }
    previous = nodes;
  }
  throw new Error("the Problems tree never settled");
}

/**
 * Bring the Gear detail panel to the front.
 *
 * It shares the bottom panel with Problems and with any terminal, so opening
 * either hides it -- ordinary IDE behaviour, and the reason a test that clicks a
 * link *inside* the detail panel has to say which tab it wants first. Playwright
 * waits for visibility before clicking, so without this the click hangs until the
 * test times out, which looks nothing like "the wrong tab is showing".
 */
/**
 * Bring a left-panel view to the front, by tab label.
 *
 * Needed because the left panel now holds four tabs -- Explorer, Catalogue,
 * Search, Source Control -- and only one is current. Anything asserting that a
 * catalogue row is *visible* has to say so first, and clicking the already
 * current tab would collapse the panel instead.
 */
/**
 * Bring a left-panel view forward, opening the panel if it is folded.
 *
 * **`lm-mod-current` cannot tell folded from open, and that cost a suite run.**
 * `SidePanelHandler.collapse()` nulls the tab bar's `currentTitle`, but one tab
 * keeps the DOM class -- so a helper that clicked only when the wanted tab was
 * *not* current expanded the panel when asked for any other view and left it
 * folded when asked for that one. With Home folding the left panel, the Explorer
 * claim failed intermittently for that reason and the catalogue claims never
 * did, which is exactly the shape of an ordering flake.
 *
 * A collapsed side panel is its tab bar and nothing else, so comparing the panel
 * to the bar answers the real question and says nothing about how wide a person
 * has dragged it.
 */
export async function revealLeft(page: Page, label: string | RegExp): Promise<void> {
  const folded = (): Promise<boolean> =>
    page.evaluate(() => {
      const panel = document.querySelector("#theia-left-content-panel") as HTMLElement | null;
      if (panel === null) return true;
      const bar = panel.querySelector(".lm-TabBar") as HTMLElement | null;
      return panel.offsetWidth <= (bar?.offsetWidth ?? 0) + 8;
    });

  const tab = page.locator("#theia-left-content-panel .lm-TabBar li", { hasText: label }).first();
  await tab.waitFor({ state: "visible", timeout: 30_000 });
  const current = await tab.evaluate((e) => e.classList.contains("lm-mod-current"));
  if (!current || (await folded())) {
    await tab.click();
  }
  // Clicking the tab of a folded panel opens it, so the wait is for the panel to
  // have content rather than for the click to have landed.
  await expect.poll(folded, { timeout: 30_000 }).toBe(false);
}

export const revealCatalogue = (page: Page): Promise<void> =>
  revealLeft(page, "Gearbox Catalogue");

/**
 * Put the catalogue back where a row lookup can find anything.
 *
 * The catalogue now folds by category and filters by text, and both remove rows
 * from the DOM -- correctly, that is what they are for. So any test that looks a
 * gear up by name has to restore the precondition rather than inherit whatever an
 * earlier test left. Found the hard way: one test folded a category and the next
 * one's `detailOf("Payments (example provider)")` returned null.
 */
export async function resetCatalogueView(page: Page): Promise<void> {
  const filter = page.locator(".gbx-widget-catalogue .gbx-filter");
  if ((await filter.count()) > 0 && (await filter.inputValue()) !== "") {
    await filter.fill("");
  }
  const folded = page.locator('.gbx-widget-catalogue .gbx-group-label[data-collapsed="true"]');
  for (let attempt = 0; attempt < 12; attempt += 1) {
    if ((await folded.count()) === 0) return;
    await folded.first().click();
  }
}

/**
 * Expand the Explorer until `file` is visible, and return its node.
 *
 * Not a list of path segments, and that is the point. Theia collapses chains of
 * single-child directories into one node, so `.gearbox/payments-demo/dev` is
 * *one* row -- but only while the intermediate directories are unexpanded. Once
 * something has expanded `.gearbox`, the same path renders as several rows. So
 * the node text depends on the tree's remembered state, and matching it exactly
 * is a test that passes or fails on what an earlier test happened to click.
 *
 * Instead: expand whatever is collapsed and looks like it leads there, until the
 * file appears. Also expands only collapsed nodes -- a click on an expanded one
 * *collapses* it, which is the same toggle hazard as the view commands in a third
 * disguise.
 */
export async function revealInExplorer(
  page: Page,
  root: string,
  pathContains: string | readonly string[],
  file: string,
): Promise<Locator> {
  await revealLeft(page, "Explorer");
  // Git's decoration letter is part of the same text node (`fileU`, `fileM`), so
  // an exact name match misses the row the moment the provider has done its job
  // -- which is exactly when the decorate claim runs after the change-count one.
  const target = page.locator(".theia-TreeNode", {
    hasText: new RegExp(`^${escapeForRegExp(file)}[MUAD]?$`),
  });
  const hints = typeof pathContains === "string" ? [pathContains] : [...pathContains];

  for (let round = 0; round < 10; round += 1) {
    if ((await target.count()) > 0 && (await target.first().isVisible())) {
      return target.first();
    }
    // The *index* is chosen in the page and the click is done by Playwright. A
    // `HTMLElement.click()` on the node div does nothing: Theia's tree listens on
    // an inner caption element, and Playwright clicks the centre of the row, which
    // lands on it.
    const index = await page.evaluate(
      ({ rootName, hints: pathHints, fileName }) => {
        const nodes = Array.from(document.querySelectorAll(".theia-TreeNode"));
        // Expansion state lives on the chevron, not on the row: an expanded node
        // is one whose `.theia-ExpansionToggle` has lost `theia-mod-collapsed`.
        // Reading it off the row instead made every node look collapsed, so the
        // loop clicked the root open and then closed again, forever.
        const collapsed = (node: Element) =>
          node.className.includes("theia-ExpandableTreeNode") &&
          node.querySelector(".theia-ExpansionToggle.theia-mod-collapsed") !== null;
        const text = (node: Element) => (node.textContent ?? "").trim();
        const pick = nodes.findIndex((node) => collapsed(node) && text(node) === rootName);
        if (pick >= 0) return pick;
        return nodes.findIndex(
          (node) =>
            collapsed(node) &&
            pathHints.some((hint) => text(node).includes(hint)) &&
            text(node) !== fileName,
        );
      },
      { rootName: root, hints, fileName: file },
    );
    if (index < 0) break;
    await page.locator(".theia-TreeNode").nth(index).click();
    await page.waitForTimeout(500);
  }

  const visible = await page
    .locator(".theia-TreeNode")
    .allTextContents()
    .then((all) => all.map((one) => one.trim()));
  throw new Error(`never reached ${file}; the tree shows: ${visible.join(", ")}`);
}

function escapeForRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Bring the Inspector to the front, by tab.
 *
 * Waits for the **panel**, not for either of its sections. That distinction cost a
 * whole suite run: `.gbx-detail` and `.gbx-explain` render only once something is
 * selected, so waiting for one here waited thirty seconds for a selection the
 * caller had not made yet -- and `detailOf` reveals the panel *before* clicking a
 * row, which is every gear-facts claim in the suite.
 *
 * By tab rather than by command for the reason `revealView` records: the tab is
 * what Theia reliably listens to, and the toggle command on an already-current
 * view closes it. Returns silently when the tab does not exist, because a caller
 * that then waits on a section gets a better failure than this could produce.
 */
/**
 * Make sure something is selected, so subject-gated surfaces are reachable.
 *
 * A catalogue row is the cheapest subject there is: it needs no product, and the
 * catalogue is always loaded. Returns immediately when a selection already
 * exists -- most callers arrive with one.
 */
export async function ensureSelection(page: Page): Promise<void> {
  // **The toolbar, not the Inspector, and the difference is a whole class of
  // silent failure.** This asked whether a *row* was selected first, which is
  // false for every selection made anywhere else -- a process, a binding, a
  // conflict's subject -- so it helpfully clicked a row and destroyed the
  // selection the caller had just made. The fix was to ask the Inspector what it
  // was inspecting, which is the right question asked of the wrong witness: the
  // Inspector is a panel a person can close, and one that no longer opens itself
  // on a selection. Asked of it, "is anything selected" silently means "is the
  // Inspector attached" -- and the fall-through is that same row click, against
  // callers that reveal the Inspector *after* selecting in the product.
  //
  // The toolbar is always attached and publishes the fact directly.
  const selected = page.locator('.gbx-toolbar[data-has-selection="true"]');
  if ((await selected.count()) > 0) return;
  await revealCatalogue(page);
  const row = page.locator(".gbx-widget-catalogue .gbx-row").first();
  await row.waitFor({ state: "visible", timeout: 60_000 });
  await row.click();
}

/**
 * Open the "Other keys" section, where free-form config keys live.
 *
 * They moved under a `<details>` because a gear with no schema still offered a
 * bare `Add key`, so the obvious thing to do with it was type a key the gear does
 * not read -- and the only answer was a refusal about the whole proposal. A
 * control that is available invites use.
 *
 * Which means a test that types a free key now does what a person does: opens the
 * section first. Idempotent, and cheap when it is already open -- it is open
 * whenever it holds something, because a key somebody set is not advanced any
 * more.
 */
export async function openAdvancedKeys(page: Page, within: string): Promise<void> {
  const details = page.locator(`${within} details.gbx-advanced`).first();
  await details.waitFor({ state: "attached", timeout: 30_000 });
  if (await details.evaluate((e) => (e as HTMLDetailsElement).open)) return;
  await details.locator("summary").click();
  await expect
    .poll(async () => details.evaluate((e) => (e as HTMLDetailsElement).open), { timeout: 15_000 })
    .toBe(true);
}

export async function revealInspector(page: Page): Promise<void> {
  await refuseIfDialogOpen(page, "revealing the Inspector");
  // **A subject first, because the panel is gated on having one.** The Inspector
  // answers about a selection, so with none it is not in the palette and not in
  // `Open View...` -- a panel whose entire content is "select something" is worse
  // than an absent one. That is the gate working, not a helper to loosen, so this
  // does what a person does: pick something, then ask for the panel about it.
  // Cheap when a selection already exists, which is the ordinary case.
  await ensureSelection(page);
  // The right panel since 2026-09-07, and not pinned to it here: a returning
  // person's saved layout may still hold the Inspector at the bottom, and this
  // helper's job is to bring the panel forward wherever the shell has it.
  const tab = page.locator(".lm-TabBar-tab", { hasText: "Gearbox Inspector" }).first();
  if ((await tab.count()) === 0) {
    // Not in any tab bar: a collapsed side panel keeps its tab, but a view that
    // was never opened has none, so the command is the only way in.
    await runCommand(page, "Gearbox Inspector");
  } else if (!(await tab.evaluate((e) => e.classList.contains("lm-mod-current")))) {
    await tab.click();
  }
  await page.locator(".gbx-inspector").waitFor({ state: "visible", timeout: 30_000 });
}

/**
 * The Inspector, by its old name.
 *
 * Kept as an alias because the callers ask for "the panel with the gear facts in
 * it", which is what this is -- one panel now, two sections.
 */
export async function revealDetail(page: Page): Promise<void> {
  await revealInspector(page);
}

/**
 * Open the Conflicts screen.
 *
 * By command, because unlike the Inspector this panel is not opened at startup --
 * a panel that appears to say "no conflicts" says nothing -- so there is no tab to
 * click until something has asked for one.
 */
export async function openConflicts(page: Page): Promise<void> {
  await revealView(page, "Gearbox Conflicts", ".gbx-conflicts");
}

/**
 * The Lock view, left on whichever half it is showing.
 *
 * `revealLock` asks for the canonical text, because every caller it has wants
 * the text. This one is for the claim about the *summary* being what opens.
 */
export async function revealLockView(page: Page): Promise<void> {
  await revealView(page, "Resolution Lock", ".gbx-lock");
  await page.locator(".gbx-lock").waitFor({ state: "visible", timeout: 60_000 });
}

export async function revealLock(page: Page): Promise<void> {
  await revealView(page, "Resolution Lock", ".gbx-lock");
  // The view opens on its Summary half now -- the canonical text is several
  // hundred lines and answers the second question a person asks. Every caller of
  // this helper wants the text, so it asks for it; the summary has a claim of its
  // own in `prd-lock.spec.ts`.
  //
  // **Waited for, not clicked if it happens to be there.** The halves exist only
  // once the lock has arrived, because the text is fetched lazily on first
  // render -- so a conditional click skipped itself on a freshly opened view and
  // left the wait below with nothing to wait for. It passed for as long as some
  // earlier test in the shared session had already opened this view, which is an
  // ordering the suite does not promise.
  const raw = page.locator('[data-lock-tab="raw"]');
  await raw.waitFor({ state: "visible", timeout: 60_000 });
  await raw.click();
  // The text is fetched lazily on first render, so the view being visible is not
  // the same as the lock being there.
  await page.locator("[data-lock-canonical]").waitFor({ state: "visible", timeout: 60_000 });
}

/**
 * Open the Product view and resolve one profile.
 *
 * Waits on `data-resolved-profile`, which the widget reads off the resolved
 * header rather than off the profile switch. Waiting on the switch instead would
 * pass the moment the button lights up, which happens before the resolution
 * lands -- so the test would read the previous profile's answer.
 */
export async function openGenerate(page: Page): Promise<void> {
  await runCommand(page, "Gearbox: Show Generate");
  await expect(page.locator(".gbx-embedded-generate")).toBeVisible();
}

/**
 * Wait for the shell to report the context it is in.
 *
 * There is no control that sets a context: it is derived from what is open, which
 * is the whole point of `StudioContextService`. So a test does not *switch* a
 * context -- it opens something and waits for the shell to agree. This replaced a
 * `switchPerspective` helper that clicked buttons which no longer exist, and it
 * asserts the same property more honestly: the header's `data-context` is set from
 * the service, not from whatever layout happened to be restored.
 */
export async function expectContext(
  page: Page,
  kind: "home" | "product" | "gear",
): Promise<void> {
  await expect(page.locator(".gbx-toolbar")).toHaveAttribute("data-context", kind, {
    timeout: 30_000,
  });
}

/**
 * Move the Product view to one of its stages.
 *
 * The panel is `Overview · Gears · Topology · Validation` since 2026-09-07: it
 * had grown to the whole product on one strip, which a UX pass reported as a
 * very long screen with no sense of where one is. A claim about a fact the
 * product renders therefore has to say which stage renders it -- which is worth
 * the extra line, because it is also the claim that the fact is *reachable*.
 */
export async function productSection(
  page: Page,
  section: "overview" | "composition" | "topology" | "validation" | "checks" | "generate" | "settings",
): Promise<void> {
  if (section === "overview" || section === "settings") {
    await page.locator('.gbx-product-menu > summary').click();
    await page.getByRole("button", { name: "Product settings", exact: true }).click();
    await expect(page.getByRole("heading", { name: "Product settings", exact: true })).toBeVisible();
    return;
  }
  const tab = page.locator(`[data-product-section="${section === "validation" ? "checks" : section}"]`);
  await tab.waitFor({ state: "visible", timeout: 30_000 });
  await tab.click();
  await expect(tab).toHaveAttribute("aria-selected", "true");
}

/**
 * Open a *named* product, whichever one is open now.
 *
 * **Which product, not just whether one is open.** This used to be `openProduct`
 * with `payments-demo` written into it and a short-circuit on
 * `data-context === "product"` -- which was fine while the corpus had one
 * product and silently wrong the moment it had two: a claim that opened
 * `configurable-gears` left it open, and the next test's `openProduct` saw a
 * product context, returned, and read the wrong description. Three claims failed
 * somewhere else entirely, which is the expensive kind of wrong.
 *
 * So the check is `[data-product-name]`, the id the description names itself by.
 * Closing first is unavoidable when it disagrees: the picker is reachable from
 * Home. That makes this helper the wrong instrument for a claim *about* the
 * product-to-product transition, which must not pass through Home -- such a
 * claim drives the picker itself.
 *
 * `fragment` matches the picker's path column (`products/<name>/product.gdl`)
 * as well as the id, because the display label is prose: `Configurable Gears`
 * for `configurable-gears`.
 */
export async function openProductById(
  page: Page,
  fragment: string,
  profile: string,
): Promise<void> {
  const context = (): Promise<string | null> =>
    page.locator(".gbx-toolbar").getAttribute("data-context");

  if ((await context()) === "product") {
    await revealView(page, "Gearbox Product", ".gbx-product");
    const openNow = await page
      .locator("[data-product-name]")
      .getAttribute("data-product-name")
      .catch(() => null);
    if (openNow !== fragment) {
      await runCommand(page, "Close Product");
      await expectContext(page, "home");
    }
  }

  if ((await context()) !== "product") {
    // The Continue card only when it names the product wanted -- it opens the
    // most recent one, which after a switch is the other one.
    // By its path, not its text: the card shows the display name, which is
    // prose (`Payments Demo`), while `fragment` is an id or a path fragment.
    const card = page.locator('[data-start-action="continue"]');
    const recent = (await card.isVisible().catch(() => false))
      ? ((await page.locator("[data-start-continue]").getAttribute("data-start-continue")) ?? "")
      : "";
    if (recent.includes(fragment)) {
      await card.click();
    } else {
      await runCommand(page, "Open Product…");
      const options = page.locator(`.quick-input-list [role="option"]`);
      await options.first().waitFor({ state: "visible", timeout: 30_000 });
      // Never `.first()` blindly: the list opens with a `Choose product.gdl…`
      // entry, so an unfiltered first click picks the file dialog.
      await options.filter({ hasText: fragment }).first().click();
    }
    await expect(page.locator(".gbx-toolbar")).toHaveAttribute("data-context", "product", {
      timeout: 90_000,
    });
  }

  // **The reveal stays.** It is not the claim about automatic placement --
  // `conformance/ux-navigation.spec.ts` asserts that with no reveal at all --
  // it is protection against the tab a *previous* test left on top in the shared
  // worker session, and the measurement in `revealView` (19.6 minutes against
  // 3.3) is why it is a tab click rather than a command.
  await revealView(page, "Gearbox Product", ".gbx-product");
  await expect(page.locator("[data-product-name]")).toHaveAttribute(
    "data-product-name",
    fragment,
    { timeout: 90_000 },
  );
  // **Overview, because the resolved header lives there.** The panel keeps
  // whichever stage was last chosen -- correct behaviour, and it means a helper
  // that waits for `[data-resolved-profile]` would be waiting for a section a
  // previous test navigated away from. This is the helper that establishes a
  // known state, so it establishes this part of it too; the profile switch
  // itself sits above the strip and works from any stage.
  await productSection(page, "overview");
  await page.locator("[data-resolved-profile]").waitFor({ state: "visible", timeout: 60_000 });
  await page.locator("[data-product-profile]").selectOption(profile);
  await page
    .locator(`[data-resolved-profile="${profile}"]`)
    .waitFor({ state: "visible", timeout: 60_000 });
}

/**
 * Select a gear in the Composition tree and return *its* settings form.
 *
 * The click is `Configure <gear>`, which sets the selection the settings half
 * renders from -- not the `<summary>`, which only folds the branch.
 *
 * **Returns a scoped locator, and that is not a convenience.** `GearSettings` is
 * one component rendered by two surfaces: the Composition pane and the
 * Inspector, which opens itself on every selection. So `[data-gear-config=…]`
 * and every marker inside it match twice, and an unscoped locator is a strict
 * mode violation rather than a wrong answer -- which is the good failure, but a
 * failure. Callers assert through the returned form.
 */
export async function configureGear(page: Page, gear: string): Promise<Locator> {
  await productSection(page, "composition");
  await page.locator(`[data-composition-gear="${gear}"]`).click();
  const form = page.locator(`.gbx-composition-settings [data-gear-config="${gear}"]`);
  await form.waitFor({ state: "visible", timeout: 60_000 });
  return form;
}

/**
 * Select one plugin connection under a host and return *its* settings form.
 *
 * Addressed the way the editor addresses it: `(host, entryIndex)`, where the
 * index is the position the `plugin(...)` entry is *written* at. The plugin id
 * alone will not do -- a host may hold the same implementation twice for
 * disjoint profiles, which is what per-connection scopes are for.
 *
 * Scoped for the reason `configureGear` gives: `PluginSettings` is one component
 * rendered by the Composition pane and by the Inspector, so an unscoped
 * `[data-plugin-settings=…]` is a strict mode violation rather than a wrong
 * answer.
 */
export async function configureConnection(
  page: Page,
  host: string,
  entryIndex: number,
): Promise<Locator> {
  await productSection(page, "composition");
  const row = page.locator(
    `[data-plugin-host="${host}"][data-plugin-index="${String(entryIndex)}"]`,
  );
  const plugin = await row.getAttribute("data-plugin-id");
  await row.locator("button").first().click();
  const form = page.locator(
    `.gbx-composition-settings [data-plugin-settings="${String(plugin)}"]`,
  );
  await form.waitFor({ state: "visible", timeout: 60_000 });
  return form;
}

/**
 * Open the corpus's real product, `payments-demo`.
 *
 * A product is opened here, not inherited from boot: Studio used to open the
 * only product it could find whenever the Product widget was constructed, so
 * every test started with one open and this helper only had to reveal the view.
 * Home is a deliberate starting point now, so the helper does what a person
 * does.
 */
export async function openProduct(page: Page, profile: string): Promise<void> {
  await openProductById(page, "payments-demo", profile);
}

export const test = base.extend<
  { freshStudio: Studio; stalledStudio: Studio; rpc: RpcControl; guarded: void },
  { studio: Studio }
>({
  /**
   * The per-test guards, and they are a fixture because hooks did not work.
   *
   * **`base.afterEach` at the top level of *this* file reached one spec file per
   * worker.** Playwright attaches a hook declared while a module is loading to
   * the suite of the file that triggered the load, and this module is imported
   * once and cached — so the guard that names the test which left a description
   * dirty was watching the first file loaded and none of the other nineteen.
   * Proved rather than reasoned: two probe specs, the second one leaving
   * `products/` dirty, both passed, and only `global-teardown` noticed.
   *
   * That is also why a stray write had no test attached to it. The guard could
   * not see the run.
   *
   * An automatic fixture follows the `test` object instead of the file, so it
   * runs for every claim in every spec. Being automatic it is also set up first
   * and therefore torn down **last**, after `freshStudio` has closed its page —
   * which is the window a late write lands in.
   *
   * It deliberately does not request `studio`: the staged-loading claims use
   * `freshStudio` and are timing-sensitive, and pulling the shared session into
   * every test would boot it for them. Write stacks reach the trace file from
   * `open()`, so the file is enough.
   */
  guarded: [
    async ({}, use, testInfo) => {
      noteTest("begin", testInfo.title);
      await use();
      noteTest("end", testInfo.title);

      // **Three levels, not two.** This file is `tests/fixtures/studio.ts`, so
      // `../..` is `ide` — where there is no `products` directory at all, and
      // `git status -- products` answers with silence. The corpus half was wrong
      // the same way and swallowed its own error. Between that and the hook
      // scope above, this guard has never once fired.
      const repo = join(__dirname, "../../..");
      await flushWriteTraces();
      // A claim's own copy is expected dirt: it is untracked by design and
      // goes when the claim does. Anything else is the defect this names.
      const dirty = productsStatus(repo)
        .split("\n")
        .filter((line) => line !== "" && !liveCopies().some((copy) => line.includes(copy)))
        .join("\n");
      const traceBlock = formatWriteTraces(readWriteTraces());
      if (dirty !== "") {
        const diff = productsDiff(repo, dirty);
        restoreProducts(repo);
        resetWriteTraces();
        throw new Error(
          `"${testInfo.title}" left the product descriptions changed:\n${dirty}\n\n${diff}\n\n` +
            traceBlock +
            `The tree has been restored. Three claims edit a description on purpose and put ` +
            `it back; anything else writing there is the defect this guard exists to name.`,
        );
      }
      resetWriteTraces();

      // The same question for the gear descriptions this suite edits, one
      // repository over. Checked after the products half rather than beside it:
      // the two have different remedies — that one may remove untracked
      // entries, this one must never touch anything but the files it names.
      const corpus = corpusStatus(repo);
      if (corpus !== "") {
        const diff = corpusDiff(repo);
        restoreCorpus(repo);
        throw new Error(
          `"${testInfo.title}" left a gear description changed:\n${corpus}\n\n${diff}\n\n` +
            `The named files have been restored. Two claims rewrite a \`gear.gdl\` on purpose ` +
            `and put it back in a \`finally\`; anything else writing there is the defect this ` +
            `guard exists to name.`,
        );
      }
    },
    { auto: true },
  ],

  studio: [
    async ({ browser }, use) => {
      const { studio, close } = await open(browser);
      await settled(studio.page);
      await use(studio);
      await close();
    },
    { scope: "worker" },
  ],

  freshStudio: async ({ browser }, use) => {
    const { studio, close } = await open(browser);
    await use(studio);
    await close();
  },

  /**
   * A controller for the RPC between this page and the backend.
   *
   * Requested *before* `stalledStudio` by depending on it, so the route is on
   * the context when the page is created. A claim that only wants a page uses
   * `freshStudio`; this pair exists for the claims that need an answer to be
   * outstanding while they look at the screen.
   */
  rpc: async ({}, use) => {
    await use(new RpcControl());
  },

  stalledStudio: async ({ browser, rpc }, use) => {
    const { studio, close } = await open(browser, (context) => rpc.install(context));
    await use(studio);
    await close();
  },
});

export { expect } from "@playwright/test";
