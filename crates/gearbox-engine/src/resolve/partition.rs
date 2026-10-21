//! Step 4: which gears end up in which process.
//!
//! The word *partition* is a misnomer kept for continuity with the plan, and the
//! misnomer is worth naming because it is the thing most easily got wrong: this
//! is **not** a partition. A process is the co-location closure of its anchor,
//! and two closures may overlap, so one gear is routinely linked into several
//! binaries. That is a consequence of `deps` being link-time, not a mistake to be
//! corrected -- and a resolver that tried to assign each gear to exactly one
//! process would be unable to represent any real product.
//!
//! One deterministic pass, no scoring and no search. Every severable edge whose
//! provider is not already inside the consumer's process becomes an anchor; that
//! is the whole algorithm. A solver would need a cost model that does not exist,
//! and the platform currently offers so few severable edges that there is nothing
//! for one to optimise.

use std::collections::{BTreeMap, BTreeSet};

use gearbox_ir::{
    ApplicationId, ApplicationKind, ApplicationRole, Catalogue, DeploymentProfileDecl, Diagnostic,
    DiagnosticCode, Diagnostics, Entrypoint, GearId, ImageRef, InclusionReason, Location,
    ResolvedApplication, ResolvedEndpoint, RuntimeCap, SpawnSpec, WorkerServe,
};

use super::closure::Closure;
use super::cuts::Cuts;
use super::profile::ProfileScoped;

/// The processes, in a stable order.
#[derive(Debug, Default)]
pub struct Partition {
    pub applications: Vec<ResolvedApplication>,
}

impl Partition {
    /// Which application a gear runs in, when exactly one contains it.
    ///
    /// `None` when a gear is in several, which is the overlap case and must be
    /// answered by asking about a specific pair rather than about a gear.
    #[must_use]
    pub fn sole_application(&self, gear: &GearId) -> Option<&ResolvedApplication> {
        let mut found = self.applications.iter().filter(|p| p.contains(gear));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// Whether two gears share at least one process.
    ///
    /// The question a binding actually needs: not "where does this gear live"
    /// but "will these two be in the same binary".
    #[must_use]
    pub fn share_an_application(&self, a: &GearId, b: &GearId) -> bool {
        self.applications
            .iter()
            .any(|p| p.contains(a) && p.contains(b))
    }
}

/// Everything the partition reads and never changes.
///
/// A struct rather than five parameters: they travel together through every
/// stage below, and naming the bundle is what keeps the signatures readable.
pub struct Inputs<'a> {
    pub catalogue: &'a Catalogue,
    pub closure: &'a Closure,
    pub cuts: &'a Cuts,
    pub scoped: &'a ProfileScoped<'a>,
    /// The gears the description named, as opposed to those reached from them.
    pub selected: &'a BTreeSet<GearId>,
    /// Gears `prefer.isolate` asked to run in their own process.
    pub isolates: &'a BTreeSet<GearId>,
    /// The description's `grpc-hub` `listen_addr`, when it sets one: a
    /// production self-hosted profile keeps a socket path it chose.
    pub hub_listen: Option<&'a str>,
}

/// Assign gears to processes for one profile.
pub fn partition(
    input: &Inputs<'_>,
    declaration: &DeploymentProfileDecl,
    product_id: &str,
    product_version: &str,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Partition {
    let Inputs {
        catalogue,
        closure,
        cuts,
        scoped,
        ..
    } = *input;
    report_undeployable_roles(catalogue, closure, uri, diagnostics);
    report_unknown_roles(catalogue, scoped, uri, diagnostics);
    for pin in &scoped.application_pins {
        for included in &pin.include {
            if !closure.contains(included) {
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::GdlEval,
                        format!(
                            "application `{}` includes `{included}`, which is not in this product",
                            pin.name
                        ),
                        "select the gear with use_gear before including it in an application",
                    )
                    .at(Location::or_file(pin.declared_at.as_ref(), uri)),
                );
            }
        }
    }
    let mut used_names: BTreeSet<String> = BTreeSet::new();
    let mut applications = Vec::new();

    match declaration {
        DeploymentProfileDecl::Embedded { .. } => {
            report_embedded_violations(cuts, scoped, uri, diagnostics);
            // One process holding the whole product. Not the anchor's closure:
            // a selected gear that nothing depends on still has to run, and in a
            // single-process profile there is nowhere else for it to be.
            let all: Vec<GearId> = closure.members.keys().cloned().collect();
            let gears = topo_sort(catalogue, closure, &all);
            if let Some(anchor) = pick_anchor(catalogue, &gears) {
                // Named after the product, and only here.
                //
                // Every other profile has several applications, so one name
                // could not serve them; this profile has exactly one and it
                // *is* the product. Naming it after its anchor said "the
                // api-gateway binary" about a binary holding nine gears --
                // and `pick_anchor` is explicit that the name is free, since
                // the anchor it picks "affects nothing but the name". The
                // anchor stays on the application either way, so nothing is
                // lost by not spelling it here.
                //
                // Falls back to the anchor for the reason `derive_name`
                // returns an `Option` at all: a product id that is not a valid
                // application name should degrade rather than stop the
                // resolution.
                let name = ApplicationId::new(product_id)
                    .ok()
                    .or_else(|| derive_name(&anchor, &used_names));
                if let Some(name) = name {
                    applications.push(build(
                        catalogue,
                        anchor,
                        gears,
                        ApplicationKind::Host,
                        name,
                        None,
                        None,
                        &mut used_names,
                    ));
                }
            }
        }
        DeploymentProfileDecl::SelfHosted { host, .. } => {
            applications = split(input, Some(host), &mut used_names);
        }
        DeploymentProfileDecl::Kubernetes { .. } => {
            applications = split(input, None, &mut used_names);
        }
    }

    assign_endpoints(
        catalogue,
        declaration,
        product_id,
        input.hub_listen,
        &mut applications,
    );
    assign_spawns(declaration, &mut applications, uri, diagnostics);
    assign_builtin_hosts(catalogue, declaration, &mut applications);
    assign_internal_listener(declaration, &mut applications);
    assign_chart_fields(declaration, product_version, &mut applications);
    assign_unit_charts(catalogue, declaration, product_version, &mut applications);
    report_orphans(closure, &applications, uri, diagnostics);
    report_duplicated_by_deps(input, &applications, uri, diagnostics);
    Partition { applications }
}

/// The base port the built-in HTTP host of a host process is allocated from.
const BUILTIN_HOST_BASE_PORT: u16 = 8080;

/// Give a host process with REST gears and no `rest_host` gear the address of
/// the runtime's built-in HTTP host (gears-rust ADR-0011 section 5, step 4).
///
/// The runtime serves those gears itself when the configuration has an
/// `http_host` section, as a public listener: only `.exposed()` operations, the
/// platform header removed first, internal operations unreachable over HTTP.
/// The address rides [`ResolvedApplication::serve`] because it is the same kind
/// of fact a worker's is -- a top-level listener no gear's key configures -- and
/// the image and chart already publish that port. A host has no directory
/// presence, so the advertised URI is informational.
fn assign_builtin_hosts(
    catalogue: &Catalogue,
    declaration: &DeploymentProfileDecl,
    applications: &mut [ResolvedApplication],
) {
    let mut taken: BTreeSet<u16> = applications
        .iter()
        .flat_map(|p| {
            p.listens
                .iter()
                .map(|e| e.address.as_str())
                .chain(p.serve.iter().map(|s| s.listen_addr.as_str()))
        })
        .filter_map(|address| address.rsplit_once(':').and_then(|(_, p)| p.parse().ok()))
        .collect();
    let mut order: Vec<usize> = (0..applications.len()).collect();
    order.sort_by(|a, b| applications[*a].name.cmp(&applications[*b].name));
    for index in order {
        let application = &applications[index];
        if application.kind != ApplicationKind::Host
            || application.rest_host.is_some()
            || !application
                .gears
                .iter()
                .any(|g| has_cap(catalogue, g, RuntimeCap::Rest))
        {
            continue;
        }
        let Some(port) = next_free_port(BUILTIN_HOST_BASE_PORT, &taken) else {
            continue;
        };
        taken.insert(port);
        let address = format!("{}:{port}", bind_host(declaration));
        applications[index].serve = Some(WorkerServe {
            advertise_uri: format!("http://{address}"),
            listen_addr: address,
            allow_loopback_advertise: allows_loopback(declaration),
        });
    }
}

/// The base port a worker's own REST listener is allocated from.
///
/// Deliberately above the gears' declared defaults, so a worker's socket does
/// not sit where a gear's would and a reader can tell the two apart at a glance.
const WORKER_SERVE_BASE_PORT: u16 = 8090;

/// Give every worker a serving address, and tell the host how to start it.
///
/// **Inventing this port is not the same as inventing a gear's.**
/// [`assign_endpoints`] skips an endpoint with no `default_port` rather than
/// fabricating one, because the gear has its own default and a made-up number
/// would silently override it. A worker's listener has no such fallback: the
/// out-of-process runtime binds what `oop_http` says and advertises it to the
/// directory, so without an address here the worker serves nothing and the
/// binding the lock calls `via directory` resolves to nothing.
///
/// The two halves are one function because they are one decision: the host's
/// spawn entry and the worker's address describe the same arrangement, and
/// splitting them would let a refactor change one without the other.
fn assign_spawns(
    declaration: &DeploymentProfileDecl,
    applications: &mut [ResolvedApplication],
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    let mut taken: BTreeSet<u16> = applications
        .iter()
        .flat_map(|p| p.listens.iter())
        .filter_map(|e| e.address.rsplit_once(':').and_then(|(_, p)| p.parse().ok()))
        .collect();

    // In name order, for the same reason `assign_endpoints` is: the lock is
    // written in name order, so any other order would let a refactor that
    // changes no topology still change every port.
    let mut order: Vec<usize> = (0..applications.len()).collect();
    order.sort_by(|a, b| applications[*a].name.cmp(&applications[*b].name));

    let mut spawns: Vec<SpawnSpec> = Vec::new();
    for index in order {
        if !applications[index].is_worker() {
            continue;
        }
        let Some(port) = next_free_port(WORKER_SERVE_BASE_PORT, &taken) else {
            continue;
        };
        taken.insert(port);
        let host = bind_host(declaration);
        let address = format!("{host}:{port}");
        let loopback = allows_loopback(declaration);
        applications[index].serve = Some(WorkerServe {
            advertise_uri: format!("http://{address}"),
            listen_addr: address,
            // Kubernetes advertises a Service DNS name (rewritten in
            // `assign_chart_fields`); a one-machine profile has to say loopback
            // is allowed or the runtime refuses to start.
            allow_loopback_advertise: loopback,
        });

        // The binary by name, not by path. Where it sits is a fact about the
        // generated tree -- which target directory it was built into, and where
        // that tree is -- and the resolver knows none of that. `target_dir`
        // travels to the lock as a profile setting instead, and the generator
        // composes the path from both. `structural::check_worker_paths` still
        // reports a missing one, because without it there is nowhere to build.
        if declaration.target_dir().is_some() {
            spawns.push(SpawnSpec {
                gear: applications[index].anchor.clone(),
                bin_name: applications[index].bin_name.clone(),
                // `--config` is the only channel that works for the file: the
                // runtime reads `TOOLKIT_CONFIG_PATH` but nothing ever sets it.
                //
                // `--role` is passed only when there is one, so an
                // undifferentiated deployment keeps the argument list it had --
                // and a role-bearing one says at the spawn which of the gear's
                // roles this process is being started in, rather than relying on
                // a name baked into the binary.
                args: {
                    let mut args = vec![
                        "--config".to_owned(),
                        format!("config/{}.yaml", applications[index].name),
                    ];
                    if let Some(role) = &applications[index].role {
                        args.push("--role".to_owned());
                        args.push(role.name.clone());
                    }
                    args
                },
                working_directory: None,
                // Deliberately empty. The host injects `TOOLKIT_DIRECTORY_ENDPOINT`
                // and `TOOLKIT_MODULE_CONFIG` itself at spawn time, and the
                // endpoint is only known once its gRPC hub has bound -- recording
                // a value here would be recording a guess as a decision.
                environment: BTreeMap::new(),
            });
        }
    }

    if spawns.is_empty() {
        return;
    }
    if let Some(host) = applications
        .iter_mut()
        .find(|p| matches!(p.kind, ApplicationKind::Host))
    {
        host.spawns = spawns;
        return;
    }
    diagnostics.push(
        Diagnostic::error(
            DiagnosticCode::TopologyNoHost,
            "workers have no host application to spawn them",
            "keep a `ApplicationKind::Host` in this profile, or name the host on `self_hosted`",
        )
        .at(Location::file(uri.to_owned())),
    );
}

/// The host a generated bind address uses.
///
/// Loopback on one-machine profiles: a process that binds every interface by
/// default is a decision nobody asked for. Kubernetes is the opposite -- a
/// Service cannot deliver a packet to `127.0.0.1` inside the pod -- so the
/// address is resolved here rather than patched in a template. The lock records
/// the fact once and every generator reads it.
fn bind_host(declaration: &DeploymentProfileDecl) -> &'static str {
    if matches!(declaration, DeploymentProfileDecl::Kubernetes { .. }) {
        "0.0.0.0"
    } else {
        "127.0.0.1"
    }
}

/// The gRPC hub, whose listener an embedded profile leaves unbound.
const GRPC_HUB: &str = "grpc-hub";

/// The hub's listen address for "serve in-process, bind nothing".
const UNBOUND: &str = "none";

fn allows_loopback(declaration: &DeploymentProfileDecl) -> bool {
    !matches!(declaration, DeploymentProfileDecl::Kubernetes { .. })
}

/// Fill each process's `listens` from what its gears declare they `serve`.
///
/// Runs once over every process rather than inside `build`, because the one
/// thing that cannot be decided per process is the thing that matters: two
/// processes must not be handed the same port. A gear linked into two binaries
/// -- which co-location closures produce routinely -- would otherwise get its
/// declared default twice and the second bind would fail at run time, long
/// after the lock said everything was fine.
///
/// Endpoints with no `default_port` are skipped rather than invented. The gear
/// then falls back to whatever its own config default is, which is a worse
/// answer than a resolved one but a much better answer than a fabricated port
/// the description never mentioned.
fn assign_endpoints(
    catalogue: &Catalogue,
    declaration: &DeploymentProfileDecl,
    product_id: &str,
    hub_listen: Option<&str>,
    applications: &mut [ResolvedApplication],
) {
    // A production single-node self-hosted hub is a socket in a private
    // directory, not TCP (gears-rust ADR-0011 section 1), one per process so
    // two hubs never bind one path: under the product's home, by application.
    // The description's own `uds://` path is the directory's hub -- the one
    // workers are handed (the host passes its bound endpoint on). The hub
    // creates the directory 0700 and the socket 0600.
    let private_sockets = matches!(declaration, DeploymentProfileDecl::SelfHosted { .. })
        && !declaration.development();
    let private_socket = |application: &ResolvedApplication| -> Option<String> {
        if !private_sockets {
            return None;
        }
        let hosts_directory = application
            .gears
            .iter()
            .any(|g| g.as_str() == "service-discovery");
        Some(
            hub_listen
                .filter(|addr| hosts_directory && addr.starts_with("uds://"))
                .map_or_else(
                    || {
                        format!(
                            "uds://~/.cf-gears/{product_id}/{}/run/{}/grpc-hub.sock",
                            declaration.id(),
                            application.name
                        )
                    },
                    str::to_owned,
                ),
        )
    };
    let mut taken: BTreeSet<u16> = BTreeSet::new();
    let host = bind_host(declaration);
    let loopback = allows_loopback(declaration);
    let embedded = matches!(declaration, DeploymentProfileDecl::Embedded { .. });

    // By process name, not by construction order: the lock is written in name
    // order, so assigning in any other order would let a resolver refactor that
    // does not change the topology still change every port.
    let mut order: Vec<usize> = (0..applications.len()).collect();
    order.sort_by(|a, b| applications[*a].name.cmp(&applications[*b].name));

    for index in order {
        let gears = applications[index].gears.clone();
        let socket = private_socket(&applications[index]);
        let mut listens = Vec::new();
        for gear in gears {
            let Some(descriptor) = catalogue.gears.get(&gear) else {
                continue;
            };
            for served in &descriptor.serves {
                if !served.binds_own_socket() {
                    continue;
                }
                let (Some(config_key), Some(default_port)) =
                    (served.config_key.as_ref(), served.default_port)
                else {
                    continue;
                };
                // One process has no caller for the hub: every gRPC service is
                // reached in-process. `none` registers them and binds nothing
                // (gears-rust ADR-0011 section 1; Gearbox ADR-0026 section 6).
                if gear.as_str() == GRPC_HUB
                    && let Some(address) = if embedded {
                        Some(UNBOUND.to_owned())
                    } else {
                        socket.clone()
                    }
                {
                    listens.push(ResolvedEndpoint {
                        name: served.name.clone(),
                        gear: gear.clone(),
                        config_key: config_key.clone(),
                        address,
                        advertise_uri: None,
                        allow_loopback_advertise: loopback,
                    });
                    continue;
                }
                let Some(port) = next_free_port(default_port, &taken) else {
                    continue;
                };
                taken.insert(port);
                listens.push(ResolvedEndpoint {
                    name: served.name.clone(),
                    gear: gear.clone(),
                    config_key: config_key.clone(),
                    address: format!("{host}:{port}"),
                    advertise_uri: None,
                    allow_loopback_advertise: loopback,
                });
            }
        }
        applications[index].listens = listens;
    }
}

/// Image, subchart and service port -- only when the profile builds images.
///
/// `service_port` is a projection of `listens` / `serve`, not an independent
/// fact. A process may listen on REST and gRPC; neighbours dial REST, because
/// that is the transport a severed declared edge actually carries. The Service
/// still lists every port.
fn assign_chart_fields(
    declaration: &DeploymentProfileDecl,
    product_version: &str,
    applications: &mut [ResolvedApplication],
) {
    let DeploymentProfileDecl::Kubernetes {
        namespace,
        image_registry,
        ..
    } = declaration
    else {
        return;
    };
    let namespace = namespace.as_deref().unwrap_or("default");
    for application in applications.iter_mut() {
        application.subchart = Some(application.name.to_string());
        application.image = Some(image_ref(
            image_registry.as_deref(),
            &application.bin_name,
            product_version,
        ));
        application.service_port = contract_port(application);
        let subchart = application.name.to_string();
        for endpoint in &mut application.listens {
            if endpoint.name == "internal"
                && let Some(port) = parse_port(&endpoint.address)
            {
                endpoint.advertise_uri = Some(format!(
                    "http://{subchart}.{namespace}.svc.cluster.local:{port}"
                ));
                endpoint.allow_loopback_advertise = false;
            }
        }
        if let Some(uri) = cluster_dns(application, namespace)
            && let Some(serve) = application.serve.as_mut()
        {
            serve.advertise_uri = uri;
            serve.allow_loopback_advertise = false;
        }
    }
}

/// The gateway's internal listener, for a production profile with more than
/// one process (gears-rust ADR-0011 §1; Gearbox ADR-0026 §6).
///
/// The public listener of a production gateway serves only exposed operations,
/// so the operations other processes call -- the `AuthZ` plane's `evaluate`,
/// Types Registry, the directory's REST -- need a listener of their own. It is
/// what the gateway advertises in the directory, and what a static endpoint to
/// the host names (`contract_port` prefers it). A development profile keeps
/// the single listener it always had; one process has no caller for it.
fn assign_internal_listener(
    declaration: &DeploymentProfileDecl,
    applications: &mut [ResolvedApplication],
) {
    if declaration.development()
        || matches!(declaration, DeploymentProfileDecl::Embedded { .. })
        || applications.len() < 2
    {
        return;
    }
    let mut taken: BTreeSet<u16> = applications
        .iter()
        .flat_map(|p| {
            p.listens
                .iter()
                .map(|e| e.address.as_str())
                .chain(p.serve.iter().map(|s| s.listen_addr.as_str()))
        })
        .filter_map(|address| address.rsplit_once(':').and_then(|(_, p)| p.parse().ok()))
        .collect();
    let host = bind_host(declaration);
    let loopback = allows_loopback(declaration);
    for application in applications.iter_mut() {
        let Some(gateway) = application
            .rest_host
            .clone()
            .filter(|gear| gear.as_str() == "api-gateway")
        else {
            continue;
        };
        let Some(port) = next_free_port(INTERNAL_LISTENER_BASE_PORT, &taken) else {
            continue;
        };
        taken.insert(port);
        application.listens.push(ResolvedEndpoint {
            name: "internal".to_owned(),
            gear: gateway,
            config_key: "internal_bind_addr".to_owned(),
            address: format!("{host}:{port}"),
            // Kubernetes rewrites this to the Service DNS in `assign_chart_fields`.
            advertise_uri: Some(format!("http://127.0.0.1:{port}")),
            allow_loopback_advertise: loopback,
        });
    }
}

/// The base port of a production gateway's internal listener.
const INTERNAL_LISTENER_BASE_PORT: u16 = 8088;

/// Ship a gear's hand-written unit chart for an application that *is* that
/// unit: anchored on the gear and linking exactly the chart's gears (plugins
/// aside, they configure rather than define the unit). Its database, if any,
/// reaches the chart through `configOverlay` and `secretEnv`. Anything else is
/// a different binary and gets a generated chart. Kubernetes only: no other
/// profile renders a chart.
fn assign_unit_charts(
    catalogue: &Catalogue,
    declaration: &DeploymentProfileDecl,
    product_version: &str,
    applications: &mut [ResolvedApplication],
) {
    let DeploymentProfileDecl::Kubernetes { image_registry, .. } = declaration else {
        return;
    };
    for application in applications.iter_mut() {
        let Some(anchor) = catalogue.gears.get(&application.anchor) else {
            continue;
        };
        let Some(chart) = &anchor.unit_chart else {
            continue;
        };
        let linked: BTreeSet<&GearId> = application
            .gears
            .iter()
            .filter(|g| {
                catalogue
                    .gears
                    .get(*g)
                    .is_none_or(|gear| gear.implements.is_none())
            })
            .collect();
        if linked != chart.gears.iter().collect() {
            continue;
        }
        application.image = Some(image_ref(
            image_registry.as_deref(),
            &chart.image,
            product_version,
        ));
        // The chart fixes its Services: the process is addressed by them, so the
        // directory endpoint and static endpoints name what the chart creates.
        let namespace = match declaration {
            DeploymentProfileDecl::Kubernetes { namespace, .. } => {
                namespace.as_deref().unwrap_or("default")
            }
            _ => "default",
        };
        for (endpoint_name, at) in &chart.services {
            let Some((service, port)) = at.rsplit_once(':') else {
                continue;
            };
            let uri = format!("http://{service}.{namespace}.svc.cluster.local:{port}");
            if endpoint_name == "serve" {
                if let Some(serve) = application.serve.as_mut() {
                    serve.listen_addr = format!("0.0.0.0:{port}");
                    serve.advertise_uri = uri;
                }
                continue;
            }
            for endpoint in &mut application.listens {
                if &endpoint.name == endpoint_name {
                    endpoint.address = format!("0.0.0.0:{port}");
                    endpoint.advertise_uri = Some(uri.clone());
                }
            }
        }
        application.service_port = contract_port(application);
        application.unit_chart = Some(gearbox_ir::ResolvedUnitChart {
            source: anchor.source.clone(),
            path: chart.path.clone(),
            image: chart.image.clone(),
        });
    }
}

fn image_ref(registry: Option<&str>, bin_name: &str, version: &str) -> ImageRef {
    ImageRef {
        registry: registry
            .filter(|registry| !registry.is_empty())
            .map(str::to_owned),
        repository: bin_name.to_owned(),
        tag: version.to_owned(),
    }
}

/// The port neighbours dial for contract traffic: a worker's own listener, or
/// the host's REST socket rather than its gRPC one.
fn contract_port(application: &ResolvedApplication) -> Option<u16> {
    if let Some(serve) = &application.serve {
        return parse_port(&serve.listen_addr);
    }
    // A production gateway's internal listener first: its public one serves
    // exposed operations only, and a neighbour calls the rest.
    application
        .listens
        .iter()
        .find(|endpoint| endpoint.name == "internal")
        .or_else(|| {
            application
                .listens
                .iter()
                .find(|endpoint| endpoint.name == "rest")
        })
        .or_else(|| application.listens.first())
        .and_then(|endpoint| parse_port(&endpoint.address))
}

pub(crate) fn cluster_dns(application: &ResolvedApplication, namespace: &str) -> Option<String> {
    // A unit chart names its own Services: the endpoint a neighbour dials is
    // the one it advertises.
    if application.unit_chart.is_some() {
        let listened = ["internal", "rest"].iter().find_map(|name| {
            application
                .listens
                .iter()
                .find(|e| e.name == *name)
                .and_then(|e| e.advertise_uri.clone())
        });
        if let Some(uri) =
            listened.or_else(|| application.serve.as_ref().map(|s| s.advertise_uri.clone()))
        {
            return Some(uri);
        }
    }
    let name = application.subchart.as_deref()?;
    let port = application.service_port?;
    Some(format!(
        "http://{name}.{namespace}.svc.cluster.local:{port}"
    ))
}

fn parse_port(address: &str) -> Option<u16> {
    address.rsplit_once(':')?.1.parse().ok()
}

/// `preferred` if it is free, else the next free port above it.
///
/// `None` when the search runs off the end of the port space, which cannot
/// happen for any realistic product but is the honest answer rather than a
/// wrap-around to a privileged port.
fn next_free_port(preferred: u16, taken: &BTreeSet<u16>) -> Option<u16> {
    (preferred..=u16::MAX).find(|port| !taken.contains(port))
}

/// A host plus one process per gear that moved out of it.
///
/// The host is not "the anchor's closure": it is everything selected, minus what
/// left. A worker leaves only if nothing remaining in the host reaches it -- and
/// when something does, the gear is in **both** binaries, which is the overlap
/// this whole design exists to represent rather than to prevent.
fn split(
    input: &Inputs<'_>,
    host_name: Option<&ApplicationId>,
    used_names: &mut BTreeSet<String>,
) -> Vec<ResolvedApplication> {
    let Inputs {
        catalogue,
        closure,
        cuts,
        scoped,
        selected,
        isolates,
        hub_listen: _,
    } = *input;
    // A pin on the REST host names the host application; moving that gear to
    // a worker would leave the remaining REST gears with no composed router.
    let host_pin = scoped
        .application_pins
        .iter()
        .find(|pin| pin.role.is_none() && has_cap(catalogue, &pin.anchor, RuntimeCap::RestHost));
    // Candidates to move out: the provider of every severable edge, plus
    // anything the description pinned to its own process.
    let mut worker_anchors: BTreeSet<GearId> = cuts
        .cuttable
        .iter()
        .map(|e| e.provider.clone())
        .chain(scoped.application_pins.iter().map(|p| p.anchor.clone()))
        .chain(isolates.iter().cloned())
        .filter(|g| closure.contains(g))
        .collect();
    if let Some(pin) = host_pin {
        worker_anchors.remove(&pin.anchor);
    }
    // An `include` places the gear: a severable edge to it does not also make
    // it a worker of its own, which put one gear in two processes and gave its
    // remote consumers the stray copy. Its own pin or an isolate still sends it
    // out.
    let pinned_anchors: BTreeSet<&GearId> = scoped
        .application_pins
        .iter()
        .map(|p| &p.anchor)
        .chain(isolates.iter())
        .collect();
    let included: BTreeSet<GearId> = scoped
        .application_pins
        .iter()
        .flat_map(|p| p.include.iter())
        .filter(|g| !pinned_anchors.contains(g))
        .cloned()
        .collect();
    worker_anchors.retain(|g| !included.contains(g));

    // `bind(mode = local)`: the provider joins the consumer's process
    // (ADR `cpt-gearbox-adr-product-placement` section 2). It stops being a
    // worker of its own only when nothing else sends it out -- another
    // consumer's severable edge, a pin or an isolate -- and a provider pinned
    // elsewhere is left where the pin puts it: that contradiction is GBX0413,
    // reported with the binding.
    let forced_out: BTreeSet<&GearId> = scoped
        .application_pins
        .iter()
        .map(|p| &p.anchor)
        .chain(isolates.iter())
        .collect();
    let wants_local = |edge: &super::cuts::CuttableEdge| {
        scoped.bindings.iter().any(|b| {
            b.consumer == edge.consumer
                && b.contract == edge.contract
                && b.mode == gearbox_ir::BindingMode::Local
        })
    };
    let local_requests: Vec<(&GearId, &GearId)> = cuts
        .cuttable
        .iter()
        .filter(|e| wants_local(e))
        .filter(|e| !forced_out.contains(&e.provider))
        .map(|e| (&e.consumer, &e.provider))
        .collect();
    for (_, provider) in &local_requests {
        let still_cut = cuts
            .cuttable
            .iter()
            .any(|e| e.provider == **provider && !wants_local(e));
        if !still_cut {
            worker_anchors.remove(*provider);
        }
    }

    // Explicit worker contributions are placed with that worker rather than
    // seeded in the default host as well. A host dependency (or explicit host
    // include) can still reach them, preserving legitimate overlapping closures.
    let worker_includes: BTreeSet<&GearId> = scoped
        .application_pins
        .iter()
        .filter(|pin| worker_anchors.contains(&pin.anchor))
        .flat_map(|pin| pin.include.iter())
        .collect();
    // The host keeps every other selected gear and everything those reach.
    let mut host_seeds: Vec<GearId> = selected
        .iter()
        .filter(|g| !worker_anchors.contains(*g))
        .filter(|g| !worker_includes.contains(*g))
        // Plugins follow their host below; being selected under a worker's
        // host is not a request for an additional copy in the main process.
        .filter(|g| {
            closure
                .members
                .get(*g)
                .is_some_and(|reasons| reasons.contains(&InclusionReason::Selected))
        })
        .cloned()
        .collect();
    if let Some(pin) = host_pin {
        host_seeds.extend(
            pin.include
                .iter()
                .filter(|id| closure.contains(id))
                .cloned(),
        );
    }
    // A provider asked to be local follows its consumer into *every* process
    // the consumer is in -- the consumer may be linked into several -- and
    // the provider's own local requests follow it in turn. So placement is
    // repeated until nothing moves: one pass would leave a provider that a
    // local request brought into a worker without the providers it asked for
    // locally itself.
    let worker_seeds = |anchor: &GearId, extra: &BTreeMap<GearId, Vec<GearId>>| -> Vec<GearId> {
        let pin = scoped.application_pins.iter().find(|p| p.anchor == *anchor);
        std::iter::once(anchor.clone())
            .chain(
                pin.into_iter()
                    .flat_map(|p| p.include.iter())
                    .filter(|id| closure.contains(id))
                    .cloned(),
            )
            .chain(extra.get(anchor).into_iter().flatten().cloned())
            .collect()
    };
    let mut worker_extra: BTreeMap<GearId, Vec<GearId>> = BTreeMap::new();
    let mut host_gears;
    let mut worker_gears: BTreeMap<GearId, Vec<GearId>>;
    loop {
        host_gears = topo_sort(catalogue, closure, &host_seeds);
        worker_anchors.retain(|a| !host_gears.contains(a) || is_forced_out(scoped, isolates, a));
        worker_gears = worker_anchors
            .iter()
            .map(|a| {
                (
                    a.clone(),
                    topo_sort(catalogue, closure, &worker_seeds(a, &worker_extra)),
                )
            })
            .collect();
        let mut moved = false;
        for (consumer, provider) in &local_requests {
            if host_gears.contains(consumer) && !host_gears.contains(provider) {
                host_seeds.push((*provider).clone());
                moved = true;
            }
            for (anchor, gears) in &worker_gears {
                if gears.contains(consumer) && !gears.contains(provider) {
                    worker_extra
                        .entry(anchor.clone())
                        .or_default()
                        .push((*provider).clone());
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }

    let mut applications = Vec::new();
    if let Some(anchor) = pick_anchor(catalogue, &host_gears) {
        let name = host_name
            .cloned()
            .or_else(|| host_pin.map(|pin| pin.name.clone()))
            .or_else(|| derive_name(&anchor, used_names));
        if let Some(name) = name {
            applications.push(build(
                catalogue,
                anchor,
                host_gears,
                ApplicationKind::Host,
                name,
                host_pin.map(|pin| pin.replicas),
                None,
                used_names,
            ));
        }
    }
    for anchor in &worker_anchors {
        let pin = scoped.application_pins.iter().find(|p| p.anchor == *anchor);
        let gears = worker_gears.remove(anchor).unwrap_or_default();
        let name = pin
            .map(|p| p.name.clone())
            .or_else(|| derive_name(anchor, used_names));
        if let Some(name) = name {
            applications.push(build(
                catalogue,
                anchor.clone(),
                gears,
                ApplicationKind::Worker,
                name,
                pin.map(|p| p.replicas),
                pin.and_then(|p| role_of(catalogue, p)),
                used_names,
            ));
        }
    }
    applications
}

/// The role a pin deploys, with the name it registers under.
///
/// `None` when the pin names no role -- every application that deploys a gear
/// undifferentiated -- and then the anchor's own id is what it answers to. A
/// role the anchor does not declare is refused separately by
/// `report_unknown_roles`, so a miss here needs no second complaint.
fn role_of(catalogue: &Catalogue, pin: &gearbox_ir::ApplicationPin) -> Option<ApplicationRole> {
    let name = pin.role.as_deref()?;
    catalogue
        .gears
        .get(&pin.anchor)?
        .declared_roles
        .iter()
        .find(|r| r.name == name)
        .map(|r| ApplicationRole {
            name: r.name.clone(),
            directory_name: r.directory_name.clone(),
        })
}

fn is_forced_out(scoped: &ProfileScoped<'_>, isolates: &BTreeSet<GearId>, gear: &GearId) -> bool {
    isolates.contains(gear) || scoped.application_pins.iter().any(|p| p.anchor == *gear)
}

/// The gear a process is named and identified by.
///
/// The REST host when there is one, because that gear owns the composed router
/// and is what a reader calls the process. Otherwise the first gear in
/// dependency order -- arbitrary but stable, and it affects nothing but the name.
fn pick_anchor(catalogue: &Catalogue, gears: &[GearId]) -> Option<GearId> {
    gears
        .iter()
        .find(|g| has_cap(catalogue, g, RuntimeCap::RestHost))
        .or_else(|| gears.last())
        .cloned()
}

/// Refuse an `application(...)` naming a role its anchor does not declare.
///
/// Both halves of the join are strings, so a typo is silent: the application
/// would be built under a directory name no instance ever registers. Reported
/// here rather than where `product.gdl` is lowered, because that layer has no
/// catalogue to ask.
fn report_unknown_roles(
    catalogue: &Catalogue,
    scoped: &ProfileScoped<'_>,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for pin in &scoped.application_pins {
        let Some(role) = pin.role.as_deref() else {
            continue;
        };
        let Some(descriptor) = catalogue.gears.get(&pin.anchor) else {
            continue;
        };
        if descriptor.declared_roles.iter().any(|r| r.name == role) {
            continue;
        }
        let declared: Vec<&str> = descriptor
            .declared_roles
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        let help = if declared.is_empty() {
            format!("`{}` declares no roles at all", pin.anchor)
        } else {
            format!("`{}` declares {}", pin.anchor, declared.join(", "))
        };
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::TopologyUnknownRole,
                format!(
                    "`application(\"{}\")` names role `{role}` on `{}`, which declares no such \
                     role",
                    pin.name, pin.anchor
                ),
                help,
            )
            .at(gearbox_ir::Location::or_file(pin.declared_at.as_ref(), uri)),
        );
    }
}

/// Report a selected gear whose roles cannot all be deployed.
///
/// A role registers under its own directory name, so a role-split gear is
/// several named workloads built from one binary. An application here is the
/// co-location closure of an anchor gear and there is one per anchor, so the
/// second role has nowhere to be.
///
/// Reported from resolution rather than from the load, because it is a claim
/// about a product: a gear whose roles nobody selects costs nothing, and the
/// catalogue cannot know which those are.
fn report_undeployable_roles(
    catalogue: &Catalogue,
    closure: &Closure,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for gear in closure.members.keys() {
        let Some(descriptor) = catalogue.gears.get(gear) else {
            continue;
        };
        let roles = &descriptor.declared_roles;
        if roles.len() < 2 {
            continue;
        }
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::TopologyRolesNotDeployable,
                format!(
                    "`{gear}` declares {} roles, and a product builds one application per \
                     anchor gear -- so at most one of them is deployed",
                    roles.len()
                ),
            )
            .at(Location::file(uri.to_owned()))
            .with_help(
                "the runtime takes whichever directory name it is given; what cannot express \
                 the second workload is this tool. Keep the roles knowing only one is built, \
                 or split the gear",
            ),
        );
    }
}

/// Complaints an embedded profile owes the operator.
///
/// Downgraded rather than refused: the product is still buildable as one
/// process, and the operator probably wants to see it.
fn report_embedded_violations(
    cuts: &Cuts,
    scoped: &ProfileScoped<'_>,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    for pin in &scoped.application_pins {
        diagnostics.push(embedded_violation(
            &format!(
                "`application(\"{}\")` asks for a second application",
                pin.name
            ),
            gearbox_ir::Location::or_file(pin.declared_at.as_ref(), uri),
        ));
        if pin.replicas > 1 {
            diagnostics.push(embedded_violation(
                &format!(
                    "`application(\"{}\", replicas = {})` asks for more than one copy",
                    pin.name, pin.replicas
                ),
                gearbox_ir::Location::or_file(pin.declared_at.as_ref(), uri),
            ));
        }
    }
    if !cuts.cuttable.is_empty() {
        // Worth saying out loud: the product *has* separable edges and this
        // profile is declining to use them. Silence reads as "nothing was
        // separable", which is a different and more discouraging fact.
        diagnostics.push(embedded_violation(
            &format!(
                "{} severable edge(s) stay local because the profile is single-application",
                cuts.cuttable.len()
            ),
            // The only one of the three that is about the profile rather than an
            // `application(...)`, and the profile declaration is not in scope
            // here -- so the file, honestly, until it is threaded.
            Location::file(uri.to_owned()),
        ));
    }
}

/// Build one process from its gear set.
#[expect(
    clippy::too_many_arguments,
    reason = "each is a distinct fact the application records; a bundle would be named after \
              this function and explain nothing"
)]
fn build(
    catalogue: &Catalogue,
    anchor: GearId,
    gears: Vec<GearId>,
    kind: ApplicationKind,
    name: ApplicationId,
    replicas: Option<u32>,
    role: Option<ApplicationRole>,
    used_names: &mut BTreeSet<String>,
) -> ResolvedApplication {
    used_names.insert(name.to_string());

    ResolvedApplication {
        role,
        rest_host: gears
            .iter()
            .find(|g| has_cap(catalogue, g, RuntimeCap::RestHost))
            .cloned(),
        grpc_hub: gears
            .iter()
            .find(|g| has_cap(catalogue, g, RuntimeCap::GrpcHub))
            .cloned(),
        needs_db: gears.iter().any(|g| has_cap(catalogue, g, RuntimeCap::Db)),
        bin_name: format!("gbx-{name}"),
        crate_name: format!("gbx-{name}"),
        replicas: replicas.unwrap_or(1),
        entrypoint: Entrypoint::for_kind(kind),
        cargo_features: features(catalogue, &gears),
        gears,
        name,
        kind,
        anchor,
        listens: Vec::new(),
        spawns: Vec::new(),
        serve: None,
        image: None,
        subchart: None,
        unit_chart: None,
        service_port: None,
    }
}

/// A process name derived from its anchor, or `None` if none can be.
///
/// `GearId` and `ApplicationId` share `validate_kebab` (`gearbox-ir/src/ids.rs`), so
/// in practice this always succeeds. It returns an `Option` rather than asserting
/// that, because the honest consequence of an unnameable process is that the
/// process is not built -- and then its gears are unplaced, which the orphan
/// check already reports. An impossible case degrades into a diagnosed one
/// instead of a panic.
fn derive_name(anchor: &GearId, used: &BTreeSet<String>) -> Option<ApplicationId> {
    ApplicationId::new(unique(anchor.as_str(), used)).ok()
}

/// A name not already taken, suffixed `-2`, `-3`, ... on collision.
///
/// Bounded rather than unbounded: the loop cannot run longer than the number of
/// processes already named, and an upper limit makes that a fact of the code
/// rather than of the caller.
fn unique(base: &str, used: &BTreeSet<String>) -> String {
    if !used.contains(base) {
        return base.to_owned();
    }
    let limit = u32::try_from(used.len())
        .unwrap_or(u32::MAX)
        .saturating_add(2);
    (2u32..=limit)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !used.contains(candidate))
        .unwrap_or_else(|| base.to_owned())
}

/// The seeds' closure in dependency order: every gear after everything it
/// depends on, which is the order the registry builds them in.
fn topo_sort(catalogue: &Catalogue, closure: &Closure, seeds: &[GearId]) -> Vec<GearId> {
    enum Step {
        Enter(GearId),
        Emit(GearId),
    }

    let mut plugins: BTreeMap<&GearId, BTreeSet<&GearId>> = BTreeMap::new();
    for (id, reasons) in &closure.members {
        for reason in reasons {
            if let InclusionReason::PluginOf { host, .. } = reason {
                plugins.entry(host).or_default().insert(id);
            }
        }
    }

    // Plugin attachment determines membership, not initialization precedence:
    // a plugin may itself depend on its host. First collect the full process
    // closure, then sort only real dependency edges to avoid introducing cycles.
    let mut reachable = BTreeSet::new();
    let mut pending = seeds.to_vec();
    while let Some(gear) = pending.pop() {
        if !reachable.insert(gear.clone()) {
            continue;
        }
        if let Some(selected) = plugins.get(&gear) {
            pending.extend(selected.iter().map(|id| (*id).clone()));
        }
        if let Some(descriptor) = catalogue.gears.get(&gear) {
            pending.extend(
                descriptor
                    .colocated_deps
                    .iter()
                    .filter(|id| closure.contains(id))
                    .cloned(),
            );
        }
    }
    let mut ordered: Vec<GearId> = Vec::new();
    let mut done: BTreeSet<GearId> = BTreeSet::new();
    // Visit explicit seeds first, then any plugins not reached by dependency edges.
    let mut stack: Vec<Step> = seeds
        .iter()
        .chain(reachable.iter())
        .cloned()
        .rev()
        .map(Step::Enter)
        .collect();
    let mut open: BTreeSet<GearId> = BTreeSet::new();
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(gear) => {
                if done.contains(&gear) || !open.insert(gear.clone()) {
                    continue;
                }
                stack.push(Step::Emit(gear.clone()));
                if let Some(descriptor) = catalogue.gears.get(&gear) {
                    // Reversed so the sorted order survives the stack.
                    for dep in descriptor.colocated_deps.iter().rev() {
                        if closure.contains(dep) && !done.contains(dep) {
                            stack.push(Step::Enter(dep.clone()));
                        }
                    }
                }
            }
            Step::Emit(gear) => {
                open.remove(&gear);
                if done.insert(gear.clone()) {
                    ordered.push(gear);
                }
            }
        }
    }
    ordered
}

fn has_cap(catalogue: &Catalogue, gear: &GearId, cap: RuntimeCap) -> bool {
    catalogue
        .gears
        .get(gear)
        .is_some_and(|g| g.runtime_caps.contains(&cap))
}

/// Cargo features the process needs, unioned over its gears.
fn features(catalogue: &Catalogue, gears: &[GearId]) -> BTreeSet<String> {
    gears
        .iter()
        .filter_map(|g| catalogue.gears.get(g))
        .flat_map(|g| g.package.features.iter().cloned())
        .collect()
}

/// A gear in the product that landed in no process at all.
///
/// Only possible when a gear is selected and nothing anchors a closure reaching
/// it -- which means it would be silently dropped from every binary. Reported
/// rather than tolerated: the operator asked for it.
/// Gears replicated into every process by design, so a second copy is the
/// point rather than a defect (ADR `cpt-gearbox-adr-referenced-gears`, "a third
/// category").
const REPLICATED_BY_DESIGN: &[&str] = &["types-registry", "authn-resolver"];

/// GBX0414: a gear the description sends to its own application -- a pin or an
/// isolate -- that another application also links through `deps`.
fn report_duplicated_by_deps(
    input: &Inputs<'_>,
    applications: &[ResolvedApplication],
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    let separated = input
        .scoped
        .application_pins
        .iter()
        .map(|pin| (&pin.anchor, pin.declared_at.as_ref()))
        .chain(input.isolates.iter().map(|gear| (gear, None)));
    for (gear, declared_at) in separated {
        if REPLICATED_BY_DESIGN.contains(&gear.as_str()) {
            continue;
        }
        for application in applications.iter().filter(|a| a.anchor != *gear) {
            if !application.gears.contains(gear) {
                continue;
            }
            // Brought in on purpose by an include of that application.
            let included = input
                .scoped
                .application_pins
                .iter()
                .any(|pin| pin.name == application.name && pin.include.contains(gear));
            if included {
                continue;
            }
            let linker = application.gears.iter().find(|g| {
                *g != gear
                    && input
                        .catalogue
                        .gears
                        .get(g)
                        .is_some_and(|d| d.colocated_deps.contains(gear))
            });
            let chain = linker.map_or_else(
                || format!("a `deps` chain in `{}`", application.name),
                |linker| format!("{linker} -> {gear}"),
            );
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::PlacementDuplicatedByDeps,
                    format!(
                        "`{gear}` has its own application, but `{}` also links it through \
                         `deps` ({chain}): the product would run two instances",
                        application.name
                    ),
                    format!(
                        "make the edge severable at its source -- `#[toolkit::consumes]` plus \
                         `consume(...)` in place of the `deps` entry{} -- or drop the pin",
                        linker.map_or_else(String::new, |l| format!(" of `{l}`"))
                    ),
                )
                .at(Location::or_file(declared_at, uri)),
            );
        }
    }
}

fn report_orphans(
    closure: &Closure,
    applications: &[ResolvedApplication],
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    let placed: BTreeSet<&GearId> = applications.iter().flat_map(|p| p.gears.iter()).collect();
    for gear in closure.members.keys() {
        if !placed.contains(gear) {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::TopologyOrphanGear,
                    format!("`{gear}` is in the product but was placed in no application"),
                    "nothing anchors a co-location closure that reaches it; either something \
                     must depend on it, or it needs its own application via `application(...)`",
                )
                .at(Location::file(uri.to_owned())),
            );
        }
    }
}

/// `at` is decided by the caller, because two of the three things this says are
/// about an `application(...)` and the third is about the profile.
fn embedded_violation(what: &str, at: gearbox_ir::Location) -> Diagnostic {
    Diagnostic::new(
        DiagnosticCode::TopologyEmbeddedViolation,
        format!("{what}, and the embedded profile is one application by definition"),
    )
    .with_help(
        "resolved as a single application anyway, so the product is still buildable; resolve for a \
         `self_hosted` or `kubernetes` profile to get the topology this asks for",
    )
    .at(at)
}
