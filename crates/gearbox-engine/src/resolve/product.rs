//! Step 10: assembling the lock, and step 9's graph alongside it.
//!
//! Everything the earlier steps decided is gathered here into the one artifact
//! every generator reads. Ordering is imposed by `gearbox_lock::canonicalize_order`
//! and the hash by `compute_hash`, so "did anything actually change" is a byte
//! comparison rather than a judgement.
//!
//! The explanation graph is built from the finished resolution rather than
//! accumulated as each decision is made. The plan asked for the latter; the
//! information is identical either way, because every decision already records
//! its own provenance -- a gear knows why it is in the closure, a binding knows
//! what was asked for and what downgraded it, a cluster choice knows whether it
//! was named. Threading a mutable graph through six modules would buy nothing
//! those records do not already carry, and would couple every step to a type
//! none of them otherwise needs.

use std::collections::BTreeMap;

use gearbox_ir::{
    Catalogue, Choice, ExplanationGraph, ExplanationNode, GearId, InclusionReason,
    KubernetesSettings, LOCK_SCHEMA_VERSION, NodeId, NodeKind, ProductIntent, ProvenanceEdge,
    ProvenanceKind, ResolvedGear, ResolvedProduct, ResolvedProductHeader, ResolvedSource, SourceId,
    binding_key,
};

use super::Resolution;

/// Build the lock from a finished resolution.
///
/// `sources` comes from the caller because only it knows where each root was
/// actually opened; the resolver is pure and has never touched a filesystem.
#[must_use]
pub fn assemble(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    resolution: &Resolution,
    sources: BTreeMap<SourceId, ResolvedSource>,
) -> ResolvedProduct {
    let declaration = intent.profiles.get(&resolution.profile);

    let mut product = ResolvedProduct {
        sqlite_databases: resolution.sqlite_databases.clone(),
        database: resolution.database.clone(),
        schema_version: LOCK_SCHEMA_VERSION,
        product: ResolvedProductHeader {
            id: intent.id.clone(),
            version: intent.version.clone(),
            profile: resolution.profile.clone(),
            profile_kind: declaration.map_or("unknown", |d| d.kind()).to_owned(),
            layout: intent
                .layout
                .clone()
                .unwrap_or_else(|| gearbox_ir::DEFAULT_LAYOUT.to_owned()),
            gearbox_version: env!("CARGO_PKG_VERSION").to_owned(),
            // Filled in below, once the body is final. Hashing it before that
            // would hash a value that no longer describes the thing.
            lock_hash: String::new(),
        },
        kubernetes: kubernetes(declaration),
        self_hosted: self_hosted(declaration),
        sources,
        gears: gears(catalogue, intent, resolution),
        applications: resolution.partition.applications.clone(),
        bindings: resolution.bindings.clone(),
        cluster: resolution.cluster.clone(),
        cuttable_if_declared: resolution.cuts.blocked.clone(),
        provenance: explain(catalogue, intent, resolution).edges,
        diagnostics: resolution.diagnostics.clone(),
        ui: resolution.ui.clone(),
    };

    gearbox_lock::canonicalize_order(&mut product);
    // A hash that cannot be computed is left empty rather than faked: an empty
    // one is visibly wrong, while a placeholder would compare equal to itself.
    product.product.lock_hash = gearbox_lock::compute_hash(&product).unwrap_or_default();
    product
}

fn kubernetes(
    declaration: Option<&gearbox_ir::DeploymentProfileDecl>,
) -> Option<KubernetesSettings> {
    match declaration {
        Some(gearbox_ir::DeploymentProfileDecl::Kubernetes {
            discovery,
            namespace,
            image_registry,
            ..
        }) => Some(KubernetesSettings {
            namespace: namespace.clone(),
            image_registry: image_registry.clone(),
            discovery: *discovery,
        }),
        _ => None,
    }
}

/// The `self_hosted` half of the same, and for the same reason.
///
/// `target_dir` travels as the description spelled it. Resolving it here would
/// bake a machine-specific path into a committed file; the generator converts it
/// once, against the output root only it knows.
fn self_hosted(
    declaration: Option<&gearbox_ir::DeploymentProfileDecl>,
) -> Option<gearbox_ir::SelfHostedSettings> {
    match declaration {
        Some(gearbox_ir::DeploymentProfileDecl::SelfHosted {
            discovery,
            target_dir,
            cargo_profile,
            ..
        }) => Some(gearbox_ir::SelfHostedSettings {
            target_dir: target_dir.clone(),
            cargo_profile: cargo_profile.clone(),
            discovery: *discovery,
        }),
        _ => None,
    }
}

/// The gears in the product, each carrying why it is here and what it was told.
fn gears(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    resolution: &Resolution,
) -> BTreeMap<GearId, ResolvedGear> {
    resolution
        .closure
        .members
        .iter()
        .filter_map(|(id, reasons)| {
            let descriptor = catalogue.gears.get(id)?;
            Some((
                id.clone(),
                ResolvedGear {
                    id: id.clone(),
                    source: descriptor.source.clone(),
                    gdl_path: descriptor.gdl_path.clone(),
                    package: descriptor.package.clone(),
                    // Resolved at merge time so no generator has to redo it.
                    crate_dir: descriptor.crate_dir(),
                    runtime_caps: descriptor.runtime_caps.clone(),
                    colocated_deps: descriptor.colocated_deps.clone(),
                    selected_by: reasons.clone(),
                    // A `use_gear` entry or the `plugin(...)` entry that chose
                    // this gear. A gear the closure merely pulled in has none,
                    // and inheriting one would be a decision nobody wrote down.
                    //
                    // **The plugin half was missing, and it made a product
                    // unstartable.** `plugin("oidc-authn-plugin", config = ...)`
                    // was evaluated, validated as far as anything validates it,
                    // and then dropped here -- the generated file carried
                    // `config: {}` and the plugin failed at startup on a field
                    // the description had supplied. Found by running the thing.
                    config: declared_config(intent, id),
                    selected_features: declared_features(intent, id),
                },
            ))
        })
        .collect()
}

/// What the description told this gear, whether it was named or chosen.
///
/// A plugin is a gear the product selected *under* a host rather than on its own
/// terms, so its configuration arrives on the `plugin(...)` call. Looking only at
/// `selected_gears` misses it entirely.
fn declared_config(intent: &ProductIntent, id: &GearId) -> BTreeMap<String, serde_json::Value> {
    if let Some(selection) = intent.selected_gears.iter().find(|s| &s.gear == id) {
        return selection.config.clone();
    }
    intent
        .selected_gears
        .iter()
        .flat_map(|selection| &selection.plugins)
        .find(|plugin| &plugin.gear == id)
        .map(|plugin| plugin.config.clone())
        .unwrap_or_default()
}

/// The Cargo features the description asked for, for one gear.
///
/// One place to look, unlike [`declared_config`]: `PluginSelection` carries a
/// `config` but no `features`, so only a `use_gear` can ask for one. A plugin
/// and a gear the closure merely pulled in both ask for nothing.
fn declared_features(intent: &ProductIntent, id: &GearId) -> std::collections::BTreeSet<String> {
    intent
        .selected_gears
        .iter()
        .find(|selection| &selection.gear == id)
        .map(|selection| selection.features.iter().cloned().collect())
        .unwrap_or_default()
}

/// Step 9: the graph that answers "why".
///
/// Node ids are content-derived, never counters, so the same resolution produces
/// the same graph byte for byte. Origins come from the product description when
/// the fact was written there (`use_gear`, `bind`, a profile constructor), and
/// from the catalogue's `gear(...)` when a gear arrived through co-location.
#[must_use]
pub fn explain(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    resolution: &Resolution,
) -> ExplanationGraph {
    let mut graph = ExplanationGraph::new();
    let Some(profile) = node_id(NodeKind::Profile, resolution.profile.as_str()) else {
        return graph;
    };
    let mut profile_node = ExplanationNode::new(
        profile.clone(),
        NodeKind::Profile,
        resolution.profile.to_string(),
    );
    if let Some(origin) = intent
        .profiles
        .get(&resolution.profile)
        .and_then(|d| d.declared_at().cloned())
    {
        profile_node = profile_node.at(origin);
    }
    graph.add_node(profile_node);

    // The description itself, so the arm whose answer is "you wrote it there"
    // has somewhere true to point. Its origin is the file: a gear's own
    // `use_gear` line rides on the gear node, and repeating it here would make
    // two rows in the panel claim the same location for different facts.
    let product = node_id(NodeKind::Product, intent.id.as_str());
    if let Some(id) = &product {
        graph.add_node(ExplanationNode::new(
            id.clone(),
            NodeKind::Product,
            intent.id.clone(),
        ));
    }

    explain_gears(catalogue, intent, resolution, product.as_ref(), &mut graph);
    explain_processes(resolution, &mut graph);
    explain_bindings(intent, resolution, &mut graph);
    explain_cluster(resolution, &profile, &mut graph);
    explain_blocked_cuts(resolution, &mut graph);

    graph.finish();
    graph
}

/// The gears, and why each of them is in the product.
///
/// Split out of [`explain`] with its four siblings when the function outgrew
/// clippy's line limit. The seam is by *family of edge*, which is also how a
/// reader looks for one.
fn explain_gears(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    resolution: &Resolution,
    product: Option<&NodeId>,
    graph: &mut ExplanationGraph,
) {
    for (gear, reasons) in &resolution.closure.members {
        let Some(id) = node_id(NodeKind::Gear, gear.as_str()) else {
            continue;
        };
        let mut node = ExplanationNode::new(id.clone(), NodeKind::Gear, gear.to_string());
        if let Some(origin) = gear_origin(catalogue, intent, gear) {
            node = node.at(origin);
        }
        graph.add_node(node);
        for reason in reasons {
            let edge = match reason {
                // **Not at the profile.** `gears = [...]` is not profile-scoped,
                // so the identical edge is emitted for every profile and
                // "selected-by dev" was false of all of them. A person who
                // named a gear is owed the file they named it in, which is
                // what the node's own origin already carries -- so the edge
                // says who did the naming and stops implying a decision the
                // resolver never made.
                InclusionReason::Selected => product.cloned().map(|to| {
                    ProvenanceEdge::new(
                        id.clone(),
                        to,
                        ProvenanceKind::Declared,
                        format!("`{gear}` is named in the product description"),
                    )
                }),
                InclusionReason::ColocatedBy { gear: by } => node_id(NodeKind::Gear, by.as_str())
                    .map(|to| {
                        ProvenanceEdge::new(
                            id.clone(),
                            to,
                            ProvenanceKind::ColocatedBy,
                            // Scoped to *this edge*, which is what the reason
                            // is about. It read "link-time and cannot be cut",
                            // which a reader took as "this gear can never run
                            // out of process" -- and that is usually false;
                            // the dep pins it, the gear itself may be perfectly
                            // deployable. The remedy is named because there is
                            // one, and GBX0607 already records it.
                            format!(
                                "`{by}` names `{gear}` in its `deps`, so the two are linked \
                             into one binary and no profile can separate them. Removing \
                             that entry is the only thing that can."
                            ),
                        )
                    }),
                // The edge points at the host, not at the profile: "why is this
                // crate in my binary" is answered by the gear that selected it,
                // and the profile is a qualifier on that answer rather than the
                // answer itself.
                InclusionReason::PluginOf { host, profile: p } => {
                    node_id(NodeKind::Gear, host.as_str()).map(|to| {
                        ProvenanceEdge::new(
                            id.clone(),
                            to,
                            ProvenanceKind::SelectedBy,
                            format!(
                                "`{gear}` is selected as a plugin of `{host}` for profile `{p}`"
                            ),
                        )
                    })
                }
            };
            if let Some(edge) = edge {
                graph.add_edge(edge);
            }
        }
    }
}

/// One node per process, pointing at the gear whose placement created it.
fn explain_processes(resolution: &Resolution, graph: &mut ExplanationGraph) {
    for application in &resolution.partition.applications {
        let (Some(id), Some(anchor)) = (
            node_id(NodeKind::Application, application.name.as_str()),
            node_id(NodeKind::Gear, application.anchor.as_str()),
        ) else {
            continue;
        };
        graph.add_node(ExplanationNode::new(
            id.clone(),
            NodeKind::Application,
            application.name.to_string(),
        ));
        graph.add_edge(ProvenanceEdge::new(
            id,
            anchor,
            ProvenanceKind::DerivedFrom,
            format!(
                "application `{}` is the co-location closure of `{}`: the {} gears that \
                 `deps` links to it, in one binary",
                application.name,
                application.anchor,
                application.gears.len()
            ),
        ));
    }
}

/// Each binding, its mode, and the diagnostic when the request was not honoured.
fn explain_bindings(intent: &ProductIntent, resolution: &Resolution, graph: &mut ExplanationGraph) {
    for binding in &resolution.bindings {
        let key = binding_key(binding.consumer.as_str(), binding.contract.as_str());
        let (Some(id), Some(consumer_application)) = (
            node_id(NodeKind::Binding, &key),
            node_id(NodeKind::Application, binding.consumer_application.as_str()),
        ) else {
            continue;
        };
        let mut node = ExplanationNode::new(
            id.clone(),
            NodeKind::Binding,
            format!("{} -> {}", binding.consumer, binding.contract),
        );
        if let Some(origin) = intent
            .bindings
            .iter()
            .find(|b| b.consumer == binding.consumer && b.contract == binding.contract)
            .and_then(|b| b.declared_at.clone())
        {
            node = node.at(origin);
        }
        graph.add_node(node);
        graph.add_edge(ProvenanceEdge::new(
            id.clone(),
            consumer_application,
            ProvenanceKind::DerivedFrom,
            format!(
                "the binding is {} because the consumer is in `{}` and the provider in `{}`",
                binding.mode, binding.consumer_application, binding.provider_application
            ),
        ));
        // The edge a reader comes here for: you asked for X and got Y.
        if let Some(code) = binding.selected.downgraded_by
            && let Some(to) = node_id(NodeKind::Diagnostic, code.as_str())
        {
            graph.add_edge(ProvenanceEdge::new(
                id,
                to,
                ProvenanceKind::DowngradedBy,
                format!(
                    "the request was not honoured, and {code} says why; the outcome above is \
                 what the topology allowed"
                ),
            ));
        }
    }
}

/// Each cluster scope: who requires it, and how its provider was chosen.
fn explain_cluster(resolution: &Resolution, profile: &NodeId, graph: &mut ExplanationGraph) {
    for binding in &resolution.cluster {
        let key = format!("{}|{}", binding.scope, binding.primitive.slug());
        let Some(id) = node_id(NodeKind::ClusterProvider, &key) else {
            continue;
        };
        graph.add_node(ExplanationNode::new(
            id.clone(),
            NodeKind::ClusterProvider,
            format!(
                "{}/{} -> {}",
                binding.scope,
                binding.primitive.slug(),
                binding
                    .resolved
                    .effective_provider()
                    .unwrap_or("unsatisfied")
            ),
        ));
        for requester in &binding.requesters {
            if let Some(to) = node_id(NodeKind::Gear, requester.as_str()) {
                graph.add_edge(ProvenanceEdge::new(
                    id.clone(),
                    to,
                    ProvenanceKind::DerivedFrom,
                    format!(
                        "`{requester}` is why this scope is resolved at all: it requires \
                     `{}` in `{}`",
                        binding.primitive.slug(),
                        binding.scope
                    ),
                ));
            }
        }
        // All three of these used to name nothing -- not the provider that was
        // asked for, not the one in use, not the alternative that lost. A
        // sentence about a choice that mentions neither side of it is a label.
        let asked_for = match &binding.selected.selected {
            Choice::Explicit { value } => Some(value.as_str()),
            Choice::Auto => None,
        };
        let in_use = binding.resolved.effective_provider();
        let (kind, because) = if binding.selected.was_downgraded() {
            (
                ProvenanceKind::DowngradedBy,
                match (asked_for, in_use) {
                    (Some(named), Some(actual)) => format!(
                        "`{named}` was named and could not be used here, so `{actual}` \
                     answers this scope instead"
                    ),
                    (Some(named), None) => format!(
                        "`{named}` was named and could not be used here, and nothing else \
                     satisfies the requirement"
                    ),
                    (None, _) => "the resolver's choice could not be used here".to_owned(),
                },
            )
        } else {
            match asked_for {
                Some(named) => (
                    ProvenanceKind::Declared,
                    format!("the description names `{named}` for this scope"),
                ),
                None => (
                    ProvenanceKind::PreferredOver,
                    match in_use {
                        Some(actual) => format!(
                            "nothing named a provider, so the resolver ranked the registered \
                         ones and took `{actual}`"
                        ),
                        None => "nothing named a provider and none is registered for this \
                             primitive"
                            .to_owned(),
                    },
                ),
            }
        };
        graph.add_edge(ProvenanceEdge::new(id, profile.clone(), kind, because));
    }

    // Blocked cuts are part of the answer to "why is this one process", so they
    // belong in the graph rather than only in the diagnostics.
}

/// Blocked cuts, because "why is this one process" is answered by what could
/// not be separated.
fn explain_blocked_cuts(resolution: &Resolution, graph: &mut ExplanationGraph) {
    for candidate in &resolution.cuts.blocked {
        let (Some(from), Some(to)) = (
            node_id(NodeKind::Gear, candidate.consumer.as_str()),
            node_id(NodeKind::Gear, candidate.provider.as_str()),
        ) else {
            continue;
        };
        graph.add_edge(ProvenanceEdge::new(
            from,
            to,
            ProvenanceKind::ConstrainedBy,
            format!("cannot be separated: {}", candidate.blocked_by),
        ));
    }
}

/// Origin of a gear node: the product's `use_gear` when named, else `gear(...)`.
fn gear_origin(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    gear: &GearId,
) -> Option<gearbox_ir::Location> {
    if let Some(origin) = intent
        .selected_gears
        .iter()
        .find(|s| &s.gear == gear)
        .and_then(|s| s.declared_at.clone())
    {
        return Some(origin);
    }
    catalogue
        .gears
        .get(gear)
        .and_then(|d| d.declared_at.clone())
}

/// A node id derived from what it names, so the graph is byte-stable.
///
/// Delegates to `NodeKind::id_for`, which is where the `{prefix}:{key}` convention
/// now lives -- a diagnostic naming the same node in its `subject` has to produce
/// the identical string, and two format strings for one wire convention is how a
/// client ends up asking about a node that is not there.
fn node_id(kind: NodeKind, key: &str) -> Option<NodeId> {
    kind.id_for(key)
}
