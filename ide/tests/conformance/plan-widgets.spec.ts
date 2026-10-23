// The widget table in §9 of docs/plans/prototype.md: which views
// exist and where they live.
//
// The placements are claims, not incidental. Each of the three that exist was
// argued for in a comment beside the contribution, and two of them were wrong
// once: the catalogue was opened from `onStart` and ended up collapsed but still
// in the DOM, and the detail panel was in the side panel where its content was
// clipped -- hiding exactly the projected facts it exists to show.

import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  expect,
  openConflicts,
  openExplain,
  openGenerate,
  openGraph,
  openGraphView,
  openProduct,
  productSection,
  resetCatalogueView,
  revealCatalogue,
  revealDetail,
  revealInspector,
  revealLeft,
  revealLock,
  runCommand,
  settled,
  test,
} from "../fixtures/studio";

const VOCABULARY = join(
  __dirname,
  "../../gearbox-studio/src/browser/gdl/generated/vocabulary.ts",
);

/** The named `readonly string[]` lists of the generated vocabulary. */
function vocabulary(): Record<string, string[]> {
  const source = readFileSync(VOCABULARY, "utf8");
  const out: Record<string, string[]> = {};
  for (const match of source.matchAll(/export const (\w+): readonly string\[\] = \[(.*?)\];/gs)) {
    out[match[1]] = [...match[2].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
  }
  return out;
}

test.describe("where the views live", () => {
  test("the Catalogue folds by category and filters [plan §9: Catalogue, foldable and filtered]", async ({
    studio,
  }) => {
    // 62 crates in `gears-rust` carry `#[toolkit::gear]` against the 14 described
    // today, so this list quadruples as descriptions land. Both halves are tested
    // because they interact: a filter has to override a fold.
    await revealCatalogue(studio.page);
    await resetCatalogueView(studio.page);

    const rows = () => studio.page.locator(".gbx-widget-catalogue .gbx-row").count();
    const all = await rows();
    expect(all).toBeGreaterThan(0);

    const example = studio.page.locator('.gbx-group[data-category="example"] .gbx-group-label');
    await expect(example).toHaveAttribute("data-collapsed", "false");
    await example.click();
    await expect(example).toHaveAttribute("data-collapsed", "true");
    expect(await rows(), "folding a category removes its rows").toBeLessThan(all);

    // The trap: a match inside a folded category must still show. Otherwise the
    // reader searches, sees nothing, and concludes the gear is not there.
    await studio.page.fill(".gbx-widget-catalogue .gbx-filter", "Payments");
    const found = await studio.page
      .locator(".gbx-widget-catalogue .gbx-row-name")
      .allTextContents();
    expect(found.every((name) => name.includes("Payments"))).toBe(true);
    expect(found.length).toBeGreaterThan(0);

    await studio.page.fill(".gbx-widget-catalogue .gbx-filter", "no-such-gear-anywhere");
    await expect(studio.page.locator(".gbx-widget-catalogue .gbx-empty")).toContainText(
      "Nothing matches",
    );

    await resetCatalogueView(studio.page);
    expect(await rows(), "clearing the filter and the fold restores every row").toBe(all);
  });

  test("the Catalogue is in the left area [plan §9: Catalogue, left]", async ({ studio }) => {
    const tabs = await studio.page.evaluate(() =>
      Array.from(document.querySelectorAll("#theia-left-content-panel .lm-TabBar li")).map((e) =>
        (e.textContent ?? "").trim(),
      ),
    );
    expect(tabs).toContain("Gearbox Catalogue");
  });

  test("the Inspector is beside the tree, not under it [plan §9: Inspector, right panel]", async ({
    studio,
  }) => {
    // It was the bottom area, and the reason recorded for that was that a side
    // panel clipped the projected facts. A UX pass measured the other side of the
    // trade: at an ordinary window height the bottom strip shows **one**
    // configuration field and hides the rest behind an inner scroll -- in the
    // panel that is the gear configurator. So it moves, and the two-column layout
    // that did the clipping becomes one column (`index.css`, `.theia-side-panel
    // .gbx-inspector`).
    //
    // Asserted by which shell area holds it, not by pixel geometry: the panel is
    // resizable and a person's own width is not this claim's business.
    await revealInspector(studio.page);
    const area = await studio.page.evaluate(() => {
      const node = document.querySelector(".gbx-inspector");
      if (node === null) return "absent";
      if (node.closest("#theia-right-content-panel") !== null) return "right";
      if (node.closest("#theia-bottom-content-panel") !== null) return "bottom";
      if (node.closest("#theia-left-content-panel") !== null) return "left";
      if (node.closest("#theia-main-content-panel") !== null) return "main";
      return "elsewhere";
    });
    expect(area).toBe("right");

    // And Conflicts stays below, because it is read *while* looking at the tree
    // that caused the complaint.
    //
    // A product first: Conflicts is a product's screen and its command is scoped
    // to that context, so with nothing open it is not in the palette -- which is
    // the gate working. The claim here is about *where* the panel lives, and it
    // needs a product to have one at all.
    await openProduct(studio.page, "dev");
    await openConflicts(studio.page);
    const conflicts = await studio.page.evaluate(
      () => document.querySelector("#theia-bottom-content-panel .gbx-conflicts") !== null,
    );
    expect(conflicts).toBe(true);
  });

  test("one selection answers both questions at once [plan §9: Inspector, one selection]", async ({
    studio,
  }) => {
    // The claim that could not be made before, and the reason the two panels were
    // merged. `Gearbox Gear` rendered from a catalogue row and `Gearbox Explain`
    // from a product focus, so choosing a gear in the *product* tree -- the
    // ordinary thing to do -- filled the second and left the first saying "select
    // a gear in the catalogue". One `SelectionService` later, both are about the
    // same gear.
    await openProduct(studio.page, "dev");
    await productSection(studio.page, "composition");
    await studio.page.locator('[data-asked-for="api-gateway"] a').click();
    await revealDetail(studio.page);

    // What it is: projected facts, which only the catalogue knows.
    await expect(
      studio.page.locator(".gbx-inspector .gbx-detail .gbx-detail-title"),
    ).toContainText("api-gateway");
    // Why it is here: a because-sentence, which only the resolution knows.
    await expect(
      studio.page.locator('.gbx-inspector .gbx-explain[data-explaining="gear:api-gateway"]'),
    ).toBeVisible();
    await expect(studio.page.locator(".gbx-inspector .gbx-step").first()).toBeVisible();

    // And the same selection lights up in the catalogue, because it is the same
    // selection rather than a copy of one.
    // By `data-row-key`, which is `source:gdl_path` -- the catalogue keys rows by
    // path rather than by id, because a pending row has no id yet.
    await revealLeft(studio.page, "Gearbox Catalogue");
    await expect(studio.page.locator(".gbx-widget-catalogue .gbx-row.gbx-selected")).toHaveAttribute(
      "data-row-key",
      /api-gateway/,
    );
  });

  test("the co-location Graph opens in the main area [plan §9: Graph, main]", async ({
    studio,
  }) => {
    await runCommand(studio.page, "Gearbox Graph");
    const inMain = await studio.page.evaluate(
      () => document.querySelector("#theia-main-content-panel .gbx-svg") !== null,
    );
    expect(inMain).toBe(true);
  });

  test("the Product view is a tree of branches [vision §60; plan §9: Product]", async ({
    studio,
  }) => {
    // Vision §60 sketches `Product -> Deployment / Gears / Contracts / Cluster /
    // Edge / Security / Artifacts`. What is built and why it differs is §60.1:
    // Processes is added because the resolver computes it and it is what makes
    // co-location legible; Security and Artifacts are absent because neither
    // exists to show; Deployment stays in the header because the profile switch
    // must work while a resolution is in flight.
    // **The branches are behind the stages now**, and the gears half changed
    // shape again with Composition: `Topology` still holds the three branches
    // that describe how a product deploys, and the gears are no longer one of
    // them. A branch was a read-only fold over the resolution's two buckets;
    // Composition is the product's own structure, so it folds per *gear* -- each
    // host with its connections under it -- rather than as one bucket of them.
    await openProduct(studio.page, "dev");
    await productSection(studio.page, "topology");
    const topologyBranches = await studio.page
      .locator("[data-branch]")
      .evaluateAll((nodes) => nodes.map((node) => node.getAttribute("data-branch")));
    expect(topologyBranches).toEqual(["applications", "contracts", "cluster"]);

    await productSection(studio.page, "composition");
    expect(
      await studio.page
        .locator("[data-branch]")
        .evaluateAll((nodes) => nodes.map((node) => node.getAttribute("data-branch"))),
      "Composition is not a bucket of gears; the branches belong to Topology",
    ).toEqual([]);

    // Both halves of the old buckets are still named and still distinguished:
    // what the product asked for, and what came with it.
    await expect(studio.page.locator('[data-asked-for="api-gateway"]')).toBeVisible();
    await expect(studio.page.locator("[data-pulled-in]").first()).toBeVisible();

    // A gear folds, and says so rather than only looking folded. **Not a
    // `<details>` any more**, and the reason is in `composition.tsx`: toggling
    // is what activating a `<summary>` does, so a summary cannot both name the
    // gear and select it -- which is why the tree used to carry a separate
    // `Configure <gear>` button and left the name doing the least of the three
    // controls on the line. The disclosure is its own control now, and the state
    // is on the host for a reader to check.
    const host = studio.page.locator('[data-asked-for="api-gateway"]');
    const twistie = host.locator('[data-composition-expand="api-gateway"]');
    await expect(host).toHaveAttribute("data-collapsed", "false");
    await expect(twistie).toHaveAttribute("aria-expanded", "true");
    await twistie.click();
    await expect(host).toHaveAttribute("data-collapsed", "true");
    await expect(twistie).toHaveAttribute("aria-expanded", "false");
    await twistie.click();
    await expect(host).toHaveAttribute("data-collapsed", "false");

    // And the row itself selects, which is the act a person expects of clicking
    // the name of a thing. It used to only fold.
    // Not asserted unpressed first: the studio is worker-scoped, so whatever ran
    // before may have left this gear selected. What this claim is about is that
    // clicking the row is what chooses, which is true from either state.
    const row = host.locator('[data-composition-gear="api-gateway"]');
    await row.click();
    await expect(row).toHaveAttribute("aria-pressed", "true");
    await expect(
      studio.page.locator('.gbx-composition-settings [data-gear-config="api-gateway"]'),
      "selecting the row is what puts the gear's form on screen",
    ).toBeVisible({ timeout: 30_000 });

    // The icon says what a leaf is, and it is chosen from `selected_by` rather
    // than from the id -- a gear is a plugin because something selected it as
    // one, and `*-plugin` in a name is a convention. Read on a co-located gear,
    // because a *selected* plugin is no longer listed here as an independent
    // entry: it belongs to the host that named it, under that host's point.
    await expect(
      studio.page.locator('[data-pulled-in="types-registry"] .codicon-package'),
    ).toBeVisible();
    await expect(
      studio.page.locator('[data-asked-for="api-gateway"] .codicon-package'),
    ).toBeVisible();
    // And the plugin is where it now belongs: under its host, as a connection.
    await expect(
      studio.page.locator('[data-plugin-host="authn-resolver"]').first(),
      "a selected plugin is a connection under its host, not a loose gear",
    ).toBeVisible();
  });

  test("the Product view shows what §9 asks it to [plan §9: Product]", async ({ studio }) => {
    // §9 lists five things: a profile dropdown, the selected gears with a
    // "pulled in by co-location" sublist, a bindings table with mode/transport/
    // mechanism chips, a cluster table, and a diagnostics summary. The cluster
    // table is absent from this assertion on purpose -- no gear in this product
    // requests a cluster scope, so there is no row to render, and demanding one
    // would be demanding a different product.
    //
    // Read across the stages, since the panel has them: the profile switch and
    // the diagnostics summary are above the strip and therefore on every one --
    // the switch because it must work while a resolution is in flight, the
    // summary because a person on Topology is exactly who needs to know the
    // resolution complained.
    await openProduct(studio.page, "dev");
    await expect(studio.page.locator("[data-profile]").first()).toBeVisible();
    await expect(studio.page.locator(".gbx-diagnostics")).toBeVisible();

    await productSection(studio.page, "composition");
    await expect(studio.page.locator("[data-asked-for]").first()).toBeVisible();
    await expect(studio.page.locator("[data-pulled-in]").first()).toBeVisible();
    // Still on screen from here: one line, on every stage.
    await expect(studio.page.locator(".gbx-diagnostics")).toBeVisible();

    await productSection(studio.page, "topology");
    await expect(studio.page.locator(".gbx-binding [data-mechanism]").first()).toBeVisible();

    // And the stage that is only about what went wrong, which is the fourth.
    await productSection(studio.page, "validation");
    await expect(studio.page.locator("[data-product-validation]")).toBeVisible();
    await expect(studio.page.locator("[data-validation-errors]")).toBeVisible();
  });

  test("the Conflicts screen lists what the resolution reported [plan §9: Conflicts]", async ({
    studio,
  }) => {
    // eCos's Config Tool makes conflicts a screen rather than a status line, and
    // the reason is what this asserts: each one carries its code, and the screen
    // says which profile it is about. The Product view keeps only a summary, so
    // the two do not print the same list twice.
    await openProduct(studio.page, "dev");
    const summary = await studio.page.locator(".gbx-diagnostics-label").innerText();
    const reported = Number(/^(\d+)/.exec(summary)?.[1] ?? "0");
    expect(reported, "this product resolves with at least one diagnostic to show").toBeGreaterThan(
      0,
    );

    // From the summary, the way a person gets there.
    await studio.page.locator("[data-show-conflicts]").click();
    const screen = studio.page.locator(".gbx-conflicts");
    await expect(screen).toBeVisible();

    // The same diagnostics, not a different set: one array in `ProductStore`, two
    // renderers, so a disagreement here would mean one of them is inventing.
    await expect(screen).toHaveAttribute("data-conflicts-count", String(reported));
    expect(await screen.locator(".gbx-conflict").count()).toBe(reported);
    await expect(screen.locator("[data-conflicts-profile]")).toHaveAttribute(
      "data-conflicts-profile",
      "dev",
    );
    // Every row names its code. A diagnostic without one cannot be looked up.
    const codes = await screen
      .locator(".gbx-conflict")
      .evaluateAll((rows) => rows.map((r) => r.getAttribute("data-conflict-code")));
    expect(codes.every((code) => code !== null && code.length > 0)).toBe(true);
  });

  test("a conflict points the Inspector at its subject [PRD cpt-gearbox-fr-explain: subject]", async ({
    studio,
  }) => {
    // `Diagnostic.subject` is documented as "the graph node this concerns, so a
    // client can select it". This is the client doing that -- which is also the
    // only way to tell whether the field carries a node the graph actually has.
    // `prod`, not `dev`: the diagnostic with an unambiguous subject is GBX0409,
    // "the endpoint override for X on Y cannot come from an environment variable",
    // and it only arises where the consumer and the provider end up in different
    // processes. In `dev` everything is one process, so nothing is remote and there
    // is no override to complain about.
    await openProduct(studio.page, "prod");
    await openConflicts(studio.page);

    const explain = studio.page.locator("[data-conflict-explain]").first();
    test.skip(
      (await explain.count()) === 0,
      "no diagnostic in this resolution names a gear, process or binding as its subject",
    );

    const subject = await explain.getAttribute("data-conflict-explain");
    await explain.click();
    await revealDetail(studio.page);
    await expect(studio.page.locator(".gbx-inspector")).toHaveAttribute(
      "data-inspecting",
      String(subject),
    );
  });

  test("the explanation travels with the Inspector [plan §9: Explain]", async ({ studio }) => {
    // A section of the Inspector rather than a panel of its own, so it is
    // wherever that panel is -- the right side since 2026-09-07. The property
    // that matters has not changed and is the one asserted: it answers about a
    // selection made elsewhere, so it must be readable *while* the Product view
    // is on screen rather than instead of it. A side panel satisfies that; the
    // main area would not.
    await openProduct(studio.page, "dev");
    await productSection(studio.page, "composition");
    await studio.page.locator('[data-asked-for="api-gateway"] a').click();
    await openExplain(studio.page);
    const area = await studio.page.evaluate(() => {
      const node = document.querySelector(".gbx-explain");
      if (node === null) return "absent";
      if (node.closest("#theia-right-content-panel") !== null) return "right";
      if (node.closest("#theia-bottom-content-panel") !== null) return "bottom";
      if (node.closest("#theia-main-content-panel") !== null) return "main";
      return "elsewhere";
    });
    expect(["right", "bottom"]).toContain(area);
  });

  test("the Lock view is in the main area [plan §9: Lock]", async ({ studio }) => {
    await openProduct(studio.page, "dev");
    await revealLock(studio.page);
    const inMain = await studio.page.evaluate(
      () => document.querySelector("#theia-main-content-panel .gbx-lock") !== null,
    );
    // Beside Product, not in the bottom strip: the lock is the same object at
    // full fidelity, and reading it means scrolling a few hundred lines.
    expect(inMain).toBe(true);
  });

  test("a Generate view exists [plan §9: Generate]", async ({ studio }) => {
    await openGenerate(studio.page);
    await expect(studio.page.locator(".gbx-generate")).toBeVisible();
  });

  test("Apply is refused when the plan writes nothing [plan §9: Generate, Apply disabled]", async ({
    studio,
  }) => {
    // Every gate on Apply was about permission or correctness -- capability,
    // write rights, resolution errors, conflicts -- and an all-`unchanged` plan
    // passes all four. So the button stayed live over a plan with no work in it,
    // the round trip ran, and the engine answered `written: 0`. A control that is
    // enabled for an operation with no effect teaches the reader that the counts
    // above it are decoration.
    await openProduct(studio.page, "dev");
    await openGenerate(studio.page);
    const counts = studio.page.locator(".gbx-generate-counts");
    await expect(counts).toBeVisible({ timeout: 60_000 });

    // Observed rather than arranged: whether this corpus has a generated tree on
    // disk depends on what ran before, and generating one here would leave the
    // repository dirty. Both states are asserted -- the point is that the button
    // agrees with the plan either way.
    // The product's templates are written by an apply too.
    const writes = await studio.page
      .locator(
        '.gbx-generate [data-action="create"], .gbx-generate [data-action="update"], ' +
          '.gbx-generate [data-template-action="create"], .gbx-generate [data-template-action="update"]',
      )
      .count();
    const apply = studio.page.locator("[data-apply]");
    if (writes === 0) {
      await expect(studio.page.locator('[data-apply-block="nothing"]')).toContainText(
        "up to date",
      );
      await expect(apply).toBeDisabled();
    } else {
      await expect(studio.page.locator('[data-apply-block="nothing"]')).toHaveCount(0);
    }
  });
});

test.describe("a product's own templates", () => {
  test("Generate writes the product's templates, sees an edit, and resets it [GDL reference: templates]", async ({
    studio,
  }) => {
    // Every generated file comes from a template, and generation writes each one
    // into the product so it can be edited there. The Generate view lists them
    // with their state; an edit made in the file shows up without a re-resolve,
    // and Reset deletes the copy so the next Apply writes the builtin back.
    const page = studio.page;
    await openProduct(page, "dev");
    await openGenerate(page);
    const key = "docker/dockerignore";
    const row = page.locator(`[data-template="${key}"]`);
    await expect(row).toHaveCount(1, { timeout: 60_000 });
    const path = await row.getAttribute("data-template-path");
    expect(path).not.toBeNull();
    let original: string | undefined;
    try {
      if ((await row.getAttribute("data-template-action")) === "create") {
        await page.locator("[data-apply]").click();
        await expect(row).toHaveAttribute("data-template-action", "unchanged", { timeout: 60_000 });
      }
      expect(existsSync(path!)).toBe(true);
      original = readFileSync(path!, "utf8");
      if ((await row.getAttribute("data-template-status")) !== "default") {
        // A copy left edited by an earlier run: take the builtin back first.
        await page.locator(`[data-template-reset="${key}"]`).click();
        await page.locator(".dialogBlock .theia-button.main").click();
        await page.locator("[data-apply]").click();
        await expect(row).toHaveAttribute("data-template-status", "default", { timeout: 60_000 });
        original = readFileSync(path!, "utf8");
      }

      writeFileSync(path!, `${original}# house rule\n`);
      await expect(row).toHaveAttribute("data-template-status", "edited", { timeout: 60_000 });

      await page.locator(`[data-template-reset="${key}"]`).click();
      await page.locator(".dialogBlock .theia-button.main").click();
      await expect(row).toHaveAttribute("data-template-action", "create", { timeout: 60_000 });
      await expect(row).toHaveAttribute("data-template-status", "default");
      expect(existsSync(path!)).toBe(false);
    } finally {
      if (original !== undefined && !existsSync(path!)) writeFileSync(path!, original);
    }
  });
});

test.describe("generating into a folder of one's own", () => {
  test("Generate writes into a chosen folder and makes it a repository [ADR-0010 amendment 2026-09-29]", async ({
    studio,
  }) => {
    // The tree used to land only under `<workspace>/.gearbox/`, inside this
    // repository, so a product could not be shipped from a repository of its
    // own without leaving Studio for the CLI. A folder chosen by name is a
    // generation target when it is new, holds only dotfiles, or holds a
    // `product.lock`; anything else is somebody's and is refused.
    const scratch = realpathSync(mkdtempSync(join(tmpdir(), "gbx-generate-out-")));
    const app = join(scratch, "app");
    const theirs = join(scratch, "theirs");
    mkdirSync(theirs);
    writeFileSync(join(theirs, "notes.txt"), "mine");
    const page = studio.page;
    const field = page.locator("[data-generate-out-input]");
    try {
      await openProduct(page, "dev");
      await openGenerate(page);
      await expect(field).toBeVisible({ timeout: 60_000 });

      // Somebody's folder is refused, the refusal names the file that makes it
      // theirs, and the field that answers it stays on screen.
      await field.fill(theirs);
      await field.press("Enter");
      await expect(page.locator(".gbx-generate .gbx-error")).toContainText("notes.txt", {
        timeout: 60_000,
      });
      await expect(field).toBeVisible();

      // A new folder is planned into, and Apply makes it a repository.
      await field.fill(app);
      await field.press("Enter");
      await expect(page.locator(".gbx-generate[data-out-root]")).toHaveAttribute(
        "data-out-root",
        app,
        { timeout: 60_000 },
      );
      const apply = page.locator("[data-apply]");
      await expect(apply).toBeEnabled();
      await apply.click();
      await expect(page.locator('[data-generate-repository="initialized"]')).toBeVisible({
        timeout: 60_000,
      });

      expect(existsSync(join(app, "product.lock"))).toBe(true);
      expect(existsSync(join(app, ".git"))).toBe(true);
      expect(readFileSync(join(app, ".gitignore"), "utf8")).toContain(".gearbox/");
      // Nothing is written beside the folder: the merge base of a chosen folder
      // lives inside it (`.gearbox/base`, created once a file needs merging), not
      // next to it in somebody's home.
      expect(readdirSync(scratch).sort()).toEqual(["app", "theirs"]);

      // Back to the default, which is where every other claim expects the tree.
      await page.locator("[data-generate-default-out]").click();
      await expect(field).toHaveValue("");
    } finally {
      if ((await page.locator("[data-generate-default-out]").count()) > 0) {
        await page.locator("[data-generate-default-out]").click();
      }
      rmSync(scratch, { recursive: true, force: true });
    }
  });
});

test.describe("the graph's own claims", () => {
  test("the layout is the same after a reload [plan §9: deterministic layout]", async ({
    freshStudio,
  }) => {
    // `elkjs` was dropped for a hand-written layout, and determinism is what
    // survived of that promise. A graph that reshuffles between reloads makes
    // "these four out of fourteen" unreadable, because the reader has to find
    // the nodes again each time.
    await settled(freshStudio.page);
    const positions = async (): Promise<string> => {
      // Only open it if it is not already open. `AbstractViewContribution`
      // registers a *toggle*, so after a reload -- where Theia's layout restorer
      // has already brought the graph back -- running the command again would
      // close it.
      //
      // Visibility, not presence: Home's Start screen opens after every ready
      // (including post-reload), so the Graph widget can sit attached but hidden
      // behind Start. `count() > 0` would then skip the command and hang on
      // `waitFor({ visible })`.
      if (!(await freshStudio.page.locator(".gbx-svg").first().isVisible().catch(() => false))) {
        await runCommand(freshStudio.page, "Gearbox Graph");
      }
      await freshStudio.page.locator(".gbx-svg").waitFor({ state: "visible" });
      const read = (): Promise<string> =>
        freshStudio.page.evaluate(() =>
          Array.from(document.querySelectorAll("[data-gear]"))
            .map((e) => {
              const box = (e as SVGGraphicsElement).getBoundingClientRect();
              return `${e.getAttribute("data-gear")}@${Math.round(box.x)},${Math.round(box.y)}`;
            })
            .sort()
            .join("|"),
        );

      // Laid out, not merely on screen. The SVG becomes visible before its nodes
      // have positions, so reading straight after `waitFor` samples a graph where
      // every node is at `0,0` -- which compares equal to nothing and, on a slow
      // machine, made a claim about *determinism* fail for being early. Waiting
      // for the layout is not a workaround: an unlaid-out graph is not the thing
      // this measures.
      await expect
        .poll(async () => (await read()).includes("@0,0"), { timeout: 30_000 })
        .toBe(false);
      return read();
    };

    const before = await positions();
    await freshStudio.page.reload({ waitUntil: "domcontentloaded" });
    await settled(freshStudio.page);
    const after = await positions();

    expect(before.length).toBeGreaterThan(0);
    expect(after).toBe(before);
  });
});

test.describe("co-location is a closure, not a partition", () => {
  // The claim the graph exists for. Co-location edges are link-time `deps`, so
  // the set of gears that must share a process with a given gear is its
  // transitive closure -- and processes therefore overlap. Drawing it as a
  // partition would tell a reader that a cut exists where none can.

  test("the closure reaches past the direct dependencies [plan §9: co-location closure]", async ({
    studio,
  }) => {
    await openGraph(studio.page);
    const edges = await studio.page.evaluate(() =>
      Array.from(document.querySelectorAll(".gbx-edge")).map((e) => [
        e.getAttribute("data-from"),
        e.getAttribute("data-to"),
      ]),
    );
    const has = (from: string, to: string) =>
      edges.some(([f, t]) => f === from && t === to);
    expect(has("api-gateway", "authn-resolver")).toBe(true);
    expect(has("api-gateway", "grpc-hub")).toBe(true);
    // The second hop: api-gateway does not declare types-registry, and still
    // cannot be separated from it.
    expect(has("authn-resolver", "types-registry")).toBe(true);
  });

  test("clicking a gear paints its transitive closure [plan §9: co-location closure]", async ({
    studio,
  }) => {
    await openGraph(studio.page);
    const painted = await studio.page.evaluate(async () => {
      // Scoped to the graph. An unscoped `[data-gear=...]` reaches whichever
      // widget rendered first, and the catalogue is to the left of this one.
      const node = document.querySelector('.gbx-widget-graph [data-gear="api-gateway"]');
      if (node === null) return null;
      node.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      await new Promise((r) => setTimeout(r, 300));
      return {
        lit: Array.from(document.querySelectorAll(".gbx-node-lit, .gbx-node-focus")).map((e) =>
          e.getAttribute("data-gear"),
        ),
        dimmed: document.querySelectorAll(".gbx-node-dim").length,
        footer: (document.querySelector(".gbx-footer")?.textContent ?? "")
          .replace(/\s+/g, " ")
          .trim(),
      };
    });
    expect(painted).not.toBeNull();
    for (const gear of ["api-gateway", "authn-resolver", "grpc-hub", "types-registry"]) {
      expect(painted!.lit).toContain(gear);
    }
    // Shown against the rest rather than alone: "these four out of fourteen" is
    // the whole point, and a closure with nothing to compare it to reads as the
    // entire product.
    expect(painted!.dimmed).toBeGreaterThan(0);
    expect(painted!.footer).toMatch(/co-locates/);
  });

  test("unconnected gears are set apart, not put in the leaf column [plan §9: isolated gears]", async ({
    studio,
  }) => {
    await openGraph(studio.page);
    const graph = await studio.page.evaluate(() => ({
      isolated: Array.from(document.querySelectorAll('[data-isolated="true"]')).map((e) =>
        e.getAttribute("data-gear"),
      ),
      touched: Array.from(document.querySelectorAll(".gbx-edge")).flatMap((e) => [
        e.getAttribute("data-from"),
        e.getAttribute("data-to"),
      ]),
    }));
    // A gear with no co-location has to be told apart from a gear everything
    // depends on: both sit at layer 0, and conflating them reads as though the
    // isolated one were depended upon.
    expect(graph.isolated.length).toBeGreaterThan(0);
    expect(graph.isolated.filter((id) => graph.touched.includes(id))).toEqual([]);
  });
});

test.describe("the .gdl grammar's generated half", () => {
  // These read the generated file rather than a rendered editor, because no
  // `gear.gdl` in the tree contains a forbidden keyword -- the interpreter
  // rejects them, so none can. Observing the colouring would mean writing an
  // illegal file into the workspace, which is a mutation this suite does not make.
  // `make grammar-check` is what proves the file matches the interpreter; these
  // tests prove the shape the widget relies on.

  test("fourteen constructs are forbidden outright [plan §9: forbidden-keyword colouring]", () => {
    const lists = vocabulary();
    expect(lists.FORBIDDEN_KEYWORDS).toHaveLength(14);
    // The set `cpt-gearbox-fr-gdl-declarative` names: control flow and anything
    // that encodes a decision.
    for (const keyword of ["if", "elif", "else", "for", "def", "lambda", "and", "or", "not"]) {
      expect(lists.FORBIDDEN_KEYWORDS).toContain(keyword);
    }
  });

  test("`while` is reserved rather than forbidden [plan §9: two keyword lists]", () => {
    // The distinction is the interpreter's, not the grammar's: Starlark has no
    // `while` at all, so it can never appear as a rejected construct -- it is a
    // reserved word. Colouring it as forbidden would tell a reader the resolver
    // refuses something it never sees.
    const lists = vocabulary();
    expect(lists.RESERVED_KEYWORDS).toContain("while");
    expect(lists.FORBIDDEN_KEYWORDS).not.toContain("while");
  });
});

test.describe("the graph is four views of one product", () => {
  // Plan §9 specifies "Graph | four views. **deps** ... **contracts** ...
  // **processes** ... **cluster**". One widget hosting four, rather than four
  // widgets, is the shape that claim describes -- and the switch is the only part
  // of it a reader can see, so it is the part worth checking.

  test("all four views are reachable from one panel [plan §9: Graph four views]", async ({
    studio,
  }) => {
    await openGraph(studio.page);
    const tabs = studio.page.locator(".gbx-widget-graph .gbx-view-tab");
    await expect(tabs).toHaveCount(4);
    await expect(tabs).toHaveText(["co-location", "contracts", "applications", "cluster"]);
  });

  test("co-location shows first, and needs no product [plan §9: Graph four views]", async ({
    studio,
  }) => {
    // The default matters. Co-location reads the catalogue, so it draws something
    // the moment the panel opens; the other three are answers about a profile, and
    // opening onto one of them would present an empty frame as the first
    // impression of the whole panel.
    await openGraph(studio.page);
    await expect(
      studio.page.locator(".gbx-widget-graph .gbx-view-tab[data-view='deps']"),
    ).toHaveAttribute("aria-selected", "true");
    await expect(studio.page.locator("[data-graph='deps']")).toBeVisible();
  });

  test("a resolution view with no product says what it needs [plan §9: Graph four views]", async ({
    freshStudio,
  }) => {
    // A fresh app, so no product has been opened. The three resolution views have
    // nothing to draw, and the check is that they explain that rather than render
    // an empty frame -- an empty frame and a broken view look identical.
    await openGraphView(freshStudio.page, "contracts");
    const empty = freshStudio.page.locator(".gbx-widget-graph .gbx-empty");
    await expect(empty).toBeVisible();
    await expect(empty).toContainText("needs a product and a profile");
  });

  test("switching profiles redraws the resolution views [plan §9: Graph four views]", async ({
    studio,
  }) => {
    // The three resolution views are per profile, so the profile switch has to
    // reach them. It does not reach them by being wired to them: the widget
    // subscribes to `ProductStore.onChanged`, which is the same edge that made the
    // catalogue's in-product toggles appear.
    await openProduct(studio.page, "dev");
    await openGraphView(studio.page, "applications");
    await expect(studio.page.locator("[data-graph='applications'] .gbx-binary")).toHaveCount(1);

    await openProduct(studio.page, "prod");
    await expect(studio.page.locator("[data-graph='applications'] .gbx-binary")).toHaveCount(3);
  });
});
