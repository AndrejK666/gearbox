//! Hand-built catalogues and intents for resolver cases the real tree cannot show.
//!
//! Used only for shapes that do not and should not exist in `gears-rust` — a
//! co-location cycle, for instance, which the runtime's own topological sort
//! refuses at startup. Everything the real tree *can* demonstrate is tested
//! against the real tree instead, because a fixture agreeing with itself proves
//! nothing about the platform.

#![allow(dead_code, reason = "each resolver stage uses a different subset")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not helpers"
)]

use std::collections::{BTreeMap, BTreeSet};

use gearbox_ir::{
    CargoRef, GearDescriptor, GearId, GearSelection, ProductIntent, ProfileId, RelPath, SourceId,
    Visibility,
};

pub fn gid(id: &str) -> GearId {
    GearId::new(id).unwrap()
}

pub fn source() -> SourceId {
    SourceId::new("fixture").unwrap()
}

/// The minimum a gear needs to take part in resolution.
///
/// Every optional field is left empty on purpose: a test that needs contracts or
/// capabilities sets them explicitly, so what a case depends on is visible in the
/// case rather than inherited from a fixture.
pub fn descriptor(id: &str) -> GearDescriptor {
    GearDescriptor {
        ui: None,
        maturity: gearbox_ir::Maturity::Stable,
        one_per_installation: false,
        id: gid(id),
        display_name: id.to_owned(),
        description: None,
        category: None,
        visibility: Visibility::default(),
        source: source(),
        gdl_path: RelPath::new(format!("{id}/gear.gdl")).unwrap(),
        sdks: Vec::new(),
        implementation: None,
        scenarios: Vec::new(),
        origin: None,
        tests: Vec::new(),
        package: CargoRef {
            crate_name: format!("cf-{id}"),
            lib_ident: id.replace('-', "_"),
            path: RelPath::new(".").unwrap(),
            features: Vec::new(),
            default_features: true,
            link: Vec::new(),
        },
        runtime_caps: [].into_iter().collect(),
        colocated_deps: [].into_iter().collect(),
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

/// A product selecting exactly `gears`, with one embedded profile.
pub fn intent(gears: &[&str]) -> ProductIntent {
    let dev = ProfileId::new("dev").unwrap();
    ProductIntent {
        ui: None,
        templates: None,
        layout: None,
        id: "fixture".to_owned(),
        display_name: "Fixture".to_owned(),
        version: "0.0.0".to_owned(),
        gdl_path: RelPath::new("product.gdl").unwrap(),
        sources: BTreeMap::new(),
        profiles: [(
            dev.clone(),
            gearbox_ir::DeploymentProfileDecl::Embedded {
                database: None,
                sqlite_databases: BTreeMap::new(),
                id: dev.clone(),
                declared_at: None,
            },
        )]
        .into_iter()
        .collect(),
        default_profile: dev,
        selected_gears: gears
            .iter()
            .map(|g| GearSelection {
                gear: gid(g),
                source: source(),
                version: None,
                package: None,
                features: Vec::new(),
                config: BTreeMap::new(),
                plugins: Vec::new(),
                declared_at: None,
            })
            .collect(),
        bindings: Vec::new(),
        cluster_scopes: Vec::new(),
        application_pins: Vec::new(),
        preferences: Vec::new(),
    }
}

// -------------------------------------------------------------- cut fixtures
//
// Six catalogues for step 3. Each is the smallest shape that exercises one
// branch, because the real slice can demonstrate only two of them: it holds
// exactly one provider and exactly one declared edge.

use gearbox_ir::{
    Catalogue, ContractDescriptor, ContractId, ContractKind, ContractVersion, ProviderDescriptor,
    Requirement, RequirementId, RequirementKind, Transport,
};

fn cid(id: &str) -> ContractId {
    ContractId::new(id).unwrap()
}

/// A contract owned by `provider`, remote-capable unless told otherwise.
fn contract(base: &str, major: u32, kind: ContractKind) -> ContractDescriptor {
    let version = ContractVersion::from_major(major);
    ContractDescriptor {
        id: cid(&format!("provider/{base}@{}", version.declared())),
        owner: gid("provider"),
        base_name: base.to_owned(),
        version,
        kind,
        rust_path: format!("provider_sdk::{base}"),
        sdk: CargoRef {
            crate_name: "cf-provider-sdk".to_owned(),
            lib_ident: "provider_sdk".to_owned(),
            path: RelPath::new("provider-sdk").unwrap(),
            features: Vec::new(),
            default_features: true,
            link: Vec::new(),
        },
        rest: None,
        grpc: None,
    }
}

fn provides(contract: &ContractDescriptor, transports: &[Transport]) -> ProviderDescriptor {
    ProviderDescriptor {
        contract: contract.id.clone(),
        provider_gear: gid("provider"),
        local_factory: Some("Self::build_local".to_owned()),
        transports: transports.iter().copied().collect::<BTreeSet<_>>(),
        policies: Vec::new(),
    }
}

fn consumes(contract: &ContractId) -> Requirement {
    Requirement {
        id: RequirementId::new("host#contract.consumes[0]").unwrap(),
        requester: gid("host"),
        kind: RequirementKind::Contract {
            contract: contract.clone(),
            from: gid("provider"),
            resolving_client: None,
        },
        capabilities: BTreeSet::new(),
        critical: true,
    }
}

/// Assemble a two-gear catalogue from parts.
fn two_gears(
    contracts: Vec<ContractDescriptor>,
    provider_provides: Vec<ProviderDescriptor>,
    host_consumes: Vec<Requirement>,
    host_deps: &[&str],
) -> Catalogue {
    let mut catalogue = Catalogue::default();

    let mut host = descriptor("host");
    host.consumes = host_consumes;
    host.colocated_deps = host_deps.iter().map(|d| gid(d)).collect();
    catalogue.gears.insert(gid("host"), host);

    let mut provider = descriptor("provider");
    provider.provides = provider_provides;
    catalogue.gears.insert(gid("provider"), provider);

    for c in contracts {
        catalogue.contracts.insert(c.id.clone(), c);
    }
    catalogue
}

/// `host` pulls `provider` in with `deps` and declares nothing.
pub fn catalogue_with_provider_as_dep() -> Catalogue {
    let c = contract("Thing", 1, ContractKind::Api);
    let p = provides(&c, &[Transport::Local, Transport::Rest]);
    two_gears(vec![c], vec![p], Vec::new(), &["provider"])
}

/// The same pair with the edge declared and no `deps`.
pub fn catalogue_with_declared_edge() -> Catalogue {
    let c = contract("Thing", 1, ContractKind::Api);
    let p = provides(&c, &[Transport::Local, Transport::Rest]);
    let r = consumes(&c.id);
    two_gears(vec![c], vec![p], vec![r], &[])
}

/// Declared *and* co-located: the local lookup wins whatever the config says.
pub fn catalogue_with_declared_edge_and_dep() -> Catalogue {
    let c = contract("Thing", 1, ContractKind::Api);
    let p = provides(&c, &[Transport::Local, Transport::Rest]);
    let r = consumes(&c.id);
    two_gears(vec![c], vec![p], vec![r], &["provider"])
}

/// `host` consumes a contract nobody provides.
pub fn catalogue_missing_provider() -> Catalogue {
    let c = contract("Thing", 1, ContractKind::Api);
    let r = consumes(&c.id);
    two_gears(vec![c], Vec::new(), vec![r], &[])
}

/// `host` wants v1; `provider` offers v2 of the same family.
pub fn catalogue_wrong_major() -> Catalogue {
    let wanted = contract("Thing", 1, ContractKind::Api);
    let offered = contract("Thing", 2, ContractKind::Api);
    let p = provides(&offered, &[Transport::Local, Transport::Rest]);
    let r = consumes(&wanted.id);
    two_gears(vec![wanted, offered], vec![p], vec![r], &[])
}

/// `host` declares `from = "stranger"`, and `provider` is what actually offers
/// the contract.
///
/// The one shape where the selected provider and the declared one differ:
/// selection falls back to the sole gear that provides it and reports
/// `GBX0404`, and the generated `consumer_wiring` key then names the fallback
/// while the runtime reads the declared name.
pub fn catalogue_declared_provider_does_not_provide() -> Catalogue {
    let c = contract("Thing", 1, ContractKind::Api);
    let p = provides(&c, &[Transport::Local, Transport::Rest]);
    let mut r = consumes(&c.id);
    if let RequirementKind::Contract { from, .. } = &mut r.kind {
        *from = gid("stranger");
    }
    let mut catalogue = two_gears(vec![c], vec![p], vec![r], &[]);
    // Present in the catalogue and providing nothing, so the failure is a
    // wrong `from` rather than an unknown gear.
    catalogue
        .gears
        .insert(gid("stranger"), descriptor("stranger"));
    catalogue
}

/// Declared and remote-capable by kind, but the provider wires up no REST.
pub fn catalogue_local_only_provider() -> Catalogue {
    let c = contract("Thing", 1, ContractKind::Api);
    let p = provides(&c, &[Transport::Local]);
    let r = consumes(&c.id);
    two_gears(vec![c], vec![p], vec![r], &[])
}

/// Two gears sharing a co-located dependency, with a severable edge between them.
///
/// The shape the real slice cannot produce, and the one the whole design exists
/// for: `shared` ends up in **both** binaries, because a process is a closure and
/// closures overlap.
pub fn catalogue_with_overlap() -> Catalogue {
    let c = contract("Thing", 1, ContractKind::Api);
    let mut catalogue = Catalogue::default();

    let mut host = descriptor("host");
    host.consumes = vec![consumes(&c.id)];
    host.colocated_deps = [gid("shared")].into_iter().collect();
    catalogue.gears.insert(gid("host"), host);

    let mut provider = descriptor("provider");
    provider.provides = vec![provides(&c, &[Transport::Local, Transport::Rest])];
    provider.colocated_deps = [gid("shared")].into_iter().collect();
    catalogue.gears.insert(gid("provider"), provider);

    catalogue.gears.insert(gid("shared"), descriptor("shared"));
    catalogue.contracts.insert(c.id.clone(), c);
    catalogue
}

/// A gear the host reaches *and* a pin forces out, so both register it.
///
/// The shape that produces a duplicate directory registration, and the only
/// one: `shared` is a co-location dependency of `host`, so the host's closure
/// holds it and -- carrying the REST capability -- registers it under its own
/// name; a pin makes it a worker anchor as well, and the worker registers the
/// same name. Linking alone would not do this, which is the point.
pub fn catalogue_with_registered_overlap(one_per_installation: bool) -> Catalogue {
    let mut catalogue = Catalogue::default();

    let mut host = gear_with_caps("host", &[RuntimeCap::RestHost], &["shared"]);
    host.one_per_installation = false;
    catalogue.gears.insert(gid("host"), host);

    let mut shared = gear_with_caps("shared", &[RuntimeCap::Rest], &[]);
    shared.one_per_installation = one_per_installation;
    catalogue.gears.insert(gid("shared"), shared);

    catalogue
}

/// `intent`, but for a profile that can hold several processes.
pub fn self_hosted_intent(gears: &[&str]) -> ProductIntent {
    let mut intent = intent(gears);
    let local = ProfileId::new("local").unwrap();
    intent.profiles.insert(
        local.clone(),
        gearbox_ir::DeploymentProfileDecl::SelfHosted {
            database: None,
            sqlite_databases: BTreeMap::new(),
            id: local,
            host: gearbox_ir::ApplicationId::new("gateway").unwrap(),
            discovery: gearbox_ir::Discovery::Static,
            target_dir: None,
            cargo_profile: None,
            declared_at: None,
        },
    );
    intent
}

// ------------------------------------------------------- structural fixtures

use gearbox_ir::RuntimeCap;

/// A gear carrying the given runtime capabilities.
pub fn gear_with_caps(id: &str, caps: &[RuntimeCap], deps: &[&str]) -> GearDescriptor {
    let mut g = descriptor(id);
    g.runtime_caps = caps.iter().copied().collect();
    g.colocated_deps = deps.iter().map(|d| gid(d)).collect();
    g
}

/// Build a catalogue from a list of gears.
pub fn catalogue_of(gears: Vec<GearDescriptor>) -> Catalogue {
    let mut catalogue = Catalogue::default();
    for g in gears {
        catalogue.gears.insert(g.id.clone(), g);
    }
    catalogue
}

/// An intent with one `self_hosted` profile, tunable.
pub fn self_hosted(
    gears: &[&str],
    discovery: gearbox_ir::Discovery,
    target_dir: Option<&str>,
) -> ProductIntent {
    let mut intent = intent(gears);
    let local = ProfileId::new("local").unwrap();
    intent.profiles.insert(
        local.clone(),
        gearbox_ir::DeploymentProfileDecl::SelfHosted {
            database: None,
            sqlite_databases: BTreeMap::new(),
            id: local,
            host: gearbox_ir::ApplicationId::new("host").unwrap(),
            discovery,
            target_dir: target_dir.map(ToOwned::to_owned),
            cargo_profile: None,
            declared_at: None,
        },
    );
    intent
}

/// An intent with one `kubernetes` profile, tunable.
pub fn kubernetes(gears: &[&str], discovery: gearbox_ir::Discovery) -> ProductIntent {
    let mut intent = intent(gears);
    let prod = ProfileId::new("prod").unwrap();
    intent.profiles.insert(
        prod.clone(),
        gearbox_ir::DeploymentProfileDecl::Kubernetes {
            database: None,
            sqlite_databases: BTreeMap::new(),
            id: prod,
            discovery,
            namespace: None,
            image_registry: None,
            declared_at: None,
        },
    );
    intent
}

/// A gear declaring two roles, which one product cannot deploy both of.
pub fn gear_with_roles(id: &str, roles: &[&str]) -> GearDescriptor {
    let mut gear = gear_with_caps(id, &[], &[]);
    gear.declared_roles = roles
        .iter()
        .map(|name| gearbox_ir::DeclaredRole {
            name: (*name).to_owned(),
            directory_name: format!("{id}-{name}"),
            labels: std::collections::BTreeSet::new(),
        })
        .collect();
    gear
}

/// Force a second process by pinning a gear to one.
pub fn pin(intent: &mut ProductIntent, name: &str, anchor: &str, replicas: u32) {
    pin_role(intent, name, anchor, None, replicas);
}

/// The same, naming which of the anchor's roles it deploys.
pub fn pin_role(
    intent: &mut ProductIntent,
    name: &str,
    anchor: &str,
    role: Option<&str>,
    replicas: u32,
) {
    intent.application_pins.push(gearbox_ir::ApplicationPin {
        name: gearbox_ir::ApplicationId::new(name).unwrap(),
        anchor: gid(anchor),
        include: BTreeSet::new(),
        role: role.map(str::to_owned),
        replicas,
        profiles: BTreeSet::new(),
        // No span: this pin was built here, not read from a description. Every
        // diagnostic about it therefore exercises the `Location::or_file`
        // fallback, which is why the anchored cases are tested from text in
        // `tests/diagnostic_spans.rs` instead.
        declared_at: None,
    });
}

// ---------------------------------------------------------- cluster fixtures
//
// The provider table below is the **real** one, projected from the cluster
// gear's `with_*_provider` calls in `gears-rust`. Copied verbatim rather than
// invented, because a fixture that agrees with itself proves nothing about the
// platform — and the awkward parts of it (standalone is process-local, neither
// provider registers leader election) are exactly what the rules are for.

use gearbox_ir::{CapabilityId, ClusterPrimitive, ClusterProviderDecl};

pub fn cap(id: &str) -> CapabilityId {
    CapabilityId::new(id).unwrap()
}

/// `postgres`: cache and lock, linearizable, not process-local, needs credentials.
pub fn postgres() -> ClusterProviderDecl {
    ClusterProviderDecl {
        name: "postgres".to_owned(),
        primitives: [ClusterPrimitive::Cache, ClusterPrimitive::Lock]
            .into_iter()
            .collect(),
        capabilities: [
            (
                ClusterPrimitive::Cache,
                [cap("cluster.cache.linearizable")].into_iter().collect(),
            ),
            (
                ClusterPrimitive::Lock,
                [cap("cluster.lock.linearizable")].into_iter().collect(),
            ),
        ]
        .into_iter()
        .collect(),
        process_local: false,
        needs_credentials: true,
        runtime_determined: BTreeSet::new(),
        options: BTreeMap::new(),
        gated_by: BTreeMap::new(),
        credential_option: None,
    }
}

/// `standalone`: cache only, adds prefix-watch, process-local, no credentials.
pub fn standalone() -> ClusterProviderDecl {
    ClusterProviderDecl {
        name: "standalone".to_owned(),
        primitives: [ClusterPrimitive::Cache].into_iter().collect(),
        capabilities: [(
            ClusterPrimitive::Cache,
            [
                cap("cluster.cache.linearizable"),
                cap("cluster.cache.prefix-watch"),
            ]
            .into_iter()
            .collect(),
        )]
        .into_iter()
        .collect(),
        process_local: true,
        needs_credentials: false,
        runtime_determined: BTreeSet::new(),
        options: BTreeMap::new(),
        gated_by: BTreeMap::new(),
        credential_option: None,
    }
}

/// A backend that decides its capabilities at run time, as `redis` does.
///
/// Copied from the real shape rather than invented: it answers cache and lock,
/// is not process-local, needs credentials, and declares **no** capability
/// because `consistency()` returns what its startup preflight computed.
pub fn runtime_determined_provider() -> ClusterProviderDecl {
    ClusterProviderDecl {
        name: "redis".to_owned(),
        primitives: [ClusterPrimitive::Cache, ClusterPrimitive::Lock]
            .into_iter()
            .collect(),
        capabilities: BTreeMap::new(),
        process_local: false,
        needs_credentials: true,
        runtime_determined: [ClusterPrimitive::Cache, ClusterPrimitive::Lock]
            .into_iter()
            .collect(),
        options: BTreeMap::new(),
        gated_by: BTreeMap::new(),
        credential_option: None,
    }
}

/// A catalogue with the cluster gear and one gear requiring primitives.
pub fn cluster_catalogue(requires: Vec<(ClusterPrimitive, &str, &[&str])>) -> Catalogue {
    let mut catalogue = Catalogue::default();

    let mut cluster = descriptor("cluster");
    cluster.cluster_providers = vec![postgres(), standalone()];
    catalogue.gears.insert(gid("cluster"), cluster);

    let mut app = descriptor("app");
    app.colocated_deps = [gid("cluster")].into_iter().collect();
    app.requires = requires
        .into_iter()
        .enumerate()
        .map(|(n, (primitive, scope, caps))| Requirement {
            id: RequirementId::new(format!("app#cluster.{}[{n}]", primitive.slug())).unwrap(),
            requester: gid("app"),
            kind: RequirementKind::Cluster {
                primitive,
                scope: scope.to_owned(),
            },
            capabilities: caps.iter().map(|c| cap(c)).collect(),
            critical: true,
        })
        .collect();
    catalogue.gears.insert(gid("app"), app);
    catalogue
}

/// Bind a provider for one primitive of a scope.
pub fn bind_cluster(
    intent: &mut ProductIntent,
    scope: &str,
    cache: &str,
    secret_ref: Option<&str>,
) {
    intent.cluster_scopes.push(gearbox_ir::ClusterScopeIntent {
        scope: scope.to_owned(),
        cache: gearbox_ir::ProviderBinding {
            provider: cache.to_owned(),
            options: BTreeMap::new(),
            secret_ref: secret_ref.map(ToOwned::to_owned),
            declared_at: None,
        },
        leader_election: None,
        lock: None,
        profiles: BTreeSet::new(),
        // Built here, not read from a description -- see `pin_role`.
        declared_at: None,
        entry_index: intent.cluster_scopes.len(),
    });
}
