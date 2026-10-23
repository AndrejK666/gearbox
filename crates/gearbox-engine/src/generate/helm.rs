//! Helm umbrella chart: one subchart per process.
//!
//! Emitted only for a Kubernetes profile. Chart.yaml and values.yaml are
//! serialized data, not templates -- minijinja would turn a version string
//! into an accidental substitution, and "Template text. Serialize data" is
//! the rule this milestone exists to keep. The YAML *inside* `templates/`
//! is Helm, so minijinja runs with `<< >>` delimiters and leaves `{{ }}`
//! for Helm.
//!
//! Service names are the process/subchart names, not Helm fullnames.
//! `advertise_uri` in the lock is `http://{subchart}.{namespace}.svc...`;
//! a Service named `{release}-{name}` would be a DNS name nobody dials.
//! The cluster SDK is stricter still: if the process links the `cluster`
//! gear, a second Service is named exactly `cluster` on port 50051.
//!
//! Secrets never appear in values. `${VAR}` placeholders in the generated
//! config become `secretKeys`, and `secret_ref = "existingSecret:<name>"`
//! on a cluster binding becomes `existingSecret`. The chart injects those
//! names as `secretKeyRef` env; the operator's Secret holds the values.
//! Catalogue `ConfigField.secret` is honoured earlier, when `config/<p>.yaml`
//! is written: a literal credential becomes `${GEAR_FIELD}` so this harvest
//! can name it without putting the value in the chart.

use std::collections::{BTreeMap, BTreeSet};

use gearbox_ir::{ApplicationKind, FileEntry, FileKind, FileSet, Ownership, ResolvedApplication};
use minijinja::context;
use serde::Serialize;

use super::paths;
use super::templates;
use super::{GenerateError, GenerateInput, K8S_HOME_DIR, NONROOT_UID, header, operator_header};

const CLUSTER_SERVICE: &str = "cluster";
const CLUSTER_PORT: u16 = 50051;

#[derive(Serialize)]
struct NamedPort {
    name: String,
    port: u16,
}

/// Every Helm artefact for this lock, or none if the profile does not
/// generate a chart.
///
/// Reads each process's already-generated `config/<p>.yaml` out of `files`
/// so the `ConfigMap`'s companion `files/` copy is the same bytes the binary
/// will `--config`. The template itself uses `.Files.Get`, so YAML that
/// contains `{{` is not evaluated as Helm.
///
/// # Errors
/// Returns [`GenerateError`] when a template cannot render, a path is not
/// a valid relative path, or a process has no image under Kubernetes.
pub fn files(input: &GenerateInput<'_>, files: &FileSet) -> Result<Vec<FileEntry>, GenerateError> {
    if input.lock.kubernetes.is_none() {
        return Ok(Vec::new());
    }

    let product = input.lock.product.id.as_str();
    let version = input.lock.product.version.as_str();
    let mut out = Vec::new();

    // Hand-written unit charts first: the umbrella depends on them by their
    // own name and version.
    let mut copied: BTreeMap<String, (String, String)> = BTreeMap::new();
    for application in &input.lock.applications {
        let sub = subchart_name(application);
        if let Some(chart) = super::unit_chart::copy(input, application, product, sub)? {
            copied.insert(sub.to_owned(), (chart.name, chart.version));
            out.extend(chart.files);
        }
    }

    out.push(umbrella_chart(input, product, version, &copied)?);
    out.extend(umbrella_values(input, files)?);
    out.push(umbrella_helpers(product)?);

    for application in &input.lock.applications {
        let sub = subchart_name(application);
        if copied.contains_key(sub) {
            continue;
        }
        out.push(subchart_chart(input, product, sub, version)?);
        out.extend(subchart_templates(input, application, files)?);
    }
    Ok(out)
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
struct DatabaseSecretValues {
    existing_secret: String,
    password_env: String,
}

fn database_secret(
    application: &ResolvedApplication,
    input: &GenerateInput<'_>,
) -> Result<Option<DatabaseSecretValues>, GenerateError> {
    if !application.gears.iter().any(|id| {
        input
            .lock
            .gears
            .get(id)
            .is_some_and(|gear| gear.runtime_caps.contains(&gearbox_ir::RuntimeCap::Db))
            && !input.lock.sqlite_databases.contains_key(id)
    }) {
        return Ok(None);
    }
    let db = input
        .lock
        .database
        .as_ref()
        .ok_or_else(|| GenerateError::Database("missing Postgres settings".to_owned()))?;
    db.validate().map_err(GenerateError::Database)?;
    Ok(Some(DatabaseSecretValues {
        existing_secret: db.existing_secret.clone().ok_or_else(|| {
            GenerateError::Database("Kubernetes postgres requires existing_secret".to_owned())
        })?,
        password_env: db
            .password_env()
            .ok_or_else(|| {
                GenerateError::Database(
                    "password must reference an environment variable".to_owned(),
                )
            })?
            .to_owned(),
    }))
}

/// Whether `application` links the tenant `AuthN` resolver.
fn hosts_authn(application: &ResolvedApplication) -> bool {
    application
        .gears
        .iter()
        .any(|gear| gear.as_str() == "authn-resolver")
}

/// Whether the process runs the directory, which is that process's memory: one
/// replica, and a rollout that stops it before starting the next -- a surge
/// would start a second, independent registry (ADR-0004 amendment).
fn hosts_directory(application: &ResolvedApplication) -> bool {
    application
        .gears
        .iter()
        .any(|gear| gear.as_str() == "service-discovery")
}

/// Requests and limits for a production workload; a development profile
/// leaves them to the operator, as before.
fn production_resources(input: &GenerateInput<'_>) -> Option<serde_json::Value> {
    (!input.lock.product.development).then(|| {
        serde_json::json!({
            "requests": {"cpu": "100m", "memory": "128Mi"},
            "limits": {"memory": "512Mi"},
        })
    })
}

fn uses_sqlite(application: &ResolvedApplication, input: &GenerateInput<'_>) -> bool {
    application
        .gears
        .iter()
        .any(|id| input.lock.sqlite_databases.contains_key(id))
}

fn directory_endpoint(input: &GenerateInput<'_>) -> Option<String> {
    let settings = input.lock.kubernetes.as_ref()?;
    let host = input.lock.applications.iter().find(|app| {
        app.gears
            .iter()
            .any(|id| id.as_str() == "service-discovery")
    })?;
    let grpc = host
        .listens
        .iter()
        .find(|endpoint| endpoint.gear.as_str() == "grpc-hub")?;
    // A unit chart's hub is behind the Service the chart names.
    if host.unit_chart.is_some()
        && let Some(uri) = &grpc.advertise_uri
    {
        return Some(uri.clone());
    }
    let port: u16 = grpc.address.rsplit(':').next()?.parse().ok()?;
    Some(format!(
        "http://{}.{}.svc.cluster.local:{port}",
        subchart_name(host),
        settings.namespace.as_deref().unwrap_or("default")
    ))
}

fn subchart_name(application: &ResolvedApplication) -> &str {
    application
        .subchart
        .as_deref()
        .unwrap_or_else(|| application.name.as_str())
}

fn umbrella_chart(
    input: &GenerateInput<'_>,
    product: &str,
    version: &str,
    unit_charts: &BTreeMap<String, (String, String)>,
) -> Result<FileEntry, GenerateError> {
    helm_safe("Chart.yaml name", product)?;
    helm_safe("Chart.yaml version", version)?;
    let mut dependencies = Vec::new();
    for application in &input.lock.applications {
        let sub = subchart_name(application);
        helm_safe("Chart.yaml dependency name", sub)?;
        // A unit chart keeps its own name and version; the process name is
        // its alias, which is also its values key.
        let (name, dep_version) = unit_charts
            .get(sub)
            .map_or((sub, version), |(n, v)| (n.as_str(), v.as_str()));
        helm_safe("Chart.yaml dependency name", name)?;
        helm_safe("Chart.yaml dependency version", dep_version)?;
        dependencies.push(context! { name => name, key => sub, version => dep_version });
    }
    let body = templates::render_helm(
        "helm/umbrella/Chart.yaml",
        input.templates.get("helm/umbrella/Chart.yaml")?,
        context! {
            header => header("#").trim_end(),
            product => product,
            version => version,
            dependencies => dependencies,
        },
    )?;
    Ok(FileEntry::text(
        paths::rel(&["helm", product, "Chart.yaml"])?,
        body,
        FileKind::Yaml,
        Ownership::Generated,
    ))
}

fn subchart_chart(
    input: &GenerateInput<'_>,
    product: &str,
    sub: &str,
    version: &str,
) -> Result<FileEntry, GenerateError> {
    helm_safe("subchart Chart.yaml name", sub)?;
    helm_safe("subchart Chart.yaml version", version)?;
    let body = templates::render_helm(
        "helm/Chart.yaml",
        input.templates.get("helm/Chart.yaml")?,
        context! { header => header("#").trim_end(), name => sub, version => version },
    )?;
    Ok(FileEntry::text(
        paths::rel(&["helm", product, "charts", sub, "Chart.yaml"])?,
        body,
        FileKind::Yaml,
        Ownership::Generated,
    ))
}

/// Umbrella values and the schema that constrains them.
///
/// `values.yaml` is [`Ownership::OperatorOwned`]: the first real consumer of
/// the three-way merge. Images and replica counts still start here so
/// `helm template` (which only auto-loads `values.yaml`) renders. A second
/// generate updates the image tag through merge3 while an operator's
/// `replicaCount` edit survives.
///
/// `values.generated.yaml` is the same bytes, always overwritten, so the
/// operator can diff what the lock decided from what they have kept.
/// Helm does not load it; passing `-f` is optional.
///
/// The merge base lives in `.gearbox/<product>/.base/`, shared across
/// profiles. Only Kubernetes emits this chart, so a `dev` generate cannot
/// clobber a `prod` operator file.
fn umbrella_values(
    input: &GenerateInput<'_>,
    files: &FileSet,
) -> Result<Vec<FileEntry>, GenerateError> {
    let mut blocks: BTreeMap<String, SubchartBlock> = BTreeMap::new();
    for application in &input.lock.applications {
        if application.unit_chart.is_some() {
            // The unit chart's own values: its image (registry folded into the
            // repository, as the chart reads it), and the description's
            // configuration of its gears, merged by the chart over its own
            // before its production rules are checked.
            let image = application.image.as_ref();
            // `${VAR}` placeholders in the overlay, from the existing Secret the
            // description names -- the same pairing a generated chart makes --
            // or else the product's own (`unit_secret`).
            let config_rel = paths::rel(&["config", &format!("{}.yaml", application.name)])?;
            let database = database_secret(application, input)?;
            let mut secret_env: Vec<UnitSecretEnv> = match unit_secret(application, input)? {
                Some(secret) => files
                    .get(&config_rel)
                    .and_then(FileEntry::as_text)
                    .map(secret_vars)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|name| database.as_ref().is_none_or(|db| &db.password_env != name))
                    .map(|name| UnitSecretEnv {
                        name: name.clone(),
                        secret_name: secret.clone(),
                        key: name,
                    })
                    .collect(),
                None => Vec::new(),
            };
            if let Some(db) = &database {
                secret_env.push(UnitSecretEnv {
                    name: db.password_env.clone(),
                    secret_name: db.existing_secret.clone(),
                    key: db.password_env.clone(),
                });
            }
            blocks.insert(
                subchart_name(application).to_owned(),
                SubchartBlock::Unit(UnitChartValues {
                    enabled: true,
                    replicas: application.replicas,
                    strategy: (application.replicas > 1).then_some("RollingUpdate"),
                    secret_env,
                    image: UnitImageValues {
                        repository: image.map_or_else(String::new, |i| {
                            i.registry.as_deref().map_or_else(
                                || i.repository.clone(),
                                |registry| format!("{registry}/{}", i.repository),
                            )
                        }),
                        tag: image.map_or_else(String::new, |i| i.tag.clone()),
                    },
                    config_overlay: super::config::unit_chart_overlay(input, application)?,
                    // A worker registers with the directory this product resolved.
                    directory_endpoint: application
                        .is_worker()
                        .then(|| directory_endpoint(input))
                        .flatten(),
                    authn: hosts_authn(application).then_some(UnitAuthnValues { mode: "product" }),
                }),
            );
            continue;
        }
        let image = image_values(application, input);
        let sqlite = uses_sqlite(application, input);
        let config_rel = paths::rel(&["config", &format!("{}.yaml", application.name)])?;
        let mut keys = files
            .get(&config_rel)
            .and_then(FileEntry::as_text)
            .map(secret_vars)
            .unwrap_or_default();
        let database = database_secret(application, input)?;
        if let Some(secret) = &database {
            keys.retain(|key| key != &secret.password_env);
        }
        let kube = kube_platform_token(input);
        let block = SubchartValues {
            database,
            enabled: true,
            replica_count: application.replicas,
            image,
            service_account: ServiceAccountValues {
                create: true,
                // The directory admits a registrar whose ServiceAccount is the
                // name it registers under (and whose namespace is allowed):
                // with Kube platform auth a worker's account is that name.
                name: (kube.is_some() && application.is_worker())
                    .then(|| application.registers_as().to_owned()),
                annotations: None,
            },
            name_override: None,
            fullname_override: None,
            pod_annotations: None,
            common_labels: None,
            common_annotations: None,
            pod_labels: None,
            node_selector: None,
            tolerations: None,
            affinity: None,
            resources: production_resources(input),
            extra_env: None,
            extra_volumes: kube.as_ref().map(|token| {
                serde_json::json!([{
                    "name": "platform-token",
                    "projected": {"sources": [{"serviceAccountToken": {
                        "path": token.file,
                        "audience": token.audience,
                        "expirationSeconds": 3600,
                    }}]},
                }])
            }),
            extra_volume_mounts: kube.as_ref().map(|token| {
                serde_json::json!([{
                    "name": "platform-token",
                    "mountPath": token.dir,
                    "readOnly": true,
                }])
            }),
            pod_security_context: Some(restricted_pod_security_context()),
            container_security_context: Some(restricted_container_security_context()),
            liveness_probe: ProbeValues::liveness(),
            readiness_probe: ProbeValues::readiness(),
            startup_probe: ProbeValues::startup(),
            service: ServiceValues {
                kind: "ClusterIP".to_owned(),
                annotations: None,
                session_affinity: None,
            },
            home_volume: HomeVolumeValues {
                kind: if sqlite {
                    HomeVolumeKind::PersistentVolumeClaim
                } else {
                    HomeVolumeKind::EmptyDir
                },
                size_limit: None,
                claim_name: None,
            },
            // A unit that validates Kube platform tokens calls TokenReview with
            // its own ServiceAccount token: mounted, and granted below.
            automount_service_account_token: kube.is_some(),
            token_review: kube.is_some(),
            strategy: (sqlite || hosts_directory(application))
                .then(|| serde_json::json!({"type": "Recreate"})),
            termination_grace_period_seconds: None,
            priority_class_name: None,
            topology_spread_constraints: None,
            revision_history_limit: None,
            init_containers: None,
            extra_containers: None,
            custom: BTreeMap::new(),
            existing_secret: existing_secret(application, input)?,
            secret_keys: if keys.is_empty() { None } else { Some(keys) },
        };
        blocks.insert(
            subchart_name(application).to_owned(),
            SubchartBlock::Generated(Box::new(block)),
        );
    }
    let values = UmbrellaValues {
        demo_ui_image: input
            .lock
            .ui
            .as_ref()
            .map(|_| format!("gbx-demo-ui:{}", input.lock.product.version)),
        // Emitted empty rather than omitted. `global` is where a house policy
        // puts the labels it requires on everything, and a key absent from the
        // file an operator edits is a key nobody finds. `imageRegistry` stays
        // unset on purpose: each image already carries the registry the profile
        // named, and repeating it here would dress an override up as a fact.
        global: Some(GlobalValues {
            image_registry: None,
            image_pull_secrets: None,
            common_labels: Some(BTreeMap::new()),
            common_annotations: Some(BTreeMap::new()),
        }),
        subcharts: blocks,
    };
    let yaml = serde_saphyr::to_string(&values).map_err(|source| GenerateError::Yaml {
        what: "helm values.yaml",
        source,
    })?;
    let product = input.lock.product.id.as_str();
    // Same values, different headers, and the headers are not decoration: one
    // file is overwritten without asking and the other is merged. Handing both
    // the `do not edit` banner would tell an operator not to use the only file
    // this design asks them to use.
    Ok(vec![
        FileEntry::text(
            paths::rel(&["helm", product, "values.yaml"])?,
            format!("{}\n{yaml}", operator_header("#", "values.generated.yaml")),
            FileKind::Yaml,
            Ownership::OperatorOwned,
        ),
        FileEntry::text(
            paths::rel(&["helm", product, "values.generated.yaml"])?,
            format!("{}\n{yaml}", header("#")),
            FileKind::Yaml,
            Ownership::Generated,
        ),
        values_schema(input)?,
    ])
}

/// JSON Schema for the umbrella values, with `additionalProperties: false`.
///
/// Top-level keys are the process/subchart names, which vary per product, so
/// the root object is assembled here rather than derived from one struct.
/// The per-subchart shape comes from [`schemars`] so the schema and the
/// values we serialize cannot drift by a field someone added to one side.
fn values_schema(input: &GenerateInput<'_>) -> Result<FileEntry, GenerateError> {
    let subchart = draft7_subchart_schema()?;
    let mut properties = serde_json::Map::new();
    properties.insert("global".to_owned(), global_schema()?);
    if input.lock.ui.is_some() {
        properties.insert(
            "demoUiImage".to_owned(),
            serde_json::json!({"type":"string", "minLength":1}),
        );
    }
    let mut required = Vec::new();
    for application in &input.lock.applications {
        let name = subchart_name(application);
        let schema = if application.unit_chart.is_some() {
            // A unit chart validates its values with its own schema.
            serde_json::json!({"type": "object"})
        } else if hosts_directory(application) && !uses_sqlite(application, input) {
            // The directory is process memory: one replica, never surged.
            serde_json::json!({"allOf": [subchart, {
                "type": "object",
                "required": ["strategy"],
                "properties": {
                    "replicaCount": {"const": 1},
                    "strategy": {"type": "object", "required": ["type"], "properties": {"type": {"const": "Recreate"}}}
                }
            }]})
        } else if uses_sqlite(application, input) {
            serde_json::json!({"allOf": [subchart, {
                "type": "object",
                "required": ["homeVolume", "strategy"],
                "properties": {
                    "replicaCount": {"const": 1},
                    "strategy": {"type": "object", "required": ["type"], "properties": {"type": {"const": "Recreate"}}},
                    "homeVolume": {
                        "type": "object", "required": ["type", "claimName"],
                        "properties": {"type": {"const": "persistentVolumeClaim"}, "claimName": {"type": "string", "minLength": 1}}
                    }
                }
            }]})
        } else {
            subchart.clone()
        };
        properties.insert(name.to_owned(), schema);
        required.push(serde_json::Value::String(name.to_owned()));
    }
    let schema = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$comment": format!(
            "GENERATED by gearbox {} -- do not edit. Run `gearbox generate` to regenerate.",
            env!("CARGO_PKG_VERSION")
        ),
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    });
    let body = serde_json::to_string_pretty(&schema).map_err(|source| GenerateError::Json {
        what: "values.schema.json",
        source,
    })?;
    Ok(FileEntry::text(
        paths::rel(&["helm", input.lock.product.id.as_str(), "values.schema.json"])?,
        format!("{body}\n"),
        FileKind::Json,
        Ownership::Generated,
    ))
}

/// Helm's validator is draft-07 (`definitions`, not `$defs`) and does not
/// follow `$ref` the way schemars 1 emits it. Inline the `$defs` so the
/// schema we hand Helm is a tree of `type`/`properties` only.
fn draft7_subchart_schema() -> Result<serde_json::Value, GenerateError> {
    let mut raw = draft7(
        serde_json::to_value(schemars::schema_for!(SubchartValues)),
        "the subchart values schema",
    )?;
    // Helm copies `global` onto every subchart's values. Refusing it would
    // make `helm template` fail on a chart that did not set global at all.
    if let Some(properties) = raw.get_mut("properties").and_then(|p| p.as_object_mut()) {
        properties.insert("global".to_owned(), serde_json::json!({ "type": "object" }));
    }
    Ok(raw)
}

/// The umbrella's `global` block, derived from the struct that writes it.
///
/// **Hand-written once, and it drifted the first time a field was added.** The
/// literal `json!` this replaced still described only `imageRegistry` and
/// `imagePullSecrets`, so `helm lint` rejected the very `commonLabels` the
/// generator had just written into `values.yaml` -- a chart refusing its own
/// output. Deriving it from [`GlobalValues`] makes that unrepresentable.
fn global_schema() -> Result<serde_json::Value, GenerateError> {
    draft7(
        serde_json::to_value(schemars::schema_for!(GlobalValues)),
        "the global values schema",
    )
}

/// schemars emits 2020-12; Helm's validator reads draft-07 and does not follow
/// `$ref`, so definitions are inlined and the dialect markers dropped.
fn draft7(
    schema: Result<serde_json::Value, serde_json::Error>,
    what: &'static str,
) -> Result<serde_json::Value, GenerateError> {
    let mut raw = schema.map_err(|source| GenerateError::Json { what, source })?;
    let defs = raw
        .as_object_mut()
        .and_then(|object| object.remove("$defs"))
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(object) = raw.as_object_mut() {
        object.remove("$schema");
        object.remove("title");
        object.remove("description");
    }
    inline_refs(&mut raw, &defs);
    Ok(raw)
}

fn inline_refs(value: &mut serde_json::Value, defs: &serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(reference)) = map.get("$ref")
                && let Some(name) = reference.strip_prefix("#/$defs/")
                && let Some(def) = defs.get(name)
            {
                *value = def.clone();
                inline_refs(value, defs);
                return;
            }
            for nested in map.values_mut() {
                inline_refs(nested, defs);
            }
        }
        serde_json::Value::Array(items) => {
            for nested in items {
                inline_refs(nested, defs);
            }
        }
        _ => {}
    }
}

/// Shared by the values files (serde) and `values.schema.json` (schemars),
/// so a field cannot appear in one and not the other.
///
/// Optional hatches are omitted from the default YAML so a first generate
/// stays small, but they stay in the schema so `--set` of an unknown key
/// still fails and a documented hatch still type-checks.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is one Helm value of the subchart, not a state machine"
)]
struct SubchartValues {
    #[serde(skip_serializing_if = "Option::is_none")]
    database: Option<DatabaseSecretValues>,
    enabled: bool,
    replica_count: u32,
    image: ImageValues,
    service_account: ServiceAccountValues,
    #[serde(skip_serializing_if = "Option::is_none")]
    name_override: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fullname_override: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    pod_annotations: Option<serde_json::Value>,
    /// Labels for this subchart's resources only.
    ///
    /// The narrower half of the pair: `global.commonLabels` covers the product,
    /// this covers one process. Both are merged on top of the standard
    /// `app.kubernetes.io/*` set rather than replacing it -- a policy label and a
    /// selector label are not competing for the same slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    common_labels: Option<BTreeMap<String, String>>,
    /// Annotations for this subchart's resources only.
    #[serde(skip_serializing_if = "Option::is_none")]
    common_annotations: Option<BTreeMap<String, String>>,
    /// Labels on the pod template alone, not on the objects around it.
    ///
    /// Distinct from `commonLabels` because service meshes and cost tooling
    /// select on pods. Never merged into `selectorLabels`: a Deployment's
    /// selector is immutable after creation, so a label that reached it would
    /// make the next `helm upgrade` fail rather than roll.
    #[serde(skip_serializing_if = "Option::is_none")]
    pod_labels: Option<BTreeMap<String, String>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    node_selector: Option<serde_json::Value>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    tolerations: Option<serde_json::Value>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    affinity: Option<serde_json::Value>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    resources: Option<serde_json::Value>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    extra_env: Option<serde_json::Value>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    extra_volumes: Option<serde_json::Value>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    extra_volume_mounts: Option<serde_json::Value>,
    /// The pod's security context, written out rather than hidden in a helper.
    ///
    /// **Emitted with the restricted defaults filled in, and that is the point.**
    /// It used to live in `_helpers.tpl` as literal template text, which made it
    /// unreachable: the only escape was a non-empty `.Values.podSecurityContext`,
    /// and an empty map is falsy in Helm, so there was no way to say "omit this".
    /// A cluster that assigns UIDs itself -- `OpenShift` under `restricted-v2`
    /// rejects a pod that names its own `runAsUser` -- could not run this chart at
    /// all without replacing the template.
    ///
    /// As a value it is merged, schema-described, and deletable. The default is
    /// unchanged, so a chart nobody edits is as locked down as it was.
    pod_security_context: Option<PodSecurityContext>,
    /// The container's security context. Same reasoning, worse starting point.
    ///
    /// This one had no `.Values` escape at all: the template called the helper
    /// unconditionally, so `readOnlyRootFilesystem` and the dropped capabilities
    /// were not adjustable by any means short of a replacement template.
    container_security_context: Option<ContainerSecurityContext>,
    /// Liveness probe timings. The route and port are not here on purpose.
    ///
    /// **The operator owns the timings; the lock owns the address.** A path in
    /// values is a path that can disagree with the configuration this same run
    /// generated -- the REST host's `prefix_path` moves `/healthz` to
    /// `/cf/healthz`, and a chart that let someone type the old one would fail
    /// readiness for a reason no diff explains.
    ///
    /// Timings were not adjustable at all before: a process slow to start in
    /// somebody else's cluster was restarted forever, and the only cure was
    /// replacing the template.
    liveness_probe: ProbeValues,
    /// Readiness probe timings.
    readiness_probe: ProbeValues,
    /// Startup probe, off by default.
    ///
    /// Off because a startup probe that exists suppresses liveness until it
    /// passes, and guessing a budget for someone else's slowest dependency is
    /// how a chart ships a hidden outage. Turning it on is one flag.
    startup_probe: ProbeValues,
    /// The Service this process is reached through.
    service: ServiceValues,
    /// The volume mounted at the runtime's `home_dir`.
    home_volume: HomeVolumeValues,
    /// Whether the pod gets a Kubernetes API token mounted.
    ///
    /// `false`, which is a hardening rather than an inconvenience: nothing in a
    /// generated topology talks to the API server. The runtime's resolver set is
    /// Directory, Null and Static -- the absence of a cluster-native one is what
    /// GBX0603 reports -- so the token would be an unused credential inside every
    /// pod, which is the kind of thing a policy scanner is right to flag.
    automount_service_account_token: bool,
    /// Grant this unit's `ServiceAccount` `tokenreviews` `create`: the product's
    /// platform auth is `kube`, and every generated unit may serve a platform
    /// operation it has to validate (gears-rust ADR-0011 §3).
    token_review: bool,
    /// Rollout strategy. Free-form: `RollingUpdate` percentages and `Recreate`
    /// have different shapes and Kubernetes owns both.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    strategy: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    termination_grace_period_seconds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority_class_name: Option<String>,
    /// Spread constraints, the standard way to survive a zone failure.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    topology_spread_constraints: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision_history_limit: Option<u32>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    init_containers: Option<serde_json::Value>,
    /// Sidecars the operator adds -- a log shipper, a proxy, a secrets agent.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "super::json::option"
    )]
    extra_containers: Option<serde_json::Value>,
    /// Values this generator makes no promises about.
    ///
    /// **The one open door in a deliberately closed schema, and it exists for
    /// the replaced template.** `additionalProperties: false` is what
    /// `cpt-gearbox-fr-values-schema` asks for, and it works: an operator who
    /// misspells `replicaCount` is told so. But it also refused every key a
    /// *house* template might read, so a site could override
    /// `helm/deployment.yaml` and then have nowhere to put the values that
    /// template needed -- a corporate chart with no corporate settings.
    ///
    /// Everything outside this key stays closed. Inside it nothing is checked,
    /// and that is the honest bargain: Gearbox does not know what a template it
    /// did not write is reading.
    #[serde(serialize_with = "super::json::btree_map")]
    custom: BTreeMap<String, serde_json::Value>,
    /// Name of a Secret the operator already created. Never a credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    existing_secret: Option<String>,
    /// Env names taken from `${VAR}` placeholders in this process's config.
    #[serde(skip_serializing_if = "Option::is_none")]
    secret_keys: Option<Vec<String>>,
}

#[derive(serde::Serialize)]
struct UmbrellaValues {
    #[serde(rename = "demoUiImage", skip_serializing_if = "Option::is_none")]
    demo_ui_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    global: Option<GlobalValues>,
    #[serde(flatten)]
    subcharts: BTreeMap<String, SubchartBlock>,
}

/// One process's block of the umbrella values: a generated subchart's, or a
/// hand-written unit chart's, which has its own shape and its own schema.
#[derive(serde::Serialize)]
#[serde(untagged)]
enum SubchartBlock {
    Generated(Box<SubchartValues>),
    Unit(UnitChartValues),
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UnitChartValues {
    enabled: bool,
    /// The application's `replicas`. The unit chart decides what it allows:
    /// Flight Control's schema holds the directory to one, cluster's refuses
    /// more than one on a standalone backend.
    replicas: u32,
    /// `RollingUpdate` once there is more than one replica: the charts default
    /// to `Recreate`, which is right for one process holding state and would
    /// take every replica down at once.
    #[serde(skip_serializing_if = "Option::is_none")]
    strategy: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    secret_env: Vec<UnitSecretEnv>,
    image: UnitImageValues,
    #[serde(serialize_with = "super::json::map")]
    config_overlay: serde_json::Map<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    directory_endpoint: Option<String>,
    /// A unit hosting `authn-resolver` (Flight Control's chart) authenticates
    /// tenants with the plugins the description selected, passed in
    /// `configOverlay`: `product` takes them as given, where the chart's own
    /// default would bring its OIDC plugin and refuse every other.
    #[serde(skip_serializing_if = "Option::is_none")]
    authn: Option<UnitAuthnValues>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UnitSecretEnv {
    name: String,
    secret_name: String,
    key: String,
}

#[derive(serde::Serialize)]
struct UnitAuthnValues {
    mode: &'static str,
}

#[derive(serde::Serialize)]
struct UnitImageValues {
    repository: String,
    tag: String,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
struct GlobalValues {
    #[serde(skip_serializing_if = "Option::is_none")]
    image_registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_pull_secrets: Option<Vec<String>>,
    /// Labels put on every resource of every subchart.
    ///
    /// A house policy that demands `cost-center` or `owner` on everything it
    /// deploys had no way to say so: the label set was literal text in
    /// `_helpers.tpl`, and adding one key meant replacing the whole helper. Here
    /// once, at the umbrella, because "every resource" is what the policy says.
    #[serde(skip_serializing_if = "Option::is_none")]
    common_labels: Option<BTreeMap<String, String>>,
    /// Annotations put on every resource of every subchart.
    #[serde(skip_serializing_if = "Option::is_none")]
    common_annotations: Option<BTreeMap<String, String>>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
struct ImageValues {
    /// The registry, kept out of `repository` so `global.imageRegistry` composes.
    ///
    /// A site that mirrors images sets `global.imageRegistry` once and expects
    /// every image to move. That works only if the per-image value it replaces is
    /// this field; with the registry folded into `repository`, the template
    /// produced `mirror.corp/registry.example.com/payments/gbx-audit` and the
    /// pull failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    registry: Option<String>,
    /// The image name, never carrying the registry.
    repository: String,
    tag: String,
    pull_policy: String,
}

/// The volume mounted at the runtime's `home_dir`.
///
/// `readOnlyRootFilesystem` makes this load-bearing rather than decorative: the
/// runtime calls `create_dir_all` on `server.home_dir`, and with the root
/// filesystem read-only that path has to be a mount.
///
/// **A discriminator rather than the Kubernetes volume-source shape, and the
/// reason is Helm's merge.** Values are merged, not replaced: an operator who
/// writes a `persistentVolumeClaim` source over a default `emptyDir` source gets
/// *both* keys, and Kubernetes rejects a volume with two sources. Measured on the
/// rendered manifest. Naming the choice in a field the operator overwrites makes
/// the wrong result unrepresentable instead of merely documented.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct HomeVolumeValues {
    #[serde(rename = "type")]
    kind: HomeVolumeKind,
    /// `emptyDir` only. A quantity such as `1Gi`.
    #[serde(skip_serializing_if = "Option::is_none")]
    size_limit: Option<String>,
    /// `persistentVolumeClaim` only, and required there.
    #[serde(skip_serializing_if = "Option::is_none")]
    claim_name: Option<String>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
enum HomeVolumeKind {
    EmptyDir,
    PersistentVolumeClaim,
}

/// The process Service.
///
/// The *name* is deliberately absent: the lock dials neighbours at
/// `http://{subchart}.{namespace}.svc.cluster.local`, so renaming the Service
/// would point every consumer at a DNS name that answers nothing. `type` and
/// `annotations` are safe to move because they change how the same name is
/// reached, not what it is.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ServiceValues {
    /// `ClusterIP` unless the operator says otherwise.
    ///
    /// There was no way to say otherwise: the template emitted no `type` line at
    /// all, so an internal load balancer -- the ordinary way to expose a gateway
    /// inside a corporate network -- was unreachable without a new template.
    #[serde(rename = "type")]
    kind: String,
    /// Annotations on the Service, where a cloud's load-balancer controller reads
    /// its configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_affinity: Option<String>,
}

/// One probe's schedule, without its address.
///
/// Kubernetes' own defaults, written out rather than left implicit, because a
/// value an operator cannot see is a value they cannot tune -- and tuning these
/// is the entire reason the block exists.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ProbeValues {
    enabled: bool,
    initial_delay_seconds: u32,
    period_seconds: u32,
    timeout_seconds: u32,
    failure_threshold: u32,
    /// Only meaningful for readiness; Kubernetes requires 1 for the other two.
    #[serde(skip_serializing_if = "Option::is_none")]
    success_threshold: Option<u32>,
}

impl ProbeValues {
    fn liveness() -> Self {
        Self {
            enabled: true,
            initial_delay_seconds: 0,
            period_seconds: 10,
            timeout_seconds: 1,
            failure_threshold: 3,
            success_threshold: None,
        }
    }

    fn readiness() -> Self {
        Self {
            success_threshold: Some(1),
            ..Self::liveness()
        }
    }

    /// Thirty failures at ten seconds: five minutes to start, then liveness takes
    /// over. A budget only someone who turns this on has any business choosing.
    fn startup() -> Self {
        Self {
            enabled: false,
            failure_threshold: 30,
            ..Self::liveness()
        }
    }
}

/// The pod's security context.
///
/// **No `deny_unknown_fields`, unlike its neighbours, and the asymmetry is the
/// point.** Kubernetes owns this schema, not Gearbox: an operator who needs
/// `runAsGroup`, `supplementalGroups`, `seLinuxOptions` or a field added in a
/// release after this one must be able to write it without waiting for us. The
/// closed set that `cpt-gearbox-fr-values-schema` asks for is the set of *our*
/// keys; inside a Kubernetes object the API server is the authority that was
/// going to check it anyway.
///
/// Typed rather than a free-form `serde_json::Value` because our YAML serializer
/// is pinned to the dialect `gears-rust` reads, and it renders a
/// `serde_json::Number` as the private newtype `serde_json` wraps it in --
/// `runAsUser: {"$serde_json::private::Number": "65532"}`. Measured, not feared.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct PodSecurityContext {
    run_as_non_root: bool,
    run_as_user: u32,
    fs_group: u32,
}

/// The container's security context. Open for the same reason.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the shape is Kubernetes's SecurityContext, and three of its fields are booleans; \
              grouping them into an enum would stop this serializing to the object the API expects"
)]
#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ContainerSecurityContext {
    run_as_non_root: bool,
    read_only_root_filesystem: bool,
    allow_privilege_escalation: bool,
    seccomp_profile: SeccompProfile,
    capabilities: Capabilities,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct SeccompProfile {
    /// Serialized as `type`, which is a Rust keyword.
    #[serde(rename = "type")]
    kind: String,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    /// `ALL`, in capitals, because the admission controller compares case-sensitively.
    ///
    /// The template this replaced wrote `all`. Pod Security Admission matches the
    /// dropped capability against the literal `ALL`, so a chart carrying the
    /// lowercase spelling is refused by the very `restricted` profile it was
    /// written to satisfy -- while looking, in a diff, exactly right.
    drop: Vec<String>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
struct ServiceAccountValues {
    create: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// Annotations on the `ServiceAccount`.
    ///
    /// **This is how every managed Kubernetes hands a pod its cloud identity** --
    /// IRSA on EKS, Workload Identity on GKE, the AAD label pair on AKS all read
    /// an annotation here. Without it the chart could not be used on any of the
    /// three without replacing the template, which made "no secrets in values"
    /// harder to honour rather than easier: the alternative to a workload
    /// identity is a static key in a Secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations: Option<BTreeMap<String, String>>,
}

fn umbrella_helpers(product: &str) -> Result<FileEntry, GenerateError> {
    Ok(FileEntry::text(
        paths::rel(&["helm", product, "templates", "_helpers.tpl"])?,
        format!(
            "{{{{/* GENERATED by gearbox {} -- do not edit. Workloads live in charts/. */}}}}\n",
            env!("CARGO_PKG_VERSION")
        ),
        FileKind::Text,
        Ownership::Generated,
    ))
}

fn subchart_templates(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
    files: &FileSet,
) -> Result<Vec<FileEntry>, GenerateError> {
    let product = input.lock.product.id.as_str();
    let name = subchart_name(application);
    let config_filename = format!("{}.yaml", application.name);
    let config_rel = paths::rel(&["config", &config_filename])?;
    let config_raw = files
        .get(&config_rel)
        .and_then(FileEntry::as_text)
        .ok_or_else(|| GenerateError::BadPath {
            path: config_rel.as_str().to_owned(),
        })?;

    let ports = named_ports(application);
    let http_port = application
        .service_port
        .or_else(|| ports.first().map(|p| p.port))
        .unwrap_or(80);
    let prefix = rest_prefix(input, application)?;
    let directory = matches!(application.kind, ApplicationKind::Worker)
        .then(|| directory_endpoint(input))
        .flatten();
    let liveness_path = probe_path(&prefix, "/healthz");
    helm_safe("liveness probe path", &liveness_path)?;
    let readiness_path = probe_path(&prefix, "/readyz");
    helm_safe("readiness probe path", &readiness_path)?;
    let cluster_service = application
        .gears
        .iter()
        .any(|g| g.as_str() == CLUSTER_SERVICE);
    // The Service keeps the port the SDK dials; the pod's hub listens wherever
    // the resolver put it, which is not 50051 when another process took that
    // first.
    let cluster_target_port = application
        .listens
        .iter()
        .find(|endpoint| endpoint.gear.as_str() == "grpc-hub")
        .and_then(|endpoint| port_of(&endpoint.address))
        .unwrap_or(CLUSTER_PORT);

    let common = context! {
        name => name,
        application => application.name.as_str(),
    };

    let helpers = templates::render_helm(
        "helm/helpers.tpl",
        input.templates.get("helm/helpers.tpl")?,
        common,
    )?;
    let deployment = templates::render_helm(
        "helm/deployment.yaml",
        input.templates.get("helm/deployment.yaml")?,
        context! {
            name => name,
            application => application.name.as_str(),
            config_filename => config_filename.as_str(),
            bin_name => application.bin_name.as_str(),
            http_port => http_port,
            liveness_path => liveness_path.as_str(),
            readiness_path => readiness_path.as_str(),
            home_dir => K8S_HOME_DIR,
            container_ports => &ports,
            directory_endpoint => directory,
        },
    )?;
    let service = templates::render_helm(
        "helm/service.yaml",
        input.templates.get("helm/service.yaml")?,
        context! {
            name => name,
            // The subchart's name, and it must stay that even for a role-bearing
            // application. A role's `directory_name` is what an instance
            // registers under in the *gears* directory; this is what Kubernetes
            // DNS resolves, and `resolve::partition::cluster_dns` builds every
            // static endpoint in the lock as
            // `http://{subchart}.{namespace}.svc.cluster.local`. Naming the
            // Service after the role would therefore point every consumer at a
            // name that answers nothing -- which `ServiceValues` above says in
            // so many words, and which is why the two are separate keys rather
            // than one.
            service_name => name,
            ports => &ports,
            cluster_service => cluster_service,
            cluster_port => CLUSTER_PORT,
            cluster_target_port => cluster_target_port,
        },
    )?;
    let configmap = templates::render_helm(
        "helm/configmap.yaml",
        input.templates.get("helm/configmap.yaml")?,
        context! {
            name => name,
            config_filename => config_filename.as_str(),
        },
    )?;
    let serviceaccount = templates::render_helm(
        "helm/serviceaccount.yaml",
        input.templates.get("helm/serviceaccount.yaml")?,
        context! { name => name },
    )?;

    let base = |file: &str| -> Result<_, GenerateError> {
        paths::rel(&["helm", product, "charts", name, "templates", file])
    };
    let files_rel = paths::rel(&["helm", product, "charts", name, "files", &config_filename])?;
    Ok(vec![
        FileEntry::text(
            files_rel,
            config_raw.to_owned(),
            FileKind::Yaml,
            Ownership::Generated,
        ),
        FileEntry::text(
            base("_helpers.tpl")?,
            helpers,
            FileKind::Text,
            Ownership::Generated,
        ),
        FileEntry::text(
            base("deployment.yaml")?,
            deployment,
            FileKind::Yaml,
            Ownership::Generated,
        ),
        FileEntry::text(
            base("service.yaml")?,
            service,
            FileKind::Yaml,
            Ownership::Generated,
        ),
        FileEntry::text(
            base("configmap.yaml")?,
            configmap,
            FileKind::Yaml,
            Ownership::Generated,
        ),
        FileEntry::text(
            base("serviceaccount.yaml")?,
            serviceaccount,
            FileKind::Yaml,
            Ownership::Generated,
        ),
    ])
}

fn helm_safe(at: &'static str, value: &str) -> Result<(), GenerateError> {
    if value.contains("{{") || value.contains("}}") || value.contains('\n') || value.contains('\r')
    {
        return Err(GenerateError::UnsafeHelm {
            at,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// Env names the runtime will try to expand out of this process's config.
///
/// Scanned from the generated YAML rather than from a field table, because
/// the postgres plugin, toolkit-db and static-credstore all share `${VAR}`
/// / `${VAR:-default}` and the `ConfigMap` is what the binary actually loads.
/// A placeholder the process will not expand is still harmless as env.
///
/// **Which fields are credentials is a catalogue fact.** `ConfigField.secret`
/// is projected from `secrecy::SecretString`. **Where the value comes from**
/// -- an environment variable, a Kubernetes Secret, a file -- is still a
/// deployment fact derived here from the placeholders generation wrote, which
/// is why this scan reads the YAML and not the field table.
fn secret_vars(config: &str) -> Vec<String> {
    let mut found = BTreeSet::new();
    let mut rest = config;
    while let Some(start) = rest.find("${") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find('}') else {
            break;
        };
        let inner = &rest[..end];
        let name = inner.split_once(':').map_or(inner, |(name, _)| name);
        if is_env_name(name) {
            found.insert(name.to_owned());
        }
        rest = &rest[end + 1..];
    }
    found.into_iter().collect()
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() || c == '_' => {
            chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        }
        _ => false,
    }
}

/// `secret_ref = "existingSecret:<name>"` is the lock spelling of a Secret
/// the operator already created. Any other shape stays in config only:
/// a vault path is not a Kubernetes object name.
fn existing_secret(
    application: &ResolvedApplication,
    input: &GenerateInput<'_>,
) -> Result<Option<String>, GenerateError> {
    // The process that runs `cluster` connects to every scope's backend, so it
    // needs the credentials of every scope, requesters there or not (a cluster
    // placed in its own application has none).
    let hosts_cluster = application
        .gears
        .iter()
        .any(|gear| gear.as_str() == CLUSTER_SERVICE);
    let Some(name) = input.lock.cluster.iter().find_map(|binding| {
        if !hosts_cluster
            && !binding
                .requesters
                .iter()
                .any(|gear| application.contains(gear))
        {
            return None;
        }
        binding
            .secret_ref
            .as_deref()?
            .strip_prefix("existingSecret:")
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }) else {
        return Ok(None);
    };
    helm_safe("existingSecret", &name)?;
    Ok(Some(name))
}

/// The Secret a unit chart's `${VAR}` placeholders come from: the one
/// [`existing_secret`] finds, **otherwise the product's own**, the one its
/// Postgres names (`postgres(existing_secret = ...)`).
///
/// Only for a unit chart. Its overlay is this generator's output and
/// `secretEnv` is the one way a value reaches it, so without a source a Flight
/// Control unit's `AuthN` tokens, say, reached the pod unfilled. A generated
/// chart keeps its opt-in: `existingSecret` in the operator's values, beside
/// the `extraEnv` an installation may supply instead.
fn unit_secret(
    application: &ResolvedApplication,
    input: &GenerateInput<'_>,
) -> Result<Option<String>, GenerateError> {
    if let Some(name) = existing_secret(application, input)? {
        return Ok(Some(name));
    }
    let Some(name) = input
        .lock
        .database
        .as_ref()
        .and_then(|db| db.existing_secret.clone())
        .filter(|name| !name.is_empty())
    else {
        return Ok(None);
    };
    helm_safe("existingSecret", &name)?;
    Ok(Some(name))
}

/// The product's Kube platform token, when its platform auth is `kube`: the
/// projected token every generated unit presents and the audience it is minted
/// for, from the gateway's `internal_auth` -- the one platform-plane setting a
/// description makes (`generate::config` copies it to the hubs and workers).
struct KubeToken {
    audience: String,
    dir: String,
    file: String,
}

fn kube_platform_token(input: &GenerateInput<'_>) -> Option<KubeToken> {
    let auth = input
        .lock
        .gears
        .iter()
        .find(|(id, _)| id.as_str() == "api-gateway")?
        .1
        .config
        .get("internal_auth")?;
    if auth.get("provider")?.as_str()? != "kube" {
        return None;
    }
    let audience = auth
        .get("audiences")
        .and_then(|a| a.get(0))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("toolkit-internal")
        .to_owned();
    let path = auth
        .get("token_path")
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || format!("/var/run/secrets/tokens/{audience}"),
            str::to_owned,
        );
    let (dir, file) = path.rsplit_once('/')?;
    Some(KubeToken {
        audience,
        dir: dir.to_owned(),
        file: file.to_owned(),
    })
}

/// The three parts of the image, taken from the lock rather than parsed back out.
///
/// The lock holds them apart ([`gearbox_ir::ImageRef`]) precisely so this
/// function does not have to guess where a registry ends and a repository begins
/// -- a guess that has no correct form, since a registry may carry a port and a
/// repository may carry slashes.
///
/// A profile that builds no images leaves `image` unset. Naming the binary and
/// the product version is the same answer `build.sh` gives there, so a chart
/// generated for such a profile is at least self-consistent.
fn image_values(application: &ResolvedApplication, input: &GenerateInput<'_>) -> ImageValues {
    let (registry, repository, tag) = application.image.as_ref().map_or_else(
        || {
            (
                None,
                application.bin_name.clone(),
                input.lock.product.version.clone(),
            )
        },
        |image| {
            (
                image.registry.clone(),
                image.repository.clone(),
                image.tag.clone(),
            )
        },
    );
    ImageValues {
        registry,
        repository,
        tag,
        pull_policy: "IfNotPresent".to_owned(),
    }
}

fn named_ports(application: &ResolvedApplication) -> Vec<NamedPort> {
    let mut ports = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for endpoint in &application.listens {
        if let Some(port) = port_of(&endpoint.address)
            && seen.insert(port)
        {
            ports.push(NamedPort {
                name: port_name(&endpoint.name),
                port,
            });
        }
    }
    if let Some(serve) = &application.serve
        && let Some(port) = port_of(&serve.listen_addr)
        && seen.insert(port)
    {
        ports.push(NamedPort {
            name: "http".to_owned(),
            port,
        });
    }
    ports
}

fn port_of(address: &str) -> Option<u16> {
    address.rsplit(':').next()?.parse().ok()
}

fn port_name(raw: &str) -> String {
    let name: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if name.is_empty() {
        "http".to_owned()
    } else {
        name
    }
}

fn rest_prefix(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
) -> Result<String, GenerateError> {
    if !matches!(application.kind, ApplicationKind::Host) {
        return Ok(String::new());
    }
    let Some(id) = &application.rest_host else {
        return Ok(String::new());
    };
    let Some(gear) = input.lock.gears.get(id) else {
        return Ok(String::new());
    };
    let Some(serde_json::Value::String(raw)) = gear.config.get("prefix_path") else {
        return Ok(String::new());
    };
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    helm_safe("prefix_path", trimmed)?;
    Ok(format!("/{trimmed}"))
}

/// The restricted Pod Security Standard, as data.
fn restricted_pod_security_context() -> PodSecurityContext {
    PodSecurityContext {
        run_as_non_root: true,
        run_as_user: NONROOT_UID,
        fs_group: NONROOT_UID,
    }
}

/// The container half of the same standard.
fn restricted_container_security_context() -> ContainerSecurityContext {
    ContainerSecurityContext {
        run_as_non_root: true,
        read_only_root_filesystem: true,
        allow_privilege_escalation: false,
        seccomp_profile: SeccompProfile {
            kind: "RuntimeDefault".to_owned(),
        },
        capabilities: Capabilities {
            drop: vec!["ALL".to_owned()],
        },
    }
}

fn probe_path(prefix: &str, route: &str) -> String {
    if prefix.is_empty() {
        route.to_owned()
    } else {
        format!("{prefix}{route}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helm_delimiters_leave_mustache_intact() {
        let source = templates::TemplateSet::builtin("helm/helpers.tpl").expect("builtin");
        let rendered = templates::render_helm(
            "helm/helpers.tpl",
            source,
            context! { name => "api-gateway" },
        )
        .expect("render");
        assert!(
            rendered.contains("{{ include \"api-gateway.chart\" . }}"),
            "{rendered}"
        );
        assert!(
            rendered.contains(r#"define "api-gateway.serviceAccountName""#),
            "`<< name >>` must be substituted throughout, not only once:\n{rendered}"
        );
        assert!(
            !rendered.contains("<<"),
            "`<<` survived into the Helm template:\n{rendered}"
        );
        assert!(
            rendered.contains("{{"),
            "Helm's `{{ }}` did not survive:\n{rendered}"
        );
    }

    /// A registry with a port survives, which is what parsing could never promise.
    ///
    /// `localhost:5000/gbx-audit:0.1.0` has two colons and only the second is a
    /// tag separator. The parser this replaced answered that case by giving up --
    /// it returned the whole string as a repository and invented the tag `latest`
    /// -- and no rule over the string alone does better, because `host:port` and
    /// `repo:tag` are the same shape. The lock never joined them, so nothing here
    /// has to take them apart.
    #[test]
    fn a_registry_with_a_port_stays_whole() {
        let image = gearbox_ir::ImageRef {
            registry: Some("localhost:5000".to_owned()),
            repository: "gbx-audit".to_owned(),
            tag: "0.1.0".to_owned(),
        };
        assert_eq!(image.reference(), "localhost:5000/gbx-audit:0.1.0");
        assert_eq!(image.registry.as_deref(), Some("localhost:5000"));
        assert_eq!(image.repository, "gbx-audit");
    }

    #[test]
    fn secret_vars_reads_plain_and_defaulted_placeholders() {
        let config = "connection_string: postgres://u:${PG_PASSWORD}@${PG_HOST:-db}/payments\n\
                      token: ${not_an_env}\n\
                      other: ${i}\n";
        assert_eq!(
            secret_vars(config),
            vec!["PG_HOST".to_owned(), "PG_PASSWORD".to_owned()]
        );
    }
}
