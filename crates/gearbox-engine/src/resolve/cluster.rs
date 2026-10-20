//! Step 7: which backend answers each cluster primitive.
//!
//! The provider table is **projected** from the `with_*_provider` calls in the
//! cluster gear's registry, not declared anywhere, so a provider added in Rust
//! cannot go missing here and one removed cannot linger. Today that table holds
//! three entries, and the shape of what they offer decides most of this step:
//!
//! * `postgres` answers cache and lock, is linearizable for both, is not
//!   process-local, and needs credentials;
//! * `standalone` answers cache only, adds prefix-watch, is process-local, and
//!   needs none;
//! * `redis` answers cache and lock, is not process-local, needs credentials,
//!   and **declares no capability at all** -- it reads its consistency off the
//!   server it connects to, so there is no composition-time fact to project
//!   (GBX0520). It therefore satisfies only a requirement that asks for none,
//!   which is the honest answer rather than a limitation of this step.
//!
//! **None of them registers leader election.** That is not an omission in this code:
//! zero leader-election providers exist in the runtime, so the primitive always
//! resolves to the SDK's compare-and-swap default layered over whatever cache is
//! bound. Reported rather than hidden, because "leader election works" and
//! "leader election is CAS over your cache" are different promises.
//!
//! The most valuable rule here is the last one. A process-local backend in a
//! topology with more than one process is a correctness bug the runtime would
//! start happily and never complain about: `standalone`'s cache lives in one
//! process's memory, so leader election over it elects a leader *per replica*.

use std::collections::{BTreeMap, BTreeSet};

use gearbox_ir::{
    CapabilityId, Catalogue, Choice, ClusterPrimitive, ClusterProviderDecl, ClusterResolution,
    Diagnostic, DiagnosticCode, Diagnostics, GearId, Location, Preference, ProviderBinding,
    RequirementKind, ResolvedApplication, ResolvedClusterBinding, Selected,
};

use super::closure::Closure;
use super::partition::Partition;
use super::profile::ProfileScoped;

/// One primitive that something in the product needs.
#[derive(Debug, Default)]
struct Need {
    requesters: BTreeSet<GearId>,
    capabilities: BTreeSet<CapabilityId>,
}

/// Resolve every cluster primitive the product asks for.
pub fn resolve(
    catalogue: &Catalogue,
    closure: &Closure,
    partition: &Partition,
    scoped: &ProfileScoped<'_>,
    // The preferences used to arrive separately, and then `intent` arrived too
    // -- two ways to reach one list, which is one more than a caller should
    // have to keep in step.
    intent: &gearbox_ir::ProductIntent,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Vec<ResolvedClusterBinding> {
    let needs = collect_needs(catalogue, closure);
    if needs.is_empty() {
        return Vec::new();
    }

    let providers = provider_table(catalogue, closure, intent);
    let spread = is_spread(partition);
    let prefer_existing = intent
        .preferences
        .contains(&Preference::ExistingInfrastructure);

    let mut out: Vec<ResolvedClusterBinding> = Vec::new();
    for ((scope, primitive), need) in needs {
        // Within a scope, the cache is decided first: leader election and lock
        // both fall back to a default layered over it, and `existing
        // infrastructure` prefers whatever the scope already uses.
        let already: BTreeSet<String> = out
            .iter()
            .filter(|b| b.scope == scope)
            .filter_map(|b| b.resolved.effective_provider().map(str::to_owned))
            .collect();

        let declared_scope = scoped.cluster_scopes.iter().find(|s| s.scope == scope);
        let declared = declared_scope.and_then(|s| s.binding(primitive));
        let declared_cache = declared_scope.map(|s| &s.cache);

        // The cache this scope actually resolved to, read back by name rather
        // than taken from `already`. `already` is a set, so `iter().next()` is
        // whichever provider sorts first among *all* of them -- which is the
        // cache only because `ClusterPrimitive` happens to order `Cache` before
        // the two primitives that fall back to it. Add a third provider that
        // answers leader election and a scope that binds it, and the lock's
        // "compare-and-swap over the cache" would name that one instead.
        let resolved_cache: Option<String> = out
            .iter()
            .find(|b| b.scope == scope && b.primitive == ClusterPrimitive::Cache)
            .and_then(|b| b.resolved.effective_provider().map(str::to_owned));

        // **Against the binding as written.** The options belong to the
        // `provider(...)` call somebody typed, so they are checked whether or
        // not resolution ends up choosing that provider -- and if the name is
        // not registered, `GBX0505` has already said so and there is no schema.
        if let Some(written) = declared {
            crate::resolve::cluster_options::check(
                &providers,
                primitive,
                written,
                uri,
                diagnostics,
            );
        }

        let binding = decide(
            &Context {
                scope: &scope,
                primitive,
                need: &need,
                providers: &providers,
                already: &already,
                declared_cache,
                resolved_cache: resolved_cache.as_deref(),
                spread,
                prefer_existing,
                uri,
                declared_at: declared_scope.and_then(|s| s.declared_at.as_ref()),
            },
            declared,
            diagnostics,
        );
        out.push(binding);
    }

    out.sort_by(|a, b| (&a.scope, a.primitive).cmp(&(&b.scope, b.primitive)));
    out
}

/// The cluster gear's id, the name its Kubernetes Service is published under.
const CLUSTER_GEAR: &str = "cluster";
/// The gRPC hub, which serves cluster's coordination plane to other processes.
const GRPC_HUB: &str = "grpc-hub";
/// The port the cluster SDK dials by convention (`cluster_sdk::CLUSTER_GRPC_PORT`).
const CLUSTER_GRPC_PORT: u16 = 50051;

/// The profile contract every remote cluster client resolves its scope through.
const CLUSTER_PROFILE_CONTRACT: &str = "cluster/ClusterProfileApi@v1";

/// The contract a remote requester of `primitive` reaches.
fn primitive_contract(primitive: ClusterPrimitive) -> &'static str {
    match primitive {
        ClusterPrimitive::Cache => "cluster/ClusterCacheApi@v1",
        ClusterPrimitive::Lock => "cluster/DistributedLockApi@v1",
        ClusterPrimitive::LeaderElection => "cluster/LeaderElectionApi@v1",
    }
}

/// Remote cluster bindings: a requester in an application without the
/// `cluster` gear reaches it over gRPC, addressed by the SDK
/// (ADR `cpt-gearbox-adr-product-placement` section 4).
///
/// Recorded only for requesters that are apart from cluster, so a product that
/// keeps them together resolves to the same lock as before. The checks are the
/// conditions under which the SDK's address is real: a Kubernetes profile
/// (GBX0415), one application serving cluster (GBX0416), and a hub in it
/// listening on the network (GBX0314).
pub fn remote_bindings(
    catalogue: &Catalogue,
    partition: &Partition,
    cluster: &[ResolvedClusterBinding],
    intent: &gearbox_ir::ProductIntent,
    declaration: &gearbox_ir::DeploymentProfileDecl,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Vec<gearbox_ir::ResolvedBinding> {
    let Ok(cluster_gear) = GearId::new(CLUSTER_GEAR) else {
        return Vec::new();
    };
    let hosting: Vec<&ResolvedApplication> = partition
        .applications
        .iter()
        .filter(|a| a.contains(&cluster_gear))
        .collect();
    let Some(serving) = hosting.first() else {
        return Vec::new();
    };

    // Each process linking cluster renders a Service named `cluster`, so on
    // Kubernetes two of them cannot install, whether or not anyone is remote.
    if hosting.len() > 1
        && matches!(
            declaration,
            gearbox_ir::DeploymentProfileDecl::Kubernetes { .. }
        )
    {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::ClusterInSeveralApplications,
                format!(
                    "`cluster` is linked into {}: each renders a Service named `cluster`, which \
                     the SDK addresses, and one chart cannot hold two",
                    hosting
                        .iter()
                        .map(|a| format!("`{}`", a.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "give `cluster` one application and reach it remotely from the others",
            )
            .at(Location::file(uri.to_owned())),
        );
    }

    // (requester, its application, contract), once each.
    let mut wanted: BTreeSet<(GearId, gearbox_ir::ApplicationId, &'static str)> = BTreeSet::new();
    for binding in cluster {
        for requester in &binding.requesters {
            for application in &partition.applications {
                if application.contains(requester) && !application.contains(&cluster_gear) {
                    wanted.insert((
                        requester.clone(),
                        application.name.clone(),
                        primitive_contract(binding.primitive),
                    ));
                }
            }
        }
    }
    if wanted.is_empty() {
        return Vec::new();
    }

    let gearbox_ir::DeploymentProfileDecl::Kubernetes { namespace, .. } = declaration else {
        let apps: BTreeSet<&gearbox_ir::ApplicationId> = wanted.iter().map(|w| &w.1).collect();
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::RemoteClusterNeedsKubernetes,
                format!(
                    "{} reach `cluster` in another process, but profile `{}` is not \
                     Kubernetes: the cluster SDK addresses it only by Kubernetes DNS",
                    apps.iter()
                        .map(|a| format!("`{a}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    declaration.id()
                ),
                "put `cluster` in the requester's application (`include = [\"cluster\"]`) \
                 for this profile, or deploy it on Kubernetes",
            )
            .with_evidence("gears/system/cluster/cluster-sdk/src/wiring.rs:123")
            .at(Location::file(uri.to_owned())),
        );
        return Vec::new();
    };
    let namespace = namespace.clone().unwrap_or_else(|| "default".to_owned());
    let Ok(hub) = GearId::new(GRPC_HUB) else {
        return Vec::new();
    };
    let hub_listen = intent
        .selected_gears
        .iter()
        .find(|g| g.gear == hub)
        .and_then(|g| g.config.get("listen_addr"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let off_network = hub_listen
        .as_deref()
        .is_some_and(|addr| addr == "none" || addr.starts_with("uds://"));
    if !serving.contains(&hub) || off_network {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::TopologyGrpcWithoutHub,
                format!(
                    "`cluster` in application `{}` is reached remotely, but {}",
                    serving.name,
                    if off_network {
                        format!(
                            "its gRPC hub listens on `{}`, which no other process can dial",
                            hub_listen.unwrap_or_default()
                        )
                    } else {
                        "that application has no gRPC hub".to_owned()
                    }
                ),
                "cluster's coordination plane is served by the application's gRPC hub on TCP; \
                 select `grpc-hub` into it with a TCP `listen_addr` (and platform enforcement)",
            )
            .with_evidence("libs/toolkit/src/runtime/host_runtime.rs:800")
            .at(Location::file(uri.to_owned())),
        );
    }

    // The SDK derives this address itself. It is pinned only under static
    // discovery, where there is no directory to wait for: the runtime then marks
    // the dependency resolved at wiring, as for any static endpoint
    // (ADR-0026 section 5). Under directory discovery readiness waits for the
    // directory to know `cluster`.
    let endpoint = (declaration.discovery() == Some(gearbox_ir::Discovery::Static)).then(|| {
        format!("http://{CLUSTER_GEAR}.{namespace}.svc.cluster.local:{CLUSTER_GRPC_PORT}")
    });
    // The SDK's remote client needs each primitive's service and the profile
    // service over gRPC; a declaration that does not say so is not a binding the
    // runtime can make, and is refused rather than recorded.
    let over_grpc = |contract: &str| {
        catalogue.gears.get(&cluster_gear).is_some_and(|gear| {
            gear.provides.iter().any(|p| {
                p.contract.as_str() == contract
                    && p.transports.contains(&gearbox_ir::Transport::Grpc)
            })
        })
    };
    let needed: BTreeSet<&str> = wanted
        .iter()
        .map(|w| w.2)
        .chain(std::iter::once(CLUSTER_PROFILE_CONTRACT))
        .collect();
    let missing: Vec<&str> = needed.into_iter().filter(|c| !over_grpc(c)).collect();
    if !missing.is_empty() {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::RemoteClusterWithoutGrpc,
                format!(
                    "requesters reach `cluster` remotely, but it provides {} over no gRPC \
                     transport",
                    missing
                        .iter()
                        .map(|c| format!("`{c}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "declare the provider with `grpc = grpc(...)` in cluster's `gear.gdl` (and \
                 `#[toolkit::provides]` in its code), or keep the requesters with `cluster`",
            )
            .with_evidence("gears/system/cluster/cluster-sdk/src/client/remote.rs")
            .at(Location::file(uri.to_owned())),
        );
        return Vec::new();
    }
    wanted
        .into_iter()
        .filter_map(|(requester, application, contract)| {
            let contract = gearbox_ir::ContractId::new(contract).ok()?;
            catalogue
                .contracts
                .contains_key(&contract)
                .then(|| gearbox_ir::ResolvedBinding {
                    consumer: requester,
                    consumer_application: application,
                    contract,
                    provider: cluster_gear.clone(),
                    provider_application: serving.name.clone(),
                    mode: gearbox_ir::ResolvedBindingMode::Remote,
                    transport: gearbox_ir::Transport::Grpc,
                    mechanism: gearbox_ir::BindingMechanism::ProvidesClientWiring,
                    endpoint_source: Some(
                        "cluster SDK: cluster.$POD_NAMESPACE.svc.cluster.local:50051".to_owned(),
                    ),
                    endpoint: endpoint.clone(),
                    // Cluster requirements have no optional form; written once per
                    // process as `gears.cluster-sdk.config.consumer_wiring.cluster`.
                    critical: true,
                    // Decided once every binding is known; see `resolve::resolve_at`.
                    gates_readiness: false,
                    selected: Selected::auto(),
                })
        })
        .collect()
}

/// Everything one decision needs, so the signature stays readable.
struct Context<'a> {
    scope: &'a str,
    primitive: ClusterPrimitive,
    need: &'a Need,
    providers: &'a [ClusterProviderDecl],
    /// Providers already chosen elsewhere in this scope.
    already: &'a BTreeSet<String>,
    /// This scope's cache binding, whatever primitive is being decided.
    ///
    /// The compare-and-swap default *is* the cache, so a credential the cache
    /// binding names is the credential that default uses -- but only when it
    /// names the same backend. See [`credential_source`].
    declared_cache: Option<&'a ProviderBinding>,
    /// The provider this scope's cache resolved to, if it resolved to one.
    ///
    /// What the compare-and-swap default is layered over. Read from the cache
    /// binding by name, not inferred from [`Context::already`].
    resolved_cache: Option<&'a str>,
    /// Whether the topology has more than one process or any replication.
    spread: bool,
    prefer_existing: bool,
    uri: &'a str,
    /// Where this scope's `cluster_profile(...)` was written, when the
    /// description declared one.
    ///
    /// The scope rather than the individual `provider(...)`: the failing
    /// primitive may be `cache`, `leader_election` or `lock`, and pointing at
    /// the cache binding when the lock is at fault would underline the wrong
    /// line. The `cluster_profile(...)` span contains all three, which is the
    /// call-level answer `cpt-gearbox-adr-gdl-language-server` settles on.
    declared_at: Option<&'a Location>,
}

/// Group the closure's cluster requirements by scope and primitive.
fn collect_needs(
    catalogue: &Catalogue,
    closure: &Closure,
) -> BTreeMap<(String, ClusterPrimitive), Need> {
    let mut needs: BTreeMap<(String, ClusterPrimitive), Need> = BTreeMap::new();
    for gear in closure.members.keys() {
        let Some(descriptor) = catalogue.gears.get(gear) else {
            continue;
        };
        for requirement in &descriptor.requires {
            let RequirementKind::Cluster { primitive, scope } = &requirement.kind else {
                continue;
            };
            let entry = needs.entry((scope.clone(), *primitive)).or_default();
            entry.requesters.insert(gear.clone());
            // Unioned: two gears sharing a scope both get what they asked for,
            // and a provider must satisfy the union or satisfy neither.
            entry
                .capabilities
                .extend(requirement.capabilities.iter().cloned());
        }
    }
    needs
}

/// The providers registered by gears in the product.
///
/// **A registration this build will not contain is dropped here**, before
/// anything can choose it. Refusing an explicit binding is not enough: the
/// resolver picks a provider on its own when a scope leaves a primitive
/// unbound, and the tree's first native leader election sits behind a cargo
/// feature -- so a build without that feature would have had one chosen for it,
/// resolved cleanly, written to the lock, and elected one leader per replica at
/// run time. Silent, which is the failure `GBX0503` exists to prevent one level
/// up.
///
/// The gated *primitive* is removed rather than the whole provider: a plugin
/// registers several, and one gated primitive says nothing about the others.
fn provider_table(
    catalogue: &Catalogue,
    closure: &Closure,
    intent: &gearbox_ir::ProductIntent,
) -> Vec<ClusterProviderDecl> {
    let mut table: Vec<ClusterProviderDecl> = closure
        .members
        .keys()
        .filter_map(|g| catalogue.gears.get(g).map(|gear| (g, gear)))
        .flat_map(|(id, gear)| {
            gear.cluster_providers
                .iter()
                .map(move |provider| in_this_build(provider, id, intent))
        })
        .collect();
    table.sort_by(|a, b| a.name.cmp(&b.name));
    table.dedup_by(|a, b| a.name == b.name);
    table
}

/// A provider with the primitives this build does not link removed.
fn in_this_build(
    provider: &ClusterProviderDecl,
    owner: &gearbox_ir::GearId,
    intent: &gearbox_ir::ProductIntent,
) -> ClusterProviderDecl {
    if provider.gated_by.is_empty() {
        return provider.clone();
    }
    let selected = |feature: &str| {
        intent
            .selected_gears
            .iter()
            .filter(|s| &s.gear == owner)
            .any(|s| s.features.iter().any(|f| f == feature))
    };
    let mut out = provider.clone();
    out.primitives
        .retain(|primitive| match provider.gated_by.get(primitive) {
            None => true,
            Some(gearbox_ir::FeatureGate::Feature(feature)) => selected(feature),
            // Unreadable: not linked as far as anything here can tell, and
            // `GBX0525` says so where a binding names it. Choosing it *for*
            // somebody, on a condition nobody could read, would be worse.
            Some(gearbox_ir::FeatureGate::Unreadable(_)) => false,
        });
    out.capabilities
        .retain(|primitive, _| out.primitives.contains(primitive));
    out
}

/// Whether the topology can put two things in different memory.
fn is_spread(partition: &Partition) -> bool {
    partition.applications.len() > 1
        || partition
            .applications
            .iter()
            .any(gearbox_ir::ResolvedApplication::is_replicated)
}

fn decide(
    ctx: &Context<'_>,
    declared: Option<&ProviderBinding>,
    diagnostics: &mut Diagnostics,
) -> ResolvedClusterBinding {
    let candidates: Vec<&ClusterProviderDecl> = ctx
        .providers
        .iter()
        .filter(|p| p.satisfies(ctx.primitive, &ctx.need.capabilities))
        .collect();

    let (resolved, selected) = match declared {
        Some(request) => explicit(ctx, request, &candidates, diagnostics),
        None => automatic(ctx, &candidates, diagnostics),
    };

    guard_process_local(ctx, &resolved, diagnostics);
    report_runtime_capabilities(ctx, &resolved, diagnostics);
    check_credentials(ctx, &resolved, declared, diagnostics);

    // The credential the check just accepted, so the generator writes the one
    // the resolver reasoned about rather than re-deriving it. Only for a
    // `Provider`: the compare-and-swap default is engaged by *omitting* the
    // key, so there is no section for a reference to appear in and recording
    // one on that binding would describe a line nothing writes.
    let secret_ref = declared.and_then(|d| d.secret_ref.clone()).or_else(|| {
        let ClusterResolution::Provider { name } = &resolved else {
            return None;
        };
        credential_source(ctx, name, None).and_then(|d| d.secret_ref.clone())
    });

    ResolvedClusterBinding {
        scope: ctx.scope.to_owned(),
        primitive: ctx.primitive,
        required_capabilities: ctx.need.capabilities.clone(),
        requesters: ctx.need.requesters.iter().cloned().collect(),
        selected,
        resolved,
        options: declared.map(|d| d.options.clone()).unwrap_or_default(),
        secret_ref,
    }
}

/// Where the credential for `provider` in this scope is named, if anywhere.
///
/// The primitive's own binding first. Failing that, the scope's cache binding
/// -- but **only when it names the same backend**. A `secret_ref` is a
/// credential for one provider, not a credential in general, and the scope's
/// cache is the only other binding that can be talking about the same one:
/// leader election and lock both fall back to a default layered over it.
///
/// Reading only the primitive's own binding is what refused a correctly
/// specified product. No provider registers leader election, so there is no
/// `leader_election = provider(...)` for a `secret_ref` to live on, and
/// demanding one asked the operator for something unspellable.
fn credential_source<'a>(
    ctx: &Context<'a>,
    provider: &str,
    declared: Option<&'a ProviderBinding>,
) -> Option<&'a ProviderBinding> {
    if let Some(own) = declared.filter(|d| d.secret_ref.is_some()) {
        return Some(own);
    }
    ctx.declared_cache
        .filter(|c| c.provider == provider && c.secret_ref.is_some())
}

/// Whether this scope could hand `provider` a credential if it chose it.
fn can_credential(ctx: &Context<'_>, provider: &ClusterProviderDecl) -> bool {
    !provider.needs_credentials || credential_source(ctx, &provider.name, None).is_some()
}

/// The description named a provider.
fn explicit(
    ctx: &Context<'_>,
    request: &ProviderBinding,
    candidates: &[&ClusterProviderDecl],
    diagnostics: &mut Diagnostics,
) -> (ClusterResolution, Selected<String>) {
    let registered = ctx.providers.iter().any(|p| p.name == request.provider);
    let selected = if !registered {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::ClusterUnregisteredProvider,
                format!(
                    "scope `{}` asks for the `{}` provider, which the runtime does not register",
                    ctx.scope, request.provider
                ),
                format!("registered providers are: {}", names(ctx.providers)),
            )
            .at(loc(ctx)),
        );
        Selected::downgraded(
            request.provider.clone(),
            DiagnosticCode::ClusterUnregisteredProvider,
        )
    } else if !candidates.iter().any(|p| p.name == request.provider) {
        diagnostics.push(unsatisfiable(ctx, Some(&request.provider)));
        Selected::downgraded(
            request.provider.clone(),
            DiagnosticCode::ClusterUnsatisfiable,
        )
    } else {
        Selected::honoured(request.provider.clone())
    };

    let resolved = if selected.was_downgraded() {
        ClusterResolution::Unsatisfied
    } else {
        ClusterResolution::Provider {
            name: request.provider.clone(),
        }
    };

    (resolved, selected)
}

/// Nothing was named, so the resolver ranks.
fn automatic(
    ctx: &Context<'_>,
    candidates: &[&ClusterProviderDecl],
    diagnostics: &mut Diagnostics,
) -> (ClusterResolution, Selected<String>) {
    let ranked = rank(ctx, candidates);

    // A backend this scope cannot hand a credential to is not a usable answer:
    // choosing it resolves the primitive and then refuses the product with
    // GBX0506. This is the whole of why the demo's requester declares no
    // `cluster.lock`. `postgres` is the only lock provider, so a lock
    // requirement auto-selected it into the single-process `dev` profile, where
    // the operator declared no provider for it and therefore no credentials --
    // an error on a product that was specified correctly and could not be
    // specified any other way, since `standalone` does not answer locks
    // (GBX0505) and naming postgres in `dev` would drag a real database into the
    // profile designed to need none.
    //
    // Which is worth refusing *for*, and that is the second half of the rule.
    // Where the SDK can build the primitive out of the cache there is a working
    // answer sitting right there, so take it. Where it cannot -- the cache
    // itself, since nothing is layered over a cache that does not exist -- take
    // the uncredentialled provider anyway and let [`check_credentials`] name
    // what is missing: "add a `secret_ref`" is an instruction the operator can
    // follow, and "no provider answers this" would simply be false.
    let has_cas_fallback = ctx.primitive != ClusterPrimitive::Cache && ctx.resolved_cache.is_some();
    let chosen = ranked
        .filter(|p| can_credential(ctx, p))
        .or(if has_cas_fallback { None } else { ranked });

    if let Some(chosen) = chosen {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::ClusterAutoSelected,
                format!(
                    "`{}` answers `{}` in scope `{}`",
                    chosen.name,
                    ctx.primitive.config_key(),
                    ctx.scope
                ),
            )
            .with_help(format!(
                "nothing named a provider, so one was ranked: {}. Name one with \
                 `cluster_profile(..., {} = provider(\"...\"))` to fix the choice",
                why(ctx, candidates),
                ctx.primitive.config_key()
            ))
            .at(loc(ctx)),
        );
        return (
            ClusterResolution::Provider {
                name: chosen.name.clone(),
            },
            Selected::auto(),
        );
    }

    // Nothing usable satisfies the primitive directly. For leader election and
    // lock the SDK layers a compare-and-swap implementation over the bound cache
    // -- which for leader election is always the answer, since no provider
    // registers it.
    if ctx.primitive != ClusterPrimitive::Cache
        && let Some(cache) = ctx.resolved_cache
    {
        let why_not = if ranked.is_some() {
            "a registered provider implements this primitive, but this scope names no credential \
             source it could use, so choosing it would resolve the primitive and then refuse the \
             product. The SDK builds the primitive from the cache's atomic operations instead"
        } else {
            "no registered provider implements this primitive, so the SDK builds it from the \
             cache's atomic operations"
        };
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::ClusterSdkDefault,
                format!(
                    "`{}` in scope `{}` resolves to the SDK's compare-and-swap default over the \
                     `{cache}` cache",
                    ctx.primitive.config_key(),
                    ctx.scope
                ),
            )
            .with_help(format!(
                "{why_not}; its guarantees are the cache's guarantees, which is a weaker promise \
                 than a purpose-built backend",
            ))
            // The code declares `requires_evidence`, and rightly: this is a claim
            // about what the runtime ships. The citation is the backend itself,
            // whose `features()` reads the cache's consistency rather than
            // declaring its own -- which is the same sentence the help gives, in
            // code.
            .with_evidence(
                "gears/system/cluster/cluster/src/defaults/leader.rs \
                 (CasBasedLeaderElectionBackend::features)",
            )
            .at(loc(ctx)),
        );
        return (
            ClusterResolution::SdkCasDefault {
                over_cache: cache.to_owned(),
            },
            Selected::auto(),
        );
    }

    diagnostics.push(unsatisfiable(ctx, None));
    (ClusterResolution::Unsatisfied, Selected::auto())
}

/// Deterministic ranking. No scoring, no search.
fn rank<'a>(
    ctx: &Context<'_>,
    candidates: &[&'a ClusterProviderDecl],
) -> Option<&'a ClusterProviderDecl> {
    let mut ordered: Vec<&ClusterProviderDecl> = candidates.to_vec();
    ordered.sort_by(|a, b| {
        // (a) something already in use in this scope, when asked for;
        let a_existing = ctx.prefer_existing && ctx.already.contains(&a.name);
        let b_existing = ctx.prefer_existing && ctx.already.contains(&b.name);
        // (b) a backend that survives being spread, when the topology spreads;
        let a_spreadable = !ctx.spread || !a.process_local;
        let b_spreadable = !ctx.spread || !b.process_local;
        b_existing
            .cmp(&a_existing)
            .then_with(|| b_spreadable.cmp(&a_spreadable))
            // (c) and finally the name, so the answer never depends on order.
            .then_with(|| a.name.cmp(&b.name))
    });
    ordered.first().copied()
}

/// The ranking, spelled out for the diagnostic.
fn why(ctx: &Context<'_>, candidates: &[&ClusterProviderDecl]) -> String {
    let names = candidates
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut reasons = vec![format!("candidates were {names}")];
    if ctx.prefer_existing && !ctx.already.is_empty() {
        reasons
            .push("`prefer.existing_infrastructure` favoured what the scope already uses".into());
    }
    if ctx.spread {
        reasons.push(
            "the topology has more than one application, so a process-local backend ranked last"
                .into(),
        );
    }
    reasons.push("ties broke on the name".into());
    reasons.join("; ")
}

/// Nothing can answer this. Show the per-provider comparison.
fn unsatisfiable(ctx: &Context<'_>, named: Option<&str>) -> Diagnostic {
    let required = ctx
        .need
        .capabilities
        .iter()
        .map(|c| c.as_str().to_owned())
        .collect::<Vec<_>>()
        .join(", ");
    let mut rows = Vec::new();
    for provider in ctx.providers {
        if !provider.primitives.contains(&ctx.primitive) {
            rows.push(format!(
                "  {}: does not answer {}",
                provider.name,
                ctx.primitive.config_key()
            ));
            continue;
        }
        let have = provider.capabilities_for(ctx.primitive);
        let missing: Vec<&str> = ctx
            .need
            .capabilities
            .iter()
            .filter(|c| !have.contains(*c))
            .map(CapabilityId::as_str)
            .collect();
        rows.push(if missing.is_empty() {
            format!("  {}: satisfies all of them", provider.name)
        } else if provider.runtime_determined.contains(&ctx.primitive) {
            // "missing" would be a half-truth here: the backend may well have
            // the capability, and decides on connecting. Saying so is the
            // difference between "cannot" and "cannot be known yet".
            format!(
                "  {}: declares none -- it decides them at run time, from the infrastructure it \
                 connects to, so it cannot promise {} in advance",
                provider.name,
                missing.join(", ")
            )
        } else {
            format!("  {}: missing {}", provider.name, missing.join(", "))
        });
    }

    let subject = named.map_or_else(
        || {
            format!(
                "no registered provider answers `{}`",
                ctx.primitive.config_key()
            )
        },
        |name| {
            format!(
                "the requested provider `{name}` cannot answer `{}`",
                ctx.primitive.config_key()
            )
        },
    );

    // **Name who asked.** The scope is not the asker, and reading this without
    // the asker is how it looked like a stray error: a product that had just
    // added `api-contracts-consumer` was told about `event-broker`, which is the
    // *scope* name the consumer declares its requirement under -- and no
    // `event-broker` gear was in the product at all.
    let asked_by = ctx
        .need
        .requesters
        .iter()
        .map(|gear| format!("`{gear}`"))
        .collect::<Vec<_>>()
        .join(", ");

    // **An empty table is a different answer, not a shorter one.** `rows` is
    // empty when the closure holds no provider gear, and the help then read
    // `per provider:` followed by nothing -- a heading for a list that does not
    // exist, which says less than saying so.
    let help = if rows.is_empty() {
        format!(
            "no cluster provider gear is in this product's closure, so there is nothing that \
             could answer `{}`; add a gear that provides it",
            ctx.primitive.config_key()
        )
    } else {
        format!("per provider:\n{}", rows.join("\n"))
    };

    Diagnostic::error(
        DiagnosticCode::ClusterUnsatisfiable,
        format!(
            "{subject} in scope `{}` with {{{required}}}, required by {asked_by}",
            ctx.scope
        ),
        help,
    )
    .at(loc(ctx))
}

/// The rule that catches a silent correctness bug.
fn guard_process_local(
    ctx: &Context<'_>,
    resolved: &ClusterResolution,
    diagnostics: &mut Diagnostics,
) {
    if !ctx.spread {
        return;
    }
    let Some(effective) = resolved.effective_provider() else {
        return;
    };
    let Some(provider) = ctx.providers.iter().find(|p| p.name == effective) else {
        return;
    };
    if !provider.process_local {
        return;
    }
    diagnostics.push(
        Diagnostic::error(
            DiagnosticCode::ClusterProcessLocalInMultiProcess,
            format!(
                "`{effective}` answers `{}` in scope `{}`, and its state lives inside one process",
                ctx.primitive.config_key(),
                ctx.scope
            ),
            "the topology has more than one application or a replicated one, so each copy gets its \
             own state: a lock locks nothing across applications, and leader election over such a \
             cache elects a leader per replica. The runtime starts this without complaint. \
             Choose a backend that is not process-local, or keep the requesters in one \
             unreplicated application",
        )
        .at(loc(ctx)),
    );
}

/// Say when the chosen backend will not know its own capabilities until it runs.
///
/// Reported here rather than at projection time, and only for a backend a scope
/// actually resolved onto. The fact belongs to a decision, not to the
/// catalogue: emitting it while projecting would put a permanent `info` on
/// every catalogue load about a provider no product need ever name.
fn report_runtime_capabilities(
    ctx: &Context<'_>,
    resolved: &ClusterResolution,
    diagnostics: &mut Diagnostics,
) {
    let Some(effective) = resolved.effective_provider() else {
        return;
    };
    let Some(provider) = ctx.providers.iter().find(|p| p.name == effective) else {
        return;
    };
    if !provider.runtime_determined.contains(&ctx.primitive) {
        return;
    }
    diagnostics.push(
        Diagnostic::new(
            DiagnosticCode::ClusterCapabilityRuntimeDetermined,
            format!(
                "`{effective}` answers `{}` in scope `{}` and decides its capabilities at run time",
                ctx.primitive.config_key(),
                ctx.scope
            ),
        )
        .with_help(
            "the backend reads them off the infrastructure it connects to, so nothing in Rust \
             states them and nothing here can. It answers the primitive and satisfies only a \
             requirement that asks for no capability -- if this scope needs a guarantee, name a \
             backend that declares it",
        )
        .with_evidence(
            "gears/system/cluster/plugins/redis-cluster-plugin/src/cache/mod.rs:347 \
             (consistency() returns what the startup preflight computed)",
        )
        .at(loc(ctx)),
    );
}

/// A provider that needs credentials must be told where they are.
fn check_credentials(
    ctx: &Context<'_>,
    resolved: &ClusterResolution,
    declared: Option<&ProviderBinding>,
    diagnostics: &mut Diagnostics,
) {
    let Some(effective) = resolved.effective_provider() else {
        return;
    };
    let Some(provider) = ctx.providers.iter().find(|p| p.name == effective) else {
        return;
    };
    // The scope's cache binding counts as a source for the primitive being
    // checked, but only when it names this same backend -- which is exactly the
    // compare-and-swap case, whose effective provider *is* the cache. Matching
    // on the resolution instead would accept the cache's credential for a
    // backend it is not a credential for: `over_cache` is whatever the cache
    // resolved to, and a scope may bind a different provider for another
    // primitive. [`credential_source`] compares the names rather than trusting
    // the shape.
    if !provider.needs_credentials || credential_source(ctx, effective, declared).is_some() {
        return;
    }
    diagnostics.push(
        Diagnostic::error(
            DiagnosticCode::ClusterNoCredentialSource,
            // The primitive is named, and it has to be. `Diagnostics::finish`
            // drops exact duplicates, so a scope whose cache and whose lock both
            // lack a source used to collapse into one complaint naming neither
            // -- the operator adds one `secret_ref`, resolves again, and meets
            // the same sentence.
            format!(
                "`{effective}` answers `{}` in scope `{}` and needs credentials, which the scope \
                 names no source for",
                ctx.primitive.config_key(),
                ctx.scope
            ),
            "add `secret_ref = \"...\"` to the provider; the reference is written into the \
             generated configuration, and the credential itself never enters the lock or any \
             values file",
        )
        .at(loc(ctx)),
    );
}

fn names(providers: &[ClusterProviderDecl]) -> String {
    if providers.is_empty() {
        return "none -- the product contains no gear that registers any".to_owned();
    }
    providers
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where a diagnostic about one cluster decision points.
fn loc(ctx: &Context<'_>) -> Location {
    Location::or_file(ctx.declared_at, ctx.uri)
}

/// Report a replicated stateful gear with no leader election in its scope.
///
/// Step 8, kept here because it reads the same bindings. Two copies of a
/// stateful gear both doing the work is not something the runtime notices.
pub fn report_stateful_replicas(
    catalogue: &Catalogue,
    partition: &Partition,
    bindings: &[ResolvedClusterBinding],
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    // Whether *this process* is covered by an election, not whether one exists
    // somewhere in the lock. A binding whose requesters all sit in other
    // processes elects nothing here, and silencing this warning on the strength
    // of it would hide the second copy of the work.
    //
    // Deliberately not conditioned on the cluster gear being in this process.
    // It serves its scopes over gRPC as well as in-process, so a replicated
    // process reaches an election that lives elsewhere; requiring co-location
    // here would warn about topologies that coordinate perfectly well.
    let elected = |application: &ResolvedApplication| {
        bindings.iter().any(|b| {
            b.primitive == ClusterPrimitive::LeaderElection
                && !matches!(b.resolved, ClusterResolution::Unsatisfied)
                && b.requesters.iter().any(|g| application.contains(g))
        })
    };

    for application in partition
        .applications
        .iter()
        .filter(|p| p.is_replicated() && !elected(p))
    {
        let stateful: Vec<&GearId> = application
            .gears
            .iter()
            .filter(|g| {
                catalogue
                    .gears
                    .get(*g)
                    .is_some_and(|d| d.runtime_caps.contains(&gearbox_ir::RuntimeCap::Stateful))
            })
            .collect();
        if stateful.is_empty() {
            continue;
        }
        let named = stateful
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::ClusterStatefulReplicasWithoutElection,
                format!(
                    "application `{}` runs {} copies of stateful gears with no leader election: \
                     {named}",
                    application.name, application.replicas
                ),
            )
            .with_help(
                "every copy will do the work: a reconciliation loop runs N times, a scheduled \
                 job fires N times. Add a `leader_election` binding to the gear's cluster \
                 scope, or run one replica",
            )
            // File-level, and not for lack of a span. The complaint names an
            // application and a remedy in a cluster scope, and neither is in
            // hand here -- `report_stateful_replicas` walks applications, not
            // scopes. Two candidate anchors and no way to tell which is at
            // fault is exactly when the file is the honest answer.
            .at(Location::file(uri.to_owned())),
        );
    }
}

/// Whether a selection was honoured, for the explanation graph.
#[must_use]
pub fn was_requested(binding: &ResolvedClusterBinding) -> bool {
    matches!(binding.selected.selected, Choice::Explicit { .. })
}
