//! Step 5: constraints the runtime imposes, checked before a binary exists.
//!
//! Each of these mirrors something the platform does. Two mirror things it does
//! *silently*, and those are the ones worth having: a REST host inside a worker
//! starts, reports healthy, and answers nothing; a host with directory discovery
//! and no gRPC hub blocks at startup rather than failing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};

use gearbox_engine::resolve::resolve;
use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{
    Catalogue, DiagnosticCode, Discovery, ProductIntent, ProfileId, RuntimeCap, SourceId,
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

fn pid(s: &str) -> ProfileId {
    ProfileId::new(s).unwrap()
}

fn codes(r: &gearbox_engine::resolve::Resolution) -> Vec<DiagnosticCode> {
    r.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn the_real_product_satisfies_every_structural_constraint() {
    let (Some(cat), Some(prod)) = (catalogue(), product()) else {
        eprintln!("skipping: ../gears-rust or the product description is not present");
        return;
    };
    for profile in ["dev", "local", "prod"] {
        let r = resolve(&cat, &prod, &pid(profile));
        let errors: Vec<String> = r
            .diagnostics
            .errors()
            .map(|d| format!("[{}] {}", d.code, d.message))
            .collect();
        assert!(errors.is_empty(), "{profile}: {errors:#?}");
    }
}

#[test]
fn self_hosted_says_it_is_one_machine() {
    // Not a defect in the product: the runtime implements exactly one spawn
    // backend and it starts local processes. Said out loud so a multi-process
    // topology is not mistaken for a distributed one.
    let (Some(cat), Some(prod)) = (catalogue(), product()) else {
        eprintln!("skipping: fixtures not present");
        return;
    };
    let local = resolve(&cat, &prod, &pid("local"));
    assert!(codes(&local).contains(&DiagnosticCode::GapNoRemoteSpawnBackend));
    let spawn = local
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::GapNoRemoteSpawnBackend)
        .expect("GBX0604");
    assert!(
        spawn
            .evidence
            .as_deref()
            .unwrap_or_default()
            .contains("LocalProcessBackend")
            || spawn
                .evidence
                .as_deref()
                .unwrap_or_default()
                .contains("bootstrap/run.rs"),
        "a runtime-gap diagnostic must cite the spawn backend: {:?}",
        spawn.evidence
    );
    // And it points at the `self_hosted(...)` that made it true, not at the top
    // of the file. A diagnostic carrying the whole-file sentinel is not
    // published to the editor at all (`gearbox_rpc::lsp::publishable`), so this
    // is the difference between a squiggle and silence.
    let at = spawn.location.as_ref().expect("GBX0604 carries a location");
    assert_ne!(
        at.range,
        gearbox_ir::Range::whole_file(),
        "the profile declaration carries a span; the diagnostic must use it: {at:?}"
    );
    let declared_on = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../products/payments-demo/product.gdl"),
    )
    .expect("the product this test just resolved");
    let expected = declared_on
        .lines()
        .position(|line| line.contains("self_hosted("))
        .expect("the description declares a self_hosted profile");
    assert_eq!(
        at.range.start.line as usize, expected,
        "must be anchored on the `self_hosted(...)` line"
    );

    // Kubernetes does not spawn at all, so the note would be wrong there.
    let prod_ = resolve(&cat, &prod, &pid("prod"));
    assert!(!codes(&prod_).contains(&DiagnosticCode::GapNoRemoteSpawnBackend));
    assert!(
        codes(&prod_).contains(&DiagnosticCode::GapNoK8sDnsResolver),
        "static discovery across processes is GBX0603: {:?}",
        codes(&prod_)
    );
    let dns = prod_
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::GapNoK8sDnsResolver)
        .expect("GBX0603");
    assert!(
        dns.evidence
            .as_deref()
            .unwrap_or_default()
            .contains("discovery.rs"),
        "GBX0603 must cite the resolver set: {:?}",
        dns.evidence
    );
}

#[test]
fn two_rest_hosts_in_one_application_is_refused() {
    // The registry refuses the second at startup, so this is a binary that does
    // not boot.
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("first", &[RuntimeCap::RestHost], &[]),
        support::gear_with_caps("second", &[RuntimeCap::RestHost], &[]),
    ]);
    let r = resolve(&cat, &support::intent(&["first", "second"]), &pid("dev"));

    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyMultipleRestHost)
        .expect("GBX0303");
    assert!(d.message.contains("first"), "{}", d.message);
    assert!(
        d.message.contains("second"),
        "both offenders must be named, not just the count: {}",
        d.message
    );
}

#[test]
fn two_grpc_hubs_in_one_application_is_refused() {
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("first", &[RuntimeCap::GrpcHub], &[]),
        support::gear_with_caps("second", &[RuntimeCap::GrpcHub], &[]),
    ]);
    let r = resolve(&cat, &support::intent(&["first", "second"]), &pid("dev"));
    assert!(codes(&r).contains(&DiagnosticCode::TopologyMultipleGrpcHub));
}

#[test]
fn a_rest_host_in_a_worker_is_refused() {
    // The worker starts, reports healthy, and never receives a request, because
    // it serves through its own out-of-process router rather than the composed
    // gateway. Silent success is why this is an error.
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("host", &[RuntimeCap::RestHost], &[]),
        support::gear_with_caps("moved", &[RuntimeCap::Rest], &["embedded-host"]),
        support::gear_with_caps("embedded-host", &[RuntimeCap::RestHost], &[]),
    ]);
    let mut intent = support::self_hosted(&["host", "moved"], Discovery::Static, Some("target"));
    support::pin(&mut intent, "worker", "moved", 1);

    let r = resolve(&cat, &intent, &pid("local"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyRestHostInWorker)
        .expect("GBX0312");
    assert!(d.message.contains("worker"), "{}", d.message);
}

#[test]
fn rest_gears_with_no_host_are_refused() {
    let cat = support::catalogue_of(vec![support::gear_with_caps(
        "api",
        &[RuntimeCap::Rest],
        &[],
    )]);
    let r = resolve(&cat, &support::intent(&["api"]), &pid("dev"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyRestWithoutHost)
        .expect("GBX0305");
    assert!(d.message.contains("api"), "{}", d.message);
}

#[test]
fn grpc_gears_with_no_hub_are_refused() {
    // The counterpart of GBX0305, and the one that was missing. The runtime
    // refuses this outright -- `RegistryError::GrpcRequiresHub` -- so without
    // the check a product resolves clean and dies building its registry.
    let cat = support::catalogue_of(vec![support::gear_with_caps(
        "coordinator",
        &[RuntimeCap::Grpc],
        &[],
    )]);
    let r = resolve(&cat, &support::intent(&["coordinator"]), &pid("dev"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyGrpcWithoutHub)
        .expect("GBX0314");
    assert!(d.message.contains("coordinator"), "{}", d.message);
    // It asserts something about the runtime, so it owes the source that proves
    // it -- the same rule GBX0312 and GBX0309 answer to.
    assert!(d.evidence.is_some(), "GBX0314 carries no evidence");
}

#[test]
fn a_grpc_gear_beside_a_hub_is_clean() {
    // The negative half, because a check that never stays quiet is a check that
    // will be switched off.
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("coordinator", &[RuntimeCap::Grpc], &[]),
        support::gear_with_caps("hub", &[RuntimeCap::GrpcHub], &["coordinator"]),
    ]);
    let r = resolve(&cat, &support::intent(&["coordinator", "hub"]), &pid("dev"));
    assert!(
        !r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TopologyGrpcWithoutHub),
        "{:#?}",
        r.diagnostics
    );
}

#[test]
fn a_hub_separated_from_the_gears_it_hosts_is_reported_on_both_sides() {
    // The complaint this answers: isolating `grpc-hub` was accepted in silence
    // and produced a running, empty server. Measured before the check existed --
    // the host's loss was already reported (GBX0314, an error), and the worker
    // holding the lone hub was reported not at all.
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("coordinator", &[RuntimeCap::Grpc], &[]),
        support::gear_with_caps("hub", &[RuntimeCap::GrpcHub], &[]),
    ]);
    let mut intent =
        support::self_hosted(&["coordinator", "hub"], Discovery::Static, Some("target"));
    support::pin(&mut intent, "hubby", "hub", 1);

    let r = resolve(&cat, &intent, &pid("local"));
    let orphaned = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyHostWithNothingToHost)
        .expect("GBX0315");
    assert!(orphaned.message.contains("hubby"), "{}", orphaned.message);
    assert!(orphaned.message.contains("hub"), "{}", orphaned.message);
    // The other side keeps saying what it said: the gears left behind are the
    // half that actually breaks, and they are an error while this is a warning.
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TopologyGrpcWithoutHub),
        "{:#?}",
        r.diagnostics
    );
}

#[test]
fn a_hub_beside_the_gears_it_hosts_is_quiet() {
    // The negative half. A check that never stays quiet gets switched off.
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("coordinator", &[RuntimeCap::Grpc], &[]),
        support::gear_with_caps("hub", &[RuntimeCap::GrpcHub], &["coordinator"]),
    ]);
    let r = resolve(&cat, &support::intent(&["coordinator", "hub"]), &pid("dev"));
    assert!(
        !r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TopologyHostWithNothingToHost),
        "{:#?}",
        r.diagnostics
    );
}

#[test]
fn directory_discovery_without_the_directory_server_is_refused() {
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("host", &[], &[]),
        support::gear_with_caps("grpc-hub", &[RuntimeCap::GrpcHub], &[]),
        support::gear_with_caps("moved", &[], &[]),
    ]);
    let mut intent = support::self_hosted(
        &["host", "grpc-hub", "moved"],
        Discovery::Directory,
        Some("target"),
    );
    support::pin(&mut intent, "worker", "moved", 1);

    let r = resolve(&cat, &intent, &pid("local"));
    assert!(codes(&r).contains(&DiagnosticCode::TopologyNoOrchestrator));
    assert!(
        !codes(&r).contains(&DiagnosticCode::TopologyNoGrpcHub),
        "the hub is present; only the directory server is missing"
    );
}

#[test]
fn directory_discovery_without_the_grpc_hub_is_refused() {
    // The failure this prevents is the least debuggable one available: the spawn
    // phase waits for the hub's endpoint, so the host blocks at startup instead
    // of reporting anything.
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("host", &[], &[]),
        support::gear_with_caps("service-discovery", &[], &[]),
        support::gear_with_caps("moved", &[], &[]),
    ]);
    let mut intent = support::self_hosted(
        &["host", "service-discovery", "moved"],
        Discovery::Directory,
        Some("target"),
    );
    support::pin(&mut intent, "worker", "moved", 1);

    let r = resolve(&cat, &intent, &pid("local"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyNoGrpcHub)
        .expect("GBX0309");
    assert!(
        d.help.as_deref().unwrap_or_default().contains("blocks"),
        "the help has to say what actually happens: {:?}",
        d.help
    );
}

#[test]
fn every_emitted_diagnostic_satisfies_its_invariants() {
    // `Diagnostic::validate` is not called at construction, so a runtime-gap
    // without evidence used to ship (GBX0604). This is the check the PRD
    // required of the emitted set, not of a hand-built fixture.
    let (Some(cat), Some(prod)) = (catalogue(), product()) else {
        eprintln!("skipping: fixtures not present");
        return;
    };
    for profile in ["dev", "local", "prod"] {
        let r = resolve(&cat, &prod, &pid(profile));
        let problems: Vec<String> = r
            .diagnostics
            .iter()
            .flat_map(|d| d.validate().err().unwrap_or_default())
            .collect();
        assert!(
            problems.is_empty(),
            "{profile} emitted diagnostics that fail their own invariants: {problems:#?}"
        );
    }
}

#[test]
fn kubernetes_static_with_one_process_does_not_warn_about_dns() {
    // Nothing to discover, so pinning would be a lie about a topology that
    // never consults an address.
    let cat = support::catalogue_of(vec![support::gear_with_caps("only", &[], &[])]);
    let intent = support::kubernetes(&["only"], Discovery::Static);
    let r = resolve(&cat, &intent, &pid("prod"));
    assert!(!codes(&r).contains(&DiagnosticCode::GapNoK8sDnsResolver));
}

#[test]
fn a_single_process_never_needs_discovery() {
    // Nothing moved out, so nothing is discovered, so the prerequisites do not
    // apply. Reporting them here would be noise on a product that works.
    let cat = support::catalogue_of(vec![support::gear_with_caps("only", &[], &[])]);
    let intent = support::self_hosted(&["only"], Discovery::Directory, Some("target"));

    let r = resolve(&cat, &intent, &pid("local"));
    assert!(!codes(&r).contains(&DiagnosticCode::TopologyNoOrchestrator));
    assert!(!codes(&r).contains(&DiagnosticCode::TopologyNoGrpcHub));
    assert!(!codes(&r).contains(&DiagnosticCode::GapNoRemoteSpawnBackend));
}

#[test]
fn a_worker_with_no_target_dir_has_no_path() {
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("host", &[], &[]),
        support::gear_with_caps("moved", &[], &[]),
    ]);
    let mut intent = support::self_hosted(&["host", "moved"], Discovery::Static, None);
    support::pin(&mut intent, "worker", "moved", 1);

    let r = resolve(&cat, &intent, &pid("local"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyNoTargetDir)
        .expect("GBX0310");
    assert!(d.message.contains("worker"), "{}", d.message);
}

#[test]
fn target_dir_is_only_needed_when_something_moved() {
    let cat = support::catalogue_of(vec![support::gear_with_caps("only", &[], &[])]);
    let intent = support::self_hosted(&["only"], Discovery::Static, None);
    let r = resolve(&cat, &intent, &pid("local"));
    assert!(!codes(&r).contains(&DiagnosticCode::TopologyNoTargetDir));
}

#[path = "support/resolve_fixtures.rs"]
mod support;

#[test]
fn a_gear_with_two_roles_is_told_only_one_is_deployed() {
    // The claim GBX0601 used to make at load time and blame the runtime for.
    // It is a product fact: one application per anchor gear, so the second
    // role has nowhere to be -- and a gear whose roles nobody selects says
    // nothing at all, which is why this moved out of the catalogue.
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("host", &[], &[]),
        support::gear_with_roles("broker", &["ingest", "delivery"]),
    ]);
    let intent = support::self_hosted(&["host", "broker"], Discovery::Static, Some("t"));
    let r = resolve(&cat, &intent, &pid("local"));

    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyRolesNotDeployable)
        .expect("GBX0318");
    assert!(
        d.message.contains("broker") && d.message.contains("2 roles"),
        "{}",
        d.message
    );
    assert!(
        d.help.as_deref().unwrap_or_default().contains("this tool"),
        "the help says where the limitation is: {:?}",
        d.help
    );
}

#[test]
fn one_role_is_deployable_and_says_nothing() {
    let cat = support::catalogue_of(vec![
        support::gear_with_caps("host", &[], &[]),
        support::gear_with_roles("broker", &["ingest"]),
    ]);
    let intent = support::self_hosted(&["host", "broker"], Discovery::Static, Some("t"));
    let r = resolve(&cat, &intent, &pid("local"));

    assert!(
        !r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TopologyRolesNotDeployable),
        "one role is one application"
    );
}

#[test]
fn a_gear_the_host_reaches_and_a_pin_forces_out_is_registered_twice() {
    // Counted on registration, not on linking. `shared` is in the host's
    // closure and registers there as a REST provider; the pin makes it a
    // worker anchor, and the worker registers the same name. A consumer
    // resolving it round-robins between two endpoints.
    let cat = support::catalogue_with_registered_overlap(false);
    let mut intent = support::self_hosted(&["host", "shared"], Discovery::Static, Some("t"));
    support::pin(&mut intent, "shared-out", "shared", 1);

    let r = resolve(&cat, &intent, &pid("local"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyDuplicateRegistration)
        .expect("GBX0320");

    // A warning while nothing says the two are not interchangeable: this is
    // load balancing until a gear says otherwise.
    assert_eq!(d.severity, gearbox_ir::Severity::Warning);
    assert!(
        d.message.contains("shared") && d.message.contains("shared-out"),
        "both applications belong in the message: {}",
        d.message
    );
}

#[test]
fn the_same_collision_is_refused_when_only_one_of_the_gear_may_run() {
    // The same topology, and now the two endpoints own separate state.
    let cat = support::catalogue_with_registered_overlap(true);
    let mut intent = support::self_hosted(&["host", "shared"], Discovery::Static, Some("t"));
    support::pin(&mut intent, "shared-out", "shared", 1);

    let r = resolve(&cat, &intent, &pid("local"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyDuplicateRegistration)
        .expect("GBX0320");
    assert_eq!(d.severity, gearbox_ir::Severity::Error);
    assert!(
        d.message.contains("only one of it may run"),
        "{}",
        d.message
    );
}

#[test]
fn replicating_such_a_gear_is_the_same_defect_by_another_road() {
    // Every replica registers the name from its own process, so this needs no
    // second application at all.
    let cat = support::catalogue_with_registered_overlap(true);
    let mut intent = support::self_hosted(&["host", "shared"], Discovery::Static, Some("t"));
    support::pin(&mut intent, "shared-out", "shared", 3);

    let r = resolve(&cat, &intent, &pid("local"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| {
            d.code == DiagnosticCode::TopologyDuplicateRegistration
                && d.message.contains("replicas")
        })
        .expect("the replica refusal");
    assert_eq!(d.severity, gearbox_ir::Severity::Error);
    assert!(d.message.contains("3 replicas"), "{}", d.message);
}

#[test]
fn under_an_embedded_profile_none_of_this_can_fire() {
    // One application by definition, so there is nowhere for a second
    // registration to come from. The pin is refused as GBX0307 instead, and
    // that is the whole answer.
    let cat = support::catalogue_with_registered_overlap(true);
    let mut intent = support::intent(&["host", "shared"]);
    support::pin(&mut intent, "shared-out", "shared", 3);

    let r = resolve(&cat, &intent, &pid("dev"));
    assert_eq!(
        r.partition.applications.len(),
        1,
        "embedded is one application"
    );
    assert!(
        !codes(&r).contains(&DiagnosticCode::TopologyDuplicateRegistration),
        "{:?}",
        codes(&r)
    );
    assert!(
        codes(&r).contains(&DiagnosticCode::TopologyEmbeddedViolation),
        "the pin is what is reported instead: {:?}",
        codes(&r)
    );
}
