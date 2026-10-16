//! The GDL vocabulary, as starlark globals.
//!
//! Every parameter is `require = named`: GDL is keyword-only by design, so a
//! description reads as a set of labelled facts rather than a positional
//! signature nobody can remember. Lists are `UnpackList<&'v T>` rather than
//! `UnpackList<Value>` so a wrong-typed element fails to unpack with starlark's
//! own message, naming both the expected and the actual type.
//!
//! Note what these functions deliberately cannot do: none of them reads the
//! filesystem, the environment, the clock, or the deployment profile. A
//! description file therefore cannot branch on a resolution input even if the
//! dialect let it branch at all -- which is the third layer of
//! `cpt-gearbox-fr-gdl-declarative`, and the reason it holds structurally
//! rather than by review.

// These fire on code `#[starlark_module]` generates, not on anything written
// here: the macro emits one wrapper per vocabulary function, each taking every
// declared parameter and returning `Result` whether or not the body can fail.
// `allow` rather than `expect` because which of them fires depends on the
// expansion, and an unfulfilled `expect` is itself an error.
#![allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    reason = "artifacts of #[starlark_module] expansion, not of hand-written code"
)]

use gearbox_ir::ClusterPrimitive;
use starlark::environment::{Globals, GlobalsBuilder};
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;

use crate::edit::PROFILE_KINDS;
use crate::records::{
    CargoRecord, ClusterPluginRecord, ClusterRequireRecord, ConfigRecord, ConsumeRecord,
    DocsRecord, EndpointRecord, ExtensionPointRecord, FeatureRecord, GrpcRecord, LifecycleRecord,
    ProvideRecord, RestRecord, RoleRecord,
};
use crate::sink::{GdlSink, GearDecl, Maturity};
use crate::values::GdlEnum;
use crate::vocabulary;

/// Pull the sink out of the evaluator.
///
/// A missing sink is a wiring bug in this crate, not a user error, so it fails
/// the evaluation with an internal message rather than producing a diagnostic
/// that would look like the description's fault.
fn sink<'a>(eval: &'a Evaluator<'_, '_, '_>) -> anyhow::Result<&'a GdlSink> {
    eval.extra
        .and_then(|e| e.downcast_ref::<GdlSink>())
        .ok_or_else(|| anyhow::anyhow!("internal error: no GdlSink installed on the evaluator"))
}

/// A plugin spec as a description writes it: the family's own GTS segment.
///
/// One segment, ending in `~`, and without the `PluginV1` base -- every plugin
/// spec derives from `cf.toolkit.plugins.plugin.v1~`, so writing it would be the
/// same prefix on every line, and a full chain written here would be matched
/// against nothing. Only the shape is checked here; whether the SDK declares it
/// is the engine's question.
fn check_plugin_spec(field: &str, spec: &str) -> anyhow::Result<()> {
    const BASE: &str = "cf.toolkit.plugins.plugin.v1~";
    if let Some(own) = spec.strip_prefix(BASE) {
        return Err(anyhow::anyhow!(
            "{field}: write the spec's own segment `{own}`, without the `{BASE}` base every \
             plugin spec shares"
        ));
    }
    let segments = spec.matches('~').count();
    if spec.starts_with("gts.") || segments != 1 || !spec.ends_with('~') || spec.len() < 2 {
        return Err(anyhow::anyhow!(
            "{field}: `{spec}` is not a plugin spec segment; write one GTS segment ending in `~`, \
             e.g. \"cf.core.authn_resolver.plugin.v1~\""
        ));
    }
    Ok(())
}

/// Refuse a field the Rust attributes own.
///
/// Naming the owning attribute is the point: "unknown argument" would leave a
/// gear author guessing where the fact belongs, whereas this tells them it
/// already has a home and where (`cpt-gearbox-fr-gdl-no-restatement`).
fn restated(field: &str, owner: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "`{field}` is projected from Rust and must not be declared here; it is owned by \
         {owner}. Remove it from gear.gdl."
    )
}

#[starlark_module]
fn gdl_vocabulary(builder: &mut GlobalsBuilder) {
    /// `cargo(...)` -- where a gear's or SDK's crate lives.
    ///
    /// Spelled `crate_name` rather than `crate` because the starlark parameter
    /// name is the Rust identifier and `r#crate` is not a legal raw identifier.
    /// Unambiguous next to `lib` regardless: one is the package, one is the
    /// library target.
    fn cargo<'v>(
        #[starlark(require = named)] crate_name: &str,
        #[starlark(require = named)] lib: &str,
        #[starlark(require = named, default = ".")] path: &str,
        #[starlark(require = named)] features: Option<UnpackList<String>>,
        #[starlark(require = named, default = true)] default_features: bool,
        #[starlark(require = named)] link: Option<UnpackList<String>>,
        #[starlark(require = named)] attr: Option<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<CargoRecord> {
        Ok(CargoRecord {
            crate_name: crate_name.to_owned(),
            lib_ident: lib.to_owned(),
            path: path.to_owned(),
            features: features.map(|l| l.items).unwrap_or_default(),
            default_features,
            link: link.map(|l| l.items).unwrap_or_default(),
            attr: attr.map(str::to_owned),
            declared_at: crate::declarative::call_location(eval),
        })
    }

    /// `docs(...)` -- override where this gear's documents live.
    ///
    /// Only needed when they are not at `docs/` beside the gear or one level up.
    fn docs<'v>(
        #[starlark(require = named)] prd: Option<&str>,
        #[starlark(require = named)] design: Option<&str>,
        #[starlark(require = named)] adr: Option<UnpackList<String>>,
        #[starlark(require = named)] openapi: Option<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<DocsRecord> {
        Ok(DocsRecord {
            prd: prd.map(str::to_owned),
            design: design.map(str::to_owned),
            adr: adr.map(|l| l.items).unwrap_or_default(),
            openapi: openapi.map(str::to_owned),
            declared_at: crate::declarative::call_location(eval),
        })
    }

    /// `config(...)` -- where the gear's configuration lives, and what to show.
    ///
    /// See [`ConfigRecord`] for why the locator and the curation belong
    /// together and why neither restates the other.
    fn config<'v>(
        #[starlark(require = named)] rust: Option<&str>,
        #[starlark(require = named)] exposes: Option<UnpackList<String>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ConfigRecord> {
        Ok(ConfigRecord {
            rust: rust.map(str::to_owned),
            exposes: exposes.map(|l| l.items).unwrap_or_default(),
            declared_at: crate::declarative::call_location(eval),
        })
    }

    /// `extension_point("spec", trait = "...", sdk = cargo(...))` -- one point a
    /// host lets plugins fill.
    ///
    /// The spec is positional because it is the identity: it is what instances
    /// register under and what the host selects by. `trait` and `sdk` are what
    /// a person needs to write a plugin for it.
    fn extension_point<'v>(
        #[starlark(require = pos)] spec: &str,
        #[starlark(require = named)] r#trait: &str,
        #[starlark(require = named)] sdk: Option<&'v CargoRecord>,
        // Where in the host's config the vendor it selects by lives. Declared
        // because nothing in Rust says which `vendor` field is the selector:
        // account-management has two, one it selects its IdP plugin by
        // (`idp.vendor`) and one it registers itself under as a
        // tenant-resolver plugin (`tr_plugin.vendor`).
        #[starlark(require = named)] selector: Option<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ExtensionPointRecord> {
        check_plugin_spec("extension_point", spec)?;
        if let Some(path) = selector {
            let valid = !path.is_empty()
                && path.split('.').all(|seg| {
                    let mut chars = seg.chars();
                    chars
                        .next()
                        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                });
            if !valid {
                return Err(anyhow::anyhow!(
                    "extension_point(\"{spec}\", selector = \"{path}\"): a selector is a dotted \
                     path of config field names, e.g. `selector = \"idp.vendor\"`"
                ));
            }
        }
        if r#trait.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "extension_point(\"{spec}\", trait = \"\") names no trait; write the \
                 ClientHub interface plugins register under, e.g. `trait = \"AuthNResolverPluginClient\"`"
            ));
        }
        Ok(ExtensionPointRecord {
            spec: spec.to_owned(),
            trait_ident: r#trait.to_owned(),
            sdk: sdk.cloned(),
            selector: selector.map(str::to_owned),
            declared_at: crate::declarative::call_location(eval),
        })
    }

    /// `feature("name", kinds = [...])` -- one Cargo feature this gear offers.
    ///
    /// The name is positional because it is the whole subject; `kinds` is the
    /// exception rather than the rule and reads better named.
    fn feature<'v>(
        #[starlark(require = pos)] name: &str,
        #[starlark(require = named)] kinds: Option<UnpackList<String>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<FeatureRecord> {
        let kinds = kinds.map(|l| l.items).unwrap_or_default();
        // Checked here rather than lowered and reported later, for the reason a
        // fixed enumeration always is: there are exactly three deployment kinds,
        // a fourth spelling is a typo, and a typo that survived would silently
        // make the feature unofferable everywhere.
        for kind in &kinds {
            if !PROFILE_KINDS.contains(&kind.as_str()) {
                return Err(anyhow::anyhow!(
                    "feature(\"{name}\", kinds = [... \"{kind}\" ...]) names no deployment kind; \
                     write one of {}",
                    PROFILE_KINDS.join(", ")
                ));
            }
        }
        Ok(FeatureRecord {
            name: name.to_owned(),
            kinds,
            declared_at: crate::declarative::call_location(eval),
        })
    }

    /// `lifecycle(...)` -- start/stop behaviour, mirroring the gear macro's clause.
    fn lifecycle(
        #[starlark(require = named)] entry: Option<&str>,
        #[starlark(require = named)] stop_timeout: Option<&str>,
        #[starlark(require = named, default = false)] await_ready: bool,
    ) -> anyhow::Result<LifecycleRecord> {
        Ok(LifecycleRecord {
            entry: entry.map(str::to_owned),
            stop_timeout: stop_timeout.map(str::to_owned),
            await_ready,
        })
    }

    /// `endpoint(...)` -- something the gear listens on, or is mounted on.
    fn endpoint(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] config_key: Option<&str>,
        #[starlark(require = named)] default_port: Option<u32>,
        #[starlark(require = named)] via: Option<&str>,
    ) -> anyhow::Result<EndpointRecord> {
        let port = match default_port {
            Some(p) => Some(u16::try_from(p).map_err(|_| {
                anyhow::anyhow!("default_port {p} is not a valid TCP port (0-65535)")
            })?),
            None => None,
        };
        Ok(EndpointRecord {
            name: name.to_owned(),
            config_key: config_key.map(str::to_owned),
            default_port: port,
            via: via.map(str::to_owned),
        })
    }

    /// `rest(...)` -- a contract's REST projection.
    fn rest(
        #[starlark(require = named)] base_path: &str,
        #[starlark(require = named, default = false)] require_full_coverage: bool,
        #[starlark(require = named)] visibility: Option<&str>,
    ) -> anyhow::Result<RestRecord> {
        if let Some(v) = visibility {
            anyhow::ensure!(
                matches!(v, "exposed" | "internal"),
                "visibility must be \"exposed\" or \"internal\", got {v:?}"
            );
        }
        Ok(RestRecord {
            base_path: base_path.to_owned(),
            require_full_coverage,
            visibility: visibility.map(str::to_owned),
        })
    }

    /// `grpc(...)` -- a contract's gRPC projection.
    fn grpc(
        #[starlark(require = named)] package: &str,
        #[starlark(require = named)] service: &str,
        #[starlark(require = named)] stubs_module: &str,
    ) -> anyhow::Result<GrpcRecord> {
        Ok(GrpcRecord {
            package: package.to_owned(),
            service: service.to_owned(),
            stubs_module: stubs_module.to_owned(),
        })
    }

    /// `provide(...)` -- a contract this gear provides.
    fn provide<'v>(
        #[starlark(require = named)] contract: &str,
        #[starlark(require = named)] rust: &str,
        #[starlark(require = named)] sdk: &'v CargoRecord,
        #[starlark(require = named)] local: Option<&str>,
        #[starlark(require = named)] rest: Option<&'v RestRecord>,
        #[starlark(require = named)] grpc: Option<&'v GrpcRecord>,
        #[starlark(require = named)] policies: Option<UnpackList<String>>,
        // Accepted only to be refused by name: see `restated` below.
        #[starlark(require = named)] version: Option<&str>,
        #[starlark(require = named)] transports: Option<Value<'v>>,
        #[starlark(require = named)] kind: Option<&'v GdlEnum>,
    ) -> anyhow::Result<ProvideRecord> {
        if version.is_some() {
            return Err(restated("version", "#[toolkit::contract(version = ...)]"));
        }
        if kind.is_some() {
            return Err(restated(
                "kind",
                "the contract trait's name suffix (Api / Embedded / Backend / Extension)",
            ));
        }
        if transports.is_some() {
            return Err(restated(
                "transports",
                "the `<Base>Rest` / `<Base>Grpc` projection traits beside the base trait \
                 in the sdk crate",
            ));
        }

        Ok(ProvideRecord {
            contract: contract.to_owned(),
            rust: rust.to_owned(),
            sdk: sdk.clone(),
            local: local.map(str::to_owned),
            rest: rest.cloned(),
            grpc: grpc.cloned(),
            policies: policies.map(|l| l.items).unwrap_or_default(),
        })
    }

    /// `consume(...)` -- a declared contract edge, and the only kind of edge
    /// the resolver may ever place across a process boundary.
    fn consume<'v>(
        #[starlark(require = named)] contract: &str,
        #[starlark(require = named)] rust: &str,
        #[starlark(require = named)] sdk: &'v CargoRecord,
        #[starlark(require = named, default = false)] critical: bool,
        #[starlark(require = named)] resolving_client: Option<&str>,
        // Accepted only to be refused by name: see `restated`.
        #[starlark(require = named)] from_: Option<&str>,
        #[starlark(require = named)] version: Option<&str>,
        #[starlark(require = named)] kind: Option<&'v GdlEnum>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ConsumeRecord> {
        if from_.is_some() {
            return Err(restated("from_", "#[toolkit::consumes(from = ...)]"));
        }
        if version.is_some() {
            return Err(restated("version", "#[toolkit::contract(version = ...)]"));
        }
        if kind.is_some() {
            return Err(restated("kind", "the contract trait's name suffix"));
        }
        Ok(ConsumeRecord {
            contract: contract.to_owned(),
            rust: rust.to_owned(),
            sdk: sdk.clone(),
            critical,
            resolving_client: resolving_client.map(str::to_owned),
            declared_at: crate::declarative::call_location(eval),
        })
    }

    /// `cluster_plugin(...)` -- where a backend plugin crate lives.
    ///
    /// Providers themselves are projected from
    /// `ClusterGear::provider_registry()`; this only says where to look and
    /// carries the facts Rust does not state.
    ///
    /// `*_options` names the struct a primitive's options are deserialized
    /// into. **A join key, not a schema**: the struct is already the authority
    /// -- every one of them is `#[derive(Deserialize)]` with
    /// `#[serde(deny_unknown_fields)]` -- and what does not exist in Rust is
    /// anything connecting it to the primitive. `build_cache(options: &Map)`
    /// reads it with `serde_json::from_value` inside its own body, which the
    /// projector's method-call search cannot see, and reading the link out of
    /// that body would be our inference rather than the code's statement. Same
    /// argument as `process_local`, one level down.
    ///
    /// Omitted, the options stay the untyped bag they are today.
    fn cluster_plugin<'v>(
        #[starlark(require = named)] package: &'v CargoRecord,
        #[starlark(require = named, default = false)] process_local: bool,
        #[starlark(require = named, default = false)] needs_credentials: bool,
        #[starlark(require = named)] backend: Option<&str>,
        #[starlark(require = named)] cache_options: Option<&str>,
        #[starlark(require = named)] leader_election_options: Option<&str>,
        #[starlark(require = named)] lock_options: Option<&str>,
        #[starlark(require = named)] credential_option: Option<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<ClusterPluginRecord> {
        Ok(ClusterPluginRecord {
            package: package.clone(),
            process_local,
            needs_credentials,
            backend: backend.map(str::to_owned),
            cache_options: cache_options.map(str::to_owned),
            leader_election_options: leader_election_options.map(str::to_owned),
            lock_options: lock_options.map(str::to_owned),
            credential_option: credential_option.map(str::to_owned),
            declared_at: crate::declarative::call_location(eval),
        })
    }

    /// `role(name, directory_name?, labels = [])` -- one differentiated shape
    /// this gear runs in.
    ///
    /// `name` is the value the gear's own mode selector accepts, which is the
    /// only spelling checkable against a projected enum. `directory_name` is
    /// what an instance of this role registers under; left out, it defaults to
    /// `<gear-id>-<name>` at lowering, where the gear's id is known -- the id
    /// is projected from Rust and nothing here can see it.
    ///
    /// `labels` are the label *keys* an instance registers under, not values:
    /// a value is per-instance and belongs to the deployment, which the runtime
    /// sources from configuration or the environment. A key is a fact about the
    /// gear.
    fn role<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] directory_name: Option<&str>,
        #[starlark(require = named)] labels: Option<UnpackList<String>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<RoleRecord> {
        Ok(RoleRecord {
            name: name.to_owned(),
            directory_name: directory_name.map(str::to_owned),
            labels: labels.map(|l| l.items).unwrap_or_default(),
            declared_at: crate::declarative::call_location(eval),
        })
    }

    /// `gear(...)` -- the one declaration a `gear.gdl` makes.
    fn gear<'v>(
        #[starlark(require = named)] name: Option<&str>,
        #[starlark(require = named)] description: Option<&str>,
        #[starlark(require = named)] category: Option<&str>,
        #[starlark(require = named)] visibility: Option<&str>,
        // Required for a gear with code, refused at `design`: it is what tells
        // the projector which crate to scan, and a design gear has none yet.
        #[starlark(require = named)] package: Option<&'v CargoRecord>,
        // Required, no default; one of `Maturity::SPELLINGS`. Taken as an
        // option only so its absence gets a message rather than "missing
        // argument".
        #[starlark(require = named)] maturity: Option<&str>,
        // A locator, like `cluster_plugins`: nothing in a gear's own crate says
        // where its SDK lives, and the SDK is what declares the GTS types this
        // gear exposes and the traits its extension points name.
        #[starlark(require = named)] sdk: Option<&'v CargoRecord>,
        // The plugin role, declared on both sides and keyed by GTS spec. See
        // `ExtensionPointRecord` for why this is no longer read from the code.
        #[starlark(require = named)] extension_points: Option<UnpackList<&'v ExtensionPointRecord>>,
        #[starlark(require = named)] implements: Option<&str>,
        // The keyword before 2026-10-02. Accepted only to say what it became:
        // `implements` read as a verb about data, and a plugin *implements* its
        // host's extension point -- the word eCos CDL uses for the same role.
        #[starlark(require = named)] fills: Option<starlark::values::Value<'v>>,
        #[starlark(require = named)] docs: Option<&'v DocsRecord>,
        #[starlark(require = named)] provides: Option<UnpackList<&'v ProvideRecord>>,
        #[starlark(require = named)] consumes: Option<UnpackList<&'v ConsumeRecord>>,
        #[starlark(require = named)] requires: Option<UnpackList<&'v ClusterRequireRecord>>,
        #[starlark(require = named)] serves: Option<UnpackList<&'v EndpointRecord>>,
        #[starlark(require = named)] cluster_plugins: Option<UnpackList<&'v ClusterPluginRecord>>,
        #[starlark(require = named)] roles: Option<UnpackList<&'v RoleRecord>>,
        #[starlark(require = named)] config_schema: Option<&'v ConfigRecord>,
        #[starlark(require = named)] cargo_features: Option<UnpackList<&'v FeatureRecord>>,
        // Accepted only to be refused by name, so the diagnostic can say which
        // attribute owns the fact instead of "unknown argument" -- except `id`
        // on a design gear, which has no attribute to project it from.
        #[starlark(require = named)] id: Option<&str>,
        #[starlark(require = named)] runtime_caps: Option<UnpackList<&'v GdlEnum>>,
        #[starlark(require = named)] colocated_deps: Option<UnpackList<String>>,
        #[starlark(require = named)] lifecycle: Option<&'v LifecycleRecord>,
        #[starlark(require = named)] client: Option<&str>,
        // Accepted as an untyped value because it exists only to be refused;
        // binding it to a record type would imply the surface still exists.
        #[starlark(require = named)] cluster_providers: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        if fills.is_some() {
            return Err(anyhow::anyhow!(
                "`fills` was renamed to `implements`: write `implements = \"<spec>~\"`"
            ));
        }

        let maturity = match maturity {
            None => {
                return Err(anyhow::anyhow!(
                    "a gear declares its maturity: `maturity = \"experimental\" | \"preview\" | \
                     \"stable\" | \"deprecated\"`, or `\"design\"` for a gear with no code yet. \
                     There is no default, because `stable` would be a promise nobody made"
                ));
            }
            Some(spelling) => Maturity::parse(spelling).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown maturity `{spelling}`: expected one of {}",
                    Maturity::SPELLINGS
                        .iter()
                        .map(|s| format!("\"{s}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?,
        };

        if maturity == Maturity::Design {
            // What a design gear may say is what `gear.toml` said, plus where
            // its SDK and documents are: everything else describes code.
            for (present, field) in [
                (package.is_some(), "package"),
                (visibility.is_some(), "visibility"),
                (extension_points.is_some(), "extension_points"),
                (implements.is_some(), "implements"),
                (provides.is_some(), "provides"),
                (consumes.is_some(), "consumes"),
                (requires.is_some(), "requires"),
                (serves.is_some(), "serves"),
                (cluster_plugins.is_some(), "cluster_plugins"),
                (roles.is_some(), "roles"),
                (config_schema.is_some(), "config_schema"),
                (cargo_features.is_some(), "cargo_features"),
                (runtime_caps.is_some(), "runtime_caps"),
                (colocated_deps.is_some(), "colocated_deps"),
                (lifecycle.is_some(), "lifecycle"),
                (client.is_some(), "client"),
                (cluster_providers.is_some(), "cluster_providers"),
            ] {
                if present {
                    return Err(anyhow::anyhow!(
                        "`{field}` describes code, and a design gear has none yet: remove it, \
                         or declare `package = cargo(...)` and drop `maturity = \"design\"`"
                    ));
                }
            }
            let Some(id) = id else {
                return Err(anyhow::anyhow!(
                    "a design gear names its own id: write `id = \"<kebab-case>\"`. There is \
                     no `#[toolkit::gear(name = ...)]` to project it from yet"
                ));
            };
            gearbox_ir::GearId::new(id)
                .map_err(|e| anyhow::anyhow!("`{id}` is not a valid gear id: {e}"))?;
        }

        for (present, field, owner) in [
            (
                id.is_some() && maturity != Maturity::Design,
                "id",
                "#[toolkit::gear(name = ...)]",
            ),
            (
                runtime_caps.is_some(),
                "runtime_caps",
                "#[toolkit::gear(capabilities = [...])]",
            ),
            (
                colocated_deps.is_some(),
                "colocated_deps",
                "#[toolkit::gear(deps = [...])]",
            ),
            (
                lifecycle.is_some(),
                "lifecycle",
                "#[toolkit::gear(lifecycle(...))]",
            ),
            (client.is_some(), "client", "#[toolkit::gear(client = ...)]"),
            (
                cluster_providers.is_some(),
                "cluster_providers",
                "the with_*_provider calls in ClusterGear::provider_registry()",
            ),
        ] {
            if present {
                return Err(restated(field, owner));
            }
        }

        if let Some(spec) = implements {
            check_plugin_spec("implements", spec)?;
        }

        sink(eval)?.set_gear(GearDecl {
            maturity: Some(maturity),
            id: id
                .filter(|_| maturity == Maturity::Design)
                .map(str::to_owned),
            name: name.map(str::to_owned),
            description: description.map(str::to_owned),
            category: category.map(str::to_owned),
            visibility: visibility.map(str::to_owned),
            package: package.cloned(),
            sdk: sdk.cloned(),
            extension_points: extension_points
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
            implements: implements.map(str::to_owned),
            docs: docs.cloned(),
            provides: provides
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
            consumes: consumes
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
            requires: requires
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
            serves: serves
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
            cluster_plugins: cluster_plugins
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
            declared_roles: roles
                .map(|l| l.items.into_iter().cloned().collect())
                .unwrap_or_default(),
            config_schema: config_schema.cloned(),
            cargo_features: cargo_features.map(|l| l.items.into_iter().cloned().collect()),
            declared_at: crate::declarative::call_location(eval),
        });
        Ok(NoneType)
    }

    /// `fail(msg)` -- a declarative assertion. Permitted because it states a
    /// fact about validity rather than choosing between alternatives.
    fn fail(#[starlark(require = pos)] message: &str) -> anyhow::Result<NoneType> {
        Err(anyhow::anyhow!("{message}"))
    }
}

/// The `cluster.*` requirement constructors.
///
/// A namespace of *functions*, unlike `cap`/`transport`/... which are
/// namespaces of values -- so `GlobalsBuilder::namespace` is the right tool
/// here and an attribute-bearing value is the right tool there.
#[starlark_module]
/// `cluster.cache(...)`, `cluster.leader_election(...)`, `cluster.lock(...)`.
///
/// `profile` is mandatory and has no default. It is a join key against the
/// gear's own `impl ClusterProfile { const NAME }`, and the SDK turns it into
/// `ClientScope::new("cluster:{name}")` -- so a wrong value is not a typo that
/// degrades, it is a scope nothing ever registered, failing at startup with
/// `ProfileNotBound`. A `"default"` fallback would have been silently wrong for
/// the platform's only real consumer, which binds `"event-broker"`.
fn cluster_namespace(builder: &mut GlobalsBuilder) {
    fn cache<'v>(
        #[starlark(require = named)] profile: &str,
        #[starlark(require = named)] capabilities: Option<UnpackList<&'v GdlEnum>>,
    ) -> anyhow::Result<ClusterRequireRecord> {
        cluster_require(ClusterPrimitive::Cache, profile, capabilities)
    }

    fn leader_election<'v>(
        #[starlark(require = named)] profile: &str,
        #[starlark(require = named)] capabilities: Option<UnpackList<&'v GdlEnum>>,
    ) -> anyhow::Result<ClusterRequireRecord> {
        cluster_require(ClusterPrimitive::LeaderElection, profile, capabilities)
    }

    fn lock<'v>(
        #[starlark(require = named)] profile: &str,
        #[starlark(require = named)] capabilities: Option<UnpackList<&'v GdlEnum>>,
    ) -> anyhow::Result<ClusterRequireRecord> {
        cluster_require(ClusterPrimitive::Lock, profile, capabilities)
    }
}

/// Shared body of the three `cluster.*` constructors.
///
/// Capability names are resolved against the primitive, so asking for
/// `prefix_watch` on a lock is refused here rather than silently carried into
/// the catalogue -- `prefix_watch` is a cache property and a lock that claimed
/// it would describe something that does not exist.
fn cluster_require(
    primitive: ClusterPrimitive,
    profile: &str,
    capabilities: Option<UnpackList<&GdlEnum>>,
) -> anyhow::Result<ClusterRequireRecord> {
    let capabilities = capabilities
        .map(|list| {
            list.items
                .iter()
                .map(|v| {
                    vocabulary::cluster_capability(v, primitive)
                        .map(str::to_owned)
                        .map_err(|e| anyhow::anyhow!(e))
                })
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();

    Ok(ClusterRequireRecord {
        primitive: primitive.slug().to_owned(),
        scope: profile.to_owned(),
        capabilities,
    })
}

/// Everything GDL adds on top of whatever the builder already holds.
///
/// Factored out of [`gear_globals`] so that [`gear_vocabulary`] can apply the
/// same additions to an empty builder. One definition, two bases -- the
/// alternative, subtracting the standard set from the full one, is wrong (see
/// [`gear_vocabulary`]).
fn gear_additions(builder: &mut GlobalsBuilder) {
    gdl_vocabulary(builder);
    for (name, namespace) in vocabulary::ALL_NAMESPACES {
        builder.set(name, *namespace);
    }
    builder.namespace("cluster", cluster_namespace);
}

/// Build the globals a `gear.gdl` is evaluated against.
///
/// `GlobalsBuilder::standard()` rather than `new()`: descriptions legitimately
/// use `len`, string methods and the like. What matters is that nothing here
/// reaches the outside world.
#[must_use]
pub fn gear_globals() -> Globals {
    GlobalsBuilder::standard().with(gear_additions).build()
}

/// The gear-side GDL vocabulary alone, with no Starlark standard underneath.
///
/// Exists so the editor's grammar is generated from the very globals the
/// interpreter evaluates against, rather than from a list kept in step by hand
/// (`tests/export_grammar.rs`).
///
/// Deliberately *not* `gear_globals()` minus `GlobalsBuilder::standard()`:
/// `fail` is a Starlark standard global that GDL overrides on purpose, so a
/// subtraction by name would drop it and the editor would quietly stop
/// colouring it.
#[must_use]
pub fn gear_vocabulary() -> Globals {
    GlobalsBuilder::new().with(gear_additions).build()
}
