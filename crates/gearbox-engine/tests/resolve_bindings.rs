//! Step 6: how each severable edge is actually established.
//!
//! The claim these tests exist to check is the project's headline one: **one
//! description, three topologies, and nothing in the description says which**.
//! The same edge is local in `dev`, remote over the directory in `local`, and
//! remote over static wiring in `prod` — derived from where the gears ended up,
//! not from anything anyone wrote.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};

use gearbox_engine::resolve::resolve;
use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{
    BindingMechanism, BindingMode, Catalogue, DiagnosticCode, Discovery, GearId, ProductIntent,
    ProfileId, ResolvedBindingMode, SourceId, Transport,
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

fn codes(r: &gearbox_engine::resolve::Resolution) -> Vec<DiagnosticCode> {
    r.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn one_description_gives_three_different_bindings() {
    // The headline claim, checked on the one edge in the slice that can carry a
    // boundary. Nothing in `product.gdl` says local or remote for `dev`; the
    // answer follows from placement.
    require!(cat, prod);

    let dev = resolve(&cat, &prod, &pid("dev"));
    let v1 = dev
        .bindings
        .iter()
        .find(|b| b.contract.as_str().ends_with("@v1"))
        .expect("the v1 edge");
    assert_eq!(v1.mode, ResolvedBindingMode::Local);
    assert_eq!(v1.transport, Transport::Local);
    assert_eq!(v1.mechanism, BindingMechanism::ColocatedLocal);

    let local = resolve(&cat, &prod, &pid("local"));
    let v1 = local
        .bindings
        .iter()
        .find(|b| b.contract.as_str().ends_with("@v1"))
        .expect("the v1 edge");
    assert_eq!(v1.mode, ResolvedBindingMode::Remote);
    assert_eq!(v1.mechanism, BindingMechanism::ConsumesDirectory);

    let prod_ = resolve(&cat, &prod, &pid("prod"));
    let v1 = prod_
        .bindings
        .iter()
        .find(|b| b.contract.as_str().ends_with("@v1"))
        .expect("the v1 edge");
    assert_eq!(v1.mode, ResolvedBindingMode::Remote);
    assert_eq!(v1.mechanism, BindingMechanism::ConsumesStatic);
}

#[test]
fn a_local_binding_carries_no_endpoint() {
    // Deliberately absent rather than empty. The client hub finds the in-process
    // instance and short-circuits before any resolver runs, so an address here
    // would be a value the runtime never reads — a lie recorded in the lock.
    require!(cat, prod);
    let dev = resolve(&cat, &prod, &pid("dev"));
    for binding in &dev.bindings {
        assert_eq!(binding.mode, ResolvedBindingMode::Local);
        assert!(binding.endpoint_source.is_none(), "{binding:?}");
        assert!(binding.endpoint.is_none(), "{binding:?}");
        assert!(
            !binding.gates_readiness(),
            "a co-located binding has nothing to wait for"
        );
    }
}

#[test]
fn a_remote_binding_names_where_its_address_comes_from() {
    // Directory discovery names a lookup; kubernetes static names a config key
    // *and* pins the Service DNS, because that is the only resolver the runtime
    // has. Pinning a directory address would record a guess as a decision.
    require!(cat, prod);

    let directory = resolve(&cat, &prod, &pid("local"));
    let source = directory.bindings[0]
        .endpoint_source
        .as_deref()
        .expect("a source");
    assert!(source.starts_with("directory:"), "{source}");
    assert!(
        directory.bindings.iter().all(|b| b.endpoint.is_none()),
        "directory discovery must not pin an address the resolver did not decide"
    );

    let static_ = resolve(&cat, &prod, &pid("prod"));
    let source = static_.bindings[0]
        .endpoint_source
        .as_deref()
        .expect("a source");
    assert_eq!(
        source,
        "gears.api-contracts-consumer.config.consumer_wiring.api-contracts"
    );
    let endpoint = static_.bindings[0]
        .endpoint
        .as_deref()
        .expect("kubernetes static pins the Service DNS name");
    assert!(
        endpoint.contains(".svc.cluster.local:"),
        "the pinned address is cluster DNS, not a local spawn: {endpoint}"
    );
}

#[test]
fn two_majors_collapse_to_the_provider_gear_key() {
    // The runtime keys `consumer_wiring` by provider *gear name*, not by
    // contract. `PaymentApi@v1` and `@v2` therefore share one override; the
    // lock used to name them separately and that was a lie -- there is no
    // way to address the two majors independently through this mechanism.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("prod"));
    let keys: Vec<&str> = r
        .bindings
        .iter()
        .filter_map(|b| b.endpoint_source.as_deref())
        .collect();
    assert_eq!(keys.len(), 2, "two severed majors, two bindings: {keys:?}");
    assert!(
        keys.iter()
            .all(|k| *k == "gears.api-contracts-consumer.config.consumer_wiring.api-contracts"),
        "both bindings must name the provider gear, not the contract: {keys:?}"
    );
}

#[test]
fn a_honoured_request_is_recorded_as_honoured() {
    // The description asks for `binding_mode.remote` on v1. It was honoured in
    // `local`, and recording that is what lets Explain distinguish "you got what
    // you asked for" from "the resolver happened to agree".
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("local"));
    let v1 = r
        .bindings
        .iter()
        .find(|b| b.contract.as_str().ends_with("@v1"))
        .expect("v1");
    assert!(!v1.selected.was_downgraded());
    assert!(v1.selected.selected.explicit().is_some(), "asked for");

    let v2 = r
        .bindings
        .iter()
        .find(|b| b.contract.as_str().ends_with("@v2"))
        .expect("v2");
    assert!(v2.selected.selected.is_auto(), "nobody asked about v2");
}

#[test]
fn a_request_scoped_away_from_a_profile_is_not_a_downgrade() {
    // The description asks for remote on v1 with `profiles = ["local", "prod"]`,
    // so in `dev` there is no request at all — step 1 removed it. Recording a
    // downgrade here would invent a disappointment nobody expressed, which is
    // exactly what profile scoping exists to prevent.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("dev"));
    let v1 = r
        .bindings
        .iter()
        .find(|b| b.contract.as_str().ends_with("@v1"))
        .expect("v1");
    assert!(v1.selected.selected.is_auto(), "{:?}", v1.selected);
    assert!(!v1.selected.was_downgraded());
    assert!(
        !r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::BindingForcedLocal),
        "nothing was asked for, so nothing was refused"
    );
}

#[test]
fn a_declared_endpoint_is_refused_rather_than_dropped() {
    // `bind(endpoint = ...)` parses, reaches the IR, and is read by nothing:
    // `requested()` takes `mode` and `transport` and leaves the address behind.
    // Writing an address and being told nothing is the worst of the three
    // available behaviours, so it is refused until there is a model in which an
    // address outside the product means something.
    let cat = support::catalogue_with_declared_edge();
    let mut intent = support::intent(&["host", "provider"]);
    intent.bindings.push(gearbox_ir::BindingIntent {
        consumer: gid("host"),
        contract: gearbox_ir::ContractId::new("provider/Thing@v1").unwrap(),
        mode: BindingMode::Auto,
        transport: None,
        endpoint: Some("http://payments.internal:8080".to_owned()),
        profiles: std::collections::BTreeSet::new(),
        declared_at: None,
    });

    let r = resolve(&cat, &intent, &pid("dev"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::BindingEndpointNotHonoured)
        .expect("GBX0411");
    assert!(
        d.message.contains("payments.internal"),
        "the refusal should quote the address it is refusing: {}",
        d.message
    );
}

#[test]
fn a_binding_without_an_endpoint_is_not_refused() {
    // The negative half: the refusal must fire on the address, not on binding.
    let cat = support::catalogue_with_declared_edge();
    let intent = support::intent(&["host", "provider"]);
    let r = resolve(&cat, &intent, &pid("dev"));
    assert!(
        !r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::BindingEndpointNotHonoured),
        "{:#?}",
        r.diagnostics
    );
}

#[test]
fn asking_for_remote_in_a_single_process_profile_is_recorded_as_a_downgrade() {
    // The mechanism the real product deliberately avoids by scoping its request.
    // Without the record, the lock would show a local binding with no sign that
    // anyone wanted otherwise.
    let cat = support::catalogue_with_declared_edge();
    let mut intent = support::intent(&["host", "provider"]);
    intent.bindings.push(gearbox_ir::BindingIntent {
        consumer: gid("host"),
        contract: gearbox_ir::ContractId::new("provider/Thing@v1").unwrap(),
        mode: BindingMode::Remote,
        transport: None,
        endpoint: None,
        profiles: std::collections::BTreeSet::new(),
        declared_at: None,
    });

    let r = resolve(&cat, &intent, &pid("dev"));
    let binding = r.bindings.first().expect("one binding");
    assert_eq!(
        binding.mode,
        ResolvedBindingMode::Local,
        "one application, so local"
    );
    assert_eq!(
        binding.selected.downgraded_by,
        Some(DiagnosticCode::BindingForcedLocal)
    );
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::BindingForcedLocal)
        .expect("and reported, not only recorded");
    assert!(
        d.help.as_deref().unwrap_or_default().contains("ignored"),
        "the help must say the address would be ignored rather than rejected: {:?}",
        d.help
    );
}

#[test]
fn asking_for_grpc_across_a_boundary_is_downgraded_to_rest() {
    // `#[toolkit::consumes]` emits a REST resolving client and nothing else. The
    // gRPC in the runtime is hand-written wiring the generator does not produce,
    // so a severed edge has no gRPC path at all.
    let cat = support::catalogue_with_declared_edge();
    let mut intent = support::self_hosted(&["host", "provider"], Discovery::Static, Some("t"));
    support::pin(&mut intent, "worker", "provider", 1);
    intent.bindings.push(gearbox_ir::BindingIntent {
        consumer: gid("host"),
        contract: gearbox_ir::ContractId::new("provider/Thing@v1").unwrap(),
        mode: BindingMode::Remote,
        transport: Some(Transport::Grpc),
        endpoint: None,
        profiles: std::collections::BTreeSet::new(),
        declared_at: None,
    });

    let r = resolve(&cat, &intent, &pid("local"));
    let binding = r.bindings.first().expect("one binding");
    assert_eq!(binding.mode, ResolvedBindingMode::Remote);
    assert_eq!(binding.transport, Transport::Rest, "downgraded");
    assert_eq!(
        binding.selected.downgraded_by,
        Some(DiagnosticCode::BindingGrpcUnsupported)
    );
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::BindingGrpcUnsupported)
    );
}

#[test]
fn a_nested_hyphenated_provider_cannot_come_from_the_environment() {
    // The runtime's environment remapping converts underscores to hyphens only in
    // the segment right after the gears prefix, so `api-contracts` nested under
    // `consumer_wiring` can never be matched by a variable. An operator planning
    // to set it at deploy time needs telling. The help names the real key, not
    // a contract-derived one the runtime does not read.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("prod"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::BindingEnvCannotExpressWiring)
        .expect("GBX0409");
    let help = d.help.as_deref().unwrap_or_default();
    assert!(
        help.contains("gears.api-contracts-consumer.config.consumer_wiring.api-contracts"),
        "the key itself has to appear: {help:?}"
    );

    // Directory discovery does not read a wiring key at all, so the warning
    // would be false there.
    let local = resolve(&cat, &prod, &pid("local"));
    assert!(
        !local
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::BindingEnvCannotExpressWiring)
    );
}

#[test]
fn a_single_segment_provider_is_expressible_as_an_environment_variable() {
    // `provider` has no hyphen, so the nested key is one segment and the
    // remapping is not the problem. GBX0409 would be a lie on this shape.
    let cat = support::catalogue_with_declared_edge();
    let mut intent = support::self_hosted(&["host", "provider"], Discovery::Static, Some("t"));
    support::pin(&mut intent, "worker", "provider", 1);
    let r = resolve(&cat, &intent, &pid("local"));
    assert!(
        r.bindings
            .iter()
            .any(|b| b.mechanism == BindingMechanism::ConsumesStatic),
        "the edge must actually be static for this to mean anything: {:?}",
        r.bindings
    );
    assert!(
        !r.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::BindingEnvCannotExpressWiring),
        "a one-word provider is expressible: {:?}",
        r.diagnostics
    );
}

#[test]
fn readiness_gating_follows_criticality_and_placement_together() {
    // `critical` defaults to false and the slice's consumer does not set it, so
    // nothing in the real product gates readiness. Asserting that keeps the
    // default visible: a change to it would show up here rather than as a
    // product that suddenly refuses to become ready.
    require!(cat, prod);
    let r = resolve(&cat, &prod, &pid("prod"));
    assert!(
        r.bindings.iter().all(|b| !b.gates_readiness()),
        "no consumed contract in the slice is declared critical: {:?}",
        r.bindings
    );

    // A critical remote binding does gate; a critical *local* one does not,
    // because there is nothing to wait for.
    let cat = support::catalogue_with_declared_edge();
    let mut intent = support::self_hosted(&["host", "provider"], Discovery::Static, Some("t"));
    support::pin(&mut intent, "worker", "provider", 1);
    let remote = resolve(&cat, &intent, &pid("local"));
    assert!(
        remote
            .bindings
            .iter()
            .all(gearbox_ir::ResolvedBinding::gates_readiness),
        "{:?}",
        remote.bindings
    );

    let local = resolve(&cat, &support::intent(&["host", "provider"]), &pid("dev"));
    assert!(local.bindings.iter().all(|b| !b.gates_readiness()));
}

#[test]
fn bindings_are_stable_across_runs() {
    require!(cat, prod);
    for profile in ["dev", "local", "prod"] {
        let a = resolve(&cat, &prod, &pid(profile));
        let b = resolve(&cat, &prod, &pid(profile));
        assert_eq!(a.bindings, b.bindings, "{profile}");
    }
}

#[path = "support/resolve_fixtures.rs"]
mod support;

#[test]
fn a_selected_provider_that_is_not_the_declared_one_skews_the_wiring_key() {
    // The generated configuration looks right and the override never fires:
    // the generator writes `consumer_wiring.provider`, the runtime reads
    // `consumer_wiring.stranger` -- the name the attribute emitted.
    let cat = support::catalogue_declared_provider_does_not_provide();
    let intent = support::kubernetes(&["host", "provider"], Discovery::Static);
    let r = resolve(&cat, &intent, &pid("prod"));

    let found = codes(&r);
    // The fallback that produces the skew is itself already reported.
    assert!(
        found.contains(&DiagnosticCode::BindingNoProvider),
        "{found:?}"
    );
    let skew = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::BindingWiringKeySkew)
        .expect("GBX0412");
    assert!(
        skew.message.contains("consumer_wiring.provider")
            && skew.message.contains("consumer_wiring.stranger"),
        "both keys belong in the message: {}",
        skew.message
    );
    assert!(
        skew.help
            .as_deref()
            .unwrap_or_default()
            .contains("stranger"),
        "the help names the declared gear: {:?}",
        skew.help
    );
}

#[test]
fn directory_discovery_is_silent_about_the_wiring_key() {
    // What makes the error severity defensible: the key is only read by a
    // statically discovered binding. Under directory discovery the endpoint
    // source is a lookup and no override key exists to be wrong.
    let cat = support::catalogue_declared_provider_does_not_provide();
    let intent = support::kubernetes(&["host", "provider"], Discovery::Directory);
    let r = resolve(&cat, &intent, &pid("prod"));

    assert!(
        !codes(&r).contains(&DiagnosticCode::BindingWiringKeySkew),
        "{:?}",
        codes(&r)
    );
}

#[test]
fn the_declared_provider_being_the_selected_one_is_silent() {
    let cat = support::catalogue_with_declared_edge();
    let intent = support::kubernetes(&["host", "provider"], Discovery::Static);
    let r = resolve(&cat, &intent, &pid("prod"));

    assert!(
        !codes(&r).contains(&DiagnosticCode::BindingWiringKeySkew),
        "the ordinary product must stay quiet: {:?}",
        codes(&r)
    );
}
