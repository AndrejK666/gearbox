//! Step 6: how each declared contract edge is actually established.
//!
//! The mode is *derived from placement*, never declared. A description may ask
//! for local or remote, and the answer is whatever the topology produced -- which
//! is the only way the two can be guaranteed to agree. Asking and getting are
//! both recorded, so "you asked for gRPC and got REST" is an explanation rather
//! than a surprise.
//!
//! One asymmetry is worth stating because it looks like a bug. When consumer and
//! provider share a process the binding is local **whatever the request says**,
//! and no endpoint is written: the runtime's client hub finds the in-process
//! instance and short-circuits before any resolver is consulted. Configuring a
//! remote address there does not fail, it is simply ignored -- so writing one
//! would be writing a lie into the lock.

use gearbox_ir::{
    ApplicationId, BindingMechanism, BindingMode, BindingRequest, Catalogue, ContractDescriptor,
    ContractId, DeploymentProfileDecl, Diagnostic, DiagnosticCode, Diagnostics, Discovery, GearId,
    Location, NodeKind, ResolvedBinding, ResolvedBindingMode, Selected, Transport, binding_key,
};

use super::cuts::Cuts;
use super::partition::Partition;
use super::profile::ProfileScoped;

/// Derive a binding for every severable edge, plus the forced-local ones.
pub fn derive(
    catalogue: &Catalogue,
    cuts: &Cuts,
    partition: &Partition,
    scoped: &ProfileScoped<'_>,
    declaration: &DeploymentProfileDecl,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Vec<ResolvedBinding> {
    let mut bindings = Vec::new();

    for edge in &cuts.cuttable {
        let Some(contract) = catalogue.contracts.get(&edge.contract) else {
            continue;
        };
        let request = requested(scoped, &edge.consumer, &edge.contract);

        // One binding per process the consumer is in. A gear linked into
        // several binaries calls from each of them, and each needs its own
        // answer -- local where the provider is beside it, wired where it is
        // not. Recording only the first would leave the other copies with no
        // wiring at all.
        let Some(provider_home) = application_of(partition, &edge.provider) else {
            continue;
        };
        for consumer_app in partition
            .applications
            .iter()
            .filter(|a| a.contains(&edge.consumer))
        {
            // Placement decides the mode. Both being in one process is not a
            // preference the resolver expressed; it is where they ended up.
            let together = consumer_app.contains(&edge.provider);
            let consumer_application = consumer_app.name.clone();
            let provider_application = if together {
                consumer_application.clone()
            } else {
                provider_home.clone()
            };

            let binding = if together {
                let forced_by = request
                    .is_some_and(|r| r.mode == BindingMode::Remote)
                    .then(|| {
                        why_together(
                            catalogue,
                            scoped,
                            &edge.consumer,
                            &edge.provider,
                            &consumer_application,
                        )
                    });
                local(
                    edge,
                    contract,
                    consumer_application,
                    request,
                    forced_by
                        .as_deref()
                        .filter(|_| !matches!(declaration, DeploymentProfileDecl::Embedded { .. })),
                    uri,
                    diagnostics,
                )
            } else {
                if request.is_some_and(|r| r.mode == BindingMode::Local) {
                    report_local_contradicted(
                        edge,
                        scoped,
                        &provider_application,
                        uri,
                        diagnostics,
                    );
                }
                remote(
                    edge,
                    contract,
                    consumer_application,
                    provider_application,
                    request,
                    declaration,
                    uri,
                    diagnostics,
                )
            };
            bindings.push(binding);
        }
    }

    if !matches!(declaration, DeploymentProfileDecl::Embedded { .. }) {
        report_remote_forced_by_deps(catalogue, cuts, scoped, uri, diagnostics);
    }
    report_unhonoured_endpoints(scoped, uri, diagnostics);

    bindings.sort_by(|a, b| {
        (&a.consumer, &a.contract, &a.consumer_application).cmp(&(
            &b.consumer,
            &b.contract,
            &b.consumer_application,
        ))
    });
    bindings
}

/// Refuse a `bind(endpoint = ...)`, because nothing here reads it.
///
/// Over every declaration rather than only the ones that matched an edge: the
/// complaint is that the *address* does nothing, and that is true whether or
/// not the edge resolved. A person who wrote one is owed the news either way.
fn report_unhonoured_endpoints(
    scoped: &ProfileScoped<'_>,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for binding in &scoped.bindings {
        let Some(endpoint) = &binding.endpoint else {
            continue;
        };
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::BindingEndpointNotHonoured,
                format!(
                    "`{}` binds `{}` with endpoint `{endpoint}`, which this build does not \
                     honour",
                    binding.consumer, binding.contract
                ),
                "remove the `endpoint` argument: the address a consumer reaches a provider at \
                 is derived from where the resolver placed them, and an address written here \
                 is read by nothing. Pointing at a provider outside the product is not \
                 expressible yet",
            )
            .at(gearbox_ir::Location::or_file(
                binding.declared_at.as_ref(),
                uri,
            )),
        );
    }
}

/// The process a gear runs in, preferring the one it was placed in first.
///
/// An overlapping gear is in several; the host is the answer that matters for a
/// binding, and the host is always first.
fn application_of(partition: &Partition, gear: &GearId) -> Option<ApplicationId> {
    partition
        .applications
        .iter()
        .find(|p| p.contains(gear))
        .map(|p| p.name.clone())
}

/// What the description asked for about this edge, if anything.
fn requested(
    scoped: &ProfileScoped<'_>,
    consumer: &GearId,
    contract: &ContractId,
) -> Option<BindingRequest> {
    scoped
        .bindings
        .iter()
        .find(|b| b.consumer == *consumer && b.contract == *contract)
        .map(|b| BindingRequest {
            mode: b.mode,
            transport: b.transport,
            critical: b.critical,
        })
}

/// What puts `provider` in `consumer`'s application: an `include`, a `deps`
/// chain, or (failing both) the default host gathering both.
fn why_together(
    catalogue: &Catalogue,
    scoped: &ProfileScoped<'_>,
    consumer: &GearId,
    provider: &GearId,
    application: &ApplicationId,
) -> String {
    if let Some(pin) = scoped
        .application_pins
        .iter()
        .find(|p| p.name == *application && p.include.contains(provider))
    {
        return format!("`application(\"{}\")` includes `{provider}`", pin.name);
    }
    if let Some(chain) = deps_chain(catalogue, consumer, provider) {
        return format!("the `deps` chain {}", chain.join(" -> "));
    }
    format!("both are placed in application `{application}`")
}

/// The shortest `deps` path from `from` to `to`, by gear id.
fn deps_chain(catalogue: &Catalogue, from: &GearId, to: &GearId) -> Option<Vec<String>> {
    let mut previous: std::collections::BTreeMap<&GearId, &GearId> =
        std::collections::BTreeMap::new();
    let mut queue = std::collections::VecDeque::from([from]);
    let mut seen = std::collections::BTreeSet::from([from]);
    while let Some(gear) = queue.pop_front() {
        if gear == to {
            let mut chain = vec![to.to_string()];
            let mut at = to;
            while let Some(prev) = previous.get(at) {
                chain.push(prev.to_string());
                at = prev;
            }
            chain.reverse();
            return Some(chain);
        }
        for dep in catalogue
            .gears
            .get(gear)
            .into_iter()
            .flat_map(|g| &g.colocated_deps)
        {
            if seen.insert(dep) {
                previous.insert(dep, gear);
                queue.push_back(dep);
            }
        }
    }
    None
}

/// `bind(mode = remote)` on an edge a `deps` chain makes unseverable: the
/// provider is linked into the consumer's binary whatever the placement says, so
/// no binding is even derived (GBX0413; GBX0407 still describes the edge).
fn report_remote_forced_by_deps(
    catalogue: &Catalogue,
    cuts: &Cuts,
    scoped: &ProfileScoped<'_>,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for request in scoped
        .bindings
        .iter()
        .filter(|b| b.mode == BindingMode::Remote)
    {
        let Some(blocked) = cuts.blocked.iter().find(|c| {
            c.consumer == request.consumer
                && c.contract.as_ref() == Some(&request.contract)
                && c.blocked_by == gearbox_ir::CutBlocker::ColocationClosure
        }) else {
            continue;
        };
        let chain = deps_chain(catalogue, &blocked.consumer, &blocked.provider).map_or_else(
            || format!("`{}` co-locates `{}`", blocked.consumer, blocked.provider),
            |chain| format!("the `deps` chain {}", chain.join(" -> ")),
        );
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::BindingModeContradicted,
                format!(
                    "`{}` asks for `{}` remotely, but {chain} links `{}` into its binary",
                    request.consumer, request.contract, blocked.provider
                ),
                "the client hub finds the in-process instance before any remote client runs; \
                 make the edge severable at its source (`#[toolkit::consumes]` plus \
                 `consume(...)` instead of `deps`), or drop `mode = binding_mode.remote`",
            )
            .at(Location::or_file(request.declared_at.as_ref(), uri)),
        );
    }
}

/// `bind(mode = local)` that placement could not honour: an `application(...)`
/// pins the provider elsewhere (GBX0413).
fn report_local_contradicted(
    edge: &super::cuts::CuttableEdge,
    scoped: &ProfileScoped<'_>,
    provider_application: &ApplicationId,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    let because = scoped
        .application_pins
        .iter()
        .find(|p| p.anchor == edge.provider)
        .map_or_else(
            || {
                format!(
                    "`{}` is isolated in application `{provider_application}`",
                    edge.provider
                )
            },
            |pin| {
                format!(
                    "`application(\"{}\", anchor = \"{}\")` pins it there",
                    pin.name, pin.anchor
                )
            },
        );
    diagnostics.push(
        Diagnostic::error(
            DiagnosticCode::BindingModeContradicted,
            format!(
                "`{}` asks for `{}` locally, but `{}` is in application `{provider_application}`: {because}",
                edge.consumer, edge.contract, edge.provider
            ),
            "drop the pin (or the isolate) to let the provider join its consumer, or drop \
             `mode = binding_mode.local`",
        )
        .at(Location::file(uri.to_owned())),
    );
}

/// Consumer and provider share a binary.
fn local(
    edge: &super::cuts::CuttableEdge,
    contract: &ContractDescriptor,
    application: ApplicationId,
    request: Option<BindingRequest>,
    forced_by: Option<&str>,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> ResolvedBinding {
    // `mode = remote` against a placement that puts them together: a
    // contradiction outside an embedded profile (GBX0413), an inapplicable
    // request inside one (GBX0407, below).
    if let Some(because) = forced_by {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::BindingModeContradicted,
                format!(
                    "`{}` asks for `{}` remotely, but `{}` is in its own application `{application}`: {because}",
                    edge.consumer, edge.contract, edge.provider
                ),
                "the client hub finds the in-process instance before any remote client runs; \
                 make the edge severable at its source (`#[toolkit::consumes]` plus \
                 `consume(...)` instead of `deps`), drop the `include`, or drop \
                 `mode = binding_mode.remote`",
            )
            .at(Location::file(uri.to_owned())),
        );
    }
    // A request to separate them that placement did not honour. Recorded on the
    // binding *and* reported, because the lock alone would show a local binding
    // with no hint that anyone wanted otherwise.
    let downgraded = request.is_some_and(|r| r.mode == BindingMode::Remote) && forced_by.is_none();
    if downgraded {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::BindingForcedLocal,
                format!(
                    "`{}` asked for a remote binding of `{}`, but both gears are in application `{}`",
                    edge.consumer, edge.contract, application
                ),
            )
            .with_help(
                "the client hub finds the in-process instance and short-circuits before any \
                 endpoint is consulted, so a remote address here would be ignored rather than \
                 used; separating them needs a profile with more than one application",
            )
            .at(Location::file(uri.to_owned())),
        );
    }

    ResolvedBinding {
        consumer: edge.consumer.clone(),
        consumer_application: application.clone(),
        contract: contract.id.clone(),
        provider: edge.provider.clone(),
        provider_application: application,
        mode: ResolvedBindingMode::Local,
        transport: Transport::Local,
        mechanism: BindingMechanism::colocated(),
        // Deliberately absent, not empty: there is no endpoint, and writing one
        // would record a value the runtime never reads.
        endpoint_source: None,
        endpoint: None,
        // The product's `bind(critical = ...)` wins over the gear's default.
        critical: request.and_then(|r| r.critical).unwrap_or(edge.critical),
        // Decided once every binding is known; see `resolve::resolve_at`.
        gates_readiness: false,
        selected: selection(
            request,
            if forced_by.is_some() {
                Some(DiagnosticCode::BindingModeContradicted)
            } else {
                downgraded.then_some(DiagnosticCode::BindingForcedLocal)
            },
        ),
    }
}

/// Consumer and provider are in different binaries.
#[expect(
    clippy::too_many_arguments,
    reason = "each is a distinct fact the binding records; a bundle would be named after this \
              function and explain nothing"
)]
fn remote(
    edge: &super::cuts::CuttableEdge,
    contract: &ContractDescriptor,
    consumer_application: ApplicationId,
    provider_application: ApplicationId,
    request: Option<BindingRequest>,
    declaration: &DeploymentProfileDecl,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> ResolvedBinding {
    // REST is the only transport a severed declared edge can carry: the
    // consumption macro emits a REST resolving client and there is no other path.
    // Cross-process gRPC exists in the runtime, but only through hand-written
    // wiring the generator does not produce.
    // `mode = local` that placement could not honour; reported by the caller.
    let mut downgrade = request
        .is_some_and(|r| r.mode == BindingMode::Local)
        .then_some(DiagnosticCode::BindingModeContradicted);
    if request.and_then(|r| r.transport) == Some(Transport::Grpc) {
        downgrade = Some(DiagnosticCode::BindingGrpcUnsupported);
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::BindingGrpcUnsupported,
                format!(
                    "`{}` asked for gRPC on `{}`, which crosses a process boundary; using REST",
                    edge.consumer, edge.contract
                ),
            )
            .with_help(
                "`#[toolkit::consumes]` emits a REST resolving client and nothing else, so a \
                 severed edge has no gRPC path; the gRPC that exists in the runtime is \
                 hand-written wiring the generator does not produce",
            )
            .at(Location::file(uri.to_owned())),
        );
    }

    let discovery = declaration.discovery().unwrap_or(Discovery::Static);
    let mechanism = BindingMechanism::for_discovery(discovery);
    // Where the address comes from, named rather than resolved: the value itself
    // is a deployment concern, and pinning it here would make the lock
    // environment-specific. The static key is the one the runtime actually
    // reads -- `gears.{consumer}.config.consumer_wiring.{provider}` -- not a
    // contract-derived name. Two majors of one provider therefore share a
    // slot; the lock must not pretend they are independently addressable.
    let endpoint_source = match discovery {
        Discovery::Static => static_endpoint_source(&edge.consumer, &edge.provider),
        Discovery::Directory => format!("directory:{provider_application}"),
    };

    ResolvedBinding {
        consumer: edge.consumer.clone(),
        consumer_application,
        contract: contract.id.clone(),
        provider: edge.provider.clone(),
        provider_application,
        mode: ResolvedBindingMode::Remote,
        transport: Transport::Rest,
        mechanism,
        endpoint_source: Some(endpoint_source),
        endpoint: None,
        // The product's `bind(critical = ...)` wins over the gear's default.
        critical: request.and_then(|r| r.critical).unwrap_or(edge.critical),
        // Decided once every binding is known; see `resolve::resolve_at`.
        gates_readiness: false,
        selected: selection(request, downgrade),
    }
}

/// Fill `endpoint` for static discovery under Kubernetes.
///
/// Directory discovery finds the address at runtime; writing one into the lock
/// would record a guess as a decision. Static discovery under Kubernetes is
/// the opposite: the only resolver the runtime has is a pinned URI, and the
/// URI is a pure function of the process's Service DNS name. One-machine
/// static profiles still leave this empty -- their address is a local spawn
/// concern, not a cluster name.
pub fn pin_static_endpoints(
    bindings: &mut [ResolvedBinding],
    partition: &Partition,
    declaration: &DeploymentProfileDecl,
) {
    if !matches!(
        declaration,
        DeploymentProfileDecl::Kubernetes {
            discovery: Discovery::Static,
            ..
        }
    ) {
        return;
    }
    let namespace = match declaration {
        DeploymentProfileDecl::Kubernetes { namespace, .. } => {
            namespace.as_deref().unwrap_or("default")
        }
        _ => return,
    };
    for binding in bindings.iter_mut().filter(|b| b.is_remote()) {
        let Some(provider) = partition
            .applications
            .iter()
            .find(|application| application.name == binding.provider_application)
        else {
            continue;
        };
        binding.endpoint = super::partition::cluster_dns(provider, namespace);
    }
}

/// Config key the runtime reads for a static endpoint override.
///
/// `StaticEndpointResolver` looks up
/// `gears.{consumer}.config.consumer_wiring.{provider}`, keyed by the *provider
/// gear name*, not by `ContractDescriptor::wiring_key`. `PaymentApi@v1` and
/// `@v2` therefore collapse to one `consumer_wiring.api-contracts` entry.
fn static_endpoint_source(consumer: &GearId, provider: &GearId) -> String {
    format!("gears.{consumer}.config.consumer_wiring.{provider}")
}

fn selection(
    request: Option<BindingRequest>,
    downgraded_by: Option<DiagnosticCode>,
) -> Selected<BindingRequest> {
    request.map_or_else(Selected::auto, |value| match downgraded_by {
        Some(code) => Selected::downgraded(value, code),
        None => Selected::honoured(value),
    })
}

/// Report a static endpoint override the environment cannot express.
///
/// The runtime's environment-key remapping turns underscores into hyphens only
/// in the segment immediately after the gears prefix. Nested keys are not
/// remapped, so a hyphenated *provider gear name* (`api-contracts`) can never
/// be matched by an environment variable. A single-segment name (`billing`)
/// can. The override has to be written into configuration instead -- which the
/// generator does, but an operator expecting to set it at deploy time needs
/// telling.
pub fn report_env_limits(bindings: &[ResolvedBinding], uri: &str, diagnostics: &mut Diagnostics) {
    for binding in bindings
        .iter()
        .filter(|b| b.mechanism == BindingMechanism::ConsumesStatic)
    {
        if !binding.provider.as_str().contains('-') {
            continue;
        }
        let key = static_endpoint_source(&binding.consumer, &binding.provider);
        let mut diagnostic = Diagnostic::new(
            DiagnosticCode::BindingEnvCannotExpressWiring,
            format!(
                "the endpoint override for `{}` on `{}` cannot come from an environment variable",
                binding.consumer, binding.contract
            ),
        )
        .with_help(format!(
            "the key is `{key}`, and the runtime's environment remapping converts underscores \
             to hyphens only in the segment right after the gears prefix, so a nested \
             hyphenated provider name never matches; the generator writes it into the \
             configuration file instead"
        ))
        .with_evidence("libs/toolkit/src/bootstrap/config/mod.rs:429")
        .at(Location::file(uri.to_owned()));
        // `subject` is documented as "the graph node this concerns, so a client can
        // select it", and until now nothing set it on any diagnostic --
        // `Diagnostic::about` existed with no callers at all, so Studio's Conflicts
        // screen had no node to offer an explanation for.
        //
        // This diagnostic because its subject is unambiguous: the complaint is
        // about exactly one binding, named by consumer and contract, and the
        // explanation graph holds a node for every resolved binding under the same
        // key -- `binding_key`, which is now written down once for both. Most other
        // diagnostics are about a resolution as a whole ("two severable edges stay
        // local because the profile is single-process" concerns no single node), and
        // inventing a subject for those would send a reader to a node that does not
        // explain them.
        //
        // The `if let` rather than an `about(...)` that takes an `Option`: an id
        // that cannot be constructed is not an error here, it is a diagnostic
        // without a subject, which is the ordinary case everywhere else.
        if let Some(node) = NodeKind::Binding.id_for(&binding_key(
            binding.consumer.as_str(),
            binding.contract.as_str(),
        )) {
            diagnostic = diagnostic.about(node);
        }
        diagnostics.push(diagnostic);
    }
}

/// Report a `consumer_wiring` key that names a provider the consumer did not
/// declare.
///
/// Takes the catalogue because the declared provider is not on the binding:
/// `ResolvedBinding::provider` is the gear `select_provider` chose, and the
/// edge does not keep the declared one either -- selection consumes it and
/// returns only the winner. The declared one is on the consumer's own
/// `Requirement`.
///
/// That field is projected from `#[toolkit::consumes(from = ...)]`, which is
/// the same string the macro emits as `dep_gear`. It used to be the
/// description's restatement of it, and comparing against a restatement would
/// have been comparing against the wrong thing.
pub fn report_wiring_key_skew(
    catalogue: &Catalogue,
    bindings: &[ResolvedBinding],
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for binding in bindings
        .iter()
        .filter(|b| b.mechanism == BindingMechanism::ConsumesStatic)
    {
        let Some(consumer) = catalogue.gears.get(&binding.consumer) else {
            continue;
        };
        let Some(declared) = consumer
            .consumes
            .iter()
            .find(|r| r.contract() == Some(&binding.contract))
            .and_then(gearbox_ir::Requirement::declared_provider)
        else {
            continue;
        };
        if *declared == binding.provider {
            continue;
        }

        let mut diagnostic = Diagnostic::error(
            DiagnosticCode::BindingWiringKeySkew,
            format!(
                "the endpoint override for `{}` on `{}` is written as `{}`, and the runtime \
                 reads `{}` -- the name `#[toolkit::consumes(from = \"{declared}\")]` emitted",
                binding.consumer,
                binding.contract,
                static_endpoint_source(&binding.consumer, &binding.provider),
                static_endpoint_source(&binding.consumer, declared),
            ),
            format!(
                "`{declared}` does not provide `{}` in this product and `{}` does, which \
                 GBX0404 already reported. Point `#[toolkit::consumes(..., from = \"{}\")]` \
                 at the gear that provides it, or select `{declared}` with `use_gear`",
                binding.contract, binding.provider, binding.provider
            ),
        )
        .with_evidence(
            "libs/toolkit-contract-macros/src/consumes.rs \
             (ConsumesAttr.from, emitted verbatim as dep_gear)",
        )
        .at(Location::file(uri.to_owned()));
        if let Some(node) = NodeKind::Binding.id_for(&binding_key(
            binding.consumer.as_str(),
            binding.contract.as_str(),
        )) {
            diagnostic = diagnostic.about(node);
        }
        diagnostics.push(diagnostic);
    }
}
