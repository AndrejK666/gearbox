// What a person is offered after the engine stops, and what survives it.
//
// Step 7 (`engine-timeout.spec.ts`) proved the mechanism: a missed deadline ends
// the engine, every later call refuses, `initialize` is the way back, nothing is
// replayed. It deliberately said nothing about the screen. These three claims
// are the screen, and each one is against a state that was measured before it
// was fixed:
//
// * A read that timed out left the panel silent. The diagnostics count did not
//   go stale, it *vanished* -- and a tab with no badge is how this panel says
//   "nothing to report". Generate stayed offered, one click from a refusal.
//   Nothing said the engine was gone.
// * `Retry`, the only button beside the error, was `ProductStore.reload()`: a
//   re-read through the engine that was not there. One press turned a product
//   with a failed resolve into an empty panel -- name, profile switcher,
//   composition and pending-changes line all gone.
// * A write whose answer never came left **no trace at all** on the panel. The
//   file on disk had the change, the composition showed the text from before it,
//   the draft went on offering to make it, and the only report was a toast.
//
// The seam is `scripts/wedging-engine.mjs`, which forwards the request it
// withholds the answer to. That is the whole reason the second scenario is
// reachable: a timeout is not evidence that the operation did not happen, and a
// stub that swallowed the request could only ever produce the case where it
// did not.

import { readFileSync } from "node:fs";
import { join } from "node:path";

import { copyProduct, type ProductCopy } from "../fixtures/product-copy";
import {
  configureGear,
  expect,
  openProductById,
  productSection,
  runCommand,
  settled,
  test,
} from "../fixtures/studio";
import { HELD_WRITE, engines, hold, logMark, release, requestsTo, since } from "./seam";

const REPO = join(__dirname, "../../..");

test.describe("recovery after the engine stops", () => {
  let product: ProductCopy;
  /** A second product, for the claim about switching during a recovery. */
  let other: ProductCopy;

  test.beforeAll(() => {
    product = copyProduct(REPO, "configurable-gears", "recovery");
    other = copyProduct(REPO, "payments-demo", "recovery-other");
  });

  test.afterAll(() => {
    product?.dispose();
    other?.dispose();
    release();
  });

  test("a read that timed out: Reconnect brings the session back with the product, profile, selection and draft intact", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);
    await openProductById(page, product.id, "dev");

    // Three things to lose, established before anything breaks: a profile that
    // is not the description's default, a selection, and an unapplied draft.
    const form = await configureGear(page, "api-gateway");
    await form.locator('[data-config-field="bind_addr"] input').fill("0.0.0.0:9099");
    await page.locator('[data-composition-gear="api-gateway"]').click();
    const draft = page.locator(".gbx-composition-draft");
    await expect(draft).toContainText("1 pending change");
    await expect(page.locator(".gbx-toolbar")).toHaveAttribute("data-has-selection", "true");
    await expect(
      page.locator("[data-validation-count]"),
      "a count to go stale later",
    ).toBeVisible();

    // ---- one read held past the cap

    hold();
    await page.locator("[data-product-profile]").selectOption("prod");
    const trouble = page.locator("[data-product-error]");
    await expect(trouble).toBeVisible({ timeout: 40_000 });

    // **The panel says the screen is not current, in all four places.** Each of
    // these was measured saying nothing: the badge disappeared rather than going
    // unknown, and Generate stayed enabled.
    await expect(trouble).toHaveAttribute("data-product-stale", "true");
    await expect(
      trouble,
      "a read that timed out says nothing about a write",
    ).toHaveAttribute("data-write-unknown", "false");
    await expect(
      page.locator("[data-validation-stale]"),
      "an unknown count, not an absent one: a tab with no badge reads as `nothing to report`",
    ).toBeVisible();
    await expect(page.locator("[data-validation-count]")).toHaveCount(0);
    await expect(page.locator("[data-product-section-generate]")).toBeDisabled();

    // And the offer is to re-establish the session, not to re-read through an
    // engine that is not there.
    await expect(page.locator("[data-reconnect-engine]")).toBeVisible();
    await expect(
      page.locator("[data-retry-read]"),
      "Retry is for a description that refused, and this is not one",
    ).toHaveCount(0);

    // ---- Reconnect, and what comes back with it

    release();
    const before = engines().length;
    await page.locator("[data-reconnect-engine]").click();
    // A new engine, which is what makes this a reconnect rather than a re-render.
    await expect.poll(() => engines().length, { timeout: 90_000 }).toBeGreaterThan(before);
    await expect(page.locator("[data-product-error]")).toHaveCount(0, { timeout: 90_000 });

    await expect(page.locator("[data-product-name]")).toHaveAttribute(
      "data-product-name",
      product.id,
    );
    // **The profile being viewed, not the description's default.** Re-reading
    // always answered with `default_profile`, so recovering while looking at
    // `prod` put the panel silently back on `dev` -- with the switcher agreeing,
    // which makes it a wrong answer rather than a lost preference.
    await expect(page.locator("[data-product-profile]" )).toHaveValue("prod");
    await expect(page.locator(".gbx-toolbar")).toHaveAttribute("data-has-selection", "true");
    await expect(draft, "the draft is the person's, not the session's").toContainText(
      "1 pending change",
    );
    // Current again, in the same four places.
    await expect(page.locator("[data-validation-stale]")).toHaveCount(0);
    await expect(page.locator("[data-validation-count]")).toBeVisible();
    await expect(page.locator("[data-product-section-generate]")).toBeEnabled();
  });

  test("a write the engine already applied: the description is re-read, the write is not re-sent, and the draft cannot be applied over it", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    // The run's log is shared by every claim here; this claim's counts start now.
    const mark = logMark();
    await settled(page);
    await openProductById(page, product.id, "dev");
    const form = await configureGear(page, "api-gateway");
    await form.locator('[data-config-field="bind_addr"] input').fill("0.0.0.0:9191");
    await page.locator('[data-composition-gear="api-gateway"]').click();
    await expect(page.locator(".gbx-composition-draft")).toContainText("1 pending change");

    // ---- the commit alone, not the dry run it was previewed from

    // A write goes out twice; withholding the method would hold the preview,
    // which is a different scenario with a different screen. `"dry_run":false`
    // names the commit.
    hold(HELD_WRITE);
    await page.locator("[data-draft-apply]").click();
    const dialog = page.locator(".dialogBlock");
    await dialog.waitFor({ state: "visible", timeout: 60_000 });
    await dialog.locator("button.theia-button.main").click();

    const trouble = page.locator("[data-product-error]");
    await expect(trouble).toBeVisible({ timeout: 60_000 });

    // **The engine did the work.** This is the fact the whole scenario rests on,
    // and it is read off the disk rather than inferred: the answer was withheld,
    // not the request.
    await expect
      .poll(() => readFileSync(product.path, "utf8").includes("0.0.0.0:9191"), { timeout: 30_000 })
      .toBe(true);

    // The panel says so, in the words that matter: not "this failed" but "this
    // may already be saved".
    await expect(trouble).toHaveAttribute("data-write-unknown", "true");
    await expect(trouble).toContainText("may already be saved");
    await expect(
      page.locator("[data-draft-unverified]"),
      "`pending` is a claim, and after this it is the wrong one",
    ).toBeVisible();
    await expect(page.locator("[data-product-section-generate]")).toBeDisabled();

    // **And the header leads with the loss.** `resolved` is about the last
    // resolution, which is still a real answer -- so it is kept, because the
    // graph is drawn from it -- but it is no longer the headline. Marking a row
    // still *labelled* `resolved` was the first attempt and was not enough: the
    // label is what a person reads first, and "resolved · not re-read since the
    // engine stopped" leads with the reassurance and qualifies it afterwards.
    await productSection(page, "overview");
    const header = page.locator("[data-header-stale]");
    await expect(header).toBeVisible();
    await expect(header.locator("span").first()).toHaveText("connection lost");
    const resolved = page.locator("[data-resolved-profile]");
    await expect(resolved).toHaveAttribute("data-resolved-stale", "true");
    await expect(resolved.locator("[data-stale-headline]")).toHaveText("results are stale");
    // The order, not merely the presence: the staleness comes first and the past
    // resolution reads as the footnote it now is.
    expect((await resolved.innerText()).trim()).toMatch(/^results are stale\b/);
    await expect(resolved).toContainText("last resolved for dev");
    await productSection(page, "composition");

    // Nothing re-sent it in the meantime, and nothing will.
    const engine = engines().at(-1)!;
    const sentBefore = requestsTo(engine.pid, "gearbox/product/applyEdits");
    expect(
      since(mark).filter((r) => r.kind === "withhold").length,
      "one withheld write, not a retry loop",
    ).toBe(1);

    // ---- recovery re-reads, and the draft meets what is actually there

    release();
    await page.locator("[data-reconnect-engine]").click();
    await expect(page.locator("[data-product-error]")).toHaveCount(0, { timeout: 90_000 });
    const fresh = engines().at(-1)!;
    expect(fresh.pid, "a new engine answered the re-read").not.toBe(engine.pid);
    expect(
      requestsTo(fresh.pid, "gearbox/product/applyEdits"),
      "recovery re-reads; it does not re-send a write whose outcome is unknown",
    ).toBe(0);
    expect(requestsTo(engine.pid, "gearbox/product/applyEdits")).toBe(sentBefore);

    // The composition now shows what is on disk, which is the applied change --
    // so the draft is a change the description already contains.
    await expect(
      (await configureGear(page, "api-gateway")).locator('[data-config-field="bind_addr"] input'),
    ).toHaveValue("0.0.0.0:9191");

    // **Applying it writes nothing and ends it.** The engine answers the dry run
    // with `changed: false`; that used to be reported as a failure, so the draft
    // stayed pending for ever with Discard as the only way out -- for an edit
    // that had been saved. No confirmation appears, because there is nothing to
    // confirm.
    await page.locator("[data-draft-apply]").click();
    await expect(page.locator(".gbx-composition-draft")).toContainText("Saved", {
      timeout: 60_000,
    });
    await expect(page.locator("[data-draft-apply]")).toHaveCount(0);
    expect(
      requestsTo(fresh.pid, "gearbox/product/applyEdits"),
      "one dry run to ask, and no commit: the description already said it",
    ).toBe(1);
  });

  test("a product opened during a recovery wins, and the late answer for the other one is dropped", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await settled(page);
    await openProductById(page, product.id, "dev");

    hold();
    await page.locator("[data-product-profile]").selectOption("prod");
    await expect(page.locator("[data-product-error]")).toBeVisible({ timeout: 40_000 });
    release();

    // **Started and not waited for.** A recovery is the whole open sequence --
    // two engine spawns, a catalogue load and a re-read -- and the thing a person
    // is most likely to do while waiting on a broken product is go and open a
    // different one.
    await page.locator("[data-reconnect-engine]").click();

    // The picker rather than `openProductById`, which would close the open
    // product first: closing is a different act, and the state under test is a
    // switch landing *into* a recovery.
    await runCommand(page, "Open Product…");
    const options = page.locator(`.quick-input-list [role="option"]`);
    await options.first().waitFor({ state: "visible", timeout: 30_000 });
    await options.filter({ hasText: other.id }).first().click();

    // **The switch wins, and this is the half that failed without the guard.**
    // An open was non-reentrant by returning the in-flight promise to every
    // caller -- correct for two opens of the same product, and for a different
    // one it meant the switch silently did nothing and answered with the other
    // product's result.
    await expect(page.locator("[data-product-name]")).toHaveAttribute(
      "data-product-name",
      other.id,
      { timeout: 120_000 },
    );

    // And it stays won. The abandoned recovery is still running when the switch
    // lands; its answer arrives afterwards and must be dropped rather than
    // installed. Two guards say so independently -- the session's generation and
    // the store's epoch -- and this is the observable both of them exist for.
    await page.waitForTimeout(15_000);
    await expect(page.locator("[data-product-name]")).toHaveAttribute(
      "data-product-name",
      other.id,
    );
    await expect(page.locator('[data-product-name="' + product.id + '"]')).toHaveCount(0);
  });
});
