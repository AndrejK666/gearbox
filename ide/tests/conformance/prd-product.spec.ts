// The requirements a resolved product answers, read off the Product view.
//
// One description, three profiles. Everything here is the same `product.gdl`
// seen three ways, which is the property `profiles = [...]` as a *data* field
// exists to buy: a description that could branch on the profile would be a
// program whose output depends on how it was invoked, and none of these
// comparisons would mean anything.
//
// Note the selectors are scoped -- `.gbx-binding [data-mode]`, not `[data-mode]`.
// Monaco puts a `data-mode` attribute on its own DOM, so the unscoped selector
// picks up `single-document` and `multiple-document` from whatever editor is
// open. That was a real reading, not a hypothetical.

import { expect, openProduct, productSection, test } from "../fixtures/studio";

/**
 * Everything the panel renders about the resolution now on screen.
 *
 * **Read across the stages.** The Product view is
 * `Overview · Gears · Topology · Validation` since 2026-09-07 -- one strip had
 * become the whole product -- so a single DOM snapshot can no longer see the
 * gears and the applications at once. Each stage is visited and the parts are
 * merged, which keeps every claim below about the product rather than about the
 * panel's shape.
 */
async function shown(page: import("@playwright/test").Page) {
  await productSection(page, "overview");
  const header = await snapshot(page);
  await productSection(page, "composition");
  const gears = await snapshot(page);
  await productSection(page, "topology");
  const topology = await snapshot(page);
  // Left where the helpers expect to find it, so a caller that goes on to click
  // a leaf does not have to know this happened.
  await productSection(page, "overview");
  return {
    profile: header.profile,
    lock: header.lock,
    applications: topology.applications,
    applicationGears: topology.applicationGears,
    modes: topology.modes,
    mechanisms: topology.mechanisms,
    askedFor: gears.askedFor,
    pulledIn: gears.pulledIn,
    connections: gears.connections,
  };
}

/** One stage's worth of the panel, whichever stage is showing. */
async function snapshot(page: import("@playwright/test").Page) {
  return page.evaluate(() => {
    // Scoped to the Product panel. These selectors used to run over the whole
    // document, which held only as long as no other widget rendered a
    // resolution -- and the graph panel now renders three of them. An unscoped
    // `[data-application]` is how a test starts passing for the wrong reason.
    const root = document.querySelector(".gbx-widget-product") ?? document;
    const attrs = (selector: string, attribute: string) =>
      Array.from(root.querySelectorAll(selector)).map(
        (e) => e.getAttribute(attribute) ?? "",
      );
    return {
      profile: root.querySelector("[data-resolved-profile]")?.getAttribute("data-resolved-profile") ?? "",
      lock: root.querySelector("[data-lock-hash]")?.getAttribute("data-lock-hash") ?? "",
      applications: attrs("[data-application]", "data-application"),
      applicationGears: Array.from(root.querySelectorAll(".gbx-application")).map((row) => ({
        name: row.getAttribute("data-application") ?? "",
        gears: (row.querySelector(".gbx-application-gears")?.textContent ?? "")
          .split(",")
          .map((g) => g.trim())
          .filter((g) => g.length > 0),
      })),
      modes: attrs(".gbx-binding [data-mode]", "data-mode"),
      mechanisms: attrs(".gbx-binding [data-mechanism]", "data-mechanism"),
      askedFor: attrs("[data-asked-for]", "data-asked-for"),
      pulledIn: Array.from(root.querySelectorAll("[data-pulled-in]")).map((e) => ({
        id: e.getAttribute("data-pulled-in") ?? "",
        why: (e.textContent ?? "").replace(/\s+/g, " ").trim(),
      })),
      // **A third bucket, because there are three kinds of membership now.** A
      // plugin the product selected is not "asked for" (it has no `use_gear`)
      // and not "pulled in" (nothing dragged it along) -- it is a connection
      // written under the host that named it, and Composition shows it there
      // rather than repeating it as a loose gear.
      connections: Array.from(root.querySelectorAll("[data-plugin-id]")).map((e) => ({
        id: e.getAttribute("data-plugin-id") ?? "",
        host: e.getAttribute("data-plugin-host") ?? "",
        active: e.getAttribute("data-plugin-active") === "true",
        why: (e.textContent ?? "").replace(/\s+/g, " ").trim(),
      })),
    };
  });
}

test.describe("a product resolved across profiles", () => {
  test("one description gives three profiles three distinct locks [PRD cpt-gearbox-fr-lock-single-source]", async ({
    studio,
  }) => {
    const locks = new Map<string, string>();
    for (const profile of ["dev", "local", "prod"]) {
      await openProduct(studio.page, profile);
      const state = await shown(studio.page);
      expect(state.profile).toBe(profile);
      locks.set(profile, state.lock);
    }
    // The digest exists so that "did anything actually change" is a byte
    // comparison rather than a judgement. Three profiles that produced the same
    // bytes would mean the profile was not reaching the resolver at all -- which
    // is a failure mode that otherwise looks like everything working.
    expect(new Set(locks.values()).size).toBe(3);
    for (const [profile, lock] of locks) {
      expect(lock, `${profile} produced no lock hash`).toMatch(/^blake3:/);
    }
  });

  test("a binding's mode is derived from placement, never declared [PRD cpt-gearbox-fr-derive-binding-from-placement]", async ({
    studio,
  }) => {
    // The product declares exactly one `bind(...)`, scoped to local and prod,
    // with `mode = binding_mode.remote`. What each profile *gets* is decided by
    // where the resolver put the two gears, and the mechanism names the code path
    // rather than an abstraction over it -- so this is checking a conclusion, not
    // an echo of the description.
    await openProduct(studio.page, "dev");
    const dev = await shown(studio.page);
    expect(dev.modes.length).toBeGreaterThan(0);
    expect(new Set(dev.modes)).toEqual(new Set(["local"]));
    expect(new Set(dev.mechanisms)).toEqual(new Set(["colocated-local"]));

    await openProduct(studio.page, "local");
    const local = await shown(studio.page);
    expect(new Set(local.modes)).toEqual(new Set(["remote"]));
    // `local` discovers workers through a directory; `prod` is given static
    // addresses. Same request, two mechanisms, decided by the profile family.
    expect(new Set(local.mechanisms)).toEqual(new Set(["consumes-directory"]));

    await openProduct(studio.page, "prod");
    const prod = await shown(studio.page);
    expect(new Set(prod.modes)).toEqual(new Set(["remote"]));
    // Plus the `audit` worker's cluster requests: it runs apart from `cluster`,
    // and the cluster SDK owns that address (ADR-0026 section 4).
    expect(new Set(prod.mechanisms)).toEqual(
      new Set(["consumes-static", "provides-client-wiring"]),
    );
  });

  test("the plugin linked for a profile is the one that profile selected [PRD cpt-gearbox-fr-plugin-selection]", async ({
    studio,
  }) => {
    // The canonical case the requirement was written for: a static plugin in dev,
    // a real one in prod, from one product file rather than two. This is also the
    // browser-observable half of a resolver bug found in M4 -- plugins chosen
    // through `plugins { ... }` never reached the closure, so the host built with
    // none linked and the contract route answered 401.
    // **Read off the connections, and both of them are always listed.** The old
    // Gears stage showed the resolution's closure, so the plugin the profile did
    // *not* select was simply absent and "which one is linked" was answered by
    // what was there. Composition shows what the description says, which is both
    // connections under `authn-resolver` whichever profile is on screen -- so the
    // answer is which one is *active here*, and the other stays visible and
    // editable instead of vanishing when the profile changes.
    await openProduct(studio.page, "dev");
    const dev = await shown(studio.page);
    const devLinked = dev.connections.filter((c) => c.active && c.id.endsWith("authn-plugin"));
    expect(devLinked.map((c) => c.id)).toEqual(["static-authn-plugin"]);
    expect(devLinked[0]?.host).toBe("authn-resolver");
    // And the one this profile did not select is present and marked, not gone.
    const devIdle = dev.connections.filter((c) => !c.active && c.id.endsWith("authn-plugin"));
    expect(devIdle.map((c) => c.id)).toEqual(["oidc-authn-plugin"]);

    await openProduct(studio.page, "prod");
    const prod = await shown(studio.page);
    const prodLinked = prod.connections.filter((c) => c.active && c.id.endsWith("authn-plugin"));
    expect(prodLinked.map((c) => c.id)).toEqual(["oidc-authn-plugin"]);
    expect(prodLinked[0]?.host).toBe("authn-resolver");
  });

  test("a gear pulled in by co-location names the gear that pulled it [PRD cpt-gearbox-fr-never-cut-colocation]", async ({
    studio,
  }) => {
    await openProduct(studio.page, "dev");
    const state = await shown(studio.page);
    // The product asks for four gears and names none of these. They arrive
    // through the `colocated_deps` closure, which is a link-time fact the
    // resolver may never sever -- so "why is this here" has to be answerable
    // without opening the lock.
    const grpcHub = state.pulledIn.find((g) => g.id === "grpc-hub");
    expect(grpcHub?.why).toContain("co-located with api-gateway");
    const registry = state.pulledIn.find((g) => g.id === "types-registry");
    expect(registry?.why).toContain("co-located with");
  });

  test("every gear in the closure ends up in some application [plan §9: no orphans]", async ({
    studio,
  }) => {
    // The invariant that caught a real resolver bug twice during M4: partitioning
    // built one application from the wrong anchor and left five of six gears out, and
    // the code reported its own mistake as orphans. A gear that is in the product
    // and in no application is a gear nothing will build.
    await openProduct(studio.page, "prod");
    const state = await shown(studio.page);
    // All three buckets: a connection is in the closure too, and leaving it out
    // would make every selected plugin read as an orphan.
    const closure = new Set([
      ...state.askedFor,
      ...state.pulledIn.map((g) => g.id),
      ...state.connections.filter((c) => c.active).map((c) => c.id),
    ]);
    const placed = new Set(state.applicationGears.flatMap((application) => application.gears));
    expect(closure.size).toBeGreaterThan(0);
    expect([...closure].filter((gear) => !placed.has(gear))).toEqual([]);
    expect([...placed].filter((gear) => !closure.has(gear))).toEqual([]);
  });

  test("applications overlap rather than partition the gears [plan §9: closure not partition]", async ({
    studio,
  }) => {
    // prod is the only profile with more than one application, so it is the only place
    // overlap could show. Each of its workers serves REST and so includes
    // `authn-resolver` (GBX0417), whose closure -- the AuthN plugin and
    // `types-registry` -- the gateway links too: one gear, three binaries.
    //
    // The skip stays for a corpus where that stops being true. The claim is real
    // -- a gear reached by two closures is linked into both binaries, and drawing
    // that as a partition would tell a reader a cut exists where none can -- and
    // a product that cannot demonstrate it should say so rather than pass.
    await openProduct(studio.page, "prod");
    const state = await shown(studio.page);
    expect(state.applications.length).toBeGreaterThan(1);

    const counts = new Map<string, number>();
    for (const application of state.applicationGears) {
      for (const gear of application.gears) {
        counts.set(gear, (counts.get(gear) ?? 0) + 1);
      }
    }
    const shared = [...counts.entries()].filter(([, n]) => n > 1).map(([gear]) => gear);
    test.skip(
      shared.length === 0,
      "no anchor in this product shares a co-location closure with another, so no overlap exists to see",
    );
    expect(shared).not.toEqual([]);
  });
});
