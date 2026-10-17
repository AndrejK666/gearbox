//! The `product.gdl` vocabulary.
//!
//! A product description states operator intent. The shape that matters most is
//! that **every deployment profile is declared as data**, and the one to resolve
//! is chosen at resolve time (`gearbox resolve --profile <id>`). That is why
//! `bind`, `cluster_profile` and `application` each carry a `profiles = [...]` list
//! instead of the file carrying an `if`: GDL has no `if`, by design, and a
//! description that could branch on the profile would be a program whose output
//! depends on how it was invoked.
//!
//! One function here takes `**kwargs`, and only one: `provider(...)`. A cluster
//! plugin's options are genuinely open -- the SDK hands a plugin a raw JSON map
//! and keeps the option schema out of the framework -- so there is no arity to
//! check. Everywhere else the parameters are spelled out, so starlark's own
//! arity and type checking does the work and an unknown argument is GBX0106
//! rather than a silently ignored key.

// These fire on code `#[starlark_module]` generates, not on anything written
// here. Same reasoning as `globals.rs`: `allow` rather than `expect`, because
// which of them fires depends on the expansion and an unfulfilled `expect` is
// itself an error.
#![allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    reason = "artifacts of #[starlark_module] expansion, not of hand-written code"
)]

use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;
use starlark::values::{UnpackValue, Value};

use crate::declarative::call_location;
use crate::records::{
    ApplicationRecord, BindRecord, ClusterProfileRecord, PluginRecord, PreferenceRecord,
    ProfileRecord, ProviderBindingRecord, SourceAtRecord, SourceRecord, UseGearRecord,
};
use crate::sink::{GdlSink, ProductDecl};
use crate::vocabulary;

/// Pull the sink out of the evaluator.
fn sink<'a>(eval: &'a Evaluator<'_, '_, '_>) -> anyhow::Result<&'a GdlSink> {
    eval.extra
        .and_then(|e| e.downcast_ref::<GdlSink>())
        .ok_or_else(|| anyhow::anyhow!("internal error: no GdlSink installed on the evaluator"))
}

/// How deep a plugin option may nest before it is refused.
///
/// A guard against recursion, not a modelling decision: `x = []; x.append(x)`
/// builds a cyclic value the host will happily hand us, and a bare recursive
/// walk over one overflows the native stack -- which aborts the process instead
/// of producing a diagnostic. Real option maps are two or three deep, so the
/// limit costs nothing.
const MAX_OPTION_DEPTH: usize = 32;

/// Convert a Starlark value to JSON for a plugin's option map.
///
/// Deliberately narrow. A cluster option is a scalar, or a list or map of them;
/// anything else -- a function, a record, a `None` -- is a mistake worth naming
/// rather than encoding as `null` and letting the plugin reject it at startup.
fn to_json(key: &str, value: Value<'_>) -> anyhow::Result<serde_json::Value> {
    to_json_at(key, value, 0)
}

fn to_json_at(key: &str, value: Value<'_>, depth: usize) -> anyhow::Result<serde_json::Value> {
    if depth > MAX_OPTION_DEPTH {
        return Err(anyhow::anyhow!(
            "option `{key}` nests more than {MAX_OPTION_DEPTH} levels deep, or contains itself. \
             Cluster options are passed to the plugin as JSON, which has neither."
        ));
    }
    if let Some(s) = value.unpack_str() {
        return Ok(serde_json::Value::String(s.to_owned()));
    }
    if let Some(b) = value.unpack_bool() {
        return Ok(serde_json::Value::Bool(b));
    }
    // `i64`, not `i32`: a port or a byte limit past two billion is ordinary, and
    // the narrow unpack made such a value a description that parsed and then
    // refused to evaluate, with a message naming neither the range nor the field.
    if let Some(i) = i64::unpack_value(value).ok().flatten() {
        return Ok(serde_json::Value::Number(i.into()));
    }
    // Floats were absent entirely, so `1.5` was reported as "not a string,
    // integer, bool, list or map" -- a list that mentions float only by omission.
    if let Some(f) = starlark::values::float::StarlarkFloat::unpack_value(value)
        .ok()
        .flatten()
    {
        let number = serde_json::Number::from_f64(f.0).ok_or_else(|| {
            anyhow::anyhow!("option `{key}` is `{}`, which JSON cannot represent", f.0)
        })?;
        return Ok(serde_json::Value::Number(number));
    }
    if let Some(list) = starlark::values::list::ListRef::from_value(value) {
        let items = list
            .iter()
            .map(|item| to_json_at(key, item, depth + 1))
            .collect::<anyhow::Result<Vec<_>>>()?;
        return Ok(serde_json::Value::Array(items));
    }
    if let Some(dict) = starlark::values::dict::DictRef::from_value(value) {
        let mut map = serde_json::Map::new();
        for (k, v) in dict.iter() {
            let name = k.unpack_str().ok_or_else(|| {
                anyhow::anyhow!("option `{key}`: map keys must be strings, got `{k}`")
            })?;
            map.insert(name.to_owned(), to_json_at(name, v, depth + 1)?);
        }
        return Ok(serde_json::Value::Object(map));
    }
    Err(anyhow::anyhow!(
        "option `{key}` has value `{value}`, which is not a string, number, bool, \
         list or map. Cluster options are passed to the plugin as JSON."
    ))
}

/// Sort kwargs into a stable order.
///
/// `SmallMap` preserves insertion order, which is the order the author happened
/// to type. The catalogue and the lock must not depend on that.
fn options(
    kwargs: SmallMap<String, Value<'_>>,
) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
    let mut out = kwargs
        .into_iter()
        .map(|(k, v)| to_json(&k, v).map(|json| (k, json)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Convert an optional Starlark map into sorted JSON pairs.
///
/// Shared by `use_gear(config = ...)` and `plugin(config = ...)`: a plugin's
/// configuration is a gear's configuration, so it needs no separate machinery.
fn config_map(
    what: &str,
    value: Option<Value<'_>>,
) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    match to_json(what, value)? {
        serde_json::Value::Object(map) => Ok(map.into_iter().collect()),
        other => Err(anyhow::anyhow!("`{what}` must be a map, got `{other}`")),
    }
}

fn strings(list: Option<UnpackList<String>>) -> Vec<String> {
    list.map(|l| l.items).unwrap_or_default()
}

#[starlark_module]
fn gdl_product_vocabulary(builder: &mut GlobalsBuilder) {
    /// `registry("crates.io", prefix = "cf-gears-")`
    ///
    /// **The source is the registry, not one package.** A product naming six
    /// gears from it fetches six packages -- and everything they depend on --
    /// through one declaration, because a gear's co-location dependencies are
    /// real Cargo dependencies and cargo brings the closure with them.
    ///
    /// `prefix` turns a gear id into a package name: `api-gateway` becomes
    /// `cf-gears-api-gateway`. A convention rather than magic, and one with an
    /// exit -- `use_gear(package = "...")` overrides it for a gear that does not
    /// follow the house naming. A wrong guess is not silent either way: the
    /// fetched crate's own `gear.gdl` declares its `crate_name`, and GBX0209
    /// checks that against the real `Cargo.toml`.
    ///
    /// # Arguments
    /// * `prefix`: What a gear id is prefixed with to name its package, e.g. `cf-gears-`.
    fn registry(
        #[starlark(require = pos)] url: &str,
        #[starlark(require = named)] prefix: Option<&str>,
    ) -> anyhow::Result<SourceAtRecord> {
        Ok(SourceAtRecord {
            kind: "registry".to_owned(),
            at: None,
            url: Some(url.to_owned()),
            tag: None,
            rev: None,
            branch: None,
            prefix: prefix.map(str::to_owned),
            subdir: None,
        })
    }

    /// `source(id = ..., at = path(...) | git(...), crates = registry(...)?)`
    ///
    /// `crates` says the gears in a checkout are also published: descriptions
    /// come from `at`, crates from the registry, at the versions `at` declares.
    ///
    /// # Arguments
    /// * `id`: The source's id, kebab-case. `use_gear(source = ...)` names it.
    /// * `at`: Where it is: `path(...)`, `git(...)` with a pin, or `registry(...)`.
    /// * `crates`: For a `path(...)` source: the registry its crates are published to, as
    ///   `registry(...)`.
    fn source<'v>(
        #[starlark(require = named)] id: &str,
        #[starlark(require = named)] at: &'v SourceAtRecord,
        #[starlark(require = named)] crates: Option<&'v SourceAtRecord>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<SourceRecord> {
        if at.kind == "git" {
            let url = at.url.as_deref().unwrap_or_default();
            // `git()` leaves the pin optional because a gear's `source` is an
            // address, not a lock. A product's source is what a lock is built
            // from, so it has to pin something.
            if at.tag.is_none() && at.rev.is_none() && at.branch.is_none() {
                return Err(anyhow::anyhow!(
                    "git(url = \"{url}\") pins nothing; give one of `tag`, `rev` or `branch`"
                ));
            }
            if let Some(subdir) = &at.subdir {
                return Err(anyhow::anyhow!(
                    "source(id = \"{id}\", at = git(..., path = \"{subdir}\")): a product's \
                     source is a whole repository; `path` on `git(...)` places a gear inside one"
                ));
            }
        }
        Ok(SourceRecord {
            id: id.to_owned(),
            at: at.clone(),
            crates: crates.cloned(),
            declared_at: call_location(eval),
        })
    }

    /// `embedded(id = ...)` -- one process; every binding is local by construction.
    ///
    /// # Arguments
    /// * `id`: The profile's id, kebab-case.
    fn embedded<'v>(
        #[starlark(require = named)] id: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ProfileRecord> {
        Ok(ProfileRecord {
            kind: "embedded".to_owned(),
            id: id.to_owned(),
            host: None,
            discovery: None,
            target_dir: None,
            cargo_profile: None,
            namespace: None,
            image_registry: None,
            declared_at: call_location(eval),
        })
    }

    /// `self_hosted(id = ..., host = ..., worker_discovery = ..., target_dir = ..., cargo_profile = ...)`
    ///
    /// # Arguments
    /// * `id`: The profile's id, kebab-case.
    /// * `host`: The application that hosts the workers.
    /// * `worker_discovery`: `"static"` or `"directory"`.
    /// * `target_dir`: The Cargo target directory the host starts workers from, relative to this
    ///   file.
    /// * `cargo_profile`: The Cargo profile: `dev`, `release` or a custom one.
    fn self_hosted<'v>(
        #[starlark(require = named)] id: &str,
        #[starlark(require = named)] host: &str,
        #[starlark(require = named)] worker_discovery: &str,
        #[starlark(require = named)] target_dir: Option<&str>,
        #[starlark(require = named)] cargo_profile: Option<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ProfileRecord> {
        Ok(ProfileRecord {
            kind: "self-hosted".to_owned(),
            id: id.to_owned(),
            host: Some(host.to_owned()),
            discovery: Some(worker_discovery.to_owned()),
            target_dir: target_dir.map(str::to_owned),
            cargo_profile: cargo_profile.map(str::to_owned),
            namespace: None,
            image_registry: None,
            declared_at: call_location(eval),
        })
    }

    /// `kubernetes(id = ..., discovery = ..., namespace = ..., image_registry = ...)`
    ///
    /// # Arguments
    /// * `id`: The profile's id, kebab-case.
    /// * `discovery`: `"static"` or `"directory"`.
    /// * `namespace`: The namespace the chart deploys to.
    /// * `image_registry`: The registry images are pulled from.
    fn kubernetes<'v>(
        #[starlark(require = named)] id: &str,
        #[starlark(require = named)] discovery: &str,
        #[starlark(require = named)] namespace: Option<&str>,
        #[starlark(require = named)] image_registry: Option<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ProfileRecord> {
        Ok(ProfileRecord {
            kind: "kubernetes".to_owned(),
            id: id.to_owned(),
            host: None,
            discovery: Some(discovery.to_owned()),
            target_dir: None,
            cargo_profile: None,
            namespace: namespace.map(str::to_owned),
            image_registry: image_registry.map(str::to_owned),
            declared_at: call_location(eval),
        })
    }

    /// `plugin("name", config = {...}, profiles = [...])`
    ///
    /// Names an implementing gear. Which extension point it implements comes from the
    /// catalogue, so there is no `interface` here to get wrong.
    ///
    /// # Arguments
    /// * `config`: The plugin's configuration, as a map.
    /// * `profiles`: The profiles it applies to. Empty means all.
    fn plugin<'v>(
        #[starlark(require = pos)] gear: &str,
        #[starlark(require = named)] config: Option<Value<'v>>,
        #[starlark(require = named)] profiles: Option<UnpackList<String>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<PluginRecord> {
        Ok(PluginRecord {
            gear: gear.to_owned(),
            config: config_map("config", config)?,
            profiles: strings(profiles),
            declared_at: call_location(eval),
        })
    }

    /// `use_gear("name", source = ..., features = [...], config = {...}, plugins = [...])`
    ///
    /// # Arguments
    /// * `source`: The id of the `source(...)` the gear comes from.
    /// * `version`: The package version. For a registry source only.
    /// * `package`: The package name, when it is not the registry's prefix plus the gear id. For a
    ///   registry source only.
    /// * `features`: The gear's Cargo features to turn on.
    /// * `config`: The gear's configuration, as a map. Keys and types are checked against the
    ///   gear's `config_schema`.
    /// * `plugins`: The plugins for the gear's extension points, as `plugin(...)`.
    fn use_gear<'v>(
        #[starlark(require = pos)] gear: &str,
        #[starlark(require = named)] source: &str,
        #[starlark(require = named)] version: Option<&str>,
        #[starlark(require = named)] package: Option<&str>,
        #[starlark(require = named)] features: Option<UnpackList<String>>,
        #[starlark(require = named)] config: Option<Value<'v>>,
        #[starlark(require = named)] plugins: Option<UnpackList<&'v PluginRecord>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<UseGearRecord> {
        Ok(UseGearRecord {
            gear: gear.to_owned(),
            source: source.to_owned(),
            version: version.map(str::to_owned),
            package: package.map(str::to_owned),
            features: strings(features),
            config: config_map("config", config)?,
            plugins: plugins
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
            declared_at: call_location(eval),
        })
    }

    /// `bind(consumer = ..., contract = ..., mode = ..., transport = ..., ...)`
    ///
    /// # Arguments
    /// * `consumer`: The consuming gear's id.
    /// * `contract`: The contract, e.g. `api-contracts/PaymentApi@v1`.
    /// * `mode`: How to bind, as a `binding_mode.*` member.
    /// * `transport`: The transport, as a `transport.*` member.
    /// * `endpoint`: A fixed address, used instead of what discovery finds.
    /// * `profiles`: The profiles it applies to. Empty means all.
    fn bind<'v>(
        #[starlark(require = named)] consumer: &str,
        #[starlark(require = named)] contract: &str,
        #[starlark(require = named)] mode: &'v crate::values::GdlEnum,
        #[starlark(require = named)] transport: Option<&'v crate::values::GdlEnum>,
        #[starlark(require = named)] endpoint: Option<&str>,
        #[starlark(require = named)] profiles: Option<UnpackList<String>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<BindRecord> {
        // Validated here rather than in the engine so a member of the wrong
        // namespace is reported at the call that wrote it. The variant name is
        // what gets stored: the record holds owned, allocation-measurable data
        // only, and re-parsing it in the engine goes through the IR's own parser,
        // so the two cannot disagree.
        vocabulary::binding_mode(mode).map_err(|e| anyhow::anyhow!(e))?;
        if let Some(t) = transport {
            vocabulary::transport(t).map_err(|e| anyhow::anyhow!(e))?;
        }
        Ok(BindRecord {
            consumer: consumer.to_owned(),
            contract: contract.to_owned(),
            mode: mode.variant.to_owned(),
            transport: transport.map(|t| t.variant.to_owned()),
            endpoint: endpoint.map(str::to_owned),
            profiles: strings(profiles),
            declared_at: call_location(eval),
        })
    }

    /// `provider("name", secret_ref = ..., **options)`
    ///
    /// References a provider the catalogue already knows -- whether it exists is
    /// checked against the projected catalogue (GBX0505), not here.
    ///
    /// # Arguments
    /// * `secret_ref`: Where the credential comes from. It reaches the generated configuration,
    ///   never the lock.
    fn provider<'v>(
        #[starlark(require = pos)] name: &str,
        #[starlark(require = named)] secret_ref: Option<&str>,
        #[starlark(kwargs)] options: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ProviderBindingRecord> {
        Ok(ProviderBindingRecord {
            provider: name.to_owned(),
            options: self::options(options)?,
            secret_ref: secret_ref.map(str::to_owned),
            declared_at: call_location(eval),
        })
    }

    /// `cluster_profile(name = ..., cache = provider(...), ...)`
    ///
    /// `cache` is mandatory: it is the anchor the SDK's compare-and-swap defaults
    /// for leader election and lock are layered over, so a scope without one has
    /// nothing to fall back to.
    ///
    /// # Arguments
    /// * `name`: The profile name, as `impl ClusterProfile { const NAME }` spells it.
    /// * `cache`: The cache provider, as `provider(...)`. Required.
    /// * `leader_election`: The leader election provider. The default is the SDK's compare-and-swap
    ///   over the cache.
    /// * `lock`: The lock provider. The default is the SDK's compare-and-swap over the cache.
    /// * `profiles`: The profiles it applies to. Empty means all.
    fn cluster_profile<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] cache: &'v ProviderBindingRecord,
        #[starlark(require = named)] leader_election: Option<&'v ProviderBindingRecord>,
        #[starlark(require = named)] lock: Option<&'v ProviderBindingRecord>,
        #[starlark(require = named)] profiles: Option<UnpackList<String>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ClusterProfileRecord> {
        Ok(ClusterProfileRecord {
            scope: name.to_owned(),
            cache: cache.clone(),
            leader_election: leader_election.cloned(),
            lock: lock.cloned(),
            profiles: strings(profiles),
            declared_at: call_location(eval),
        })
    }

    /// `application("name", anchor = ..., replicas = ..., profiles = [...])`
    ///
    /// # Arguments
    /// * `anchor`: The gear the application is built around.
    /// * `role`: One of the anchor gear's `role(...)` names. The default is its default shape.
    /// * `replicas`: How many instances run, 1 or more. The default is 1.
    /// * `profiles`: The profiles it applies to. Empty means all.
    fn application<'v>(
        #[starlark(require = pos)] name: &str,
        #[starlark(require = named)] anchor: &str,
        #[starlark(require = named)] role: Option<&str>,
        #[starlark(require = named, default = 1)] replicas: u32,
        #[starlark(require = named)] profiles: Option<UnpackList<String>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ApplicationRecord> {
        if replicas == 0 {
            return Err(anyhow::anyhow!(
                "application(\"{name}\", replicas = 0) asks for an application that does not run"
            ));
        }
        Ok(ApplicationRecord {
            name: name.to_owned(),
            anchor: anchor.to_owned(),
            role: role.map(str::to_owned),
            replicas,
            profiles: strings(profiles),
            declared_at: call_location(eval),
        })
    }

    /// `product(...)` -- the single top-level declaration of a `product.gdl`.
    ///
    /// # Arguments
    /// * `gdl_version`: The GDL this file is written in, `"MAJOR.MINOR"`. The default is `"1.0"`.
    /// * `id`: The product's id, kebab-case.
    /// * `name`: The display name. The default is the id.
    /// * `version`: The product's version.
    /// * `sources`: Where gears come from, as `source(...)`.
    /// * `templates`: The template overlay, as `path(...)`. The default is `templates/` beside this
    ///   file.
    /// * `layout`: The directory generated applications go under. The default is `apps`.
    /// * `profiles`: The deployment profiles: `embedded(...)`, `self_hosted(...)`,
    ///   `kubernetes(...)`. At least one.
    /// * `default_profile`: The id of the profile used when none is chosen.
    /// * `gears`: The gears in the product, as `use_gear(...)`.
    /// * `bindings`: How consumers reach providers, as `bind(...)`.
    /// * `cluster_profiles`: The cluster backends, as `cluster_profile(...)`.
    /// * `applications`: The applications, as `application(...)`.
    /// * `preferences`: What resolution prefers, as `prefer.*()`.
    fn product<'v>(
        // `"MAJOR.MINOR"`; read before evaluation by `version.rs`, accepted
        // here so that writing it is not an unknown argument.
        #[starlark(require = named)] gdl_version: Option<&str>,
        #[starlark(require = named)] id: &str,
        #[starlark(require = named)] name: Option<&str>,
        #[starlark(require = named)] version: &str,
        #[starlark(require = named)] sources: UnpackList<&'v SourceRecord>,
        #[starlark(require = named)] templates: Option<&'v SourceAtRecord>,
        #[starlark(require = named)] layout: Option<&str>,
        #[starlark(require = named)] profiles: UnpackList<&'v ProfileRecord>,
        #[starlark(require = named)] default_profile: &str,
        #[starlark(require = named)] gears: UnpackList<&'v UseGearRecord>,
        #[starlark(require = named)] bindings: Option<UnpackList<&'v BindRecord>>,
        #[starlark(require = named)] cluster_profiles: Option<UnpackList<&'v ClusterProfileRecord>>,
        #[starlark(require = named)] applications: Option<UnpackList<&'v ApplicationRecord>>,
        #[starlark(require = named)] preferences: Option<UnpackList<&'v PreferenceRecord>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // Checked before evaluation; see `version.rs`.
        let _ = gdl_version;
        // Refused here rather than lowered and reported later: `layout` names a
        // directory this build is about to write into, and one path segment is
        // the whole contract. A value with a separator in it would put the
        // generated crates somewhere the workspace manifest does not list.
        if let Some(layout) = layout
            && !gearbox_ir::is_valid_layout(layout)
        {
            return Err(anyhow::anyhow!(
                "layout = \"{layout}\" is not a single directory name; \
                 it must not be empty, start with a dot, or contain a path separator"
            ));
        }
        sink(eval)?.set_product(ProductDecl {
            id: id.to_owned(),
            display_name: name.unwrap_or(id).to_owned(),
            version: version.to_owned(),
            default_profile: default_profile.to_owned(),
            sources: sources.items.into_iter().cloned().collect(),
            templates: templates.cloned(),
            layout: layout.map(str::to_owned),
            profiles: profiles.items.into_iter().cloned().collect(),
            gears: gears.items.into_iter().cloned().collect(),
            bindings: bindings
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
            cluster_profiles: cluster_profiles
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
            applications: applications
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
            preferences: preferences
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
        });
        Ok(NoneType)
    }
}

/// `prefer.*` -- tie-breakers among choices that are already valid.
///
/// A namespace of functions rather than of members, because `isolate` takes an
/// argument. Kept distinct from a constraint on purpose: a preference may only
/// order candidates that already satisfy every hard requirement, so no
/// preference can make an invalid product valid.
#[starlark_module]
fn prefer_namespace(builder: &mut GlobalsBuilder) {
    fn existing_infrastructure() -> anyhow::Result<PreferenceRecord> {
        Ok(PreferenceRecord {
            kind: "existing-infrastructure".to_owned(),
            gear: None,
        })
    }

    fn fewer_applications() -> anyhow::Result<PreferenceRecord> {
        Ok(PreferenceRecord {
            kind: "fewer-applications".to_owned(),
            gear: None,
        })
    }

    fn isolate(#[starlark(require = named)] gear: &str) -> anyhow::Result<PreferenceRecord> {
        Ok(PreferenceRecord {
            kind: "isolate".to_owned(),
            gear: Some(gear.to_owned()),
        })
    }
}

/// Everything a `product.gdl` adds on top of whatever the builder already holds.
///
/// The product mirror of `globals::gear_additions`, and factored out for the
/// same reason: [`product_vocabulary`] applies it to an empty builder.
fn product_additions(builder: &mut GlobalsBuilder) {
    crate::globals::locator_vocabulary(builder);
    gdl_product_vocabulary(builder);
    for (name, namespace) in vocabulary::ALL_NAMESPACES {
        builder.set(name, *namespace);
    }
    builder.namespace("prefer", prefer_namespace);
}

/// Build the globals a `product.gdl` is evaluated against.
///
/// Deliberately a different set from `gear_globals()`: `gear()` is not callable
/// here and `product()` is not callable there, so a file that mixes the two
/// fails at the call rather than producing half of each.
#[must_use]
pub(crate) fn product_globals() -> starlark::environment::Globals {
    GlobalsBuilder::standard().with(product_additions).build()
}

/// The product-side GDL vocabulary alone, with no Starlark standard underneath.
///
/// The product mirror of `globals::gear_vocabulary`; see its note on why this
/// is built up rather than subtracted down.
#[must_use]
pub fn product_vocabulary() -> starlark::environment::Globals {
    GlobalsBuilder::new().with(product_additions).build()
}
