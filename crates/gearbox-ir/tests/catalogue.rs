//! Catalogue queries, and above all the co-location closure.
//!
//! The topology in these fixtures is the real one from `gears-rust`:
//!
//! ```text
//! api-gateway          deps = [grpc-hub, authn-resolver]   caps = [rest_host, rest, stateful]
//! grpc-hub             deps = []                           caps = [grpc_hub, stateful, system]
//! authn-resolver       deps = [types-registry]             caps = [system]
//! authz-resolver       deps = [types-registry]             caps = [system]
//! types-registry       deps = []                           caps = [system, rest]
//! payments-audit       deps = [cluster]                    caps = [rest, stateful]
//! cluster              deps = []                           caps = [stateful]
//! api-contracts        deps = []                           caps = [rest]
//! ```
//!
//! The property that matters most: a declared dependency that is absent from the
//! registry is a hard startup failure in `gears-rust`, so co-location is a
//! **downward closure**, not an equivalence relation. Two gears sharing a
//! dependency do not thereby belong in one process, and a shared dependency is
//! linked into *every* process that reaches it. Processes overlap.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers `#[test]` functions but not the \
              helpers in this file; a fixture builder that propagates errors instead of \
              panicking obscures the assertion it exists to support"
)]

use std::collections::{BTreeMap, BTreeSet};

use gearbox_ir::{
    CargoRef, Catalogue, ContractDescriptor, ContractId, ContractKind, ContractVersion,
    DeclaredRole, EndpointDecl, GearDescriptor, GearId, ProviderDescriptor, RelPath,
    ResolvedSource, RuntimeCap, SourceId, SourceKind, Transport, Visibility,
};

fn gid(s: &str) -> GearId {
    GearId::new(s).unwrap()
}

fn source_id() -> SourceId {
    SourceId::new("gears-rust").unwrap()
}

fn gear(id: &str, deps: &[&str], caps: &[RuntimeCap]) -> GearDescriptor {
    let ident = id.replace('-', "_");
    GearDescriptor {
        maturity: gearbox_ir::Maturity::Stable,
        one_per_installation: false,
        id: gid(id),
        display_name: id.to_owned(),
        description: None,
        category: None,
        visibility: Visibility::Internal,
        source: source_id(),
        gdl_path: RelPath::new(format!("gears/{id}/gear.gdl")).unwrap(),
        sdks: Vec::new(),
        implementation: None,
        scenarios: Vec::new(),
        origin: None,
        tests: Vec::new(),
        package: CargoRef::new(format!("cf-gears-{id}"), ident, RelPath::here()),
        runtime_caps: caps.iter().copied().collect(),
        colocated_deps: deps.iter().map(|d| gid(d)).collect(),
        lifecycle: None,
        provides: Vec::new(),
        consumes: Vec::new(),
        requires: Vec::new(),
        serves: Vec::new(),
        client_trait: None,
        cluster_providers: Vec::new(),
        extension_points: Vec::new(),
        implements: None,
        vendor_selector: None,
        declared_roles: Vec::new(),
        available_features: BTreeSet::new(),
        cargo_features: None,
        config_schema: None,
        docs: None,
        gts_types: Vec::new(),
        declared_at: None,
    }
}

/// The slice's real topology.
fn slice() -> Catalogue {
    use RuntimeCap::{Grpc, GrpcHub, Rest, RestHost, Stateful, System};

    let gears = [
        gear(
            "api-gateway",
            &["grpc-hub", "authn-resolver"],
            &[RestHost, Rest, Stateful],
        ),
        gear("grpc-hub", &[], &[GrpcHub, Stateful, System]),
        gear("authn-resolver", &["types-registry"], &[System]),
        gear("authz-resolver", &["types-registry"], &[System]),
        gear("types-registry", &[], &[System, Rest]),
        gear("payments-audit", &["cluster"], &[Rest, Stateful]),
        gear("cluster", &[], &[Stateful]),
        gear("api-contracts", &[], &[Rest]),
        gear("service-discovery", &[], &[Grpc, System, Rest]),
    ];

    Catalogue {
        gears: gears.into_iter().map(|g| (g.id.clone(), g)).collect(),
        contracts: BTreeMap::new(),
        designs: BTreeMap::new(),
        sources: BTreeMap::from([(
            source_id(),
            ResolvedSource {
                id: source_id(),
                kind: SourceKind::Path,
                location: "../../gears-rust".to_owned(),
                digest: "git:8f3c1a9b".to_owned(),
            },
        )]),
        diagnostics: gearbox_ir::Diagnostics::new(),
    }
}

fn names(set: &BTreeSet<GearId>) -> Vec<&str> {
    set.iter().map(GearId::as_str).collect()
}

#[test]
fn closure_pulls_in_transitive_dependencies() {
    let cat = slice();
    // api-gateway -> grpc-hub, authn-resolver -> types-registry
    assert_eq!(
        names(&cat.colocation_closure(&gid("api-gateway"))),
        [
            "api-gateway",
            "authn-resolver",
            "grpc-hub",
            "types-registry"
        ]
    );
}

#[test]
fn closure_of_a_leaf_is_just_itself() {
    let cat = slice();
    assert_eq!(
        names(&cat.colocation_closure(&gid("types-registry"))),
        ["types-registry"]
    );
    assert_eq!(names(&cat.colocation_closure(&gid("cluster"))), ["cluster"]);
}

#[test]
#[expect(
    clippy::similar_names,
    reason = "authn-resolver and authz-resolver are the real gear names, and their \
              near-identity is exactly what this test is about"
)]
fn shared_dependency_does_not_merge_its_dependents() {
    // This is the correction that shapes the whole resolver. `authn-resolver` and
    // `authz-resolver` both require `types-registry`, but that does NOT put them
    // in one process: co-location is a downward closure, not an equivalence
    // relation.
    let cat = slice();
    // The near-identical names are the real ones; the point of the test is that
    // these two gears look alike and share a dependency, yet must not merge.
    let authn_closure = cat.colocation_closure(&gid("authn-resolver"));
    let authz_closure = cat.colocation_closure(&gid("authz-resolver"));

    assert_eq!(names(&authn_closure), ["authn-resolver", "types-registry"]);
    assert_eq!(names(&authz_closure), ["authz-resolver", "types-registry"]);

    assert!(
        !authn_closure.contains(&gid("authz-resolver")),
        "the two must not merge"
    );
    assert!(
        !authz_closure.contains(&gid("authn-resolver")),
        "the two must not merge"
    );

    // And the shared dependency is in BOTH closures, so it is linked into both
    // processes. Processes overlap; they do not partition the gear set.
    let shared: BTreeSet<_> = authn_closure
        .intersection(&authz_closure)
        .cloned()
        .collect();
    assert_eq!(names(&shared), ["types-registry"]);
}

#[test]
fn independent_anchors_have_disjoint_closures() {
    // This disjointness is precisely what makes a two-process topology possible
    // for the slice.
    let cat = slice();
    let gateway = cat.colocation_closure(&gid("api-gateway"));
    let audit = cat.colocation_closure(&gid("payments-audit"));
    assert!(
        gateway.is_disjoint(&audit),
        "gateway {:?} and audit {:?} should share nothing",
        names(&gateway),
        names(&audit)
    );
    assert_eq!(names(&audit), ["cluster", "payments-audit"]);
}

#[test]
fn colocation_is_transitive_and_directional() {
    let cat = slice();
    // Transitive: types-registry is two hops from api-gateway.
    assert!(cat.is_colocated_with(&gid("api-gateway"), &gid("types-registry")));
    // Directional: the dependency does not drag its dependent along.
    assert!(!cat.is_colocated_with(&gid("types-registry"), &gid("api-gateway")));
    // Reflexive: a gear is trivially in its own process.
    assert!(cat.is_colocated_with(&gid("cluster"), &gid("cluster")));
    // Unrelated.
    assert!(!cat.is_colocated_with(&gid("api-gateway"), &gid("payments-audit")));
}

#[test]
fn closure_terminates_on_a_cycle() {
    // The registry's topological sort would reject a cycle, so this should be
    // unreachable -- but a query that hangs on malformed input is worse than one
    // that returns something the resolver can then complain about.
    let mut cat = slice();
    cat.gears
        .get_mut(&gid("cluster"))
        .unwrap()
        .colocated_deps
        .insert(gid("payments-audit"));

    let closure = cat.colocation_closure(&gid("payments-audit"));
    assert_eq!(names(&closure), ["cluster", "payments-audit"]);
}

#[test]
fn closure_tolerates_an_unknown_dependency() {
    // A dangling id is the resolver's problem to report; this query must stay
    // usable so a client can still draw a partial graph.
    let mut cat = slice();
    cat.gears
        .get_mut(&gid("payments-audit"))
        .unwrap()
        .colocated_deps
        .insert(gid("does-not-exist"));

    let closure = cat.colocation_closure(&gid("payments-audit"));
    assert_eq!(
        names(&closure),
        ["cluster", "does-not-exist", "payments-audit"]
    );
}

#[test]
fn closure_of_an_unknown_gear_is_just_that_gear() {
    let cat = slice();
    assert_eq!(names(&cat.colocation_closure(&gid("nope"))), ["nope"]);
}

#[test]
fn singleton_capabilities_are_identified() {
    // The registry permits exactly one REST host and one gRPC hub per process.
    assert!(RuntimeCap::RestHost.is_process_singleton());
    assert!(RuntimeCap::GrpcHub.is_process_singleton());
    for cap in [
        RuntimeCap::Db,
        RuntimeCap::Rest,
        RuntimeCap::Stateful,
        RuntimeCap::System,
        RuntimeCap::Grpc,
    ] {
        assert!(!cap.is_process_singleton(), "{cap} is not a singleton");
    }

    let cat = slice();
    let gateway = cat.gear(&gid("api-gateway")).unwrap();
    assert_eq!(
        gateway.singleton_caps().collect::<Vec<_>>(),
        [RuntimeCap::RestHost]
    );
    assert!(
        cat.gear(&gid("types-registry"))
            .unwrap()
            .singleton_caps()
            .next()
            .is_none()
    );
}

#[test]
fn runtime_capabilities_round_trip_and_name_their_traits() {
    // The spelling must match `#[toolkit::gear(capabilities = [...])]` exactly.
    let expected = [
        (RuntimeCap::Db, "db", "DatabaseCapability"),
        (RuntimeCap::Rest, "rest", "RestApiCapability"),
        (RuntimeCap::RestHost, "rest_host", "ApiGatewayCapability"),
        (RuntimeCap::Stateful, "stateful", "RunnableCapability"),
        (RuntimeCap::System, "system", "SystemCapability"),
        (RuntimeCap::GrpcHub, "grpc_hub", "GrpcHubCapability"),
        (RuntimeCap::Grpc, "grpc", "GrpcServiceCapability"),
    ];
    assert_eq!(
        expected.len(),
        RuntimeCap::ALL.len(),
        "the set is closed at seven"
    );

    for (cap, spelling, asserted) in expected {
        assert_eq!(cap.as_str(), spelling);
        assert_eq!(cap.asserted_trait(), asserted);
        assert_eq!(RuntimeCap::parse(spelling), Some(cap));
        assert_eq!(
            serde_json::to_string(&cap).unwrap(),
            format!("\"{spelling}\"")
        );
    }
    assert_eq!(RuntimeCap::parse("restHost"), None);
    assert_eq!(RuntimeCap::parse("cache"), None);
}

#[test]
fn provider_and_family_lookups() {
    let mut cat = slice();
    let sdk = CargoRef::new("cf-api-contracts-sdk", "api_contracts_sdk", RelPath::here());
    let owner = gid("api-contracts");

    for (path, version) in [("PaymentApi", "v1"), ("PaymentApiV2", "v2")] {
        let id = ContractId::new(format!("api-contracts/PaymentApi@{version}")).unwrap();
        cat.contracts.insert(
            id.clone(),
            ContractDescriptor {
                id: id.clone(),
                owner: owner.clone(),
                base_name: "PaymentApi".to_owned(),
                version: ContractVersion::parse(version).unwrap(),
                kind: ContractKind::Api,
                rust_path: format!("api_contracts_sdk::{path}"),
                sdk: sdk.clone(),
                rest: None,
                grpc: None,
            },
        );
        cat.gears
            .get_mut(&owner)
            .unwrap()
            .provides
            .push(ProviderDescriptor {
                contract: id,
                provider_gear: owner.clone(),
                local_factory: Some("Self::build_local".to_owned()),
                transports: [Transport::Local, Transport::Rest].into_iter().collect(),
                policies: Vec::new(),
            });
    }

    let v1 = ContractId::new("api-contracts/PaymentApi@v1").unwrap();
    let providers = cat.providers_of(&v1);
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, owner);

    // The family lets a mismatch diagnostic list what the provider does offer.
    let family: Vec<&str> = cat
        .contract_family(&v1)
        .iter()
        .map(|c| c.version.declared())
        .collect();
    assert_eq!(family, ["v1", "v2"]);

    // Nothing provides a contract nobody declared.
    let absent = ContractId::new("api-contracts/PaymentApi@v9").unwrap();
    assert!(cat.providers_of(&absent).is_empty());
    assert!(cat.contract_family(&absent).is_empty());
}

#[test]
fn crate_dir_is_already_relative_to_the_source_root() {
    // `CargoRef::path` is resolved once, at merge time, so a consumer neither
    // repeats nor re-fails that resolution. What `gear.gdl` spelled relative to
    // its own directory is not what the catalogue stores.
    let mut g = gear("payments-audit", &[], &[]);
    g.gdl_path = RelPath::new("gears/payments-audit/payments-audit/gear.gdl").unwrap();
    g.package.path = RelPath::new("gears/payments-audit/payments-audit").unwrap();

    assert_eq!(
        g.crate_dir().as_str(),
        "gears/payments-audit/payments-audit"
    );

    // A sibling crate: `path = "../payments-audit-sdk"` in the description
    // resolves to this, which is the shape `RelPath` can actually hold.
    assert_eq!(
        g.gdl_dir()
            .resolve("../payments-audit-sdk")
            .unwrap()
            .as_str(),
        "gears/payments-audit/payments-audit-sdk"
    );
}

#[test]
fn endpoints_distinguish_binding_from_being_mounted() {
    let binds = EndpointDecl {
        name: "rest".to_owned(),
        config_key: Some("bind_addr".to_owned()),
        default_port: Some(8087),
        via: None,
    };
    assert!(binds.binds_own_socket());

    // A gear that only contributes routes is mounted on the process's REST host.
    let mounted = EndpointDecl {
        name: "rest".to_owned(),
        config_key: None,
        default_port: None,
        via: Some("rest_host".to_owned()),
    };
    assert!(!mounted.binds_own_socket());
}

#[test]
fn declared_roles_are_carried_and_a_label_is_flagged() {
    let mut g = gear("event-broker", &[], &[]);
    assert!(!g.declares_unsupported());
    assert!(!g.declares_labels());

    g.declared_roles.push(DeclaredRole {
        name: "dispatcher".to_owned(),
        directory_name: "event-broker".to_owned(),
        labels: BTreeSet::new(),
    });

    // A role on its own asks for nothing this tool cannot write: it is a name.
    assert!(g.declares_unsupported());
    assert!(!g.declares_labels());

    g.declared_roles.push(DeclaredRole {
        name: "ingest".to_owned(),
        directory_name: "event-broker-ingest".to_owned(),
        labels: BTreeSet::from(["shard".to_owned()]),
    });

    // A label is what the generated `oop_http` section has no field for.
    assert!(g.declares_labels());

    let round: GearDescriptor = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
    assert_eq!(round.declared_roles, g.declared_roles);
}

#[test]
fn catalogue_round_trips_and_omits_empty_collections() {
    let cat = slice();
    let json = serde_json::to_string(&cat).unwrap();
    assert_eq!(serde_json::from_str::<Catalogue>(&json).unwrap(), cat);

    // A gear with nothing to say should not carry a dozen empty keys.
    let leaf = serde_json::to_value(cat.gear(&gid("cluster")).unwrap()).unwrap();
    let keys: BTreeSet<&str> = leaf
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        !keys.contains("provides"),
        "empty provides should be omitted"
    );
    assert!(
        !keys.contains("consumes"),
        "empty consumes should be omitted"
    );
    assert!(
        !keys.contains("colocated_deps"),
        "empty colocated_deps should be omitted"
    );
    assert!(keys.contains("runtime_caps"), "cluster has stateful");
}

fn origin(url: &str, path: &str, reference: Option<&str>) -> gearbox_ir::GitOrigin {
    gearbox_ir::GitOrigin {
        url: url.to_owned(),
        path: RelPath::new(path).unwrap(),
        reference: reference.map(str::to_owned),
    }
}

#[test]
fn a_git_origin_links_a_path_beside_the_gear_on_each_host() {
    let github = origin(
        "git@github.com:cf/gears-rust.git",
        "gears/bss/pricing",
        None,
    );
    assert_eq!(
        github.web_url("docs/PRD.md").as_deref(),
        Some("https://github.com/cf/gears-rust/blob/HEAD/gears/bss/pricing/docs/PRD.md")
    );
    // Relative to the gear's directory, so a family `docs/` one level up works.
    assert_eq!(
        github.web_url("../docs/DESIGN.md").as_deref(),
        Some("https://github.com/cf/gears-rust/blob/HEAD/gears/bss/docs/DESIGN.md")
    );
    let gitlab = origin("https://gitlab.example.com/cf/x.git", ".", Some("v1.2.0"));
    assert_eq!(
        gitlab.web_url("tests").as_deref(),
        Some("https://gitlab.example.com/cf/x/-/blob/v1.2.0/tests")
    );
    let bitbucket = origin("https://user@bitbucket.org/cf/x.git", "g", Some("main"));
    assert_eq!(
        bitbucket.web_url("README.md").as_deref(),
        Some("https://bitbucket.org/cf/x/src/main/g/README.md")
    );
    // A path that climbs out of the repository has no link.
    assert_eq!(github.web_url("../../../../x"), None);
}

#[test]
fn the_same_repository_compares_equal_whatever_the_spelling() {
    let spellings = [
        "git@github.com:CF/gears-rust.git",
        "https://github.com/cf/gears-rust",
        "https://github.com/cf/gears-rust.git/",
        "ssh://git@github.com/cf/gears-rust.git",
    ];
    for spelling in spellings {
        assert_eq!(
            gearbox_ir::GitOrigin::normalized_url(spelling),
            "github.com/cf/gears-rust",
            "{spelling}"
        );
    }
}
