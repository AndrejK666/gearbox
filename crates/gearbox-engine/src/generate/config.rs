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
    gears: BTreeMap<String, GearSection>,
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
    write_cluster(input, &mut gears);
    write_spawns(input, application, &mut gears);

    // One platform-plane provider for the embedded host and its workers. The
    // product supplies secret values through AppConfig's APP__ environment
    // overrides; no credential value is needed during generation.
    let internal_auth = input
        .lock
        .gears
        .iter()
        .find(|(id, _)| id.as_str() == "api-gateway")
        .and_then(|(_, host)| host.config.get("internal_auth"))
        .filter(|value| !value.is_null())
        .cloned();
    if let (Some(auth), Some(grpc)) = (&internal_auth, gears.get_mut("grpc-hub")) {
        grpc.config
            .entry("internal_auth")
            .or_insert_with(|| auth.clone());
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
        oop_http: application.serve.as_ref().map(|serve| OopHttpSection {
            listen_addr: serve.listen_addr.clone(),
            advertise_uri: serve.advertise_uri.clone(),
            allow_loopback_advertise: serve.allow_loopback_advertise,
            internal_auth: internal_auth.clone(),
        }),
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
        let Some(endpoint) = binding.endpoint.as_ref() else {
            continue;
        };
        let Some(section) = gears.get_mut(binding.consumer.as_str()) else {
            continue;
        };
        let wiring = section
            .config
            .entry("consumer_wiring".to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(map) = wiring.as_object_mut() {
            // Toolkit reads ConsumerWiring as an object with an endpoint plus
            // optional flattened client policies. Keep configured policies
            // when adding the resolved static endpoint.
            let provider = binding.provider.to_string();
            let mut options = map
                .remove(&provider)
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            options.insert("endpoint".to_owned(), Value::String(endpoint.clone()));
            map.insert(provider, Value::Object(options));
        }
    }
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
