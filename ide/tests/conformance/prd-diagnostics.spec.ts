// `cpt-gearbox-fr-editor-diagnostics`, in two halves:
//
//   Description-file diagnostics MUST be reported with source ranges over a
//   language-server interface, and resolution diagnostics MUST be surfaced as
//   editor problem markers replaced atomically on each resolution.
//
// Both are built now, by two contributions writing to `ProblemManager` under two
// different owners -- `gearbox` for the resolution, `gearbox-gdl` for the open
// description. The separate owners are why the last claim here can break a
// `product.gdl` without the resolution claims above losing their markers, and
// they are the mechanism `cpt-gearbox-adr-gdl-language-server` records.

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { corpusStatus, restoreCorpus } from "../fixtures/corpus-files";
import { copyProduct, type ProductCopy } from "../fixtures/product-copy";
import {
  expect,
  openConflicts,
  openProduct,
  problems,
  revealCatalogue,
  revealInExplorer,
  settled,
  test,
} from "../fixtures/studio";

const REPO = join(__dirname, "../../..");

/**
 * A gear description the corpus ships, and the one the guard already watches.
 *
 * `fixtures/corpus-files.ts` lists it; editing any other corpus file from a test
 * means adding it there first, or nothing will notice a file left rewritten.
 */
const GEAR_GDL = join(REPO, "../gears-rust/gears/system/api-gateway/gear.gdl");

/**
 * The product description, which the language-server claim breaks and restores.
 *
 * In this repository rather than in the gears corpus, so the guard in
 * `fixtures/corpus-files.ts` does not cover it -- but the two git claims read
 * this tree's own status, which is a stricter guard than that one and the reason
 * the rewrite is undone in a `finally`. `adr-0010` mutates the same file for the
 * same kind of round trip.
 */

/**
 * The token the fixture rewrites: the first profile constructor's name. The
 * constructor, not a whole call, so the profile's arguments and their order
 * (payments-demo's `dev` profile grew `development` and `database`) do not
 * make the fixture stale.
 */
const GOOD_PROFILE = "embedded(";

test.describe("diagnostics reach a person", () => {
  test("the Problems view is present to receive markers [PRD cpt-gearbox-fr-editor-diagnostics]", async ({
    studio,
  }) => {
    // **Present, not pre-opened.** It used to open itself on a first run and this
    // claim read the boot tab list, which made it a claim about the default
    // layout rather than about the destination existing. Conflicts is the domain
    // screen for the resolution's diagnostics; Problems stays for the
    // file-anchored ones Monaco publishes, and is one command away
    // (`HiddenProblemsView` records why). Reached the way a person reaches it.
    await problems(studio.page);
    const tabs = await studio.page.evaluate(() =>
      Array.from(
        document.querySelectorAll("#theia-bottom-content-panel .lm-TabBar-tabLabel"),
      ).map((e) => (e.textContent ?? "").trim()),
    );
    expect(tabs).toContain("Problems");
  });

  test("the catalogue panel renders the diagnostics a load produced [PRD cpt-gearbox-fr-editor-diagnostics]", async ({
    freshStudio,
  }) => {
    // Scoped to the catalogue widget, and that scoping is the point. Unscoped,
    // this test flipped to green the day the Product view landed -- because the
    // Product panel renders GBX0307 for the dev profile, on the same page. It was
    // reporting a resolver diagnostic as evidence that the catalogue renders
    // catalogue diagnostics, which is precisely the kind of pass-for-the-wrong-
    // reason this suite exists to prevent.
    // `.gbx-conflict`, since 2026-09-08: the catalogue renders the same row as
    // every other consumer of `Diagnostic[]`. Updated even though this claim
    // currently skips on a clean corpus -- a selector that cannot match is a
    // claim that will never observe what it says, and the skip would have hidden
    // that indefinitely.
    // **The corpus is made to produce one, because a clean corpus cannot.** This
    // skipped itself on every run -- "the gear tree loads clean, so the catalogue
    // rendered no diagnostic to inspect" -- which is a property of the corpus and
    // not of the panel, so the claim proved nothing for as long as it existed.
    //
    // `category` rather than something that fails harder: GBX0108 is a *warning*
    // (`gear category is not one the platform uses`), so the gear stays in the
    // catalogue carrying a diagnostic, which is exactly the state this claim is
    // about. An error could drop the gear and take the row with it.
    //
    // Restored in a `finally`, and the restoration is guarded: `global-setup`,
    // a per-test hook and `global-teardown` all now ask whether this file
    // matches HEAD (`fixtures/corpus-files.ts`). Before that guard existed a
    // crash here left the corpus rewritten and nothing said so.
    const { page } = freshStudio;
    // **Committed before it is broken.** A claim that rewrites a file and puts it
    // back has to start from the version it intends to put back; starting from
    // somebody's leftover means restoring the leftover. Asked of `git` rather
    // than of the text, because the committed *value* belongs to another
    // repository and pinning it here is the mistake `config_tests.rs` records.
    expect(
      corpusStatus(REPO),
      "this claim rewrites a gear description, so it must start from the committed one",
    ).toBe("");
    const original = readFileSync(GEAR_GDL, "utf8");
    expect(original, "the file this claim rewrites must declare a category").toMatch(
      /category = "[^"]*"/,
    );

    try {
      writeFileSync(
        GEAR_GDL,
        original.replace(/category = "[^"]*"/, 'category = "not-a-platform-category"'),
      );
      await settled(page);
      await revealCatalogue(page);
      // The rows live in the bottom area now; the catalogue header counts them
      // and is the way there.
      const link = page.locator(".gbx-widget-catalogue .gbx-catalogue-diagnostics-link");
      await expect(link, "the catalogue must say it has diagnostics").toBeVisible({ timeout: 60_000 });
      await link.click();

      const panel = page.locator("#theia-bottom-content-panel .gbx-widget-catalogue-diagnostics");
      const rendered = panel.locator(".gbx-conflict");
      await expect(rendered.first(), "the catalogue must render what the load produced").toBeVisible(
        { timeout: 60_000 },
      );
      expect(await rendered.first().textContent()).toMatch(/GBX\d{4}/);
      // The specific one, so a different diagnostic arriving for a different
      // reason cannot satisfy this claim by accident.
      await expect(panel.locator("[data-catalogue-diagnostics]")).toBeVisible();
      await expect(panel.locator('[data-conflict-code="GBX0108"]')).toHaveCount(1);
    } finally {
      // **Put back by `git`, not by the snapshot read above.** Writing `original`
      // back assumes it was the committed text -- and when it is not, the restore
      // re-writes the damage instead of undoing it, so one run's leftover
      // survives every later run's cleanup. That cascade is what turned a single
      // failure into three, and it is why the remedy here is the same one the
      // guards use.
      restoreCorpus(REPO);
    }
  });

  test("resolution diagnostics appear as problem markers [PRD cpt-gearbox-fr-editor-diagnostics]", async ({
    studio,
  }) => {
    // Counted on the **Conflicts screen**, which is where the resolution's
    // diagnostics are listed now. The Product view used to print them under its
    // tree and keeps only a summary: two full lists of the same array was
    // duplication, and the one squeezed under a tree was the one nobody could act
    // on. Both renderers read `ProductStore.diagnostics`, so this still compares
    // the markers against what a person is shown.
    await openProduct(studio.page, "dev");
    await openConflicts(studio.page);
    // **Waited for, not sampled.** `count()` resolves immediately against
    // whatever is rendered at that instant, and the screen is empty for as long
    // as a resolution is in flight -- which it can be here for a reason outside
    // this test, because a description saved anywhere in the tree re-resolves the
    // open product. Read once with no wait, that made "the profile produces no
    // diagnostic" a statement about timing rather than about the profile.
    const list = studio.page.locator(".gbx-conflicts .gbx-conflict");
    await expect(
      list.first(),
      "the dev profile produces no diagnostic to surface",
    ).toBeVisible({ timeout: 60_000 });
    const shown = await list.count();

    const { files, markers } = await problems(studio.page);
    // Anchored to the product description. The wire type says resolution
    // diagnostics "often have no location and are anchored by the client", and
    // the description is the only file the whole resolution is about.
    expect(files.some((file) => file.includes("product.gdl"))).toBe(true);
    expect(markers.length).toBe(shown);
  });

  test("markers are replaced atomically on each resolution [PRD cpt-gearbox-fr-editor-diagnostics]", async ({
    studio,
  }) => {
    // "Stale markers are worse than none." prod produces more diagnostics than
    // dev, so going prod → dev is the direction that would leave leftovers: a
    // per-file `setMarkers` replaces one file's markers and says nothing about a
    // file the new resolution no longer mentions.
    // Both counts wait for the screen to hold something first, for the reason
    // given in the claim above: a `count()` taken while a resolution is in flight
    // reads zero, and zero here would look like the leak this asserts against
    // rather than like a read taken too early. The comparison itself is left
    // exactly as sharp as it was -- what is waited for is the resolution, not the
    // agreement between the two numbers.
    const listed = studio.page.locator(".gbx-conflicts .gbx-conflict");

    await openProduct(studio.page, "prod");
    await openConflicts(studio.page);
    await expect(listed.first()).toBeVisible({ timeout: 60_000 });
    const prodShown = await listed.count();
    const prod = await problems(studio.page);
    expect(prod.markers.length).toBe(prodShown);

    await openProduct(studio.page, "dev");
    await openConflicts(studio.page);
    await expect(listed.first()).toBeVisible({ timeout: 60_000 });
    const devShown = await listed.count();
    const dev = await problems(studio.page);

    expect(devShown).not.toBe(prodShown);
    // This is the anti-leak assertion, and it is the whole guard: a marker left
    // over from `prod` would make the count exceed what `dev` puts on screen.
    expect(dev.markers.length).toBe(devShown);
    // And at least one of prod's really went, so two identical sets could not
    // satisfy the line above by accident.
    expect(prod.markers.some((marker) => !dev.markers.includes(marker))).toBe(true);
    // Deliberately *not* "every prod marker is gone". A diagnostic both profiles
    // report -- GBX0410 says `prefer.fewer_applications` is not honoured, and the
    // description declares it whatever the profile -- is not stale, and demanding
    // its removal made this claim fail the day such a diagnostic first existed.
  });

  /**
   * The description this claim breaks, and it is not the shipped one.
   *
   * **This test is where the stray `embeddedd` came from.** It writes a typo
   * into a `product.gdl` on purpose -- that is the whole mechanism, an evaluator
   * error with a span to underline -- and put it back in a `finally`. Which
   * holds right up until the run does not reach the `finally`: a crash, a
   * `--grep` interrupted, a machine put to sleep. Then a shipped description
   * carries a deliberate typo into the next run, where two unrelated claims fail
   * and neither names the cause. That happened, and it is what the per-claim
   * copies exist for.
   *
   * Made in `beforeAll` rather than in the test, because this claim reaches the
   * file through the **explorer tree**: the directory has to exist before the
   * page is built, or the node is not there to expand.
   */
  let broken: ProductCopy;

  test.beforeAll(() => {
    broken = copyProduct(REPO, "payments-demo", "typo");
  });

  test.afterAll(() => {
    broken?.dispose();
  });

  test("description diagnostics arrive over a language-server interface with source ranges [PRD cpt-gearbox-fr-editor-diagnostics]", async ({
    freshStudio,
  }) => {
    // The other half of the requirement, and distinct from every claim above:
    // those are about a list in a panel produced by a *resolution*, this is about
    // the description being edited, marked from the engine's `textDocument/*`
    // surface (`cpt-gearbox-adr-gdl-language-server`).
    //
    // **The range is the claim.** The version this replaces asserted only that
    // `.squiggly-error` was visible somewhere on the page, without opening a
    // `.gdl` at all -- it would have passed on a squiggle in any editor, from any
    // source, at any position. So this asserts *which text* is underlined and
    // *which line* the marker names, which is what "with source ranges"
    // distinguishes from "a diagnostic arrived".
    //
    // `freshStudio`, and for two reasons. The description is broken on disk for
    // the length of this test, so a shared session would carry a failed product
    // load into the next claim; and it leaves an editor tab open on a
    // `product.gdl`, which `prd-explain` locates by `data-uri` and would then
    // match twice.
    const { page } = freshStudio;
    // No precondition about the tree any more, and that is the point rather than
    // an omission: `copyProduct` reads the committed text with `git show HEAD:`,
    // so this claim starts from the committed description by construction rather
    // than by checking that somebody else left one alone.
    const original = readFileSync(broken.path, "utf8");
    // A typo in a profile constructor: the evaluator reports the name it could
    // not resolve, and the span it gives is the token itself rather than the
    // enclosing call -- which is what makes the underlined text checkable.
    const TYPO = "embeddedd";
    expect(original, "the fixture rewrites a profile constructor").toContain(GOOD_PROFILE);
    // One-based, as the Problems view renders it.
    const line = original.split("\n").findIndex((text) => text.includes(GOOD_PROFILE)) + 1;

    try {
      writeFileSync(broken.path, original.replace(GOOD_PROFILE, `${TYPO}(`));

      // Broken *before* it is opened, deliberately. Opening reads the file, so
      // this exercises `didOpen` with no dependency on a file watcher noticing a
      // change to a buffer that is already up -- a race that would make this
      // claim flake for a reason that has nothing to do with what it asserts.
      const node = await revealInExplorer(page, "gearbox", ["products", broken.id], "product.gdl");
      await node.dblclick();

      const editor = page.locator('.monaco-editor[data-uri*="product.gdl"]');
      await expect(editor).toBeVisible({ timeout: 30_000 });

      const squiggle = editor.locator(".squiggly-error");
      await expect(squiggle.first(), "the description must be underlined").toBeVisible({
        timeout: 30_000,
      });
      // **Where the underline is, not what it says.** Monaco draws a marker
      // squiggle as an empty absolutely-positioned overlay rather than as a class
      // on the text spans, so there is no string to read -- `allTextContents()`
      // returns `""` however right the diagnostic is. The observable the DOM does
      // offer is the position, which is the half of the claim that matters: a
      // diagnostic carrying the whole-file sentinel would be drawn on the first
      // line of the file, and this one is drawn on the line with the typo in it.
      const typoLine = editor.locator(".view-line", { hasText: TYPO });
      await expect(typoLine, "the broken line must be on screen to compare against").toHaveCount(1);
      // **Measured as one retried step, because two boxes are two samples.**
      // Monaco recycles `.view-line` nodes on every relayout, so a line that is
      // there when it is counted can be detached by the time it is measured --
      // and `boundingBox()` answers `null` for a detached node rather than
      // waiting for it to come back. That is a read losing a race, not a
      // diagnostic drawn in the wrong place, and it read as the latter.
      // Retrying re-resolves both nodes together, so the two boxes always come
      // from the same layout.
      await expect(async () => {
        const lineBox = await typoLine.boundingBox();
        const underline = await squiggle.first().boundingBox();
        expect(lineBox, "the line has no box to measure").not.toBeNull();
        expect(underline, "the squiggle has no box to measure").not.toBeNull();
        // A couple of pixels of slack in each direction: the squiggle is drawn
        // under the glyphs rather than around them, so its box is inset within
        // the line's row by a rounding amount that depends on the font metrics.
        expect(underline!.y).toBeGreaterThanOrEqual(lineBox!.y - 2);
        expect(underline!.y).toBeLessThanOrEqual(lineBox!.y + lineBox!.height + 2);
        expect(underline!.width, "a squiggle with no width underlines nothing").toBeGreaterThan(0);
      }).toPass({ timeout: 30_000 });

      // And the marker's own position, which the Problems view renders from
      // `range.start`. `Ln 1, Col 1` is what a diagnostic with no span looks
      // like; this asserts the line the typo is actually on.
      const { files, markers } = await problems(page);
      const shown = `expected Ln ${line}\nfiles: ${files.join(" | ")}\nmarkers: ${markers.join(" | ")}`;
      expect(files.some((file) => file.includes("product.gdl")), shown).toBe(true);
      expect(markers.some((marker) => marker.includes(`Ln ${line}`)), shown).toBe(true);
      expect(markers.some((marker) => marker.includes(TYPO)), shown).toBe(true);
    } finally {
      // **Put back, and not because anything depends on it.** The copy is
      // removed wholesale in `afterAll`, and nothing else reads it -- so this is
      // the cheapest way to leave the file readable if a later failure sends
      // somebody to look at it, not a repair the suite relies on. That reliance
      // is what moved: a restore that does not run can no longer damage anything
      // a person owns.
      writeFileSync(broken.path, original);
    }
  });
});
