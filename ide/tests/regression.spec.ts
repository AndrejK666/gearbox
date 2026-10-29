// Checks that are about the implementation rather than about a document.
//
// They came from `scripts/ui-smoke.mjs` and are worth keeping, but they do not
// belong in the conformance table: no design document says "an edge must have an
// arrowhead". Kept in their own file so that the conformance files stay a
// one-to-one map onto the documents.

import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { restoreProducts } from "./fixtures/products-tree";

import {
  configureGear,
  expect,
  openGenerate,
  openGraph,
  openPalette,
  openProduct,
  openProductById,
  productSection,
  revealCatalogue,
  revealInspector,
  runCommand,
  settled,
  expectContext,
  test,
} from "./fixtures/studio";

/** The repository root: this file sits in `ide/tests`. */
const REPO = join(__dirname, "../..");

test.describe("the panel is operable without a mouse", () => {
  // Every interaction in the catalogue was a bare `onClick` on a div once. A
  // panel in an IDE that only answers clicks is unusable for anyone who does not
  // use a mouse. Real key events through the browser, not synthesized ones:
  // React attaches its own listeners, and a dispatched event with the wrong
  // shape would pass here while a keyboard would not.

  test("a catalogue row can take focus", async ({ studio }) => {
    // Revealed first. The Product context collapses the left panel -- the catalogue
    // is a source of components, not the subject of that context -- so its rows are
    // not on screen until asked for. Before the rework the catalogue was always
    // there, and this test relied on that without saying so.
    await revealCatalogue(studio.page);
    await studio.page.locator(".gbx-widget-catalogue .gbx-row").first().focus();
    const focused = await studio.page.evaluate(() =>
      document.activeElement?.classList.contains("gbx-row"),
    );
    expect(focused).toBe(true);
  });

  test("Enter selects the focused row", async ({ studio }) => {
    // Deliberately a row that is not already selected: Enter on an
    // already-selected row *reveals* its description instead, which moves focus
    // into the editor. Establishing the precondition here rather than relying on
    // what an earlier test left behind, since the read-only tests share one
    // loaded application.
    await studio.page.locator(".gbx-widget-catalogue .gbx-row:not(.gbx-selected)").first().focus();
    await studio.page.keyboard.press("Enter");
    const name = await studio.page.evaluate(() =>
      document.activeElement?.classList.contains("gbx-selected") === true
        ? (document.activeElement.querySelector(".gbx-row-name")?.textContent ?? "").trim()
        : null,
    );
    expect(name, "the focused row never became selected").not.toBeNull();
    expect(name!.length).toBeGreaterThan(0);
  });

  test("one row at a time is in the tab order", async ({ studio }) => {
    // `tabIndex={0}` on every row put one tab stop per gear between the filter
    // box and the rest of the shell -- 62 of them today. The ARIA listbox
    // pattern is the fix and it is the reason the arrow keys below exist: Tab
    // reaches the list, the arrows move inside it.
    const tabbable = await studio.page.evaluate(
      () =>
        Array.from(document.querySelectorAll('.gbx-widget-catalogue [role="treeitem"]')).filter(
          (row) => row.getAttribute("tabindex") === "0",
        ).length,
    );
    expect(tabbable).toBe(1);
  });

  test("the arrow keys move through catalogue branches and rows", async ({ studio }) => {
    const tree = studio.page.locator('.gbx-widget-catalogue [role="tree"]');
    const items = tree.locator('[role="treeitem"]');
    await items.first().focus();
    const current = () => studio.page.evaluate(() => document.activeElement?.getAttribute("data-occurrence-key"));
    const first = await current();
    await studio.page.keyboard.press("ArrowDown");
    expect(await current()).not.toBe(first);
    await studio.page.keyboard.press("ArrowUp");
    expect(await current()).toBe(first);
    await studio.page.keyboard.press("ArrowUp");
    expect(await current()).toBe(first);
    await studio.page.keyboard.press("End");
    expect(await current()).not.toBe(first);
    await studio.page.keyboard.press("Home");
    expect(await current()).toBe(first);
  });

  test("the command palette opens", async ({ studio }) => {
    // Opened and dismissed without running anything: every Gearbox view command
    // is a toggle, so executing one here would change the state the other tests
    // read. Its own test so that a keybinding regression is not reported as a
    // graph regression.
    await openPalette(studio.page);
    await expect(studio.page.locator(".quick-input-widget")).toBeVisible();
    await studio.page.keyboard.press("Escape");
  });
});

test.describe("the graph draws what it says it draws", () => {
  test("every edge is directed", async ({ studio }) => {
    await openGraph(studio.page);
    const counts = await studio.page.evaluate(() => ({
      edges: document.querySelectorAll(".gbx-edge").length,
      arrows: document.querySelectorAll(".gbx-edge[marker-end]").length,
    }));
    expect(counts.edges).toBeGreaterThan(0);
    expect(counts.arrows).toBe(counts.edges);
  });

  test("every edge lands on a node the graph drew", async ({ studio }) => {
    await openGraph(studio.page);
    const graph = await studio.page.evaluate(() => ({
      nodes: Array.from(document.querySelectorAll(".gbx-node-label")).map((e) =>
        (e.textContent ?? "").trim(),
      ),
      edges: Array.from(document.querySelectorAll(".gbx-edge")).map((e) => [
        e.getAttribute("data-from"),
        e.getAttribute("data-to"),
      ]),
    }));
    // An edge to a gear outside the catalogue would be a projection bug.
    const dangling = graph.edges.filter(
      ([from, to]) => !graph.nodes.includes(from ?? "") || !graph.nodes.includes(to ?? ""),
    );
    expect(dangling).toEqual([]);
  });
});

test.describe("contributions are registered once", () => {
  // Both of these catch the same class of mistake from two sides, and it is a
  // mistake this application made: `bindViewContribution` already binds
  // `CommandContribution`, `KeybindingContribution` and `MenuContribution`, and
  // binding any of them again next to it runs the contribution twice.

  test("no menu contains the same entry twice", async ({ studio }) => {
    // Stated as an invariant over every menu rather than as a list of expected
    // entries. A test naming the entries would have to be updated by the same
    // person who added the duplicate, and `MenuModelRegistry.registerMenuAction`
    // does not deduplicate -- it makes a node and appends it, so a double
    // binding shows up here first.
    const menus = await studio.page
      .locator(".lm-MenuBar-itemLabel")
      .allTextContents()
      .then((all) => all.map((one) => one.trim()).filter((one) => one.length > 0));
    expect(menus.length).toBeGreaterThan(0);

    for (const menu of menus) {
      await studio.page.click(`.lm-MenuBar-itemLabel:text-is("${menu}")`);
      const items = await studio.page
        .locator(".lm-Menu-itemLabel")
        .allTextContents()
        .then((all) => all.map((one) => one.trim()).filter((one) => one.length > 0));
      await studio.page.keyboard.press("Escape");

      // `indexOf`, not `Set.add`. The first version of this line was
      // `items.filter((item) => !seen.add(item))`, and `Set.prototype.add`
      // returns the *set*, which is always truthy -- so the filter never matched
      // and the test passed against a menu with `Product` in it twice. Verified
      // the other way round this time: the bug was reintroduced and this failed.
      const duplicated = items.filter((item, at) => items.indexOf(item) !== at);
      expect(duplicated, `${menu} lists these twice`).toEqual([]);
    }
  });

  test("nothing is registered twice", async ({ studio }) => {
    // The command side of the same bug, and the quieter side:
    // `CommandRegistry.registerCommand` on an existing id warns and returns a
    // no-op disposable, so the *second* handler is silently discarded. Six of
    // these sat in the console unnoticed -- the fixture collects warnings and
    // only the grammar check ever read them.
    //
    // Narrow on purpose. Theia warns legitimately about slow startup steps and
    // about plugin candidates it cannot unpack, so "no warnings at all" would
    // fail every run and teach everyone to ignore it.
    const doubled = studio.consoleWarnings.filter((w) => /is already registered/.test(w));
    expect(doubled, `duplicate registrations:\n${doubled.join("\n")}`).toEqual([]);
  });
});

test.describe("nothing failed quietly", () => {
  test("the Fabric favicon is installed", async ({ studio }) => {
    // `@theia/cli` 1.75 has no favicon hook; FabricThemeContribution injects one.
    const href = await studio.page.evaluate(() => {
      const link = document.querySelector<HTMLLinkElement>("link[rel='icon']");
      return link?.href ?? null;
    });
    expect(href, "no link[rel=icon] in document.head").not.toBeNull();
    expect(href!).toMatch(/^(data:image\/svg\+xml|blob:|https?:)/);
  });

  test("no console errors", async ({ studio }) => {
    // Tolerating by name rather than by pattern: a real missing resource or a
    // real exception still fails.
    //
    // `INVALID tab` is thrown by `@theia/plugin-ext`'s own tab bookkeeping
    // (`src/plugin/tabs.ts`) when a tab update arrives for an id the extension
    // host has not recorded. It is a race in the plugin host, not in this
    // application: it appeared the day the plugin host did, it fires while the
    // suite opens and switches editors quickly, and nothing observable breaks --
    // git, the editors and every other claim keep passing. Recorded rather than
    // filtered away quietly, because it is part of what the plugin host costs.
    //
    // `[DEP0205]` is Node 26's deprecation of `module.register()`, printed on the
    // plugin host's stderr, which Theia forwards to the console as ERROR. Seen
    // the first run after moving to Node 26: once per plugin-host start, and
    // nothing else changed -- all 207 claims passed on the same run. Which module
    // in the plugin host calls it was *not* established: neither Theia's own
    // plugin-ext sources nor the bundled plugin host name it plainly. A
    // deprecation, not a failure; matched by its code, so any other message on
    // that stream still fails this test.
    const tolerated = [
      /^pageerror: INVALID tab$/,
      /plugin-host ERROR \(node:\d+\) \[DEP0205\] DeprecationWarning: `module\.register\(\)` is deprecated/,
    ];
    const real = studio.consoleErrors.filter(
      (error) => !tolerated.some((pattern) => pattern.test(error)),
    );
    expect(real, `unexpected console output:\n${real.join("\n")}`).toEqual([]);
  });

  test("the light Fabric theme is the active color theme", async ({ studio }) => {
    const theme = await studio.page.evaluate(() => {
      const bg = getComputedStyle(document.documentElement)
        .getPropertyValue("--theia-editor-background")
        .trim()
        .toLowerCase();
      const fg = getComputedStyle(document.documentElement)
        .getPropertyValue("--theia-editor-foreground")
        .trim()
        .toLowerCase();
      const bodyClass = document.body.className;
      return { bg, fg, bodyClass };
    });
    expect(theme.bg).toMatch(/#ffffff|#fff\b|rgb\(\s*255,\s*255,\s*255\s*\)/);
    // navy-deep from constructorfabric.org styles.css, now carrying the text
    // rather than the background -- which is what makes this the *Fabric* light
    // theme and not Theia's stock one.
    expect(theme.fg).toMatch(/#001838|rgb\(\s*0,\s*24,\s*56\s*\)/);
    // **Both halves, because `"vs-dark"` contains `"vs"`.** A `toContain("vs")`
    // alone passes under the dark theme too, so it would assert nothing.
    expect(theme.bodyClass).toContain("vs");
    expect(theme.bodyClass).not.toContain("vs-dark");
  });

  test("the dark Fabric theme is still there to switch back to", async ({ studio }) => {
    // The light default is a default, not a removal: the brand's dark theme is
    // the one this shell shipped with, and somebody who wants it back must be
    // able to pick it. Nothing asserted this while there was only one theme.
    //
    // Through the picker a person actually uses, rather than through
    // `ThemeService`: the registry is reachable only from inside the bundle,
    // and a theme that is registered but absent from this list is not
    // selectable, which is the thing being claimed.
    const { page } = studio;
    await runCommand(page, "Color Theme");
    const options = page.locator(`.quick-input-list [role="option"]`);
    await options.first().waitFor({ state: "visible", timeout: 30_000 });
    const labels = await options.evaluateAll((nodes) =>
      nodes.map((node) => (node.textContent ?? "").trim()),
    );
    await page.keyboard.press("Escape");
    await page.locator(".quick-input-widget").waitFor({ state: "hidden" });

    expect(labels.join(" | ")).toContain("Gearbox (Fabric Light)");
    expect(labels.join(" | ")).toContain("Gearbox (Fabric)");
  });
});

test.describe("every view has an icon, and the icon exists", () => {
  // The Catalogue tab in the left activity bar was blank: Theia renders only the
  // icon there, and no widget set `title.iconClass`. The other six were equally
  // iconless and nobody noticed, because a main-area or bottom tab shows its
  // label as well.
  //
  // Two checks, because there are two ways to get a blank button and only one of
  // them is "no icon". A codicon name that does not exist renders an empty box,
  // which looks exactly like the bug being fixed here -- so the name is checked
  // against the font rather than trusted.

  const STUDIO_SRC = join(__dirname, "../gearbox-studio/src/browser");
  const CODICON_CSS = join(
    __dirname,
    "../node_modules/@vscode/codicons/dist/codicon.css",
  );

  /** Every `.ts`/`.tsx` file under a directory, recursively. */
  function sources(dir: string): string[] {
    return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) return sources(path);
      return /\.tsx?$/.test(entry.name) ? [path] : [];
    });
  }

  test("every codicon named in the frontend is a real codicon", () => {
    const css = readFileSync(CODICON_CSS, "utf8");
    // Glyphs (`.codicon-x:before`) and modifiers. A modifier such as
    // `codicon-modifier-spin` has no glyph: it is defined in compound selectors
    // (`.codicon-loading.codicon-modifier-spin`) and its own rule, so the glyph
    // pattern alone never knew it and reported a real class as unknown.
    const known = new Set([
      ...[...css.matchAll(/^\.codicon-([a-z0-9-]+):/gm)].map((m) => m[1]),
      ...[...css.matchAll(/\.codicon-(modifier-[a-z0-9-]+)/g)].map((m) => m[1]),
    ]);
    // A sanity floor: if the stylesheet moved or its format changed, an empty
    // `known` set would make this test pass by knowing nothing.
    expect(known.size).toBeGreaterThan(400);

    const named = sources(STUDIO_SRC).flatMap((file) => {
      const text = readFileSync(file, "utf8");
      // `codicon("x")` and the literal class form. Names built by interpolation
      // -- `codicon-${iconFor(process)}` in the process view -- cannot be read
      // statically; those are covered by the views' own conformance tests, which
      // assert on the elements that carry them.
      const found: { file: string; name: string }[] = [];
      for (const m of text.matchAll(/\bcodicon\("([a-z0-9-]+)"\)/g)) {
        found.push({ file, name: m[1] });
      }
      // The literal class form, e.g. `className="codicon codicon-pinned"`. The
      // trailing character is captured and inspected rather than bounded with
      // `\b`, because a `\b` match on `codicon-chevron-${down ? ... }` stops at
      // `chevron` and reports a codicon nobody wrote. A name that runs into `${`
      // or ends in `-` is built at runtime, so it is skipped here.
      for (const m of text.matchAll(/codicon-([a-z0-9-]+)([^a-z0-9-]|$)/g)) {
        const [, name, next] = m;
        if (next === "$" || next === "{" || name.endsWith("-")) continue;
        found.push({ file, name });
      }
      return found;
    });
    expect(named.length).toBeGreaterThan(6);

    const unknown = [
      ...new Set(
        named
          .filter(({ name }) => !known.has(name))
          .map(({ file, name }) => `${file.replace(STUDIO_SRC, "")}: codicon-${name}`),
      ),
    ];
    expect(unknown).toEqual([]);
  });

  test("the Gearbox views carry a codicon in the shell", async ({ studio }) => {
    // **The state is established, not inherited.** The Inspector is gated on
    // having a subject -- a panel whose whole content is "select something" is
    // worse than an absent one -- so with nothing selected it has no tab and no
    // icon to carry. This test inherited a session where something happened to
    // be selected, and that held only because two conformance claims failed
    // earlier in the run and left one behind them. With those fixed the suite
    // reaches here at the home screen and this asserted on a tab that does not
    // exist. The same mistake the first test in this file already names --
    // "relied on that without saying so" -- two screens further in.
    await openProduct(studio.page, "dev");
    await revealInspector(studio.page);
    // Theia gives every tab the id `shell-tab-<widgetId>`, which is what makes
    // this checkable without depending on a label the activity bar does not show.
    const icons = await studio.page.evaluate(() =>
      Object.fromEntries(
        ["gearbox.catalogue", "gearbox.inspector"].map((id) => {
          const tab = document.querySelector(`#shell-tab-${CSS.escape(id)}`);
          const icon = tab?.querySelector(".lm-TabBar-tabIcon");
          return [id, icon?.className ?? ""];
        }),
      ),
    );
    // The catalogue is the reported case: left bar, icon only, so an empty class
    // is a button with nothing in it.
    expect(icons["gearbox.catalogue"]).toContain("codicon-library");
    expect(icons["gearbox.inspector"]).toContain("codicon-info");
  });
});

test.describe("the toolbar names only registered commands", () => {
  const STUDIO_SRC = join(__dirname, "../gearbox-studio/src/browser");

  function sources(dir: string): string[] {
    return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) return sources(path);
      return /\.tsx?$/.test(entry.name) ? [path] : [];
    });
  }

  test("every command id the toolbar names is registered", () => {
    // Same form as the codicon check: the source is the contract, not a running
    // registry. A toolbar that invents an id compiles and fails at click time.
    const toolbar = readFileSync(
      join(STUDIO_SRC, "shell/toolbar-widget.tsx"),
      "utf8",
    );
    const block = toolbar.match(/TOOLBAR_COMMAND_IDS = \[([\s\S]*?)\]/);
    expect(block, "TOOLBAR_COMMAND_IDS is missing").not.toBeNull();
    const ids = [...(block?.[1] ?? "").matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    expect(ids.length).toBeGreaterThan(0);

    // Both spellings a command id is declared in. `id: "..."` covers a `Command`
    // literal; `toggleCommandId: "..."` covers the one every
    // `AbstractViewContribution` registers for its view -- which is how the
    // header's Generate entry is declared, and which this test missed at first,
    // reporting a registered command as unregistered.
    const registered = sources(STUDIO_SRC).flatMap((file) => {
      const text = readFileSync(file, "utf8");
      return [
        ...[...text.matchAll(/\bid:\s*"([^"]+)"/g)],
        ...[...text.matchAll(/\btoggleCommandId:\s*"([^"]+)"/g)],
      ].map((m) => m[1]);
    });
    const unknown = ids.filter((id) => !registered.includes(id));
    expect(unknown, `toolbar names unregistered commands: ${unknown.join(", ")}`).toEqual(
      [],
    );
  });

  test("the toolbar renders exactly the commands it declares", async ({ studio }) => {
    await openProduct(studio.page, "dev");
    await expectContext(studio.page, "product");
    await expect(studio.page.locator(".gbx-toolbar-actions [data-command]")).toHaveCount(0);
    await expect(studio.page.locator('.gbx-product-context [data-command="gearbox.product.switch"]')).toBeVisible();
    await expect(studio.page.locator(".gbx-product-nav [role=tab]")).toHaveCount(4);
  });

  test.fixme("the toggle command reveals a view hidden behind another tab", async ({ studio }) => {
    // **Measured as broken, and deliberately not fixed here.**
    //
    // This asserts the path a person takes -- Gearbox > Product, or the palette --
    // as opposed to the path `revealView` takes, which clicks the shell tab
    // because that proved reliable across the suite. It passes in isolation with
    // the Graph over Product, and fails inside the suite; the toggle semantics of
    // `AbstractViewContribution` plus the palette are two stateful things and one
    // of them loses.
    //
    // A fixme rather than a fix because the mechanism is being removed: view
    // toggles through a generic menu are what the shell rework replaces with
    // context tabs, so repairing this would be work on code that is going away.
    // It stays named so the claim is not lost with the mechanism -- whatever
    // replaces it has to reveal a hidden panel from a menu.
    await openProduct(studio.page, "dev");
    await expect(studio.page.locator(".gbx-product")).toBeVisible();

    // Put the Graph on top of it in the same area.
    await openGraph(studio.page);
    const covered = !(await studio.page.locator(".gbx-product").first().isVisible());
    test.skip(!covered, "the graph did not cover Product, so there is nothing hidden to reveal");

    await runCommand(studio.page, "Gearbox Product");
    await expect(studio.page.locator(".gbx-product")).toBeVisible({ timeout: 30_000 });
  });

  test("the header reports the context, and the context follows what is open", async ({
    studio,
  }) => {
    // The property that made this rework necessary: a shell state can no longer
    // disagree with reality. `data-context` is written by `StudioContextService`
    // from `ProductStore`, not from a restored perspective -- so there is no way
    // to be in the product context with no product.
    await openProduct(studio.page, "dev");
    await expectContext(studio.page, "product");
    await expect(studio.page.locator(".gbx-product")).toBeVisible();
    await expect(studio.page.locator(".gbx-toolbar-name")).not.toBeEmpty();
  });
});

test.describe("a product is a session, not a panel", () => {
  /**
   * Closing and reopening works, and that is a claim about the engine.
   *
   * **This was a flake in another file for as long as nobody asserted it.**
   * Closing a product cleared the store but left the engine initialised on that
   * product's source roots, because nothing called `initialize` again. A later
   * open could then fail at the "starting the engine on the product's folder"
   * step, `openRecent` silently dropped the entry, and the shell stayed on Home
   * -- which surfaced as whichever test happened to run next timing out on
   * `data-context="product"`, in a describe block with nothing to do with
   * sessions. Measured: with `CatalogueStore.resetToBootSession` removed the
   * failure returns in two runs out of two; with it, six runs clean.
   *
   * So the behaviour gets a test of its own, next to the other session claims.
   * A defect whose only alarm is somebody else's flaky test is a defect that
   * gets attributed to the test.
   */
  test("a product closed and opened again opens", async ({ freshStudio }) => {
    // **Slow by nature, not by defect.** A fresh Studio plus two product loads:
    // 40 s alone, and past the default budget in a loaded full run.
    test.slow();
    const { page } = freshStudio;
    await settled(page);

    await openProduct(page, "dev");
    await expectContext(page, "product");

    await runCommand(page, "Close Product");
    await expectContext(page, "home");

    // The second open is the whole point: same session, same page, engine now
    // back on its boot roots rather than the closed product's.
    await openProduct(page, "dev");
    await expectContext(page, "product");
    await expect(page.locator("[data-resolved-profile]")).toBeVisible({ timeout: 60_000 });
  });

  test("closing returns the shell to home and empties the header", async ({ studio }) => {
    // Closing is not "hide the panel": the session ends, so the resolution, the
    // lock, the diagnostics and the selection go with it. A stale resolution
    // behind a closed product is worse than an empty one, because it looks like an
    // answer.
    await openProduct(studio.page, "dev");
    await expectContext(studio.page, "product");

    await runCommand(studio.page, "Close Product");
    await expectContext(studio.page, "home");
    await expect(studio.page.locator(".gbx-toolbar-empty")).toBeVisible();
    await expect(studio.page.locator(".gbx-toolbar-name")).toHaveCount(0);
  });

  test("a product that opened is offered again, and reopens from the picker", async ({
    studio,
  }) => {
    // Two claims in one flow, because they are the same flow. Recent is written
    // only on a successful open -- the difference between a Recent list and a list
    // of things once attempted -- and reopening goes through the picker.
    //
    // **Not through `openProduct`**, and that is the point. After a close the
    // Product panel does not re-open anything: `ensureOpen` runs when the widget
    // is constructed, not every time it is shown, so a close stays closed. A panel
    // that reopened what you just closed would make Close look broken. The panel
    // offers the picker instead, which is what this drives.
    await runCommand(studio.page, "Open Product…");
    const first = studio.page.locator(`.quick-input-list [role="option"]`);
    await first.first().waitFor({ state: "visible" });
    await first.filter({ hasText: "payments-demo" }).first().click();
    await expectContext(studio.page, "product");

    await runCommand(studio.page, "Close Product");
    await expectContext(studio.page, "home");

    await runCommand(studio.page, "Open Product…");
    const options = studio.page.locator(`.quick-input-list [role="option"]`);
    await options.first().waitFor({ state: "visible" });
    const labels = await options.evaluateAll((ns) => ns.map((n) => (n.textContent ?? "").trim()));
    await studio.page.keyboard.press("Escape");
    expect(labels.some((l) => l.includes("payments-demo"))).toBe(true);
  });

  test.fixme("closing refuses while the description has unsaved changes", async ({ studio }) => {
    // The guard is written -- `ProductSessionService.close` reads
    // `MonacoTextModelService.models` for a dirty model of the description, the
    // same check `ProductEditService` makes before writing -- but making a model
    // dirty from a test means typing into Monaco, and this repository has already
    // recorded why that is not straightforward: `.monaco-editor .inputarea` is
    // parked off-screen, so `press` hangs on it.
    //
    // Named rather than skipped silently, because the claim matters: closing under
    // an unsaved edit is exactly the loss the write gates exist to prevent.
    await openProduct(studio.page, "dev");
  });
});

test.describe("a required config field says so", () => {
  // Nothing in a design document says "a required field wears an asterisk", so
  // this lives here rather than in the conformance table. It exists because the
  // marker, the inline note and the placeholder had no DOM coverage at all --
  // the three of them could have been deleted and every suite stayed green.

  // **Read in the product, not in the add dialog.** These three markers belong
  // to `ConfigFields`, which is one component shared by the Composition pane and
  // the Inspector; the dialog stopped rendering it when configuration moved to
  // the gear it configures. `configurable-gears` names both subjects, which
  // `payments-demo` cannot: `event-broker` and `grpc-hub` reach it through the
  // closure, and a gear nothing named has no `use_gear` entry and so no form.
  //
  // `configureGear` returns the form scoped to the Composition pane, because the
  // Inspector renders the same component and an unscoped marker matches twice.

  test("a field the gear requires and does not default is marked", async ({ studio }) => {
    // `event-broker` is the corpus's one gear that exercises this:
    // `EventBrokerConfig` carries no container `#[serde(default)]`, so `mode`
    // and `default_storage_backend` are required, and neither default is a
    // literal the projector can read. Engine-side the same pair is what
    // `GBX0120` reports.
    await openProductById(studio.page, "configurable-gears", "dev");
    const form = await configureGear(studio.page, "event-broker");

    for (const field of ["mode", "default_storage_backend"]) {
      await expect(
        form.locator(`[data-config-field-required="${field}"]`),
        `${field} is required with no default, so it must be marked`,
      ).toHaveCount(1, { timeout: 30_000 });
    }
    // Said in words as well as by the glyph: a `title` on an empty span reaches
    // nobody using a screen reader.
    await expect(form.locator('[data-config-field-required="mode"] .gbx-sr-only')).toHaveText(
      "required",
    );
  });

  test("a gear whose config all defaults is marked nowhere", async ({ studio }) => {
    // The negative control, and the reason this pair exists: `grpc-hub` looks
    // like a gear that ought to demand a listen address, and it demands
    // nothing. `GrpcHubConfig` is `#[serde(deny_unknown_fields, default)]`, so
    // every field has a default; `listen_addr` is an endpoint's `config_key`
    // besides, which generation writes from the port the resolver assigned.
    // An empty panel here is the right answer, not a missing feature.
    await openProductById(studio.page, "configurable-gears", "dev");
    const form = await configureGear(studio.page, "grpc-hub");

    expect(
      await form.locator("[data-config-field]").count(),
      "grpc-hub exposes three config fields, so this is not passing on an empty form",
    ).toBeGreaterThan(0);
    await expect(form.locator("[data-config-field-required]")).toHaveCount(0);
  });

  test("the note and the placeholder say it in words too", async ({ studio }) => {
    // The marker is a glyph, and a glyph is not an explanation. Both of the
    // other two surfaces that carry the same fact had no coverage at all, so
    // either could have been deleted silently -- which is the whole reason this
    // describe exists.
    const { page } = studio;
    await openProductById(page, "configurable-gears", "dev");
    const form = await configureGear(page, "event-broker");

    for (const field of ["mode", "default_storage_backend"]) {
      await expect(
        form.locator(`[data-config-field-missing="${field}"]`),
        `${field} is required with no default, so it must say so in words`,
      ).toHaveText("required, and the gear declares no default");
      // **And the provenance beside it must not contradict it.** These carried
      // `the gear's default` over that same note, because "nobody sets this
      // key" and "the gear supplies one" were one word. A person reading both
      // could not tell whether a value was needed.
      await expect(
        form.locator(`[data-config-field="${field}"]`),
        `${field} has no default, so nothing here may say it has one`,
      ).toHaveAttribute("data-config-provenance", "unset");
      await expect(
        form.locator(`[data-config-field="${field}"] [data-config-provenance-label]`),
      ).toHaveText("not configured · required");
    }

    // The placeholder reaches the two kinds by different routes, so both are
    // asserted: an enum has no `placeholder` attribute to carry it, and shows
    // it as the text of the empty option that means "left alone".
    await expect(
      form.locator('[data-config-field="mode"] select option[value=""]'),
      "an enum carries the placeholder as its empty option",
    ).toHaveText("required");
    await expect(
      form.locator('[data-config-field="default_storage_backend"] input'),
      "a string field carries it as the attribute",
    ).toHaveAttribute("placeholder", "required");
  });
});

test.describe("the Generate screen names the profile it plans for", () => {
  // Without this the absent `docker/` and `helm/` of an embedded profile read as
  // a missing feature rather than as the definition of the profile kind -- the
  // question they actually prompted. The note is driven by
  // `resolution.product.kubernetes`, the same fact the engine branches on, so it
  // is keyed to the profile *kind* and not to a list of profile ids kept here.

  test("an embedded profile names itself and says what it does not produce", async ({
    studio,
  }) => {
    const { page } = studio;
    await openProduct(page, "dev");
    await openGenerate(page);

    await expect(page.locator("[data-generate-profile]")).toHaveAttribute(
      "data-generate-profile",
      "dev",
    );
    await expect(page.locator("[data-generate-no-deployment]")).toBeVisible();
    // And the plan agrees with the note rather than merely sitting beside it.
    await expect(page.locator('[data-plan-path^="docker/"]')).toHaveCount(0);
    await expect(page.locator('[data-plan-path^="helm/"]')).toHaveCount(0);
  });

  test("a kubernetes profile drops the note and plans the images and the chart", async ({
    studio,
  }) => {
    const { page } = studio;
    await openProduct(page, "prod");
    await openGenerate(page);

    await expect(page.locator("[data-generate-profile]")).toHaveAttribute(
      "data-generate-profile",
      "prod",
    );
    await expect(
      page.locator("[data-generate-no-deployment]"),
      "this profile does produce them, so the note must be gone",
    ).toHaveCount(0);
    expect(
      await page.locator('[data-plan-path^="docker/"]').count(),
      "a kubernetes profile plans Dockerfiles",
    ).toBeGreaterThan(0);
    expect(
      await page.locator('[data-plan-path^="helm/"]').count(),
      "a kubernetes profile plans a chart",
    ).toBeGreaterThan(0);
  });
});

test.describe("the diagnostics count is a signpost, not a hijack", () => {
  // The Validation stage already rendered the whole list with its counts; what
  // was missing was any reason to go there.
  //
  // **These used to assert the opposite of what they assert now.** An
  // error-carrying resolution took the stage to Validation, which was the right
  // answer while the arriving stage was Overview -- a summary that says nothing
  // about a product that did not resolve. The product screen opens on its
  // composition now, and a composition is built from the intent: it is there,
  // and it is what a person opened the product to see, error or not. So the
  // count and the summary line are the way to the problems, and the trip is
  // chosen rather than imposed.

  test("the count is on the tab, and warnings do not move the stage", async ({ studio }) => {
    await openProduct(studio.page, "dev");

    const tab = studio.page.locator('[data-product-section="checks"]');
    const badge = tab.locator("[data-validation-count]");
    await expect(badge).toBeVisible({ timeout: 60_000 });

    // Read off the stage rather than hard-coded: the demo's diagnostics change
    // whenever a code is added, and a number pinned here would be a second
    // place to update.
    const shown = Number(await badge.getAttribute("data-validation-count"));
    expect(shown, "this profile resolves with diagnostics to count").toBeGreaterThan(0);

    // The demo resolves with warnings and an info and no errors, which is the
    // case that must *not* move anything: a product whose ordinary state moved
    // the screen would move it always and so mean nothing.
    await expect(badge).toHaveAttribute("data-validation-worst", "warning");
    // Overview, not Composition: `openProduct` establishes a stage on purpose,
    // and the claim is that warnings leave *whatever* stage that is alone. The
    // arriving stage is claimed further down, where nothing establishes one.
    await expect(
      studio.page.getByRole("heading", { name: "Product settings", exact: true }),
      "warnings must leave the selected settings section alone",
    ).toBeVisible();

    // And the count agrees with the list it points at.
    await productSection(studio.page, "validation");
    const rows = studio.page.locator("[data-product-validation] .gbx-conflict");
    expect(await rows.count()).toBe(shown);
  });

  // --- the other half, which needs a product that resolves with an error -----
  //
  // No product in the corpus does, so one is made to for the length of these
  // two tests. GBX0120 looks like the candidate and is not: it is a *warning*
  // on purpose, because a value may still arrive from a profile. GBX0115 -- a
  // config key the gear does not declare -- is an error, and resolution does
  // not stop on it, so the lock still comes back with applications, bindings
  // and gears. That combination is exactly what the stage-moving path needs.

  /** The one line these tests rewrite, and what they rewrite it to. */
  const DECLARED = '        use_gear("api-gateway", source = "gears-rust"),\n';
  const WITH_ERROR =
    '        use_gear("api-gateway", source = "gears-rust", config = {"demo_mode": "x"}),\n';

  /**
   * Open `payments-demo` **without** establishing a stage.
   *
   * `openProduct` clicks Overview -- correct for every other test, and fatal
   * here, because the arriving stage is the claim. This does the opening half
   * only, and the demo's own default profile is the `dev` one these tests want,
   * so no profile switch is needed either.
   */
  async function openKeepingStage(page: import("@playwright/test").Page): Promise<void> {
    if ((await page.locator(".gbx-toolbar").getAttribute("data-context")) !== "product") {
      const card = page.locator('[data-start-action="continue"]');
      if (await card.isVisible().catch(() => false)) {
        await card.click();
      } else {
        await runCommand(page, "Open Product…");
        const options = page.locator(`.quick-input-list [role="option"]`);
        await options.first().waitFor({ state: "visible", timeout: 30_000 });
        await options.filter({ hasText: "payments-demo" }).first().click();
      }
      await expect(page.locator(".gbx-toolbar")).toHaveAttribute("data-context", "product", {
        timeout: 90_000,
      });
    }
    await page.locator(".gbx-product").waitFor({ state: "visible", timeout: 60_000 });
  }

  /**
   * Run `body` against a description that resolves with an error.
   *
   * The restore is not optional and not tidiness: `global-setup` refuses to
   * start the suite when `products/` differs from HEAD, and `base.afterEach`
   * fails whichever test left it dirty. A throw inside `body` would otherwise
   * block the *next* run rather than this one.
   */
  async function withAnErrorInTheDescription(body: () => Promise<void>): Promise<void> {
    const gdl = join(REPO, "products/payments-demo/product.gdl");
    const original = readFileSync(gdl, "utf8");
    expect(
      original,
      "the line these tests rewrite must be present verbatim, or they assert nothing",
    ).toContain(DECLARED);
    try {
      writeFileSync(gdl, original.replace(DECLARED, WITH_ERROR));
      await body();
    } finally {
      // **From `git`, not from the snapshot above.** This was the last place in
      // the suite that put a description back from bytes it had read: writing
      // `original` back assumes it was the committed text, and when it is not --
      // because something else left an edit behind -- the restore re-writes the
      // damage instead of undoing it. The other five sites moved to
      // `restoreProducts` after that turned one failure into three.
      restoreProducts(REPO);
    }
  }

  test("a resolution carrying an error still arrives on Composition", async ({ freshStudio }) => {
    const { page } = freshStudio;
    await withAnErrorInTheDescription(async () => {
      await settled(page);
      await openKeepingStage(page);

      const badge = page.locator("[data-validation-count]");
      await expect(badge, "the error has to reach the badge before the stage can mean anything")
        .toHaveAttribute("data-validation-worst", "error", { timeout: 90_000 });

      // The claim: an error is reported without taking the screen.
      await expect(
        page.locator('[data-product-section="composition"]'),
        "a product with an error still has a composition, and that is what was asked for",
      ).toHaveAttribute("aria-selected", "true");
      await expect(
        page.locator("[data-composition]"),
        "and the composition is rendered, not an empty panel",
      ).toBeVisible();
      await expect(
        page.locator('[data-asked-for="api-gateway"]'),
        "including the gear whose config carries the error",
      ).toBeVisible();

      // And the count is a way to the problem rather than an ornament.
      await page.locator('[data-product-section="checks"]').click();
      await expect(
        page.locator('[data-product-validation] [data-conflict-code="GBX0115"]'),
        "the stage the count points at shows the diagnostic",
      ).toBeVisible();
    });
  });

  test("rebuilding the panel comes back to Composition", async ({ freshStudio }) => {
    // **Slow by nature, not by defect.** A fresh Studio plus two product loads:
    // 40 s alone, and past the default budget in a loaded full run.
    test.slow();
    // The subscription covers a product arriving at a panel that already
    // exists. This is the other order: the panel is built while the product is
    // already resolved, so no store event follows and nothing would set the
    // stage. Closing the view and reopening it is that order, and it is a thing
    // a person does.
    const { page } = freshStudio;
    await withAnErrorInTheDescription(async () => {
      await settled(page);
      await openKeepingStage(page);
      await expect(page.locator("[data-validation-count]")).toHaveAttribute(
        "data-validation-worst",
        "error",
        { timeout: 90_000 },
      );

      // A stage a person chose, so that coming back to Composition cannot be
      // mistaken for the stage simply never having moved.
      await productSection(page, "overview");

      const tab = page.locator('[id="shell-tab-gearbox.product"]');
      await tab.locator(".lm-TabBar-tabCloseIcon").click();
      await page.locator(".gbx-widget-product").waitFor({ state: "detached", timeout: 30_000 });

      await runCommand(page, "Gearbox: Show Product");
      await page.locator(".gbx-product").waitFor({ state: "visible", timeout: 60_000 });

      await expect(
        page.locator('[data-product-section="composition"]'),
        "a fresh panel over an error-carrying resolution still opens on the product",
      ).toHaveAttribute("aria-selected", "true");
    });
  });
});
