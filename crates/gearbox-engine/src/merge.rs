//! Assembling a [`GearDescriptor`] from its two halves.
//!
//! Under ADR `cpt-gearbox-adr-macro-projected-catalogue` each fact has exactly
//! one author. The Rust attributes own the gear's identity, capabilities,
//! co-location dependencies, lifecycle and client trait, plus the identity,
//! version and kind of every contract. The description owns presentation, the
//! crate reference, transports, projections, endpoints, cluster requirements
//! and criticality. This module is the only place the two meet, which is why
//! the split is legible here rather than smeared across the codebase.
//!
//! Nothing in here *compares* the halves, because there is nothing to compare:
//! that was the old cross-check design, and its diagnostics (GBX0201-0205) are
//! retired.

use std::collections::{BTreeMap, BTreeSet};

use gearbox_gdl::GearDecl;
use gearbox_gdl::engine::FileIdentity;
use gearbox_ir::contract::{GrpcProjection, RestProjection, RestVisibility};
use gearbox_ir::requirement::ClusterPrimitive;
use gearbox_ir::{
    CapabilityId, CargoRef, ContractDescriptor, ContractId, ContractVersion, DeclaredRole,
    Diagnostic, DiagnosticCode, Diagnostics, EndpointDecl, GearDescriptor, GearId, LifecycleDecl,
    Location, PackageRef, ProviderDescriptor, RelPath, Requirement, RequirementId, RequirementKind,
    RuntimeCap, Transport, Visibility,
};
use gearbox_project::{ProjectedContract, ProjectedGear};

/// A gear plus the contracts its description referenced, split by where each
/// came from.
///
/// **Provenance is carried, not re-derived.** The catalogue used to ask
/// "is `contract.owner == declared_by`" to decide whether a copy was the
/// owner's complete one. That question has a wrong answer the moment a
/// contract's `#[toolkit::contract(gear = ...)]` names a role rather than a
/// gear: no declaring gear's id equals the owner, every copy looks like a
/// consumer's partial restatement, and the merge degenerates to whoever was
/// walked first. These two lists are built in two loops below, so the fact is
/// free here and a guess anywhere later.
#[derive(Debug)]
pub struct MergedGear {
    pub gear: GearDescriptor,
    /// Built from this gear's own `provide(...)` records: complete, carrying
    /// the transport projections.
    pub provided: Vec<ContractDescriptor>,
    /// Built from its `consume(...)` records: identity and sdk only, with
    /// `rest` and `grpc` absent rather than empty.
    pub consumed: Vec<ContractDescriptor>,
    /// The traits this gear's own files implement outside tests.
    ///
    /// Not part of the descriptor: it is evidence the catalogue checks a
    /// declared `implements` against once every host is known (GBX0526), and nothing
    /// a client should read a role from.
    pub implemented_traits: std::collections::BTreeSet<String>,
}

/// An error attributable to the description, naming the file and no position.
fn invalid(uri: &str, message: impl Into<String>, help: impl Into<String>) -> Diagnostic {
    invalid_at(Location::file(uri.to_owned()), message, help)
}

/// The same, anchored on the declaration at fault.
fn invalid_at(at: Location, message: impl Into<String>, help: impl Into<String>) -> Diagnostic {
    Diagnostic::error(DiagnosticCode::GdlEval, message, help).at(at)
}

/// Merge the projected and declared halves.
///
/// Returns `None` only when the gear has no usable identity. Anything else is
/// reported and skipped, so one bad contract reference does not hide the rest of
/// the file.
/// Everything projected out of Rust for one gear.
///
/// A struct rather than eight parameters: the list grew once per projected fact,
/// and each addition made the call site harder to read than the thing it was
/// adding. Grouping them also says what they have in common -- every field here
/// was read from source, none from the description.
pub struct Projections<'a> {
    pub gear: &'a ProjectedGear,
    pub contracts_by_trait: &'a BTreeMap<String, ProjectedContract>,
    pub cluster: &'a crate::cluster::ClusterProjection,
    pub plugin: &'a crate::plugin::PluginProjection,
    pub config: Option<gearbox_ir::ConfigSchema>,
    pub docs: Option<gearbox_ir::GearDocs>,
    /// `source = git(...)`, checked against the checkout; see `origin.rs`.
    pub origin: Option<gearbox_ir::GitOrigin>,
    /// `tests = [...]`, found like a declared document; see `docs.rs`.
    pub tests: Vec<gearbox_ir::Located>,
    pub gts_types: Vec<gearbox_ir::GtsTypeDecl>,
    /// The names in the gear crate's `[features]` table.
    pub available_features: std::collections::BTreeSet<String>,
    /// The description's curation of those, checked against them; see
    /// `features.rs`.
    pub cargo_features: Option<Vec<gearbox_ir::CargoFeature>>,
}

#[allow(
    clippy::too_many_lines,
    reason = "one merge of every projected and declared field; splitting hides the inventory"
)]
pub fn merge(
    identity: &FileIdentity,
    decl: &GearDecl,
    projections: Projections<'_>,
    diagnostics: &mut Diagnostics,
) -> Option<MergedGear> {
    let Projections {
        gear: projected,
        contracts_by_trait,
        cluster,
        plugin,
        config,
        docs,
        origin,
        tests,
        gts_types,
        available_features,
        cargo_features,
    } = projections;
    let uri = identity.uri.as_str();

    // Projected: `#[toolkit::gear(name = "...")]`. `GearId::new` enforces
    // exactly the rule the macro enforces, so a name the macro would reject
    // cannot enter the catalogue.
    let id = match GearId::new(&projected.name) {
        Ok(id) => id,
        Err(e) => {
            diagnostics.push(invalid(
                uri,
                format!(
                    "the gear attribute declares name `{}`, which is not a valid gear id: {e}",
                    projected.name
                ),
                "fix `#[toolkit::gear(name = \"...\")]` in the gear's source",
            ));
            return None;
        }
    };

    // The one cross-check the projected catalogue still needs, and it only
    // applies to gears that consume: `#[toolkit::consumes]` derives its
    // endpoint-override config key from the kebab-case of the struct
    // identifier, not from `name`, so a mismatch makes that key unreachable and
    // the runtime only warns. Gears that consume nothing are unaffected --
    // three of the eight slice gears differ here harmlessly.
    if !decl.consumes.is_empty() {
        use heck::ToKebabCase;
        let derived = projected.struct_ident.to_kebab_case();
        if derived != projected.name {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::ValidateConsumerWiringMismatch,
                    format!(
                        "gear `{id}` declares contract consumption, but kebab-case of its struct \
                         `{}` is `{derived}`, not `{}`. #[toolkit::consumes] derives the \
                         consumer_wiring config key from the struct identifier, so the override \
                         key would be unreachable.",
                        projected.struct_ident, projected.name
                    ),
                    format!(
                        "rename the struct to match, or set name = \"{derived}\" in \
                         #[toolkit::gear]"
                    ),
                )
                .at(Location::file(uri.to_owned()))
                .with_evidence("libs/toolkit-contract-macros/src/consumes.rs:183"),
            );
        }
    }

    let Some(package_record) = decl.package.as_ref() else {
        // eval_gear already reported this; defensive.
        return None;
    };
    // `gear()` refuses a description without a maturity, and a design one
    // never reaches projection; defensive, like the package above.
    let maturity = decl.maturity?;
    let scenarios = scenarios(decl);
    report_open_scenarios(uri, &id_hint(decl), decl, &scenarios, diagnostics);
    let gdl_dir = identity.gdl_path.parent();
    let package = cargo_ref(package_record, &gdl_dir, "package", uri, diagnostics);
    let sdks = sdk_refs(decl, &gdl_dir, uri, diagnostics);

    // Projected: the closed set of seven, parsed through the IR's own parser so
    // the spellings cannot drift from the macro's.
    let mut runtime_caps = BTreeSet::new();
    for spelling in &projected.runtime_caps {
        match RuntimeCap::parse(spelling) {
            Some(cap) => {
                runtime_caps.insert(cap);
            }
            None => diagnostics.push(invalid(
                uri,
                format!(
                    "the gear attribute declares capability `{spelling}`, which is not one of the \
                     seven the runtime defines"
                ),
                "fix `capabilities = [...]` in the gear's source",
            )),
        }
    }

    // Projected: link-time co-location. The macro emits a hidden re-export per
    // entry, so a missing dependency is a hard registry error at startup, and
    // the resolver can never sever these edges.
    let mut colocated_deps = BTreeSet::new();
    for dep in &projected.colocated_deps {
        match GearId::new(dep) {
            Ok(dep) => {
                colocated_deps.insert(dep);
            }
            Err(e) => diagnostics.push(invalid(
                uri,
                format!("the gear attribute declares dependency `{dep}`, which is not a valid gear id: {e}"),
                "fix `deps = [...]` in the gear's source",
            )),
        }
    }

    let mut provided_contracts = Vec::new();
    let mut consumed_contracts = Vec::new();
    let mut provides = Vec::new();
    for record in &decl.provides {
        let Some(projected_contract) =
            lookup(&record.contract, contracts_by_trait, uri, diagnostics)
        else {
            continue;
        };
        // Match the gear's own `#[toolkit::provides]` for this contract.
        let offered = projected
            .provides
            .iter()
            .find(|p| p.contract_ident == projected_contract.trait_ident);
        if let Some((provider, contract)) = build_provider(
            uri,
            &id,
            record,
            projected_contract,
            offered,
            &gdl_dir,
            diagnostics,
        ) {
            provides.push(provider);
            provided_contracts.push(contract);
        }
    }

    let mut consumes = Vec::new();
    for (ordinal, record) in decl.consumes.iter().enumerate() {
        let Some(projected_contract) =
            lookup(&record.contract, contracts_by_trait, uri, diagnostics)
        else {
            continue;
        };
        let Some(declared_from) = projected_from(
            uri,
            &id,
            record,
            &projected_contract.trait_ident,
            projected,
            diagnostics,
        ) else {
            continue;
        };
        if let Some((requirement, contract)) = build_consumer(
            uri,
            &id,
            record,
            projected_contract,
            declared_from,
            ordinal,
            &gdl_dir,
            diagnostics,
        ) {
            consumes.push(requirement);
            consumed_contracts.push(contract);
        }
    }

    let mut requires = Vec::new();
    for (ordinal, record) in decl.requires.iter().enumerate() {
        match cluster_requirement(&id, record, ordinal) {
            Ok(requirement) => requires.push(requirement),
            Err(e) => diagnostics.push(invalid(uri, e, "check the cluster requirement")),
        }
    }
    crate::cluster::check_profiles(uri, &id, &requires, cluster, diagnostics);
    report_cluster_colocation(uri, &id, &requires, projected, diagnostics);
    report_unmodelled_arguments(uri, &id, projected, diagnostics);

    // The default is applied here rather than in the constructor because it
    // needs the gear's id, which is projected from Rust and invisible to GDL.
    let declared_roles: Vec<DeclaredRole> = decl
        .declared_roles
        .iter()
        .map(|r| DeclaredRole {
            name: r.name.clone(),
            directory_name: r
                .directory_name
                .clone()
                .unwrap_or_else(|| format!("{id}-{}", r.name)),
            labels: r.labels.iter().cloned().collect(),
        })
        .collect();
    report_front_doors(uri, &id, &declared_roles, diagnostics);
    report_role_names(uri, &id, &decl.declared_roles, &declared_roles, diagnostics);
    report_role_modes(uri, &id, &declared_roles, config.as_ref(), diagnostics);
    report_role_gaps(
        uri,
        &id,
        &declared_roles,
        decl.declared_at.as_ref(),
        diagnostics,
    );

    let category = decl.category.clone();
    report_unknown_category(
        uri,
        &id,
        category.as_deref(),
        decl.declared_at.as_ref(),
        diagnostics,
    );

    let visibility = match decl.visibility.as_deref() {
        None | Some("internal") => Visibility::Internal,
        Some("public") => Visibility::Public,
        Some(other) => {
            diagnostics.push(invalid(
                uri,
                format!("unknown visibility `{other}`"),
                "use \"public\" or \"internal\"",
            ));
            Visibility::Internal
        }
    };

    let gear = GearDescriptor {
        display_name: decl.name.clone().unwrap_or_else(|| id.to_string()),
        id,
        description: decl.description.clone(),
        category,
        visibility,
        maturity,
        implementation: decl.implementation,
        scenarios,
        source: identity.source.clone(),
        gdl_path: identity.gdl_path.clone(),
        package,
        sdks,
        origin,
        tests,
        runtime_caps,
        colocated_deps,
        // Projected.
        lifecycle: projected.lifecycle.as_ref().map(|l| LifecycleDecl {
            entry: l.entry.clone(),
            stop_timeout: l.stop_timeout.clone(),
            await_ready: l.await_ready,
        }),
        provides,
        consumes,
        requires,
        serves: decl
            .serves
            .iter()
            .map(|e| EndpointDecl {
                name: e.name.clone(),
                config_key: e.config_key.clone(),
                default_port: e.default_port,
                via: e.via.clone(),
            })
            .collect(),
        // Projected.
        client_trait: projected.client_trait.clone(),
        // Projected from `provider_registry()` plus the plugin crates
        // `cluster_plugins` locates.
        cluster_providers: cluster.providers.clone(),
        // Declared, and checked against the SDK; see `plugin.rs`. A plugin's
        // `implements.point` is joined to its host after every gear is loaded.
        extension_points: plugin.extension_points.clone(),
        implements: plugin.implements.clone(),
        vendor_selector: plugin.vendor_selector.clone(),
        declared_roles,
        one_per_installation: projected.one_per_installation,
        // Projected from the crate's own `Cargo.toml`, uncurated, beside the
        // description's curation of it; see `features.rs` for why both are kept.
        available_features,
        cargo_features,
        // Declared curation over projected fields; see `config.rs`.
        config_schema: config,
        // Found by convention beside the gear and one level up; see `docs.rs`.
        docs,
        gts_types,
        declared_at: decl.declared_at.clone(),
    };

    Some(MergedGear {
        gear,
        provided: provided_contracts,
        consumed: consumed_contracts,
        implemented_traits: plugin.implemented.clone(),
    })
}

/// The `dep_gear` half of the `consumer_wiring` key, read from the attribute
/// that owns it.
///
/// `#[toolkit::consumes(from = "...")]` is the only statement of which gear a
/// consumption points at. The description used to restate it as
/// `consume(from_ = ...)` and the two could disagree; `from_` is now refused as
/// a restatement, so this is a lookup rather than a comparison.
///
/// The one thing left to report is the attribute's absence. A description that
/// declares a consumption with no attribute behind it emits no registration, so
/// the wiring phase has nothing to replay and the edge is never established --
/// and now there is not even a gear name to resolve against.
fn projected_from<'a>(
    uri: &str,
    id: &GearId,
    record: &gearbox_gdl::records::ConsumeRecord,
    trait_ident: &str,
    projected: &'a ProjectedGear,
    diagnostics: &mut Diagnostics,
) -> Option<&'a str> {
    let found = projected
        .consumes
        .iter()
        .find(|c| c.contract_ident == trait_ident);
    let Some(consumed) = found else {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::ValidateConsumerWiringMismatch,
                format!(
                    "gear `{id}` declares `consume(contract = \"{trait_ident}\")`, and its \
                         Rust carries no `#[toolkit::consumes]` for that contract. No \
                         registration is emitted, so there is nothing for the wiring phase to \
                         replay and the edge is never established."
                ),
                format!(
                    "add `#[toolkit::consumes(contract = {}, from = \"...\")]` beside the \
                         gear attribute, or remove the `consume(...)` record",
                    record.rust
                ),
            )
            .at(Location::or_file(record.declared_at.as_ref(), uri))
            .with_evidence(
                "libs/toolkit-contract-macros/src/consumes.rs \
                 (the registration is emitted by the attribute and by nothing else)",
            ),
        );
        return None;
    };
    Some(consumed.from.as_str())
}

/// Find the projected contract a `provide`/`consume` joins to by trait name.
fn lookup<'a>(
    trait_name: &str,
    contracts: &'a BTreeMap<String, ProjectedContract>,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Option<&'a ProjectedContract> {
    let found = contracts.get(trait_name);
    if found.is_none() {
        let mut known: Vec<&str> = contracts.keys().map(String::as_str).collect();
        known.sort_unstable();
        diagnostics.push(invalid(
            uri,
            format!(
                "no `#[toolkit::contract]` trait named `{trait_name}` was found in the declared \
                 sdk crate(s)"
            ),
            if known.is_empty() {
                "check that `sdk = cargo(...)` points at the crate declaring the contract"
                    .to_owned()
            } else {
                format!("traits found: {}", known.join(", "))
            },
        ));
    }
    found
}

/// Convert a declared `cargo(...)` into a catalogue reference.
///
/// The path is resolved against the description's own directory, exactly as
/// [`crate_dir`] resolves it for scanning, so the catalogue records the crate
/// the engine actually read. `RelPath::new` used to stand here, which rejects
/// `..` -- so a legitimate `path = "../payments-audit-sdk"` was stored as `.`,
/// pointing the catalogue at the description's directory instead of the SDK's.
pub(crate) fn cargo_ref(
    record: &gearbox_gdl::records::CargoRecord,
    gdl_dir: &RelPath,
    field: &str,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> CargoRef {
    let path = match gdl_dir.resolve(&record.path) {
        Ok(path) => path,
        Err(e) => {
            diagnostics.push(bad_crate_path(
                uri,
                field,
                &record.path,
                &e,
                record.declared_at.as_ref(),
            ));
            gdl_dir.clone()
        }
    };
    CargoRef {
        crate_name: record.crate_name.clone(),
        lib_ident: record.lib_ident.clone(),
        path,
        features: record.features.clone(),
        default_features: record.default_features,
        link: if record.link.is_empty() {
            vec![record.lib_ident.clone()]
        } else {
            record.link.clone()
        },
    }
}

/// The declared scenarios, as the catalogue keeps them.
pub(crate) fn scenarios(decl: &GearDecl) -> Vec<gearbox_ir::Scenario> {
    decl.scenarios
        .iter()
        .map(|s| gearbox_ir::Scenario {
            text: s.text.clone(),
            priority: s.priority.clone(),
            done: s.done,
        })
        .collect()
}

/// GBX0122: `implementation = "done"` while a declared scenario is not.
///
/// A warning, because either may be the stale one: the gear may be done and
/// the list not updated, or the status raised too early.
fn report_open_scenarios(
    uri: &str,
    gear: &str,
    decl: &GearDecl,
    scenarios: &[gearbox_ir::Scenario],
    diagnostics: &mut Diagnostics,
) {
    if decl.implementation != Some(gearbox_ir::Implementation::Done) {
        return;
    }
    let open: Vec<&str> = scenarios
        .iter()
        .filter(|s| !s.done)
        .map(|s| s.text.as_str())
        .collect();
    if open.is_empty() {
        return;
    }
    diagnostics.push(
        Diagnostic::new(
            DiagnosticCode::GdlOpenScenarios,
            format!(
                "{gear} says `implementation = \"done\"`, but {} of its scenarios {} not: {}",
                open.len(),
                if open.len() == 1 { "is" } else { "are" },
                open.iter()
                    .map(|t| format!("\"{t}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .at(Location::or_file(decl.declared_at.as_ref(), uri))
        .with_help(
            "mark the written scenarios `done = True`, or say `implementation = \"in-progress\"`",
        ),
    );
}

/// How a diagnostic names a gear before its id is projected.
fn id_hint(decl: &GearDecl) -> String {
    decl.name
        .as_deref()
        .map_or_else(|| "this gear".to_owned(), |name| format!("`{name}`"))
}

/// Every SDK a description declares, as catalogue references: the Rust SDK
/// first, then the others in the order declared.
///
/// A non-Cargo path is resolved exactly like a crate's, and a bad one is
/// reported the same way: it is the same kind of locator.
pub(crate) fn sdk_refs(
    decl: &gearbox_gdl::GearDecl,
    gdl_dir: &RelPath,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Vec<PackageRef> {
    let mut sdks: Vec<PackageRef> = decl
        .sdk
        .iter()
        .map(|record| PackageRef::from(&cargo_ref(record, gdl_dir, "sdk", uri, diagnostics)))
        .collect();
    for record in &decl.other_sdks {
        let path = gdl_dir.resolve(&record.path).unwrap_or_else(|e| {
            diagnostics.push(bad_crate_path(
                uri,
                "sdk",
                &record.path,
                &e,
                record.declared_at.as_ref(),
            ));
            gdl_dir.clone()
        });
        sdks.push(PackageRef {
            ecosystem: record.ecosystem,
            name: record.name.clone(),
            path,
        });
    }
    sdks
}

/// Build the contract id from the projected identity.
fn contract_id(projected: &ProjectedContract) -> Result<(ContractId, ContractVersion), String> {
    let version = ContractVersion::parse(&projected.version).map_err(|e| {
        format!(
            "`#[toolkit::contract(version = \"{}\")]` on `{}` is not a version Gearbox can \
             order: {e}",
            projected.version, projected.trait_ident
        )
    })?;
    let id = ContractId::new(format!(
        "{}/{}@{}",
        projected.gear,
        projected.base_name,
        version.declared()
    ))
    .map_err(|e| {
        format!(
            "cannot build a contract id for `{}`: {e}",
            projected.trait_ident
        )
    })?;
    Ok((id, version))
}

fn build_provider(
    uri: &str,
    owner: &GearId,
    record: &gearbox_gdl::records::ProvideRecord,
    projected: &ProjectedContract,
    offered: Option<&gearbox_project::ProjectedProvide>,
    gdl_dir: &RelPath,
    diagnostics: &mut Diagnostics,
) -> Option<(ProviderDescriptor, ContractDescriptor)> {
    let (id, version) = match contract_id(projected) {
        Ok(parts) => parts,
        Err(e) => {
            diagnostics.push(invalid(
                uri,
                e,
                "fix the contract attribute in the SDK crate",
            ));
            return None;
        }
    };

    // Two different facts, and conflating them is easy: the contract's
    // projection traits say which transports are *possible*, while
    // `#[toolkit::provides(transports = ...)]` says which ones this gear
    // actually wires up. `api-contracts` has `PaymentApiGrpc` yet declares
    // `[local, rest]`, because the gRPC client is behind an opt-in feature.
    // What the catalogue records for a provider is what the provider offers.
    let transports = offered.map_or_else(
        || {
            // No matching attribute: the provider still has an in-process form,
            // and claiming more would be inventing a binding.
            let mut only_local = BTreeSet::new();
            only_local.insert(Transport::Local);
            only_local
        },
        |o| o.transports.clone(),
    );

    // A provider cannot offer what the contract has no projection for -- that
    // would be a Rust-internal inconsistency, the same class as GBX0207.
    let impossible: Vec<&str> = transports
        .difference(&projected.transports)
        .map(|t| t.as_str())
        .collect();
    if !impossible.is_empty() {
        diagnostics.push(invalid(
            uri,
            format!(
                "gear `{owner}` offers `{}` over [{}], but the contract's sdk crate declares no \
                 matching projection trait",
                projected.trait_ident,
                impossible.join(", ")
            ),
            "add the `<Base>Rest`/`<Base>Grpc` projection trait to the sdk crate, or drop the \
             transport from `#[toolkit::provides]`",
        ));
    }

    let provider = ProviderDescriptor {
        contract: id.clone(),
        provider_gear: owner.clone(),
        local_factory: record.local.clone(),
        transports,
        policies: record.policies.clone(),
    };

    let contract = ContractDescriptor {
        id,
        // Projected: the contract belongs to whichever gear its attribute names.
        owner: GearId::new(&projected.gear).unwrap_or_else(|_| owner.clone()),
        base_name: projected.base_name.clone(),
        version,
        kind: projected.kind,
        rust_path: record.rust.clone(),
        sdk: cargo_ref(
            &record.sdk,
            gdl_dir,
            &format!("provide(contract = \"{}\").sdk", record.contract),
            uri,
            diagnostics,
        ),
        rest: record.rest.as_ref().map(|r| RestProjection {
            base_path: r.base_path.clone(),
            visibility: match r.visibility.as_deref() {
                Some("internal") => RestVisibility::Internal,
                _ => RestVisibility::Exposed,
            },
            require_full_coverage: r.require_full_coverage,
        }),
        grpc: record.grpc.as_ref().map(|g| GrpcProjection {
            package: g.package.clone(),
            service: g.service.clone(),
            stubs_module: g.stubs_module.clone(),
        }),
    };

    Some((provider, contract))
}

#[expect(
    clippy::too_many_arguments,
    reason = "each is a distinct fact the requirement records; the projected `from` travels \
              beside the record rather than inside it because the record no longer carries one"
)]
fn build_consumer(
    uri: &str,
    consumer: &GearId,
    record: &gearbox_gdl::records::ConsumeRecord,
    projected: &ProjectedContract,
    declared_from: &str,
    ordinal: usize,
    gdl_dir: &RelPath,
    diagnostics: &mut Diagnostics,
) -> Option<(Requirement, ContractDescriptor)> {
    let (id, version) = match contract_id(projected) {
        Ok(parts) => parts,
        Err(e) => {
            diagnostics.push(invalid(
                uri,
                e,
                "fix the contract attribute in the SDK crate",
            ));
            return None;
        }
    };

    // Projected: the owner is whichever gear the contract attribute names. The
    // declared `from_` is the product-level statement of which gear is expected
    // to supply it, and the two can legitimately differ if a description points
    // at a different provider of the same contract.
    let owner = match GearId::new(&projected.gear) {
        Ok(id) => id,
        Err(e) => {
            diagnostics.push(invalid(
                uri,
                format!(
                    "contract `{}` names owner `{}`, which is not a valid gear id: {e}",
                    projected.trait_ident, projected.gear
                ),
                "fix `#[toolkit::contract(gear = \"...\")]`",
            ));
            return None;
        }
    };

    // Projected from `#[toolkit::consumes(from = ...)]`. The runtime uses this
    // string as a directory key, so the id it parses to is the one the override
    // key has to name.
    let from = match GearId::new(declared_from) {
        Ok(id) => id,
        Err(e) => {
            diagnostics.push(invalid(
                uri,
                format!("invalid `from` gear `{declared_from}`: {e}"),
                "name the providing gear by its kebab-case id in \
                 `#[toolkit::consumes(from = \"...\")]`",
            ));
            return None;
        }
    };

    let requirement_id =
        match RequirementId::new(format!("{consumer}#contract.consumes[{ordinal}]")) {
            Ok(rid) => rid,
            Err(e) => {
                diagnostics.push(invalid(uri, format!("internal: {e}"), "report this"));
                return None;
            }
        };

    let requirement = Requirement {
        id: requirement_id,
        requester: consumer.clone(),
        kind: RequirementKind::Contract {
            contract: id.clone(),
            from,
            resolving_client: record.resolving_client.clone(),
        },
        capabilities: BTreeSet::new(),
        critical: record.critical,
    };

    let contract = ContractDescriptor {
        id,
        owner,
        base_name: projected.base_name.clone(),
        version,
        kind: projected.kind,
        rust_path: record.rust.clone(),
        sdk: cargo_ref(
            &record.sdk,
            gdl_dir,
            &format!("consume(contract = \"{}\").sdk", record.contract),
            uri,
            diagnostics,
        ),
        rest: None,
        grpc: None,
    };

    Some((requirement, contract))
}

fn cluster_requirement(
    requester: &GearId,
    record: &gearbox_gdl::records::ClusterRequireRecord,
    ordinal: usize,
) -> Result<Requirement, String> {
    let primitive = ClusterPrimitive::ALL
        .iter()
        .copied()
        .find(|p| p.slug() == record.primitive)
        .ok_or_else(|| format!("unknown cluster primitive `{}`", record.primitive))?;

    let id = RequirementId::new(format!(
        "{requester}#cluster.{}[{ordinal}]",
        primitive.slug()
    ))
    .map_err(|e| format!("internal: {e}"))?;

    let capabilities = record
        .capabilities
        .iter()
        .map(|c| CapabilityId::new(c).map_err(|e| format!("invalid capability `{c}`: {e}")))
        .collect::<Result<BTreeSet<_>, _>>()?;

    Ok(Requirement {
        id,
        requester: requester.clone(),
        kind: RequirementKind::Cluster {
            primitive,
            scope: record.scope.clone(),
        },
        capabilities,
        critical: false,
    })
}

/// Refuse a second role claiming the gear's own name.
///
/// The front door is the role registered under the bare gear id, and a
/// bare-name lookup reaching it *and only it* is what makes an internal role
/// unreachable by accident rather than by a filter someone could misconfigure.
/// Two claimants and the guarantee is gone: the lookup round-robins over two
/// services that were split apart on purpose.
fn report_front_doors(
    uri: &str,
    id: &GearId,
    roles: &[DeclaredRole],
    diagnostics: &mut Diagnostics,
) {
    let claimants: Vec<&str> = roles
        .iter()
        .filter(|r| r.directory_name == id.as_str())
        .map(|r| r.name.as_str())
        .collect();
    if claimants.len() < 2 {
        return;
    }
    diagnostics.push(
        Diagnostic::error(
            DiagnosticCode::GdlDuplicateFrontDoor,
            format!(
                "roles {} all register `{id}`, and a bare-name lookup reaches whichever \
                 answers first",
                claimants
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            format!(
                "exactly one role may be the front door. Give the others their own \
                 `directory_name`, or leave it out and take the default `{id}-<name>`"
            ),
        )
        .at(Location::file(uri.to_owned())),
    );
}

/// Say what the gear's attribute declares and this tool threw away.
///
/// **`ProjectedGear::unmodelled` promised this and showed it to nobody.** Its
/// doc says unknown arguments are recorded "so a future macro argument surfaces
/// as a known gap instead of a silent omission", and it had one writer, no
/// reader outside its own unit test, and no diagnostic code. The projection
/// dropped the field on the floor at exactly the point a report belongs.
///
/// Non-empty only in a skew window, which is the ordinary way a fact arrives:
/// `#[toolkit::gear]` refuses an argument it does not know, so the platform
/// lands one first and this parser catches up afterwards. `one_per_installation`
/// came that way, and ADR `cpt-gearbox-adr-one-per-installation` cites this
/// field by name while describing it.
fn report_unmodelled_arguments(
    uri: &str,
    id: &GearId,
    projected: &ProjectedGear,
    diagnostics: &mut Diagnostics,
) {
    if projected.unmodelled.is_empty() {
        return;
    }
    let named = projected
        .unmodelled
        .iter()
        .map(|arg| format!("`{arg}`"))
        .collect::<Vec<_>>()
        .join(", ");
    diagnostics.push(
        Diagnostic::new(
            DiagnosticCode::GapUnmodelledGearArgument,
            format!(
                "gear `{id}` declares {named} on its `#[toolkit::gear]` attribute, and this \
                 tool does not read {}",
                if projected.unmodelled.len() == 1 {
                    "it"
                } else {
                    "them"
                }
            ),
        )
        .at(Location::file(uri.to_owned()))
        .with_evidence(
            "libs/toolkit-macros/src/lib.rs:429 (`unknown attribute parameter` -- the \
             macro refuses what it does not know, so a name reaching here is one it took)",
        )
        .with_help(
            "the platform knows something about this gear that the catalogue does not, so \
             anything resolved from it decides without that fact. Nothing here is wrong -- \
             this tool is behind",
        ),
    );
}

/// Refuse a registration name the rest of the system cannot express.
///
/// `directory_name` names an entry in the same directory a `GearId` names, and
/// every other identifier in this codebase is held to one kebab rule -- the one
/// `#[toolkit::gear]` enforces on its own `name`. This one was a plain `String`
/// and was checked by nobody, so `GearId::new` is the validator here: not a
/// second spelling of the rule, but the rule itself, applied to a name that
/// lives in the namespace it governs.
///
/// **The default is the case that matters.** A role name is a serde variant
/// spelling, so leaving `directory_name` out splices a kebab gear id onto a
/// `snake_case` value: `event-broker` plus `cluster_ingest` is
/// `event-broker-cluster_ingest`. The remedy therefore differs by which half
/// produced the name, and the message says which.
fn report_role_names(
    uri: &str,
    id: &GearId,
    records: &[gearbox_gdl::records::RoleRecord],
    roles: &[DeclaredRole],
    diagnostics: &mut Diagnostics,
) {
    // Zipped rather than looked up: `roles` is built by mapping `records` just
    // above, so the two are the same length in the same order.
    for (record, role) in records.iter().zip(roles) {
        if GearId::new(role.directory_name.clone()).is_ok() {
            continue;
        }
        let help = if record.directory_name.is_some() {
            format!(
                "`directory_name` names a directory entry, so it follows the rule every \
                 gear id follows: lowercase letters, digits and single hyphens, starting \
                 with a letter. Rename it, or leave it out and take the default \
                 `{id}-<name>`"
            )
        } else {
            format!(
                "this is the default `{id}-<name>`, and `{}` is a config enum variant \
                 rather than a kebab name. Give the role an explicit `directory_name`",
                role.name
            )
        };
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::GdlRoleNameNotKebab,
                format!(
                    "role `{}` of gear `{id}` registers as `{}`, which is not a name the \
                     directory -- or a Kubernetes `Service` -- can carry",
                    role.name, role.directory_name
                ),
                help,
            )
            .at(Location::or_file(record.declared_at.as_ref(), uri)),
        );
    }
}

/// Refuse a role naming a mode the gear's own configuration will not accept.
///
/// **The check three doc comments promised and none performed.** `role(name =
/// ...)` is defined in `gear_globals`, in `DeclaredRole` and in
/// `ApplicationRole` as the value the gear's own mode selector accepts, "the
/// only spelling checkable against a projected enum" -- and the join between
/// the description's half and Rust's half was a string nobody compared. The
/// comparison itself is the one `config_check` already makes for a value a
/// product sets, against the same projected variants.
///
/// **Silent when there is nothing to check against.** A gear whose description
/// exposes no enum field may still read its mode from a field it does not put
/// in front of an integrator, so an absent enum is an absent answer rather than
/// a wrong name. Any enum field accepting the spelling is enough: which field is
/// the selector is not something the description says, and inventing a rule for
/// picking one would be this tool deciding a fact the gear owns.
fn report_role_modes(
    uri: &str,
    id: &GearId,
    roles: &[DeclaredRole],
    config: Option<&gearbox_ir::ConfigSchema>,
    diagnostics: &mut Diagnostics,
) {
    let variants: Vec<&str> = config
        .map(|schema| schema.fields.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|field| match &field.ty {
            gearbox_ir::ConfigFieldType::Enum { variants } => Some(variants),
            _ => None,
        })
        .flat_map(|variants| variants.iter().map(String::as_str))
        .collect();
    if variants.is_empty() {
        return;
    }
    for role in roles {
        if variants.contains(&role.name.as_str()) {
            continue;
        }
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::GdlRoleNotAMode,
                format!(
                    "gear `{id}` declares role `{}`, and no configuration value it exposes \
                     accepts that spelling",
                    role.name
                ),
                format!(
                    "a role's `name` is the value the gear's own mode selector takes. \
                     The spellings this gear accepts are {}",
                    variants
                        .iter()
                        .map(|v| format!("`{v}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .at(Location::file(uri.to_owned())),
        );
    }
}

/// Report what a declared role asks for and this tool cannot emit.
///
/// Only the labels half lives here. Whether a role can be *deployed* is a
/// statement about a product -- one application per anchor gear, so at most
/// one role -- and is reported at resolution as `GBX0318`. Whether its labels
/// can be written is a statement about the gear and its generated
/// configuration, which is knowable the moment the description is read.
fn report_role_gaps(
    uri: &str,
    id: &GearId,
    roles: &[DeclaredRole],
    declared_at: Option<&Location>,
    diagnostics: &mut Diagnostics,
) {
    if !roles.iter().any(|r| !r.labels.is_empty()) {
        return;
    }
    diagnostics.push(
        Diagnostic::new(
            DiagnosticCode::GapShards,
            format!(
                "gear `{id}` registers roles under labels, and nothing generated from this \
                 description can carry one"
            ),
        )
        .at(Location::or_file(declared_at, uri))
        .with_evidence(
            "libs/toolkit/src/bootstrap/config/mod.rs \
             (oop_http.labels exists; the embedded profile has no instance to label)",
        )
        .with_help(
            "drop the `labels`, or keep them knowing the generated `oop_http` section has \
             no label field to write them into",
        ),
    );
}

/// Warn when a gear's category is not one the platform uses.
///
/// A warning rather than a refusal: the taxonomy is visibly still settling, with
/// `cluster` filed under `serverless` and `account-management` under `oss`, so
/// treating the list as closed would claim more than the evidence supports. What
/// it does catch is the case that matters -- a value nothing else uses, which
/// puts a gear in a bucket of one and hides it from every category query.
fn report_unknown_category(
    uri: &str,
    id: &GearId,
    category: Option<&str>,
    // `declared_at` is the `gear(...)` span: `category` is an argument of that
    // call rather than a call of its own, so this is the finest anchor there is.
    declared_at: Option<&Location>,
    diagnostics: &mut Diagnostics,
) {
    let Some(name) = category else { return };
    if gearbox_gdl::vocabulary::KNOWN_CATEGORIES.contains(&name) {
        return;
    }
    diagnostics.push(
        Diagnostic::new(
            DiagnosticCode::GdlUnknownCategory,
            format!("gear `{id}` declares category `{name}`, which no other gear uses"),
        )
        .at(Location::or_file(declared_at, uri))
        .with_help(format!(
            "the platform's categories are: {}",
            gearbox_gdl::vocabulary::KNOWN_CATEGORIES.join(", ")
        )),
    );
}

/// Record that `deps = [cluster]` now costs more than it buys.
///
/// **This hint used to say the opposite, and the reversal is the point.** It
/// read "keep `deps = [cluster]`; it is what makes the requirement resolvable
/// today", on the premise that the cluster gear had no remote surface and a
/// consumer therefore had to share its process. That premise is gone: the gear
/// is deployable out of process, `RemoteClusterClient` implements the same
/// backend traits over gRPC, and a consumer resolves through the process's
/// single `dyn ClusterClient` whichever side of a boundary it is on. The
/// corpus proves it -- `api-contracts-consumer` declares no such dep and
/// resolves its scope in all three profiles.
///
/// What is left is a real cost with no remaining benefit. `deps` is a hard
/// topo-sort edge, so a gear that declares it pins itself into the cluster
/// gear's process *and* cannot be spawned out of process at all: the registry
/// build fails with `RegistryError::UnknownDependency` when the named gear is
/// not linked in. So the advice inverts -- the edge can go, and the
/// requirement still resolves.
///
/// Still a hint rather than a warning: a co-located consumer is a legitimate
/// topology, and one that never intends to be spawned loses nothing by
/// declaring the dep. It is the reader planning a multi-process topology who
/// needs to know, before the registry refuses to build at startup.
fn report_cluster_colocation(
    uri: &str,
    id: &GearId,
    requires: &[Requirement],
    projected: &ProjectedGear,
    diagnostics: &mut Diagnostics,
) {
    if requires.is_empty() {
        return;
    }
    // Only meaningful for a consumer, not for the cluster gear itself.
    if !projected
        .colocated_deps
        .iter()
        .any(|d| d == CLUSTER_GEAR_NAME)
    {
        return;
    }

    diagnostics.push(
        Diagnostic::new(
            DiagnosticCode::GapClusterNotDeployable,
            format!(
                "gear `{id}` requires {} cluster primitive(s) and also declares \
                 `deps = [{CLUSTER_GEAR_NAME}]`, which pins it into that gear's application and \
                 stops it being spawned out of process at all",
                requires.len()
            ),
        )
        .at(Location::file(uri.to_owned()))
        .with_evidence(
            "gears/system/cluster/cluster-sdk/src/cache/resolver.rs:64 (one `dyn ClusterClient` \
             per process, which is what a remote consumer resolves through); \
             gears/system/cluster/cluster/tests/consumer_wiring.rs:76 \
             (`RegistryError::UnknownDependency` when the named gear is not linked in)",
        )
        .with_help(
            "the dep is no longer needed for the requirement to resolve: the cluster gear is \
             deployable out of process and serves its scopes over gRPC, so a consumer reaches it \
             from another application. Drop `deps` unless this gear is meant to stay co-located -- \
             keeping it is what makes an out-of-process build fail",
        ),
    );
}

/// The cluster gear's runtime name, as `#[toolkit::gear(name = ...)]` spells it.
const CLUSTER_GEAR_NAME: &str = "cluster";

/// Resolve the crate directory a description's `package` points at.
///
/// `..` is legal here and only here. `RelPath` forbids it in a *stored* path,
/// while [`RelPath::resolve`] supports it at the point of resolution, because a
/// description legitimately points at a sibling crate (`../payments-audit-sdk`).
///
/// What is not legal is an absolute path, a backslash, or a walk above the
/// source root. Those used to fall back to the description's own directory,
/// which projected a plausible but entirely different `src/` tree and reported
/// nothing.
///
/// # Errors
/// Returns the [`gearbox_ir::IdError`] from [`RelPath::resolve`].
pub fn crate_dir(
    root: &crate::SourceRoot,
    gdl_path: &RelPath,
    package_path: &str,
    crate_name: &str,
) -> Result<std::path::PathBuf, gearbox_ir::IdError> {
    // Identity first, and only where identity is available. A registry root
    // knows where each fetched package landed; a directory root's table is
    // empty and the path is all there is. Looking the name up everywhere would
    // be worse than it sounds -- a description with a wrong path and a right
    // crate name would start resolving, and GBX0209 would never catch it.
    if let Some(dir) = root.siblings.get(crate_name) {
        return Ok(dir.clone());
    }
    let joined = gdl_path.parent().resolve(package_path)?;
    Ok(if joined.is_here() {
        root.root.clone()
    } else {
        root.root.join(joined.as_str())
    })
}

/// The diagnostic for a declared crate path that cannot be resolved.
///
/// One function rather than one message per call site: every caller of
/// [`crate_dir`] has the same mistake to explain, and five spellings of it would
/// drift.
#[must_use]
///
/// `declared_at` is the `cargo(...)` call the path was written in. Every caller
/// has the record in hand, and without it this pointed at line 1 of whichever
/// description referenced the crate.
pub fn bad_crate_path(
    uri: &str,
    field: &str,
    package_path: &str,
    error: &gearbox_ir::IdError,
    declared_at: Option<&Location>,
) -> Diagnostic {
    invalid_at(
        Location::or_file(declared_at, uri),
        format!(
            "`{field}` declares `path = \"{package_path}\"`, which cannot be resolved: {error}"
        ),
        "the path is relative to the description's own directory; `..` may reach a sibling \
         crate, but not climb above the source root, and it must not be absolute",
    )
}
