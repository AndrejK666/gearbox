//! Steps 1 and 2 of the resolver: profile narrowing and the co-location closure.
//!
//! The closure is where this project's central finding lives, so the tests are
//! about the finding rather than about the traversal: a gear nobody selected is
//! in the product because something reaches it, and the resolver must be able to
//! say which something.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use gearbox_engine::resolve::{closure, resolve};
use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{
    Catalogue, DiagnosticCode, GearId, InclusionReason, ProductIntent, ProfileId, SourceId,
};

#[path = "common/corpus.rs"]
mod corpus_support;

fn gears_rust() -> Option<PathBuf> {
    corpus_support::root()
}

fn catalogue() -> Option<Catalogue> {
    let root = gears_rust()?;
    let source = SourceRoot::open(SourceId::new("gears-rust").unwrap(), root)
        .unwrap_or_else(|error| panic!("the discovered corpus must be readable: {error}"));
    Some(load_catalogue(&[source]).catalogue)
}

fn product() -> Option<ProductIntent> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../products/payments-demo/product.gdl")
        .canonicalize()
        .ok()?;
    gearbox_engine::product::load_product(&path, None).intent
}

macro_rules! require {
    ($cat:ident, $prod:ident) => {
        let (Some($cat), Some($prod)) = (catalogue(), product()) else {
            eprintln!("skipping: ../gears-rust or the product description is not present");
            return;
        };
    };
}

fn gid(s: &str) -> GearId {
    GearId::new(s).unwrap()
}

fn pid(s: &str) -> ProfileId {
    ProfileId::new(s).unwrap()
}

#[test]
fn the_closure_pulls_in_gears_nobody_selected() {
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("dev"));
    assert!(!r.has_errors(), "{:#?}", r.diagnostics);

    let selected: Vec<String> = prod
        .selected_gears
        .iter()
        .map(|s| s.gear.to_string())
        .collect();
    // Not selected, and in the product anyway -- this is the finding.
    for pulled in ["grpc-hub", "types-registry"] {
        assert!(
            !selected.contains(&pulled.to_owned()),
            "{pulled} is selected"
        );
        assert!(
            r.closure.contains(&gid(pulled)),
            "{pulled} should be pulled in by co-location; closure is {:?}",
            r.closure.ids()
        );
    }
}

#[test]
fn every_gear_can_say_why_it_is_here() {
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("dev"));

    for (gear, reasons) in &r.closure.members {
        assert!(
            !reasons.is_empty(),
            "{gear} is in the closure for no reason"
        );
    }

    assert_eq!(
        r.closure.members.get(&gid("api-gateway")),
        Some(&vec![InclusionReason::Selected]),
        "api-gateway is named in the description"
    );

    let grpc_hub = r.closure.members.get(&gid("grpc-hub")).expect("grpc-hub");
    assert!(
        grpc_hub
            .iter()
            .all(|reason| matches!(reason, InclusionReason::ColocatedBy { .. })),
        "grpc-hub is here only because something reaches it: {grpc_hub:?}"
    );
}

#[test]
fn a_gear_reached_twice_keeps_both_reasons() {
    // `authn-resolver` is named in the description *and* reached from
    // `api-gateway`'s co-location. Both reasons matter, and the second is the one
    // that surprises: deleting the `use_gear` line would not remove the gear,
    // because the closure pulls it in regardless. Keeping only the first reason
    // would make the description look load-bearing when it is not.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("dev"));
    let reasons = r
        .closure
        .members
        .get(&gid("authn-resolver"))
        .expect("authn-resolver is in the closure");

    assert!(
        reasons.contains(&InclusionReason::Selected),
        "it is named in the description: {reasons:?}"
    );
    assert!(
        reasons.contains(&InclusionReason::ColocatedBy {
            gear: gid("api-gateway")
        }),
        "and api-gateway reaches it anyway: {reasons:?}"
    );

    let mut sorted = reasons.clone();
    sorted.sort();
    assert_eq!(&sorted, reasons, "reasons must be sorted for a stable lock");
}

#[test]
fn resolving_twice_gives_the_same_answer() {
    require!(cat, prod);
    let a = resolve(&cat, &prod, &pid("dev"));
    let b = resolve(&cat, &prod, &pid("dev"));
    assert_eq!(a.closure.members, b.closure.members);
    assert_eq!(
        a.diagnostics.as_slice().len(),
        b.diagnostics.as_slice().len()
    );
}

#[test]
fn only_plugin_selections_make_the_closure_depend_on_the_profile() {
    // The premise this test used to assert -- that the closure is identical for
    // every profile -- was wrong, and hid a real bug: plugin selections are
    // profile-scoped, and the closure did not seed from them at all, so
    // `static-authn-plugin` was in no product and the generated host ran with no
    // authn plugin linked.
    //
    // The corrected statement is narrower and still worth pinning. Co-location
    // *edges* are link-time and no profile can change them; only the *seeds*
    // differ, and only by which plugin each profile chose. Everything else must
    // match, or something has made placement leak into composition.
    require!(cat, prod);
    let dev = resolve(&cat, &prod, &pid("dev")).closure.ids();
    let local = resolve(&cat, &prod, &pid("local")).closure.ids();
    let production = resolve(&cat, &prod, &pid("prod")).closure.ids();

    let plugins = |ids: &BTreeSet<GearId>| -> BTreeSet<GearId> {
        ids.iter()
            .filter(|g| g.as_str().ends_with("-plugin"))
            .cloned()
            .collect()
    };
    assert_eq!(
        plugins(&dev),
        [gid("static-authn-plugin")].into_iter().collect(),
        "dev selects the static plugin"
    );
    assert_eq!(
        plugins(&local),
        [gid("static-authn-plugin")].into_iter().collect(),
        "local takes the static plugin too: it is a one-machine development \
         profile, and the OIDC plugin cannot start without a `jwt` section no \
         description can supply"
    );
    assert_eq!(
        plugins(&production),
        [gid("oidc-authn-plugin")].into_iter().collect(),
        "prod selects the OIDC plugin"
    );

    // Strip the plugins and all three must be identical: nothing else about a
    // profile may change what is in the product. Three profiles now differ by
    // their plugin alone, which makes the claim stronger than when two of them
    // happened to agree.
    let without_plugins = |ids: BTreeSet<GearId>| -> BTreeSet<GearId> {
        ids.into_iter()
            .filter(|g| !g.as_str().ends_with("-plugin"))
            .collect()
    };
    let dev = without_plugins(dev);
    assert_eq!(dev, without_plugins(local));
    assert_eq!(dev, without_plugins(production));
}

#[test]
fn a_plugin_says_which_host_selected_it_and_for_which_profile() {
    // "Why is this crate in my binary" is answered by the host, not by the
    // product: the description never named the plugin on its own terms.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("dev"));
    let reasons = r
        .closure
        .members
        .get(&gid("static-authn-plugin"))
        .expect("the plugin is in the product");
    assert!(
        reasons.iter().any(|reason| matches!(
            reason,
            InclusionReason::PluginOf { host, profile }
                if host == &gid("authn-resolver") && profile == &pid("dev")
        )),
        "{reasons:?}"
    );
}

#[test]
fn an_unknown_selected_gear_is_reported_and_the_rest_still_resolves() {
    require!(cat, prod);
    let mut broken = prod;
    broken.selected_gears.push(gearbox_ir::GearSelection {
        gear: gid("no-such-gear"),
        source: SourceId::new("gears-rust").unwrap(),
        version: None,
        package: None,
        features: Vec::new(),
        config: std::collections::BTreeMap::new(),
        plugins: Vec::new(),
        declared_at: None,
    });

    let r = resolve(&cat, &broken, &pid("dev"));
    let codes: Vec<DiagnosticCode> = r.diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&DiagnosticCode::TopologyUnknownGear),
        "expected GBX0301, got {codes:?}"
    );
    assert!(
        r.closure.contains(&gid("api-gateway")),
        "one bad name must not cost the rest of the product"
    );
}

#[test]
fn a_cycle_is_reported_with_the_whole_loop() {
    // Built by hand: no cycle exists in the real tree, and the runtime's own
    // topological sort would refuse one, so this can only be constructed.
    let mut cat = Catalogue::default();
    for (id, deps) in [("a", vec!["b"]), ("b", vec!["c"]), ("c", vec!["a"])] {
        let mut descriptor = support::descriptor(id);
        descriptor.colocated_deps = deps.into_iter().map(gid).collect();
        cat.gears.insert(gid(id), descriptor);
    }
    let intent = support::intent(&["a"]);

    let r = resolve(&cat, &intent, &pid("dev"));
    let cycle = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyDepsCycle)
        .expect("GBX0302");
    for gear in ["a", "b", "c"] {
        assert!(
            cycle.message.contains(gear),
            "the whole loop must be named, not just the closing edge: {}",
            cycle.message
        );
    }
}

#[test]
fn a_self_dependency_is_a_cycle() {
    let mut cat = Catalogue::default();
    let mut descriptor = support::descriptor("a");
    descriptor.colocated_deps = [gid("a")].into_iter().collect();
    cat.gears.insert(gid("a"), descriptor);

    let r = resolve(&cat, &support::intent(&["a"]), &pid("dev"));
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TopologyDepsCycle),
        "{:#?}",
        r.diagnostics
    );
}

#[test]
fn a_diamond_is_not_a_cycle() {
    // a -> {b, c} -> d. Visiting `d` twice is the ordinary case and must not be
    // mistaken for a loop; this is the failure a naive visited-set check makes.
    let mut cat = Catalogue::default();
    for (id, deps) in [
        ("a", vec!["b", "c"]),
        ("b", vec!["d"]),
        ("c", vec!["d"]),
        ("d", vec![]),
    ] {
        let mut descriptor = support::descriptor(id);
        descriptor.colocated_deps = deps.into_iter().map(gid).collect();
        cat.gears.insert(gid(id), descriptor);
    }

    let r = resolve(&cat, &support::intent(&["a"]), &pid("dev"));
    assert!(
        !r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TopologyDepsCycle),
        "a diamond is not a cycle: {:#?}",
        r.diagnostics
    );
    assert_eq!(r.closure.members.len(), 4);
}

#[test]
fn the_closure_is_empty_when_nothing_is_selected() {
    let cat = Catalogue::default();
    let r = resolve(&cat, &support::intent(&[]), &pid("dev"));
    assert!(r.closure.members.is_empty());
    assert!(!r.has_errors());
}

#[test]
fn expand_and_resolve_agree() {
    // `expand` is public so later steps can be tested without running step 1;
    // this pins the two entry points together.
    require!(cat, prod);
    let mut diagnostics = gearbox_ir::Diagnostics::new();
    // The URI `resolve` would build for this intent, so the two entry points are
    // compared on equal terms rather than one of them inventing a different one.
    let uri = gearbox_ir::file_uri(std::path::Path::new(prod.gdl_path.as_str()));
    let direct = closure::expand(&cat, &prod, &pid("dev"), &uri, &mut diagnostics);
    let through = resolve(&cat, &prod, &pid("dev"));
    assert_eq!(direct.members, through.closure.members);
}

#[path = "support/resolve_fixtures.rs"]
mod support;
