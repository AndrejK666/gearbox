//! `config/<p>.yaml` -- the runtime `AppConfig` for one process.
//!
//! Built as typed Rust and serialized with `serde-saphyr`, pinned to the exact
//! version `gears-rust` deserializes with. That pin is the point: the file is
//! read back by `toolkit::bootstrap::AppConfig`, whose per-gear bag is
//! `HashMap<String, serde_json::Value>` and whose sections carry
//! `deny_unknown_fields`. A YAML dialect that merely looks right -- a different
//! quoting rule, a different treatment of an empty map -- produces a file that
//! loads on one side and is rejected on the other.
//!
//! What is written is only what the lock decided. Everything the runtime already
//! has a default for is left out, so a reader can tell a resolved fact from a
//! restated one, and so a change in a platform default reaches the generated
//! product instead of being frozen into it.

use std::collections::{BTreeMap, BTreeSet};

use gearbox_ir::{
    ApplicationKind, Catalogue, ClusterResolution, FileEntry, FileKind, GearId, Ownership,
    ResolvedApplication, SpawnSpec,
};
use serde::Serialize;
use serde_json::{Map, Value};

use super::{GenerateError, GenerateInput, header, paths};

/// The gear that owns cluster configuration.
///
/// Named rather than discovered: `profiles` is a key inside *that* gear's
/// config, and no other gear reads it.
const CLUSTER_GEAR: &str = "cluster";

/// The REST host gear whose public listener [`write_public_listener`] sets.
const REST_HOST_GEAR: &str = "api-gateway";

#[derive(Serialize)]
struct AppConfig {
    server: ServerSection,
    #[serde(skip_serializing_if = "Option::is_none")]
    database: Option<DatabaseSection>,
    /// How a worker serves its own REST surface. Absent for a host.
    ///
    /// Its **absence is a decision the runtime acts on**: without this section
    /// the out-of-process runtime takes the legacy gRPC-only path and never
    /// registers the gear with the directory at all. A lock that says a binding
    /// resolves `via directory` would then be describing something that cannot
    /// happen, so this is written whenever the process is a worker.
    #[serde(skip_serializing_if = "Option::is_none")]
    oop_http: Option<OopHttpSection>,
    /// The runtime's built-in HTTP host, for a host process whose REST gears
    /// have no REST host gear. Without it the runtime refuses to start
    /// (`RestRequiresHost`).
    #[serde(skip_serializing_if = "Option::is_none")]
    http_host: Option<HttpHostSection>,
    /// A host's outbound platform credential: what its directory-resolving
    /// clients attach when a consumed contract is served by another process (an
    /// `AuthZ` plane in its own unit). A worker carries it in `oop_http` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    internal_auth: Option<Value>,
    /// The product's permission mapping: every process enforces the
    /// permissions its operations declare against it (gears-rust
    /// `cpt-cf-adr-operation-permissions` §2).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    permissions: BTreeMap<String, Vec<String>>,
    gears: BTreeMap<String, GearSection>,
}

/// The built-in host's listener: a public one, serving only exposed
/// operations. That is also the runtime's default; it is written so the file
/// says what the process serves.
#[derive(Serialize)]
struct HttpHostSection {
    listen_addr: String,
    public: PublicSection,
}

#[derive(Serialize)]
struct PublicSection {
    exposed_only: bool,
}

#[derive(Serialize)]
struct OopHttpSection {
    listen_addr: String,
    advertise_uri: String,
    /// The runtime defaults this to `false` and refuses to start when the
    /// advertised host is loopback without it.
    allow_loopback_advertise: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    internal_auth: Option<Value>,
    /// The further gears this worker keeps a directory record for.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    directory_gears: Vec<String>,
}

#[derive(Serialize)]
struct ServerSection {
    home_dir: String,
}

#[derive(Serialize)]
struct GearSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    database: Option<GearDatabaseSection>,
    /// Present only for a gear the host spawns out of process.
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<RuntimeSection>,
    /// Always present, even when empty. `toolkit`'s `GearConfig` defaults it,
    /// but `list_gear_names` reads the *keys of the gears map*, so a gear with
    /// nothing to configure still has to appear or the composed set and the
    /// configured set stop matching -- and that match is one of the two oracles
    /// this milestone is verified by.
    #[serde(serialize_with = "super::json::map")]
    config: Map<String, Value>,
}

#[derive(Serialize)]
struct RuntimeSection {
    #[serde(rename = "type")]
    kind: &'static str,
    execution: ExecutionSection,
}

#[derive(Serialize)]
struct ExecutionSection {
    executable_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    working_directory: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    environment: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct DatabaseSection {
    servers: BTreeMap<&'static str, PostgresServerSection>,
}
#[derive(Serialize)]
struct PostgresServerSection {
    engine: &'static str,
    host: String,
    port: u16,
    user: String,
    password: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<String, String>,
}
#[derive(Serialize)]
#[serde(untagged)]
enum GearDatabaseSection {
    Postgres {
        server: &'static str,
        dbname: String,
    },
    Sqlite {
        engine: &'static str,
        file: String,
    },
}

fn write_database(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
    gears: &mut BTreeMap<String, GearSection>,
) -> Result<Option<DatabaseSection>, GenerateError> {
    let mut used = false;
    for id in &application.gears {
        if !input
            .lock
            .gears
            .get(id)
            .is_some_and(|gear| gear.runtime_caps.contains(&gearbox_ir::RuntimeCap::Db))
        {
            continue;
        }
        if let Some(sqlite) = input.lock.sqlite_databases.get(id) {
            sqlite.validate().map_err(GenerateError::Database)?;
            if let Some(gear) = gears.get_mut(id.as_str()) {
                gear.database = Some(GearDatabaseSection::Sqlite {
                    engine: "sqlite",
                    file: sqlite.file.clone(),
                });
            }
            continue;
        }
        let db = input.lock.database.as_ref().ok_or_else(|| {
            GenerateError::Database(format!(
                "gear `{id}` needs a database; re-resolve with database = postgres(...)"
            ))
        })?;
        db.validate().map_err(GenerateError::Database)?;
        let name = db.database_names.get(id).ok_or_else(|| {
            GenerateError::Database(format!(
                "lock has no database name for gear `{id}`; re-resolve the product"
            ))
        })?;
        if let Some(gear) = gears.get_mut(id.as_str()) {
            gear.database = Some(GearDatabaseSection::Postgres {
                server: "pg",
                dbname: name.clone(),
            });
        }
        used = true;
    }
    Ok(if used {
        input.lock.database.as_ref().map(|db| DatabaseSection {
            servers: BTreeMap::from([(
                "pg",
                PostgresServerSection {
                    engine: "postgres",
                    host: db.host.clone(),
                    port: db.port,
                    user: db.user.clone(),
                    password: db.password.clone(),
                    params: db.params.clone(),
                },
            )]),
        })
    } else {
        None
    })
}

/// The runtime configuration for one process.
///
/// # Errors
/// Returns [`GenerateError::Yaml`] if the configuration cannot be serialized.
pub fn app_config(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
) -> Result<FileEntry, GenerateError> {
    // The description's own values first, then the projected facts on top.
    //
    // The order is the point: a port, a client's wiring and a cluster binding are
    // *derived* from the topology the resolver decided, so a hand-written value
    // for one of them would be describing a product that was not resolved. The
    // operator's keys go in underneath, where they belong.
    let mut gears: BTreeMap<String, GearSection> = application
        .gears
        .iter()
        .map(|id| {
            let config = input.lock.gears.get(id).map_or_else(Map::new, |gear| {
                let mut config: Map<String, Value> = gear
                    .config
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                // Secrets are already gone: `generate` redacts the whole product
                // before any of this runs, so there is nothing here to find.
                if let Some(catalogue) = input.catalogue {
                    drop_unknown_keys(catalogue, id, &mut config);
                }
                config
            });
            (
                id.to_string(),
                GearSection {
                    database: None,
                    runtime: None,
                    config,
                },
            )
        })
        .collect();

    let database = write_database(input, application, &mut gears)?;
    write_endpoints(application, &mut gears)?;
    write_consumer_wiring(input, application, &mut gears);
    write_cluster_sdk_wiring(input, application, &mut gears);
    write_cluster(input, &mut gears);
    write_spawns(input, application, &mut gears);
    write_public_listener(input, application, &mut gears);
    write_gateway_proxy(input, &mut gears);

    // A production hub on a socket refuses a directory other local users can
    // reach, rather than warning about it (gears-rust ADR-0011 section 3).
    if !input.lock.product.development
        && let Some(hub) = gears.get_mut("grpc-hub")
        && hub
            .config
            .get("listen_addr")
            .and_then(Value::as_str)
            .is_some_and(|addr| addr.starts_with("uds://"))
    {
        hub.config
            .entry("strict_socket_dir")
            .or_insert(Value::Bool(true));
    }

    // One platform-plane provider for the embedded host and its workers. The
    // product supplies secret values through AppConfig's APP__ environment
    // overrides; no credential value is needed during generation.
    // The directory admits registrars from the release's namespace only: an
    // empty allowlist would accept a same-named ServiceAccount from anywhere.
    //
    // **On development profiles too.** This was production-only, and Family
    // Labs -- `development = True` on a real cluster with `platform_auth =
    // "kube"` -- got an empty list: any namespace's `settings-service`
    // ServiceAccount could register that gear's endpoint and receive its
    // traffic and forwarded credentials. A namespace is known on every
    // Kubernetes profile, so there is no reason to leave it open.
    if let (Some(kubernetes), Some(directory)) =
        (&input.lock.kubernetes, gears.get_mut("service-discovery"))
    {
        directory
            .config
            .entry("platform_namespaces")
            .or_insert_with(|| {
                Value::Array(vec![Value::String(
                    kubernetes
                        .namespace
                        .clone()
                        .unwrap_or_else(|| "default".to_owned()),
                )])
            });
    }

    write_registrar_gears(input, &mut gears);

    let internal_auth = input
        .lock
        .gears
        .iter()
        .find(|(id, _)| id.as_str() == REST_HOST_GEAR)
        .and_then(|(_, host)| host.config.get("internal_auth"))
        .filter(|value| !value.is_null())
        .cloned();
    if let (Some(auth), Some(grpc)) = (&internal_auth, gears.get_mut("grpc-hub")) {
        grpc.config
            .entry("internal_auth")
            .or_insert_with(|| auth.clone());
        // A production hub that validates platform tokens enforces them
        // (ADR-0011 §3); written under the description's value.
        if !input.lock.product.development {
            grpc.config
                .entry("internal_auth_enforcement")
                .or_insert(Value::String("required".to_owned()));
        }
    }

    let home_dir = if input.lock.kubernetes.is_some() {
        // `readOnlyRootFilesystem` makes `~` unwritable; the chart mounts an
        // emptyDir here. The path is a generator fact, not a lock one: the
        // runtime only cares that `create_dir_all` succeeds.
        super::K8S_HOME_DIR.to_owned()
    } else {
        format!(
            "~/.cf-gears/{}/{}",
            input.lock.product.id, input.lock.product.profile
        )
    };
    let config = AppConfig {
        server: ServerSection { home_dir },
        database,
        oop_http: application
            .serve
            .as_ref()
            .filter(|_| application.is_worker())
            .map(|serve| OopHttpSection {
                listen_addr: serve.listen_addr.clone(),
                advertise_uri: serve.advertise_uri.clone(),
                allow_loopback_advertise: serve.allow_loopback_advertise,
                internal_auth: internal_auth.clone(),
                directory_gears: directory_gears(input.lock, application)
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            }),
        http_host: application
            .serve
            .as_ref()
            .filter(|_| !application.is_worker())
            .map(|serve| HttpHostSection {
                listen_addr: serve.listen_addr.clone(),
                public: PublicSection { exposed_only: true },
            }),
        internal_auth: internal_auth.filter(|_| !application.is_worker()),
        permissions: input.lock.permissions.clone(),
        gears,
    };

    let body = serde_saphyr::to_string(&config).map_err(|source| GenerateError::Yaml {
        what: "the runtime configuration",
        source,
    })?;

    Ok(FileEntry::text(
        paths::rel(&["config", &format!("{}.yaml", application.name)])?,
        format!("{}\n{body}", header("#")),
        FileKind::Yaml,
        Ownership::Generated,
    ))
}

/// The REST host's public listener serves only exposed operations in an
/// embedded profile and in every production one (Gearbox ADR-0026 section 6,
/// test cases G18 and P4).
///
/// The all-in-one binary has no other process, so nothing but the public
/// surface should answer. A production multi-process gateway also has the
/// internal listener the resolver assigned (`internal_bind_addr`), whose
/// advertised address is written here. Both are written *under* the
/// description's values; turning the filter off is GBX0325 on a production
/// profile. A development multi-process profile keeps the gateway's single
/// listener.
/// A self-hosted host's edge proxies to its workers' routes, found in its own
/// directory through its own hub.
///
/// On Kubernetes Flight Control's chart sets `gateway_proxy` itself. A generated
/// self-hosted host had none, so its edge published nothing a worker served and
/// answered 404 for every worker route (A23). The hub is addressed as it
/// listens: `uds://~/...` (the gateway expands `~` as the hub does) or a TCP
/// address. A `gateway_proxy` the description wrote is kept.
fn write_gateway_proxy(input: &GenerateInput<'_>, gears: &mut BTreeMap<String, GearSection>) {
    let directory = input
        .lock
        .self_hosted
        .as_ref()
        .is_some_and(|settings| settings.discovery == gearbox_ir::Discovery::Directory);
    let hosts_directory = gears.contains_key("service-discovery");
    if !directory || !hosts_directory || input.lock.applications.len() < 2 {
        return;
    }
    let Some(endpoint) = gears
        .get("grpc-hub")
        .and_then(|hub| hub.config.get("listen_addr"))
        .and_then(Value::as_str)
        .filter(|addr| *addr != "none")
        .map(|addr| {
            if addr.contains("://") {
                addr.to_owned()
            } else {
                format!("http://{addr}")
            }
        })
    else {
        return;
    };
    if let Some(gateway) = gears.get_mut(REST_HOST_GEAR) {
        gateway.config.entry("gateway_proxy").or_insert_with(
            || serde_json::json!({"enabled": true, "directory_endpoint": endpoint}),
        );
    }
}

fn write_public_listener(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
    gears: &mut BTreeMap<String, GearSection>,
) {
    let embedded = input.lock.kubernetes.is_none() && input.lock.self_hosted.is_none();
    let production = !input.lock.product.development;
    if let Some(gateway) = gears
        .get_mut(REST_HOST_GEAR)
        .filter(|_| embedded || production)
    {
        let public = gateway
            .config
            .entry("public")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(public) = public {
            public.entry("exposed_only").or_insert(Value::Bool(true));
        }
        // The internal listener's advertised address (`internal_bind_addr`
        // itself is the endpoint's config key, written with the others).
        if let Some(uri) = application
            .listens
            .iter()
            .find(|e| e.name == "internal" && e.gear.as_str() == REST_HOST_GEAR)
            .and_then(|e| e.advertise_uri.clone())
        {
            gateway
                .config
                .entry("internal_advertise_uri")
                .or_insert(Value::String(uri));
        }
    }
}

/// What a unit chart is given for one process: the description's own
/// configuration of each of its gears, and the cluster profiles the resolver
/// bound -- not the topology the generator would have written (ports,
/// endpoints, wiring, spawns), which the chart owns
/// (`ResolvedApplication::unit_chart`). Merged by the chart over its own
/// configuration, before its production rules are checked.
pub(super) fn unit_chart_overlay(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
) -> Result<Map<String, Value>, GenerateError> {
    let mut gears: BTreeMap<String, GearSection> = application
        .gears
        .iter()
        .map(|id| {
            let mut config: Map<String, Value> =
                input.lock.gears.get(id).map_or_else(Map::new, |gear| {
                    gear.config
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                });
            if let Some(catalogue) = input.catalogue {
                drop_unknown_keys(catalogue, id, &mut config);
            }
            (
                id.to_string(),
                GearSection {
                    database: None,
                    runtime: None,
                    config,
                },
            )
        })
        .collect();
    // The database the description resolved, as for a generated process: the
    // server section and each gear's database. Its password stays a `${VAR}`
    // the chart's `secretEnv` maps from the existing Secret.
    let database = write_database(input, application, &mut gears)?;
    write_cluster(input, &mut gears);
    // A hand-written unit (Flight Control) may host the directory: it admits the
    // workers' further records only if the overlay says so.
    write_registrar_gears(input, &mut gears);
    let mut gear_sections = Map::new();
    for (id, section) in gears {
        let mut gear = Map::new();
        if let Some(db) = &section.database {
            gear.insert("database".to_owned(), to_json(db)?);
        }
        if !section.config.is_empty() {
            gear.insert("config".to_owned(), Value::Object(section.config));
        }
        if !gear.is_empty() {
            gear_sections.insert(id, Value::Object(gear));
        }
    }
    let mut overlay = Map::new();
    overlay.insert("gears".to_owned(), Value::Object(gear_sections));
    // A hand-written unit enforces the product's permissions like a generated one.
    if !input.lock.permissions.is_empty() {
        overlay.insert("permissions".to_owned(), to_json(&input.lock.permissions)?);
    }
    if let Some(database) = &database {
        overlay.insert("database".to_owned(), to_json(database)?);
    }
    Ok(overlay)
}

/// The directory admits a worker for the further gears it registers, and for
/// nothing else (gears-rust ADR-0010 section 6). Merged with what the
/// description lists, so its own entries still stand.
fn write_registrar_gears(input: &GenerateInput<'_>, gears: &mut BTreeMap<String, GearSection>) {
    let registrars = registrar_gears(input.lock);
    if registrars.is_empty() {
        return;
    }
    let Some(directory) = gears.get_mut("service-discovery") else {
        return;
    };
    let entry = directory
        .config
        .entry("registrar_gears".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(map) = entry.as_object_mut() else {
        return;
    };
    for (registrar, listed) in registrars {
        let gears = map
            .entry(registrar)
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(gears) = gears.as_array_mut() {
            for gear in listed {
                let gear = Value::String(gear.to_string());
                if !gears.contains(&gear) {
                    gears.push(gear);
                }
            }
        }
    }
}

fn to_json(value: &impl Serialize) -> Result<Value, GenerateError> {
    serde_json::to_value(value).map_err(|source| GenerateError::Json {
        what: "a unit chart's configuration overlay",
        source,
    })
}

/// Drop keys the projected schema does not name.
///
/// The resolver already reports [`gearbox_ir::DiagnosticCode::GdlUnknownConfigKey`];
/// this is the generator's half of the same rule, so a lock that still carries
/// a typo cannot write a file `deny_unknown_fields` will refuse.
fn drop_unknown_keys(catalogue: &Catalogue, id: &GearId, config: &mut Map<String, Value>) {
    let Some(schema) = catalogue.gear(id).and_then(|g| g.config_schema.as_ref()) else {
        return;
    };
    let known: BTreeSet<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
    config.retain(|key, _| known.contains(key.as_str()));
}
/// Each socket the resolver assigned, under the gear's own configuration key.
///
/// The key comes from the gear's `serves` declaration, not from a table here:
/// the REST host calls it `bind_addr` and the gRPC hub calls it `listen_addr`,
/// and the generator has to write whichever one the gear actually reads.
fn write_endpoints(
    application: &ResolvedApplication,
    gears: &mut BTreeMap<String, GearSection>,
) -> Result<(), GenerateError> {
    for endpoint in &application.listens {
        let Some(section) = gears.get_mut(endpoint.gear.as_str()) else {
            // A `listens` entry for a gear not in the process is a resolver
            // bug. Skipping it writes no wrong key; `report_orphans` is what
            // notices the gear itself is missing.
            continue;
        };
        insert_path(
            &mut section.config,
            &endpoint.config_key,
            Value::String(endpoint.address.clone()),
        )?;
    }
    Ok(())
}

/// Endpoint overrides for edges that cross a process boundary.
///
/// Written into the *consumer's* configuration, because that is where
/// `StaticEndpointResolver` reads them. A co-located binding gets nothing at
/// all -- absence is what makes the client hub's local lookup win, and writing
/// a loopback URL for it would be a configuration key that silently overrides a
/// decision the lock says was made at link time.
fn write_consumer_wiring(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
    gears: &mut BTreeMap<String, GearSection>,
) {
    for binding in &input.lock.bindings {
        if binding.consumer_application != application.name || !binding.is_remote() {
            continue;
        }
        // The cluster SDK derives its own address and reads its criticality
        // from a process-wide section (`write_cluster_sdk_wiring`).
        if binding.mechanism == gearbox_ir::BindingMechanism::ProvidesClientWiring {
            continue;
        }
        let Some(section) = gears.get_mut(binding.consumer.as_str()) else {
            continue;
        };
        let wiring = section
            .config
            .entry("consumer_wiring".to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(map) = wiring.as_object_mut() else {
            continue;
        };
        // Toolkit reads ConsumerWiring as an object with an endpoint plus
        // optional flattened client policies. Keep configured policies when
        // adding what resolution decided.
        let provider = binding.provider.to_string();
        let mut options = map
            .remove(&provider)
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        if let Some(endpoint) = &binding.endpoint {
            options.insert("endpoint".to_owned(), Value::String(endpoint.clone()));
        } else if let Some(name) = directory_name(input, binding) {
            options.insert("directory_name".to_owned(), Value::String(name));
        }
        // Written for every remote binding; absent would mean critical too
        // (gears-rust ADR-0010 section 3), but the file should say what was
        // decided. Two majors of one provider share this slot, so it is critical
        // if either is.
        let critical = binding.critical
            || options
                .get("critical")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        options.insert("critical".to_owned(), Value::Bool(critical));
        map.insert(provider, Value::Object(options));
    }
}

/// `gears.cluster-sdk.config.consumer_wiring.cluster.critical` for a process
/// whose cluster requesters reach `cluster` remotely: one SDK client per
/// process, so one key, true if any requester's binding is critical
/// (gears-rust ADR-0010 section 3, Gearbox ADR-0026 section 5).
fn write_cluster_sdk_wiring(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
    gears: &mut BTreeMap<String, GearSection>,
) {
    let mut remote = input.lock.bindings.iter().filter(|b| {
        b.consumer_application == application.name
            && b.mechanism == gearbox_ir::BindingMechanism::ProvidesClientWiring
    });
    let Some(first) = remote.next() else {
        return;
    };
    let endpoint = first.endpoint.clone();
    let critical = first.critical || remote.any(|b| b.critical);
    let mut options = Map::new();
    // Pinned under static discovery only. The SDK still derives its channel
    // from Kubernetes DNS; the key makes the runtime treat the dependency as a
    // static endpoint, resolved at wiring, instead of waiting for a directory
    // the profile does not have.
    if let Some(endpoint) = endpoint {
        options.insert("endpoint".to_owned(), Value::String(endpoint));
    } else if let Some(name) = directory_name(input, first) {
        // Readiness waits for the directory to know `cluster`; a worker that
        // links it beside another anchor registers under that anchor's name.
        options.insert("directory_name".to_owned(), Value::String(name));
    }
    options.insert("critical".to_owned(), Value::Bool(critical));
    let mut wiring = Map::new();
    wiring.insert("cluster".to_owned(), Value::Object(options));
    let mut config = Map::new();
    config.insert("consumer_wiring".to_owned(), Value::Object(wiring));
    gears.insert(
        "cluster-sdk".to_owned(),
        GearSection {
            database: None,
            runtime: None,
            config,
        },
    );
}

/// The directory name to look the provider up under, when it is not the
/// provider's own: a gear a worker links beside its anchor (or role) and does
/// not keep a record for is found under the worker's name. A REST provider the
/// worker lists in `directory_gears`, and every REST gear of a host, is found
/// under its own (gears-rust ADR-0010 section 6).
fn directory_name(
    input: &GenerateInput<'_>,
    binding: &gearbox_ir::ResolvedBinding,
) -> Option<String> {
    let provider_application = input
        .lock
        .applications
        .iter()
        .find(|a| a.name == binding.provider_application)?;
    let registers_as = provider_application.registers_as();
    (provider_application.is_worker()
        && registers_as != binding.provider.as_str()
        && !directory_gears(input.lock, provider_application).contains(&binding.provider))
    .then(|| registers_as.to_owned())
}

/// The further gears a worker keeps a directory record for, beside the name it
/// registers under (gears-rust ADR-0010 section 6): each REST gear it provides
/// to a consumer in another process.
///
/// A gear another application already registers under that name is left out:
/// a second record would split its consumers between two endpoints. Its
/// consumers keep finding it under this worker's name.
fn directory_gears(
    lock: &gearbox_ir::ResolvedProduct,
    application: &ResolvedApplication,
) -> BTreeSet<GearId> {
    if !application.is_worker() {
        return BTreeSet::new();
    }
    let rest = |gear: &GearId| {
        lock.gears
            .get(gear)
            .is_some_and(|g| g.runtime_caps.contains(&gearbox_ir::RuntimeCap::Rest))
    };
    let registered_elsewhere = |gear: &GearId| {
        lock.applications
            .iter()
            .filter(|other| other.name != application.name)
            .any(|other| {
                if other.is_worker() {
                    other.registers_as() == gear.as_str()
                } else {
                    other.gears.contains(gear)
                        && lock.gears.get(gear).is_some_and(|g| {
                            !g.runtime_caps.contains(&gearbox_ir::RuntimeCap::RestHost)
                        })
                }
            })
    };
    lock.bindings
        .iter()
        .filter(|b| {
            b.provider_application == application.name
                && b.consumer_application != application.name
                && b.provider.as_str() != application.registers_as()
        })
        .map(|b| b.provider.clone())
        .filter(|gear| rest(gear) && !registered_elsewhere(gear))
        .collect()
}

/// Registrar -> the further gears it registers, for every worker that has any.
/// A worker's identity carries the name it registers under.
fn registrar_gears(lock: &gearbox_ir::ResolvedProduct) -> BTreeMap<String, BTreeSet<GearId>> {
    lock.applications
        .iter()
        .map(|application| {
            (
                application.registers_as().to_owned(),
                directory_gears(lock, application),
            )
        })
        .filter(|(_, gears)| !gears.is_empty())
        .collect()
}

/// The cluster gear's per-scope backend bindings.
///
/// `leader_election` and `lock` are deliberately **not** written when the lock
/// resolved them to the SDK's compare-and-swap default: omitting the key is
/// what engages that default. Writing `leader_election: { provider: cache }`
/// would name a provider that is not registered and fail startup, so the
/// omission is the instruction, not the absence of one.
fn write_cluster(input: &GenerateInput<'_>, gears: &mut BTreeMap<String, GearSection>) {
    let Some(section) = gears.get_mut(CLUSTER_GEAR) else {
        return;
    };

    // **Every binding in the lock, not only the ones a co-located gear asked
    // for.** This used to filter on `binding.requesters.any(process.contains)`,
    // which reads plausibly -- configure what this process needs -- and is
    // wrong, because the section being written is not the requester's. It is the
    // *cluster gear's*, and that gear serves every scope in the product to
    // whoever asks, in-process or over its gRPC surface.
    //
    // The filter therefore produced its worst output exactly where the topology
    // was most real. In the demo's `prod` profile a process pin puts the
    // requester in `audit` while `cluster` stays in `api-gateway`: the two never
    // meet, the intersection is empty, and the cluster gear was configured with
    // `config: {}` -- no postgres, no credential, nothing to serve -- while the
    // lock said the scope resolved. Green resolve, green generate, wrong file.
    //
    // Co-location is not the constraint it was taken for. The cluster gear
    // declares `grpc` and `rest` among its `runtime_caps`, and [`super::helm`]
    // already emits a dedicated `cluster` Service on its gRPC port for whatever
    // process holds it, which is what a remote consumer resolves through.
    let mut profiles: Map<String, Value> = Map::new();
    for binding in &input.lock.cluster {
        let ClusterResolution::Provider { name } = &binding.resolved else {
            continue;
        };

        let mut backend = Map::new();
        backend.insert("provider".to_owned(), Value::String(name.clone()));
        for (key, value) in &binding.options {
            backend.insert(key.clone(), value.clone());
        }
        if let Some(secret) = &binding.secret_ref {
            // `SecretRef { name }`, not a bare string. The cluster gear
            // deserializes this section into its own config struct
            // (`cluster/src/config.rs:135`), and a string there is
            // `invalid type: string, expected struct SecretRef` -- an
            // initialization failure that takes the whole host down before any
            // gear starts. Found by running the generated product; the shape is
            // a fact about the consumer, so it is written the consumer's way.
            let mut reference = Map::new();
            reference.insert("name".to_owned(), Value::String(secret.clone()));
            backend.insert("secret_ref".to_owned(), Value::Object(reference));
        }

        let scope = profiles
            .entry(binding.scope.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(map) = scope.as_object_mut() {
            map.insert(binding.primitive.slug().to_owned(), Value::Object(backend));
        }
    }

    if !profiles.is_empty() {
        section
            .config
            .insert("profiles".to_owned(), Value::Object(profiles));
    }
}

/// Out-of-process execution for each worker this host starts.
///
/// The `runtime` key must exist for every spawned gear, and it is the key
/// rather than its contents that matters: the host builds its spawn table by
/// iterating the configured gears and reading each one's runtime kind, so a
/// worker with no entry is a worker nobody starts.
///
/// **The entry is created rather than found, and that is the whole point.** A
/// spawned gear is deliberately *not* linked into the host -- the runtime's
/// registry discovers gears by `inventory` and takes no notice of
/// `runtime.type`, so a linked-and-spawned gear runs twice, in-process and as a
/// child. Configured and linked are therefore different sets, and this is the
/// one place they differ: the host is configured to start a gear it does not
/// contain. Looking the section up and skipping when absent, which is what this
/// did, silently produced a host that started nothing.
fn write_spawns(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
    gears: &mut BTreeMap<String, GearSection>,
) {
    if !matches!(application.kind, ApplicationKind::Host) {
        return;
    }
    for spawn in &application.spawns {
        let section = gears
            .entry(spawn.gear.to_string())
            .or_insert_with(|| GearSection {
                database: None,
                runtime: None,
                config: Map::new(),
            });
        section.runtime = Some(RuntimeSection {
            kind: "oop",
            execution: execution(input, spawn),
        });
    }
}

fn execution(input: &GenerateInput<'_>, spawn: &SpawnSpec) -> ExecutionSection {
    ExecutionSection {
        executable_path: format!("{}/{}", super::workspace::target_dir(input), spawn.bin_name),
        args: spawn.args.clone(),
        working_directory: spawn.working_directory.clone(),
        environment: spawn.environment.clone(),
    }
}

/// Insert `value` at a possibly dotted configuration key.
///
/// `serves` records the key a gear reads, and nothing stops that key from
/// naming a nested field. Splitting on `.` here means a gear that declares
/// `health.bind_addr` gets a nested map rather than a literal key with a dot in
/// it, which the gear's `deny_unknown_fields` would reject.
fn insert_path(
    target: &mut Map<String, Value>,
    key: &str,
    value: Value,
) -> Result<(), GenerateError> {
    let mut segments = key.split('.').peekable();
    let mut cursor = target;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            cursor.insert(segment.to_owned(), value);
            return Ok(());
        }
        let next = cursor
            .entry(segment.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        // A non-map already sitting at an intermediate segment means two
        // endpoints disagree about the shape of one key. Replacing it would
        // hide that; leaving it alone would drop this endpoint silently.
        match next.as_object_mut() {
            Some(map) => cursor = map,
            None => {
                return Err(GenerateError::ConfigShape {
                    key: key.to_owned(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dotted_key_becomes_a_nested_map() {
        let mut target = Map::new();
        insert_path(&mut target, "health.bind_addr", Value::String("x".into()))
            .expect("nested key");
        let health = target.get("health").and_then(Value::as_object);
        assert_eq!(
            health
                .and_then(|m| m.get("bind_addr"))
                .and_then(Value::as_str),
            Some("x")
        );
    }

    #[test]
    fn a_flat_key_stays_flat() {
        let mut target = Map::new();
        insert_path(&mut target, "bind_addr", Value::String("x".into())).expect("flat key");
        assert_eq!(target.get("bind_addr").and_then(Value::as_str), Some("x"));
    }

    #[test]
    fn a_non_object_intermediate_is_a_shape_error() {
        let mut target = Map::new();
        target.insert(
            "health".to_owned(),
            Value::String("already a scalar".into()),
        );
        let err = insert_path(&mut target, "health.bind_addr", Value::String("x".into()))
            .expect_err("shape conflict");
        assert!(matches!(err, GenerateError::ConfigShape { .. }));
    }
}
