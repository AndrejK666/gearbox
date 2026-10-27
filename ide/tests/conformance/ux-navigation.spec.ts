// Navigation as an atomic act: opening a product ends in the Product workspace,
// and a reload ends on Home.
//
// Both claims come from the UX pass of 2026-09-07, and both were unobservable
// before it. The mechanism they guard is in `StudioContextService.recompute`:
// `PerspectiveService.switchPerspective` returns early when its target is
// already the active perspective, and `ShellLayoutRestorer` sets that id from
// persisted state before any Gearbox code runs. So a session that had a product
// open came back with `gearbox.product` active and no product open -- and the
// *next* open asked for the perspective the shell already believed was active,
// got no `onActivate`, and left the Start screen in the centre with the header
// saying `payments-demo`.
//
// The suite could not see it for a second reason worth keeping: `openProduct`
// in the fixture calls `revealView`, which performs the missing step itself.
// These two tests therefore drive the shell the way a person does and assert
// visibility with no reveal anywhere.

import { readFileSync, writeFileSync } from "node:fs";

import { restoreCorpus } from "../fixtures/corpus-files";
import { copyProduct, type ProductCopy } from "../fixtures/product-copy";
import { restoreProducts } from "../fixtures/products-tree";
import { join } from "node:path";

/** The repository root, for the one claim that edits a description on disk. */
const REPO = join(__dirname, "../../..");

import {
  expect,
  expectContext,
  openGraph,
  configureGear,
  openProduct,
  openProductById,
  productSection,
  resetCatalogueView,
  revealCatalogue,
  revealInspector,
  runCommand,
  settled,
  test,
} from "../fixtures/studio";

const IDE = join(__dirname, "../..");

/**
 * Take width from the Product panel by dragging the sash beside it.
 *
 * The right-hand divider, found by geometry rather than by an index: Lumino
 * keeps a handle for every split including the collapsed ones, and only the
 * tall visible ones are dividers a person could drag.
 */
async function narrowThePane(
  page: import("@playwright/test").Page,
  by: number,
): Promise<void> {
  const handle = await page.evaluate(() =>
    Array.from(document.querySelectorAll(".lm-SplitPanel-handle"))
      .filter((h) => !h.classList.contains("lm-mod-hidden"))
      .map((h) => h.getBoundingClientRect())
      .filter((r) => r.height > 100)
      .sort((a, b) => b.x - a.x)
      .map((r) => ({ x: Math.round(r.x), y: Math.round(r.y + r.height / 2) }))[0],
  );
  expect(handle, "the Inspector has to be resizable for this to be set up").toBeDefined();
  await page.mouse.move(handle.x, handle.y);
  await page.mouse.down();
  await page.mouse.move(handle.x - by, handle.y, { steps: 12 });
  await page.mouse.up();
  await expect
    .poll(async () => Math.round((await page.locator(".gbx-product").boundingBox())?.width ?? 0), {
      timeout: 15_000,
    })
    .toBeLessThan(900);
}

/** The Composition grid as it actually lays out, with the room it was given. */
async function measurePane(
  page: import("@playwright/test").Page,
): Promise<{ window: number; pane: number; columns: number }> {
  return page.evaluate(() => {
    const pane = document.querySelector(".gbx-product");
    const grid = document.querySelector(".gbx-composition");
    return {
      window: window.innerWidth,
      pane: Math.round(pane?.getBoundingClientRect().width ?? 0),
      // The computed value is the resolved track list, so its length is the
      // number of columns the browser actually used.
      columns: grid ? getComputedStyle(grid).gridTemplateColumns.split(" ").length : 0,
    };
  });
}

test.describe("Home is the start screen and nothing else", () => {
  test("nothing opens itself into the bottom panel on Home [plan §9.1: an empty domain panel is worse than an absent one]", async ({
    freshStudio,
  }) => {
    // Four panels used to open themselves on a first run: Problems, Outline, the
    // Inspector and -- before `HiddenTerminal` -- a `zsh`. None of them has a
    // subject on Home. Problems has no resolution to report on, an outline of a
    // `product.gdl` is a shorter Product tree, and the Inspector's whole content
    // with nothing selected is an invitation to select something. §9.1 already
    // calls an empty domain panel worse than an absent one; this is that rule
    // applied to the panels this application did not choose.
    //
    // The catalogue is deliberately **not** part of this claim. It stays on Home
    // as the secondary "Browse" half that ADR-0011's amendment describes, and
    // ADR-0009's claims are about watching it load.
    const { page } = freshStudio;
    await settled(page);
    await expect(page.locator(".gbx-toolbar")).toHaveAttribute("data-context", "home");

    const bottom = await page.evaluate(() =>
      Array.from(
        document.querySelectorAll("#theia-bottom-content-panel .lm-TabBar-tabLabel"),
      ).map((e) => (e.textContent ?? "").trim()),
    );
    expect(bottom).toEqual([]);
  });
});

test.describe("a screen belongs to a subject", () => {
  test("closing a product takes its screens with it [ADR-0011 §Amendment: a screen belongs to a subject]", async ({
    studio,
  }) => {
    // The finding this is built from, verbatim: "after Close the product closes,
    // but the active tab stays an empty `Gearbox Product`". The cause is not a
    // stray widget -- it is that nothing ever decided a screen may *stop* being
    // allowed. A menu is re-read each time it opens; a widget already on screen
    // is never asked again.
    //
    // **Read with Generate rather than Add Gear.** Add Gear was the example here
    // because it was the harder half -- the Product view at least renders a
    // picker with no product, whereas the configurator held a proposal composed
    // against one, and `ProductEditService` resolves its target when it commits
    // rather than when it was staged. Adding is a modal dialog now, so it is not
    // a screen that could outlive anything: it captures the product path, and
    // closes itself when the open product changes. The dangerous half is closed
    // at the source rather than by withdrawal, and the claim here is carried by
    // Generate, which is still a product-scoped screen in the main area.
    // See `cpt-gearbox-adr-domain-specific-ide-shell` Amendment 2026-09-18.
    const { page } = studio;
    await openProduct(page, "dev");
    await runCommand(page, "Gearbox: Show Generate");
    await expect(page.locator(".gbx-embedded-generate")).toBeVisible({ timeout: 60_000 });

    await page.locator('[data-command="gearbox.product.close"]').click();
    await expectContext(page, "home");

    const main = await page.evaluate(() =>
      Array.from(document.querySelectorAll("#theia-main-content-panel .lm-TabBar-tabLabel")).map(
        (e) => (e.textContent ?? "").trim(),
      ),
    );
    expect(main, "a product's screens outlived the product").not.toContain("Gearbox Product");
    expect(main, "a product's screens outlived the product").not.toContain("Gearbox Generate");
    // And the context's own screen is in front, rather than whichever sibling
    // Lumino picked when the active tab closed -- the rule that took ten Add Gear
    // claims down the last time anything here was closed.
    await expect(page.locator(".gbx-start")).toBeVisible();
  });
});

test.describe("a screen that wants the room", () => {
  test("the panels come back when the last such screen closes, not the first [ADR-0011 §Amendment: the room is arranged before the screen appears]", async ({
    freshStudio,
  }) => {
    // Found by using the thing: `Add Gear -> Graph -> close Add Gear` gave the
    // panels back while the Graph -- which had asked for the room in the same
    // episode -- was still the screen in front of them. The service tracked one
    // screen per episode, so the first close looked like the last.
    //
    // **Read with New Gear rather than Add Gear**, because adding a gear is a
    // modal dialog now and a dialog does not ask for the room -- it takes the
    // whole screen for as long as it is open and gives it back on its own. The
    // claim is about *focus screens*, of which there are still three; any two of
    // them exercise it, and the episode logic does not know which. See
    // `cpt-gearbox-adr-domain-specific-ide-shell` Amendment 2026-09-18.
    //
    // **Read by comparing the panel to its own tab bar**, which is what makes
    // this independent of how wide a person has dragged it: a collapsed side
    // panel is its tab bar and nothing else. `lm-mod-current` was tried first and
    // is not the signal -- `collapse()` nulls the tab bar's `currentTitle`, but
    // the DOM class stays, so it read as open at 49 pixels wide.
    // **A fresh app, and that is not caution -- the shared session cannot show
    // this.** An episode spans consecutive focus screens by design, so a Graph
    // that an earlier test left open keeps its episode running, and a later
    // `enterFocus` correctly does nothing. Correct behaviour, unobservable claim.
    const { page } = freshStudio;
    await settled(page);
    const leftOpen = (): Promise<boolean> =>
      page.evaluate(() => {
        const panel = document.querySelector("#theia-left-content-panel") as HTMLElement | null;
        if (panel === null) return false;
        const bar = panel.querySelector(".lm-TabBar") as HTMLElement | null;
        return panel.offsetWidth > (bar?.offsetWidth ?? 0) + 8;
      });

    await openProduct(page, "dev");
    await revealCatalogue(page);
    await resetCatalogueView(page);
    expect(await leftOpen(), "the catalogue must start open for this to mean anything").toBe(true);

    await runCommand(page, "Gearbox: New Gear");
    await expect(page.locator(".gbx-widget-create-gear")).toBeVisible({ timeout: 30_000 });
    expect(await leftOpen(), "opening a focus screen did not fold the catalogue").toBe(false);

    await openGraph(page);
    // Still one episode: opening a second such screen must not re-snapshot, or
    // the episode records "already folded" as the state it owes.
    expect(await leftOpen()).toBe(false);

    await page.locator('[id="shell-tab-gearbox.gear.create"] .lm-TabBar-tabCloseIcon').click();
    await expect(page.locator(".gbx-widget-create-gear")).toHaveCount(0);
    expect(
      await leftOpen(),
      "the panels came back while a screen that wanted the room was still open",
    ).toBe(false);

    await page.locator('[id="shell-tab-gearbox.graph"] .lm-TabBar-tabCloseIcon').click();
    await expect(page.locator(".gbx-widget-graph")).toHaveCount(0);
    expect(await leftOpen(), "the last close did not give the catalogue back").toBe(true);
  });
});

test.describe("the product has stages", () => {
  test("the Product view is four stages and a way out to Generate [plan §9.1: Product navigation]", async ({
    studio,
  }) => {
    // The panel had grown to the whole product on one strip -- header, actions,
    // profile switch, profile fields, four foldable branches and a diagnostics
    // line -- and a UX pass reported it as a very long screen with no sense of
    // where one is. The four names are the stages of composing a product: what
    // it *is*, what it is *made of*, how that *deploys*, and what is *wrong*.
    //
    // "Made of" is **Composition**, and it is where the view opens. It replaced
    // a Gears stage that listed the resolution's two buckets read-only: the same
    // question, but answered from the intent and answerable *into* -- a gear is
    // configured where it is seen, rather than in a panel somewhere else.
    //
    // Generate is the fifth stage and is a view of its own, so the strip links
    // out to it rather than reproducing a file plan and an Apply button in two
    // places. That is the difference this claim fixes in place: a tab would
    // become a second answer to the same question.
    await openProduct(studio.page, "dev");
    const strip = studio.page.locator(".gbx-product-nav");
    await expect(strip).toBeVisible();
    const tabs = await strip
      .locator("[data-product-section]")
      .evaluateAll((nodes) => nodes.map((node) => node.getAttribute("data-product-section")));
    expect(tabs).toEqual(["composition", "topology", "checks", "generate"]);
    // Which stage a product *opens* on is not asserted here: `openProduct`
    // establishes Overview on purpose, because the resolved header lives there.
    // The arriving stage is claimed in `regression.spec.ts`, which opens without
    // establishing one.

    // The profile switch and the diagnostics summary are above the strip, so they
    // survive a change of stage: the switch has to work while a resolution is in
    // flight, and one line saying the resolution complained belongs on every
    // stage rather than only on the one about complaints.
    await productSection(studio.page, "topology");
    await expect(studio.page.locator("[data-product-profile]")).toBeVisible();
    await expect(studio.page.locator(".gbx-product-status")).toBeVisible();
    await expect(studio.page.locator("[data-resolved-profile]")).toHaveCount(0);

    await expect(strip.locator("[data-product-section-generate]")).toBeVisible();
    await strip.locator("[data-product-section-generate]").click();
    await expect(studio.page.locator(".gbx-generate")).toBeVisible({ timeout: 60_000 });
  });
});

test.describe("validation is a stage, not a doorway", () => {
  test("Validation shows the diagnostics rather than a way to reach them [plan §9.1: Validation is a screen]", async ({
    studio,
  }) => {
    // The finding: a centre panel holding two counts and two nearly identical
    // buttons, while the rows carrying the code, the remedy and the location
    // lived only in the bottom panel. Asserted as the three things that changed:
    // the summary is there, the rows are there, and the duplicate button is not.
    const { page } = studio;
    await openProduct(page, "dev");
    await productSection(page, "validation");

    const stage = page.locator("[data-product-validation]");
    await expect(stage).toBeVisible();

    // The counts stay -- they are the orientation the list is read against.
    await expect(stage.locator("[data-validation-errors]")).toBeVisible();
    await expect(stage.locator("[data-validation-warnings]")).toBeVisible();

    // The rows themselves, in the stage rather than only in the bottom panel,
    // and each naming its code: a diagnostic without one cannot be looked up.
    const rows = stage.locator(".gbx-conflict");
    const count = await rows.count();
    expect(count, "this profile resolves with at least one diagnostic to show").toBeGreaterThan(0);
    const codes = await rows.evaluateAll((all) =>
      all.map((r) => r.getAttribute("data-conflict-code")),
    );
    expect(codes.every((code) => code !== null && code.length > 0)).toBe(true);

    // And the remedy, which is the half three of the four old renderers dropped.
    expect(await stage.locator(".gbx-conflict-help").count()).toBeGreaterThan(0);

    // The one-line summary is suppressed *here* and nowhere else: on this stage
    // it would be a second copy of the summary above, beside a button
    // duplicating the link below it.
    await expect(page.locator("[data-show-conflicts]")).toHaveCount(0);
    await productSection(page, "topology");
    await expect(page.locator(".gbx-product-status")).toBeVisible();
    await productSection(page, "validation");

    // **The demoted link still does what it says.** It survives because the
    // bottom panel is where the list is read *beside* the tree, so the assertion
    // is not that a panel appears but that it appears **there** -- a Conflicts
    // screen in the main area would have replaced the thing it is about.
    await stage.locator("[data-validation-open-conflicts]").click();
    await expect(page.locator("#theia-bottom-content-panel .gbx-conflicts")).toBeVisible({
      timeout: 30_000,
    });

    // And a row's location opens the description. Left until last because it puts
    // an editor in the main area, which is the one action here that navigates.
    //
    // The expected file is read from the link rather than written in: what the
    // claim is about is that the location resolves to the file it names. On this
    // corpus both dev diagnostics point at `product.gdl:1`.
    const where = stage.locator(".gbx-conflict-where").first();
    const label = ((await where.textContent()) ?? "").trim();
    const file = /([^/\s:]+\.gdl):\d+$/.exec(label)?.[1];
    expect(file, `a location link should name a .gdl file, got "${label}"`).toBeDefined();
    await where.click();
    await expect(
      page.locator("#theia-main-content-panel .lm-TabBar-tabLabel", { hasText: String(file) }),
    ).toBeVisible({ timeout: 30_000 });
  });

  test("what a proposal would introduce is the same row, at a smaller weight [plan §9.1: Validation is a screen]", async ({
    studio,
  }) => {
    // **Reported unobserved, and the reason is a fix rather than a gap.** This
    // used to drive `rg-tr-plugin`, the one gear whose addition introduced a
    // diagnostic -- and it introduced one *because the preview was describing the
    // wrong operation*: it asked the engine what a top-level `use_gear` of a
    // plugin would do, which is the form this build refuses to write. With the
    // preview computed from the batch that will actually be written, no proposal
    // on this corpus introduces a diagnostic, so there is no compact list to look
    // at. Tried all nine addable gears, plugins with a host chosen included.
    //
    // **The DOM half is back, because the corpus now has such a proposal.**
    // `GBX0120` reports a required configuration field that nothing supplies,
    // and `event-broker` is the gear that carries two of them: `mode` and
    // `default_storage_backend` are required, and `EventBrokerConfig` declares
    // no container default. So adding it without configuring it introduces a
    // diagnostic the current resolution does not have, which is exactly the
    // subtraction section 6 performs.
    const widget = readFileSync(
      join(IDE, "gearbox-studio/src/browser/add-gear/add-gear-dialog.tsx"),
      "utf8",
    );
    expect(widget, "Add Gear should render diagnostics with the shared row").toMatch(
      /<DiagnosticsList[\s\S]{0,200}density="compact"/,
    );

    const { page } = studio;
    await openProduct(page, "dev");
    await page.locator("[data-add-gear]").click();
    await expect(page.locator("[data-add-gear-flow]")).toBeVisible({ timeout: 30_000 });
    // A list of buttons rather than a `<select>`: the dialog shows each candidate
    // with its source and whether it is already in the product, which an option
    // element cannot carry.
    await page.locator('[data-add-gear-select="event-broker"]').click();

    const introduced = page.locator("[data-add-gear-impact-diagnostics]");
    await expect(introduced).toBeVisible({ timeout: 60_000 });
    // The shared row, at compact density -- the claim this test is for -- and
    // carrying its code, because a diagnostic nobody can look up is a sentence.
    const rows = introduced.locator(".gbx-conflict");
    expect(await rows.count()).toBeGreaterThan(0);
    await expect(introduced.locator('[data-conflict-code="GBX0120"]').first()).toBeVisible();

    await page.locator("[data-add-gear-cancel]").click();
  });
});

test.describe("an open says which part of it is slow", () => {
  test("the wait is four named steps that advance [plan §9.1: opening is staged]", async ({
    freshStudio,
  }) => {
    // Three seconds of `Loading payments-demo…` cannot tell a slow catalogue from
    // a description that will never evaluate, and an open is two engine spawns
    // plus a catalogue load. So the panel names the four steps and says which one
    // it is on.
    //
    // A fresh app and the picker, not the Continue card: there are no recents on
    // a first run, and clicking `Open Product…` is the path that exists.
    const { page } = freshStudio;
    await settled(page);
    await runCommand(page, "Open Product…");
    const options = page.locator(`.quick-input-list [role="option"]`);
    await options.first().waitFor({ state: "visible", timeout: 30_000 });
    await options.filter({ hasText: "payments-demo" }).first().click();

    // Sampled, because the claim is about *advancing* -- a single reading cannot
    // tell a checklist from a picture of one.
    const samples: { stage: string | null; done: number; steps: number; label: string }[] = [];
    for (let round = 0; round < 120; round += 1) {
      const snap = await page.evaluate(() => {
        const el = document.querySelector("[data-opening-stage]");
        return {
          stage: el?.getAttribute("data-opening-stage") ?? null,
          label: el?.getAttribute("data-product-opening") ?? "",
          steps: document.querySelectorAll("[data-step]").length,
          done: document.querySelectorAll("[data-step].gbx-opening-done").length,
        };
      });
      if (snap.stage !== null) samples.push(snap);
      if ((await page.locator("[data-resolved-profile]").count()) > 0) break;
      await page.waitForTimeout(80);
    }

    expect(samples.length, "the open finished without the panel ever saying so").toBeGreaterThan(0);
    // All four steps, always: a list that grew as it went would hide how much is
    // left, which is the question a wait raises.
    expect(new Set(samples.map((s) => s.steps))).toEqual(new Set([4]));
    // The product's name, because "Loading…" with no subject is what an
    // application that has lost track of itself says.
    expect(samples[0]?.label).toContain("payments-demo");
    // It starts on the first step with nothing ticked -- a checklist that
    // pre-ticks its steps is a progress bar in a costume -- and it advances.
    expect(samples[0]?.stage).toBe("workspace");
    expect(samples[0]?.done).toBe(0);
    expect(Math.max(...samples.map((s) => s.done)), "no step was ever completed").toBeGreaterThan(
      0,
    );
    expect(new Set(samples.map((s) => s.stage)).size, "the step never changed").toBeGreaterThan(1);

    // **And the branch that renders this comes before the one that renders a
    // product's error**, which is an ordering that broke once and cannot be
    // observed here: with a *failed* product in the store, `status === "error"`
    // answered first, so opening another product kept the old one's error on
    // screen for the whole open and then in place of the new one's refusal.
    //
    // Asserted on the source rather than through the shell, and the reason has
    // narrowed since it was written: it needs two products *and* a failure. A
    // second product is reachable -- `adr-0013-create-product.spec.ts` makes
    // them -- and since 2026-09-16 so is a failure, because a claim in
    // `regression.spec.ts` writes an error into the demo description and
    // restores it in a `finally`. What is left is that assembling both at once
    // is a fixture, not a claim, and an ordering bug of this shape returns
    // silently either way. Promotable to a behavioural claim; not promoted here.
    const widget = readFileSync(
      join(IDE, "gearbox-studio/src/browser/product/product-widget.tsx"),
      "utf8",
    );
    const openingBranch = widget.indexOf('opening.status !== "idle"');
    const errorBranch = widget.indexOf('state.status === "error"');
    expect(openingBranch, "the opening branch is gone").toBeGreaterThan(0);
    expect(errorBranch, "the error branch is gone").toBeGreaterThan(0);
    expect(
      openingBranch < errorBranch,
      "a product's error is rendered before another product's open, so it outlives its subject",
    ).toBe(true);
  });

  // **There is no browser claim for a refused open, and the reason is no longer
  // that one is impossible.** Every product in this corpus opens. Writing one
  // that does not evaluate was described here as putting it under `products/`,
  // "which `global-setup` refuses" -- that was too strong, and 2026-09-16 proved
  // it: `global-setup` refuses a `products/` that is *already* dirty when the
  // suite starts, and blesses a claim that edits a description and puts it back,
  // which is what `regression.spec.ts` now does to provoke an error. A GDL parse
  // or evaluation error written the same way would give a product that does not
  // open.
  //
  // Killing the engine looks like the other way in and is not: `initialize`
  // spawns a new engine on every call, so an open that begins with
  // `catalogue.load` gets a fresh one and succeeds -- verified by trying it, and
  // the panel duly never reported a failure.
  //
  // So the failure paths are checked deterministically in
  // `scripts/store-smoke.mjs` against `shell/opening-outcome.js`, which is where
  // the decisions live and which imports nothing but types: a `git(...)` source
  // belongs to `describe`, a catalogue that reports `status: "error"` stops its
  // own step, and a product the store did not resolve is not an open at all. That
  // script runs in `npm run verify`. A `⚪ not observed` row here would still be
  // the wrong shape -- it suggests a later run might happen to see it, when what
  // is true is that no run will unless somebody writes the mutation on purpose.
});

test.describe("Overview says what the product is", () => {
  test("Overview reports the shape, the tree and the sources without asking for any of it [plan §9.1: Overview is the product at a glance]", async ({
    studio,
  }) => {
    // The stage was two rows -- a profile and a link -- which made the emptiest
    // screen in the application the one every open lands on.
    const { page } = studio;
    await openProduct(page, "dev");
    await productSection(page, "overview");

    const gears = page.locator("[data-overview-gears]");
    const processes = page.locator("[data-overview-applications]");
    await expect(gears).toBeVisible();
    await expect(processes).toBeVisible();
    expect(Number(await gears.getAttribute("data-overview-gears"))).toBeGreaterThan(0);
    expect(Number(await processes.getAttribute("data-overview-applications"))).toBeGreaterThan(0);
    // The count that surprises people: a closure nobody asked for.
    await expect(gears).toHaveText(/\d+/);
    await expect(page.locator("[data-figure='gears']")).toContainText("pulled in");

    // The sources are the description's, so a root that failed to load is named.
    const sources = page.locator("[data-overview-sources]");
    await expect(sources).toBeVisible();
    expect(Number(await sources.getAttribute("data-overview-sources"))).toBeGreaterThan(0);

    // **Arriving on the stage must not start work.** `ensurePlan` is a round trip
    // to the engine, and a render that asked for one would do it on every repaint
    // of a panel that repaints on every store change.
    //
    // Asserted at the source, because the observable version is not sound: a plan
    // that completed between two readings leaves the status looking untouched, so
    // "the status did not change" is a check that passes when the rule is broken.
    // What the rule actually says is that this widget never calls `ensurePlan`,
    // and that is a statement about the source -- the same shape as the codicon
    // and accessibility claims, which read these files for the same reason.
    const widget = readFileSync(
      join(IDE, "gearbox-studio/src/browser/product/product-widget.tsx"),
      "utf8",
    );
    expect(
      /ensurePlan\s*\(/.test(widget),
      "the Product view asks the engine for a plan while rendering",
    ).toBe(false);
    // And the visible half: the stage reports a status rather than a spinner,
    // which is what "not planned yet is an answer" means.
    const status = page.locator("[data-overview-generate]");
    await expect(status).toBeVisible();
    expect(await status.getAttribute("data-overview-generate")).not.toBe("planning");

    // A count with no way through is trivia: the figures move between stages.
    await page.locator("[data-figure='applications']").click();
    await expect(page.locator('[data-product-section="topology"]')).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });
});

test.describe("opening a product is one act", () => {
  test("a reload with a product open comes back to Home [plan §9.1: Home is a screen, not an empty area]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);

    // Boot no longer opens a product on the strength of finding one, so getting
    // there is an act: the picker, since a fresh profile has nothing to continue.
    await page.locator('[data-start-action="open"]').click();
    const options = page.locator(`.quick-input-list [role="option"]`);
    await options.first().waitFor({ state: "visible", timeout: 30_000 });
    await options.filter({ hasText: "payments-demo" }).first().click();
    await expect(page.locator(".gbx-toolbar")).toHaveAttribute("data-context", "product", {
      timeout: 90_000,
    });
    // Resolved, not merely opening. `data-context` flips as soon as the store has
    // a reference, which is seconds before the open finishes -- and Recent is
    // written at the *end*, deliberately, because it is a list of products that
    // opened rather than of ones once attempted. Reloading before then aborts the
    // open and there is nothing to continue.
    // Overview first: `data-resolved-profile` is the resolved header, and the
    // panel opens on Composition now. The stage is not the claim here -- "the
    // open finished resolving" is -- so this goes where that fact is rendered,
    // exactly as `openProduct` does and for the same reason.
    await productSection(page, "overview");
    await expect(page.locator("[data-resolved-profile]")).toBeVisible({ timeout: 90_000 });

    await page.reload({ waitUntil: "domcontentloaded" });
    await settled(page);

    await expect(page.locator(".gbx-toolbar")).toHaveAttribute("data-context", "home", {
      timeout: 30_000,
    });
    await expect(page.locator(".gbx-start")).toBeVisible();
    // And the way back is one click, named after the thing it returns to. The
    // card shows the display name; the product it returns to is its path.
    await expect(page.locator('[data-start-action="continue"]')).toBeVisible();
    await expect(page.locator("[data-start-continue]")).toHaveAttribute(
      "data-start-continue",
      /payments-demo/,
    );
  });

  test("opening a product leaves the Product workspace on screen [plan §9.1: Open Product is atomic]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);

    // Round trip through a product and a reload first, because that is what puts
    // `gearbox.product` into the restored perspective id. Opening a product in a
    // shell that has never had one open takes a path where the early return
    // cannot fire, so a test that skipped this would pass against the defect.
    await page.locator('[data-start-action="open"]').click();
    const options = page.locator(`.quick-input-list [role="option"]`);
    await options.first().waitFor({ state: "visible", timeout: 30_000 });
    await options.filter({ hasText: "payments-demo" }).first().click();
    await expect(page.locator(".gbx-toolbar")).toHaveAttribute("data-context", "product", {
      timeout: 90_000,
    });
    // Overview first: `data-resolved-profile` is the resolved header, and the
    // panel opens on Composition now. The stage is not the claim here -- "the
    // open finished resolving" is -- so this goes where that fact is rendered,
    // exactly as `openProduct` does and for the same reason.
    await productSection(page, "overview");
    await expect(page.locator("[data-resolved-profile]")).toBeVisible({ timeout: 90_000 });
    await page.reload({ waitUntil: "domcontentloaded" });
    await settled(page);

    await page.locator('[data-start-action="continue"]').click();

    // The whole claim: no `revealView`, no `View > Gearbox Product`, no tab click.
    // While the engine restarts twice the panel says which product it is waiting
    // for, and it is the Product panel saying it rather than Home still sitting
    // there.
    await expect(page.locator(".gbx-product")).toBeVisible({ timeout: 90_000 });
    // Overview first: `data-resolved-profile` is the resolved header, and the
    // panel opens on Composition now. The stage is not the claim here -- "the
    // open finished resolving" is -- so this goes where that fact is rendered,
    // exactly as `openProduct` does and for the same reason.
    await productSection(page, "overview");
    await expect(page.locator("[data-resolved-profile]")).toBeVisible({ timeout: 90_000 });
    // Home is not in front of it: the header would say `payments-demo` while the
    // centre offered to open one. Not *absent* -- the Start screen stays a tab
    // in the main area, and `StartViewContribution` records why closing it is
    // the wrong instrument. The complaint was about what is on screen.
    await expect(page.locator(".gbx-start")).toBeHidden();
  });

  test("saving the description on disk re-resolves it without a click [plan §9.1: the panel shows what the file says]", async ({
    freshStudio,
  }) => {
    // Nothing subscribed to the filesystem before `DescriptionWatchService`, so
    // every Gearbox surface went on rendering the resolution from before the
    // save -- twelve of them, all reading the same two stores. The complaint
    // arrives as "the Inspector does not update"; the Inspector is not special.
    //
    // A fresh app because the claim is about a *product this session opened*
    // and the watch is scoped to it, and on the real description rather than a
    // fixture because Theia's watcher is what delivers the event and it only
    // watches the workspace.
    const { page } = freshStudio;
    await settled(page);
    await openProduct(page, "dev");

    const gdl = join(REPO, "products/payments-demo/product.gdl");
    const original = readFileSync(gdl, "utf8");
    const hash = page.locator("[data-resolved-profile]");
    const before = await hash.getAttribute("data-lock-hash");

    try {
      // A real edit, and one every resolution reads, so the lock hash has to
      // move if the file was read again at all.
      writeFileSync(gdl, original.replace('version = "0.1.0"', 'version = "0.1.1"'));
      // No click, no command, no reveal between here and the assertion.
      await expect(hash).not.toHaveAttribute("data-lock-hash", before ?? "", {
        timeout: 30_000,
      });
    } finally {
      // **From `git`, not from the snapshot above.** Writing `original` back
      // assumes it was the committed text, and when it is not -- because an
      // earlier run or an earlier claim left its own edit behind -- the restore
      // re-writes the damage instead of undoing it. One leftover then survives
      // every later cleanup, which is how a single failure became three in
      // `prd-diagnostics`.
      restoreProducts(REPO);
    }
  });

  test("saving a gear description re-reads the catalogue without a reload [plan §9.1: the panel shows what the file says]", async ({
    freshStudio,
  }) => {
    // The other half of the same complaint. `product.gdl` was picked up above;
    // a `gear.gdl` was not, and the reason recorded in `DescriptionWatchService`
    // -- that catching it would mean respawning the engine -- turned out to be a
    // client-side habit rather than a protocol requirement. `catalogue/load`
    // re-reads on the process already running.
    //
    // The gear's `description` is the probe, and the choice is load-bearing.
    // Not its `name`: `detailOf` finds a row *by* display name and four other
    // claims call `detailOf("API Gateway")`, so a run interrupted between the
    // write and the restore would break them with a failure naming nothing.
    // Not a structural fact like `deps` either -- that moves resolutions this
    // suite asserts on elsewhere. A description is declared, presentational,
    // and pinned by nobody.
    const { page } = freshStudio;
    await settled(page);

    // Selected *before* the edit, and not touched after it. The claim is that
    // an open panel changes on its own; re-clicking the row afterwards would
    // prove only that the store can be re-read, which was never in doubt.
    const shown = await freshStudio.detailOf("API Gateway");
    expect(shown, "the api-gateway row is in the catalogue").not.toBeNull();

    const gdl = join(REPO, "../gears-rust/gears/system/api-gateway/gear.gdl");
    const original = readFileSync(gdl, "utf8");
    const probe = `watched at ${Date.now()}`;

    try {
      writeFileSync(gdl, original.replace(/description = "[^"]*"/, `description = "${probe}"`));
      // No click, no command, no reveal between here and the assertion. The
      // budget covers a 700ms settle plus a full staged rescan of the corpus.
      await expect(page.locator(".gbx-detail")).toContainText(probe, { timeout: 60_000 });
    } finally {
      // Load-bearing: `global-setup` guards `products/` only, so nothing else
      // in this suite would notice a `gear.gdl` left rewritten. Restored from
      // `git` for the reason given on the claim above -- this file is one of
      // `GUARDED_CORPUS_FILES`, so the helper already knows about it.
      restoreCorpus(REPO);
    }
  });

  test("Composition stacks when its pane is narrow, whatever the window is [ADR-0023 §Amendment: one surface configures]", async ({
    freshStudio,
  }) => {
    // **The pane, not the window, and the difference was the defect.** The only
    // rule that could stack this grid was a `@media (max-width: 800px)`, keyed
    // on the viewport — and the Product panel is the main area, which is the
    // window minus whatever the left and right panels take. With both open on a
    // 1708px window the pane had 606px and was still forced into two columns
    // with a 230px floor.
    //
    // **Narrowed by dragging, not by resizing the window**, and that is the
    // second lesson. Shrinking the viewport made the measurement depend on
    // where the previous claim had left the side panels: this asserted a pane
    // under 900px and got 988 the day a neighbour left the Inspector closed.
    // Dragging states the arrangement instead of inheriting it, and it makes
    // the disagreement exact — the window never changes at all.
    const { page } = freshStudio;
    await openProductById(page, "configurable-gears", "dev");
    await productSection(page, "composition");
    await revealCatalogue(page);
    await revealInspector(page);

    const wide = await measurePane(page);
    expect(wide.columns, "it starts as two columns, or the narrowing proves nothing").toBe(2);

    await narrowThePane(page, 360);

    const narrow = await measurePane(page);
    expect(narrow.window, "the window never moved, and it is over any viewport threshold").toBe(
      wide.window,
    );
    expect(narrow.window).toBeGreaterThan(1200);
    expect(narrow.pane, "and the pane it leaves is under one").toBeLessThan(900);
    expect(narrow.columns, "so the columns stack").toBe(1);
  });

  test("a diagnostic leads to the control that would fix it [plan §9.1: Validation is a screen]", async ({
    studio,
  }) => {
    // **The stage stopped being a doorway and was still a dead end.** A warning
    // about a gear's `mode` offered two destinations: the `.gdl` at a range, and
    // a panel saying *why*. Neither is the box that sets it, so the way from the
    // warning to the field was to remember the gear's name, go to Composition,
    // and find it again.
    const { page } = studio;
    await openProductById(page, "configurable-gears", "dev");
    await productSection(page, "validation");

    const configure = page.locator("[data-conflict-configure-field]").first();
    await expect(configure, "a diagnostic about a gear offers its form").toBeVisible({
      timeout: 60_000,
    });
    const gear = await configure.getAttribute("data-conflict-configure");
    const field = await configure.getAttribute("data-conflict-configure-field");
    expect(gear).not.toBeNull();
    expect(field, "and this one is about one key, so it says which").not.toBeNull();
    await configure.click();

    // One act: the stage *and* the object, with the form on screen. A button
    // that showed Composition without carrying the selection would leave the
    // person exactly where they started.
    await expect(page.locator('[data-product-section="composition"]')).toHaveAttribute(
      "aria-selected",
      "true",
      { timeout: 30_000 },
    );
    await expect(
      page.locator(`.gbx-composition-settings [data-gear-config="${String(gear)}"]`),
    ).toBeVisible({ timeout: 30_000 });

    // **The control, not the panel.** Focusing the pane put a person in front of
    // the right form and left them to find the row the message had just named.
    await expect
      .poll(
        () =>
          page.evaluate(() =>
            document.activeElement?.closest("[data-config-field]")?.getAttribute("data-config-field"),
          ),
        { timeout: 15_000 },
      )
      .toBe(field);
  });

  test("scrolling one half of Composition leaves the other and the head alone [ADR-0023 §Amendment: one surface configures]", async ({
    freshStudio,
  }) => {
    // **The panel was one scrolling block.** Reading a form carried the
    // product's name, its profile switcher and the stage tabs off the top, and
    // scrolling to reach a gear moved the settings beside it. Making the stage
    // strip `sticky` fixed neither: it pinned a strip in the middle of a header
    // whose other halves still left.
    //
    // Two things had to be true before any of that could work, and both were
    // silently false: the panel was 12px taller than the widget holding it
    // (`height: 100%` plus padding under `content-box`), and the grid's row was
    // sized to its content by `align-items: start`, so columns carrying
    // `overflow: auto` were never smaller than what they held and had nothing to
    // scroll.
    //
    // **Set up by dragging the sash, not by shrinking the window**, and the
    // difference is why this claim exists twice. The first version made the
    // *viewport* small and left the side panels wherever the previous claim had
    // them — so it never reproduced the arrangement the report came from, and I
    // called the defect fixed on the strength of it. What the report describes
    // is a **layout**: a wide Inspector and a Product panel around 630px, inside
    // a window that is not small at all.
    const { page } = freshStudio;
    await openProductById(page, "configurable-gears", "dev");
    await productSection(page, "composition");
    await revealCatalogue(page);
    await revealInspector(page);
    const form = await configureGear(page, "event-broker");
    await expect(form).toBeVisible({ timeout: 60_000 });

    // The handle between the main area and the right panel: the rightmost one
    // tall enough to be a real divider rather than a collapsed placeholder.
    const handle = await page.evaluate(() =>
      Array.from(document.querySelectorAll(".lm-SplitPanel-handle"))
        .filter((h) => !h.classList.contains("lm-mod-hidden"))
        .map((h) => h.getBoundingClientRect())
        .filter((r) => r.height > 100)
        .sort((a, b) => b.x - a.x)
        .map((r) => ({ x: Math.round(r.x), y: Math.round(r.y + r.height / 2) }))[0],
    );
    expect(handle, "the Inspector has to be resizable for this to be set up").toBeDefined();
    await page.mouse.move(handle.x, handle.y);
    await page.mouse.down();
    await page.mouse.move(handle.x - 360, handle.y, { steps: 12 });
    await page.mouse.up();

    const product = page.locator(".gbx-product");
    await expect
      .poll(async () => Math.round((await product.boundingBox())?.width ?? 0), { timeout: 15_000 })
      .toBeLessThan(800);
    expect(
      page.viewportSize()?.width ?? 0,
      "and the window is not the reason — a rule keyed on it would not fire here",
    ).toBeGreaterThan(1200);

    const tree = page.locator(".gbx-composition-tree");
    await expect
      .poll(() => tree.evaluate((el) => el.scrollHeight > el.clientHeight + 1), { timeout: 15_000 })
      .toBe(true);

    const tops = () =>
      page.evaluate(() => ({
        name: Math.round(document.querySelector("[data-product-name]")!.getBoundingClientRect().top),
        settings: Math.round(
          document.querySelector(".gbx-composition-settings")!.getBoundingClientRect().top,
        ),
      }));
    const before = await tops();
    await tree.evaluate((el) => {
      el.scrollTop = 300;
    });
    await expect.poll(() => tree.evaluate((el) => el.scrollTop), { timeout: 10_000 }).toBeGreaterThan(0);

    const after = await tops();
    expect(after.name, "the product it belongs to stays on screen").toBe(before.name);
    expect(after.settings, "and the form beside it does not move").toBe(before.settings);
  });

  test("a resolve in flight is said, and nothing calls the profile clean meanwhile [plan §9.1: Validation is a screen]", async ({
    rpc,
    stalledStudio,
  }) => {
    // **Caused, not waited for.** This state used to be checked by opening a
    // product and hoping to catch the moment — twice, and twice the report came
    // back "could not confirm". The answer to `resolve` is withheld here, so the
    // moment lasts as long as the claim needs it.
    //
    // What it is about: `resolveCurrent` sets `status: "resolving"` *without*
    // clearing the diagnostics array, and the empty state used to be decided by
    // `diagnostics.length === 0` alone — so a profile that had nothing to report
    // yet announced that it had resolved with nothing to report, and three
    // warnings arrived a moment later.
    //
    // This is a claim about the **frontend**. The request reaches the backend
    // and a healthy engine answers it; what is held is the answer's journey
    // back. Nothing here says anything about the engine's own timeout.
    const { page } = stalledStudio;
    // **The second resolve, not the first.** Opening a product waits for one to
    // land — the profile switcher is rendered from it — so stalling the opening
    // resolve stalls the helper rather than the screen. A profile switch is the
    // ordinary way a person starts another one, and it is the state the report
    // described: a product already on screen, re-resolving.
    // **From the clean profile to the one with warnings**, and the direction is
    // the claim. `resolveCurrent` does not clear the diagnostics it holds, so a
    // re-resolve from a profile that *had* warnings keeps showing them — stale,
    // and nothing says so, which is its own defect and belongs to the stale-
    // status work. Starting clean is what puts the screen in the state this is
    // about: an empty array that has not been earned yet.
    await openProductById(page, "configurable-gears", "dev");
    await productSection(page, "validation");
    const validationStage = page.locator("[data-product-validation]");
    await expect(validationStage).toBeVisible({ timeout: 60_000 });

    const resolving = rpc.stallNext("resolve");
    await page.locator("[data-product-profile]").selectOption("prod");
    await resolving.held;

    const validation = validationStage;
    // The panel says it is working, for as long as the answer is withheld.
    await expect(page.locator("[data-product-resolving]")).toBeVisible({ timeout: 30_000 });
    await expect(
      validation.locator("[data-validation-clean]"),
      "a profile that has not resolved has not resolved with nothing to report",
    ).toHaveCount(0);

    // And releasing it finishes the wait, which is what says the answer was
    // held rather than lost.
    resolving.release();
    await expect(page.locator("[data-product-resolving]")).toHaveCount(0, { timeout: 30_000 });
    // A verdict, whichever one: this profile may resolve cleanly, and the claim
    // is that the wait *ended* — that the answer was held on its way back and
    // not lost. Naming one of the two outcomes would be asserting the corpus.
    await expect
      .poll(
        async () =>
          (await validation.locator("[data-validation-clean]").count()) +
          (await validation.locator("[data-conflict-code]").count()),
        { timeout: 30_000 },
      )
      .toBeGreaterThan(0);
  });

  test("a resolve that failed says so, and does not leave the old verdict standing [plan §9.1: Validation is a screen]", async ({
    rpc,
    stalledStudio,
  }) => {
    // **Driven, because this one never happened on demand.** `fail()` stores
    // `diagnosticsOf(error) ?? []`, so an engine error carrying no structured
    // diagnostics used to render as "this profile resolved with nothing to
    // report" — a clean bill of health for a resolution that did not happen.
    //
    // A forged failure, not a held answer: the difference between "still
    // working" and "ended badly" is the whole claim, and a hold can only
    // produce the first.
    const { page } = stalledStudio;
    await openProductById(page, "configurable-gears", "dev");
    await productSection(page, "validation");
    const validation = page.locator("[data-product-validation]");
    await expect(validation).toBeVisible({ timeout: 60_000 });

    const refused = rpc.failNext("resolve", "the engine refused this profile");
    await page.locator("[data-product-profile]").selectOption("prod");
    await refused.held;

    await expect(
      validation.locator("[data-validation-failed]"),
      "a profile that could not be resolved has not been found clean",
    ).toBeVisible({ timeout: 30_000 });
    await expect(validation.locator("[data-validation-failed]")).toContainText(
      "the engine refused this profile",
    );
    await expect(validation.locator("[data-validation-clean]")).toHaveCount(0);
    await expect(
      validation.locator("[data-conflict-code]"),
      "and the previous profile's findings are not shown as this one's",
    ).toHaveCount(0);
  });

});

test.describe("a description being re-read", () => {
  // A product of this claim's own: it touches the file to make the watcher
  // re-read it, and a shipped description is nobody's to touch. Made in
  // `beforeAll` because `ProductStore.ensureDiscovered` returns early once it
  // holds a list — a product created after a page booted is never offered.
  let copy: ProductCopy | undefined;

  test.beforeAll(() => {
    copy = copyProduct(REPO, "configurable-gears", "reload");
  });

  test.afterAll(() => {
    copy?.dispose();
    copy = undefined;
  });

  test("a product being re-read does not look like a product with nothing in it [plan §9.1: opening is staged]", async ({
    rpc,
    stalledStudio,
  }) => {
    // `ProductStore.open()` sets `status: "loading"` with no intent yet, so the
    // tree used to render `Selected gears (0)` and "your product has no gears
    // yet" for as long as the read took — a six-gear product announcing it was
    // empty. Fixed, and until now unclaimed, because the state lasts a moment.
    //
    // **A re-read, because the first read has no panel to be wrong on.** With
    // the opening answer held the session never establishes and the context
    // stays on Home; the state that was reported is one where the panel is
    // already on screen and the description is read again. Saving the file is
    // how a person causes that, and `DescriptionWatchService` is what turns it
    // into a reload.
    const product = copy;
    expect(product, "the copy is made in beforeAll").toBeDefined();
    const { page } = stalledStudio;
    await openProductById(page, product!.id, "dev");
    await productSection(page, "composition");
    const tree = page.locator(".gbx-composition-tree");
    await expect(tree.locator("[data-asked-for]").first()).toBeVisible({ timeout: 60_000 });

    const reading = rpc.stallNext("loadProduct");
    // A comment, so the bytes change and nothing else does.
    writeFileSync(product!.path, `${readFileSync(product!.path, "utf8")}\n# re-read\n`);
    await reading.held;

    await expect(tree.locator("[data-composition-loading]")).toBeVisible({ timeout: 30_000 });
    await expect(
      tree,
      "a product still being read has not been found to be empty",
    ).not.toContainText("no gears yet");

    reading.release();
    await expect(tree.locator("[data-composition-loading]")).toHaveCount(0, { timeout: 60_000 });
    await expect(tree.locator("[data-asked-for]").first()).toBeVisible({ timeout: 60_000 });
  });
});
