//! The catalogue: developer-owned facts about what exists.
//!
//! One of the model's three separate representations. This one answers "what is
//! there and what does it need", says nothing about what anyone wants, and is
//! derived entirely from `gear.gdl` files cross-checked against Rust source.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::contract::{CargoRef, ContractDescriptor, PackageRef, ProviderDescriptor};
use crate::diagnostics::Diagnostics;
use crate::ids::{ContractId, GearId, RelPath, SourceId};
use crate::requirement::{ClusterProviderDecl, Requirement};

/// A runtime capability a gear declares.
///
/// The complete closed set of seven. Each corresponds one-to-one with a trait the
/// gear macro asserts an implementation of at compile time, which is why no
/// eighth value can be invented here: there would be no trait behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCap {
    /// Owns database schema and participates in migrations.
    Db,
    /// Contributes REST routes, mounted by the process's REST host.
    Rest,
    /// Hosts the process-wide REST surface. At most one per process.
    RestHost,
    /// Has a start/stop lifecycle and background work.
    Stateful,
    /// Participates in the early and late system phases.
    System,
    /// Hosts the process-wide gRPC surface. At most one per process.
    GrpcHub,
    /// Registers gRPC services.
    Grpc,
}

impl RuntimeCap {
    pub const ALL: &'static [Self] = &[
        Self::Db,
        Self::Rest,
        Self::RestHost,
        Self::Stateful,
        Self::System,
        Self::GrpcHub,
        Self::Grpc,
    ];

    /// The spelling used in `#[toolkit::gear(capabilities = [...])]`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Db => "db",
            Self::Rest => "rest",
            Self::RestHost => "rest_host",
            Self::Stateful => "stateful",
            Self::System => "system",
            Self::GrpcHub => "grpc_hub",
            Self::Grpc => "grpc",
        }
    }

    /// The trait the gear macro asserts an implementation of.
    ///
    /// Recorded so a diagnostic can name the trait a gear is missing rather than
    /// just the capability it claimed.
    #[must_use]
    pub const fn asserted_trait(self) -> &'static str {
        match self {
            Self::Db => "DatabaseCapability",
            Self::Rest => "RestApiCapability",
            Self::RestHost => "ApiGatewayCapability",
            Self::Stateful => "RunnableCapability",
            Self::System => "SystemCapability",
            Self::GrpcHub => "GrpcHubCapability",
            Self::Grpc => "GrpcServiceCapability",
        }
    }

    /// Whether a process may contain at most one gear with this capability.
    ///
    /// The runtime registry enforces this at startup; the resolver enforces it
    /// earlier so an invalid composition never gets built.
    #[must_use]
    pub const fn is_process_singleton(self) -> bool {
        matches!(self, Self::RestHost | Self::GrpcHub)
    }

    /// Parse the spelling used in the gear macro.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == s)
    }
}

impl std::fmt::Display for RuntimeCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a gear is meant to be selected directly by an integrator.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Composable by an integrator.
    Public,
    /// Present because something else needs it; not offered as a choice.
    #[default]
    Internal,
}

/// How much a gear with code promises, as its description declares it.
///
/// Ordered by promise, so `<` reads as "less settled than" -- except
/// `Deprecated`, which is last because it is the end of the line, not because
/// it promises most. A gear with no code is not here: it is `implementation =
/// "design"`, has no maturity, and is kept in [`Catalogue::designs`].
///
/// One of two axes. This one is what a gear promises its users; how much of
/// it is written is [`Implementation`].
///
/// No `Default`. The description must say, and a missing value is refused at
/// evaluation: `Stable` as a default makes forgetting the field a promise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Maturity {
    /// Code exists; its API and behaviour may change freely.
    Experimental,
    /// Usable, but nobody has declared it stable.
    Preview,
    /// Supported for ordinary production use.
    Stable,
    /// Still available; not for new products.
    Deprecated,
}

impl Maturity {
    /// Every level, in order of increasing promise.
    pub const ALL: [Self; 4] = [
        Self::Experimental,
        Self::Preview,
        Self::Stable,
        Self::Deprecated,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Experimental => "experimental",
            Self::Preview => "preview",
            Self::Stable => "stable",
            Self::Deprecated => "deprecated",
        }
    }

    /// The level a description spells, if it is one.
    #[must_use]
    pub fn parse(spelling: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == spelling)
    }
}

/// How much of a gear is written: the status `GEARS.md` marks with `[ ]` and
/// `[x]`, for the whole gear.
///
/// The other axis from [`Maturity`]. A `stable` gear may still have work
/// planned; an `experimental` one may be complete. Absent means the
/// description does not say, which is not a claim either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum Implementation {
    /// Documents, perhaps an SDK; no crate yet. Such a gear is in
    /// [`Catalogue::designs`], not [`Catalogue::gears`].
    Design,
    /// Code exists and work on it continues.
    InProgress,
    /// Everything the gear sets out to do is written.
    Done,
}

impl Implementation {
    /// Every value, in order of progress.
    pub const ALL: [Self; 3] = [Self::Design, Self::InProgress, Self::Done];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::InProgress => "in-progress",
            Self::Done => "done",
        }
    }

    /// The value a description spells, if it is one.
    #[must_use]
    pub fn parse(spelling: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|i| i.as_str() == spelling)
    }
}

/// A hand-written chart that packages one executable unit anchored on this gear
/// (`gear(deploy = helm_chart(...))`, GDL 1.3).
///
/// When a product application *is* that unit -- anchored on this gear, linking
/// exactly `gears` (plugins aside) -- the generator ships this chart instead of
/// generating one, and the unit's own image instead of a generated crate: the
/// chart owns the unit's ports, listeners and hardening, and the product
/// supplies only its gears' configuration (`configOverlay`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct UnitChart {
    /// The chart directory, relative to the source root.
    pub path: RelPath,
    /// The image repository the unit's Dockerfile builds, without registry or tag.
    pub image: String,
    /// The non-plugin gears the unit's binary links.
    pub gears: BTreeSet<GearId>,
    /// The Services the chart fixes, by endpoint name (`grpc`, `internal`,
    /// `rest`, or `serve` for a worker's own listener) to `service:port`. The
    /// resolver addresses the process by these, not by names and ports it
    /// would have chosen for a generated chart.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub services: BTreeMap<String, String>,
}

/// An authentication plane a gear's REST operations use (gears-rust ADR-0008,
/// ADR-0011 section 3). Anonymous operations need no validator, so they are not
/// a plane here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum RestPlane {
    /// Operations a tenant bearer token authenticates; served only with a
    /// tenant authenticator (`authn-resolver`) in the process.
    Tenant,
    /// Operations the platform token authenticates (`.platform()`).
    Platform,
}

impl RestPlane {
    /// Every value.
    pub const ALL: [Self; 2] = [Self::Tenant, Self::Platform];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::Platform => "platform",
        }
    }

    /// The value a description spells, if it is one.
    #[must_use]
    pub fn parse(spelling: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == spelling)
    }
}

/// One thing a gear lets its users do, and whether it is written yet.
///
/// What a `GEARS.md` line holds: `- [x] p1 - enqueue and execute jobs`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Scenario {
    pub text: String,

    /// `p1` (first) to `p5`, when the description gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,

    /// Whether it is written.
    #[serde(default)]
    pub done: bool,
}

/// A gear's lifecycle declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct LifecycleDecl {
    /// The method name the runtime calls to start background work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,

    /// How long to wait for a graceful stop, spelled as the runtime spells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_timeout: Option<String>,

    /// Whether startup waits for this gear to report ready.
    #[serde(default)]
    pub await_ready: bool,
}

/// Something a gear listens on.
///
/// `config_key` is the real per-gear configuration key, not an invention: the
/// REST host and the gRPC hub spell their bind addresses differently, and the
/// generator has to write whichever one this gear actually reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EndpointDecl {
    /// A short name, unique within the gear, e.g. `rest` or `grpc`.
    pub name: String,

    /// The per-gear configuration key carrying the bind address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_key: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_port: Option<u16>,

    /// Set when this gear does not bind anything itself but is mounted on the
    /// process's REST host, or on a worker's own out-of-process router.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

impl EndpointDecl {
    /// Whether this endpoint binds its own socket.
    #[must_use]
    pub const fn binds_own_socket(&self) -> bool {
        self.config_key.is_some() && self.via.is_none()
    }
}

/// One differentiated shape a gear runs in, and the name it registers under.
///
/// A role is a directory registration name: instances of one role answer to it
/// and a bare-name lookup reaches only whichever role claims the gear's own id.
/// That is the platform's model (ADR `cpt-cf-adr-instance-addressable-discovery`),
/// and `gear.gdl` is the manifest it asks for and never defines
/// (ADR `cpt-gearbox-adr-role-qualified-names`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DeclaredRole {
    /// The value the gear's own mode selector accepts.
    pub name: String,

    /// What an instance of this role registers under.
    ///
    /// Always present by the time it reaches here: the description may leave it
    /// out, and lowering fills in `<gear-id>-<name>`. Resolved rather than
    /// optional because every reader wants the name, and deriving it in each of
    /// them is one derivation too many.
    pub directory_name: String,

    /// The label *keys* an instance of this role registers under.
    ///
    /// Keys, not values: a value is per-instance -- a `StatefulSet` ordinal, a
    /// topic pattern -- and belongs to the deployment. The key is a fact about
    /// the gear.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub labels: BTreeSet<String>,
}

/// The shape of one configuration value, to the precision a control needs.
///
/// `Enum` carries its variants **as data**, read from the Rust enum. A gear
/// author adding an enum, or a variant to one, must not require a new Gearbox
/// release -- the same reason an extension point keys on a trait's shape rather
/// than on a list of trait names.
///
/// `Complex` is the honest answer for anything that is not a scalar: a nested
/// structure, a list, a `#[serde(flatten)]` map, or a type declared outside the
/// crates that were scanned. It carries no control, and inventing one would
/// write a value the gear rejects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigFieldType {
    Str,
    Bool,
    Int,
    Float,
    /// The wire spellings of a unit-only enum, after `rename_all` and `rename`.
    Enum {
        variants: Vec<String>,
    },
    Complex,
}

/// One configuration field a product may set on a gear.
///
/// Projected from the gear's config struct, never declared: the name, the type,
/// whether it is required and what it defaults to are all stated in Rust, and
/// restating them in a description is the drift ADR
/// `cpt-gearbox-adr-macro-projected-catalogue` exists to make impossible.
///
/// Two consumers read this, and that is why it is one model rather than two: the
/// Studio renders a typed control per field, and the chart generator owes
/// `cpt-gearbox-fr-values-schema` a JSON Schema constraining exactly "field
/// exists, field type, whether field is required".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ConfigFieldDecl {
    /// The name **on the wire**, after `#[serde(rename)]` and `rename_all`.
    ///
    /// Not the Rust ident: `TenantConfig::tenant_type` is written `type` in
    /// every configuration file in the tree, and a control labelled
    /// `tenant_type` would write a key the gear never reads.
    pub name: String,

    #[serde(rename = "type")]
    pub ty: ConfigFieldType,

    /// Whether omitting the field is an error.
    pub required: bool,

    /// The compiled-in default, when it is a literal the projector can read.
    ///
    /// A placeholder in a control, and the `default` of a values schema. Absent
    /// means "not readable from here", not "there is none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "string | number | boolean | null")]
    pub default: Option<serde_json::Value>,

    /// The field's doc comment, which is the only prose an operator gets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,

    /// The value is a credential slot.
    ///
    /// Read from the Rust type (`secrecy::SecretString`), not guessed from the
    /// name. `cpt-gearbox-fr-no-secrets-in-values` requires generated values to
    /// express such a field as a reference to an externally managed secret, and
    /// a generator cannot do that for a field it cannot tell from a hostname.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,

    /// One level of a struct-typed field: the fields of the struct its type
    /// names, when the scanned crate declares it. The control is still
    /// [`ConfigFieldType::Complex`]; a check reads these to name a required
    /// key missing from a set value (`jwt.claim_mapping`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<ConfigFieldDecl>,
}

/// A gear's configuration surface, as its description curates it.
///
/// The description contributes one product judgement Rust has no way to hold --
/// *which* fields are worth putting in front of an integrator -- and the field
/// facts come from Rust. `ApiGatewayConfig` declares fourteen fields while the
/// configuration files in the tree set five.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ConfigSchema {
    /// The Rust struct the gear deserializes its configuration into.
    pub rust: String,

    /// The exposed fields, in the order the description named them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<ConfigFieldDecl>,
}

/// Where a gear's source comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// A local directory.
    Path,
    /// A Git repository at a pinned revision.
    Git,
    /// A package registry. The unpacked package directory is the root.
    Registry,
}

impl SourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Git => "git",
            Self::Registry => "registry",
        }
    }
}

/// A source, resolved to something on disk with a recorded digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedSource {
    pub id: SourceId,
    pub kind: SourceKind,

    /// The path or URL, as declared.
    pub location: String,

    /// What was actually read.
    ///
    /// For a Git worktree this is the commit, marked dirty when the tree has
    /// uncommitted changes; for a plain path it is a content digest. Either way
    /// it is what makes a lock reproducible rather than merely repeatable.
    pub digest: String,
}

/// Everything known about one gear.
// `PartialEq` without `Eq`: a descriptor now carries a projected config default,
// which may be a float, and floats have no total equality. Nothing keys a map on
// a descriptor -- `GearId` does that -- so the marker was never load-bearing.
/// Whether `name` is a permission name as gears-rust
/// `toolkit::api::operation_policy::is_valid_permission_name` reads one: at
/// least two dot-separated segments of `[a-z0-9_-]`, each starting with a
/// letter or a digit (`settings.values.write`).
#[must_use]
pub fn is_valid_permission_name(name: &str) -> bool {
    let segments: Vec<&str> = name.split('.').collect();
    segments.len() >= 2
        && segments.iter().all(|segment| {
            segment
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct GearDescriptor {
    pub id: GearId,

    /// Human-readable name. Free to change without breaking references, which is
    /// the entire reason it is separate from `id`.
    pub display_name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,

    #[serde(default)]
    pub visibility: Visibility,

    /// Declared in `gear(maturity = ...)`, and required there.
    ///
    /// Not carried into the lock, like `config_schema`: it is what the gear
    /// promises, not something resolution decided. A product reports what it
    /// uses below `stable` (GBX0322-0324) instead.
    pub maturity: Maturity,

    /// How much of the gear is written, when the description says. Never
    /// `Design`: a gear with no code is a [`DesignGear`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementation: Option<Implementation>,

    /// What the gear lets its users do, in the order declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<Scenario>,

    /// Which declared source this gear was read from.
    pub source: SourceId,

    /// Where its description lives, relative to that source's root.
    pub gdl_path: RelPath,

    pub package: CargoRef,

    /// This gear's SDKs, in every language it has one: the Rust SDK first, then
    /// the others in the order declared.
    ///
    /// Located, not projected: nothing in the gear's crate says where its SDKs
    /// are. At most one is Cargo; that one is also what the projector reads for
    /// the traits the gear's extension points name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sdks: Vec<PackageRef>,

    /// Where this gear lives in a git repository, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<GitOrigin>,

    /// Where this gear's tests are, as declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<Located>,

    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub runtime_caps: BTreeSet<RuntimeCap>,

    /// Gears that must be in the same binary as this one.
    ///
    /// Link-time, not logical: the gear macro emits a hidden re-export for each,
    /// so the crate is physically present, and the registry treats a declared
    /// dependency that is absent as a hard failure. These edges can therefore
    /// never be severed by the resolver -- they form a closure that pulls each
    /// dependency into every process reaching it, rather than a partition.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub colocated_deps: BTreeSet<GearId>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<LifecycleDecl>,

    /// Contracts this gear provides.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provides: Vec<ProviderDescriptor>,

    /// Declared contract edges. Severable, unlike `colocated_deps`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumes: Vec<Requirement>,

    /// Non-contract requirements, which today means cluster primitives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<Requirement>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serves: Vec<EndpointDecl>,

    /// The planes the gear's REST operations use, when its description says
    /// (`rest_planes = [...]`). `None` is not "none": undeclared, the resolver
    /// assumes tenant operations, the fail-closed reading of the runtime's start
    /// check (GBX0417). `Some` of an empty set is a gear whose REST capability
    /// serves only probes. An authored claim: the runtime is what checks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest_planes: Option<BTreeSet<RestPlane>>,

    /// The hand-written chart of the unit this gear anchors, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_chart: Option<UnitChart>,

    /// The client trait the gear macro asserts is object-safe, if declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_trait: Option<String>,

    /// Cluster providers this gear registers. Only the cluster gear has any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cluster_providers: Vec<ClusterProviderDecl>,

    /// Plugin extension points this gear expects an implementation for.
    ///
    /// Declared in the description by GTS spec and verified against the SDK. A
    /// gear may have several: `mini-chat` declares an audit point and a
    /// model-policy point, each implemented independently.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_points: Vec<ExtensionPointDecl>,

    /// The extension point this gear *implements*, if it is a plugin.
    ///
    /// Declared, like the host's side. A gear may be both: bss-rate-provider
    /// implements the ledger's point and declares one of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implements: Option<PluginImpl>,

    /// The vendor string this gear's config selects a plugin by.
    ///
    /// Per gear, not per point: the only multi-point host in the tree
    /// (`mini-chat`) declares two extension points and exactly one `vendor`
    /// field, so one selector covers all of a host's points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_selector: Option<String>,

    /// Roles declared for forward compatibility and excluded from resolution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared_roles: Vec<DeclaredRole>,

    /// Whether only one of this gear may run in an installation.
    ///
    /// Projected from `#[toolkit::gear(one_per_installation = ...)]`. The
    /// runtime cannot enforce it -- a process refuses a second `rest_host`
    /// because it sees its own gears, and no process sees another -- so the
    /// refusal is this tool's (ADR `cpt-gearbox-adr-one-per-installation`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub one_per_installation: bool,

    /// The abstract permissions the gear's operations declare, or that its
    /// code asks for (gears-rust `cpt-cf-adr-operation-permissions` section 6):
    /// string literals in `.permission("...")`, `#[permission("...")]` on its
    /// SDK's `rest_contract` methods, and `PermissionMap::holds(_, "...")`.
    /// A product maps each to the scopes that grant it.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub permissions: BTreeSet<String>,

    /// Where the gear names a permission this tool could not read -- a
    /// constant or a variable instead of a literal -- so [`Self::permissions`]
    /// is known to be incomplete. Each entry is a `file:line` in the crate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable_permissions: Vec<String>,

    /// The Cargo features the gear's own crate declares.
    ///
    /// Projected from its `Cargo.toml`, because `use_gear(..., features = [...])`
    /// writes Cargo feature names and a client with no list of them can only
    /// offer a text box -- where a typo becomes a feature that does not exist and
    /// a build failure two steps later. Empty means the crate declares none,
    /// which 7 of the 14 gears in the corpus correctly do.
    ///
    /// **Uncurated on purpose, and a client must say so.** Some of these gate a
    /// crate's own test matrix (`integration` needs a Docker daemon), and which
    /// features an integrator should be offered is a declaration nobody has
    /// written yet -- the same projected-facts / declared-selection split
    /// `config_schema` makes, whose declared half is `exposes`.
    ///
    /// Not carried into the lock, for the same reason as `config_schema`: what a
    /// gear *can* be built with is not a decision the resolution made.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub available_features: BTreeSet<String>,

    /// The Cargo features this gear offers, curated and scoped by the
    /// description.
    ///
    /// The declared counterpart of [`GearDescriptor::available_features`], and
    /// the same split `config_schema` makes with `exposes`: Cargo states what
    /// features *exist*, and the description states which are worth offering
    /// and where each belongs. Empty means nobody has curated this gear yet,
    /// which is why a client falls back to the projected list rather than
    /// showing nothing.
    ///
    /// Checked, not trusted: a name here that the crate's `[features]` does not
    /// declare is `GBX0213`, for the reason `exposes` is checked against the
    /// struct it curates.
    ///
    /// Not carried into the lock, like `available_features`: what a gear *can*
    /// be built with is not a decision the resolution made. What the resolution
    /// decided is `ResolvedGear.selected_features`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_features: Option<Vec<CargoFeature>>,

    /// The gear's configuration surface, when its description locates one.
    ///
    /// Was an opaque `RelPath` pointing at a schema file that nothing ever
    /// opened. It is now the merge of a locator the description gives and the
    /// fields projected from the struct that locator names -- see
    /// [`ConfigSchema`]. Absent means the description declares no
    /// `config_schema`, which for a gear that reads no configuration is the
    /// right answer rather than a gap.
    ///
    /// Not carried into the lock: `ResolvedGear` records what was *decided*, and
    /// a catalogue's account of what a gear *can* be configured with is not a
    /// decision. The previous wording claimed the opposite and was never true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<ConfigSchema>,

    /// Where this gear's own documents live.
    ///
    /// Found by convention next to the gear and one level up, because the
    /// platform keeps them at `gears/<name>/docs/` while a `gear.gdl` sits in a
    /// crate subdirectory below that. Absent when the gear has none, which is
    /// ordinary rather than a gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<GearDocs>,

    /// GTS types this gear exposes, from the schema declarations in its SDK.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gts_types: Vec<GtsTypeDecl>,

    /// Where `gear(...)` was written in this gear's description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<crate::diagnostics::Location>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<crate::ui::GearUi>,
}

/// How far a gear has got through a staged catalogue load.
///
/// Only the incomplete stages are named. A gear that finishes projection leaves
/// the pending list and enters the catalogue, so there is no `Projected` variant
/// here -- being in `Catalogue::gears` *is* that state, and giving it a second
/// spelling would invite the two to disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LoadStage {
    /// Its `gear.gdl` was found; nothing has been read.
    Discovered,
    /// The description was evaluated, so the declared facts are known.
    Declared,
}

/// A gear found, and perhaps declared, but not yet projected.
///
/// Keyed by `gdl_path` rather than `GearId`, and that is not a convenience:
/// `id` is projected from `#[toolkit::gear(name = ...)]`, so it does not exist
/// until the crate is parsed. A registry view therefore has a name to display
/// long before it has an identifier to key by (ADR
/// `cpt-gearbox-adr-staged-catalogue-loading`).
///
/// Existing as a separate list, rather than as a state on `GearDescriptor`, is
/// what lets `Option::None` and an empty `Vec` keep the single meaning *absent*
/// everywhere in the catalogue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PendingGear {
    pub source: SourceId,

    /// Where its description lives, relative to that source's root. The stable
    /// key for this gear until projection supplies an `id`.
    pub gdl_path: RelPath,

    pub stage: LoadStage,

    /// Available from `Declared` onwards; `None` while merely `Discovered`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// What a tree groups by, and available before anything is parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

/// A gear described before it has code: `implementation = "design"` in its `gear.gdl`.
///
/// **A separate collection, not a state of [`GearDescriptor`].** A descriptor's
/// facts are projected from a crate -- its package, capabilities, contracts --
/// and a design gear has none, so as a descriptor every one of them would be a
/// default standing in for "unknown", and every consumer of `gears` would have
/// to learn to skip it. Kept apart, the resolver and the generator never see a
/// design gear at all, and the one thing that does -- naming it in
/// `use_gear(...)` -- gets its own diagnostic (GBX0321) instead of
/// "unknown gear".
///
/// What it carries is what the platform's `gear.toml` carried for these gears
/// before it was retired: a name, a description, a category, plus where its
/// SDK and documents are.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DesignGear {
    /// Declared, not projected: there is no attribute yet to project it from.
    pub id: GearId,

    pub display_name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,

    pub source: SourceId,

    pub gdl_path: RelPath,

    /// The SDKs that exist ahead of the gear, in any language, ordered as on
    /// [`GearDescriptor::sdks`] -- `llm-gateway` and `model-registry` publish
    /// their Rust SDKs first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sdks: Vec<PackageRef>,

    /// Where this gear will live in a git repository, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<GitOrigin>,

    /// What the gear will let its users do, in the order declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<Scenario>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<GearDocs>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<crate::diagnostics::Location>,
}

/// Documents describing one gear, all relative to its source root.
///
/// Paths rather than content: the catalogue stays small, and an editor can open
/// them. Relative to the source root like `gdl_path`, so a lock built on one
/// machine still points somewhere on another.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GearDocs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prd: Option<RelPath>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<RelPath>,

    /// Architecture decision records, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adr: Vec<RelPath>,

    /// A checked-in `OpenAPI` document, when one exists.
    ///
    /// Grouped with the prose deliberately: it is found the same way and is the
    /// same kind of pointer. Usually absent -- the runtime builds the spec from
    /// the REST projections, and only four gears in the platform check one in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openapi: Option<RelPath>,

    /// The documents that are not on disk here, as web links.
    ///
    /// A document declared as a URL is one. So is a declared path that is not
    /// in this checkout when the gear names its `source = git(...)`: the link
    /// points at the same path in that repository. A path on disk always wins.
    #[serde(default, skip_serializing_if = "DocUrls::is_empty")]
    pub urls: DocUrls,
}

impl GearDocs {
    /// Whether anything was found at all, so the caller can skip an empty block.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prd.is_none()
            && self.design.is_none()
            && self.adr.is_empty()
            && self.openapi.is_none()
            && self.urls.is_empty()
    }
}

/// [`GearDocs`] as web links, for what is not on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DocUrls {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prd: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adr: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openapi: Option<String>,
}

impl DocUrls {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prd.is_none() && self.design.is_none() && self.adr.is_empty() && self.openapi.is_none()
    }
}

/// Where something a description points at is: on disk, or on the web.
///
/// One of the two is set. `path` when the target is in this checkout, relative
/// to the source root; `url` when it is not, either because the description
/// wrote a URL or because the gear's `source = git(...)` says where it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Located {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<RelPath>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Where a gear lives in a git repository, from `source = git(...)`.
///
/// What a reader holding only the description follows to the code and the
/// documents. Paths in the description are looked for on disk first; this is
/// the fallback, and what `validate` checks a checkout against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GitOrigin {
    /// The repository, as declared: `https://...` or `git@host:org/repo`.
    pub url: String,

    /// The gear's directory inside the repository; `.` for its root.
    pub path: RelPath,

    /// The tag, revision or branch, when one is declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

impl GitOrigin {
    /// A web link to `relative`, a path written relative to the gear's
    /// directory, in this repository.
    ///
    /// `None` when the path climbs out of the repository. Hosts differ in where
    /// the reference goes: `/blob/` for GitHub and most others, `/-/blob/` for
    /// GitLab, `/src/` for Bitbucket. Without a reference the link follows the
    /// default branch (`HEAD`).
    #[must_use]
    pub fn web_url(&self, relative: &str) -> Option<String> {
        let target = self.path.resolve(relative).ok()?;
        let base = web_base(&self.url);
        let reference = self.reference.as_deref().unwrap_or("HEAD");
        let infix = if base.contains("://gitlab.") {
            "-/blob"
        } else if base.contains("://bitbucket.org/") {
            "src"
        } else {
            "blob"
        };
        Some(if target.is_here() {
            format!("{base}/tree/{reference}")
        } else {
            format!("{base}/{infix}/{reference}/{target}")
        })
    }

    /// Whether `remote` is this repository or a fork of it.
    ///
    /// A fork keeps the host and the repository's name and changes the owner:
    /// a checkout of `me/gears-rust` is a checkout of `org/gears-rust` for the
    /// purpose of finding a path in it. A different name is a different
    /// repository.
    ///
    /// **An SSH host alias stands for a host it does not name.** A remote such
    /// as `git@github-me:me/gears-rust.git` dials whatever `~/.ssh/config`
    /// maps `github-me` to, which is how a second account on one forge is
    /// usually cloned. Resolving it would mean running `ssh -G`; a dotless SSH
    /// host is never a real forge's name, so the name alone decides there.
    #[must_use]
    pub fn same_or_fork(&self, remote: &str) -> bool {
        let host_and_name = |url: &str| {
            let normalized = Self::normalized_url(url);
            let mut segments = normalized.split('/');
            let host = segments.next().unwrap_or_default().to_owned();
            let name = segments.next_back().unwrap_or_default().to_owned();
            (host, name)
        };
        let (host, name) = host_and_name(&self.url);
        let (remote_host, remote_name) = host_and_name(remote);
        name == remote_name && (host == remote_host || is_ssh_alias(remote, &remote_host))
    }

    /// The repository in the one spelling two remotes can be compared in:
    /// host and path, lowercase, with no scheme, user or `.git`.
    #[must_use]
    pub fn normalized_url(url: &str) -> String {
        web_base(url)
            .split_once("://")
            .map_or_else(|| url.to_owned(), |(_, rest)| rest.to_owned())
            .to_lowercase()
    }
}

/// Whether `url` is an SSH remote whose `host` is an `~/.ssh/config` alias.
fn is_ssh_alias(url: &str, host: &str) -> bool {
    let url = url.trim();
    let ssh = url.starts_with("ssh://") || (!url.contains("://") && url.contains(':'));
    ssh && !host.contains('.')
}

/// `git@host:org/repo.git` and `https://host/org/repo.git` as `https://host/org/repo`.
fn web_base(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    if let Some((scheme, rest)) = url.split_once("://") {
        // `ssh://git@host/org/repo` and `https://user@host/...`: drop the user.
        let rest = rest.split_once('@').map_or(rest, |(_, host)| host);
        let scheme = if scheme.starts_with("http") {
            scheme
        } else {
            "https"
        };
        return format!("{scheme}://{rest}");
    }
    // scp-like: `git@host:org/repo`.
    let rest = url.split_once('@').map_or(url, |(_, host)| host);
    format!("https://{}", rest.replacen(':', "/", 1))
}

/// One curated Cargo feature, and the deployment kinds it belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CargoFeature {
    /// The Cargo feature name, as `use_gear(features = [...])` would write it.
    pub name: String,

    /// Which deployment kinds this feature belongs to: `embedded`,
    /// `self-hosted`, `kubernetes`, spelled as `DeploymentProfileDecl::kind`
    /// spells them.
    ///
    /// **Empty means every kind, and that is the ordinary case.** A non-empty
    /// list is two statements at once: the feature is *offered* for those kinds
    /// and *refused* for the rest. `k8s-auth` is the case that motivated it --
    /// a Kubernetes deployment needs it and a local one must not have it, and
    /// nothing in `Cargo.toml` can say so.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub kinds: BTreeSet<String>,
}

/// A GTS type a gear exposes.
///
/// "Exposes" means declared in the gear's SDK crate, which is what other gears
/// can depend on. A type declared only in the main crate is internal, and a
/// `gts_id!` reference is not a declaration at all -- there are over a thousand
/// of those in the tree, mostly in tests.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct GtsTypeDecl {
    /// The GTS identifier, e.g.
    /// `cf.toolkit.plugins.plugin.v1~cf.core.cluster.plugin.v1~`.
    pub type_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Where it is declared, relative to the SDK crate's `src/`.
    pub relative: String,
}

/// A point a host lets plugins fill.
///
/// **The identity is the GTS spec**, not the trait. Every plugin family in the
/// corpus registers instances under a spec derived from `PluginV1`, and the host
/// selects by it -- and two points can share one trait: the ledger's rate
/// provider and bss-rate-provider's sources both implement
/// `bss_ledger_sdk::RateProviderV1`, and only their specs tell them apart.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct ExtensionPointDecl {
    /// The full GTS type id of the plugin spec, e.g.
    /// `cf.toolkit.plugins.plugin.v1~cf.core.authn_resolver.plugin.v1~`. The
    /// join key: a plugin's [`PluginImpl::spec`] matches this.
    pub spec: String,

    /// The interface plugins register under, as written, e.g.
    /// `AuthNResolverPluginClient`. Never a derived short name:
    /// `to_kebab_case("AuthNResolverPluginClient")` gives
    /// `auth-n-resolver-plugin-client`, the split GBX0206 exists to catch.
    pub trait_ident: String,

    /// The library identifier of the crate `trait_ident` lives in, e.g.
    /// `authn_resolver_sdk`. What `qualified()` spells.
    pub sdk_lib: String,

    /// The SDK crate itself, so a client can write a locator that points at it.
    ///
    /// **Added because `sdk_lib` alone is not enough to name the crate.** A
    /// scaffolded plugin needs `sdk = cargo(crate_name, lib, path)`, and a client
    /// given only the library identifier had to guess the other two -- which the
    /// New Gear wizard duly did, from the *host's* package, producing a locator
    /// naming the host crate at a path invented from a fixed `../../`. The
    /// description the point was projected from has all three, so the fix is to
    /// carry them rather than to reconstruct them.
    ///
    /// `path` keeps `CargoRef`'s own meaning: relative to the *source root* the
    /// host was read from, which is what makes it resolvable by anyone who knows
    /// where that root is.
    pub sdk: CargoRef,

    /// The host's config key it selects a plugin by, as written in
    /// `extension_point(selector = ...)`: a dotted path such as `idp.vendor`.
    /// Absent means the top-level `vendor`. The *value* it defaults to is the
    /// gear's [`GearDescriptor::vendor_selector`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
}

impl ExtensionPointDecl {
    /// How the point is spelled in diagnostics and CLI output.
    #[must_use]
    pub fn qualified(&self) -> String {
        format!("{}::{}", self.sdk_lib, self.trait_ident)
    }
}

/// The extension point a plugin gear implements.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PluginImpl {
    /// The full GTS type id of the spec this plugin implements; matches
    /// [`ExtensionPointDecl::spec`] on its host.
    pub spec: String,

    /// The host's declaration of that point, once the catalogue has found it.
    ///
    /// `None` when no described gear declares the spec, which is reported
    /// (GBX0519) rather than guessed at: a plugin names only the spec, and the
    /// trait and SDK are the host's to state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<ExtensionPointDecl>,

    /// The vendor this plugin registers itself under, compiled in as a default.
    ///
    /// `None` means the crate states no default, so a product that does not set
    /// one leaves the plugin unreachable by the host's selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_vendor: Option<String>,

    /// Lower wins when several plugins share a vendor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_priority: Option<i64>,
}

impl PluginImpl {
    /// How the implemented point is spelled in diagnostics: the host's trait when the
    /// catalogue has joined it, the spec when no described gear declares it.
    #[must_use]
    pub fn describe(&self) -> String {
        self.point
            .as_ref()
            .map_or_else(|| self.spec.clone(), ExtensionPointDecl::qualified)
    }
}

impl GearDescriptor {
    /// Whether this gear declares the extension point keyed by `spec`.
    #[must_use]
    pub fn declares_point(&self, spec: &str) -> bool {
        self.extension_points.iter().any(|p| p.spec == spec)
    }

    #[must_use]
    pub fn has_cap(&self, cap: RuntimeCap) -> bool {
        self.runtime_caps.contains(&cap)
    }

    /// The single-per-process capabilities this gear claims.
    pub fn singleton_caps(&self) -> impl Iterator<Item = RuntimeCap> + '_ {
        self.runtime_caps
            .iter()
            .copied()
            .filter(|c| c.is_process_singleton())
    }

    /// The contracts this gear provides.
    pub fn provided_contracts(&self) -> impl Iterator<Item = &ContractId> + '_ {
        self.provides.iter().map(|p| &p.contract)
    }

    /// The directory holding this gear's description, relative to its source root.
    #[must_use]
    pub fn gdl_dir(&self) -> RelPath {
        self.gdl_path.parent()
    }

    /// The gear's crate directory, relative to its source root.
    ///
    /// Already resolved: [`CargoRef::path`] is stored root-relative, because the
    /// merge that produced it is the one place that knows both the description's
    /// location and how to report a path that does not resolve.
    #[must_use]
    pub fn crate_dir(&self) -> RelPath {
        self.package.path.clone()
    }

    /// Whether this gear declared anything the runtime cannot realize.
    ///
    /// Used to decide whether to attach the corresponding refusal diagnostics,
    /// which are per-gear rather than per-product.
    #[must_use]
    pub fn declares_unsupported(&self) -> bool {
        !self.declared_roles.is_empty()
    }

    /// Whether any declared role registers under a label.
    ///
    /// Which is per-instance addressing by another name: a label is how one
    /// instance of a role is told apart from its peers.
    #[must_use]
    pub fn declares_labels(&self) -> bool {
        self.declared_roles.iter().any(|r| !r.labels.is_empty())
    }
}

/// What exists.
///
/// Ordered maps throughout: iteration order is part of the determinism guarantee,
/// not an implementation detail.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct Catalogue {
    pub gears: BTreeMap<GearId, GearDescriptor>,
    pub contracts: BTreeMap<ContractId, ContractDescriptor>,
    pub sources: BTreeMap<SourceId, ResolvedSource>,

    /// Gears described before they have code. Never resolved; see
    /// [`DesignGear`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub designs: BTreeMap<GearId, DesignGear>,

    /// Problems found while building the catalogue.
    #[serde(default)]
    pub diagnostics: Diagnostics,
}

impl Catalogue {
    #[must_use]
    pub fn gear(&self, id: &GearId) -> Option<&GearDescriptor> {
        self.gears.get(id)
    }

    #[must_use]
    pub fn contract(&self, id: &ContractId) -> Option<&ContractDescriptor> {
        self.contracts.get(id)
    }

    /// Which gears implement `point`.
    ///
    /// This is what makes "which plugins can I use here" a catalogue fact rather
    /// than a CLI trick: the editor, the CLI and the validator all ask the same
    /// question of the same data. Several answers is the normal case, and legal
    /// -- the host picks among linked plugins at runtime by vendor and priority.
    #[must_use]
    pub fn implementations_of(&self, point: &ExtensionPointDecl) -> Vec<&GearDescriptor> {
        self.gears
            .values()
            .filter(|g| g.implements.as_ref().is_some_and(|f| f.spec == point.spec))
            .collect()
    }

    /// Which gears provide `contract`.
    ///
    /// Returns a vector rather than an option: nothing forbids two gears from
    /// providing the same contract, and the resolver has to be able to say so.
    #[must_use]
    pub fn providers_of(&self, contract: &ContractId) -> Vec<&GearDescriptor> {
        self.gears
            .values()
            .filter(|g| g.provided_contracts().any(|c| c == contract))
            .collect()
    }

    /// Every version of the contract family `contract` belongs to.
    ///
    /// Used to explain a major mismatch by listing what the provider does offer.
    #[must_use]
    pub fn contract_family(&self, contract: &ContractId) -> Vec<&ContractDescriptor> {
        let Some(target) = self.contract(contract) else {
            return Vec::new();
        };
        self.contracts
            .values()
            .filter(|c| c.owner == target.owner && c.base_name == target.base_name)
            .collect()
    }

    /// The cluster provider declarations found anywhere in the catalogue.
    ///
    /// Only the cluster gear declares any, but resolving through the catalogue
    /// rather than a hard-coded gear id keeps the resolver from knowing that gear
    /// by name.
    #[must_use]
    pub fn cluster_providers(&self) -> Vec<&ClusterProviderDecl> {
        self.gears
            .values()
            .flat_map(|g| g.cluster_providers.iter())
            .collect()
    }

    /// Find a cluster provider by the name used in configuration.
    #[must_use]
    pub fn cluster_provider(&self, name: &str) -> Option<&ClusterProviderDecl> {
        self.cluster_providers()
            .into_iter()
            .find(|p| p.name == name)
    }

    /// The transitive co-location closure of `root`, including `root` itself.
    ///
    /// This is the set of gears that must be in the same binary as `root`. It is
    /// a closure, not an equivalence class: two gears sharing a dependency do not
    /// thereby belong together, which is why processes overlap rather than
    /// partition.
    ///
    /// Unknown ids are skipped; reporting them is the resolver's job, and this
    /// needs to stay usable on an incomplete catalogue so a UI can still draw a
    /// partial graph.
    #[must_use]
    pub fn colocation_closure(&self, root: &GearId) -> BTreeSet<GearId> {
        let mut closed = BTreeSet::new();
        let mut queue = BTreeSet::from([root.clone()]);

        while let Some(next) = queue.pop_first() {
            if !closed.insert(next.clone()) {
                continue;
            }
            if let Some(descriptor) = self.gears.get(&next) {
                for dep in &descriptor.colocated_deps {
                    if !closed.contains(dep) {
                        queue.insert(dep.clone());
                    }
                }
            }
        }

        closed
    }

    /// Whether `provider` is inside `consumer`'s co-location closure.
    ///
    /// When it is, the runtime short-circuits to the in-process instance whatever
    /// the configuration says, so the binding is local no matter what anyone asks
    /// for.
    #[must_use]
    pub fn is_colocated_with(&self, consumer: &GearId, provider: &GearId) -> bool {
        self.colocation_closure(consumer).contains(provider)
    }
}
