//! The resolved product: derived implementation.
//!
//! The third of the model's three representations, and the only one written by a
//! machine. Serialized as `product.lock`, committed, diffed, and the sole input
//! to every generator -- so no generator ever re-derives a decision, and the Helm
//! output cannot come to disagree with the Cargo output.
//!
//! Two shapes carry most of the meaning. [`Selected`] keeps what was asked for
//! next to what was produced, so a downgrade is visible rather than silent. And
//! [`BindingMechanism`] names the actual code path the runtime will take, rather
//! than an abstraction over it, so the lock can be checked against behaviour.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::catalogue::{ResolvedSource, RuntimeCap};
use crate::contract::{CargoRef, Transport};
use crate::diagnostics::{DiagnosticCode, Diagnostics};
use crate::explain::ProvenanceEdge;
use crate::ids::{ApplicationId, CapabilityId, ContractId, GearId, ProfileId, RelPath, SourceId};
use crate::intent::{BindingMode, Discovery};
use crate::requirement::ClusterPrimitive;

/// The schema version of the lock format.
///
/// A reader that meets an unknown version must refuse rather than guess: a lock
/// half-understood is worse than one not read at all.
pub const LOCK_SCHEMA_VERSION: u32 = 3;

/// What was asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "choice", rename_all = "snake_case")]
pub enum Choice<T> {
    /// Nothing was asked for; the resolver chose.
    Auto,
    /// Something specific was asked for.
    Explicit { value: T },
}

impl<T> Choice<T> {
    #[must_use]
    pub const fn is_auto(&self) -> bool {
        matches!(self, Self::Auto)
    }

    #[must_use]
    pub const fn explicit(&self) -> Option<&T> {
        match self {
            Self::Auto => None,
            Self::Explicit { value } => Some(value),
        }
    }
}

/// What was asked for, alongside why the outcome differs if it does.
///
/// The resolved value lives on the surrounding struct, not in here: it is not
/// optional, whereas a request is. Keeping them adjacent is what turns "you asked
/// for gRPC and got REST" from a surprise into an explanation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Selected<T> {
    pub selected: Choice<T>,

    /// The code explaining why the outcome differs from the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downgraded_by: Option<DiagnosticCode>,
}

impl<T> Selected<T> {
    /// The resolver chose freely.
    #[must_use]
    pub const fn auto() -> Self {
        Self {
            selected: Choice::Auto,
            downgraded_by: None,
        }
    }

    /// A request was made and honoured.
    #[must_use]
    pub const fn honoured(value: T) -> Self {
        Self {
            selected: Choice::Explicit { value },
            downgraded_by: None,
        }
    }

    /// A request was made and could not be honoured.
    #[must_use]
    pub const fn downgraded(value: T, code: DiagnosticCode) -> Self {
        Self {
            selected: Choice::Explicit { value },
            downgraded_by: Some(code),
        }
    }

    /// Whether the outcome differs from the request.
    #[must_use]
    pub const fn was_downgraded(&self) -> bool {
        self.downgraded_by.is_some()
    }
}

/// Whether an application hosts others or is hosted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum ApplicationKind {
    /// Runs the full host runtime, and spawns workers when there are any.
    Host,
    /// Runs the out-of-process runtime: its own HTTP router, probes,
    /// self-registration, and heartbeat.
    Worker,
}

/// Which runtime entry point a generated binary calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Entrypoint {
    /// The host runtime.
    RunServer,
    /// The out-of-process worker runtime.
    RunOopWithOptions,
}

impl Entrypoint {
    #[must_use]
    pub const fn for_kind(kind: ApplicationKind) -> Self {
        match kind {
            ApplicationKind::Host => Self::RunServer,
            ApplicationKind::Worker => Self::RunOopWithOptions,
        }
    }
}

/// Something a process listens on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedEndpoint {
    pub name: String,

    /// The gear that owns the socket.
    pub gear: GearId,

    /// The configuration key carrying the address.
    pub config_key: String,

    pub address: String,

    /// The address other processes should use, when it differs from the bind
    /// address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertise_uri: Option<String>,

    /// Whether a loopback address may be advertised.
    ///
    /// Acceptable on one machine and wrong anywhere else, so it is recorded
    /// rather than assumed.
    #[serde(default)]
    pub allow_loopback_advertise: bool,
}

/// Where a worker serves its own REST surface, and how it advertises it.
///
/// Only a worker has one. A host's REST-contributing gears mount on its REST
/// host; a worker has none -- `GBX0312` refuses one -- and serves through the
/// out-of-process runtime's own listener instead. That listener is configured
/// by a **top-level** `oop_http` section rather than by any gear's key, which is
/// why this cannot ride [`ResolvedApplication::listens`].
///
/// It is also what makes the worker findable at all: the advertised URI is what
/// the runtime registers with the directory, and a severed binding resolved
/// `via directory` has nothing to resolve to without it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WorkerServe {
    /// The address the worker binds.
    pub listen_addr: String,

    /// The address it publishes to the directory.
    pub advertise_uri: String,

    /// Whether a loopback address may be advertised.
    ///
    /// The runtime defaults this to `false` and **refuses to start** when the
    /// advertised host is loopback without it, so a single-machine profile has
    /// to say so rather than leave it implied.
    #[serde(default)]
    pub allow_loopback_advertise: bool,
}

/// A worker a host process starts.
///
/// Mirrors the runtime's per-gear execution configuration exactly, because that
/// is what the generated configuration has to contain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SpawnSpec {
    /// The gear whose runtime kind is set to out-of-process.
    pub gear: GearId,

    /// The binary the host starts, by name.
    ///
    /// **Not a path, and that is the correction.** Where the binary sits depends
    /// on where the tree was generated and on whether a shared Cargo target
    /// directory was declared -- neither of which the resolver knows. It used to
    /// write `{target_dir}/debug/{bin}` here, copying a string the description
    /// had expressed relative to *itself*; the runtime then resolved it relative
    /// to the host's working directory, which is the generated tree. The two
    /// bases differ by one level, so the host looked for the worker in a
    /// directory that does not exist.
    pub bin_name: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

/// A gear as it appears in the resolved product.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedGear {
    pub id: GearId,
    pub source: SourceId,

    /// Where its description was read from, for traceability.
    pub gdl_path: RelPath,

    pub package: CargoRef,

    /// The crate directory relative to its source root, resolved once here so no
    /// generator has to redo it.
    pub crate_dir: RelPath,

    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub runtime_caps: BTreeSet<RuntimeCap>,

    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub colocated_deps: BTreeSet<GearId>,

    /// Why this gear is in the product at all.
    ///
    /// Either the description selected it, or something's co-location closure
    /// pulled it in. Recorded because "why is this even here" is one of the most
    /// common questions about a resolved product.
    pub selected_by: Vec<InclusionReason>,

    /// Cargo features the *product* asked for, on top of what `gear.gdl`
    /// declares in its `cargo(...)`.
    ///
    /// Kept apart from [`Self::package`]`.features` rather than merged into it,
    /// because the two have different provenance and this repository keeps
    /// paying for conflations of exactly that shape: the package's list is
    /// projected from the gear's own description, this one is a decision the
    /// integrator made. The generator unions them; the lock records which was
    /// which.
    ///
    /// It exists because it was missing. `GearSelection.features` was filled
    /// from the description and read by nobody, so the feature checkboxes in
    /// Add Gear wrote into `product.gdl` and changed nothing that was built.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub selected_features: BTreeSet<String>,

    /// The configuration the description set on this gear.
    ///
    /// In the lock because it is part of what was *decided*: two products whose
    /// descriptions differ only in a config value are different products, and a
    /// `lock_hash` that could not tell them apart would be answering about the
    /// wrong one. It is also how the value reaches a generator at all --
    /// `GenerateInput` carries the lock and nothing else.
    ///
    /// Empty for a gear the closure pulled in: only a `use_gear` entry can carry
    /// configuration, and inventing an inherited one would be a decision nobody
    /// wrote down.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, unknown>")]
    pub config: BTreeMap<String, serde_json::Value>,
}

/// How a gear came to be in the product.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum InclusionReason {
    /// Named directly in the product description.
    Selected,
    /// Pulled in by another gear's co-location dependency.
    ColocatedBy { gear: GearId },
    /// Chosen as a plugin of a host gear, for this deployment profile.
    ///
    /// A reason of its own rather than `Selected`, because the product did not
    /// name it on its own terms: it named it *under* a host, and the answer to
    /// "why is this crate in my binary" is that host. The profile is part of the
    /// reason because a plugin selection may be scoped to one -- which makes it
    /// the only thing that can put a gear in one profile's product and not
    /// another's.
    PluginOf { host: GearId, profile: ProfileId },
}

/// A role an application is started in, and the name that role registers under.
///
/// Both halves travel together because every consumer wants both: the generated
/// worker takes `--role <name>` at startup and registers under
/// `directory_name`, and the explanation graph says which is which. Deriving one
/// from the other at each reader would be the same lookup twice.
///
/// **It does not name the Kubernetes Service, and the doc here used to say it
/// did.** Those are two different lookups. `directory_name` is an entry in the
/// gears directory, which is what a bare-name resolve reaches. A Service name is
/// what cluster DNS resolves, and every static endpoint in the lock is built by
/// `resolve::partition::cluster_dns` as
/// `http://{subchart}.{namespace}.svc.cluster.local` -- so the chart is named
/// after the subchart and nothing else, or consumers dial a name that answers
/// nothing. Acting on the old claim broke the demo's `audit` chart on the first
/// run, because `registers_as()` falls back to the *anchor* and `audit` is
/// anchored on `api-contracts-consumer`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApplicationRole {
    /// The value the gear's own mode selector accepts, and what `--role` takes.
    pub name: String,
    /// What an instance started in this role registers under, in the gears
    /// directory. Not a DNS name; see the type's own documentation.
    pub directory_name: String,
}

/// One application in the resolved topology.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedApplication {
    pub name: ApplicationId,
    pub kind: ApplicationKind,

    /// The gear whose co-location closure defines this application.
    ///
    /// The closure key, and only that. It used to double as the worker's
    /// directory identity, which is why roles had nowhere to go; `role` and
    /// `registers_as()` carry that now.
    pub anchor: GearId,

    /// Which of the anchor's declared roles this application runs as.
    ///
    /// Absent for every application that deploys a gear undifferentiated,
    /// which keeps a lock written today byte-identical to one written before
    /// the field existed.
    ///
    /// **A role is an input to the process, not a property of the binary.** One
    /// gear is one crate and one binary whichever roles it runs in, and the
    /// role arrives at startup -- `--role` on the command line, defaulted to
    /// what this application was resolved for. So this field says which role
    /// the generated deployment *starts* it in, and the binary can be started
    /// in any of them (ADR `cpt-gearbox-adr-role-qualified-names`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ApplicationRole>,

    /// The gears in this binary, in dependency order.
    ///
    /// **May overlap other applications.** A gear reached by two closures is linked
    /// into both binaries; that is a consequence of co-location being a closure
    /// rather than a partition, not a mistake.
    pub gears: Vec<GearId>,

    pub replicas: u32,

    pub entrypoint: Entrypoint,

    /// The binary name.
    pub bin_name: String,

    /// The generated crate's package name.
    pub crate_name: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listens: Vec<ResolvedEndpoint>,

    /// The single REST host gear, if this application has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest_host: Option<GearId>,

    /// The single gRPC hub gear, if this application has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc_hub: Option<GearId>,

    #[serde(default)]
    pub needs_db: bool,

    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub cargo_features: BTreeSet<String>,

    /// Workers this application starts. Only a host has any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spawns: Vec<SpawnSpec>,

    /// How this application serves, when it is a worker. Only a worker has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serve: Option<WorkerServe>,

    /// The container image, when the profile builds images.
    ///
    /// A pure function of the lock, which is why it is resolved rather than left
    /// for a template to invent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageRef>,

    /// The chart subdirectory, when the profile generates a chart.
    ///
    /// Equal to the application name. Helm looks for subcharts under `charts/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subchart: Option<String>,

    /// The port neighbours dial for contract traffic, when the profile
    /// generates a chart.
    ///
    /// A projection of `listens` / `serve`, not an independent fact. A process
    /// may listen on several sockets (REST and gRPC); this is the REST one,
    /// because that is the transport a severed declared edge actually carries.
    /// The Service still declares every port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_port: Option<u16>,
}

impl ResolvedApplication {
    #[must_use]
    pub fn contains(&self, gear: &GearId) -> bool {
        self.gears.contains(gear)
    }

    /// The name this application registers under when started as resolved.
    ///
    /// One accessor rather than the rule at each reader: no role means the
    /// anchor's own id, which is what an undifferentiated deployment answers
    /// to. A process started in another role answers to that role's name
    /// instead -- the binary takes it at startup.
    #[must_use]
    pub fn registers_as(&self) -> &str {
        self.role
            .as_ref()
            .map_or_else(|| self.anchor.as_str(), |r| r.directory_name.as_str())
    }

    #[must_use]
    pub const fn is_worker(&self) -> bool {
        matches!(self.kind, ApplicationKind::Worker)
    }

    /// Whether this process runs more than one copy of itself.
    #[must_use]
    pub const fn is_replicated(&self) -> bool {
        self.replicas > 1
    }
}

/// A container image, in the three parts a chart addresses separately.
///
/// **Three fields rather than one string, because the chart needs them apart and
/// the resolver is the only place that still knows where the seams are.** Helm's
/// conventional `global.imageRegistry` re-prefixes every image so a site can
/// mirror them; that composes only if `repository` is the registry-less name.
/// Holding one `registry/repo:tag` string meant the generator had to split it
/// back apart by guessing at punctuation -- and guessing wrong, since a registry
/// may carry a port and a repository may carry slashes. Splitting a string this
/// crate assembled two functions earlier is a smell in any case: the parts were
/// known, and were thrown away.
///
/// `tag` is the product version; a digest is not expressible and is deliberately
/// left to the operator's `image.tag` override rather than half-modelled here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ImageRef {
    /// The registry and any namespace under it, without a trailing slash.
    ///
    /// `None` when the profile named none, which is what an operator building
    /// locally gets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,

    /// The image name, never carrying the registry.
    pub repository: String,

    pub tag: String,
}

impl ImageRef {
    /// The single string `docker build -t` and `docker pull` take.
    #[must_use]
    pub fn reference(&self) -> String {
        match &self.registry {
            Some(registry) => format!("{registry}/{}:{}", self.repository, self.tag),
            None => format!("{}:{}", self.repository, self.tag),
        }
    }
}

/// How a binding is actually established.
///
/// Deliberately names the real code path rather than an abstraction over it, so a
/// reader can check the lock against what the runtime does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum BindingMechanism {
    /// The provider is in the same binary, so the client hub's local lookup
    /// short-circuits. No readiness gate, and no configuration can change it.
    ColocatedLocal,

    /// A declared contract edge resolved through the static endpoint resolver,
    /// reading the consumer's endpoint override from configuration. REST only.
    ConsumesStatic,

    /// A declared contract edge resolved through the directory service.
    ConsumesDirectory,

    /// The provider's client wiring points at a remote address while its crate
    /// stays linked in. Used only when a transport request forces it.
    ProvidesClientWiring,
}

impl BindingMechanism {
    /// The mechanism for a same-process binding.
    #[must_use]
    pub const fn colocated() -> Self {
        Self::ColocatedLocal
    }

    /// The mechanism for a severed edge under `discovery`.
    #[must_use]
    pub const fn for_discovery(discovery: Discovery) -> Self {
        match discovery {
            Discovery::Static => Self::ConsumesStatic,
            Discovery::Directory => Self::ConsumesDirectory,
        }
    }

    /// Whether this mechanism gates the consumer's readiness.
    ///
    /// A local binding does not: the local lookup succeeds immediately. A remote
    /// one does, which is how a critical dependency keeps a process out of
    /// rotation without turning startup into a global ordering problem.
    #[must_use]
    pub const fn gates_readiness(self) -> bool {
        !matches!(self, Self::ColocatedLocal)
    }
}

/// What was requested for a binding, as one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct BindingRequest {
    pub mode: BindingMode,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
}

/// What a binding resolved *to*.
///
/// Two variants where [`BindingMode`] has three, and the missing one is the
/// point: `Auto` is a request to decide, so a resolved binding carrying it would
/// be a lock recording that nothing was decided. Keeping the request enum out of
/// the resolved model makes that state unrepresentable rather than merely
/// unexpected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum ResolvedBindingMode {
    /// Consumer and provider are in the same process.
    Local,
    /// They are in different processes, so the edge crosses a transport.
    Remote,
}

impl ResolvedBindingMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }
}

impl std::fmt::Display for ResolvedBindingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One resolved contract binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedBinding {
    pub consumer: GearId,
    pub consumer_application: ApplicationId,
    pub contract: ContractId,
    pub provider: GearId,
    pub provider_application: ApplicationId,

    /// What the runtime will actually produce. Derived from placement, never
    /// configured (`cpt-gearbox-fr-derive-binding-from-placement`).
    pub mode: ResolvedBindingMode,

    pub transport: Transport,
    pub mechanism: BindingMechanism,

    /// Where the address comes from: a configuration key, a directory lookup, or
    /// nothing at all for a local binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_source: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// Whether the consumer cannot serve without this.
    #[serde(default)]
    pub critical: bool,

    pub selected: Selected<BindingRequest>,
}

impl ResolvedBinding {
    #[must_use]
    pub const fn is_remote(&self) -> bool {
        matches!(self.mode, ResolvedBindingMode::Remote)
    }

    /// Whether this binding keeps the consumer out of rotation until it resolves.
    #[must_use]
    pub const fn gates_readiness(&self) -> bool {
        self.critical && self.mechanism.gates_readiness()
    }
}

/// What a cluster primitive resolved to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "via", rename_all = "kebab-case")]
pub enum ClusterResolution {
    /// A provider the runtime registers.
    Provider { name: String },

    /// The SDK's compare-and-swap default, layered over this scope's cache.
    ///
    /// Not a fallback in the sense of a compromise: it is the intended design
    /// for the primitives that have no dedicated backend, and it is engaged by
    /// leaving the key out of configuration.
    SdkCasDefault { over_cache: String },

    /// No registered provider satisfies the primitive. Recorded so the lock
    /// does not invent an empty provider name that later looks like a choice.
    Unsatisfied,
}

impl ClusterResolution {
    /// The provider whose capabilities actually decide behaviour, if any.
    ///
    /// For the compare-and-swap default that is the underlying cache, not the
    /// primitive being asked about -- which is why a process-local cache makes
    /// leader election process-local too. `Unsatisfied` and an empty explicit
    /// name are `None`, not `""`.
    #[must_use]
    pub fn effective_provider(&self) -> Option<&str> {
        match self {
            Self::Provider { name } if !name.is_empty() => Some(name.as_str()),
            Self::SdkCasDefault { over_cache } => Some(over_cache.as_str()),
            Self::Provider { .. } | Self::Unsatisfied => None,
        }
    }
}

/// One resolved cluster primitive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedClusterBinding {
    /// The cluster scope name.
    pub scope: String,

    pub primitive: ClusterPrimitive,

    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub required_capabilities: BTreeSet<CapabilityId>,

    /// The gears that asked for it.
    pub requesters: Vec<GearId>,

    pub selected: Selected<String>,

    pub resolved: ClusterResolution,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, unknown>")]
    pub options: BTreeMap<String, serde_json::Value>,

    /// A reference to externally managed credentials. Never a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
}

/// Why an edge could not be severed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum CutBlocker {
    /// The provider is inside the consumer's co-location closure, so it is in the
    /// same binary and the local lookup wins regardless of configuration.
    ColocationClosure,

    /// The dependency is not declared as a contract edge -- it is a direct
    /// type-keyed client lookup. Severing it would fail at runtime, and the
    /// resolver cannot see enough to know otherwise.
    UndeclaredHubEdge,

    /// The contract's kind is in-process only.
    InProcessOnlyContract,

    /// The provider declares no transport that a severed edge can carry.
    NoRemoteTransport,

    /// The profile is single-process.
    ProfileForbidsSplit,
}

impl CutBlocker {
    /// Whether declaring the edge would remove this obstacle.
    ///
    /// Only true for the undeclared case: the rest are facts about the code or
    /// the profile that an annotation cannot change.
    #[must_use]
    pub const fn fixable_by_declaring(self) -> bool {
        matches!(self, Self::UndeclaredHubEdge)
    }

    /// Why the edge cannot be cut, as a clause a sentence can be built around.
    ///
    /// A phrase rather than a label, because the one caller is the explanation
    /// graph and a reader there is owed a reason, not a term. It exists at all
    /// because that caller used to interpolate `{:?}` and put
    /// `cannot be separated: [ColocationClosure]` in front of a person.
    #[must_use]
    pub const fn because(self) -> &'static str {
        match self {
            Self::ColocationClosure => {
                "the provider is in the consumer's co-location closure, so the local instance \
                 wins whatever the configuration says"
            }
            Self::UndeclaredHubEdge => {
                "the dependency is a direct type-keyed lookup rather than a declared contract, \
                 so a boundary would fail at run time"
            }
            Self::InProcessOnlyContract => "the contract's kind is in-process only",
            Self::NoRemoteTransport => {
                "the provider declares no transport a severed edge could carry"
            }
            Self::ProfileForbidsSplit => "the profile is a single application",
        }
    }
}

impl std::fmt::Display for CutBlocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.because())
    }
}

/// How much separating an edge would buy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CutSavings {
    /// Gears that would move into their own process.
    pub gears_moved: u32,

    /// Gears that would leave the original binary entirely.
    pub binary_gears_removed: u32,
}

/// An edge the resolver would sever, but cannot.
///
/// This is a deliverable, not an apology: with the great majority of gears wired
/// by co-location and undeclared client lookups, the list of edges that *would*
/// become severable is the actionable path from today's single binary toward one
/// process per gear (`cpt-gearbox-fr-report-cuttable-if-declared`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CutCandidate {
    pub consumer: GearId,
    pub provider: GearId,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<ContractId>,

    pub blocked_by: CutBlocker,

    /// The literal source edit that would remove the obstacle, when one would.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_edit: Option<String>,

    /// The file that edit belongs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<RelPath>,

    #[serde(default)]
    pub estimated_savings: CutSavings,
}

/// Where generated application crates live when the description says nothing.
///
/// `apps/`, not `processes/`: the directory names what it holds, and what it
/// holds is one application per entry (ADR-0016).
pub const DEFAULT_LAYOUT: &str = "apps";

fn default_layout() -> String {
    DEFAULT_LAYOUT.to_owned()
}

/// Whether `segment` may name the generated application directory.
///
/// One path segment, so it cannot climb out of the output root or bury the
/// crates under a path the workspace manifest would have to spell differently.
/// Deliberately permissive about the spelling itself -- `processes` has to keep
/// working for a checkout that already has one.
#[must_use]
pub fn is_valid_layout(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains('/')
        && !segment.contains('\\')
        && !segment.starts_with('.')
}

/// Identity and provenance of a resolved product.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedProductHeader {
    pub id: String,
    pub version: String,

    pub profile: ProfileId,

    /// The profile family: `embedded`, `self-hosted`, or `kubernetes`.
    pub profile_kind: String,

    /// The directory the generated application crates live under, one path
    /// segment. Default [`DEFAULT_LAYOUT`].
    ///
    /// **In the lock rather than only in the intent**, unlike `templates`, and
    /// for a reason the orphan hazard makes concrete. The generator has no
    /// delete path: every fate is create, update, keep or conflict. So a tree
    /// generated under one layout and regenerated under another keeps both, and
    /// the rewritten root `Cargo.toml` lists only the new one -- a package
    /// inside a workspace that neither includes nor excludes it, which is the
    /// state `generate::workspace` exists to prevent. Recording the layout in
    /// the lock, which is written *into* the generated tree, is what lets the
    /// next run see what the last one used and say so.
    ///
    /// It is covered by `lock_hash` as every other field is, so changing the
    /// layout is a change to the lock rather than a silent move.
    #[serde(default = "default_layout")]
    pub layout: String,

    /// Which build produced this, so a stale lock is recognizable.
    pub gearbox_version: String,

    /// A digest of the canonical lock body with this field blanked.
    ///
    /// Makes "did anything actually change" a byte comparison rather than a
    /// judgement (`cpt-gearbox-nfr-determinism`).
    pub lock_hash: String,
}

/// Kubernetes-specific product settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct KubernetesSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_registry: Option<String>,

    pub discovery: Discovery,
}

/// What a `self_hosted` profile decided, beyond the processes themselves.
///
/// Mirrors [`KubernetesSettings`]: a place for the facts the profile declared
/// that every generator needs and no process carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SelfHostedSettings {
    /// The Cargo target directory the operator wants shared, **as the
    /// description spelled it** -- relative to the description, not to anything
    /// generated.
    ///
    /// Kept in the declared form on purpose. Resolving it here would put a
    /// machine-specific absolute path in a file that is committed and diffed;
    /// the generator converts it once, against the output root it alone knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_dir: Option<String>,

    /// Which Cargo profile directory the host should exec (`dev` -> `debug`).
    ///
    /// Absent means `debug`. Kubernetes images stay `release` and do not
    /// read this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_profile: Option<String>,

    pub discovery: Discovery,
}

/// The resolved product. Serialized as `product.lock`.
///
/// Every collection is in canonical order, fixed by the resolver's final pass, so
/// two runs over the same inputs serialize identically.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedProduct {
    /// Explicit per-gear SQLite choices; these gears are excluded from Postgres.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sqlite_databases: BTreeMap<GearId, crate::SqliteDatabase>,
    /// Complete external database configuration, including resolved per-gear names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<crate::PostgresDatabase>,
    pub schema_version: u32,

    pub product: ResolvedProductHeader,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kubernetes: Option<KubernetesSettings>,

    /// Present only for a `self_hosted` profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_hosted: Option<SelfHostedSettings>,

    pub sources: BTreeMap<SourceId, ResolvedSource>,

    pub gears: BTreeMap<GearId, ResolvedGear>,

    /// Ordered by name.
    pub applications: Vec<ResolvedApplication>,

    /// Ordered by consumer then contract.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<ResolvedBinding>,

    /// Ordered by scope then primitive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cluster: Vec<ResolvedClusterBinding>,

    /// Ordered by consumer, provider, then contract.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuttable_if_declared: Vec<CutCandidate>,

    /// Ordered by source, kind, then target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<ProvenanceEdge>,

    #[serde(default, skip_serializing_if = "Diagnostics::is_empty")]
    pub diagnostics: Diagnostics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<crate::ui::ResolvedUi>,
}

impl ResolvedProduct {
    #[must_use]
    pub fn application(&self, name: &ApplicationId) -> Option<&ResolvedApplication> {
        self.applications.iter().find(|p| &p.name == name)
    }

    /// Verify database settings before a lock reaches a generator or runtime.
    ///
    /// # Errors
    /// Returns a reason if a DB gear lacks a resolved name or a connection is unsafe.
    pub fn validate_database(&self) -> Result<(), String> {
        let mut expected: BTreeSet<_> = self
            .gears
            .iter()
            .filter(|(_, gear)| gear.runtime_caps.contains(&RuntimeCap::Db))
            .map(|(id, _)| id)
            .collect();
        for (id, sqlite) in &self.sqlite_databases {
            sqlite.validate()?;
            if !expected.remove(id) {
                return Err(format!(
                    "SQLite override `{id}` is not a resolved database gear"
                ));
            }
            let placements: Vec<_> = self
                .applications
                .iter()
                .filter(|app| app.gears.contains(id))
                .collect();
            if placements.len() != 1 || placements.iter().any(|app| app.replicas != 1) {
                return Err(format!(
                    "SQLite gear `{id}` requires exactly one application with one replica"
                ));
            }
        }
        let Some(db) = &self.database else {
            if expected.is_empty() {
                return Ok(());
            }
            return Err("database-backed gears have no database; configure the profile and run gearbox resolve".to_owned());
        };
        db.validate()?;
        if !expected.is_empty() && self.kubernetes.is_some() && db.existing_secret.is_none() {
            return Err("Kubernetes postgres requires existing_secret".to_owned());
        }
        if db.database_names.keys().collect::<BTreeSet<_>>() != expected {
            return Err(
                "database_names must name exactly the resolved DB gears; run gearbox resolve"
                    .to_owned(),
            );
        }
        Ok(())
    }

    /// The processes a gear is linked into.
    ///
    /// Plural on purpose: a gear reached by two co-location closures is in both
    /// binaries.
    #[must_use]
    pub fn applications_containing(&self, gear: &GearId) -> Vec<&ResolvedApplication> {
        self.applications
            .iter()
            .filter(|p| p.contains(gear))
            .collect()
    }

    /// The single host process, if the topology has one.
    #[must_use]
    pub fn host_application(&self) -> Option<&ResolvedApplication> {
        self.applications
            .iter()
            .find(|p| matches!(p.kind, ApplicationKind::Host))
    }

    /// Bindings that cross a process boundary.
    #[must_use]
    pub fn remote_bindings(&self) -> Vec<&ResolvedBinding> {
        self.bindings.iter().filter(|b| b.is_remote()).collect()
    }

    /// Whether the topology has more than one process.
    #[must_use]
    pub fn is_multi_application(&self) -> bool {
        self.applications.len() > 1
    }

    /// Whether any process runs more than one copy of itself.
    #[must_use]
    pub fn has_replicas(&self) -> bool {
        self.applications
            .iter()
            .any(ResolvedApplication::is_replicated)
    }

    /// Whether this topology needs cross-process coordination.
    ///
    /// The decisive question for cluster provider selection: a process-local
    /// backend is fine for exactly one unreplicated process and silently wrong
    /// otherwise.
    #[must_use]
    pub fn needs_cross_process_coordination(&self) -> bool {
        self.is_multi_application() || self.has_replicas()
    }

    /// Whether writing this lock and generating from it is permitted.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        !self.diagnostics.has_errors()
    }
}
