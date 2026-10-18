//! Product intent: what someone wants.
//!
//! The second of the model's three representations, and the only one a human
//! authors. It says nothing about how the product is realized -- that is derived.
//!
//! Two properties are deliberate. First, there is **no way to state whether a
//! binding is local or remote**: that is a consequence of placement, and offering
//! it as a knob would let a description contradict the topology it asked for.
//! Second, profile-specific choices are expressed by **scoping a declaration to a
//! list of profiles**, not by branching. Data, not control flow -- which is what
//! keeps the description language declarative.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::contract::Transport;
use crate::diagnostics::Location;
use crate::ids::{ApplicationId, ContractId, GearId, ProfileId, RelPath, SourceId};
use crate::requirement::ClusterPrimitive;

/// Where to get a gear's source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SourceDecl {
    /// A local directory, relative to the product description.
    Path {
        at: String,
        /// `crates = registry("crates.io")`: the gears here are also published,
        /// so the generated build takes their crates from this registry at the
        /// versions the checkout declares, and reads only descriptions from the
        /// checkout. `None` is a path dependency on the checkout, as before.
        ///
        /// A declaration of intent, not a guarantee: generation checks each
        /// crate against what was actually published and falls back to the
        /// checkout, loudly, where the two differ.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        crates: Option<String>,
        /// Where `source(...)` was written in the product description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        declared_at: Option<Location>,
    },

    /// A package registry, and the naming convention that turns a gear id into
    /// a package name.
    ///
    /// **The source is the registry, not one package.** Everything a named gear
    /// depends on arrives with it, because a gear's co-location dependencies are
    /// real Cargo dependencies -- so one declaration fetches the closure.
    ///
    /// Immutable in the sense [`SourceDecl::is_immutable`] means: a version
    /// requirement is a range, so what it resolves to is a decision, and the
    /// lock is where that decision is written down.
    Registry {
        /// The registry, as Cargo names it: `crates.io`, or an alternate.
        url: String,
        /// Prepended to a gear id to name its package. `None` means the id is
        /// the package name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        /// Where `source(...)` was written in the product description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        declared_at: Option<Location>,
    },

    /// A Git repository at a pinned reference.
    ///
    /// Pinning is by tag, revision, or branch; the resolved commit is what lands
    /// in the lock, because a branch is not a reproducible input.
    Git {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rev: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        /// Where `source(...)` was written in the product description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        declared_at: Option<Location>,
    },
}

impl SourceDecl {
    /// Where this source was declared, when known.
    ///
    /// The span of the whole `source(...)` call, not of the `at = path(...)`
    /// inside it: `path`, `git` and `registry` take no `Evaluator`, so the
    /// enclosing call is the finest span that exists. See
    /// `cpt-gearbox-adr-gdl-language-server` on why call granularity is the
    /// ceiling here.
    #[must_use]
    pub const fn declared_at(&self) -> Option<&Location> {
        match self {
            Self::Path { declared_at, .. }
            | Self::Registry { declared_at, .. }
            | Self::Git { declared_at, .. } => declared_at.as_ref(),
        }
    }

    /// Whether this reference pins an immutable point in history.
    ///
    /// A branch does not, so a lock built from one is repeatable but not
    /// reproducible.
    #[must_use]
    pub const fn is_immutable(&self) -> bool {
        match self {
            Self::Path { .. } | Self::Registry { .. } => false,
            Self::Git { rev, tag, .. } => rev.is_some() || tag.is_some(),
        }
    }
}

/// How a consumer finds a remote provider's address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Discovery {
    /// Addresses are pinned in configuration and read by the static endpoint
    /// resolver.
    ///
    /// The only mechanism available under Kubernetes, because no cluster-native
    /// endpoint resolver exists.
    Static,

    /// Addresses come from the directory service over gRPC.
    ///
    /// Requires the directory server and the gRPC hub in the host process: the
    /// host's worker-spawn phase waits for the hub's endpoint in order to hand it
    /// to each child.
    Directory,
}

impl Discovery {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Directory => "directory",
        }
    }

    /// Whether this mechanism needs the directory server co-resident with the host.
    #[must_use]
    pub const fn needs_directory(self) -> bool {
        matches!(self, Self::Directory)
    }
}

/// A deployment profile, as declared.
///
/// A composition-time concept: the runtime has no such type, only a per-gear
/// runtime kind. Each variant is projected onto that plus a deployment topology.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "profile", rename_all = "snake_case")]
pub enum DeploymentProfileDecl {
    /// Everything in one process. Every contract binding is local by construction.
    Embedded {
        id: ProfileId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        database: Option<crate::PostgresDatabase>,
        /// Explicit per-gear SQLite files relative to the gear home.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        sqlite_databases: BTreeMap<GearId, crate::SqliteDatabase>,
        /// Where `embedded(...)` was written in the product description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        declared_at: Option<Location>,
    },

    /// One host process that spawns worker processes.
    ///
    /// Workers are local operating-system processes; no other spawn backend is
    /// implemented, so this profile is one machine.
    SelfHosted {
        id: ProfileId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        database: Option<crate::PostgresDatabase>,
        /// Explicit per-gear SQLite files relative to the gear home.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        sqlite_databases: BTreeMap<GearId, crate::SqliteDatabase>,
        /// Which application is the host.
        host: ApplicationId,
        discovery: Discovery,
        /// Where worker binaries will be built, needed to write each worker's
        /// executable path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_dir: Option<String>,
        /// Which Cargo profile directory the host should exec (`dev` -> `debug`).
        ///
        /// Recorded as the description spelled it so two generates of the same
        /// product cannot drift with the operator's environment.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cargo_profile: Option<String>,
        /// Where `self_hosted(...)` was written in the product description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        declared_at: Option<Location>,
    },

    /// One container image and workload per process.
    Kubernetes {
        id: ProfileId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        database: Option<crate::PostgresDatabase>,
        /// Explicit per-gear SQLite files relative to the gear home.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        sqlite_databases: BTreeMap<GearId, crate::SqliteDatabase>,
        discovery: Discovery,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_registry: Option<String>,
        /// Where `kubernetes(...)` was written in the product description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        declared_at: Option<Location>,
    },
}

impl DeploymentProfileDecl {
    /// SQLite overrides selected by this deployment profile.
    #[must_use]
    pub const fn sqlite_databases(&self) -> &BTreeMap<GearId, crate::SqliteDatabase> {
        match self {
            Self::Embedded {
                sqlite_databases, ..
            }
            | Self::SelfHosted {
                sqlite_databases, ..
            }
            | Self::Kubernetes {
                sqlite_databases, ..
            } => sqlite_databases,
        }
    }

    /// The external database selected by this deployment profile.
    #[must_use]
    pub const fn database(&self) -> Option<&crate::PostgresDatabase> {
        match self {
            Self::Embedded { database, .. }
            | Self::SelfHosted { database, .. }
            | Self::Kubernetes { database, .. } => database.as_ref(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> &ProfileId {
        match self {
            Self::Embedded { id, .. }
            | Self::SelfHosted { id, .. }
            | Self::Kubernetes { id, .. } => id,
        }
    }

    /// Where this profile was declared in the product description, when known.
    #[must_use]
    pub const fn declared_at(&self) -> Option<&Location> {
        match self {
            Self::Embedded { declared_at, .. }
            | Self::SelfHosted { declared_at, .. }
            | Self::Kubernetes { declared_at, .. } => declared_at.as_ref(),
        }
    }

    /// The profile family, as it appears in the lock.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Embedded { .. } => "embedded",
            Self::SelfHosted { .. } => "self-hosted",
            Self::Kubernetes { .. } => "kubernetes",
        }
    }

    /// Whether this profile can have more than one process.
    #[must_use]
    pub const fn is_multi_application(&self) -> bool {
        !matches!(self, Self::Embedded { .. })
    }

    /// How remote addresses are found, if there can be any.
    #[must_use]
    pub const fn discovery(&self) -> Option<Discovery> {
        match self {
            Self::Embedded { .. } => None,
            Self::SelfHosted { discovery, .. } | Self::Kubernetes { discovery, .. } => {
                Some(*discovery)
            }
        }
    }

    /// Where worker binaries are built, when this profile spawns any.
    ///
    /// Only `self_hosted` has one: it is the directory the host builds an
    /// absolute executable path from. Kubernetes runs images an operator
    /// deploys, so there is nothing local to point at.
    #[must_use]
    pub fn target_dir(&self) -> Option<&str> {
        match self {
            Self::SelfHosted { target_dir, .. } => target_dir.as_deref(),
            Self::Embedded { .. } | Self::Kubernetes { .. } => None,
        }
    }
}

/// One scalar a typed configuration control can write.
///
/// Deliberately narrower than the `serde_json::Value` a description may hold. A
/// hand-written `config = {...}` can nest a list or a map and the evaluator
/// accepts it, but the surgical editor cannot: a nested literal has no control
/// to render it and no merge rule that preserves the comments inside it.
/// Narrowing here makes that refusal a deserialization failure at the wire
/// rather than an arm in the renderer somebody has to remember.
///
/// Untagged, because on the wire these *are* the JSON scalars the evaluator
/// already reads. A tag would state the type a second time, and two statements
/// of one fact are how a client ends up sending a bool labelled as a string.
///
/// **Variant order is load-bearing.** `Bool` before `Int` so `true` does not
/// arrive as `1`; `Int` before `Float` so `8087` stays an integer instead of
/// becoming `8087.0` in the description.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum ConfigValue {
    Bool(bool),
    /// `i64` in Rust so a hand-written description may hold any integer, but
    /// `number` on the wire: ts-rs would otherwise emit `bigint`, which
    /// `JSON.stringify` refuses outright. The cost is the usual JSON one --
    /// integers past 2^53 do not round-trip through a browser -- and a config
    /// key holding one would be remarkable.
    Int(#[ts(type = "number")] i64),
    Float(f64),
    Str(String),
}

impl std::fmt::Display for ConfigValue {
    /// The GDL literal that reads back as this value.
    ///
    /// Starlark spells its booleans `True`/`False` -- they are globals, not
    /// literals, so its AST has no `Bool` variant at all -- and numerals are
    /// bare. Only the string arm needs quoting, and it is the caller's
    /// `quote_string` that does it, so escaping lives in one place.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bool(true) => f.write_str("True"),
            Self::Bool(false) => f.write_str("False"),
            Self::Int(i) => write!(f, "{i}"),
            // `{:?}` rather than `{}`: `Display` prints `8087` for `8087.0`,
            // which is an integer literal in Starlark and would silently retype
            // the field on the next read. `Debug` for `f64` is specified to
            // round-trip, which is the property wanted here.
            #[allow(
                clippy::use_debug,
                reason = "Debug is the only float formatter specified to round-trip; \
                          Display drops the fractional part of a whole float and \
                          would retype the field"
            )]
            Self::Float(v) => write!(f, "{v:?}"),
            Self::Str(s) => f.write_str(s),
        }
    }
}

/// A gear someone asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GearSelection {
    pub gear: GearId,

    /// Which declared source to read it from.
    pub source: SourceId,

    /// The version requirement, when the source is a registry.
    ///
    /// A *requirement*, not a version: what it resolves to is decided by cargo
    /// and recorded in the lock, the same way a `Cargo.toml` range and a
    /// `Cargo.lock` entry differ. Meaningless for a path source, and reported as
    /// such rather than ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// The package name, when the source's prefix does not produce it.
    ///
    /// The escape hatch for a gear that does not follow the house naming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,

    /// Extra Cargo features to enable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,

    /// Opaque per-gear runtime configuration, merged into the generated config.
    ///
    /// An ordered map, not `serde_json::Map`, because iteration order reaches the
    /// generated configuration file and must be stable. Values are passed
    /// through untouched; nulls are rejected during evaluation, since the lock is
    /// TOML and TOML has no null.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, unknown>")]
    pub config: BTreeMap<String, serde_json::Value>,

    /// Implementations chosen for this gear's plugin extension points.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PluginSelection>,

    /// Where `use_gear(...)` was written in the product description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<Location>,
}

/// One plugin implementation chosen for a host gear.
///
/// Which extension point it implements is a catalogue fact, not recorded here: the
/// product names an implementing gear and the catalogue says what that gear
/// implements. Recording it twice would let the two disagree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PluginSelection {
    pub gear: GearId,

    /// Per-plugin configuration. `vendor` and `priority` here override the
    /// crate's compiled-in defaults, and overriding one side without the other
    /// is what makes a host resolve nothing.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, unknown>")]
    pub config: BTreeMap<String, serde_json::Value>,

    /// Profiles this choice applies to. Empty means every profile.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub profiles: BTreeSet<ProfileId>,

    /// This entry's position in the host's written `plugins = [...]` list.
    ///
    /// **The written position, not this vector's.** Evaluation drops entries --
    /// a plugin id that does not parse, a duplicate selection for one profile --
    /// so the nth `PluginSelection` is not in general the nth `plugin(...)` in
    /// the file. An editor that addressed entries by their position here would
    /// aim at the wrong one the moment a malformed sibling existed, which is
    /// exactly when a person needs to edit the others.
    ///
    /// Carried rather than recomputed because only evaluation still knows both
    /// orders at once; by the time the client has an intent, the dropped entries
    /// are gone without trace.
    #[serde(default)]
    pub entry_index: usize,

    /// Where `plugin(...)` was written in the product description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<Location>,
}

impl PluginSelection {
    /// The vendor this selection asks for, if the product set one.
    ///
    /// # Errors
    /// Returns an error when the key is present but not a string.
    pub fn configured_vendor(&self) -> Result<Option<&str>, &'static str> {
        match self.config.get("vendor") {
            None => Ok(None),
            Some(serde_json::Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err("`vendor` must be a string"),
        }
    }

    /// The priority this selection asks for, if the product set one.
    ///
    /// # Errors
    /// Returns an error when the key is present but not an integer.
    pub fn configured_priority(&self) -> Result<Option<i64>, &'static str> {
        match self.config.get("priority") {
            None => Ok(None),
            Some(serde_json::Value::Number(n)) => {
                n.as_i64().map(Some).ok_or("`priority` must be an integer")
            }
            Some(_) => Err("`priority` must be an integer"),
        }
    }
}

/// What someone asked for regarding one binding.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum BindingMode {
    /// Let the resolver decide.
    #[default]
    Auto,
    /// Keep consumer and provider together.
    Local,
    /// Separate them.
    Remote,
}

/// A request about one contract edge.
///
/// Note what is absent: there is no way to declare a binding *is* local or
/// remote, only to ask for it. The resolver decides, and records both.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct BindingIntent {
    pub consumer: GearId,
    pub contract: ContractId,

    #[serde(default)]
    pub mode: BindingMode,

    /// A transport preference. Honoured only if the provider offers it and the
    /// edge can actually carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,

    /// A pinned address, overriding whatever discovery would produce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// Profiles this applies to. Empty means all of them.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub profiles: BTreeSet<ProfileId>,

    /// Where `bind(...)` was written in the product description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<Location>,
}

/// A cluster provider choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProviderBinding {
    /// The provider name, which must be one the runtime registers.
    pub provider: String,

    /// Backend-specific settings, passed through verbatim.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, unknown>")]
    pub options: BTreeMap<String, serde_json::Value>,

    /// A reference to externally managed credentials. Never a credential itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,

    /// Where `provider(...)` was written in the product description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<Location>,
}

/// A cluster scope's provider bindings.
///
/// The cache is the anchor and must be bound; leaving the other two unbound is
/// what engages the SDK's compare-and-swap default over that cache, so an absent
/// entry is a meaningful choice rather than an omission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ClusterScopeIntent {
    /// The scope name. The runtime calls this a cluster profile; renamed so it
    /// cannot be confused with a deployment profile.
    pub scope: String,

    pub cache: ProviderBinding,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader_election: Option<ProviderBinding>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<ProviderBinding>,

    /// Profiles this applies to. Empty means all of them.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub profiles: BTreeSet<ProfileId>,

    /// Where `cluster_profile(...)` was written in the product description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<Location>,

    /// The entry's position in the written `cluster_profiles` list.
    ///
    /// **Carried rather than derived, and the difference is a real edit going
    /// to the wrong place.** An option is addressed by written position -- two
    /// entries can share a `name` for disjoint deployment profiles, which
    /// `payments-demo` does -- and the obvious way to get that position is the
    /// index in this list. It is not the same number: a scope bound twice in one
    /// profile is reported and **skipped**, so from that point on this list is
    /// shorter than the one in the file. Reading the position off this array
    /// would then address the entry after the one on screen.
    #[serde(default)]
    pub entry_index: usize,
}

impl ClusterScopeIntent {
    /// The binding for `primitive`, if one was made.
    #[must_use]
    pub const fn binding(&self, primitive: ClusterPrimitive) -> Option<&ProviderBinding> {
        match primitive {
            ClusterPrimitive::Cache => Some(&self.cache),
            ClusterPrimitive::LeaderElection => self.leader_election.as_ref(),
            ClusterPrimitive::Lock => self.lock.as_ref(),
        }
    }
}

/// An explicitly requested application.
///
/// Names, replicates or adds process-local services to an application; the
/// resolver derives the partition on its own otherwise.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApplicationPin {
    pub name: ApplicationId,

    /// The gear whose co-location closure this application is built from.
    pub anchor: GearId,

    /// Additional gears from the product closure to co-locate in this process.
    /// Their dependencies and selected plugins are included as well.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub include: BTreeSet<GearId>,

    /// Which of the anchor's declared roles this application runs as.
    ///
    /// Not a `GearId`: a role's *name* is the value the gear's own mode
    /// selector accepts, and its directory name is a separate field on the
    /// declaration. Absent means the gear is deployed undifferentiated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    #[serde(default = "one")]
    pub replicas: u32,

    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub profiles: BTreeSet<ProfileId>,

    /// Where `application(...)` was written in the product description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<Location>,
}

const fn one() -> u32 {
    1
}

/// A tie-breaker among choices that are all valid.
///
/// Distinct from a constraint, which decides validity. A preference may only
/// order candidates that already satisfy every hard requirement, so no preference
/// can ever make an invalid product valid.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(tag = "prefer", rename_all = "snake_case")]
pub enum Preference {
    /// Favour a provider already in use elsewhere in the same scope, rather than
    /// introducing another dependency.
    ExistingInfrastructure,

    /// Keep gears together when the choice is otherwise free.
    FewerApplications,

    /// Give this gear its own process when that is possible.
    Isolate { gear: GearId },
}

/// What someone wants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProductIntent {
    pub id: String,
    pub display_name: String,
    pub version: String,

    /// Where the product description was read from.
    pub gdl_path: RelPath,

    pub sources: BTreeMap<SourceId, SourceDecl>,

    /// The product's template overlay directory, relative to the description.
    ///
    /// **Declared so it can point outside the product.** The overlay was found by
    /// convention alone -- a `templates/` directory beside `product.gdl` -- which
    /// works for one product and forces a house that keeps twenty to hold twenty
    /// copies of the same chart. A path names one directory the whole fleet can
    /// share.
    ///
    /// Absent keeps the convention, and the convention's forgiveness with it: an
    /// absent `templates/` is the ordinary case, not an error. A path written
    /// here is the opposite -- someone meant it, so a directory that is not there
    /// is reported rather than silently ignored.
    ///
    /// Only `path(...)` is expressible. `git(...)` would make generation fetch,
    /// and `generate` is a pure function of the lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub templates: Option<String>,

    /// Which directory the generated application crates go under.
    ///
    /// Product-level rather than profile-level: a directory name is a fact
    /// about the tree, and one description produces one tree shape whichever
    /// profile it resolves for. Absent means
    /// [`DEFAULT_LAYOUT`](crate::resolved::DEFAULT_LAYOUT).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,

    pub profiles: BTreeMap<ProfileId, DeploymentProfileDecl>,

    pub default_profile: ProfileId,

    /// The gears asked for directly. Their co-location closures bring in more.
    pub selected_gears: Vec<GearSelection>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<BindingIntent>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cluster_scopes: Vec<ClusterScopeIntent>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub application_pins: Vec<ApplicationPin>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferences: Vec<Preference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<crate::ui::ProductUi>,
}

impl ProductIntent {
    #[must_use]
    pub fn profile(&self, id: &ProfileId) -> Option<&DeploymentProfileDecl> {
        self.profiles.get(id)
    }

    /// Whether a declaration scoped to `scoped_to` applies under `profile`.
    ///
    /// An empty scope means every profile. This one predicate is what replaces
    /// conditionals in the description language.
    #[must_use]
    pub fn applies(scoped_to: &BTreeSet<ProfileId>, profile: &ProfileId) -> bool {
        scoped_to.is_empty() || scoped_to.contains(profile)
    }

    /// The binding requests that apply under `profile`.
    #[must_use]
    pub fn bindings_for(&self, profile: &ProfileId) -> Vec<&BindingIntent> {
        self.bindings
            .iter()
            .filter(|b| Self::applies(&b.profiles, profile))
            .collect()
    }

    /// The cluster scopes that apply under `profile`.
    #[must_use]
    pub fn cluster_scopes_for(&self, profile: &ProfileId) -> Vec<&ClusterScopeIntent> {
        self.cluster_scopes
            .iter()
            .filter(|c| Self::applies(&c.profiles, profile))
            .collect()
    }

    /// The process pins that apply under `profile`.
    #[must_use]
    pub fn application_pins_for(&self, profile: &ProfileId) -> Vec<&ApplicationPin> {
        self.application_pins
            .iter()
            .filter(|p| Self::applies(&p.profiles, profile))
            .collect()
    }

    /// Whether `preference` was asked for.
    #[must_use]
    pub fn prefers(&self, preference: &Preference) -> bool {
        self.preferences.contains(preference)
    }

    /// Whether a specific gear was asked to be isolated.
    #[must_use]
    pub fn isolates(&self, gear: &GearId) -> bool {
        self.preferences
            .iter()
            .any(|p| matches!(p, Preference::Isolate { gear: g } if g == gear))
    }
}
