//! A product's infrastructure services (GDL 1.6): third-party services --
//! Keycloak, PostgreSQL -- a product runs beside its gears, described by a
//! `service.gdl` passport with `kind = "infra"` and chosen by a product with
//! `product(infra = [use_service(...)])`.
//!
//! Gearbox installs none of them. It reads the passports, checks a product's
//! choice against them, binds what each provides to the variables a gear's
//! configuration reads (`oidc.issuer` is `${OIDC_ISSUER}`) and records the
//! choice in the lock, so the deployment that installs them (Studio's) and the
//! processes that read them agree on one name per value.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{GearId, ProfileId, RelPath, SourceId};

/// What a `service.gdl` says it is.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum ServiceKind {
    /// A service of the organization's own, built from its own Dockerfile.
    #[default]
    Own,
    /// A third-party service a product runs beside its gears: pulled, never built.
    Infra,
}

impl ServiceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Infra => "infra",
        }
    }
}

/// A `service.gdl`: a service that is not a gear, in one call.
///
/// An own service's `provides` is a list of capability keys, recorded here
/// with empty templates; an infra service's is a dict of capability to the
/// template of its value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ServicePassport {
    /// Its id, kebab-case.
    pub name: String,
    pub kind: ServiceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The image: an own service's without a tag (a build tags it), an infra
    /// service's as its publisher names it, with a tag or a digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Own only: its Dockerfile and build context, relative to the passport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Each port it listens on, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ports: BTreeMap<String, u16>,
    /// The route that answers when it is healthy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    /// Own only: the variables it reads, each to what fills it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// The platform gears it calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// Each capability it provides, to the template of its value (`""` when
    /// only the key is listed).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provides: BTreeMap<String, String>,
    /// Its documents, relative to the passport.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<String>,
    /// Infra only: the container's arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Infra only: variables with literal values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, String>,
    /// Infra only: variables whose values the deployment makes and keeps.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    /// Infra only: it keeps data, on a volume of its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stateful: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_path: Option<String>,
    /// Infra only: a ready chart it may be installed from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart_version: Option<String>,
}

/// One `service.gdl` the catalogue found under a source root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ServiceDescriptor {
    pub source: SourceId,
    /// The passport, relative to its source root.
    pub gdl_path: RelPath,
    /// BLAKE3 of the passport's text: the lock records which passport a
    /// product was resolved against.
    pub passport_hash: String,
    pub passport: ServicePassport,
}

/// `use_service("keycloak", source = ..., ...)`: one infra service a product
/// runs beside its gears.
///
/// What the product is, not where it runs: the publisher's image or chart it
/// chooses and the passport's configuration it changes. Where the service
/// runs -- with the product or in the organization's shared place -- and the
/// copy of the image an environment's registry holds belong to the
/// environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ServiceSelection {
    /// The passport's `name`.
    pub name: String,
    /// The source the passport is found in.
    pub source: SourceId,
    /// The publisher's image, instead of the passport's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// A ready chart, instead of the passport's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart_version: Option<String>,
    /// Values for keys of the passport's `config`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, String>,
    /// Per profile, merged over `config`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profile_config: BTreeMap<ProfileId, BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<crate::Location>,
}

/// An image as the product chose it, and its digest when known.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct LockedImage {
    /// The publisher's reference, as written (`quay.io/keycloak/keycloak:26.0.7`).
    pub reference: String,
    /// `sha256:...`: from the reference when it names one, else absent until
    /// the deployment that pulls it records one. Gearbox does not reach a
    /// registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

impl LockedImage {
    /// The reference, with the digest it carries split out.
    #[must_use]
    pub fn new(reference: &str) -> Self {
        let digest = reference
            .split_once('@')
            .map(|(_, digest)| digest.to_owned())
            .filter(|d| d.starts_with("sha256:"));
        Self {
            reference: reference.to_owned(),
            digest,
        }
    }
}

/// One infra service as resolved for a profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedService {
    pub name: String,
    pub source: SourceId,
    pub gdl_path: RelPath,
    pub passport_hash: String,
    /// The passport as found.
    pub passport: ServicePassport,
    /// The image to run: the product's choice, else the passport's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<LockedImage>,
    /// The ready chart: the product's choice, else the passport's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart_version: Option<String>,
    /// The passport's `config` with the product's values (and this profile's)
    /// over it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, String>,
    /// What the product changed: the keys of `config` it set.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub overridden: BTreeSet<String>,
}

/// One capability a declared infra service provides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProvidedCapability {
    /// The service that provides it.
    pub service: String,
    /// The variable a gear's configuration reads it as: `OIDC_ISSUER`.
    pub env: String,
    /// The passport's template of its value, resolved at deploy.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub template: String,
}

/// Something the product's gears need running beside them, and the declared
/// service that meets it, if one does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct InfraNeed {
    /// `postgres`, `oidc`, `keycloak`, `clickhouse`, `timescaledb`.
    pub need: String,
    /// The capability that meets it: `postgres`, `oidc.issuer`, ...
    pub capability: String,
    /// The gears that need it.
    pub gears: BTreeSet<GearId>,
    /// The declared service that provides `capability`. Absent is not an
    /// error: the need may be met outside the product.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub met_by: Option<String>,
}

/// A product's infrastructure, as resolved for a profile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResolvedInfra {
    /// In the order the product lists them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ResolvedService>,
    /// Capability -> the service that provides it and its variable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub capabilities: BTreeMap<String, ProvidedCapability>,
    /// What the gears need, and which service meets each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<InfraNeed>,
}

impl ResolvedInfra {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.services.is_empty() && self.needs.is_empty()
    }
}

/// The variable a gear's configuration reads a capability as:
/// `oidc.issuer` -> `OIDC_ISSUER`, `keycloak.admin-url` -> `KEYCLOAK_ADMIN_URL`.
#[must_use]
pub fn capability_variable(capability: &str) -> String {
    capability
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Whether `s` is a capability key: `oidc.issuer`, `database.url`, `postgres`.
#[must_use]
pub fn is_capability(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && !s.ends_with('.')
}

/// What each need of the engine's binds to: a gear that keeps data needs
/// `postgres`; the plugins that talk to a service of their own, by the end of
/// their gear id, need that service's capability.
pub const NEED_BINDINGS: &[(&str, &str, &str)] = &[
    // (need, capability, gear-id suffix; "" for a gear with `cap.db`)
    ("postgres", "postgres", ""),
    ("oidc", "oidc.issuer", "oidc-authn-plugin"),
    ("keycloak", "keycloak.admin_url", "keycloak-idp-plugin"),
    (
        "clickhouse",
        "clickhouse.url",
        "clickhouse-usage-collector-plugin",
    ),
    (
        "timescaledb",
        "timescaledb.url",
        "timescaledb-usage-collector-plugin",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capability_is_read_as_its_variable() {
        assert_eq!(capability_variable("oidc.issuer"), "OIDC_ISSUER");
        assert_eq!(
            capability_variable("keycloak.admin_url"),
            "KEYCLOAK_ADMIN_URL"
        );
        assert_eq!(capability_variable("database.url"), "DATABASE_URL");
        assert_eq!(capability_variable("postgres"), "POSTGRES");
    }

    #[test]
    fn an_image_digest_is_split_out_of_its_reference() {
        let pinned = LockedImage::new("quay.io/keycloak/keycloak:26.0.7@sha256:abc");
        assert_eq!(pinned.digest.as_deref(), Some("sha256:abc"));
        assert_eq!(
            LockedImage::new("quay.io/keycloak/keycloak:26.0.7").digest,
            None
        );
    }

    #[test]
    fn capability_keys() {
        assert!(is_capability("oidc.issuer"));
        assert!(!is_capability("OIDC"));
        assert!(!is_capability("oidc."));
    }
}
