// The workspace, and the Git that depends on it.
//
// ADR 0011 says Explorer, Terminal and Git remain visible. Two things had to be
// true before Git could be anything but an empty panel, and neither was:
//
//   - a workspace has to be open, because the VS Code git extension finds
//     repositories by walking workspace folders;
//   - it has to be **multi-root**, because the two repositories that matter are
//     siblings -- `gearbox` and `gears-rust` -- so no single folder
//     contains both.
//
// The second is the reason this is not just "open a folder". A single-folder
// workspace would have covered half the tree a person edits and looked like it
// worked.

import { readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { expect, revealInExplorer, revealLeft, test } from "../fixtures/studio";

const IDE = join(__dirname, "../..");

/**
 * A file that makes the repository dirty, for as long as the claim needs it.
 *
 * **The two Git claims below used to depend on the developer's working tree.**
 * They asserted "the badge is greater than zero" and "some node carries a letter",
 * which is true whenever someone has uncommitted work and false the moment they
 * commit -- so the suite went red on a clean checkout and green again after any
 * edit. A conformance run that reports the state of somebody's editor is not
 * reporting on the application.
 *
 * So the claim makes its own change: one untracked file, asserted on, then removed.
 * That is also a stronger statement than the old one -- the provider is not merely
 * non-zero, it *noticed something appear* -- and it holds on a clean checkout,
 * which is where a conformance suite has to hold.
 *
 * Outside `products/`, because the three guards on the descriptions exist to catch
 * exactly this kind of write, and rightly.
 */
function withDirtyRepo<T>(repo: string, run: (marker: string) => Promise<T>): Promise<T> {
  const name = `.scm-probe-${process.pid}`;
  const file = join(repo, "ide", name);
  writeFileSync(file, "A file one conformance claim makes and removes. Safe to delete.\n");
  return run(name).finally(() => rmSync(file, { force: true }));
}

test.describe("the workspace Studio opens for itself", () => {
  test("both repositories are workspace roots [ADR-0011 §Consequences: Explorer remains]", async ({
    studio,
  }) => {
    await revealLeft(studio.page, "Explorer");
    const roots = await studio.page.evaluate(() =>
      Array.from(document.querySelectorAll(".theia-TreeNode"))
        .filter((node) => node.className.includes("theia-DirNode"))
        .map((node) => (node.textContent ?? "").trim()),
    );
    // The multi-root claim, stated as the two names: `gears-rust` is a sibling of
    // the builder repository, so its presence is the whole reason the workspace
    // is multi-root rather than one folder.
    expect(roots).toContain("gearbox");
    expect(roots).toContain("gears-rust");
  });

  test("the watcher does not walk the Rust target directories [ADR-0011 §Scope: which packages are present]", () => {
    // Not a UI claim, and it is here because the failure is invisible until it is
    // not: opening two Rust checkouts as workspace roots points Theia's file
    // watcher at two multi-gigabyte `target/` trees. `.gearbox/` is generated
    // wholesale, so watching it reports churn nobody acts on.
    const app = JSON.parse(readFileSync(join(IDE, "browser-app/package.json"), "utf8")) as {
      theia: { frontend: { config: { preferences: Record<string, unknown> } } };
    };
    const exclude = app.theia.frontend.config.preferences["files.watcherExclude"] as
      | Record<string, boolean>
      | undefined;
    expect(exclude?.["**/target/**"]).toBe(true);
    expect(exclude?.["**/.gearbox/**"]).toBe(true);
  });
});

test.describe("Git, from the VS Code extension", () => {
  test("Source Control lists both repositories [ADR-0011 §Consequences: Git remains]", async ({
    studio,
  }) => {
    // Theia 1.75 ships no `@theia/git` -- its last release was `1.61.0-next.8`.
    // This provider is `vscode.git` running in the plugin host, which is what the
    // earlier decision to install `@theia/plugin-ext` bought.
    await revealLeft(studio.page, /^Source Control/);
    const repos = await studio.page
      .locator(".theia-scm-repository-name")
      .allTextContents()
      .then((names) => names.map((name) => name.trim()));
    expect(repos).toContain("gearbox");
    expect(repos).toContain("gears-rust");
  });

  test("the change count is the repository's, not a placeholder [ADR-0011 §Consequences: Git remains]", async ({
    studio,
  }) => {
    // A provider that registered but read nothing would still render a name and
    // an empty change list. Writing a probe and seeing it appear under the
    // builder root is what tells "wired up" from "present".
    //
    // Assert the change *list*, not the activity-bar badge. The badge aggregates
    // whatever repository the multi-root SCM view last selected; earlier claims
    // in the suite leave that selection elsewhere, so a poll on the badge times
    // out even while the builder's changes are on screen. Selecting the builder
    // root and waiting for the probe row is the same claim, and it is stable
    // across run order.
    await revealLeft(studio.page, /^Source Control/);
    // The *row*, not the name inside it, and that is not a stylistic choice.
    // `theia-scm-repository-name` is a span with `flex: 1 1 0%` and no floor
    // beside a branch button that never shrinks, so on a branch whose name is
    // long enough the span is squeezed to zero width -- present in the DOM with
    // the right text, and unclickable. Measured at 0px on
    // `fix/cluster-generation-and-self-hosted-target`, which is how this was
    // found: the claim above passes throughout, because `allTextContents()`
    // does not care about visibility, and this one timed out for two minutes.
    // The row carries the repository as a `title`, is the thing a person clicks
    // anyway, and is 257x22 whatever the branch is called. `index.css` fixes the
    // squeeze itself; this makes the claim independent of it.
    await studio.page
      .locator('.theia-scm-repository-item[title$="/gearbox"]')
      .click();

    // The *count*, not the probe's row. The change list is virtualized, so a row
    // fifty entries down is not in the DOM at all -- and on a working tree with
    // fifty other edits, which is every tree in the middle of a change, that is
    // where the probe lands. Reading the panel's text for the marker therefore
    // failed for a reason that had nothing to do with the claim. The count is
    // rendered whatever the scroll position, and "it went up by one when a file
    // appeared" is the same statement: a provider that registered but read
    // nothing would show a number that never moves.
    const changeCount = async () => {
      const panel = await studio.page
        .locator("#theia-left-content-panel")
        .innerText()
        .catch(() => "");
      // The unstaged group's own header: `STAGED CHANGES n` is another group,
      // and a staged file in the tree would otherwise be read as this count.
      const seen = [...panel.matchAll(/(?<!STAGED\s)CHANGES\s+(\d+)/g)].at(-1);
      // No number means no changes -- which is what a clean checkout looks like,
      // and is a perfectly good baseline. Returning a sentinel here made the
      // claim fail on a tree with nothing modified.
      return seen ? Number(seen[1]) : 0;
    };

    // Deliberately no assertion on `before`. An earlier version required it to
    // be positive, as a guard against a degenerate pass -- and thereby tied the
    // claim to how many files happened to be edited, which is exactly the
    // fragility this test was rewritten to remove. The guard was never needed:
    // a provider that registered and read nothing shows a number that does not
    // move, and `before + 1` catches that whatever `before` is.
    //
    // **Read once it has settled, and compared as "went up".** Taken while the
    // provider was still refreshing after the repository was selected, `before`
    // was an intermediate number and the count then moved for reasons of its
    // own, so `before + 1` failed in full runs and passed alone. Two equal reads
    // a second apart make the baseline the repository's; "greater" is the claim
    // -- a provider that read nothing shows a number that never moves -- and
    // stays true when the tree gains another change while the probe exists.
    let before = -1;
    await expect
      .poll(
        async () => {
          const first = await changeCount();
          await studio.page.waitForTimeout(1_000);
          const second = await changeCount();
          before = second;
          return first === second;
        },
        { timeout: 30_000 },
      )
      .toBe(true);
    await withDirtyRepo(join(IDE, ".."), async () => {
      await expect.poll(changeCount, { timeout: 60_000 }).toBeGreaterThan(before);
    });
  });

  test("git decorates the Explorer [ADR-0011 §Consequences: Git remains]", async ({ studio }) => {
    // Independent of the Source Control view: the letters the extension puts on
    // changed files in the file tree. If the provider were registered but inert,
    // the tree would be undecorated.
    //
    // Expand the builder root first. After a perspective switch Theia restores a
    // snapshot that may have had the folders collapsed, and a collapsed tree
    // has no letters to find.
    await withDirtyRepo(join(IDE, ".."), async (marker) => {
      // The probe file is in `ide/`, so that is the folder to expand: a collapsed
      // tree has no letters to find, and after a perspective switch Theia may have
      // restored a snapshot with the folders closed. Expanding by naming the probe
      // file itself also waits for the tree to have noticed it.
      await revealInExplorer(studio.page, "gearbox", "ide", marker);
      await expect
        .poll(async () =>
          studio.page.evaluate(() =>
            Array.from(document.querySelectorAll(".theia-TreeNode"))
              .map((node) => (node.textContent ?? "").trim())
              .filter((text) => /[MUAD]$/.test(text)).length,
          ),
        )
        .toBeGreaterThan(0);
    });
  });
});

test.describe("what the workspace makes checkable", () => {
  test("a product.lock opened as a file is read-only [PRD cpt-gearbox-fr-lock-read-only]", async ({
    studio,
  }) => {
    // Implemented long before it could be reached: with no workspace there was no
    // way to open a `product.lock` at all, so `ReadOnlyLockEditorProvider` sat
    // unverified.
    // Every segment down to the profile is a hint, and the last one is why.
    // Theia collapses a single-child chain into one `.gearbox/payments-demo/dev`
    // row and stops collapsing the moment a sibling appears -- which is what a
    // second `gearbox generate --profile prod`, or the `.base/` cache an
    // operator-owned file creates, does to this tree. Naming `dev` walks both
    // layouts; naming only the two parents walked the collapsed one and dead-ended
    // in the expanded one.
    const lock = await revealInExplorer(
      studio.page,
      "gearbox",
      [".gearbox", "payments-demo", "dev"],
      "product.lock",
    );
    await lock.dblclick();

    // Not `.monaco-editor`.first(): Generate's preview is a diff editor that
    // stays in the DOM after ADR-0010 opens a planned file, and its gutter
    // matches first while remaining hidden.
    const editor = studio.page
      .locator(".monaco-editor:not(.gutter)")
      .locator("visible=true")
      .first();
    await editor.waitFor({ state: "visible" });
    await expect(editor.locator(".view-lines .view-line").first()).toContainText("GENERATED");

    // Asserted as behaviour, not as a CSS class: the requirement is that a person
    // cannot edit the file, and a class name is Monaco's business and free to
    // change. "An edit to the lock is silently discarded by the next resolve,
    // which is the failure mode worth preventing rather than detecting."
    // Clicked into the text and typed with the keyboard, not `press` on
    // `.inputarea`: Monaco's input is a textarea parked off-screen, so
    // Playwright's actionability check waits on it forever.
    //
    // And asserted on the refusal rather than on the text. Comparing the rendered
    // lines does not work -- Monaco virtualises them, so a click that scrolls
    // changes what is on screen without anything being edited, which is exactly
    // what the first version of this test mistook for a failure.
    await editor.locator(".view-lines").first().click();
    await studio.page.keyboard.type("xx");

    // Monaco's own overlay, carrying the message `ReadOnlyLockEditorProvider`
    // supplies. Asserting the text means asserting that *our* rebind is what
    // refused, not merely that something did.
    await expect(studio.page.locator(".monaco-editor-overlaymessage")).toContainText(
      "product.lock is generated",
    );
    // And nothing reached the document: an edited editor is marked dirty at once.
    const tab = studio.page.locator(".lm-TabBar-tab", { hasText: "product.lock" }).first();
    expect(await tab.getAttribute("class")).not.toContain("dirty");
  });
});
