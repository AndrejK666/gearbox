//! The opaque records GDL's functions exchange.
//!
//! `provide(...)`, `cargo(...)`, `lifecycle(...)` and friends each return one of
//! these, and `gear(...)` consumes them. They are deliberately opaque: a caller
//! cannot inspect or mutate one from Starlark, so the only thing a description
//! file can do with a record is hand it to the function that expects it.
//!
//! Every record carries owned data only, which makes them all *simple* starlark
//! values -- no `Freeze`, no `Trace`. The shared derive stack is applied by
//! [`gdl_record!`] rather than repeated fifteen times.

// See the note on `unsafe_code` in the root Cargo.toml: starlark's
// `ProvidesStaticType` is an unsafe trait and its derive is required here. No
// `unsafe` block is written by hand.
#![allow(
    unsafe_code,
    reason = "starlark's ProvidesStaticType is an unsafe trait and its derive is \
              required on every host value"
)]

use std::fmt;

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::starlark_simple_value;
use starlark::values::{NoSerialize, StarlarkPagablePanic, StarlarkValue, starlark_value};

/// Declares an opaque GDL record.
///
/// Generates the derive stack validated against starlark 0.14.2, a `Display`
/// that prints the record's GDL spelling, and the `StarlarkValue` impl. The
/// `Display` is what a type-mismatch error quotes back, so it names the
/// constructor a reader wrote rather than the Rust type.
macro_rules! gdl_record {
    (
        $(#[$meta:meta])*
        $name:ident as $starlark_type:literal {
            $(
                $(#[$field_meta:meta])*
                pub $field:ident : $ty:ty,
            )*
        }
    ) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, PartialEq, Eq,
            ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative
        )]
        pub struct $name {
            $(
                $(#[$field_meta])*
                pub $field: $ty,
            )*
        }

        starlark_simple_value!($name);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($starlark_type, "(...)"))
            }
        }

        // `#[starlark_value]` requires a named lifetime parameter and fails to
        // expand against `'_`. Unlike a hand-written impl, macro-generated code
        // is not flagged by `elidable_lifetime_names`, so no waiver is needed.
        #[starlark_value(type = $starlark_type)]
        impl<'v> StarlarkValue<'v> for $name {}
    };
}

gdl_record! {
    /// `ui_module(package = path(...), requires = [...])`
    UiModuleRecord as "gdl_ui_module" {
        pub package: String,
        pub requires: Vec<String>,
    }
}

gdl_record! {
    /// `ui_product(shell = path(...), sdk = path(...), modules = [...])`
    UiProductRecord as "gdl_ui_product" {
        pub shell: String,
        pub sdk: String,
        pub modules: Vec<String>,
        #[allocative(skip)]
        pub source: Option<gearbox_ir::SourceId>,
    }
}

gdl_record! {
    /// `cargo(crate = ..., lib = ..., path = ..., features = [...], link = [...])`
    ///
    /// `lib` is mandatory rather than derived from `crate`: a crate with no
    /// explicit `[lib]` section takes its library identifier from its package
    /// name, so `cf-api-contracts` is `cf_api_contracts`, not `api_contracts`.
    /// Deriving it would emit a link line that does not compile.
    CargoRecord as "gdl_cargo" {
        pub crate_name: String,
        pub lib_ident: String,
        pub path: String,
        pub features: Vec<String>,
        pub default_features: bool,
        /// `use ... as _;` idents. Empty means "just `lib_ident`"; a gear with
        /// plugins names each nested module path explicitly.
        pub link: Vec<String>,
        /// Optional narrowing path to the `#[toolkit::gear]` attribute, relative
        /// to the crate root.
        ///
        /// Omitted means "scan `src/` and require exactly one". Needed when a
        /// crate declares several gears -- `gears/mini-chat/mini-chat` declares
        /// three -- because projection is meaningless until exactly one
        /// attribute is identified.
        pub attr: Option<String>,
        /// Where `cargo(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `npm(...)`, `nuget(...)`, `go(...)` -- a package in an ecosystem Gearbox
    /// locates but does not project.
    ///
    /// One record for all of them: past its name and path, nothing about a
    /// non-Cargo package is read. `cargo(...)` stays separate because a build
    /// needs its `lib`, features and link set.
    PackageRecord as "gdl_package" {
        #[allocative(skip)]
        pub ecosystem: gearbox_ir::Ecosystem,
        pub name: String,
        pub path: String,
        /// Where the constructor was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `scenario("...", priority = "p1", done = True)` -- one thing a gear lets
    /// its users do, and whether it is written.
    ScenarioRecord as "gdl_scenario" {
        pub text: String,
        pub priority: Option<String>,
        pub done: bool,
    }
}

gdl_record! {
    /// `docs(prd = ..., design = ..., adr = [...], openapi = ...)`
    ///
    /// Every field is an override, not a requirement. The platform keeps these at
    /// `gears/<name>/docs/` in 35 of 35 cases, so the engine finds them by
    /// convention and this record exists only for a gear laid out differently.
    /// Paths are relative to the description's own directory.
    DocsRecord as "gdl_docs" {
        pub prd: Option<String>,
        pub design: Option<String>,
        pub adr: Vec<String>,
        pub openapi: Option<String>,
        /// Where `docs(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `config(rust = ..., exposes = [...])` -- the gear's configuration surface.
    ///
    /// A **locator plus a curation**, and neither half restates the other. `rust`
    /// says which struct the gear deserializes its configuration into, playing
    /// the same role `sdk` plays for an extension point: nothing in the gear's
    /// own crate says it, and the projector reads the fields once it is told
    /// where to look. It is optional because the projector can usually find the
    /// struct itself, from the single `ctx.config*()` call in `impl Gear::init`.
    ///
    /// `exposes` is the half Rust cannot hold: *which* of a struct's fields is
    /// worth putting in front of an integrator, and in what order.
    /// `ApiGatewayConfig` declares fourteen and the configuration files in the
    /// tree set five. Naming a field that does not exist is an error rather than
    /// a silent omission -- that check is what keeps this from becoming a second
    /// copy of the struct.
    ConfigRecord as "gdl_config" {
        pub rust: Option<String>,
        pub exposes: Vec<String>,
        /// Where `config(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `feature("k8s-auth", kinds = ["kubernetes"])` -- one Cargo feature a gear
    /// offers an integrator, and where it belongs.
    ///
    /// **The declared half of a projected fact, exactly as `exposes` is for
    /// config fields.** A crate's `[features]` table is projected into
    /// `available_features`, and it is uncurated: `integration` wants a Docker
    /// daemon, `e2e-diagnostics` is a test switch, and `default` is not a choice
    /// at all. Which of them is worth putting in front of someone composing a
    /// product is a judgement Cargo has no way to hold.
    ///
    /// `kinds` is the second judgement, and the one the manifest cannot express
    /// either: `k8s-auth` is not merely *available* for a Kubernetes
    /// deployment, it is what that deployment needs and what a local one must
    /// not have. Empty means the feature suits every deployment kind, which is
    /// the ordinary case and why it is not written.
    FeatureRecord as "gdl_feature" {
        pub name: String,
        pub kinds: Vec<String>,
        /// Where `feature(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `extension_point("cf.core.authn_resolver.plugin.v1~", trait = "...")` --
    /// one point a host lets plugins fill.
    ///
    /// **Declared, because reading it out of the code was a guess.** A point
    /// used to be any `pub trait` with `Plugin` in its name, and a plugin any
    /// crate that implemented one. The corpus broke that five ways: hosts that
    /// implement their own trait as a proxy or a built-in, a trait with no
    /// `Plugin` in it, one crate declaring three gears, two points over one
    /// trait. What every real family does have is a GTS spec derived from
    /// `PluginV1` that instances register under and the host selects by -- so
    /// that is the key, and it is checked against the SDK rather than trusted.
    ///
    /// `spec` is the spec's own segment, without the `PluginV1` base. `sdk` is
    /// where `trait` lives, and is written only when that is not the gear's own
    /// SDK -- bss-rate-provider's sources implement a trait from the ledger's.
    ExtensionPointRecord as "gdl_extension_point" {
        pub spec: String,
        pub trait_ident: String,
        pub sdk: Option<CargoRecord>,
        /// The host's config key it selects a plugin by, as a dotted path --
        /// `"idp.vendor"`. Absent means a top-level `vendor`.
        pub selector: Option<String>,
        /// Where `extension_point(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `lifecycle(entry = ..., stop_timeout = ..., await_ready = ...)`
    LifecycleRecord as "gdl_lifecycle" {
        pub entry: Option<String>,
        pub stop_timeout: Option<String>,
        pub await_ready: bool,
    }
}

gdl_record! {
    /// `endpoint(name = ..., config_key = ..., default_port = ..., via = ...)`
    ///
    /// `config_key` is the gear's real per-gear configuration key -- the REST
    /// host reads `bind_addr`, the gRPC hub reads `listen_addr` -- so the
    /// generator writes whichever one this gear actually reads. `via` marks a
    /// gear that binds nothing itself and is mounted on the process's REST host.
    EndpointRecord as "gdl_endpoint" {
        pub name: String,
        pub config_key: Option<String>,
        pub default_port: Option<u16>,
        pub via: Option<String>,
    }
}

gdl_record! {
    /// `rest(base_path = ..., require_full_coverage = ..., visibility = ...)`
    RestRecord as "gdl_rest" {
        pub base_path: String,
        pub require_full_coverage: bool,
        /// `"exposed"` or `"internal"`; validated on the way into the IR.
        pub visibility: Option<String>,
    }
}

gdl_record! {
    /// `grpc(package = ..., service = ..., stubs_module = ...)`
    GrpcRecord as "gdl_grpc" {
        pub package: String,
        pub service: String,
        pub stubs_module: String,
    }
}

gdl_record! {
    /// `provide(contract = ..., version = ..., kind = ..., rust = ..., sdk = ..., ...)`
    ProvideRecord as "gdl_provide" {
        /// The contract trait's name, used as a join key against the
        /// `#[toolkit::contract]` that owns its identity, version and kind.
        pub contract: String,
        /// The Rust path of the *versioned* trait, e.g. `...::PaymentApiV2`.
        pub rust: String,
        pub sdk: CargoRecord,
        /// The associated fn building the in-process implementation.
        pub local: Option<String>,
        pub rest: Option<RestRecord>,
        pub grpc: Option<GrpcRecord>,
        pub policies: Vec<String>,
    }
}

gdl_record! {
    /// `consume(contract = ..., version = ..., from_ = ..., critical = ..., ...)`
    ///
    /// `from_` rather than `from` because `from` is a Starlark keyword.
    ConsumeRecord as "gdl_consume" {
        /// Join key against the owning `#[toolkit::contract]`.
        pub contract: String,
        pub rust: String,
        pub sdk: CargoRecord,
        pub critical: bool,
        pub resolving_client: Option<String>,
        /// Where `consume(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `cluster.cache(profile = ..., capabilities = [...])` and its siblings.
    ClusterRequireRecord as "gdl_cluster_require" {
        /// `cache`, `leader-election` or `lock`.
        pub primitive: String,
        /// The cluster scope name. The runtime calls this a cluster profile;
        /// renamed so it cannot be confused with a deployment profile.
        ///
        /// A join key, not free text: it must name one of the gear's own
        /// `impl ClusterProfile` markers, or the requirement can never be bound
        /// (GBX0508).
        pub scope: String,
        /// From `cluster_cap.*`, already resolved to capability ids.
        pub capabilities: Vec<String>,
    }
}

gdl_record! {
    /// `cluster_plugin(package = cargo(...), process_local = ..., ...)` -- where a
    /// cluster backend plugin lives, plus the two facts about it that Rust does
    /// not state.
    ///
    /// A locator, not a restatement. `ClusterGear::provider_registry()` owns
    /// *which* providers exist and the plugin crates own their names and
    /// capabilities; all of that is projected. What cannot be projected is the
    /// mapping from a library identifier to a directory on disk -- the registry
    /// writes `standalone_cluster_plugin::StandaloneCacheProvider` and nothing in
    /// that expression says where the crate is -- so `package` supplies it, the
    /// same role `sdk` plays for a contract.
    ClusterPluginRecord as "gdl_cluster_plugin" {
        pub package: CargoRecord,
        /// Whether the backend's state lives inside a single process.
        ///
        /// Declared, not projected: no Rust construct states it. The nearest
        /// signal is that one plugin's options carry a connection string and the
        /// other's do not, and reading "process-local" out of that is an
        /// inference of ours rather than a statement of the code's. It is the
        /// decisive property for multi-process topologies, so it is worth
        /// declaring explicitly instead of guessing precisely.
        pub process_local: bool,
        /// Whether the provider needs credentials before it can connect.
        /// Declared for the same reason as [`Self::process_local`].
        pub needs_credentials: bool,
        /// Optional narrowing path to the backend impl, relative to the plugin
        /// crate root.
        ///
        /// Unnecessary today: each plugin crate holds exactly one impl of each
        /// backend trait, so the impl is locatable by trait alone. This is the
        /// escape hatch for a crate that grows a second one, and its absence is
        /// what GBX0510 tells the author to supply.
        pub backend: Option<String>,
        /// The struct a primitive's options are deserialized into, if declared.
        ///
        /// The join key the provider traits do not carry: they have
        /// `provider()` and `build_*(options: &serde_json::Map)`, and the type
        /// on the other side of that map is named only inside the `build_*`
        /// body. Declared here for the reason [`Self::process_local`] is --
        /// no Rust construct states it, and inferring it from a function body
        /// would be our reading rather than the code's statement.
        pub cache_options: Option<String>,
        pub leader_election_options: Option<String>,
        pub lock_options: Option<String>,
        /// Which option carries the credential, for a plugin that needs one.
        ///
        /// Declared for the same reason again, and a sharper case of it: the
        /// fields in question -- postgres's `connection_string`, redis's `url`
        /// -- are plain `String`, so the projector's `secrecy` test says nothing
        /// about them and a name heuristic catches one of the two. What the
        /// plugin *does* know is which of its options it will not work without,
        /// and that is a statement it can make here.
        pub credential_option: Option<String>,
        /// Where `cluster_plugin(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `role(name = ..., sharded = ..., instance_addressable = ...)`
    ///
    /// Parsed for forward compatibility and excluded from resolution: the
    /// runtime has no role concept, and a worker's directory identity is a
    /// single name fixed in its binary with no configuration override. Carries
    /// GBX0318 at resolution, and GBX0602 when it asks for labels.
    RoleRecord as "gdl_role" {
        pub name: String,
        pub directory_name: Option<String>,
        pub labels: Vec<String>,
        /// Where `role(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

// ---------------------------------------------------------------- product side
//
// A `product.gdl` states operator intent: which gears, which topology, which
// backend behind each cluster scope. Every profile is declared as *data* and
// selected at resolve time, which is why each of these carries a `profiles`
// list rather than the file carrying an `if`.

gdl_record! {
    /// `source(id = ..., at = path(...) | git(...))`
    SourceRecord as "gdl_source" {
        pub id: String,
        pub at: SourceAtRecord,
        /// `crates = registry(...)`: where the build takes this source's crates.
        pub crates: Option<SourceAtRecord>,
        /// Where this `source(...)` call was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `path(...)`, `git(...)` or `registry(...)`.
    ///
    /// All three name a real source. A registry source is the registry rather
    /// than one package -- `prefix` is what turns a gear id into a package name
    /// -- and the fetching is cargo's, in `gearbox_engine::registry`.
    SourceAtRecord as "gdl_source_at" {
        /// `path`, `git` or `registry`.
        pub kind: String,
        pub at: Option<String>,
        pub url: Option<String>,
        pub tag: Option<String>,
        pub rev: Option<String>,
        pub branch: Option<String>,
        /// Registry only: what a gear id is prefixed with to name its package.
        pub prefix: Option<String>,
        /// Git only: the directory inside the repository, from `git(path = ...)`.
        ///
        /// A gear's `source` uses it to say where the gear sits; a product's
        /// source is a whole repository and refuses it.
        pub subdir: Option<String>,
    }
}

gdl_record! {
    /// `postgres(...)`: a validated external database connection.
    PostgresRecord as "gdl_postgres" {
        #[allocative(skip)]
        pub settings: gearbox_ir::PostgresDatabase,
    }
}

gdl_record! {
    /// `embedded(...)`, `self_hosted(...)` or `kubernetes(...)`.
    ProfileRecord as "gdl_profile" {
        pub database: Option<PostgresRecord>,
        #[allocative(skip)]
        pub sqlite_databases: std::collections::BTreeMap<gearbox_ir::GearId, gearbox_ir::SqliteDatabase>,
        /// `embedded`, `self-hosted` or `kubernetes`.
        pub kind: String,
        pub id: String,
        pub host: Option<String>,
        pub discovery: Option<String>,
        pub target_dir: Option<String>,
        pub cargo_profile: Option<String>,
        pub namespace: Option<String>,
        pub image_registry: Option<String>,
        /// Where this profile constructor was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `use_gear("name", source = ..., features = [...], config = {...})`
    UseGearRecord as "gdl_use_gear" {
        pub gear: String,
        pub source: String,
        /// Registry only: the version requirement, in Cargo's spelling.
        pub version: Option<String>,
        /// Registry only: the package name, when the source's prefix is wrong
        /// for this gear.
        pub package: Option<String>,
        pub features: Vec<String>,
        /// Opaque per-gear configuration, carried through to the generator.
        pub config: Vec<(String, serde_json::Value)>,
        /// Implementations chosen for this gear's plugin extension points.
        pub plugins: Vec<PluginRecord>,
        /// Where this `use_gear(...)` call was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `bind(consumer = ..., contract = ..., mode = ..., transport = ..., ...)`
    BindRecord as "gdl_bind" {
        pub consumer: String,
        pub contract: String,
        /// From `binding_mode.*`.
        pub mode: String,
        /// From `transport.*`, when the operator pinned one.
        pub transport: Option<String>,
        pub endpoint: Option<String>,
        pub profiles: Vec<String>,
        /// Where this `bind(...)` call was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `provider("name", secret_ref = ..., **options)` -- a *reference* to a
    /// provider the catalogue already knows, plus the operator's options for it.
    ///
    /// Not to be confused with the retired gear-side `provider(...)` record,
    /// which described a provider. Providers are projected now; this names one.
    ProviderBindingRecord as "gdl_provider_binding" {
        pub provider: String,
        /// Free-form, because each plugin defines its own option schema and the
        /// SDK deliberately keeps that schema out of the framework: options
        /// arrive at a plugin as a raw JSON map.
        pub options: Vec<(String, serde_json::Value)>,
        pub secret_ref: Option<String>,
        /// Where `provider(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `cluster_profile(name = ..., cache = provider(...), ...)`
    ///
    /// `name` is the operator side of the join key a gear declares as
    /// `impl ClusterProfile { const NAME }`.
    ClusterProfileRecord as "gdl_cluster_profile" {
        pub scope: String,
        pub cache: ProviderBindingRecord,
        /// Omitted means the SDK compare-and-swap default over the cache.
        pub leader_election: Option<ProviderBindingRecord>,
        pub lock: Option<ProviderBindingRecord>,
        pub profiles: Vec<String>,
        /// Where `cluster_profile(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `application("name", anchor = ..., replicas = ..., profiles = [...])`
    ApplicationRecord as "gdl_application" {
        pub name: String,
        pub anchor: String,
        pub include: Vec<String>,
        /// Which of the anchor's declared roles this application runs as.
        ///
        /// The product's half of the join: the gear says which roles exist, the
        /// product says which of them it deploys and how many copies of each.
        pub role: Option<String>,
        pub replicas: u32,
        pub profiles: Vec<String>,
        /// Where `application(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}

gdl_record! {
    /// `prefer.existing_infrastructure()` and friends.
    PreferenceRecord as "gdl_preference" {
        /// `existing-infrastructure`, `fewer-applications` or `isolate`.
        pub kind: String,
        pub gear: Option<String>,
    }
}

gdl_record! {
    /// `plugin("name", config = {...}, profiles = [...])` -- one implementation
    /// chosen for a host's extension point.
    ///
    /// No `interface` field: the catalogue already knows which point this gear
    /// fills, so naming it here would restate a projected fact. A host with
    /// several points simply takes several entries.
    PluginRecord as "gdl_plugin" {
        /// The implementing gear's id.
        pub gear: String,
        /// Per-plugin configuration, merged into the generated config. `vendor`
        /// and `priority` here override the crate's compiled-in defaults.
        pub config: Vec<(String, serde_json::Value)>,
        /// Which deployment profiles this choice applies to. Empty means all.
        ///
        /// The reason this exists: the canonical case is static auth in dev and
        /// real OIDC in prod, and that has to be one product file, not two.
        pub profiles: Vec<String>,
        /// Where `plugin(...)` was written.
        #[allocative(skip)]
        pub declared_at: Option<gearbox_ir::Location>,
    }
}
