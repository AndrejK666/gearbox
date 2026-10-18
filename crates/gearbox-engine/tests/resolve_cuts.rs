//! Step 3: which edges a process boundary may run through.
//!
//! The interesting assertions here are about what is *not* reported. Only a
//! declared contract edge is ever severable, and the report of edges that would
//! become severable if declared is only useful if it names pairs the resolver is
//! actually holding together — a list of every unrelated pair would be noise
//! wearing the shape of a work list.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};

use gearbox_engine::resolve::resolve;
use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{
    Catalogue, CutBlocker, DiagnosticCode, GearId, ProductIntent, ProfileId, SourceId,
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
fn the_one_declared_edge_in_the_slice_is_severable() {
    // This is the edge the whole slice was chosen to contain: without at least
    // one genuinely severable declared edge the resolver would be correct and
    // undemonstrable, which is a named risk in the PRD.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("dev"));

    let edges: Vec<(String, String, String)> = r
        .cuts
        .cuttable
        .iter()
        .map(|e| {
            (
                e.consumer.to_string(),
                e.provider.to_string(),
                e.contract.to_string(),
            )
        })
        .collect();

    assert_eq!(
        edges,
        vec![
            (
                "api-contracts-consumer".to_owned(),
                "api-contracts".to_owned(),
                "api-contracts/PaymentApi@v1".to_owned()
            ),
            (
                "api-contracts-consumer".to_owned(),
                "api-contracts".to_owned(),
                "api-contracts/PaymentApi@v2".to_owned()
            ),
        ],
        "both majors are consumed and both are severable"
    );
}

#[test]
fn nothing_in_the_slice_is_severable_if_declared_and_that_is_the_finding() {
    // Empty for *this product*, and the emptiness is information rather than a
    // gap. A `deps` entry becomes convertible only when its target declares a
    // contract, and nothing in the demo product's closure depends on a gear
    // that does.
    //
    // **The good failure this comment predicted has happened, and the
    // prediction was right.** It used to say "exactly one gear provides
    // anything -- `api-contracts` -- which nothing pulls in by co-location",
    // and that a platform gear growing a declared contract would end the
    // emptiness. `authz-resolver` grew one: it provides `AuthZResolverApi` over
    // REST, and `account-management`, `resource-group` and `credstore` each
    // reach it through `deps`.
    //
    // The work list is still empty only because the demo product selects none
    // of those three. So the corpus can now *demonstrate* a severable-if-
    // declared edge -- the central promise it previously could not show -- and
    // the product that shows it is a product nobody has written yet.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("dev"));

    let reported: Vec<String> = r
        .cuts
        .blocked
        .iter()
        .filter(|c| c.blocked_by == CutBlocker::UndeclaredHubEdge)
        .map(|c| format!("{} -> {}", c.consumer, c.provider))
        .collect();
    assert!(reported.is_empty(), "unexpected work list: {reported:?}");

    let providers: Vec<&GearId> = cat
        .gears
        .values()
        .filter(|g| !g.provides.is_empty())
        .map(|g| &g.id)
        .collect();
    assert_eq!(
        providers,
        vec![&gid("api-contracts"), &gid("authz-resolver")],
        "the emptiness above depends on which gears provide anything at all"
    );
}

#[test]
fn an_unrelated_pair_is_never_suggested() {
    // The failure this guards is a report that lists every gear against every
    // contract it does not consume. `api-gateway` has no reason to speak to
    // `api-contracts`, and telling someone to declare that edge is worse than
    // saying nothing: it invents work.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("dev"));
    assert!(
        !r.cuts
            .blocked
            .iter()
            .any(|c| c.consumer == gid("api-gateway") && c.provider == gid("api-contracts")),
        "the two are unrelated; nothing holds them together to release"
    );
}

#[test]
fn a_deps_edge_to_a_provider_is_the_work_list() {
    // The mechanism the real slice cannot demonstrate. `host` keeps `provider` in
    // its process with a `deps` entry -- which is how a gear guarantees a
    // type-keyed hub lookup will find it -- and `provider` declares a
    // remote-capable contract `host` does not consume.
    let cat = support::catalogue_with_provider_as_dep();
    let r = resolve(&cat, &support::intent(&["host"]), &pid("dev"));

    let candidate = r
        .cuts
        .blocked
        .iter()
        .find(|c| c.blocked_by == CutBlocker::UndeclaredHubEdge)
        .expect("the deps edge should be reported");
    assert_eq!(candidate.consumer, gid("host"));
    assert_eq!(candidate.provider, gid("provider"));
    assert!(
        candidate.blocked_by.fixable_by_declaring(),
        "the whole point is that an annotation removes this obstacle"
    );

    let edit = candidate.suggested_edit.as_deref().expect("a literal edit");
    assert!(edit.starts_with("#[toolkit::consumes("), "got: {edit}");
    assert!(edit.contains("from = \"provider\""), "got: {edit}");
    assert!(candidate.file.is_some(), "the edit needs a file to go in");

    let help = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::BindingCuttableIfDeclared)
        .and_then(|d| d.help.clone())
        .expect("GBX0401 carries the edit");
    assert!(
        help.contains("drop `provider` from its `deps`"),
        "declaring alone is not enough -- the deps entry is what forces co-location: {help}"
    );
}

#[test]
fn declaring_the_edge_moves_it_off_the_work_list() {
    // The other half of the previous test: once declared, the pair stops being a
    // suggestion. Without this, the report could be a list nobody can ever clear.
    let cat = support::catalogue_with_declared_edge();
    // Both selected: with no `deps` edge, the provider reaches the product only by
    // being named, and leaving it out turns every case below into GBX0404.
    let r = resolve(&cat, &support::intent(&["host", "provider"]), &pid("dev"));
    assert!(
        !r.cuts
            .blocked
            .iter()
            .any(|c| c.blocked_by == CutBlocker::UndeclaredHubEdge),
        "still on the work list after declaring: {:?}",
        r.cuts.blocked
    );
}

#[test]
fn a_provider_inside_the_closure_is_forced_local() {
    // Declared, but the provider is reachable by co-location, so the runtime's
    // local lookup wins whatever the configuration says. Reported as information,
    // not an error: this is a correct product, just not a separable one.
    let cat = support::catalogue_with_declared_edge_and_dep();
    let r = resolve(&cat, &support::intent(&["host"]), &pid("dev"));

    assert!(r.cuts.cuttable.is_empty(), "{:?}", r.cuts.cuttable);
    let blocker = r.cuts.blocked.first().expect("one blocked edge");
    assert_eq!(blocker.blocked_by, CutBlocker::ColocationClosure);
    assert!(
        !blocker.blocked_by.fixable_by_declaring(),
        "the edge is already declared; declaring cannot help twice"
    );
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::BindingForcedLocal),
        "{:#?}",
        r.diagnostics
    );
}

#[test]
fn a_consumed_contract_with_no_provider_is_an_error() {
    let cat = support::catalogue_missing_provider();
    // Both selected: with no `deps` edge, the provider reaches the product only by
    // being named, and leaving it out turns every case below into GBX0404.
    let r = resolve(&cat, &support::intent(&["host", "provider"]), &pid("dev"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::BindingNoProvider)
        .expect("GBX0404");
    assert!(
        d.help.as_deref().unwrap_or_default().contains("use_gear"),
        "the remedy is to select the provider: {:?}",
        d.help
    );
}

#[test]
fn a_provider_at_the_wrong_major_is_a_different_error() {
    // Exact major equality: parallel majors coexist by design and there is no
    // adapter. "You have v2 and want v1" is a different problem from "nobody
    // provides this", and collapsing them would send the reader to add a gear
    // that is already there.
    let cat = support::catalogue_wrong_major();
    // Both selected: with no `deps` edge, the provider reaches the product only by
    // being named, and leaving it out turns every case below into GBX0404.
    let r = resolve(&cat, &support::intent(&["host", "provider"]), &pid("dev"));

    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::BindingMajorMismatch),
        "{:#?}",
        r.diagnostics
    );
    assert!(
        !r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::BindingNoProvider),
        "a wrong major must not also be reported as absent"
    );
}

#[test]
fn a_provider_without_rest_cannot_carry_a_severed_edge() {
    let cat = support::catalogue_local_only_provider();
    // Both selected: with no `deps` edge, the provider reaches the product only by
    // being named, and leaving it out turns every case below into GBX0404.
    let r = resolve(&cat, &support::intent(&["host", "provider"]), &pid("dev"));

    assert!(r.cuts.cuttable.is_empty());
    assert_eq!(
        r.cuts.blocked.first().map(|c| c.blocked_by),
        Some(CutBlocker::NoRemoteTransport)
    );
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::BindingNoRemoteTransport)
        .expect("GBX0406");
    assert!(
        d.message.contains("offers: local"),
        "naming what it does offer is what makes this actionable: {}",
        d.message
    );
}

#[test]
fn classification_is_stable_across_runs() {
    require!(cat, prod);
    let a = resolve(&cat, &prod, &pid("dev"));
    let b = resolve(&cat, &prod, &pid("dev"));
    assert_eq!(a.cuts.cuttable, b.cuts.cuttable);
    assert_eq!(a.cuts.blocked, b.cuts.blocked);
}

#[path = "support/resolve_fixtures.rs"]
mod support;
