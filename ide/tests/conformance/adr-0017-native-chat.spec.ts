// ADR cpt-gearbox-adr-native-chat-surface, and PRD cpt-gearbox-fr-chat-context.
//
// **What is browser-observable here, and what deliberately is not.** The claims
// this file asserts are structural: that an agent exists to answer at all, that
// the context the chat is handed follows the selection, and that the rows which
// feed it can be dragged. What a model *says* is asserted nowhere. That needs a
// key and a network, and a second run would not agree with the first -- a
// conformance suite whose result depends on a sampler has reported nothing.
//
// The claims about the tool surface's shape -- every tool returns engine output,
// none applies generation -- are enforced where they are decidable: in the
// TypeScript that declares them, checked by `tsc`, and in the generator tests
// under `crates/gearbox-ir`. Asserting "no tool can write" through a browser
// would mean asking a model to try, which is the sampler problem again.

import type { Locator, Page } from "@playwright/test";

import {
  expect,
  openConflicts,
  openProduct,
  resetCatalogueView,
  revealCatalogue,
  runCommand,
  test,
} from "../fixtures/studio";

/** The AI Chat panel, brought forward wherever the shell has it. */
async function openChat(page: Page): Promise<void> {
  const tab = page.locator(".lm-TabBar-tab", { hasText: "AI Chat" }).first();
  if ((await tab.count()) === 0) {
    await runCommand(page, "AI Chat");
  } else if (!(await tab.evaluate((e) => e.classList.contains("lm-mod-current")))) {
    await tab.click();
  }
  await page.locator(".theia-ChatInput").first().waitFor({ state: "visible", timeout: 30_000 });
}

/**
 * Drag `source` and drop it on the chat input, carrying one `DataTransfer`.
 *
 * The same object for both events on purpose: that is what a real drag does,
 * and it is the only way the payload written by `dragstart` is the payload the
 * drop handler reads.
 */
async function dropOnChat(page: Page, source: Locator): Promise<void> {
  const dataTransfer = await page.evaluateHandle(() => new DataTransfer());
  await source.dispatchEvent("dragstart", { dataTransfer });
  const target = page.locator(".theia-ChatInput").first();
  await target.dispatchEvent("dragover", { dataTransfer });
  await target.dispatchEvent("drop", { dataTransfer });
}

/** The titles of the context chips currently attached to the chat input. */
function chipTitles(page: Page) {
  return page.locator(".theia-ChatInput-ChatContext-Element .theia-ChatInput-ChatContext-title");
}

test.describe("the chat is a Studio surface", () => {
  test("the chat offers a Gearbox agent to answer with [ADR-0017 Decision Outcome]", async ({
    studio,
  }) => {
    const { page } = studio;
    await openChat(page);
    // Typing `@` opens the agent picker. Before this ADR that list was empty and
    // every request came back "No agent was found to handle this request", so
    // the presence of the entry is the whole claim.
    const input = page.locator(".theia-ChatInput .monaco-editor").first();
    await input.click();
    await page.keyboard.type("@");
    // No `settled`: it waits for the Start screen, and whether Home is showing
    // depends on what ran before in this worker. The assertion's own retry is
    // the wait, and it does not care which screen is up.
    const suggestions = page.locator(".monaco-list-row");
    await expect(suggestions.filter({ hasText: "Gearbox" }).first()).toBeVisible({
      timeout: 15_000,
    });
    await page.keyboard.press("Escape");
  });

  test("selecting a gear names it on a chip in the chat [ADR-0017 context from the services]", async ({
    studio,
  }) => {
    const { page } = studio;
    await openProduct(page, "dev");
    await openChat(page);
    await revealCatalogue(page);
    await resetCatalogueView(page);

    const rows = page.locator(".gbx-row:not(.gbx-pending)");
    await rows.first().waitFor({ state: "visible", timeout: 30_000 });
    await rows.first().click();

    // One chip, and it names the *selection* rather than the variable: a pill
    // reading "Gearbox Selection" would be true and useless. No `settled` here
    // -- that waits for the Start screen, which a product being open has
    // replaced; the assertion's own retry is the wait.
    await expect(chipTitles(page)).toHaveCount(1, { timeout: 30_000 });
    const named = (await chipTitles(page).first().textContent())?.trim() ?? "";
    expect(named).not.toBe("");
    expect(named).not.toBe("Gearbox Selection");
  });

  test("selecting another gear renames the chip rather than adding one [ADR-0017 context from the services]", async ({
    studio,
  }) => {
    const { page } = studio;
    await openProduct(page, "dev");
    await openChat(page);
    await revealCatalogue(page);
    await resetCatalogueView(page);

    const rows = page.locator(".gbx-row:not(.gbx-pending)");
    await rows.first().waitFor({ state: "visible", timeout: 30_000 });
    await rows.first().click();
    await expect(chipTitles(page)).toHaveCount(1, { timeout: 30_000 });
    const before = (await chipTitles(page).first().textContent())?.trim() ?? "";

    await rows.nth(1).click();

    // The chip follows the selection instead of pinning the first one, and
    // there is still only one. `deleteContextElement` is protected, so a chip
    // attached per selection could never be cleared again -- which is why the
    // single chip means "whatever is selected" and resolves when the request
    // is sent.
    await expect(chipTitles(page).first()).not.toHaveText(before, { timeout: 30_000 });
    await expect(chipTitles(page)).toHaveCount(1);
  });

  test("opening the chat after a selection still shows the chip [ADR-0017 context from the services]", async ({
    studio,
  }) => {
    const { page } = studio;
    await openProduct(page, "dev");
    await revealCatalogue(page);
    await resetCatalogueView(page);

    // Select *first*, open the chat *after* -- the commonest order, and the one
    // that was broken: the selection had already fired its one change event, and
    // selecting the same gear again is deduplicated, so a chip attached only on
    // change never arrived.
    const rows = page.locator(".gbx-row:not(.gbx-pending)");
    await rows.first().waitFor({ state: "visible", timeout: 30_000 });
    await rows.first().click();

    await openChat(page);
    await expect(chipTitles(page)).toHaveCount(1, { timeout: 30_000 });
  });

  test("a catalogue row dropped on the chat becomes a chip [ADR-0017 context from the services]", async ({
    studio,
  }) => {
    const { page } = studio;
    await openProduct(page, "dev");
    await openChat(page);
    await revealCatalogue(page);
    await resetCatalogueView(page);
    const row = page.locator(".gbx-row:not(.gbx-pending)").first();
    await row.waitFor({ state: "visible", timeout: 30_000 });
    // A projected row carries a `GearId` and is draggable by its id chip -- not
    // the whole row, whose text has to stay selectable; a pending one has none
    // yet (ADR-0009), so there is nothing to drag it as.
    await expect(row.locator(".gbx-drag-handle")).toHaveAttribute("draggable", "true");

    // **The drop is performed, not inferred from the attribute.** A row that
    // says `draggable` and a drop handler that recognises it are two separate
    // facts, and only the pair is the claim -- the payload's MIME type and the
    // handler's have to agree, which an attribute assertion cannot see.
    // Establish a chip for a *different* row first, so the assertion below
    // cannot be satisfied by a chip that was already there: this test ran green
    // in 185ms before that precaution, which is not long enough to have proved
    // anything.
    await row.click();
    await expect(chipTitles(page)).toHaveCount(1, { timeout: 30_000 });
    const before = (await chipTitles(page).first().textContent())?.trim() ?? "";

    const other = page.locator(".gbx-row:not(.gbx-pending)").nth(1);
    await dropOnChat(page, other.locator(".gbx-drag-handle"));
    // The drop selects what was dropped, so the chip must now name the other
    // row. Still one chip, because the selection chip is single by design.
    await expect(chipTitles(page).first()).not.toHaveText(before, { timeout: 30_000 });
    await expect(chipTitles(page)).toHaveCount(1);
  });

  test("a conflict row dropped on the chat becomes a chip [ADR-0017 context from the services]", async ({
    studio,
  }) => {
    const { page } = studio;
    await openProduct(page, "dev");
    await openChat(page);
    await openConflicts(page);
    const row = page.locator("li[data-conflict-code]").first();
    await row.waitFor({ state: "visible", timeout: 30_000 });
    // By the code chip, so the message and help stay selectable.
    const handle = row.locator(".gbx-drag-handle");
    await expect(handle).toHaveAttribute("draggable", "true");

    await dropOnChat(page, handle);
    // Named, not counted. A dropped diagnostic attaches the *diagnostics*
    // variable, whose chip the label provider titles "Gearbox Diagnostics" --
    // asserting a non-zero chip count would pass on the selection chip alone
    // and prove nothing about the drop.
    await expect(
      chipTitles(page).filter({ hasText: "Gearbox Diagnostics" }),
    ).toHaveCount(1, { timeout: 30_000 });
  });
});
