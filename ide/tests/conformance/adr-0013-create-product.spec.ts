// ADR cpt-gearbox-adr-create-product — create, clone and in-description edits.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { copyProduct, type ProductCopy } from "../fixtures/product-copy";

import {
  configureConnection,
  configureGear,
  expect,
  expectContext,
  openAdvancedKeys,
  openProduct,
  productSection,
  runCommand,
  settled,
  test,
} from "../fixtures/studio";

const REPO = join(__dirname, "../../..");
const DEMO = join(REPO, "products/payments-demo/product.gdl");
const DEMO_REL = "products/payments-demo/product.gdl";

function diffOf(path: string): string {
  return execFileSync("git", ["diff", "--stat", "--", path], {
    cwd: REPO,
    encoding: "utf8",
  }).trim();
}

function commentLines(text: string): number {
  return text.split("\n").filter((line) => line.trim().startsWith("#")).length;
}

/**
 * Queue the removal of one free config key, and see that it took.
 *
 * Clicking and moving on is what this suite did, and it loses to a remount: the
 * settings pane re-renders on the draft epoch and on every store change, so a
 * click can land on a node that is already detached. The row going is the
 * observable that says the edit reached the draft, so the click and that check
 * retry together.
 */
async function removeKey(
  form: import("@playwright/test").Locator,
  key: string,
): Promise<void> {
  const row = form.locator(`[data-config-key="${key}"]`);
  // It has to be there before it can be removed. Without this the retry below
  // cannot tell "already gone" from "not rendered yet".
  await expect(row, `${key} must be on screen to be removed`).toHaveCount(1, {
    timeout: 30_000,
  });
  // **Absence counts as success only after a click.** The first version of this
  // returned early whenever the row was missing, which reads as idempotence and
  // is not: during a remount the row is missing because it has not come back,
  // so the helper did nothing, queued nothing, and the Apply it was preparing
  // for waited forever on a button that never appeared. A 360s timeout on the
  // *next* line, from a helper that reported success.
  let clicked = false;
  await expect(async () => {
    if (clicked && (await row.count()) === 0) return;
    await form.locator(`[data-config-remove="${key}"]`).click({ timeout: 5_000 });
    clicked = true;
    await expect(row).toHaveCount(0, { timeout: 5_000 });
  }).toPass({ timeout: 20_000 });
}

async function acceptPreview(page: import("@playwright/test").Page): Promise<void> {
  const dialog = page.locator(".dialogBlock", {
    has: page.locator(".gbx-edit-preview, .gbx-create-preview"),
  });
  await expect(dialog).toBeVisible();
  await dialog.locator(".theia-button.main").click();
}

/**
 * Take the destination the wizard suggests, explicitly.
 *
 * The field opens empty with the suggestion as its placeholder -- ADR-0013 says
 * the destination "must not silently take the first workspace root", and it used
 * to be pre-filled with exactly that. Every flow that does not care *where* the
 * product goes still has to say so, which is the point.
 *
 * **Call this after setting the id.** The suggestion contains the id, and filling
 * the field marks it chosen -- so calling this first pins the destination to
 * `products/new-product/` and a later id change does not move it. Which is how
 * this helper first left a `products/new-product` behind while the test's own
 * cleanup removed a directory of a different name.
 */
async function chooseSuggestedDestination(page: import("@playwright/test").Page): Promise<void> {
  const destination = page.locator("[data-create-destination]");
  const suggested = await destination.getAttribute("placeholder");
  expect(suggested ?? "", "the wizard must suggest a destination").not.toBe("");
  await destination.fill(suggested ?? "");
}

function removeProductDir(id: string): void {
  rmSync(join(REPO, "products", id), { recursive: true, force: true });
}

async function openCreateWizard(page: import("@playwright/test").Page): Promise<void> {
  await page.waitForSelector(".theia-ApplicationShell", { timeout: 60_000 });
  // Prefer the Start button; palette is the fallback if Start is covered.
  const startCreate = page.locator('[data-start-action="create"]');
  if ((await startCreate.count()) > 0 && (await startCreate.isVisible().catch(() => false))) {
    await startCreate.click();
  } else {
    await runCommand(page, "New Product");
  }
  await expect(page.locator(".gbx-create-preview, [data-engine-status='disconnected']").first()).toBeVisible({
    timeout: 30_000,
  });
}

test.describe("create and clone a product", () => {
  const id = "conformance-create";
  const rel = `products/${id}/product.gdl`;

  test.afterEach(() => {
    removeProductDir(id);
  });

  test("Create shows preview text, Cancel writes nothing, Create opens the product [ADR-0013 §Confirmation]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);
    expect(diffOf(rel)).toBe("");

    await openCreateWizard(page);
    await expect(page.locator('[data-create-modes] [data-create-mode="blank"]')).toBeVisible();
    // The destination is chosen, not assumed, so there is nothing to preview
    // until one is -- see the destination-picker claim below.
    await expect(page.locator(".gbx-create-preview")).toContainText("Choose a destination");
    await page.locator("[data-create-id]").fill(id);
    await page.locator("[data-create-name]").fill("Conformance Create");
    await chooseSuggestedDestination(page);
    await expect(page.locator(".gbx-create-preview")).toContainText("product(");
    await expect(page.locator(".gbx-create-preview")).toContainText(`id = "${id}"`, {
      timeout: 10_000,
    });

    await page.locator("[data-create-cancel]").click();
    await expect(page.locator(".gbx-create")).toHaveCount(0);
    expect(diffOf(rel)).toBe("");

    await openCreateWizard(page);
    await page.locator("[data-create-id]").fill(id);
    await page.locator("[data-create-name]").fill("Conformance Create");
    await chooseSuggestedDestination(page);
    await expect(page.locator(".gbx-create-preview")).toContainText(`id = "${id}"`, {
      timeout: 10_000,
    });
    await page.locator("[data-create-submit]").click();
    await acceptPreview(page);

    // Overview, because the resolved header lives there and a product opens on
    // its Composition now. The claim is that the new product opened *and
    // resolved*, not that any particular stage is showing.
    await productSection(page, "overview");
    await expect(page.locator("[data-resolved-profile]")).toBeVisible({ timeout: 60_000 });
    expect(existsSync(join(REPO, "products", id, "product.gdl"))).toBe(true);
  });

  /**
   * Creating, closing, and creating again all work in one session.
   *
   * **The reported symptom was that the second create is impossible.** A Blank
   * product created with the wizard's default source selection declared this
   * checkout as one of its sources -- the wizard checked every workspace root,
   * and the first is the checkout that contains `products/`. Creating it
   * succeeded, because at boot the engine's roots are the corpus only. It then
   * opened the product, and opening re-initialises the engine with the roots the
   * description declares, so the checkout became a source root. Closing never
   * re-initialised, so it stayed one: every later create under
   * `<checkout>/products/...` was refused with "is inside a source root",
   * unchecking the box in the next wizard changed nothing, and the only cure was
   * opening some other product whose sources happened to exclude the checkout.
   *
   * ADR-0013: "Start-screen create runs against the repository workspace the
   * engine already knows from boot -- not against an open product session."
   * Two halves make that true, and this claim fails if either regresses: the
   * wizard no longer offers a root that contains the destination, and
   * `ProductSessionService.close` returns the engine to its boot roots.
   */
  test("a second product can be created after the first is closed [ADR-0013 §Where the file is created]", async ({
    freshStudio,
  }) => {
    const first = "conformance-first";
    const second = "conformance-second";
    const { page } = freshStudio;

    try {
      await settled(page);

      for (const productId of [first, second]) {
        await openCreateWizard(page);
        await page.locator("[data-create-id]").fill(productId);
        await page.locator("[data-create-name]").fill(productId);
        await chooseSuggestedDestination(page);
        await expect(page.locator(".gbx-create-preview")).toContainText(`id = "${productId}"`, {
          timeout: 15_000,
        });

        // The refusal this claim is about was raised by the dry run, so it would
        // be in the pane instead of a product.
        await expect(page.locator(".gbx-create-preview")).not.toContainText(
          "inside a source root",
        );

        await page.locator("[data-create-submit]").click();
        await acceptPreview(page);
        await productSection(page, "overview");
        await expect(page.locator("[data-resolved-profile]")).toBeVisible({ timeout: 60_000 });
        expect(existsSync(join(REPO, "products", productId, "product.gdl"))).toBe(true);

        await runCommand(page, "Close Product");
        await expectContext(page, "home");
      }
    } finally {
      removeProductDir(first);
      removeProductDir(second);
    }
  });

  test(
    "Clone Git reviews the checkout before it creates anything [ADR-0013 §Amendment: clone review]",
    async ({ freshStudio }) => {
      // The flow was: type a URL, press Create, and the clone, the search for a
      // `product.gdl` and the write all happened behind that one press. So "the
      // URL is wrong", "that branch does not exist" and "this repository has no
      // product in it" were all failures *of a write*, and a failed attempt left
      // a directory whose deterministic name made every retry fail on `already
      // exists`.
      //
      // Cloned from a local repository this test makes, so the claim needs no
      // network and no fixture repository to exist anywhere: `git clone` takes a
      // path, and what is under review is Studio's flow rather than git's.
      const { page } = freshStudio;
      const cloned = "conformance-cloned";
      const origin = join(REPO, "ide", ".tmp-clone-origin");
      rmSync(origin, { recursive: true, force: true });
      try {
        execFileSync("git", ["init", "-q", "--initial-branch", "trunk", origin]);
        execFileSync("git", ["-C", origin, "config", "user.email", "t@example.invalid"]);
        execFileSync("git", ["-C", origin, "config", "user.name", "Test"]);
        writeFileSync(join(origin, "product.gdl"), readFileSync(DEMO, "utf8"));
        execFileSync("git", ["-C", origin, "add", "product.gdl"]);
        execFileSync("git", ["-C", origin, "commit", "-qm", "one product"]);
        const head = execFileSync("git", ["-C", origin, "rev-parse", "HEAD"], {
          encoding: "utf8",
        }).trim();

        await settled(page);
        await openCreateWizard(page);
        await page.locator('[data-create-modes] [data-create-mode="clone-git"]').click();

        // Refused by being unavailable, not by a message after the fact: Create
        // used to be enabled with no URL at all and said so only once pressed.
        const review = page.locator("[data-clone-review]");
        await expect(review).toBeDisabled();
        const create = page.locator("[data-create-submit]");
        await expect(create).toBeDisabled();

        // **The other gates satisfied up front, so that what follows is about
        // the review and nothing else.** Create needs an id and a destination
        // too -- the destination because ADR-0013 forbids assuming one -- and a
        // claim about the review has to hold those constant or it cannot tell
        // which gate it is observing.
        await page.locator("[data-create-id]").fill(cloned);
        await page.locator("[data-create-name]").fill("Cloned From Git");
        await chooseSuggestedDestination(page);
        await expect(create, "an id and a destination are not a checkout").toBeDisabled();

        await page.locator("[data-clone-git-url]").fill(origin);
        await expect(review).toBeEnabled();
        // Still nothing to create from: a URL is a thing a person typed, and a
        // review is a checkout that exists.
        await expect(create).toBeDisabled();

        await review.click();
        await expect(page.locator("[data-clone-status='reviewing']")).toBeVisible({
          timeout: 60_000,
        });

        // The two facts a URL does not carry. The commit, because "cloned trunk"
        // is not what a person needs to know when a repository moves...
        await expect(page.locator("[data-clone-commit]")).toHaveAttribute("data-clone-commit", head);
        // ...and which `product.gdl` in the repository is meant. One is a fact
        // and reads as one; several would be a choice.
        await expect(page.locator("[data-clone-candidate]")).toContainText("product.gdl");
        await expect(create).toBeEnabled();

        // The checkout belongs to the URL that produced it: changing the field
        // throws it away rather than letting Create write from a repository the
        // form no longer names. Checked here, before the create consumes it.
        await page.locator("[data-clone-git-url]").fill(`${origin}-elsewhere`);
        await expect(page.locator("[data-clone-status='idle']")).toBeVisible();
        await expect(create).toBeDisabled();
        await page.locator("[data-clone-git-url]").fill(origin);
        await review.click();
        await expect(page.locator("[data-clone-status='reviewing']")).toBeVisible({
          timeout: 60_000,
        });
        await expect(create).toBeEnabled();

        // **And it creates.** The claim used to stop here, at "Create is
        // enabled", which is exactly where the happy path was broken: the
        // checkout landed beside the repository rather than inside the declared
        // workspace, so the engine refused `clone_from` as a path outside the
        // workspace and every source root -- correctly -- and Create failed
        // after a clone that had worked. A review step that does not end in a
        // product is a review of nothing.
        await expect(page.locator(".gbx-create-preview")).toContainText(`id = "${cloned}"`, {
          timeout: 30_000,
        });
        await create.click();
        await acceptPreview(page);
        await productSection(page, "overview");
        await expect(page.locator("[data-resolved-profile]")).toBeVisible({ timeout: 90_000 });
        expect(existsSync(join(REPO, "products", cloned, "product.gdl"))).toBe(true);

        // The checkout has nothing left to hold, so a successful create discards
        // it -- one of the terminal transitions, and the one that would leak a
        // directory per product if it were forgotten.
        await expect(page.locator("[data-clone-status]")).toHaveCount(0);
        const attempts = join(REPO, ".gearbox", "git-clones");
        expect(
          existsSync(attempts) ? readdirSync(attempts) : [],
          "a successful create left its checkout behind",
        ).toEqual([]);
      } finally {
        rmSync(origin, { recursive: true, force: true });
        removeProductDir(cloned);
      }
    },
  );

  test("mode selector is visible on New Product from Start [ADR-0013 amendment]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);
    await openCreateWizard(page);
    await expect(page.locator(".gbx-create [data-create-modes]").first()).toBeVisible();
    await expect(page.locator('[data-create-modes] [data-create-mode="blank"]')).toBeVisible();
    await expect(page.locator('[data-create-modes] [data-create-mode="clone-local"]')).toBeVisible();
    await expect(page.locator('[data-create-modes] [data-create-mode="clone-git"]')).toBeVisible();
  });

  test("the destination is choosable, and sources are relative to it [ADR-0013 §Amendment: destination picker]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);
    await openCreateWizard(page);

    // The affordance the wizard was missing: a folder to put the product in,
    // rather than "the first workspace root, silently".
    const browse = page.locator("[data-destination-browse]");
    await expect(browse).toBeVisible();
    await expect(browse).toBeEnabled();

    // **Nothing is chosen yet, and the wizard says so rather than guessing.**
    // The ADR's sentence is "It must not silently take the first workspace
    // root", and the field used to open pre-filled with exactly that, computed
    // by index -- so Create could be pressed without anybody choosing a folder.
    // The suggestion is a placeholder now, which keeps it visible and one click
    // from `Choose…` without letting it stand in for a decision.
    const destination = page.locator("[data-create-destination]");
    await expect(destination).toHaveValue("");
    await expect(destination).toHaveAttribute("placeholder", /\/products\/.*\/product\.gdl$/);
    await expect(
      page.locator("[data-create-submit]"),
      "a create with no destination is a create against the first workspace root",
    ).toBeDisabled();

    // What the picker is *for*, asserted without opening a modal this suite would
    // then have to close: the description's own directory decides how `sources`
    // are written. `relativeSource` used to rebuild the default path and measure
    // from there, so any other destination produced `at = path(...)` entries
    // pointing at nothing -- the picker turns that from a typo into one click.
    const sourceAt = async (): Promise<string> => {
      const preview = await page.locator(".gbx-create-preview").innerText();
      return /at = path\("([^"]+)"\)/.exec(preview)?.[1] ?? "";
    };
    const chosen = (await destination.getAttribute("placeholder")) ?? "";
    expect(chosen, "the wizard must suggest somewhere").not.toBe("");
    await destination.fill(chosen);
    await expect(page.locator("[data-create-submit]")).toBeEnabled();

    await expect
      .poll(async () => (await sourceAt()).length, { timeout: 15_000 })
      .toBeGreaterThan(0);
    const shallow = await sourceAt();

    // Relative, not absolute -- the other half of what this found. The corpus is a
    // sibling of this checkout, and a source named by absolute path is correct on
    // the machine that generated it and broken for everyone who clones the result.
    // `payments-demo` names its own source `../../../gears-rust`; that is the form.
    expect(shallow, "a new product must not name its sources by absolute path").not.toMatch(
      /^\//,
    );

    await destination.fill(chosen.replace(/\/product\.gdl$/, "/deeper/product.gdl"));

    // One directory further down is one `../` further up. Equality would mean the
    // destination is decoration.
    await expect.poll(async () => await sourceAt(), { timeout: 15_000 }).toBe(`../${shallow}`);
  });

  /**
   * The root the product lands in is not offered as one of its sources.
   *
   * **This is where the "blocked forever" bug started.** The wizard pre-checked
   * every workspace root, and the first of them is this checkout, which contains
   * `products/`. A product declaring its own home as a source cannot be written
   * -- `writable_out_root` refuses any path inside a source root -- but the first
   * create in a session succeeded anyway, because at boot the engine's roots are
   * the corpus only. It then *opened* the new product, which re-initialised the
   * engine with the roots that product declared, and from then on every create
   * under `<checkout>/products/...` was refused with "is inside a source root".
   */
  test("a root containing the destination is not offered as a source [ADR-0013 §Where the file is created]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);
    await openCreateWizard(page);

    const destination = page.locator("[data-create-destination]");
    const inside = (await destination.getAttribute("placeholder")) ?? "";
    expect(inside).not.toBe("");
    await destination.fill(inside);

    // The checkout contains the suggested destination, so it must be refused as
    // a source; the corpus is a sibling and must still be offered.
    const unusable = page.locator("[data-create-source][data-source-unusable='true']");
    await expect(unusable).toHaveCount(1);
    await expect(unusable.locator("input")).toBeDisabled();
    await expect(unusable.locator("input")).not.toBeChecked();

    const usable = page.locator("[data-create-source]:not([data-source-unusable='true'])");
    expect(await usable.count(), "the corpus is a sibling and stays selectable").toBeGreaterThan(0);
    await expect(usable.first().locator("input")).toBeEnabled();
  });

  test("a blank or malformed product id is refused before the preview [ADR-0013 §Confirmation]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);
    await openCreateWizard(page);

    const destination = page.locator("[data-create-destination]");
    await destination.fill((await destination.getAttribute("placeholder")) ?? "");

    const id = page.locator("[data-create-id]");
    const submit = page.locator("[data-create-submit]");
    const preview = page.locator(".gbx-create-preview");

    // A single space used to be written as `id = " "`, and the product it made
    // opened and resolved with no diagnostics at all.
    // Two refusals, worded for what is wrong: nothing there at all, or something
    // there that is not an id.
    await id.fill(" ");
    await expect(submit).toBeDisabled();
    await expect(preview).toContainText("needs an id", { timeout: 15_000 });

    for (const bad of ["Demo", "demo_product", "de--mo"]) {
      await id.fill(bad);
      await expect(submit).toBeDisabled();
      await expect(preview).toContainText("is not a product id", { timeout: 15_000 });
    }

    await id.fill("conformance-id-ok");
    await expect(submit).toBeEnabled();
    await expect(preview).toContainText("conformance-id-ok", { timeout: 15_000 });
  });

  test("Clone Local stamps version into the preview [ADR-0013 amendment]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);

    await openCreateWizard(page);
    await page.locator('[data-create-modes] [data-create-mode="clone-local"]').click();
    await page.locator("[data-clone-path]").fill(DEMO);
    await chooseSuggestedDestination(page);
    await page.locator("[data-create-version]").fill("9.9.9");
    await expect(page.locator(".gbx-create-preview")).toContainText(`version = "9.9.9"`, {
      timeout: 15_000,
    });
    await expect(page.locator("[data-clone-sources-note]")).toContainText(
      "relative paths re-based to the new folder",
    );
    await page.locator("[data-create-cancel]").click();
  });

  test("Clone keeps the source comment line count [ADR-0013 §Confirmation]", async ({ freshStudio }) => {
    const { page } = freshStudio;
    await settled(page);
    const comments = commentLines(readFileSync(DEMO, "utf8"));

    await openCreateWizard(page);
    await page.locator('[data-create-modes] [data-create-mode="clone-local"]').click();
    await page.locator("[data-clone-path]").fill(DEMO);
    await chooseSuggestedDestination(page);
    await expect
      .poll(async () => commentLines(await page.locator(".gbx-create-preview").innerText()), {
        timeout: 15_000,
      })
      .toBe(comments);
    await page.locator("[data-create-cancel]").click();
  });
});

test.describe("a clone and a create say what they will write", () => {
  test("a clone written deeper still names the same sources [ADR-0013 §Amendment: a clone keeps what its paths mean]", async ({
    freshStudio,
  }) => {
    // Measured before the fix: Clone Local one level deeper than
    // `payments-demo` wrote `path("../../../gears-rust")` verbatim, the file
    // evaluated, the preview and the write agreed -- and the product could not
    // be opened, because its only source root did not exist. Asserted on the
    // preview alone: the dry run is the text the write would produce, and a
    // write would land in `products/`, which this harness refuses to dirty.
    const { page } = freshStudio;
    await settled(page);
    await openCreateWizard(page);
    await page.locator('[data-create-modes] [data-create-mode="clone-local"]').click();
    await page.locator("[data-clone-path]").fill(DEMO);
    const destination = page.locator("[data-create-destination]");
    const suggested = (await destination.getAttribute("placeholder")) ?? "";
    expect(suggested, "the wizard must suggest a destination").toMatch(/\/product\.gdl$/);
    const preview = page.locator(".gbx-create-preview");

    // Beside the source's own folder, at its depth: the spelling is unchanged.
    await destination.fill(suggested);
    await expect(preview).toContainText('at = path("../../../gears-rust")', { timeout: 15_000 });

    // One folder deeper is one `../` more, for every path the file states
    // relative to itself -- the source and the self-hosted target directory.
    await destination.fill(suggested.replace(/\/product\.gdl$/, "/deeper/product.gdl"));
    await expect(preview).toContainText('at = path("../../../../gears-rust")', {
      timeout: 15_000,
    });
    await expect(preview).toContainText('target_dir = "../../../../gears-rust/target"');
    await page.locator("[data-create-cancel]").click();
  });

  test("Create waits for the preview of what is in the fields [ADR-0013 §Amendment: Create waits for its preview]", async ({
    freshStudio,
  }) => {
    // The pane refreshes on a debounce plus a dry run, and for that window it
    // used to show the previous answer beside a live Create -- pressed then, the
    // create was refused after the person confirmed. One snapshot straight after
    // the keystroke: the window lasts at least the 200 ms debounce, so a Create
    // that is live in it is live on a stale preview.
    const { page } = freshStudio;
    await settled(page);
    await openCreateWizard(page);
    const destination = page.locator("[data-create-destination]");
    const submit = page.locator("[data-create-submit]");
    const preview = page.locator(".gbx-create-preview");
    const suggested = (await destination.getAttribute("placeholder")) ?? "";

    await destination.fill(suggested);
    await expect(preview).toContainText("at = path(", { timeout: 15_000 });
    await expect(submit).toBeEnabled();

    await destination.fill(suggested.replace(/\/product\.gdl$/, "/deeper/product.gdl"));
    // Polled rather than read once, with a budget well inside the 200 ms
    // debounce: a Theia widget renders on the next animation frame, not with the
    // keystroke, so one read can precede the render and see the old button. The
    // window is at least the debounce long, so a Create that is still live
    // after 150 ms is live on a stale preview.
    await expect
      .poll(
        () =>
          page.evaluate(() => ({
            disabled: (document.querySelector("[data-create-submit]") as HTMLButtonElement)
              .disabled,
            ready: document.querySelector(".gbx-create-preview")?.getAttribute("data-preview-ready"),
          })),
        { message: "Create must wait for the preview of the new destination", timeout: 150, intervals: [10] },
      )
      .toEqual({ disabled: true, ready: "false" });

    // And comes back once the answer for the new destination is on screen.
    await expect(preview).toHaveAttribute("data-preview-ready", "true", { timeout: 15_000 });
    await expect(submit).toBeEnabled();
    await page.locator("[data-create-cancel]").click();
  });
});

test.describe("a product that cannot be opened", () => {
  let broken: ProductCopy;

  test.beforeEach(() => {
    broken = copyProduct(REPO, "payments-demo", "missing-source", (text) =>
      text.replace('path("../../../gears-rust")', 'path("../../../no-such-corpus")'),
    );
  });

  test.afterEach(() => {
    broken?.dispose();
  });

  test("does not take the engine with it [ADR-0013 §Amendment: a failed open puts the engine back]", async ({
    freshStudio,
  }) => {
    // Measured before the fix: the open re-initialized the engine on the
    // product's roots, the missing one failed, and nothing put the old session
    // back. Start said "Engine disconnected", New Product, New Gear and Continue
    // were disabled, and Retry repeated the same failure.
    const { page } = freshStudio;
    await settled(page);
    expect(readFileSync(broken.path, "utf8")).toContain("no-such-corpus");

    await runCommand(page, "Open Product…");
    const options = page.locator(`.quick-input-list [role="option"]`);
    await options.first().waitFor({ state: "visible", timeout: 30_000 });
    await options.filter({ hasText: broken.id }).first().click();

    await expect(
      page.locator(".theia-notification-message").filter({ hasText: "no-such-corpus" }).first(),
      "the refusal names the root that is not there",
    ).toBeVisible({ timeout: 90_000 });
    await expectContext(page, "home");

    await expect
      .poll(() => page.locator('[data-engine-status="disconnected"]').count(), {
        message: "the engine is back on the session the open replaced",
        timeout: 60_000,
      })
      .toBe(0);
    await expect(page.locator('[data-start-action="create"]')).toBeEnabled({ timeout: 30_000 });
  });
});

test.describe("a gear created for a product ends up in it", () => {
  // The whole flow, and the reason it needed engine work. A scaffold cannot land
  // inside a source root -- `writable_out_root` refuses it, tier 5 of ADR-0010 is
  // why -- so a gear created for a product is in a directory that product does
  // not read. `use_gear` alone would name a gear from a source the description
  // does not declare. So the finish is one `applyEdits` batch: `add_source` then
  // `add_gear`.
  //
  // **Against `payments-demo`, not a product made here.** A product created by
  // the wizard declares a source that *contains* `<product>/gears`, so the
  // scaffold is refused before this flow starts -- which is correct behaviour and
  // the wrong subject for this claim. The demo's own source is a sibling
  // checkout, which is the ordinary shape. The description and the scaffolded
  // directory both go back in `finally`.
  //
  // **On its own Studio.** Run on the shared session, it left that session
  // inside a product whose description had been edited behind it and then
  // restored by `git checkout`, and the next claim on the session ("a config
  // edit changes one line") failed in every full run while passing alone.
  const GEAR = "conformance-audit";
  const SCAFFOLD = join(REPO, "products/payments-demo/gears");

  test("Create Gear declares its folder as a source and adds the gear [ADR-0013 §Amendment: create for a product]", async ({
    freshStudio: studio,
  }) => {
    expect(diffOf(DEMO_REL)).toBe("");
    try {
      await openProduct(studio.page, "dev");

      // From inside the product, so the panel is told which product it is for.
      await studio.page.getByLabel("More add actions", { exact: true }).click();
      await studio.page.locator("[data-create-gear]").click();
      await expect(studio.page.locator("[data-create-gear-for]")).toContainText("Payments Demo", {
        timeout: 30_000,
      });
      await studio.page.locator("[data-create-gear-id]").fill(GEAR);
      await expect(
        studio.page.locator(".gbx-create-preview [data-plan-path]").first(),
      ).toBeVisible({ timeout: 30_000 });
      await studio.page.locator("[data-create-gear-submit]").click();

      // One preview for the description edit, and it names both halves.
      const preview = studio.page.locator(".dialogBlock", {
        has: studio.page.locator(".gbx-edit-preview"),
      });
      await expect(preview).toBeVisible({ timeout: 60_000 });
      await expect(preview).toContainText("source(");
      await expect(preview).toContainText(`use_gear("${GEAR}"`);
      await studio.page.locator(".dialogBlock .theia-button.main").click();

      // And the description says both, which is what makes the gear reachable.
      await expect
        .poll(() => readFileSync(join(REPO, DEMO_REL), "utf8"), { timeout: 60_000 })
        .toContain(`use_gear("${GEAR}"`);
      const text = readFileSync(join(REPO, DEMO_REL), "utf8");
      expect(text).toMatch(/source\(id = "gears", at = path\("gears"\)\)/);
      // Span surgery, so the description's 29 comments are still there.
      expect(commentLines(text)).toBe(commentLines(execFileSync("git", ["show", `HEAD:${DEMO_REL}`], { cwd: REPO, encoding: "utf8" })));

      // Back in the Product workspace, which is where the flow started.
      await expect(studio.page.locator(".gbx-product")).toBeVisible({ timeout: 60_000 });

      // **And the gear is one the catalogue has.** The scaffold used to write a
      // lib of comments, so there was no `#[toolkit::gear]` to project (GBX0211)
      // and the product the gear had just joined failed with GBX0301. With the
      // toolkit found in the session's roots, the skeleton is a real gear.
      //
      // **Read once the resolution has settled, and read it twice.** A single
      // "clean" sample passed while the re-resolve was still in flight, so this
      // check stayed green through the defect it names: the batch adds a source,
      // the engine's roots were the old ones until the product was reopened, and
      // the settled answer was GBX0301.
      await productSection(studio.page, "validation");
      const settledText = async (): Promise<string> => {
        await expect(studio.page.locator('.gbx-toolbar [data-status="working"]')).toHaveCount(0, {
          timeout: 90_000,
        });
        return studio.page.locator(".gbx-product").innerText();
      };
      await studio.page.waitForTimeout(3_000);
      expect(await settledText()).not.toMatch(/GBX0301|GBX0211/);
      await studio.page.waitForTimeout(3_000);
      expect(await settledText()).not.toMatch(/GBX0301|GBX0211/);
    } finally {
      rmSync(SCAFFOLD, { recursive: true, force: true });
      if (diffOf(DEMO_REL) !== "") {
        execFileSync("git", ["checkout", "--", DEMO_REL], { cwd: REPO });
      }
    }
  });
});

test.describe("edit config and profiles in the open product", () => {

  test("a config edit changes one line [ADR-0013 §Confirmation]", async ({ studio }) => {
    expect(diffOf(DEMO_REL)).toBe("");

    try {
      await openProduct(studio.page, "dev");
      // **Configured where a product is configured.** This reached the form
      // through the catalogue and the Inspector, which worked only because the
      // Product panel was on Overview and therefore rendering no form of its
      // own -- an unscoped `[data-gear-config]` that matched once by accident of
      // stage. Configuring is the Composition pane's act now, and `configureGear`
      // is how a person performs it.
      const form = await configureGear(studio.page, "api-gateway");

      // Free keys live under "Other keys" now -- see `openAdvancedKeys`. Opened
      // and typed into as one retried step: the section folds on any remount, so
      // the two must not be separated by an await that a reload can land in.
      await expect(async () => {
        await openAdvancedKeys(studio.page, ".gbx-composition-settings");
        await form.locator("[data-config-new-key]").fill("demo_mode", { timeout: 5_000 });
      }).toPass({ timeout: 30_000 });
      await form.locator("[data-config-new-value]").fill("demo_value");
      await form.locator('[data-add-config="api-gateway"]').click();
      await studio.page.locator(".gbx-workspace-draft [data-draft-apply]").click();
      await acceptPreview(studio.page);
      await expect(form.locator('[data-config-key="demo_mode"]')).toBeVisible({
        timeout: 30_000,
      });
      // **Polled, because the row above does not say the write landed.** The row
      // renders from the draft overlay the moment the key is added, so it is
      // already on screen before Apply is clicked -- which made this a read of
      // `git diff` racing the file write, and it lost. The file is the only thing
      // here that answers "was it written", so it is what this waits on.
      await expect.poll(() => diffOf(DEMO_REL), { timeout: 30_000 }).toContain(
        "1 insertion(+)",
      );

      // Undo through the interface, not with `git checkout` + reload: restoring
      // the file behind the app leaves the store holding the edited description
      // (§9.1), and a reload of the shared worker page races the next test against
      // an empty catalogue `rootPaths()`.
      await removeKey(form, "demo_mode");
      await studio.page.locator(".gbx-workspace-draft [data-draft-apply]").click();
      await acceptPreview(studio.page);
      await expect(form.locator('[data-config-key="demo_mode"]')).toHaveCount(0, {
        timeout: 30_000,
      });
      // The same race in the other direction: the draft's removal takes the row
      // off screen before the second write reaches disk.
      await expect.poll(() => diffOf(DEMO_REL), { timeout: 30_000 }).toBe("");
    } finally {
      if (diffOf(DEMO_REL) !== "") {
        execFileSync("git", ["checkout", "--", DEMO_REL], { cwd: REPO });
      }
    }
  });

  test("draft edits two config keys with one Apply preview; Discard restores [ADR-0013 §Confirmation]", async ({
    studio,
  }) => {
    expect(diffOf(DEMO_REL)).toBe("");

    try {
      await openProduct(studio.page, "dev");
      // **Scoped to the form, and the reason for scoping has changed.** It read
      // `.gbx-inspector`, because `GearSettings` was rendered by two surfaces
      // and an unscoped locator depended on which stage the Product panel
      // happened to be on. One surface renders it now; the scope stays because a
      // locator that names the object it is about survives the next rearrangement
      // too.
      const form = await configureGear(studio.page, "api-gateway");

      // Free keys live under "Other keys" now -- see `openAdvancedKeys`.
      //
      // **The section is reopened on every attempt, and that is not belt and
      // braces.** It is open exactly when it holds something, so any remount
      // between opening it and typing into it folds it again -- and `fill` then
      // waits on an invisible input until the whole test times out. The remount
      // that did it came from the *previous* test's trailing reload, which is to
      // say from outside this test entirely; a fold is recoverable, so this
      // recovers from it rather than reading it as a failure.
      //
      // **And the whole add is inside the retry, not just the key.** The value
      // and the Add button fold with the key: with the key alone guarded, the
      // remount landed one line later -- twice, on the value `fill` and then on
      // the click -- in full-file runs, while the claim passed on its own. The
      // row appearing is what "added" means, so it is the condition; a second
      // pass after a click that did land replaces the same draft slot.
      const queue = async (key: string, value: string): Promise<void> => {
        await expect(async () => {
          await openAdvancedKeys(studio.page, ".gbx-composition-settings");
          await form.locator("[data-config-new-key]").fill(key, { timeout: 5_000 });
          await form.locator("[data-config-new-value]").fill(value, { timeout: 5_000 });
          await form.locator('[data-add-config="api-gateway"]').click({ timeout: 5_000 });
          await expect(form.locator(`[data-config-key="${key}"]`)).toBeVisible({
            timeout: 5_000,
          });
        }).toPass({ timeout: 30_000 });
      };
      await queue("draft_a", "one");
      await queue("draft_b", "two");
      await expect(form.locator('[data-config-key="draft_a"]')).toBeVisible();
      await expect(form.locator('[data-config-key="draft_b"]')).toBeVisible();
      expect(diffOf(DEMO_REL)).toBe("");

      // **The pair is in the header, and there is exactly one.** It used to be
      // rendered by this panel *and* by the Product view, both gated on the
      // product-wide `hasDraft()`, so a locator scoped to the gear-config block
      // was picking one of two buttons that did the same thing to the same draft
      // and remounted different inputs.
      const draft = studio.page.locator(".gbx-workspace-draft");
      await expect(draft.locator("[data-draft-apply]")).toHaveCount(1);
      await draft.locator("[data-draft-discard]").click();
      await expect(form.locator('[data-config-key="draft_a"]')).toHaveCount(0);
      await expect(form.locator('[data-config-key="draft_b"]')).toHaveCount(0);
      await expect(draft.locator("[data-draft-apply]")).toHaveCount(0);
      expect(diffOf(DEMO_REL)).toBe("");

      // Discard emptied the section, so it folded again -- it is open exactly
      // when it holds something, because a key somebody set is not advanced any
      // more. Reopening is what a person does to add the pair a second time, and
      // `queue` is where that now happens.
      await queue("draft_a", "one");
      await queue("draft_b", "two");
      await draft.locator("[data-draft-apply]").click();
      // **One confirmation, by its own class.** This read "a `.dialogBlock`
      // containing a preview", which stopped identifying the write dialog when
      // adding a gear became a modal that also shows what it would write.
      await expect(studio.page.locator(".gbx-edit-confirm")).toHaveCount(1);
      await acceptPreview(studio.page);
      await expect(form.locator('[data-config-key="draft_a"]')).toBeVisible({ timeout: 30_000 });
      await expect(form.locator('[data-config-key="draft_b"]')).toBeVisible();
      // Polled for the reason stated below, and in this direction too: the rows
      // above are on screen from the draft overlay before Apply is clicked, so
      // they cannot be what says the write happened.
      await expect.poll(() => diffOf(DEMO_REL), { timeout: 30_000 }).not.toBe("");

      // **Each removal asserts itself.** These were two blind clicks, and a
      // remount between them -- the previous Apply's reload is still settling,
      // since the poll above waits on the *file* and the re-resolve continues
      // after it -- ate one. The Apply then carried a draft of one removal, the
      // file kept the other key, and the failure read exactly like the race the
      // poll below guards against. It was not a race: it was a lost click, and
      // a stable wrong answer.
      await removeKey(form, "draft_a");
      await removeKey(form, "draft_b");
      await draft.locator("[data-draft-apply]").click();
      await acceptPreview(studio.page);
      // Polled, not sampled: accepting the preview starts the write, and reading
      // `git diff` on the next line races it -- one key already removed and the
      // other not yet looks exactly like the failure this asserts against.
      await expect
        .poll(() => diffOf(DEMO_REL), { timeout: 30_000 })
        .toBe("");
    } finally {
      if (diffOf(DEMO_REL) !== "") {
        execFileSync("git", ["checkout", "--", DEMO_REL], { cwd: REPO });
      }
    }
  });

  test("a config key named password is refused with an explanation [ADR-0013 §Confirmation]", async ({
    studio,
  }) => {
    await openProduct(studio.page, "dev");
    const form = await configureGear(studio.page, "api-gateway");

    // Free keys live under "Other keys" now -- see `openAdvancedKeys`. Opened,
    // filled and submitted as one retried step: the section folds on any
    // remount, and one landing between the fill and the click left "Add key"
    // hidden for the rest of the claim (a full run on 2026-10-21).
    await expect(async () => {
      await openAdvancedKeys(studio.page, ".gbx-composition-settings");
      await form.locator("[data-config-new-key]").fill("password", { timeout: 5_000 });
      await form.locator("[data-config-new-value]").fill("literal", { timeout: 5_000 });
      await form.locator('[data-add-config="api-gateway"]').click({ timeout: 5_000 });
    }).toPass({ timeout: 30_000 });
    // Matched among the notifications rather than at the top of the stack. The
    // claim is that the refusal is *said*, and notifications accumulate in the
    // shared session -- reading `.first()` made this assert which message was
    // most recent, so an unrelated success further up the file failed it.
    await expect(
      studio.page
        .locator(".theia-notification-message", {
          hasText: /refusing|password|secret|could not be edited/i,
        })
        .first(),
    ).toBeVisible({ timeout: 10_000 });
    expect(diffOf(DEMO_REL)).toBe("");
  });

  test("one connection is edited without touching the one beside it [ADR-0013 §Amendment: a plugin is not a selected gear]", async ({
    studio,
  }) => {
    // **The claim the per-entry editor exists for.** `set_plugins` rewrites a
    // host's whole list as bare `plugin("id")` entries, which drops the profiles
    // and config of every entry it did not mean to touch. That is why
    // `PluginTarget` addresses one written position and why this asserts on the
    // *diff* rather than on the form: the proof is what would reach the file.
    //
    // Nothing is written. The draft is discarded and both confirmations are
    // cancelled, so the `finally` below is a net under a failed assertion rather
    // than a cleanup.
    const { page } = studio;
    expect(diffOf(DEMO_REL), "the description must start clean").toBe("");

    try {
      await openProduct(page, "dev");

      // `authn-resolver` holds two connections, and the precondition is that
      // both are on screen: the one this profile does not select is marked, not
      // hidden. Which of them is *active* is `prd-product.spec.ts`'s claim.
      await productSection(page, "composition");
      const connections = page.locator('[data-plugin-host="authn-resolver"]');
      await expect(connections).toHaveCount(2);
      await expect(
        page.locator('[data-plugin-id="oidc-authn-plugin"]'),
        "a connection inactive in this profile stays visible and editable",
      ).toHaveAttribute("data-plugin-active", "false");

      // Entry 0 is `static-authn-plugin`, written `profiles = ["dev", "local"]`.
      const form = await configureConnection(page, "authn-resolver", 0);
      // Read as a map rather than as a list: the boxes are the product's
      // profiles, and asserting their *order* would be asserting how the intent
      // serialises its profile map, which is not this claim's business.
      await expect(
        form.locator("[data-profile-scope]"),
        "the scope is narrow, and says so rather than leaving it to be inferred",
      ).toHaveAttribute("data-profile-scope", "selected");
      expect(
        Object.fromEntries(
          await form.locator("[data-profile]").evaluateAll((boxes) =>
            boxes.map((box) => [
              box.getAttribute("data-profile") ?? "",
              (box as HTMLInputElement).checked,
            ]),
          ),
        ),
        "the boxes say what the description writes, and nothing more",
      ).toEqual({ dev: true, local: true, prod: false });

      // One scope change and one typed config value, into one draft. `priority`
      // is an `i16` on `StaticAuthNPluginConfig`, so the projected control is a
      // number box -- a field is typed here because Rust said so, which is the
      // half of this that a Rust test cannot reach.
      await form.locator('[data-profile="prod"]').click();
      await form.locator('[data-config-field="priority"] input').fill("50");
      const toolbar = page.locator(".gbx-workspace-draft");
      await expect(toolbar.locator("[data-draft-apply]")).toHaveCount(1);

      await toolbar.locator("[data-draft-apply]").click();
      const confirm = page.locator(".gbx-edit-confirm");
      await expect(confirm).toBeVisible({ timeout: 60_000 });

      // Named by ordinal, because a host may hold one plugin twice and "remove
      // `static-authn-plugin`" would not say which.
      const targets = confirm.locator("[data-edit-target]");
      await expect(targets).toHaveCount(2);
      await expect(targets.first()).toContainText("connection 1");

      // **The diff is the claim.** The edited entry changes and the sibling is
      // absent from it entirely -- not reformatted, not re-emitted.
      const diff = await confirm.locator(".gbx-edit-preview").innerText();
      expect(diff).toContain('"dev", "local", "prod"');
      expect(diff).toContain('"priority": 50');
      expect(diff, "the sibling connection is not in the diff").not.toContain(
        "oidc-authn-plugin",
      );

      await confirm.locator(".theia-button.secondary").click();
      await expect(confirm).toHaveCount(0);
      expect(diffOf(DEMO_REL), "cancelling writes nothing").toBe("");

      await toolbar.locator("[data-draft-discard]").click();
      await expect(toolbar.locator("[data-draft-apply]")).toHaveCount(0);

      // Removing addresses the same entry, and takes only it.
      const again = await configureConnection(page, "authn-resolver", 0);
      await again.getByText("Remove this connection").click();
      const removal = page.locator(".gbx-edit-confirm");
      await expect(removal).toBeVisible({ timeout: 60_000 });
      const removed = await removal.locator(".gbx-edit-preview").innerText();
      expect(removed).toContain("static-authn-plugin");
      expect(removed, "the sibling survives a removal too").not.toContain(
        "oidc-authn-plugin",
      );
      await removal.locator(".theia-button.secondary").click();
      await expect(removal).toHaveCount(0);
      expect(diffOf(DEMO_REL)).toBe("");
    } finally {
      if (diffOf(DEMO_REL) !== "") {
        execFileSync("git", ["checkout", "--", DEMO_REL], { cwd: REPO });
      }
    }
  });


  test("narrowing a connection's scope cannot silently widen it [ADR-0013 §Amendment: a plugin is not a selected gear]", async ({
    studio,
  }) => {
    // **The widest setting was reached by unchecking the last box.** An empty
    // scope means every profile -- `gearbox_ir::intent::applies` is
    // `scoped_to.is_empty() || scoped_to.contains(profile)`, and the writer
    // emits no argument at all rather than `profiles = []`, which would read as
    // "no profile". The model is right; the control lied about it, because
    // clearing the last checkbox looks exactly like switching something off.
    // The person who found this read the hint under the boxes and still had to
    // open the preview to learn what had happened.
    //
    // Nothing is written here: the scope is a draft, and it is discarded.
    const { page } = studio;
    expect(diffOf(DEMO_REL), "the description must start clean").toBe("");
    try {
      await openProduct(page, "dev");
      const form = await configureConnection(page, "authn-resolver", 0);

      // Written `profiles = ["dev", "local"]`, so narrowing to one is a real
      // move rather than an edge the fixture arranged.
      await form.locator('[data-profile="local"]').click();
      await expect(form.locator("[data-profile-scope]")).toHaveAttribute(
        "data-profile-scope",
        "selected",
      );
      const dev = form.locator('[data-profile="dev"]');
      await expect(dev).toBeChecked();
      await expect(
        dev,
        "the last profile standing refuses to be cleared, because clearing it means every profile",
      ).toBeDisabled();

      // And the widest setting is still reachable -- deliberately, by name.
      await form.locator("[data-profile-scope-all]").click();
      await expect(form.locator("[data-profile-scope]")).toHaveAttribute(
        "data-profile-scope",
        "all",
      );
      await expect(
        form.locator("[data-profile]"),
        "with every profile claimed there is nothing to pick from",
      ).toHaveCount(0);

      await page.locator(".gbx-workspace-draft [data-draft-discard]").click();
      await expect.poll(() => diffOf(DEMO_REL), { timeout: 30_000 }).toBe("");
    } finally {
      if (diffOf(DEMO_REL) !== "") {
        execFileSync("git", ["checkout", "--", DEMO_REL], { cwd: REPO });
      }
    }
  });

  /**
   * **Every kind the picker offers, because this claim used to cover one.**
   * It filled an id, confirmed, and never touched `[data-profile-new-kind]` --
   * so it exercised `embedded` and stayed green while `self_hosted` and
   * `kubernetes` failed every time: the form sent no fields, and the grammar
   * requires `host` + `worker_discovery` / `discovery`. A picker offering three
   * options of which two could not work, under a claim that said adding a
   * profile works.
   */
  for (const kind of ["embedded", "self_hosted", "kubernetes"] as const) {
    // **`freshStudio`, one context each.** Three tests that each add a profile to
    // the same description, remove it, and reload the store do not survive
    // sharing a page: the middle one saw the chip it had just removed, because
    // the previous test's trailing reload was still in flight. Isolation here is
    // cheaper than a wait tuned to whichever of the three runs second.
    test(`an added ${kind} profile appears in the switcher and resolves [ADR-0013 §Confirmation]`, async ({
      freshStudio,
    }) => {
      const studio = freshStudio;
      const profileId = `conformance-${kind.replace(/_/g, "-")}`;
      expect(diffOf(DEMO_REL)).toBe("");

      try {
        await settled(studio.page);
        await openProduct(studio.page, "dev");
        await studio.page.getByLabel("Product menu", { exact: true }).click();
        await studio.page.locator("[data-add-profile]").click();
        await studio.page.locator("[data-profile-new-id]").fill(profileId);
        await studio.page.locator("[data-profile-new-kind]").selectOption(kind);
        if (kind === "self_hosted") {
          // **The host application is asked for, not assumed.** The box used to
          // open holding `localhost` -- a hostname -- which became an application
          // of that name. Empty now, and Add waits for it.
          await expect(studio.page.locator("[data-profile-add-confirm]")).toBeDisabled();
          await studio.page.locator("[data-profile-new-host]").fill("gateway");
        }
        await studio.page.locator("[data-profile-add-confirm]").click();
        await acceptPreview(studio.page);
        await expect(studio.page.locator(`[data-product-profile] option[value="${profileId}"]`)).toBeAttached({
          timeout: 30_000,
        });
        await studio.page.locator("[data-product-profile]").selectOption(profileId);
        await expect(studio.page.locator(`[data-resolved-profile="${profileId}"]`)).toBeVisible({
          timeout: 60_000,
        });

        await studio.page.locator(`[data-remove-profile="${profileId}"]`).click();
        await acceptPreview(studio.page);
        await expect(studio.page.locator(`[data-product-profile] option[value="${profileId}"]`)).toHaveCount(0, {
          timeout: 30_000,
        });
        expect(diffOf(DEMO_REL)).toBe("");
      } finally {
        if (diffOf(DEMO_REL) !== "") {
          execFileSync("git", ["checkout", "--", DEMO_REL], { cwd: REPO });
        }
      }
    });
  }
});
