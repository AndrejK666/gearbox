//! The merge, against the real `gears-rust` tree.
//!
//! These assert the half of each fact that comes from Rust, on facts that
//! appear in **no** `gear.gdl`. That is the whole point of ADR
//! `cpt-gearbox-adr-macro-projected-catalogue`: if these pass while the
//! descriptions stay silent about capabilities and dependencies, each fact is
//! authored exactly once.
//!
//! Skipped when the sibling checkout is absent, so the suite still runs
//! standalone.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;

use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{Catalogue, DiagnosticCode, GearId, RuntimeCap, Severity, SourceId};

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

macro_rules! require_tree {
    () => {
        match catalogue() {
            Some(c) => c,
            None => {
                eprintln!("skipping: ../gears-rust not present");
                return;
            }
        }
    };
}

fn gid(s: &str) -> GearId {
    GearId::new(s).unwrap()
}

#[test]
fn the_slice_loads_without_unexpected_diagnostics() {
    let catalogue = require_tree!();
    let problems: Vec<String> = catalogue
        .diagnostics
        .iter()
        // A local fork legitimately has a different remote URL. This test checks
        // projection, while the source-origin diagnostic remains visible in CLI.
        .filter(|d| d.code != DiagnosticCode::GdlSourceMismatch)
        // Runtime-gap hints (such as EB's deliberate local Cluster dependency)
        // describe deployment choices rather than malformed source projection.
        .filter(|d| d.severity != Severity::Hint)
        .map(|d| format!("[{}] {}", d.code, d.message))
        .collect();
    assert!(
        problems.is_empty(),
        "unexpected diagnostics:\n{}",
        problems.join("\n")
    );
    // Eight original gears, plus tenant-resolver and the five plugin gears
    // that make the extension-point projection testable on real code, plus the
    // four `platform-host` members described later -- account-management,
    // authz-resolver, resource-group, credstore -- plus `event-broker`, the
    // corpus's one role-split gear -- plus the twenty-five implemented gears
    // described once the model was proven: plugins of described hosts, the
    // license-resolver, usage-collector and bss-rate-provider families, eight
    // plain services, chat-engine, and mini-chat's three -- plus
    // settings-service, described when `gear.toml` was retired -- plus the
    // four that arrived upstream: bss-approvals, bss-products, graph-storage
    // and the clickhouse usage-collector plugin.
    //
    // The assertion above is the one that matters -- every description and
    // not one diagnostic between them. The count is here so that a corpus
    // moving underneath the suite is found in one place rather than inferred
    // from a stranger failure elsewhere.
    assert_eq!(catalogue.gears.len(), 49, "the slice gears");
}

#[test]
fn identity_is_projected_from_the_gear_attribute() {
    // No gear.gdl contains an `id`; every one of these came from
    // #[toolkit::gear(name = "...")].
    let catalogue = require_tree!();
    for expected in [
        "api-gateway",
        "grpc-hub",
        "service-discovery",
        "authn-resolver",
        "types-registry",
        "cluster",
        "api-contracts",
        "api-contracts-consumer",
    ] {
        assert!(
            catalogue.gears.contains_key(&gid(expected)),
            "`{expected}` should have been projected from its gear attribute"
        );
    }
}

#[test]
fn capabilities_and_dependencies_are_projected() {
    let catalogue = require_tree!();
    let gateway = catalogue.gear(&gid("api-gateway")).expect("api-gateway");

    // From `capabilities = [rest_host, rest, stateful]`, in the IR's canonical
    // order rather than declaration order.
    assert_eq!(
        gateway.runtime_caps.iter().copied().collect::<Vec<_>>(),
        [RuntimeCap::Rest, RuntimeCap::RestHost, RuntimeCap::Stateful]
    );

    // From `deps = [grpc_hub, authn_resolver]`, snake to kebab as the macro does.
    assert_eq!(
        gateway
            .colocated_deps
            .iter()
            .map(GearId::as_str)
            .collect::<Vec<_>>(),
        ["authn-resolver", "grpc-hub"]
    );

    // And the closure the whole resolver rests on is therefore real.
    let closure = catalogue.colocation_closure(&gid("api-gateway"));
    assert!(
        closure.contains(&gid("types-registry")),
        "transitively, via authn-resolver"
    );
}

#[test]
fn lifecycle_is_projected_including_its_absences() {
    let catalogue = require_tree!();

    let gateway = catalogue.gear(&gid("api-gateway")).unwrap();
    let lc = gateway
        .lifecycle
        .as_ref()
        .expect("api-gateway has a lifecycle");
    assert_eq!(lc.entry.as_deref(), Some("serve"));
    assert_eq!(lc.stop_timeout.as_deref(), Some("30s"));
    assert!(lc.await_ready, "the bare `await_ready` flag form");

    // grpc-hub declares lifecycle(entry, await_ready) with NO stop_timeout.
    // Projecting a default here would invent a fact.
    let hub = catalogue.gear(&gid("grpc-hub")).unwrap();
    let lc = hub.lifecycle.as_ref().expect("grpc-hub has a lifecycle");
    assert_eq!(lc.stop_timeout, None, "absent, not defaulted");
    assert!(lc.await_ready);

    // **A gear that declares no lifecycle at all**, which is the "absences"
    // half of this claim's name: `None` here must mean the attribute said
    // nothing, not that the projector gave up.
    //
    // This used to name `types-registry`, which gained a lifecycle on
    // 2026-09-21. The claim is about the projection, not about that gear, so it
    // moves to one that still has none -- `authn-resolver`, whose attribute
    // carries `name`, `deps` and `capabilities` and stops there.
    assert!(
        catalogue
            .gear(&gid("authn-resolver"))
            .unwrap()
            .lifecycle
            .is_none(),
        "a gear whose attribute declares no lifecycle must project none"
    );
}

#[test]
fn the_client_trait_is_projected() {
    let catalogue = require_tree!();
    assert_eq!(
        catalogue
            .gear(&gid("service-discovery"))
            .unwrap()
            .client_trait
            .as_deref(),
        Some("cf_system_sdks::directory::DirectoryClient")
    );
    assert!(
        catalogue
            .gear(&gid("api-gateway"))
            .unwrap()
            .client_trait
            .is_none()
    );
}

#[test]
fn every_contract_owner_in_the_corpus_is_a_name_the_catalogue_declares() {
    // The inertness guard for GBX0317. The check that owner names resolve is
    // only worth having if it stays quiet on the tree it will actually see, and
    // the three owners here -- `api-contracts`, `authz-resolver`, `cluster` --
    // are all described. A red here means either a real dangling owner or a
    // check that over-reaches.
    let catalogue = require_tree!();
    let unknown: Vec<&str> = catalogue
        .diagnostics
        .iter()
        .filter(|d| d.code == gearbox_ir::DiagnosticCode::TopologyUnknownContractOwner)
        .map(|d| d.message.as_str())
        .collect();
    assert!(unknown.is_empty(), "{unknown:?}");
}

#[test]
fn contract_identity_is_projected_from_the_contract_attribute() {
    // No gear.gdl states a version or a kind; every major came from
    // #[toolkit::contract(gear, version)] plus the trait-name suffix.
    //
    // `AuthZResolverApi@v1` is the platform's first real contract in this
    // catalogue -- the two `PaymentApi` majors are the example pair. It arrived
    // with `authz-resolver`'s description and is what makes an installation's
    // surface projectable rather than restated: the contract is read out of the
    // SDK, and the description only names the trait as a join key.
    let catalogue = require_tree!();
    let ids: Vec<&str> = catalogue
        .contracts
        .keys()
        .map(gearbox_ir::ContractId::as_str)
        .collect();
    assert_eq!(
        ids,
        [
            "api-contracts/PaymentApi@v1",
            "api-contracts/PaymentApi@v2",
            "authz-resolver/AuthZResolverApi@v1",
        ]
    );

    let v2 = catalogue
        .contracts
        .values()
        .find(|c| c.version.major() == 2)
        .expect("v2");
    assert_eq!(v2.base_name, "PaymentApi", "the trailing major is stripped");
    assert_eq!(
        v2.rust_path, "api_contracts_sdk::PaymentApiV2",
        "but the path keeps it"
    );
    assert_eq!(v2.owner.as_str(), "api-contracts");
    assert!(v2.remote_capable(), "Api kind, from the suffix");
    // The REST projection is declared, so it survives the merge.
    assert_eq!(v2.rest.as_ref().unwrap().base_path, "/api-contracts/v2");
    assert!(v2.rest.as_ref().unwrap().require_full_coverage);
}

#[test]
fn declared_facts_survive_the_merge() {
    let catalogue = require_tree!();
    let gateway = catalogue.gear(&gid("api-gateway")).unwrap();

    assert_eq!(gateway.display_name, "API Gateway");
    assert_eq!(gateway.category.as_deref(), Some("api-ingress"));
    // The endpoint's real config key -- a runtime-configuration fact with no
    // Rust attribute to own it.
    assert_eq!(gateway.serves[0].config_key.as_deref(), Some("bind_addr"));
    assert_eq!(gateway.serves[0].default_port, Some(8087));

    // The lib idents that cannot be derived, because those crates have no [lib].
    assert_eq!(
        catalogue
            .gear(&gid("api-contracts"))
            .unwrap()
            .package
            .lib_ident,
        "cf_api_contracts"
    );
}

#[test]
fn a_multi_gear_crate_needs_an_attr_and_says_so() {
    // mini-chat declares three gears in one crate. Without `attr` the locator
    // must refuse and list the candidates -- guessing would silently project
    // the wrong gear's facts.
    let Some(root) = gears_rust() else {
        eprintln!("skipping: ../gears-rust not present");
        return;
    };
    let crate_dir = root.join("gears/mini-chat/mini-chat");
    let files = gearbox_project::scan_crate(&crate_dir).expect("scan mini-chat");

    let err = gearbox_project::locate_gear_attribute(&files, "mini-chat", None)
        .expect_err("three gears must be ambiguous");
    let message = err.to_string();
    assert!(message.contains("declares 3 gears"), "{message}");
    assert!(
        message.contains("src/gear.rs"),
        "candidates listed: {message}"
    );
    assert!(message.contains("attr ="), "the remedy is named: {message}");

    // Narrowed, it resolves to exactly one.
    let site = gearbox_project::locate_gear_attribute(&files, "mini-chat", Some("src/gear.rs"))
        .expect("narrowing resolves it");
    let projected = gearbox_project::project_gear(&site).expect("parse");
    assert_eq!(projected.name, "mini-chat");
    assert_eq!(projected.struct_ident, "MiniChatGear");
}

#[test]
fn an_attr_that_escapes_the_crate_is_refused() {
    let Some(root) = gears_rust() else { return };
    let files = gearbox_project::scan_crate(&root.join("gears/system/api-gateway")).unwrap();
    let err = gearbox_project::locate_gear_attribute(
        &files,
        "api-gateway",
        Some("../../../etc/passwd.rs"),
    )
    .expect_err("an escaping attr must be refused");
    assert!(err.to_string().contains("escapes the crate root"), "{err}");
}

#[test]
fn the_catalogue_is_deterministic() {
    let a = require_tree!();
    let b = catalogue().expect("second load");
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap(),
        "two loads of the same tree must be byte-identical"
    );
}
