//! Contracts, their transport projections, and who provides them.
//!
//! The load-bearing fact here is that a contract's **kind is encoded in its Rust
//! trait-name suffix**, and the kind determines whether the contract may cross a
//! process boundary at all. That makes a whole class of placement error
//! statically decidable, which is why this module mirrors the macro's
//! classification rules character for character rather than approximating them.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{ContractId, GearId, IdError, RelPath};

/// What a contract is for, and whether it can leave the process.
///
/// Mirrors `toolkit_contract::descriptor::ContractKind`. The four kinds are the
/// complete set; a trait whose name ends in none of them is a compile error in
/// `gears-rust`, so Gearbox must reject it too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContractKind {
    /// The gear **provides** it, and it may be reached remotely.
    Api,
    /// The gear **provides** it, always in-process.
    Embedded,
    /// The gear **requires** it, and it may be reached remotely.
    Backend,
    /// The gear **requires** it, always in-process.
    Extension,
}

impl ContractKind {
    /// Every kind, in declaration order.
    pub const ALL: &'static [Self] = &[Self::Api, Self::Embedded, Self::Backend, Self::Extension];

    /// The trait-name suffix this kind corresponds to.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Api => "Api",
            Self::Embedded => "Embedded",
            Self::Backend => "Backend",
            Self::Extension => "Extension",
        }
    }

    /// Whether the declaring gear provides this contract.
    #[must_use]
    pub const fn provides(self) -> bool {
        matches!(self, Self::Api | Self::Embedded)
    }

    /// Whether the declaring gear requires this contract.
    #[must_use]
    pub const fn requires(self) -> bool {
        matches!(self, Self::Backend | Self::Extension)
    }

    /// Whether this contract may cross a process boundary.
    ///
    /// This is *the* placement constraint: when false, consumer and provider
    /// must share a process, and a resolver that separates them has produced an
    /// invalid product.
    #[must_use]
    pub const fn remote_capable(self) -> bool {
        matches!(self, Self::Api | Self::Backend)
    }

    /// Classify a Rust trait name by its suffix, ignoring a trailing major
    /// marker so `PaymentApiV2` classifies exactly like `PaymentApi`.
    #[must_use]
    pub fn from_trait_name(name: &str) -> Option<Self> {
        let base = strip_version_suffix(name);
        // Order matters no more than it does in the macro: the four suffixes
        // share no common tail.
        if base.ends_with("Api") {
            Some(Self::Api)
        } else if base.ends_with("Embedded") {
            Some(Self::Embedded)
        } else if base.ends_with("Backend") {
            Some(Self::Backend)
        } else if base.ends_with("Extension") {
            Some(Self::Extension)
        } else {
            None
        }
    }
}

/// Strip a trailing `V<digits>` major marker from a trait name.
///
/// Mirrors `toolkit_contract_macros::support::strip_version_suffix`: digits must
/// actually be present, be preceded by `V`, and leave something in front of that
/// `V` to classify. So `PaymentApiV2` yields `PaymentApi`, while `PaymentApi`,
/// `V2`, and `Api2` are returned unchanged.
#[must_use]
pub fn strip_version_suffix(name: &str) -> &str {
    let without_digits = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if without_digits.len() < name.len()
        && without_digits.len() > 1
        && without_digits.ends_with('V')
    {
        // `V` is ASCII, so trimming one byte lands on a char boundary.
        &without_digits[..without_digits.len() - 1]
    } else {
        name
    }
}

/// The major marker a trait name declares, lowercased (`PaymentApiV2` -> `v2`).
///
/// `None` when the name carries no marker, which the macro permits: an unmarked
/// name is simply unconstrained by the declared version.
#[must_use]
pub fn version_marker(name: &str) -> Option<String> {
    let stripped = strip_version_suffix(name);
    if stripped.len() == name.len() {
        None
    } else {
        Some(name[stripped.len()..].to_ascii_lowercase())
    }
}

/// A contract's declared version.
///
/// `gears-rust` writes this as a string (`version = "v1"`). Gearbox keeps the
/// original spelling alongside the parsed major, because the major is what
/// compatibility is decided on while the spelling is what appears in a REST base
/// path and must be reproduced exactly.
/// The two fields are private and there is no public constructor but [`parse`]
/// and [`from_major`], so a `ContractVersion` in hand is one those two would
/// produce: `declared` really does spell `major`. [`Deserialize`] runs the same
/// parse, because a `product.lock` is an input like any other.
///
/// [`parse`]: ContractVersion::parse
/// [`from_major`]: ContractVersion::from_major
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, TS)]
pub struct ContractVersion {
    /// Exactly as declared, e.g. `v1`.
    declared: String,
    /// The major number compatibility is decided on.
    major: u32,
}

/// The wire shape, deserialized and then re-parsed.
///
/// Named separately rather than deserialized through `parse` on a string,
/// because the serialized form is a two-field table and changing that would
/// break every `product.lock` already written.
#[derive(Deserialize)]
struct ContractVersionWire {
    declared: String,
    major: u32,
}

impl<'de> Deserialize<'de> for ContractVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ContractVersionWire::deserialize(deserializer)?;
        let parsed = Self::parse(&wire.declared).map_err(serde::de::Error::custom)?;
        if parsed.major != wire.major {
            return Err(serde::de::Error::custom(format!(
                "contract version `{}` spells major {}, but the payload says {}",
                wire.declared, parsed.major, wire.major
            )));
        }
        Ok(parsed)
    }
}

impl ContractVersion {
    /// Parse a declared version of the form `v<digits>`.
    ///
    /// # Errors
    /// Returns [`IdError`] when the spelling is not `v` followed by a decimal
    /// major with no leading zero. Non-numeric spellings such as `v1beta1` are
    /// legal in the macro for an unmarked trait name, but Gearbox cannot order
    /// them, so it refuses them rather than guessing.
    pub fn parse(declared: &str) -> Result<Self, IdError> {
        const EXPECTED: &str = "`v` followed by a major number, e.g. v1";
        let malformed = || IdError::Malformed {
            kind: "contract version",
            value: declared.to_owned(),
            expected: EXPECTED,
        };

        let digits = declared.strip_prefix('v').ok_or_else(malformed)?;
        if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) {
            return Err(malformed());
        }
        let major: u32 = digits.parse().map_err(|_| malformed())?;

        Ok(Self {
            declared: declared.to_owned(),
            major,
        })
    }

    /// Build from a major number, spelling it canonically.
    #[must_use]
    pub fn from_major(major: u32) -> Self {
        Self {
            declared: format!("v{major}"),
            major,
        }
    }

    /// The version exactly as declared, e.g. `v1`.
    ///
    /// What a REST base path and a contract id must reproduce; use
    /// [`major`](Self::major) for any comparison.
    #[must_use]
    pub fn declared(&self) -> &str {
        &self.declared
    }

    /// The major number compatibility is decided on.
    #[must_use]
    pub const fn major(&self) -> u32 {
        self.major
    }

    /// Whether a consumer wanting `self` is satisfied by a provider offering
    /// `other`.
    ///
    /// Exact major equality, deliberately: parallel majors coexist by design in
    /// `gears-rust` (two traits, two projections, two registrations), and there
    /// is no adapter between them. Widening here would let the resolver bless a
    /// pairing the runtime cannot wire.
    #[must_use]
    pub const fn satisfied_by(&self, other: &Self) -> bool {
        self.major == other.major
    }
}

impl std::fmt::Display for ContractVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.declared)
    }
}

/// How a contract can be reached.
///
/// The complete set implemented by the runtime's client wiring. `Local` means an
/// in-process instance; the other two are the only wire protocols that exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Local,
    Rest,
    Grpc,
}

impl Transport {
    pub const ALL: &'static [Self] = &[Self::Local, Self::Rest, Self::Grpc];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Rest => "rest",
            Self::Grpc => "grpc",
        }
    }

    /// Whether this transport crosses a process boundary.
    #[must_use]
    pub const fn is_remote(self) -> bool {
        matches!(self, Self::Rest | Self::Grpc)
    }

    /// Whether a severed contract edge can actually use this transport.
    ///
    /// Only REST can. The consumption macro emits a REST resolving client and
    /// has no gRPC branch, so declaring gRPC on a severed edge produces
    /// configuration that silently does nothing. Cross-process gRPC does exist
    /// in the runtime, but only through hand-written wiring the generator does
    /// not produce.
    #[must_use]
    pub const fn usable_on_severed_edge(self) -> bool {
        matches!(self, Self::Rest)
    }
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a contract's REST surface is reachable from outside the product.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum RestVisibility {
    /// Routable from the edge.
    #[default]
    Exposed,
    /// Reachable only from inside the product.
    Internal,
}

/// A contract's REST projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RestProjection {
    /// The route prefix, e.g. `/api/v1/payments`. The declared version must
    /// appear as a segment, which the macro enforces when full coverage is
    /// required.
    pub base_path: String,
    pub visibility: RestVisibility,
    /// Whether the projection asserts method parity with the base trait.
    pub require_full_coverage: bool,
}

/// A contract's gRPC projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GrpcProjection {
    pub package: String,
    pub service: String,
    pub stubs_module: String,
}

/// A Cargo crate a gear or SDK lives in.
///
/// `lib_ident` is mandatory and never derived. A crate with no explicit `[lib]`
/// section takes its library identifier from its package name, so the two differ
/// in practice -- `cf-api-contracts` has library identifier `cf_api_contracts`,
/// not `api_contracts`. Deriving it would silently emit a link line that does
/// not compile.
// Ordered because ExtensionPointDecl carries one and is itself ordered: points
// are sorted for stable output, and a locator inside one has to compare for that
// to hold.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct CargoRef {
    /// The package name, e.g. `cf-gears-payments-audit`.
    pub crate_name: String,

    /// The library identifier used in Rust paths, e.g. `payments_audit`.
    pub lib_ident: String,

    /// Where the crate lives, relative to its **source root**.
    ///
    /// Resolved once, when the description is merged, rather than kept as the
    /// author spelled it. `gear.gdl` writes it relative to its own directory and
    /// may write `../payments-audit-sdk`; a `RelPath` cannot hold that, and
    /// storing the raw spelling would leave every consumer to redo -- and
    /// re-fail -- the same resolution.
    pub path: RelPath,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,

    #[serde(default = "default_true")]
    pub default_features: bool,

    /// The identifiers to keep alive with a `use ... as _;` line.
    ///
    /// Defaults to just `lib_ident`, but may name nested module paths, because a
    /// gear's plugins are separate registrations inside the same crate and each
    /// needs its own line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link: Vec<String>,
}

const fn default_true() -> bool {
    true
}

impl CargoRef {
    /// A crate reference whose link set is just its own library.
    #[must_use]
    pub fn new(crate_name: impl Into<String>, lib_ident: impl Into<String>, path: RelPath) -> Self {
        let lib_ident = lib_ident.into();
        Self {
            crate_name: crate_name.into(),
            lib_ident: lib_ident.clone(),
            path,
            features: Vec::new(),
            default_features: true,
            link: vec![lib_ident],
        }
    }

    /// The identifiers that must appear as `use ... as _;` lines.
    #[must_use]
    pub fn link_idents(&self) -> Vec<&str> {
        if self.link.is_empty() {
            vec![self.lib_ident.as_str()]
        } else {
            self.link.iter().map(String::as_str).collect()
        }
    }
}

/// The package manager a package belongs to.
///
/// Gearbox projects only `Cargo`. The others are located, not read: a gear's
/// SDK in another language is a fact a reader needs, and nothing in the Rust
/// crate records it. Adding an ecosystem is a variant here and a constructor in
/// GDL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Ecosystem {
    Cargo,
    Npm,
    Nuget,
    Go,
}

impl Ecosystem {
    /// The GDL constructor that declares a package of this ecosystem.
    #[must_use]
    pub const fn constructor(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Nuget => "nuget",
            Self::Go => "go",
        }
    }
}

/// A package in any ecosystem, as far as a reader needs it: what it is called
/// and where it lives.
///
/// The Cargo detail a build needs stays on [`CargoRef`]; this is the uniform
/// view, one per SDK, whatever its language.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct PackageRef {
    pub ecosystem: Ecosystem,

    /// The name the ecosystem knows it by: a crate name, an npm package, a
    /// `nuget` id, a Go module path.
    pub name: String,

    /// Where it lives, relative to its source root. Resolved like
    /// [`CargoRef::path`].
    pub path: RelPath,
}

impl From<&CargoRef> for PackageRef {
    fn from(cargo: &CargoRef) -> Self {
        Self {
            ecosystem: Ecosystem::Cargo,
            name: cargo.crate_name.clone(),
            path: cargo.path.clone(),
        }
    }
}

/// A contract, as the product model sees it.
///
/// Rust remains the source of truth for what the contract *is* -- its methods,
/// types, and errors. This describes only what product composition needs: who
/// owns it, which version, whether it can leave the process, and how it projects
/// onto a wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ContractDescriptor {
    pub id: ContractId,

    /// The gear that declares it.
    pub owner: GearId,

    /// The trait name with any major marker stripped, e.g. `PaymentApi`.
    pub base_name: String,

    pub version: ContractVersion,

    pub kind: ContractKind,

    /// The Rust path of the *versioned* trait, e.g. `api_contracts_sdk::PaymentApiV2`.
    /// This is what generated code names, so it must be the real path, marker and all.
    pub rust_path: String,

    /// The crate the trait lives in.
    pub sdk: CargoRef,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest: Option<RestProjection>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc: Option<GrpcProjection>,
}

impl ContractDescriptor {
    /// Whether this contract may cross a process boundary.
    #[must_use]
    pub const fn remote_capable(&self) -> bool {
        self.kind.remote_capable()
    }

    /// The Rust trait identifier, without its module path.
    #[must_use]
    pub fn trait_ident(&self) -> &str {
        self.rust_path
            .rsplit_once("::")
            .map_or(self.rust_path.as_str(), |(_, ident)| ident)
    }

    /// The `snake_case` key the runtime uses for this contract's client wiring.
    ///
    /// The runtime spells this key as `heck`'s `snake_case` of the contract trait
    /// identifier, so this uses `heck` too rather than reimplementing it -- a
    /// key that disagrees by one underscore is a wiring override that silently
    /// never applies.
    #[must_use]
    pub fn wiring_key(&self) -> String {
        use heck::ToSnakeCase;
        self.trait_ident().to_snake_case()
    }
}

/// One gear providing one contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProviderDescriptor {
    pub contract: ContractId,
    pub provider_gear: GearId,

    /// The associated function that builds the in-process implementation.
    ///
    /// Its signature takes the gear context and a policy stack and returns the
    /// contract behind a shared pointer -- notably with no receiver, so it cannot
    /// close over gear state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_factory: Option<String>,

    /// The transports this provider offers. Always contains at least `Local`.
    pub transports: BTreeSet<Transport>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<String>,
}

impl ProviderDescriptor {
    /// Whether this provider can serve a consumer in another process.
    #[must_use]
    pub fn serves_remotely(&self) -> bool {
        self.transports
            .iter()
            .copied()
            .any(Transport::usable_on_severed_edge)
    }
}
