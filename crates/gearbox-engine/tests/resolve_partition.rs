//! Step 4: which gears end up in which process.
//!
//! The name *partition* is inherited from the plan and is wrong in the way that
//! matters: a process is the co-location closure of what it holds, and closures
//! overlap, so one gear is routinely linked into several binaries. Several tests
//! here exist only to pin that down, because a resolver that assigned each gear
//! to exactly one process would look tidier and be unable to represent any real
//! product.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use gearbox_engine::resolve::resolve;
use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{
    ApplicationKind, Catalogue, DiagnosticCode, GearId, ProductIntent, ProfileId, SourceId,
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
fn the_embedded_profile_holds_the_whole_product() {
    // Not the anchor's closure: a selected gear that nothing depends on still has
    // to run, and in one process there is nowhere else for it to be. Getting this
    // wrong drops `service-discovery` and `api-contracts` silently.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("dev"));

    assert_eq!(r.partition.applications.len(), 1);
    let application = &r.partition.applications[0];
    let placed: BTreeSet<&GearId> = application.gears.iter().collect();
    let expected: BTreeSet<&GearId> = r.closure.members.keys().collect();
    assert_eq!(placed, expected, "every gear in the product must be in it");
    assert_eq!(application.kind, ApplicationKind::Host);
    assert_eq!(application.rest_host.as_ref(), Some(&gid("api-gateway")));
}

#[test]
fn nothing_is_ever_orphaned() {
    require!(cat, prod);
    for profile in ["dev", "local", "prod"] {
        let r = resolve(&cat, &prod, &pid(profile));
        let orphans: Vec<&str> = r
            .diagnostics
            .iter()
            .filter(|d| d.code == DiagnosticCode::TopologyOrphanGear)
            .map(|d| d.message.as_str())
            .collect();
        assert!(orphans.is_empty(), "{profile}: {orphans:?}");
    }
}

#[test]
fn gears_come_after_everything_they_depend_on() {
    // The order the registry builds them in. A gear before its dependency would
    // produce a binary that fails at startup rather than at generation.
    require!(cat, prod);
    for profile in ["dev", "local", "prod"] {
        let r = resolve(&cat, &prod, &pid(profile));
        for application in &r.partition.applications {
            for (index, gear) in application.gears.iter().enumerate() {
                let Some(descriptor) = cat.gears.get(gear) else {
                    continue;
                };
                for dep in &descriptor.colocated_deps {
                    if let Some(at) = application.gears.iter().position(|g| g == dep) {
                        assert!(
                            at < index,
                            "{profile}/{}: {gear} at {index} precedes its dependency {dep} at {at}",
                            application.name
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_severable_edge_actually_moves_a_gear_out() {
    // The payoff of step 3. `api-contracts` is the provider of the one severable
    // edge in the slice, so a multi-process profile puts it in its own binary --
    // and the same product in `dev` does not.
    require!(cat, prod);

    let dev = resolve(&cat, &prod, &pid("dev"));
    assert_eq!(dev.partition.applications.len(), 1);

    let local = resolve(&cat, &prod, &pid("local"));
    let names: Vec<String> = local
        .partition
        .applications
        .iter()
        .map(|p| p.name.to_string())
        .collect();
    assert_eq!(names, vec!["gateway", "api-contracts"]);

    let worker = &local.partition.applications[1];
    assert_eq!(worker.kind, ApplicationKind::Worker);
    assert_eq!(worker.gears, vec![gid("api-contracts")]);
}

#[test]
fn the_host_process_takes_its_name_from_the_profile() {
    // `host = "gateway"` names a *process*, not a gear, and no gear is called
    // that. Reading it as a gear id sends the anchor search off a cliff and every
    // other gear ends up orphaned.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("local"));
    let host = r.partition.applications.first().expect("a host");
    assert_eq!(host.name.to_string(), "gateway");
    assert_eq!(host.anchor, gid("api-gateway"), "anchored on the REST host");
    assert_eq!(host.bin_name, "gbx-gateway");
}

#[test]
fn a_pinned_process_keeps_its_name_and_replicas() {
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("prod"));
    let audit = r
        .partition
        .applications
        .iter()
        .find(|p| p.name.as_str() == "audit")
        .expect("the pinned application");
    assert_eq!(audit.anchor, gid("api-contracts-consumer"));
    assert_eq!(audit.replicas, 2);
    assert_eq!(audit.kind, ApplicationKind::Worker);
}

#[test]
fn a_pin_only_applies_to_the_profiles_it_names() {
    // `process("audit", ..., profiles = ["prod"])`. In `local` it must not exist,
    // which is step 1 doing its job and step 4 honouring it.
    require!(cat, prod);
    let local = resolve(&cat, &prod, &pid("local"));
    assert!(
        !local
            .partition
            .applications
            .iter()
            .any(|p| p.name.as_str() == "audit"),
        "the pin is scoped to prod"
    );
}

#[test]
fn processes_overlap_and_that_is_correct() {
    // The finding, made concrete. `shared` is a co-location dependency of both
    // `host` and `provider`, and `provider` moves into its own process — so
    // `shared` is linked into both binaries. A partition could not express this.
    let cat = support::catalogue_with_overlap();
    let intent = support::self_hosted_intent(&["host", "provider"]);
    let r = resolve(&cat, &intent, &pid("local"));

    assert_eq!(r.partition.applications.len(), 2, "{:#?}", r.partition);
    let holding: Vec<String> = r
        .partition
        .applications
        .iter()
        .filter(|p| p.contains(&gid("shared")))
        .map(|p| p.name.to_string())
        .collect();
    assert_eq!(holding.len(), 2, "`shared` belongs to both: {holding:?}");
    assert!(
        !r.partition
            .share_an_application(&gid("host"), &gid("provider")),
        "the severable edge did separate them"
    );
    assert!(
        r.partition
            .share_an_application(&gid("host"), &gid("shared")),
        "and `shared` stayed with the host as well"
    );
    assert!(
        r.partition.sole_application(&gid("shared")).is_none(),
        "asking which single application holds an overlapping gear has no answer"
    );
}

#[test]
fn an_embedded_profile_says_what_it_is_declining_to_do() {
    // Two severable edges exist and the profile is using neither. Silence here
    // would read as "nothing was separable", which is a different and more
    // discouraging fact than the true one.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("dev"));
    let note = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyEmbeddedViolation)
        .expect("GBX0307");
    assert!(note.message.contains("severable edge"), "{}", note.message);
    assert!(
        !note.severity.is_error(),
        "a single-application product is still buildable"
    );
}

#[test]
fn an_unknown_profile_is_refused_rather_than_defaulted() {
    // Resolving `embedded` when someone asked for `prod` would produce a
    // plausible product with the wrong topology — worse than producing nothing.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("staging"));
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::GdlUnknownProfile),
        "{:#?}",
        r.diagnostics
    );
    assert!(r.partition.applications.is_empty());
}

#[test]
fn the_partition_is_stable_across_runs() {
    require!(cat, prod);
    for profile in ["dev", "local", "prod"] {
        let a = resolve(&cat, &prod, &pid(profile));
        let b = resolve(&cat, &prod, &pid(profile));
        assert_eq!(
            a.partition.applications, b.partition.applications,
            "{profile}"
        );
    }
}

#[test]
fn kubernetes_fills_the_chart_fields_and_binds_every_interface() {
    // image / subchart / service_port exist only when the profile builds
    // images. BIND_HOST is 0.0.0.0 here because a Service cannot deliver a
    // packet to 127.0.0.1 inside the pod -- and that fact belongs in the lock,
    // not in a template.
    require!(cat, prod);
    let local = resolve(&cat, &prod, &pid("local"));
    for application in &local.partition.applications {
        assert!(application.image.is_none(), "{application:?}");
        assert!(application.subchart.is_none(), "{application:?}");
        assert!(application.service_port.is_none(), "{application:?}");
        for endpoint in &application.listens {
            assert!(
                endpoint.address.starts_with("127.0.0.1:"),
                "{}",
                endpoint.address
            );
        }
    }

    let prod_ = resolve(&cat, &prod, &pid("prod"));
    let gateway = prod_
        .partition
        .applications
        .iter()
        .find(|p| p.anchor.as_str() == "api-gateway")
        .expect("the host");
    let image = gateway
        .image
        .as_ref()
        .expect("a kubernetes profile builds images");
    // Asserted in parts, not as the joined reference: keeping the registry out of
    // `repository` is the whole point, and a test on `reference()` alone would
    // pass just as well with them folded back together.
    assert_eq!(
        image.registry.as_deref(),
        Some("registry.example.com/payments")
    );
    assert_eq!(image.repository, "gbx-api-gateway");
    assert_eq!(image.tag, "0.1.0");
    assert_eq!(
        image.reference(),
        "registry.example.com/payments/gbx-api-gateway:0.1.0"
    );
    assert_eq!(gateway.subchart.as_deref(), Some(gateway.name.as_str()));
    assert_eq!(
        gateway.service_port,
        Some(8087),
        "neighbours dial REST, not the gRPC hub: listens={:?}",
        gateway.listens
    );
    for endpoint in &gateway.listens {
        assert!(
            endpoint.address.starts_with("0.0.0.0:"),
            "{}",
            endpoint.address
        );
        assert!(!endpoint.allow_loopback_advertise);
    }

    let worker = prod_
        .partition
        .applications
        .iter()
        .find(|p| p.is_worker() && p.anchor.as_str() == "api-contracts")
        .expect("the contracts worker");
    let serve = worker.serve.as_ref().expect("a worker serves");
    assert!(!serve.allow_loopback_advertise);
    assert!(
        serve.advertise_uri.contains(&format!(
            "{}.payments.svc.cluster.local:",
            worker.subchart.as_deref().unwrap()
        )),
        "{}",
        serve.advertise_uri
    );
}

#[path = "support/resolve_fixtures.rs"]
mod support;

#[test]
fn a_rest_host_pin_names_the_host_instead_of_creating_a_worker() {
    let cat = support::catalogue_of(vec![
        support::gear_with_caps(
            "gateway",
            &[
                gearbox_ir::RuntimeCap::RestHost,
                gearbox_ir::RuntimeCap::Rest,
            ],
            &[],
        ),
        support::gear_with_caps("events", &[gearbox_ir::RuntimeCap::Rest], &[]),
        support::gear_with_caps("settings", &[gearbox_ir::RuntimeCap::Rest], &[]),
    ]);
    let mut intent = support::kubernetes(
        &["gateway", "events", "settings"],
        gearbox_ir::Discovery::Static,
    );
    support::pin(&mut intent, "demo-gateway", "gateway", 1);
    support::pin(&mut intent, "demo-worker", "settings", 1);
    let result = resolve(&cat, &intent, &pid("prod"));
    assert_eq!(result.partition.applications.len(), 2);
    let host = result
        .partition
        .applications
        .iter()
        .find(|app| app.name.as_str() == "demo-gateway")
        .unwrap();
    assert_eq!(host.kind, ApplicationKind::Host);
    assert!(host.gears.contains(&gid("gateway")));
    assert!(host.gears.contains(&gid("events")));
    let worker = result
        .partition
        .applications
        .iter()
        .find(|app| app.name.as_str() == "demo-worker")
        .unwrap();
    assert_eq!(worker.kind, ApplicationKind::Worker);
    assert!(worker.gears.contains(&gid("settings")));
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TopologyRestHostInWorker)
    );
}

#[test]
fn two_pins_on_one_anchor_are_a_duplicate() {
    // An application is the co-location closure of its anchor, and there is one
    // per anchor: the worker anchors are a set and a pin is looked up rather
    // than filtered for. So a second pin on one anchor describes one process
    // twice, and until now it vanished without a word -- `report_duplicates`
    // keys on the pin's *name*, which these two do not share.
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("host", &[], &[]),
        support::gear_with_caps("moved", &[], &[]),
    ]);
    let mut intent =
        support::self_hosted(&["host", "moved"], gearbox_ir::Discovery::Static, Some("t"));
    support::pin(&mut intent, "first", "moved", 1);
    support::pin(&mut intent, "second", "moved", 4);

    let r = resolve(&cat, &intent, &pid("local"));

    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::GdlDuplicateProfileScoped)
        .expect("GBX0110");
    assert!(
        d.message.contains("moved"),
        "the message names the anchor they collide on: {}",
        d.message
    );

    // And the surviving behaviour, named so a later change to it is visible:
    // one anchor yields one application, the first pin's name and replicas win,
    // and `second`'s `replicas = 4` is dropped.
    let workers: Vec<_> = r
        .partition
        .applications
        .iter()
        .filter(|a| a.kind == gearbox_ir::ApplicationKind::Worker)
        .collect();
    assert_eq!(workers.len(), 1, "one anchor, one application");
    assert_eq!(workers[0].name.as_str(), "first");
    assert_eq!(workers[0].replicas, 1, "the second pin's replicas vanish");
}

#[test]
fn two_pins_differing_by_role_are_not_a_duplicate() {
    // The one way two pins on one anchor are not a contradiction: they name
    // different roles, which is what a role-split gear is for.
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("host", &[], &[]),
        support::gear_with_roles("broker", &["ingest", "delivery"]),
    ]);
    let mut intent = support::self_hosted(
        &["host", "broker"],
        gearbox_ir::Discovery::Static,
        Some("t"),
    );
    support::pin_role(&mut intent, "ingest", "broker", Some("ingest"), 1);
    support::pin_role(&mut intent, "delivery", "broker", Some("delivery"), 1);

    let r = resolve(&cat, &intent, &pid("local"));
    assert!(
        !r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::GdlDuplicateProfileScoped),
        "{:?}",
        r.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>()
    );
}

#[test]
fn an_application_cannot_include_a_gear_outside_the_product() {
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("host", &[], &[]),
        support::gear_with_caps("moved", &[], &[]),
        support::gear_with_caps("unselected", &[], &[]),
    ]);
    let mut intent = support::self_hosted(
        &["host", "moved"],
        gearbox_ir::Discovery::Static,
        Some("target"),
    );
    support::pin(&mut intent, "worker", "moved", 1);
    intent.application_pins[0].include.insert(gid("unselected"));
    let result = resolve(&cat, &intent, &pid("local"));
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::GdlEval && d.message.contains("unselected"))
    );
    assert!(
        !result
            .partition
            .applications
            .iter()
            .any(|app| app.gears.contains(&gid("unselected")))
    );
}

#[test]
fn a_pin_naming_a_role_the_anchor_does_not_declare_is_refused() {
    // The check cannot live in GDL lowering: whether a gear declares a role is
    // a fact about the catalogue, which that layer does not have.
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("host", &[], &[]),
        support::gear_with_roles("broker", &["ingest"]),
    ]);
    let mut intent = support::self_hosted(
        &["host", "broker"],
        gearbox_ir::Discovery::Static,
        Some("t"),
    );
    support::pin_role(&mut intent, "typo", "broker", Some("ingset"), 1);

    let r = resolve(&cat, &intent, &pid("local"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyUnknownRole)
        .expect("a refusal");
    assert!(
        d.message.contains("ingset") && d.message.contains("broker"),
        "{}",
        d.message
    );
}

#[test]
fn the_embedded_application_is_named_after_the_product() {
    // One application that *is* the product, so it carries the product's name
    // rather than its anchor's. The anchor is still recorded, so nothing about
    // which gear owns the router is lost.
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("host", &[gearbox_ir::RuntimeCap::RestHost], &[]),
        support::gear_with_caps("other", &[], &[]),
    ]);
    let intent = support::intent(&["host", "other"]);

    let r = resolve(&cat, &intent, &pid("dev"));
    let application = &r.partition.applications[0];
    assert_eq!(application.name.as_str(), "fixture", "the product's id");
    assert_eq!(application.anchor.as_str(), "host", "the anchor is kept");
    // And the crate follows the name, because that is what it is derived from.
    assert_eq!(application.crate_name, "gbx-fixture");
}

#[test]
fn a_product_id_that_is_not_an_application_name_falls_back_to_the_anchor() {
    // `ApplicationId` is kebab and a product id is a free string, so the two
    // can disagree. An honest degradation rather than a resolution that stops:
    // the same reason `derive_name` returns an `Option` at all.
    let cat = support::catalogue_of(vec![support::gear_with_caps(
        "host",
        &[gearbox_ir::RuntimeCap::RestHost],
        &[],
    )]);
    let mut intent = support::intent(&["host"]);
    intent.id = "Payments Demo".to_owned();

    let r = resolve(&cat, &intent, &pid("dev"));
    assert_eq!(r.partition.applications.len(), 1);
    assert_eq!(r.partition.applications[0].name.as_str(), "host");
}
