// `cpt-gearbox-fr-studio` is one requirement with six clauses, and they are in
// very different states. Splitting it into six tests is the point: as one
// requirement it reads as "not done", which says nothing about which part.
//
//   The system MUST provide an Eclipse Theia application that browses the
//   catalogue, edits and resolves a product across profiles, renders the
//   dependency, contract, application, and cluster graphs, answers "why" for a
//   selected decision, and previews and applies generation -- containing no
//   resolution logic of its own.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { join } from "node:path";

import { copyProduct, withoutGear, type ProductCopy } from "../fixtures/product-copy";
import {
  closeGraph,
  expect,
  openExplain,
  openGenerate,
  openGraph,
  openGraphView,
  openProduct,
  openProductById,
  productSection,
  resetCatalogueView,
  revealCatalogue,
  settled,
  test,
} from "../fixtures/studio";

const REPO = join(__dirname, "../../..");
/**
 * Where Studio generates: the engine's own default, asserted rather than assumed.
 *
 * This was `.gearbox/studio/` while `product.lock` still depended on how the
 * client spelled its source root -- one shared tree would have meant the CLI and
 * Studio rewriting each other's lock. The digest is content-based now, so both
 * produce byte-identical trees and one tree serves both. Asserting the path keeps
 * that a claim: if Studio ever invents its own root again, this fails.
 */
const GENERATE_OUT = join(REPO, ".gearbox/payments-demo/dev");
const GENERATE_OUT_SUFFIX = ".gearbox/payments-demo/dev";

const PRODUCT = "products/payments-demo/product.gdl";

/** `git diff --stat` for the demo description, or "" when it matches HEAD. */
function diffOfProduct(): string {
  return execFileSync("git", ["diff", "--stat", "--", PRODUCT], {
    cwd: REPO,
    encoding: "utf8",
  }).trim();
}

/**
 * Accept the edit dialog, and only the edit dialog.
 *
 * `.theia-button.main` is every Theia dialog's default button, so an unscoped
 * click accepts whatever happens to be open -- and the dialog this accepts
 * *writes to a description*.
 */
async function acceptEdit(page: import("@playwright/test").Page): Promise<void> {
  // **By the confirmation's own class, not by "has a preview".** The add-gear
  // configurator is a modal showing what it would write, so the preview pane no
  // longer identifies *this* dialog -- and accepting the wrong one is exactly
  // what the note above says must not happen.
  const dialog = page.locator(".gbx-edit-confirm");
  await expect(dialog, "the edit dialog is not open, so there is nothing to accept").toBeVisible();
  await dialog.locator(".theia-button.main").click();
}


/**
 * One generated file this test may delete and have regenerated.
 *
 * A config file rather than a `.rs`: it is `Ownership::Generated`, so removing it
 * makes the plan say `create`, and restoring it cannot invalidate an incremental
 * Rust build the way touching a source file would.
 *
 * Deleting one file rather than clearing the tree, which is what this test used
 * to do. The tree is now the one §12 step 2 builds and runs, and it carries
 * gigabytes of `target/`; a suite that wipes it costs a rebuild every run. One
 * file is also the stronger assertion -- it proves the plan is accurate per file
 * rather than merely non-empty.
 *
 * Named after the *application*, which under `dev` is the product: the embedded
 * profile builds exactly one application and it is the whole product, so it
 * carries the product's name. This said `api-gateway` -- the anchor gear -- for
 * a while after that changed, and nothing caught it, because this suite had not
 * run in thirty-one commits and a Playwright spec is typechecked by nothing.
 */
const REGENERABLE = "config/payments-demo.yaml";

const STUDIO_SRC = join(__dirname, "../../gearbox-studio/src");

/** Every `.ts`/`.tsx` file under a directory, recursively. */
function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sources(path);
    return /\.tsx?$/.test(entry.name) ? [path] : [];
  });
}

test.describe("cpt-gearbox-fr-studio, clause by clause", () => {
  test("it browses the catalogue [PRD cpt-gearbox-fr-studio: browses the catalogue]", async ({
    studio,
  }) => {
    const rows = await studio.page.locator(".gbx-widget-catalogue .gbx-row").count();
    expect(rows).toBeGreaterThan(0);
    const detail = await studio.detailOf("API Gateway");
    expect(detail).toContain("api-gateway");
  });

  test("it renders the dependency graph [PRD cpt-gearbox-fr-studio: renders the dependency graph]", async ({
    studio,
  }) => {
    // `openGraph`, not the command directly: the view command is a *toggle*, and
    // this suite shares one loaded application across the read-only tests, so a
    // file that ran earlier may already have opened it.
    await openGraph(studio.page);
    await expect(studio.page.locator(".gbx-svg")).toBeVisible();
    const nodes = await studio.page.locator(".gbx-node-label").count();
    expect(nodes).toBeGreaterThan(0);
  });

  test("it resolves a product across profiles [PRD cpt-gearbox-fr-studio: edits and resolves a product]", async ({
    studio,
  }) => {
    // The *editing* half is the `.gdl` editor Theia already provides -- there is
    // deliberately no form, because a form would be a second way to express a
    // description and the two would drift. What the view adds is the half a text
    // editor cannot show: what the description resolves to, per profile.
    await openProduct(studio.page, "dev");
    await expect(studio.page.locator(".gbx-product")).toBeVisible();
    const profiles = await studio.page.locator("[data-profile]").allTextContents();
    expect(profiles.length).toBeGreaterThan(1);
    await expect(studio.page.locator("[data-resolved-profile='dev']")).toBeVisible();
  });

  // The three resolution graphs are checked on the **`prod`** profile, not the
  // default `dev`, and that is not incidental. On `dev` the demo product resolves
  // to two local bindings and one application, so each of these
  // views would render truthfully and show nothing that could have made it wrong.
  // On `prod` the same description resolves to a severed contract edge beside a
  // local one, and to two applications instead of one.
  //
  // It also shows a gear in several binaries: each worker serves REST and so
  // includes `authn-resolver` (GBX0417), whose closure the gateway links too.

  test("it renders the contract graph [PRD cpt-gearbox-fr-studio: renders the contract graph]", async ({
    studio,
  }) => {
    await openProduct(studio.page, "prod");
    await openGraphView(studio.page, "contracts");

    const graph = studio.page.locator("[data-graph='contracts']");
    await expect(graph).toBeVisible();

    // `PaymentApi@v1` is bound remote for `local` and `prod` while `@v2` stays
    // local, between the same pair of gears. So this profile must show a severed
    // edge -- the picture that distinguishes a contract edge from a co-location
    // edge, which can never be severed.
    // Two severed edges: this pair, and the consumer's cluster requests, which
    // the `audit` worker makes over gRPC (ADR-0026 section 4).
    await expect(graph.locator(".gbx-edge[data-remote='true']")).toHaveCount(2);
    await expect(
      graph.locator(".gbx-edge[data-remote='true'][data-to='cluster']"),
    ).toHaveAttribute("data-from", "api-contracts-consumer");
    const severed = graph.locator(".gbx-edge[data-remote='true'][data-to='api-contracts']");
    await expect(severed).toHaveCount(1);
    await expect(severed).toHaveAttribute("data-from", "api-contracts-consumer");
    await expect(severed).toHaveAttribute("data-to", "api-contracts");
    await expect(severed).toHaveClass(/gbx-edge-remote/);
    // Both contracts between this pair travel it. `mode` is derived from
    // placement, never configured, so two bindings between the same pair of gears
    // always agree about it -- which is why one arrow per pair loses nothing.
    await expect(severed).toHaveAttribute(
      "data-contract",
      "api-contracts/PaymentApi@v1 api-contracts/PaymentApi@v2",
    );

    // The same edge, the same description, the other profile: local. That contrast
    // is the claim -- a contract edge is a resolver decision, not a declared fact,
    // and the co-location graph has no equivalent because a `deps` edge cannot
    // change with the profile.
    await openProduct(studio.page, "dev");
    await openGraphView(studio.page, "contracts");
    const local = studio.page.locator("[data-graph='contracts'] .gbx-edge");
    await expect(local).toHaveCount(1);
    await expect(local).toHaveAttribute("data-remote", "false");
    await expect(local).toHaveClass(/gbx-edge-local/);
  });

  test("it renders the application graph [PRD cpt-gearbox-fr-studio: renders the application graph]", async ({
    studio,
  }) => {
    await openProduct(studio.page, "prod");
    await openGraphView(studio.page, "applications");

    // Boxes with gear chips, as plan §9 specifies, rather than the `.gbx-svg` this
    // test guessed at before the view existed: a node-link drawing cannot show one
    // gear inside two boxes, which is the whole content of the view.
    const graph = studio.page.locator("[data-graph='applications']");
    await expect(graph).toBeVisible();
    // Three, not two: `audit` is declared in the description, and `api-contracts`
    // becomes an application of its own because the remote binding needs it
    // reachable across a boundary. An application the resolver *derived* is
    // exactly the kind of thing this view exists to make visible.
    await expect(graph.locator(".gbx-binary")).toHaveCount(3);
    for (const name of ["api-gateway", "audit", "api-contracts"]) {
      await expect(graph.locator(`[data-binary='${name}']`)).toBeVisible();
    }
    // Not a partition: each worker serves REST and so includes `authn-resolver`
    // (GBX0417), and its closure -- the AuthN plugin and `types-registry` -- is in
    // the gateway too. Three gears, each in all three binaries, marked shared.
    // Seven, not six: the product selects `cluster` explicitly now, because the
    // provider registry every cluster requirement resolves against is projected
    // from that gear and nothing else pulls it in.
    await expect(graph.locator("[data-binary='api-gateway'] [data-gear]")).toHaveCount(7);
    for (const gear of ["authn-resolver", "oidc-authn-plugin", "types-registry"]) {
      await expect(graph.locator(`[data-shared='true'][data-gear='${gear}']`)).toHaveCount(3);
    }
    await expect(graph.locator("[data-shared='true']")).toHaveCount(9);
  });

  test("it renders the cluster graph [PRD cpt-gearbox-fr-studio: renders the cluster graph]", async ({
    studio,
  }) => {
    await openProduct(studio.page, "prod");
    await openGraphView(studio.page, "cluster");

    // Observed rather than skipped since `api-contracts-consumer` requires the
    // `event-broker` scope: its crate carries the `impl ClusterProfile` marker
    // that makes the profile name a join key, so a `ResolvedClusterBinding`
    // reaches this view in every profile. The requester was to be
    // `payments-audit` (plan §10), a gear nobody wrote; the requirement moved to
    // a gear that exists rather than waiting for one that does not.
    const drawn = studio.page.locator("[data-graph='cluster']");
    await expect(drawn).toBeVisible();
    await expect(drawn.locator("[data-cluster-requirement]").first()).toBeVisible();
    await expect(drawn.locator("[data-cluster-provider]").first()).toBeVisible();
  });

  /**
   * The empty state still explains itself.
   *
   * It used to be the only thing this file asserted about the cluster view, in
   * the branch that skipped when nothing required a primitive. Requiring one
   * made the graph observable and took that assertion with it -- so the view's
   * own comment claimed the empty state "still says what is missing" with
   * nothing left checking it. A view that renders a blank frame here is
   * indistinguishable from a broken one, which is exactly what the skipped
   * branch existed to guard against.
   *
   * Reaching it now costs an edit: `api-contracts-consumer` is the only
   * requester in the corpus, so the state appears only with it removed. The
   * write goes through the same previewed path everything else uses, and the
   * `finally` restores the description whether or not the assertions held.
   */
  test("the cluster view explains an empty resolution [PRD cpt-gearbox-fr-studio: renders the cluster graph]", async ({
    studio,
  }) => {
    const page = studio.page;
    expect(diffOfProduct(), "the description must start clean").toBe("");

    try {
      await openProduct(page, "dev");
      await revealCatalogue(page);
      await resetCatalogueView(page);

      // **Removed from the composition, not from the catalogue.** The
      // catalogue's control for a gear already in the product is "show it
      // there" now: removing is an act on the product's own structure, so the
      // surface that owns the structure is the one that offers it. The
      // catalogue is only asked to agree afterwards.
      const toggle = page.locator('[data-toggle-gear="api-contracts-consumer"]');
      await expect(toggle).toHaveAttribute("data-in-product", "true", { timeout: 60_000 });
      await productSection(page, "composition");
      await page
        .locator(
          '[data-asked-for="api-contracts-consumer"] ' +
            'button[aria-label="Remove api-contracts-consumer from product"]',
        )
        .click();
      await acceptEdit(page);
      await expect(toggle).toHaveAttribute("data-in-product", "false", { timeout: 60_000 });

      await openGraphView(page, "cluster");
      await expect(page.locator("[data-graph='cluster']")).toHaveCount(0);
      const empty = page.locator(".gbx-widget-graph .gbx-cluster-empty");
      await expect(empty).toBeVisible();
      await expect(empty).toContainText("no gear in the catalogue currently requires it");
    } finally {
      // The Graph goes away *before* the reload, and `settled` runs after it.
      // Restoring the description is what this reload is for; leaving a screen
      // that takes the room in front of it made the restorer re-activate the
      // Graph seconds into the next test, which then waited a full minute for a
      // Product header that was behind it. `closeGraph` says why in full.
      await closeGraph(page);
      execFileSync("git", ["checkout", "--", PRODUCT], { cwd: REPO });
      await page.reload();
      await settled(page);
    }
  });

  test("it answers why for a selected decision [PRD cpt-gearbox-fr-studio: answers why]", async ({
    studio,
  }) => {
    // §9 expected a `gearbox/product/explain` call. There is none and none is
    // needed: `ResolveResult` carries the whole `ExplanationGraph` with the
    // resolution, so "why" costs no second round trip and cannot answer about a
    // different resolution than the one on screen. The substance is checked in
    // `prd-explain.spec.ts`; this clause is only that the view exists and answers.
    await openProduct(studio.page, "prod");
    await openExplain(studio.page);
    // Bindings are on the Topology stage; the panel has four since 2026-09-07.
    await productSection(studio.page, "topology");
    await studio.page.click("[data-binding]");
    await expect(studio.page.locator("[data-explaining]")).toBeVisible();
    await expect(studio.page.locator(".gbx-step").first()).toBeVisible();
  });

  test("it previews and applies generation [PRD cpt-gearbox-fr-generate-preview]", async ({
    studio,
  }) => {
    // Remove one generated file so the plan has something to create. Without
    // this the tree is already current and every line reads `unchanged`, which
    // proves nothing about applying.
    rmSync(join(GENERATE_OUT, REGENERABLE), { force: true });

    await openProduct(studio.page, "dev");
    await openGenerate(studio.page);
    await expect(studio.page.locator("[data-plan-path]").first()).toBeVisible({
      timeout: 60_000,
    });
    await expect(studio.page.locator(".gbx-generate")).toBeVisible();

    // The output root is part of the claim, not an implementation detail the test
    // may quietly follow.
    await expect(studio.page.locator(".gbx-generate")).toHaveAttribute(
      "data-out-root",
      new RegExp(`${GENERATE_OUT_SUFFIX.replace(/\./g, "\\.")}$`),
    );

    const line = studio.page.locator(`[data-plan-path="${REGENERABLE}"]`);
    await expect(line).toHaveAttribute("data-action", "create");

    const porcelain = (): string =>
      execFileSync("git", ["status", "--porcelain"], { cwd: REPO, encoding: "utf8" });
    const before = porcelain();
    expect(
      existsSync(join(GENERATE_OUT, REGENERABLE)),
      "the preview must not have written the file it plans to create",
    ).toBe(false);
    expect(porcelain(), "a preview must not change the git tree").toBe(before);

    await studio.page.locator("[data-apply]").click();
    await expect(studio.page.locator("[data-written-count]")).toBeVisible({ timeout: 60_000 });
    const written = Number(
      await studio.page.locator("[data-written-count]").getAttribute("data-written-count"),
    );
    expect(written).toBeGreaterThan(0);
    expect(existsSync(join(GENERATE_OUT, REGENERABLE))).toBe(true);
    await expect(line).toHaveAttribute("data-action", "create");

    // Applying again writes nothing and every line reads `unchanged`. Not a
    // nicety: ADR-0010 requires generation to be idempotent by content, and this
    // is the only place that is observable from the interface.
    await studio.page.locator("[data-apply]").click();
    await expect(studio.page.locator("[data-written-count]")).toHaveAttribute(
      "data-written-count",
      "0",
      { timeout: 60_000 },
    );
    const actions = await studio.page
      .locator("[data-plan-path]")
      .evaluateAll((nodes) => nodes.map((node) => node.getAttribute("data-action")));
    expect(actions.length).toBeGreaterThan(0);
    expect(actions.every((action) => action === "unchanged")).toBe(true);
  });

  test("the resolver notice is gone, and gone because the engine says so [PRD cpt-gearbox-fr-rpc-api]", async ({
    studio,
  }) => {
    // The notice was driven by the engine's own `capabilities.resolve`, which is
    // why it needed no edit when M4 landed -- it disappeared on its own. That is
    // the property worth asserting: not that the text is right, but that it is
    // absent, and absent for the right reason.
    await studio.detailOf("API Gateway");
    await expect(studio.page.locator(".gbx-gap")).toHaveCount(0);
  });

  test("it contains no resolution logic of its own [PRD cpt-gearbox-fr-studio: no resolution logic]", () => {
    // The check looks for `BindingMechanism` *literals* in the frontend, and the
    // choice of signal is the whole design of this test.
    //
    // The first version forbade the type names -- `ResolvedProcess`,
    // `ResolvedBindingMode` -- and it failed the moment the Product view was
    // built, because rendering a resolution means importing the types of the
    // thing you are rendering. That was a false positive, not a finding: reading
    // `binding.mechanism` and printing it is consumption, which the clause
    // permits and in fact requires.
    //
    // A mechanism literal is different. It names the real code path the runtime
    // takes, the resolver derives it from placement, and there is no legitimate
    // reason for a client to write one down: a frontend that renders a mechanism
    // prints the value it was given, while a frontend that *decides* one has to
    // name it. So the literals are the line, and the types are not.
    //
    // It also deliberately does not flag `closureOf` in the graph widget, which
    // computes a transitive closure over projected `colocated_deps`. Co-location
    // is link-time and no resolution changes it, so that is presentation of a
    // projected fact rather than a decision.
    const mechanisms = [
      "colocated-local",
      "consumes-static",
      "consumes-directory",
      "provides-client-wiring",
    ];
    const offenders = sources(join(STUDIO_SRC, "browser"))
      .concat(sources(join(STUDIO_SRC, "node")))
      .flatMap((file) => {
        const text = readFileSync(file, "utf8");
        return mechanisms.filter((m) => text.includes(`"${m}"`)).map((m) => `${file}: "${m}"`);
      });
    expect(offenders).toEqual([]);
  });
});

/**
 * A cluster binding's options, which had no editor and no check at all.
 *
 * The options were carried verbatim from the description into the generated
 * YAML and nothing looked at a key: the authority was always there -- every
 * backend deserializes them into a struct with `#[serde(deny_unknown_fields)]`
 * -- but it was an authority that spoke at *backend startup*, in a deployed
 * system, where the description that caused the failure is not to hand. What
 * was missing was the join key, which `cluster_plugin(cache_options = "...")`
 * now declares.
 *
 * Configured on Topology rather than in Composition, and that is not a second
 * surface: a cluster binding is not a gear and has no row in the Composition
 * tree. The stage that shows it configures it.
 */
test.describe("a cluster binding's options are the backend's own struct", () => {
  let copy: ProductCopy;

  test.beforeEach(() => {
    copy = copyProduct(REPO, "payments-demo", "provider-options");
  });

  test.afterEach(() => {
    copy?.dispose();
  });

  /** Reach the form for `event-broker/cache` under the profile that uses postgres. */
  async function openOptions(page: import("@playwright/test").Page): Promise<void> {
    await openProductById(page, copy.id, "prod");
    await productSection(page, "topology");
    await expect(page.locator('[data-provider-options="event-broker/cache"]')).toBeVisible({
      timeout: 60_000,
    });
  }

  test("an option is edited, previewed against the binding it names, and written to it [PRD cpt-gearbox-fr-studio: edits and resolves a product]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await openOptions(page);
    const form = page.locator('[data-provider-options="event-broker/cache"]');

    // **The addressee, in full.** The scope name alone does not say which
    // binding this writes to: `payments-demo` declares `event-broker` twice, for
    // disjoint deployment profiles, with different providers.
    const head = form.locator("[data-provider-options-head]");
    await expect(head).toContainText("event-broker");
    await expect(head).toContainText("cache");
    await expect(head.locator("[data-provider-options-provider]")).toHaveText("postgres");
    await expect(head.locator("[data-provider-options-profiles]")).toHaveText("local, prod");
    await expect(form).toHaveAttribute("data-provider-options-rust", "PostgresClusterConfig");

    await form.locator('[data-config-field="pool_max_size"] input').fill("20");
    await head.click();
    // **The product's one draft, and the toolbar is where it lives.** The
    // `N pending changes` line belongs to the Composition stage; this edit is
    // made on Topology, so what says the change is queued is the pair in the
    // toolbar -- which is product-wide by design -- and the control's own
    // modified cue, which says *where* the unapplied edit is.
    await expect(page.locator("[data-draft-apply]")).toBeVisible();
    await expect(
      form.locator('[data-config-field="pool_max_size"]'),
      "the field says its value is not the file's yet",
    ).toHaveAttribute("data-field-modified", "true");

    await page.locator("[data-draft-apply]").click();
    const dialog = page.locator(".dialogBlock");
    await dialog.waitFor({ state: "visible", timeout: 60_000 });
    // The edit is named as what it is, not as "a change to the product".
    await expect(dialog.locator("[data-edit-target]")).toContainText("event-broker");
    await expect(dialog.locator("[data-edit-target]")).toContainText("pool_max_size");
    await expect(dialog.locator(".gbx-edit-preview")).toContainText("pool_max_size");
    await dialog.locator("button.theia-button.main").click();

    await expect
      .poll(() => readFileSync(copy.path, "utf8"), { timeout: 60_000 })
      .toContain("pool_max_size = 20");
    const written = readFileSync(copy.path, "utf8");
    // The neighbour with the same name, and the comments, are where they were.
    expect(written, "the dev scope was rewritten").toContain(
      'cache = provider("standalone"),',
    );
    expect(written, "a comment inside the edited call was lost").toContain(
      "# The reference, not the credential.",
    );

    // And the panel is showing what is on disk, not what was typed.
    await expect(
      page.locator('[data-provider-options="event-broker/cache"] [data-config-field="pool_max_size"] input'),
    ).toHaveValue("20", { timeout: 60_000 });
    await expect(
      page.locator("[data-draft-apply]"),
      "nothing is owed once it is written",
    ).toHaveCount(0);
  });

  test("the credential has no box and a way to write the reference instead [PRD cpt-gearbox-fr-no-secrets-in-values]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await openOptions(page);
    const form = page.locator('[data-provider-options="event-broker/cache"]');

    // `connection_string` is a plain `String` in Rust, so nothing in the
    // projection marks it and the Studio's name heuristic misses it. The plugin
    // declares which option it is, and the form does not offer to save one.
    await expect(form.locator('[data-config-field="connection_string"]')).toHaveCount(0);
    const note = form.locator("[data-provider-credential]");
    await expect(note).toHaveAttribute("data-provider-credential", "connection_string");
    await expect(note, "the syntax, not just a refusal").toContainText("secret_ref");
    await expect(note).toContainText("${VAR}");

    // **A way there, not only a sentence about it.** The description may be
    // long and the same provider may be bound twice, so the button opens the
    // file at the `provider(...)` the note is about.
    await note.locator("[data-provider-credential-edit]").click();
    await expect(
      page.locator('.monaco-editor[data-uri*="product.gdl"]'),
      "the description opened at the call",
    ).toBeVisible({ timeout: 60_000 });
  });

  test("a default the projection cannot read is not a value somebody must supply [ADR-0002: the catalogue is projected]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await openOptions(page);
    const form = page.locator('[data-provider-options="event-broker/cache"]');

    // Three states, not two. `replication_mode` carries `#[serde(default)]`
    // whose value is not a literal this projection can read, so it has no
    // default to show *and* needs nothing from anybody -- and a form that
    // collapsed the two would demand a value the backend already supplies.
    const unreadable = form.locator('[data-config-field="replication_mode"]');
    await expect(unreadable).toHaveAttribute("data-config-provenance", "default");
    await expect(unreadable).not.toHaveAttribute("data-config-provenance", "unset");
    // And one the description does set reads as set, with a way to drop it.
    await expect(form.locator('[data-config-field="schema"]')).toHaveAttribute(
      "data-config-provenance",
      "explicit",
    );
  });
});

/**
 * A written value this form cannot render, and must therefore not replace.
 *
 * Its own describe because it needs a different description: no shipped one
 * writes a non-scalar under a provider option, and the corpus's option structs
 * declare no nested field either -- so the state has to be derived. The state is
 * ordinary enough in principle (the options are `serde_json::Value` on the wire)
 * and the failure it guards against is silent: an empty text box over a list is
 * an offer to convert it on the next keystroke.
 */
test.describe("an option value the form cannot show is one it must not rewrite", () => {
  let copy: ProductCopy;
  let unknownKey: ProductCopy;

  test.beforeEach(() => {
    copy = copyProduct(REPO, "payments-demo", "provider-complex", (text) =>
      text.replace('schema = "cluster"', 'schema = ["a", "b"]'),
    );
    unknownKey = copyProduct(REPO, "payments-demo", "provider-stray", (text) =>
      text.replace('schema = "cluster",', 'schema = "cluster",\n                pool_maximum_size = 10,'),
    );
  });

  test.afterEach(() => {
    copy?.dispose();
    unknownKey?.dispose();
  });

  test("a list written under a scalar option is refused by the control and survives a write beside it [ADR-0023 §2.4: nothing is silently converted]", async ({
    freshStudio,
  }) => {
    const { page } = freshStudio;
    await openProductById(page, copy.id, "prod");
    await productSection(page, "topology");
    const form = page.locator('[data-provider-options="event-broker/cache"]');
    await expect(form).toBeVisible({ timeout: 60_000 });

    const written = form.locator('[data-config-field="schema"]');
    // Set -- so not "a value somebody must supply" -- and refused, so there is
    // no box offering to replace it.
    await expect(written).toHaveAttribute("data-config-provenance", "explicit");
    await expect(written).toContainText("not a scalar");
    await expect(written.locator("input")).toHaveCount(0);

    // A write beside it leaves it exactly as the description has it.
    await form.locator('[data-config-field="pool_max_size"] input').fill("21");
    await form.locator("[data-provider-options-head]").click();
    await page.locator("[data-draft-apply]").click();
    const dialog = page.locator(".dialogBlock");
    await dialog.waitFor({ state: "visible", timeout: 60_000 });
    await dialog.locator("button.theia-button.main").click();

    await expect
      .poll(() => readFileSync(copy.path, "utf8"), { timeout: 60_000 })
      .toContain("pool_max_size = 21");
    expect(
      readFileSync(copy.path, "utf8"),
      "the value the form could not render was rewritten",
    ).toContain('schema = ["a", "b"]');
  });

  test("a key the backend does not read is shown with what it says, not dropped [ADR-0023 §2.4: nothing is silently converted]", async ({
    freshStudio,
  }) => {
    // The form renders the struct's fields, so a key that is not one of them
    // would otherwise be invisible on the only screen that shows this binding:
    // written in the description, refused by the engine, and absent here.
    const { page } = freshStudio;
    await openProductById(page, unknownKey.id, "prod");
    await productSection(page, "topology");
    const form = page.locator('[data-provider-options="event-broker/cache"]');
    await expect(form).toBeVisible({ timeout: 60_000 });

    const stray = form.locator('[data-provider-option-unknown="pool_maximum_size"]');
    await expect(stray).toBeVisible();
    await expect(stray, "with the value it was given").toContainText("10");
    await expect(stray, "and what to do about it").toContainText("does not read this key");
    // Not a control: nothing here knows what it was meant to be.
    await expect(stray.locator("input")).toHaveCount(0);
  });
});
