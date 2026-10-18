//! `<layout>/<p>/Cargo.toml`.
//!
//! Every inherited field is written out as a literal. The generated crates sit
//! under `.gearbox/<product>/<profile>/`, which is outside the `gears-rust`
//! workspace, so `edition.workspace = true` -- the spelling every crate this
//! output links against uses -- resolves to nothing here. That is the single
//! most likely way for a generated manifest to fail, and it fails at parse time
//! with a message about workspace inheritance rather than about editions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gearbox_ir::{FileEntry, FileKind, GearId, Ownership, ResolvedApplication, ResolvedGear};
use serde::Serialize;

use super::{GenerateError, GenerateInput, header, paths, workspace};

/// Where the platform SDK lives inside a source root.
pub const TOOLKIT_SUBDIR: &str = "libs/toolkit";

/// The Cargo package name and the alias the generated code uses for it.
pub const TOOLKIT_PACKAGE: &str = "cf-gears-toolkit";
pub const TOOLKIT_ALIAS: &str = "toolkit";

/// The toolkit crate under the first of `roots` that has one, checked by its
/// manifest rather than by the directory existing.
///
/// For the one caller that has no anchor gear to resolve it through: a new
/// gear's scaffold, which wants a dependency on the platform SDK and has only
/// the session's roots to find it in. The first root wins, as it does for a
/// gear declared under two roots (ADR-0012). A directory at `libs/toolkit`
/// whose manifest names another package is not the toolkit, and answering it
/// would write a dependency on the wrong crate.
#[must_use]
pub fn locate_toolkit<'a>(roots: impl IntoIterator<Item = &'a Path>) -> Option<PathBuf> {
    roots
        .into_iter()
        .map(|root| root.join(TOOLKIT_SUBDIR))
        .find(|dir| {
            gearbox_project::project_manifest(dir)
                .is_ok_and(|m| m.package_name == TOOLKIT_PACKAGE && m.lib_ident == TOOLKIT_ALIAS)
        })
}

/// The runtime crates a generated host binary needs, with the versions
/// `gears-rust` resolves.
///
/// Pinned to the platform's own `[workspace.dependencies]` rather than to this
/// repository's, because the generated crate is compiled against `gears-rust`
/// sources: a `tokio` that unified differently from the one the toolkit expects
/// is a runtime failure, not a build one.
fn runtime_deps() -> [(&'static str, Dependency); 3] {
    [
        ("anyhow", Dependency::version("1.0", &[])),
        ("clap", Dependency::version("4.5", &["derive"])),
        ("tokio", Dependency::version("1.47", &["full"])),
    ]
}

#[derive(Serialize)]
struct Manifest {
    package: Package,
    #[serde(rename = "bin")]
    bins: Vec<Bin>,
    dependencies: BTreeMap<String, Dependency>,
}

#[derive(Serialize)]
struct Package {
    name: String,
    version: String,
    edition: &'static str,
    #[serde(rename = "rust-version")]
    rust_version: &'static str,
    publish: bool,
}

#[derive(Serialize)]
struct Bin {
    name: String,
    path: &'static str,
}

#[derive(Clone, Serialize)]
struct Dependency {
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    features: Vec<String>,
    #[serde(rename = "default-features", skip_serializing_if = "Option::is_none")]
    default_features: Option<bool>,
}

impl Dependency {
    /// A registry dependency.
    fn version(version: &'static str, features: &[&str]) -> Self {
        Self {
            package: None,
            version: Some(version.to_owned()),
            registry: None,
            path: None,
            features: features.iter().map(|f| (*f).to_owned()).collect(),
            default_features: None,
        }
    }
}

/// The manifest for one host process crate.
///
/// # Errors
/// Returns [`GenerateError`] when a gear names a source the caller gave no path
/// for, when a gear is absent from the lock's gear table, or when two gears in
/// the process share a library identifier -- each of which would produce a
/// manifest that does not build.
pub fn application_manifest(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
) -> Result<FileEntry, GenerateError> {
    let crate_dir = input
        .out_root
        .join(input.layout())
        .join(application.name.as_str());
    let dependencies = dependencies(input, application, &crate_dir)?;

    let manifest = Manifest {
        package: Package {
            name: application.crate_name.clone(),
            version: input.lock.product.version.clone(),
            edition: workspace::package_edition(),
            rust_version: workspace::package_rust_version(),
            publish: false,
        },
        bins: vec![Bin {
            name: application.bin_name.clone(),
            path: "src/main.rs",
        }],
        dependencies,
    };

    let body = toml::to_string_pretty(&manifest).map_err(|source| GenerateError::Toml {
        what: "an application manifest",
        source,
    })?;

    Ok(FileEntry::text(
        paths::rel(&[input.layout(), application.name.as_str(), "Cargo.toml"])?,
        format!("{}\n{body}", header("#")),
        FileKind::Toml,
        Ownership::Generated,
    ))
}

/// Every dependency the process crate declares, keyed by the identifier the
/// generated Rust will name it with.
///
/// Keyed by `lib_ident` and not by `crate_name`, because the key of a Cargo
/// dependency is what `use <key> as _;` has to spell. `cf-api-contracts` has no
/// `[lib]` section, so its identifier is `cf_api_contracts`, and a generator
/// that derived the key from the package name would emit `cf_api_contracts` in
/// one file and `cf-api-contracts` in the other.
fn dependencies(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
    crate_dir: &Path,
) -> Result<BTreeMap<String, Dependency>, GenerateError> {
    let mut dependencies = BTreeMap::new();

    // The SDK, resolved through the anchor gear's source root. A product drawing
    // gears from several sources would need to say which one is the platform;
    // no field records that today, and the anchor's source is the only answer
    // available that is not a guess about directory names.
    let anchor = gear_of(input, application, &application.anchor)?;
    let anchor_root = source_root(input, anchor)?;
    let (version, registry, path) = located(
        input,
        TOOLKIT_PACKAGE,
        crate_dir,
        &anchor_root.join(TOOLKIT_SUBDIR),
    );
    dependencies.insert(
        TOOLKIT_ALIAS.to_owned(),
        Dependency {
            package: Some(TOOLKIT_PACKAGE.to_owned()),
            version,
            registry,
            path,
            features: vec!["bootstrap".to_owned()],
            default_features: None,
        },
    );

    // Enable only the database drivers actually selected for this application.
    let database_features: BTreeSet<_> = application
        .gears
        .iter()
        .filter(|id| {
            input
                .lock
                .gears
                .get(*id)
                .is_some_and(|gear| gear.runtime_caps.contains(&gearbox_ir::RuntimeCap::Db))
        })
        .map(|id| {
            if input.lock.sqlite_databases.contains_key(id) {
                "sqlite".to_owned()
            } else {
                "pg".to_owned()
            }
        })
        .collect();
    if !database_features.is_empty() {
        let (version, registry, path) = located(
            input,
            "cf-gears-toolkit-db",
            crate_dir,
            &anchor_root.join("libs/toolkit-db"),
        );
        dependencies.insert(
            "toolkit_db".to_owned(),
            Dependency {
                package: Some("cf-gears-toolkit-db".to_owned()),
                version,
                registry,
                path,
                features: database_features.into_iter().collect(),
                default_features: None,
            },
        );
    }

    for (name, dependency) in runtime_deps() {
        dependencies.insert(name.to_owned(), dependency);
    }

    for id in &application.gears {
        let gear = gear_of(input, application, id)?;
        let root = source_root(input, gear)?;
        let (version, registry, path) = located(
            input,
            &gear.package.crate_name,
            crate_dir,
            &root.join(gear.package.path.as_str()),
        );
        let dependency = Dependency {
            // Always spelled out, even where it matches the key: the key is the
            // library identifier and this is the package name, and
            // `cf-api-contracts` linking as `cf_api_contracts` rather than as
            // `api_contracts` is precisely the mistake GBX0209 exists for.
            package: Some(gear.package.crate_name.clone()),
            version,
            registry,
            path,
            // The union of what the gear declares and what the product asked
            // for. Kept separate in the lock and joined here, which is the one
            // place the distinction stops mattering: cargo takes a set.
            features: union_features(gear),
            // Written only when false. `true` is Cargo's default, and a manifest
            // restating every default is a manifest nobody reads.
            default_features: (!gear.package.default_features).then_some(false),
        };

        // Two crates claiming one library identifier would silently drop one of
        // them, and the `use x as _;` line that survived would register the
        // wrong gear -- or fail to compile, if we were lucky.
        if let Some(existing) = dependencies.insert(gear.package.lib_ident.clone(), dependency)
            && existing.package.as_deref() != Some(gear.package.crate_name.as_str())
        {
            return Err(GenerateError::LibIdentCollision {
                ident: gear.package.lib_ident.clone(),
                first: existing.package.unwrap_or_default(),
                second: gear.package.crate_name.clone(),
            });
        }
    }

    Ok(dependencies)
}

/// How a crate is named: the exact registry version when the product takes it
/// from a registry (`crates = registry(...)` on its source, and checked by
/// `gearbox_engine::published`), otherwise a path into the checkout.
///
/// A crate that differs from what was published is still named by version here;
/// the workspace's `[patch]` table points it at the checkout, so every crate
/// that depends on it links the same copy.
fn located(
    input: &GenerateInput<'_>,
    crate_name: &str,
    crate_dir: &Path,
    checkout: &Path,
) -> (Option<String>, Option<String>, Option<String>) {
    match input.registry.and_then(|plan| plan.get(crate_name)) {
        Some(entry) => (
            Some(format!("={}", entry.version)),
            (entry.registry != "crates.io").then(|| entry.registry.clone()),
            None,
        ),
        None => (None, None, Some(dep_path(crate_dir, checkout))),
    }
}

/// The lock's entry for a gear the process claims to contain.
fn gear_of<'a>(
    input: &'a GenerateInput<'_>,
    application: &ResolvedApplication,
    id: &GearId,
) -> Result<&'a ResolvedGear, GenerateError> {
    input
        .lock
        .gears
        .get(id)
        .ok_or_else(|| GenerateError::UnknownGear {
            application: application.name.to_string(),
            gear: id.to_string(),
        })
}

/// The absolute root of the source a gear came from.
fn source_root<'a>(
    input: &'a GenerateInput<'_>,
    gear: &ResolvedGear,
) -> Result<&'a Path, GenerateError> {
    input
        .source_roots
        .get(&gear.source)
        .map(std::path::PathBuf::as_path)
        .ok_or_else(|| GenerateError::UnknownSource {
            gear: gear.id.to_string(),
            id: gear.source.to_string(),
        })
}

/// A `path = ` value: relative when one exists, absolute otherwise.
///
/// Relative is the goal -- a generated tree that still resolves after the whole
/// checkout moves -- but a source root on another Windows volume has no
/// relative form, and an absolute path that works beats a relative one that
/// does not.
/// Every Cargo feature this gear is built with.
///
/// Two provenances, one list, and the join happens here rather than in the
/// resolver because cargo takes a set and the lock is owed the difference:
/// `package.features` is projected from the gear's own `cargo(...)`, while
/// `selected_features` is what the product asked for on the `use_gear` line.
///
/// Sorted and de-duplicated, because a manifest that reorders between two runs
/// of the same resolution would break `cpt-gearbox-nfr-determinism` for no
/// reason anyone could see.
fn union_features(gear: &gearbox_ir::ResolvedGear) -> Vec<String> {
    let mut all: std::collections::BTreeSet<&str> =
        gear.package.features.iter().map(String::as_str).collect();
    all.extend(gear.selected_features.iter().map(String::as_str));
    all.into_iter().map(ToOwned::to_owned).collect()
}

fn dep_path(from: &Path, target: &Path) -> String {
    paths::relative(from, target)
        .as_deref()
        .map_or_else(|| paths::to_slash(target), paths::to_slash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gear(declared: &[&str], selected: &[&str]) -> gearbox_ir::ResolvedGear {
        let mut package = gearbox_ir::CargoRef::new(
            "cf-gears-api",
            "api",
            gearbox_ir::RelPath::new("gears/api").unwrap(),
        );
        package.features = declared.iter().map(ToString::to_string).collect();
        gearbox_ir::ResolvedGear {
            id: gearbox_ir::GearId::new("api").unwrap(),
            source: gearbox_ir::SourceId::new("gears-rust").unwrap(),
            gdl_path: gearbox_ir::RelPath::new("gears/api/gear.gdl").unwrap(),
            package,
            crate_dir: gearbox_ir::RelPath::new("gears/api").unwrap(),
            runtime_caps: std::collections::BTreeSet::new(),
            colocated_deps: std::collections::BTreeSet::new(),
            selected_by: Vec::new(),
            config: std::collections::BTreeMap::new(),
            selected_features: selected.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn the_two_provenances_are_joined_for_cargo() {
        // Cargo takes one set, so this is the place the distinction the lock
        // keeps stops mattering.
        assert_eq!(
            union_features(&gear(&["otel"], &["k8s-auth"])),
            vec!["k8s-auth".to_owned(), "otel".to_owned()]
        );
    }

    #[test]
    fn a_feature_declared_and_also_asked_for_appears_once() {
        assert_eq!(
            union_features(&gear(&["otel"], &["otel"])),
            vec!["otel".to_owned()]
        );
    }

    #[test]
    fn the_order_does_not_depend_on_which_side_supplied_it() {
        // `cpt-gearbox-nfr-determinism` reaches the generated manifest too: a
        // list that reordered between two runs of one resolution would be a
        // diff nobody could explain.
        assert_eq!(
            union_features(&gear(&["b", "a"], &["c"])),
            union_features(&gear(&["c"], &["a", "b"]))
        );
    }
}
