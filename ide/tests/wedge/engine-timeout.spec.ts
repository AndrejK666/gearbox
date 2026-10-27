// A real deadline, missed, against a session that was working.
//
// **Why this is not in `tests/conformance/`.** It is not a claim about a design
// document; it is the seam the claims about recovery will be driven through, and
// it needs a backend nobody else is using -- see `playwright.wedge.config.ts`.
// What it proves is the mechanism: that the cap comes from the environment and
// fires, that missing it ends the engine process rather than merely abandoning
// the request, that every later call then refuses, that `initialize` is what
// makes them work again, and that nothing re-sends the operation that was
// abandoned. The **person's** way back -- a Reconnect engine action, the stale
// results ceasing to claim they are current -- is the next step, and this is
// what it will be verified against.
//
// Everything asserted here was, until this file existed, reasoning about a
// constant. The 60s cap is the reason: no claim can sit through one, so nothing
// downstream of the timeout had ever been reached by a test. One sentence was
// already wrong when this first ran -- the message naming the missed deadline was
// being overwritten by its own cleanup (`gearbox-engine-process.ts`), which is
// what a reader saw instead of the deadline for as long as the code existed.

import { join } from "node:path";

import { copyProduct, type ProductCopy } from "../fixtures/product-copy";
import { expect, openProductById, runCommand, settled, test } from "../fixtures/studio";
import {
  HELD_METHOD,
  WEDGE_TIMEOUT_MS,
  alive,
  engines,
  hold,
  logMark,
  release,
  requestsTo,
  since,
} from "./seam";

const REPO = join(__dirname, "../../..");

test.describe("an engine that misses its deadline", () => {
  let product: ProductCopy;

  // **Its own product, under its own id.** The claim's engine dies mid-resolve;
  // a description shared with the conformance suite would make that this
  // claim's business and also everybody else's. `.gearbox/` is keyed on the
  // declared id, so the copy is renamed -- see `fixtures/product-copy.ts`.
  test.beforeAll(() => {
    product = copyProduct(REPO, "configurable-gears", "engine-timeout");
  });

  test.afterAll(() => {
    product?.dispose();
    // Held state is global to the server, so leaving it set would withhold the
    // first answer of whatever ran next.
    release();
  });

  test("a missed deadline ends the engine, and `initialize` is the way back", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    // **From here, not from the start of the run.** One server serves every
    // claim in this project, so the log is the run's and the baseline is this
    // claim's. Counted absolutely, "nothing was withheld on the way in" passed
    // until a second claim existed to withhold something.
    const mark = logMark();
    const held = () => since(mark).filter((record) => record.kind === "withhold");

    // **Waited for, because `freshStudio` does not.** Only the worker-scoped
    // fixture boots and settles; a test-scoped one hands over a page that has
    // merely navigated, and driving the palette during the preload is how this
    // first ran -- refused by the dialog guard, which counts a `.dialogBlock`
    // that the shell has not finished assembling.
    await settled(page);

    // ---- the proxy passes a working session through

    // Stated as an assertion rather than assumed: a wedge that withheld
    // everything could only ever show the *first* call failing, and every
    // consequence below is a consequence for a session that had been working.
    await openProductById(page, product.id, "dev");
    // **The current engine, not the first.** Opening a product spawns three in
    // succession -- the boot catalogue, the no-roots step that lets a
    // description be evaluated, then the product's own session -- because
    // `initialize` is what sets the engine's roots and there is no other way to
    // change them (`CatalogueStore.load`). Measured here rather than assumed:
    // reading the first `start` line asked the boot engine whether it had
    // resolved anything, and it had not.
    const engine = engines().at(-1);
    expect(engine, "the backend spawned an engine through the proxy").toBeDefined();
    expect(
      requestsTo(engine!.pid, HELD_METHOD),
      "opening the product resolved it, through the proxy, for real",
    ).toBeGreaterThan(0);
    expect(held(), "and nothing was withheld on the way in").toHaveLength(0);
    await expect(page.locator("[data-product-error]")).toHaveCount(0);

    // ---- one operation held past the cap

    hold();
    // A profile switch, because it is one click and it is a `resolve`: the
    // engine does the work and is never allowed to report it, which is the state
    // a timeout is actually about. An operation the engine never *received*
    // would be a different bug with the same screen.
    await page.locator("[data-product-profile]").selectOption("prod");
    await expect(page.locator("[data-product-resolving]")).toBeVisible({ timeout: 30_000 });
    // Polled, not read: the record is written when the request reaches the
    // proxy, which is after the screen says it is working.
    await expect
      .poll(() => held().length, { timeout: 30_000 })
      .toBeGreaterThan(0);

    const failure = page.locator("[data-product-error]");
    // Longer than the cap, and not much: the wait itself is the measurement.
    await expect(failure).toBeVisible({ timeout: WEDGE_TIMEOUT_MS + 20_000 });
    // **The cap came from the environment, and the sentence says so.** 8000ms is
    // not the 60s default, so this line is also the claim that
    // `GEARBOX_PRODUCT_TIMEOUT_MS` was read -- and it is the sentence a person
    // reads, which is worth asserting on after finding it was the wrong one.
    await expect(failure).toContainText(`did not answer`);
    await expect(failure).toContainText(`${WEDGE_TIMEOUT_MS}ms`);

    // ---- the old process ended

    // Two answers, deliberately: the proxy's account of itself, and the
    // machine's. A log line saying a child exited is a claim made by the process
    // whose death is in question.
    await expect
      .poll(() => since(mark).some((record) => record.kind === "engine-exit"), { timeout: 20_000 })
      .toBe(true);
    await expect
      .poll(() => alive(engine!.pid) || alive(engine!.enginePid), { timeout: 20_000 })
      .toBe(false);

    // ---- and every later call refuses

    const before = requestsTo(engine!.pid, HELD_METHOD);
    // **Back to `dev`, and it has to be a different profile.** `setProfile`
    // returns early when the value has not changed, so clicking `prod` a second
    // time asks the backend nothing at all -- correct behaviour, and it left this
    // step reading the previous error for thirty seconds.
    await page.locator("[data-product-profile]").selectOption("dev");
    // The engine's disposal is sticky, so this is refused by the backend without
    // an engine to refuse it -- which is why the count below does not move. The
    // header, meanwhile, still says the product resolved; that staleness is the
    // recovery step's subject, not this one's.
    await expect(failure).toContainText("the engine is not initialized", { timeout: 30_000 });
    expect(
      requestsTo(engine!.pid, HELD_METHOD),
      "a refused call is not a call somebody's engine received",
    ).toBe(before);

    // ---- `initialize` is what makes them work again

    release();
    // The only thing in the application today that re-initializes. A person's
    // way back from this screen is the Retry button beside the error, which
    // re-reads the product against the engine that is not there -- the gap step 8
    // closes with a Reconnect engine action.
    const spawnedSoFar = engines().length;
    await runCommand(page, "Gearbox: Reload Catalogue");
    await expect.poll(() => engines().length, { timeout: 60_000 }).toBe(spawnedSoFar + 1);
    const fresh = engines()[spawnedSoFar];
    expect(alive(fresh.pid), "the replacement engine is running").toBe(true);

    // **Nothing replayed it.** A timed-out operation's outcome is unknown -- the
    // engine did the work and the answer was thrown away -- so re-sending it is
    // not a decision the supervisor gets to make on somebody's behalf. Measured
    // rather than argued: the new process was asked for no resolve at all.
    expect(
      requestsTo(fresh.pid, HELD_METHOD),
      "the abandoned operation was not replayed against the new engine",
    ).toBe(0);

    // And the session works, which is what makes the refusals above a state
    // rather than the end of the session.
    await page.locator("[data-product-profile]").selectOption("prod");
    await expect(page.locator('[data-resolved-profile="prod"]')).toBeVisible({ timeout: 60_000 });
    await expect(page.locator("[data-product-error]")).toHaveCount(0);
    expect(
      requestsTo(fresh.pid, HELD_METHOD),
      "and that resolve is the one asked for here",
    ).toBe(1);
  });
});
