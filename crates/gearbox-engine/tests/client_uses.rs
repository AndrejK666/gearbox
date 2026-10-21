//! GBX0215: a contract a gear's code resolves from the `ClientHub` without
//! declaring it (test case G16 of the service-deployment roadmap).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a setup failure IS the test failure"
)]

use std::collections::{BTreeMap, BTreeSet};

use gearbox_engine::client_use_check::check;
use gearbox_ir::{Catalogue, DiagnosticCode, Diagnostics, GearId};

#[path = "support/resolve_fixtures.rs"]
mod support;

#[path = "common/corpus.rs"]
mod corpus_support;

/// The codes `check` reports when `host` resolves `traits`.
fn codes(catalogue: &Catalogue, traits: &[&str]) -> Vec<DiagnosticCode> {
    let uses: BTreeMap<GearId, BTreeSet<String>> = [(
        support::gid("host"),
        traits.iter().map(|t| (*t).to_owned()).collect(),
    )]
    .into_iter()
    .collect();
    let mut diagnostics = Diagnostics::new();
    check(catalogue, &uses, |_| None, &mut diagnostics);
    diagnostics.iter().map(|d| d.code).collect()
}

fn thing_trait(catalogue: &Catalogue) -> String {
    catalogue
        .contracts
        .values()
        .next()
        .unwrap()
        .trait_ident()
        .to_owned()
}

#[test]
fn a_resolved_contract_nobody_declared_is_reported() {
    let mut catalogue = support::catalogue_with_declared_edge();
    let thing = thing_trait(&catalogue);
    let host = catalogue.gears.get_mut(&support::gid("host")).unwrap();
    host.consumes.clear();
    // Its own crate: calls are answered for per crate, and the fixture puts
    // every gear in one.
    host.package.path = gearbox_ir::RelPath::new("host").unwrap();
    assert_eq!(
        codes(&catalogue, &[&thing]),
        vec![DiagnosticCode::ValidateUndeclaredClientUse]
    );
}

#[test]
fn a_consumed_or_deps_linked_contract_is_declared() {
    let catalogue = support::catalogue_with_declared_edge();
    let thing = thing_trait(&catalogue);
    assert_eq!(codes(&catalogue, &[&thing]), vec![], "consumed");

    let catalogue = support::catalogue_with_provider_as_dep();
    assert_eq!(codes(&catalogue, &[&thing]), vec![], "linked through deps");
}

#[test]
fn a_trait_that_is_no_contract_is_not_a_finding() {
    let catalogue = support::catalogue_with_declared_edge();
    assert_eq!(codes(&catalogue, &["DirectoryClient"]), vec![]);
}

/// On the corpus: the projection sees the consumer's real call.
#[test]
fn the_consumer_crate_resolves_payment_api() {
    let Some(root) = corpus_support::root() else {
        return;
    };
    let files = gearbox_project::scan_crate(
        &root.join("examples/toolkit/api-contracts/api-contracts-consumer"),
    )
    .unwrap();
    let uses = gearbox_project::project_client_uses(&files);
    assert!(uses.contains("PaymentApi"), "{uses:?}");
}
