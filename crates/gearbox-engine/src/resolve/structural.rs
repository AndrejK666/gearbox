//! Step 5: constraints the runtime imposes on a topology.
//!
//! Every check here mirrors something the platform actually does, and the value
//! of the step is that it says so *before* a binary is built rather than when one
//! fails to start. The two that matter most are the ones the runtime enforces
//! silently: a second REST host in a process, which the registry rejects, and a
//! REST host inside a worker, which starts happily and then never receives
//! traffic because a worker serves through its own out-of-process router.
//!
//! Directory discovery has two prerequisites that are easy to miss because
//! neither is named in the description: the directory server itself, and the gRPC
//! hub the spawn phase blocks on while waiting for its endpoint. Missing either
//! produces a host that hangs at startup, which is the least debuggable failure
//! this resolver can prevent.

use std::collections::BTreeMap;

use gearbox_ir::{
    ApplicationId, ApplicationKind, Catalogue, DeploymentProfileDecl, Diagnostic, DiagnosticCode,
    Diagnostics, Discovery, GearId, ResolvedApplication, RestPlane, RuntimeCap,
};

use super::partition::Partition;

/// The gear that answers directory lookups.
const DIRECTORY_SERVER: &str = "service-discovery";
/// The gear that publishes the endpoint the spawn phase waits for.
const GRPC_HUB: &str = "grpc-hub";

/// Check the resolved topology against what the runtime will accept.
pub fn check(
    catalogue: &Catalogue,
    partition: &Partition,
    declaration: &DeploymentProfileDecl,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for application in &partition.applications {
        check_singletons(catalogue, application, uri, diagnostics);
        check_worker_shape(application, uri, diagnostics);
        check_rest_without_host(catalogue, application, uri, diagnostics);
        check_tenant_authenticator(catalogue, application, uri, diagnostics);
        check_grpc_without_hub(catalogue, application, uri, diagnostics);
        check_host_with_nothing_to_host(catalogue, application, uri, diagnostics);
        check_replicated_singletons(catalogue, application, uri, diagnostics);
    }
    check_duplicate_registrations(catalogue, partition, uri, diagnostics);
    check_discovery(partition, declaration, uri, diagnostics);
    check_worker_paths(partition, declaration, uri, diagnostics);
    report_spawn_gap(partition, declaration, uri, diagnostics);
}

/// Replicas of a gear only one of which may run in an installation.
///
/// A replica is another process registering the same name from its own pod, so
/// replicating such a gear is the same defect as pinning it out twice, reached
/// by the other road. Reported under the same code for that reason.
///
/// An error rather than the warning an embedded `replicas > 1` gets
/// (`GBX0307`, where the product is still buildable as one process): here the
/// topology resolves, builds, starts, reports healthy, and answers wrongly.
fn check_replicated_singletons(
    catalogue: &Catalogue,
    application: &ResolvedApplication,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    if application.replicas < 2 {
        return;
    }
    for gear in &application.gears {
        if !catalogue
            .gears
            .get(gear)
            .is_some_and(|g| g.one_per_installation)
        {
            continue;
        }
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::TopologyDuplicateRegistration,
                format!(
                    "`{}` runs {} replicas and holds `{gear}`, of which only one may run in an \
                     installation",
                    application.name, application.replicas
                ),
                format!(
                    "every replica registers `{gear}` from its own process, so a consumer \
                     resolving it would round-robin between copies that own separate state. Run \
                     one replica, or move `{gear}` to an application that is not replicated"
                ),
            )
            .at(Location(uri)),
        );
    }
}

/// Names registered in the directory by two applications at once.
///
/// **Counted on registration, not on linking**, and the difference is the whole
/// check. A worker registers one name -- `registers_as()`, its anchor's or its
/// role's -- and nothing else it contains. A host registers one per REST
/// provider it holds, plus one per gRPC provider, each under that gear's own
/// name. So a gear compiled into several binaries produces no second
/// registration by itself; what does is a gear the host's closure reaches
/// *and* a description forces out, which `split` keeps as a worker anchor
/// while the host still holds it.
///
/// A consumer resolving that name then round-robins between two endpoints.
/// Ordinarily that is load balancing and this says so once; for a gear the
/// platform declared `one_per_installation` the two endpoints own disjoint
/// state and it is a refusal.
///
/// Outside the per-application loop because the count is a property of the
/// partition. Under an embedded profile there is one application, so this
/// cannot fire.
fn check_duplicate_registrations(
    catalogue: &Catalogue,
    partition: &Partition,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    // name -> the applications registering it, in partition order.
    let mut by_name: BTreeMap<&str, Vec<&ApplicationId>> = BTreeMap::new();
    for application in &partition.applications {
        for name in registered_names(catalogue, application) {
            by_name.entry(name).or_default().push(&application.name);
        }
    }

    for (name, applications) in by_name {
        if applications.len() < 2 {
            continue;
        }
        let where_ = applications
            .iter()
            .map(|a| format!("`{a}`"))
            .collect::<Vec<_>>()
            .join(" and ");
        let one_per_installation = GearId::new(name)
            .ok()
            .and_then(|id| catalogue.gears.get(&id))
            .is_some_and(|gear| gear.one_per_installation);

        let mut diagnostic = if one_per_installation {
            Diagnostic::error(
                DiagnosticCode::TopologyDuplicateRegistration,
                format!(
                    "{where_} both register `{name}` in the directory, and only one of it may \
                     run in an installation"
                ),
                format!(
                    "a consumer resolving `{name}` would round-robin between two endpoints that \
                     own separate state. Drop the `application(...)` pin or the `prefer.isolate` \
                     that forces it out, so it is registered once"
                ),
            )
        } else {
            Diagnostic::new(
                DiagnosticCode::TopologyDuplicateRegistration,
                format!("{where_} both register `{name}` in the directory"),
            )
            .with_help(format!(
                "a consumer resolving `{name}` will round-robin between them, which is load \
                 balancing if the two are interchangeable and a split if they are not. Drop the \
                 pin or the `prefer.isolate` to register it once"
            ))
        };
        diagnostic = diagnostic.at(Location(uri));
        diagnostics.push(diagnostic);
    }
}

/// The names one application registers in the directory.
///
/// A worker: one, `registers_as()`. A host: every REST provider it holds --
/// which the runtime spells as the REST capability minus the host itself,
/// because the gateway registers the others rather than itself -- plus every
/// gRPC provider, which the hub registers the same way.
fn registered_names<'a>(
    catalogue: &Catalogue,
    application: &'a ResolvedApplication,
) -> Vec<&'a str> {
    if application.is_worker() {
        return vec![application.registers_as()];
    }
    application
        .gears
        .iter()
        .filter(|g| {
            (has_cap(catalogue, g, RuntimeCap::Rest)
                && !has_cap(catalogue, g, RuntimeCap::RestHost))
                || has_cap(catalogue, g, RuntimeCap::Grpc)
        })
        .map(GearId::as_str)
        .collect()
}

/// At most one REST host and one gRPC hub per process.
///
/// The registry enforces both at startup, so a second one is a binary that
/// refuses to boot. Reported per process because the offending set is what a
/// reader has to act on, not the count.
fn check_singletons(
    catalogue: &Catalogue,
    application: &ResolvedApplication,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for (cap, code, what) in [
        (
            RuntimeCap::RestHost,
            DiagnosticCode::TopologyMultipleRestHost,
            "REST host",
        ),
        (
            RuntimeCap::GrpcHub,
            DiagnosticCode::TopologyMultipleGrpcHub,
            "gRPC hub",
        ),
    ] {
        let holders: Vec<&GearId> = application
            .gears
            .iter()
            .filter(|g| has_cap(catalogue, g, cap))
            .collect();
        if holders.len() > 1 {
            let named = holders
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            diagnostics.push(
                Diagnostic::error(
                    code,
                    format!(
                        "application `{}` contains {} {what} gears: {named}",
                        application.name,
                        holders.len()
                    ),
                    format!(
                        "the registry allows one {what} per process and refuses the rest at \
                         startup; separate them into different applications, or select only one"
                    ),
                )
                .at(Location(uri)),
            );
        }
    }
}

/// A worker must not carry a REST host.
///
/// A worker serves over the router `oop_serve` builds for it, not through the
/// composed gateway, so a REST host there registers routes nothing will call.
/// The process starts, reports healthy, and silently answers nothing -- which is
/// why this is an error rather than a warning.
fn check_worker_shape(application: &ResolvedApplication, uri: &str, diagnostics: &mut Diagnostics) {
    if application.kind != ApplicationKind::Worker {
        return;
    }
    if let Some(host) = &application.rest_host {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::TopologyRestHostInWorker,
                format!(
                    "worker `{}` contains the REST host `{host}`",
                    application.name
                ),
                "a worker serves through its own out-of-process router, so routes registered \
                 with the composed gateway are never reached; keep the REST host in the host \
                 application, or pin this gear there with `application(...)`",
            )
            .at(Location(uri)),
        );
    }
}

/// A registration host with nothing in its process to register with it.
///
/// The derivation ADR-0016 chose instead of a new field: the two capabilities
/// `is_process_singleton` names are exactly the two in-process registration
/// hosts, so "this gear is only meaningful as a co-tenant" follows from facts
/// already projected out of Rust.
///
/// Each host is paired with the capability it serves. The pairing is a table
/// rather than a convention on the names, because a convention would silently
/// admit an eighth capability that has no host at all.
const REGISTRATION_HOSTS: [(RuntimeCap, RuntimeCap, &str); 2] = [
    (RuntimeCap::GrpcHub, RuntimeCap::Grpc, "gRPC services"),
    (RuntimeCap::RestHost, RuntimeCap::Rest, "REST routes"),
];

fn check_host_with_nothing_to_host(
    catalogue: &Catalogue,
    application: &ResolvedApplication,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for (host_cap, served_cap, what) in REGISTRATION_HOSTS {
        debug_assert!(
            host_cap.is_process_singleton(),
            "the pairing above must stay the singleton caps, or the derivation is a coincidence"
        );
        let hosts: Vec<&GearId> = application
            .gears
            .iter()
            .filter(|g| has_cap(catalogue, g, host_cap))
            .collect();
        if hosts.is_empty() {
            continue;
        }
        if application
            .gears
            .iter()
            .any(|g| has_cap(catalogue, g, served_cap))
        {
            continue;
        }
        let named = hosts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::TopologyHostWithNothingToHost,
                format!(
                    "application `{}` contains {named} and no gear that registers {what} with it",
                    application.name
                ),
            )
            .with_help(format!(
                "a gear hands its registrations to the host through a Rust closure, so the two \
                 have to share a binary; either place a gear with that capability here, or stop \
                 separating `{named}` from the gears that were using it"
            ))
            .with_evidence("libs/toolkit/src/runtime/host_runtime.rs:795")
            .at(Location(uri)),
        );
    }
}

/// A process that registers gRPC services and has no hub to mount them in.
///
/// Every process, not only the host, and the asymmetry with its REST
/// counterpart below is the runtime's: `run_grpc_phase` is reached from the
/// host path *and* from `run_oop_serving`, while a worker's REST routes go
/// through its own out-of-process router and need no `rest_host`.
fn check_grpc_without_hub(
    catalogue: &Catalogue,
    application: &ResolvedApplication,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    if application.grpc_hub.is_some() {
        return;
    }
    let grpc_gears: Vec<&GearId> = application
        .gears
        .iter()
        .filter(|g| has_cap(catalogue, g, RuntimeCap::Grpc))
        .collect();
    if grpc_gears.is_empty() {
        return;
    }
    let named = grpc_gears
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    diagnostics.push(
        Diagnostic::error(
            DiagnosticCode::TopologyGrpcWithoutHub,
            format!(
                "application `{}` registers gRPC services with no gRPC hub to mount them: {named}",
                application.name
            ),
            "a `grpc` gear hands its service registrations to the application's hub, and the \
             registration is a Rust closure, so the hub has to be in the same binary; select a \
             gear with the `grpc_hub` capability into this application",
        )
        .with_evidence("libs/toolkit/src/runtime/host_runtime.rs:800")
        .at(Location(uri)),
    );
}

/// The gear that turns a bearer token into a tenant security context.
const AUTHN_RESOLVER: &str = "authn-resolver";

/// GBX0417: a process serving REST with no tenant authenticator refuses to
/// start (gears-rust ADR-0011 section 3), so say so before a binary exists.
///
/// Per-operation authentication is not projected, so the rule is the process's:
/// REST gears and no `authn-resolver`. A REST gear whose operations are all
/// anonymous or platform-plane would start anyway; the help says what to add if
/// it does serve tenant operations, which every REST gear in the corpus does.
fn check_tenant_authenticator(
    catalogue: &Catalogue,
    application: &ResolvedApplication,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    if application
        .gears
        .iter()
        .any(|g| g.as_str() == AUTHN_RESOLVER)
    {
        return;
    }
    // A gear that says its REST operations use no tenant plane -- cluster, whose
    // REST capability serves only probes -- needs no tenant authenticator.
    // Undeclared is read as tenant, as the runtime would find it.
    let rest_gears: Vec<&GearId> = application
        .gears
        .iter()
        .filter(|g| has_cap(catalogue, g, RuntimeCap::Rest))
        .filter(|g| {
            catalogue
                .gears
                .get(*g)
                .and_then(|gear| gear.rest_planes.as_ref())
                .is_none_or(|planes| planes.contains(&RestPlane::Tenant))
        })
        .collect();
    if rest_gears.is_empty() {
        return;
    }
    diagnostics.push(
        Diagnostic::error(
            DiagnosticCode::ApplicationWithoutAuthenticator,
            format!(
                "application `{}` serves REST from {} and links no `{AUTHN_RESOLVER}`: the \
                 runtime refuses to serve tenant-authenticated operations without a tenant \
                 authenticator",
                application.name,
                rest_gears
                    .iter()
                    .map(|g| format!("`{g}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            format!(
                "add `include = [\"{AUTHN_RESOLVER}\"]` to application `{}` (with an AuthN \
                 plugin selected on `{AUTHN_RESOLVER}`). A gear whose REST operations use no \
                 tenant plane says so in its `gear.gdl` with `rest_planes`",
                application.name
            ),
        )
        .with_evidence("libs/toolkit/src/runtime/oop_serve.rs (ensure_validators)")
        .at(Location(uri)),
    );
}

/// A process with REST gears and no REST host has nowhere to publish them.
fn check_rest_without_host(
    catalogue: &Catalogue,
    application: &ResolvedApplication,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    // Only meaningful for a host process: a worker publishes through `oop_serve`,
    // which needs no `rest_host` gear at all. A host with an address for the
    // built-in HTTP host is served by it (`partition::assign_builtin_hosts`).
    if application.kind != ApplicationKind::Host
        || application.rest_host.is_some()
        || application.serve.is_some()
    {
        return;
    }
    let rest_gears: Vec<&GearId> = application
        .gears
        .iter()
        .filter(|g| has_cap(catalogue, g, RuntimeCap::Rest))
        .collect();
    if rest_gears.is_empty() {
        return;
    }
    let named = rest_gears
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    diagnostics.push(
        Diagnostic::error(
            DiagnosticCode::TopologyRestWithoutHost,
            format!(
                "application `{}` has REST gears with no REST host to compose them: {named}",
                application.name
            ),
            "a `rest` gear registers its routes with the application's REST host; without one the \
             routes exist and nothing serves them, so select a gear with the `rest_host` \
             capability into this application",
        )
        .at(Location(uri)),
    );
}

/// A remote consumer of a gear behind the built-in HTTP host.
///
/// That host is a public listener (gears-rust ADR-0011 section 5): it serves only
/// `.exposed()` operations and answers 404 for the rest, and a contract's REST
/// routes are internal. The binding resolves, the consumer starts, and every
/// call fails -- so it is the same refusal as a process with no REST host at all,
/// reported under the same code.
pub fn check_builtin_host_providers(
    partition: &Partition,
    bindings: &[gearbox_ir::ResolvedBinding],
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    // Only REST: a gRPC binding is served by the process's hub, not its HTTP host.
    for binding in bindings
        .iter()
        .filter(|b| b.is_remote() && b.transport == gearbox_ir::Transport::Rest)
    {
        let Some(provider) = partition
            .applications
            .iter()
            .find(|a| a.name == binding.provider_application)
        else {
            continue;
        };
        if provider.kind != ApplicationKind::Host
            || provider.rest_host.is_some()
            || provider.serve.is_none()
        {
            continue;
        }
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::TopologyRestWithoutHost,
                format!(
                    "`{}` in application `{}` reaches `{}` in `{}` over REST, but `{}` has no REST \
                     host gear: its built-in HTTP host serves only exposed operations",
                    binding.consumer,
                    binding.consumer_application,
                    binding.provider,
                    binding.provider_application,
                    binding.provider_application,
                ),
                "select a gear with the `rest_host` capability (api-gateway) into the provider's \
                 application -- its internal listener serves every operation -- or place the \
                 consumer in the same application",
            )
            .with_evidence("libs/toolkit/src/runtime/http_host.rs:1")
            .at(Location(uri)),
        );
    }
}

/// Directory discovery needs two gears the description never names.
///
/// Kubernetes with static discovery is the other case: addresses are pinned
/// because the runtime has no cluster-native DNS resolver (GBX0603). That
/// warning is the counterpart of the Directory checks below -- both exist so
/// nobody reads a topology as discovering something the runtime cannot.
fn check_discovery(
    partition: &Partition,
    declaration: &DeploymentProfileDecl,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    if matches!(declaration, DeploymentProfileDecl::Kubernetes { .. })
        && declaration.discovery() == Some(Discovery::Static)
        && partition.applications.len() >= 2
    {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GapNoK8sDnsResolver,
                "the kubernetes profile pins application addresses statically; no cluster-native \
                 endpoint resolver exists",
            )
            .with_help(
                "the runtime's EndpointResolver set is Directory, Null, and Static -- there is \
                 no DNS resolver, so a neighbour's Service name is written into configuration \
                 rather than discovered",
            )
            .with_evidence("libs/toolkit/src/discovery.rs:38")
            .at(declared(declaration, uri)),
        );
        return;
    }

    if declaration.discovery() != Some(Discovery::Directory) {
        return;
    }
    // Only relevant once something has actually moved out: a single process
    // resolves everything locally and never consults the directory.
    if partition.applications.len() < 2 {
        return;
    }

    let Some(host) = partition
        .applications
        .iter()
        .find(|application| application.kind == ApplicationKind::Host)
    else {
        return;
    };
    let has = |gear: &str| GearId::new(gear).is_ok_and(|id| host.contains(&id));

    if !has(DIRECTORY_SERVER) {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::TopologyNoOrchestrator,
                format!(
                    "the profile discovers workers through the directory, but `{DIRECTORY_SERVER}` \
                     is not in the host application `{}`",
                    host.name
                ),
                format!(
                    "workers self-register with the directory server and consumers read it back; \
                     without it every remote binding resolves to nothing. Add \
                     `use_gear(\"{DIRECTORY_SERVER}\")`, or switch the profile to static discovery"
                ),
            )
            .at(declared(declaration, uri)),
        );
    }

    if !has(GRPC_HUB) {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::TopologyNoGrpcHub,
                format!(
                    "the profile discovers workers through the directory, but `{GRPC_HUB}` is not \
                     in the host application `{}`",
                    host.name
                ),
                format!(
                    "the spawn phase waits for the gRPC hub's endpoint before starting any \
                     worker, so without it the host blocks at startup rather than failing. Add \
                     `use_gear(\"{GRPC_HUB}\")`, or switch the profile to static discovery"
                ),
            )
            .at(declared(declaration, uri)),
        );
    }
}

/// A worker binary needs a path, and only `target_dir` can supply one.
fn check_worker_paths(
    partition: &Partition,
    declaration: &DeploymentProfileDecl,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    let DeploymentProfileDecl::SelfHosted { target_dir, .. } = declaration else {
        return;
    };
    if target_dir.is_some() {
        return;
    }
    let workers: Vec<&ResolvedApplication> = partition
        .applications
        .iter()
        .filter(|p| p.is_worker())
        .collect();
    if workers.is_empty() {
        return;
    }
    let named = workers
        .iter()
        .map(|p| p.name.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    diagnostics.push(
        Diagnostic::error(
            DiagnosticCode::TopologyNoTargetDir,
            format!("the profile has no `target_dir`, so no path can be written for: {named}"),
            "the host spawns each worker by absolute executable path, which is built from the \
             Cargo target directory; add `target_dir = \"...\"` to the profile",
        )
        .at(declared(declaration, uri)),
    );
}

/// Say plainly that self-hosted means one machine.
///
/// Not a defect in the product: the runtime implements exactly one spawn
/// backend, which starts local operating-system processes. Recorded so nobody
/// reads a multi-process topology as a distributed one.
fn report_spawn_gap(
    partition: &Partition,
    declaration: &DeploymentProfileDecl,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    if !matches!(declaration, DeploymentProfileDecl::SelfHosted { .. }) {
        return;
    }
    if !partition
        .applications
        .iter()
        .any(ResolvedApplication::is_worker)
    {
        return;
    }
    diagnostics.push(
        Diagnostic::new(
            DiagnosticCode::GapNoRemoteSpawnBackend,
            "workers run as local operating-system processes on the host's machine",
        )
        .with_help(
            "the runtime implements one spawn backend and it is local; this profile is \
             multi-application, not multi-machine. Use the kubernetes profile for that",
        )
        .with_evidence("libs/toolkit/src/bootstrap/run.rs:74")
        .at(declared(declaration, uri)),
    );
}

fn has_cap(catalogue: &Catalogue, gear: &GearId, cap: RuntimeCap) -> bool {
    catalogue
        .gears
        .get(gear)
        .is_some_and(|g| g.runtime_caps.contains(&cap))
}

/// Shorthand for a diagnostic that names the file and no position in it.
#[expect(non_snake_case, reason = "reads as a constructor at each call site")]
fn Location(uri: &str) -> gearbox_ir::Location {
    gearbox_ir::Location::file(uri.to_owned())
}

/// Where a diagnostic *about the profile declaration* points.
///
/// The `embedded(...)` / `self_hosted(...)` / `kubernetes(...)` call when the
/// description recorded one, the file otherwise.
///
/// **Only for checks that are about the profile itself.** Most checks in this
/// file take a `ResolvedApplication` and are about an `application(...)` call,
/// which carries no span yet; anchoring those on the profile would underline a
/// declaration that is not the one at fault, and a squiggle in the wrong place
/// is a false claim rather than an imprecise one
/// (`cpt-gearbox-adr-gdl-language-server`).
fn declared(declaration: &DeploymentProfileDecl, uri: &str) -> gearbox_ir::Location {
    gearbox_ir::Location::or_file(declaration.declared_at(), uri)
}
