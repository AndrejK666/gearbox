// ADR-0011 amendment 2026-09-02 — session trust: Product opens, Discard restores,
// no window.prompt, Generate short label, Start buttons as the path.

import { execFileSync } from "node:child_process";

import {
  expect,
  expectContext,
  openProduct,
  paletteOffers,
  productSection,
  revealCatalogue,
  revealInspector,
  runCommand,
  settled,
  test,
} from "../fixtures/studio";

/**
 * Kill every running engine, the way a crash would.
 *
 * `pkill`, not a graceful shutdown: the claim below is about what the shell does
 * when the engine goes away *without* being asked, which is the state the UX
 * report found -- New Product still offered, the wizard opening as a blank tab.
 *
 * It kills **every** engine because there is one per frontend connection
 * (measured: one page, one `gearbox rpc`; two pages, two), and no way to tell
 * from outside which belongs to which page. So the test repairs both sessions
 * afterwards rather than leaving the shared one broken for everything that runs
 * after it.
 */
function killEngines(): void {
  try {
    execFileSync("pkill", ["-f", "gearbox rpc"]);
  } catch {
    // `pkill` exits 1 when nothing matched, which is not a failure here.
  }
}

test.describe("session trust [ADR-0011 amendment 2026-09-02]", () => {
  test("Open Product from Start shows the Product view without View menu [ADR-0011 §Amendment: Product openView]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);
    await expectContext(page, "home");
    // Prefer a Start list row: Open Product… opens a quick-pick that is flaky
    // when discover already named the only product. A list button always opens.
    await expect(page.locator(".gbx-start")).toBeVisible({ timeout: 60_000 });
    const listed = page.locator("[data-start-product]").first();
    await expect(listed).toBeVisible({ timeout: 60_000 });
    await listed.click();
    // Overview holds the resolved header, and a product opens on Composition.
    // What is claimed is that the list button opened *and resolved* a product.
    await expect(page.locator(".gbx-product")).toBeVisible({ timeout: 60_000 });
    await productSection(page, "overview");
    await expect(page.locator(".gbx-product [data-resolved-profile]")).toBeVisible({
      timeout: 60_000,
    });
    await expectContext(page, "product");
  });

  test("a dead engine disables the actions that need it, and Retry brings them back [ADR-0011 §Amendment: disconnected state]", async ({
    freshStudio,
    studio,
  }) => {
    // The first critical problem in the 2026-09-02 UX report: the engine was down
    // and the shell went on offering New Product, Resolve and Generate, so the
    // wizard opened as an empty tab with no error. The fix is
    // `EngineConnectionService`; this is what holds it.
    //
    // Both fixtures on purpose. `freshStudio` is the page under test; `studio` is
    // the shared one every later claim uses, and killing the engines takes its
    // engine too -- so this claim owns putting it back.
    const { page } = freshStudio;
    await settled(page);
    await expect(page.locator(".gbx-start")).toBeVisible({ timeout: 60_000 });

    try {
      killEngines();

      // Said, not merely implied by things not working.
      await expect(page.locator('[data-engine-status="disconnected"]')).toBeVisible({
        timeout: 60_000,
      });
      await expect(page.locator('[data-start-action="create"]')).toBeDisabled();

      // And not reachable by any other door. `QuickCommandService` filters by
      // `isEnabled`, so a command gated on the engine leaves the palette -- which
      // is what stops the empty wizard rather than merely greying a button.
      //
      // The match is on the words rather than on one id, and that is what caught
      // the hole: `NEW_PRODUCT` was gated while `View: Toggle New Product` --
      // registered automatically for the wizard's own view -- was not, and opened
      // the same blank tab. Two commands, one door.
      const offered = await paletteOffers(page, "New Product");
      expect(offered.filter((label) => /New Product/.test(label))).toEqual([]);
    } finally {
      // Retry is the user's way back, so the repair is also the assertion.
      await page.locator("[data-engine-retry]").click();
      await expect(page.locator('[data-engine-status="disconnected"]')).toHaveCount(0, {
        timeout: 60_000,
      });
      await expect(page.locator('[data-start-action="create"]')).toBeEnabled();

      // The shared session has its own engine and its own frontend; nothing tells
      // it to reconnect, so this does.
      await runCommand(studio.page, "Gearbox: Reload Catalogue");
      // Revealed, because the panel may be behind a collapsed left side: the
      // product context collapses that area on its first activation, and the
      // catalogue is a source of components there rather than the subject. What
      // this line is about is the *rows*, so it makes sure they are on screen
      // before asking whether they are.
      await revealCatalogue(studio.page);
      await expect(studio.page.locator(".gbx-widget-catalogue .gbx-row").first()).toBeVisible({
        timeout: 90_000,
      });
    }
  });

  test("toolbar Generate is labelled Generate, not Toggle [ADR-0011 §Amendment: Generate shortTitle]", async ({
    studio,
  }) => {
    await openProduct(studio.page, "dev");
    const generate = studio.page.locator('[data-command="gearbox.generate.toggle"]');
    await expect(generate).toBeVisible();
    await expect(generate).toHaveText(/^\s*Generate\s*$/);
  });

  test("Discard on a profile field restores the saved value [ADR-0013 §Amendment: Discard restores]", async ({
    studio,
  }) => {
    // **This claim passed while the behaviour was broken, and the difference was
    // which Discard.** The pair used to be rendered twice -- by the Inspector and
    // by the Product view, both gated on the product-wide `hasDraft()` -- and each
    // bumped a remount counter private to its own widget. `.first()` happened to
    // be the Product view's, so the input the test read was the one that had been
    // remounted; discarding from the Inspector cleared the `modified` badge and
    // left the typed text on screen, which is what the UX pass of 2026-09-07 saw.
    // There is one pair now, in the header, so the test asserts that too: a claim
    // about "the" Discard is only meaningful if there is one.
    await openProduct(studio.page, "local");
    const host = studio.page.locator('[data-profile-field="host"]').first();
    await expect(host).toBeVisible({ timeout: 30_000 });
    const saved = await host.inputValue();
    await host.fill("ux-test-host-should-not-stick");

    const discard = studio.page.locator("[data-draft-discard]");
    await expect(discard).toHaveCount(1);
    await expect(studio.page.locator('[data-status="modified"]')).toBeVisible();
    // The control says the edit is in it, because the buttons no longer do.
    await expect(host).toHaveAttribute("data-field-modified", "true");

    await discard.click();
    await expect(studio.page.locator('[data-profile-field="host"]').first()).toHaveValue(saved, {
      timeout: 10_000,
    });
    await expect(studio.page.locator('[data-status="modified"]')).toHaveCount(0);
    await expect(studio.page.locator('[data-profile-field="host"]').first()).not.toHaveAttribute(
      "data-field-modified",
      "true",
    );
  });

  test("Add profile uses an in-panel form, not window.prompt [ADR-0013 §Amendment: no window.prompt]", async ({
    studio,
  }) => {
    await openProduct(studio.page, "dev");
    // Profile edit sits under the selected profile in the Product tree.
    await studio.page.locator("[data-product-profile]").selectOption("dev");
    await studio.page.locator("[data-add-profile]").click();
    await expect(studio.page.locator("[data-profile-new-id]")).toBeVisible({ timeout: 15_000 });
    await expect(studio.page.locator("[data-profile-new-kind]")).toBeVisible();
    await studio.page.locator("[data-profile-add-cancel]").click();
    await expect(studio.page.locator("[data-profile-new-id]")).toHaveCount(0);
  });

  test("Home has no Product menu while Start is showing [ADR-0011 §Amendment: no Product menu on Home]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);
    await expectContext(page, "home");
    const menus = await page.locator("#theia-top-panel .p-MenuBar-itemLabel").allTextContents();
    expect(menus.map((m) => m.trim())).not.toContain("Product");
  });

  test("Inspector opens when an application is selected [ADR-0011 §Amendment: Inspector from selection]", async ({
    studio,
  }) => {
    await openProduct(studio.page, "prod");
    // Applications live on the Topology stage -- the panel is
    // `Overview · Gears · Topology · Validation` since 2026-09-07 -- and a
    // selection made there is the one this claim is about.
    await productSection(studio.page, "topology");
    await studio.page.locator("[data-application]").first().click();
    await expect(studio.page.locator(".gbx-inspector, .gbx-explain, .gbx-detail").first()).toBeVisible({
      timeout: 30_000,
    });
    // And it did not require hunting View → Inspector first.
    await revealInspector(studio.page);
    await expect(studio.page.locator("[data-explaining], .gbx-detail-title").first()).toBeVisible();
  });

  test("replacing the engine does not report the old one's death [ADR-0011 §Amendment: Product openView]", async ({
    rpc,
    stalledStudio,
  }) => {
    // **The mirror of the claim above, and the order is the claim.** That one
    // kills the engine and requires the shell to say so. This one replaces it —
    // every `initialize` disposes the previous handle — and requires the shell
    // *not* to say so, because a healthy engine is answering.
    //
    // `exited` for the disposed handle settles late by construction:
    // `dispose()` gives the child a second before SIGTERM and three before
    // SIGKILL. So the sequence this reproduces is exactly the one that was
    // broken — new engine live and serving, old engine's exit arriving
    // afterwards — and the notification used to be sent regardless of which
    // handle it was about, which marked the connection dead.
    const { page } = stalledStudio;
    const exits = rpc.observe("onEngineExit");

    // **A working session is shown by work, not by a badge.** There is no
    // "connected" marker in this shell — only `data-engine-status="disconnected"`,
    // which the Start screen and the create wizard raise when the engine is
    // down. So the evidence that a healthy engine is answering is that the
    // catalogue it projects is on screen.
    await settled(page);
    await revealCatalogue(page);
    const rows = page.locator(".gbx-widget-catalogue .gbx-row");
    await expect(rows.first()).toBeVisible({ timeout: 60_000 });
    await expect(page.locator('[data-engine-status="disconnected"]')).toHaveCount(0);

    // Reloading the catalogue re-initializes, which is what replaces the engine.
    await runCommand(page, "Gearbox: Reload Catalogue");
    await settled(page);
    await expect(rows.first(), "the new engine answers").toBeVisible({ timeout: 60_000 });

    // **Past the old child's death, not merely past the reload.** SIGKILL is
    // three seconds out; asserting before that would pass on a notification
    // that had not been sent yet.
    await page.waitForTimeout(5000);

    expect(
      exits(),
      "a replaced engine's exit is not news about the connection, so it is not sent",
    ).toBe(0);
    await expect(
      page.locator('[data-engine-status="disconnected"]'),
      "and the shell does not report a break while a healthy engine answers",
    ).toHaveCount(0);

    // And the session still works, which is what makes the absence meaningful.
    await expect(rows.first()).toBeVisible();
  });
});
