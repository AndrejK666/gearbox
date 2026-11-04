//! Gear crates named at the versions the product's own `Cargo.lock` pins.
//!
//! **The failure this exists for.** A product whose own gears depend on
//! published platform crates -- `cf-gears-toolkit = "0.10"` from crates.io --
//! has already chosen the platform release it builds against, and its
//! `Cargo.lock` records the choice. Generation used to name every platform gear
//! by path into a checkout of the platform's source instead. The checkout is
//! newer than the release, so the generated graph held two copies of the
//! toolkit's dependencies (`gts` 0.12 for the product's code, 0.16 for the
//! checkout's), and cargo refused to unify the traits between them.
//!
//! **The rule.** For every crate the generated manifests name -- a gear, the
//! toolkit, a client SDK -- the product's lock is asked: if it pins that crate
//! from crates.io at exactly one version, the manifest names `"=<version>"`
//! from crates.io and no `[patch]` points it anywhere else. Otherwise the crate
//! stays what it was: a path dependency, or whatever `crates = registry(...)`
//! decided for it. A crate the product builds itself appears in its lock with
//! no `source`, so the product's own gears stay path dependencies without a
//! rule of their own.
//!
//! **Which lock.** In order: one named on the command line or in
//! `GEARBOX_CARGO_LOCK`; one already in the output root (a build service seeds
//! it there); the `Cargo.lock` of the product's own `path(...)` source, when
//! exactly one has one. A `git(...)` source is a fetched copy of somebody
//! else's repository, and its lock is not the product's decision.
//!
//! Like [`crate::published`], this reads the world, so the CLI calls it and
//! `generate` receives only the resulting [`RegistryPlan`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gearbox_ir::{Diagnostic, DiagnosticCode, Location, ResolvedProduct, SourceDecl, SourceId};

use crate::published::{RegistryCrate, RegistryPlan};

/// The `source` Cargo writes for a crates.io package in a lock, in either
/// protocol.
const CRATES_IO_SOURCES: [&str; 2] = [
    "registry+https://github.com/rust-lang/crates.io-index",
    "sparse+https://index.crates.io/",
];

/// Where the generated build takes gear crates from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GearsFrom {
    /// From crates.io where the product's lock pins them, else from source.
    #[default]
    Auto,
    /// As `Auto`, and say so loudly when there is no lock or a gear is not in it.
    Registry,
    /// Always from source (or `crates = registry(...)`), ignoring any lock.
    Source,
}

impl std::str::FromStr for GearsFrom {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(Self::Auto),
            "registry" => Ok(Self::Registry),
            "source" => Ok(Self::Source),
            other => Err(format!(
                "`{other}` is not one of `auto`, `registry`, `source`"
            )),
        }
    }
}

/// The lock the versions come from.
#[derive(Clone, Debug)]
pub struct ProductLock {
    pub path: PathBuf,
    pub text: String,
    /// Already the output root's own `Cargo.lock`, so not written again.
    pub in_out_root: bool,
}

/// Find the product's lock.
///
/// # Errors
/// Returns an error when `explicit` names a file that cannot be read; a lock
/// found by looking that cannot be read is skipped instead.
pub fn find(
    explicit: Option<&Path>,
    out_root: &Path,
    sources: &BTreeMap<SourceId, SourceDecl>,
    source_roots: &BTreeMap<SourceId, PathBuf>,
) -> Result<Option<ProductLock>, std::io::Error> {
    if let Some(path) = explicit {
        let text = std::fs::read_to_string(path)?;
        let in_out_root =
            path.canonicalize().ok() == out_root.join("Cargo.lock").canonicalize().ok();
        return Ok(Some(ProductLock {
            path: path.to_path_buf(),
            text,
            in_out_root,
        }));
    }
    let seeded = out_root.join("Cargo.lock");
    if let Ok(text) = std::fs::read_to_string(&seeded) {
        return Ok(Some(ProductLock {
            path: seeded,
            text,
            in_out_root: true,
        }));
    }
    let own: Vec<PathBuf> = sources
        .iter()
        .filter(|(_, decl)| matches!(decl, SourceDecl::Path { .. }))
        .filter_map(|(id, _)| source_roots.get(id))
        .map(|root| root.join("Cargo.lock"))
        .filter(|lock| lock.is_file())
        .collect();
    // Two own locks could pin one crate twice; choosing is not this module's
    // decision, exactly as for the source lock the tree is seeded with.
    let [path] = own.as_slice() else {
        return Ok(None);
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    Ok(Some(ProductLock {
        path: path.clone(),
        text,
        in_out_root: false,
    }))
}

/// The crates.io versions a lock pins, by package name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LockedVersions(BTreeMap<String, BTreeSet<String>>);

impl LockedVersions {
    /// Read the `[[package]]` entries of a `Cargo.lock`.
    ///
    /// # Errors
    /// Returns the parse error when the text is not TOML.
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for package in value
            .get("package")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            let field = |key: &str| package.get(key).and_then(toml::Value::as_str);
            let (Some(name), Some(version), Some(source)) =
                (field("name"), field("version"), field("source"))
            else {
                continue;
            };
            if CRATES_IO_SOURCES.contains(&source) {
                out.entry(name.to_owned())
                    .or_default()
                    .insert(version.to_owned());
            }
        }
        Ok(Self(out))
    }

    /// Every crates.io version the lock holds for `name`.
    #[must_use]
    pub fn versions(&self, name: &str) -> Option<&BTreeSet<String>> {
        self.0.get(name)
    }

    /// The one crates.io version the lock holds for `name`, if exactly one.
    #[must_use]
    pub fn single(&self, name: &str) -> Option<&str> {
        let versions = self.0.get(name)?;
        match versions.len() {
            1 => versions.iter().next().map(String::as_str),
            _ => None,
        }
    }
}

/// What [`pin_crates`] decided.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pinned {
    /// Crate name to the version now named.
    pub pinned: BTreeMap<String, String>,
    /// Crates the lock holds at more than one crates.io version.
    pub ambiguous: BTreeMap<String, Vec<String>>,
}

/// Name every crate in `names` that `locked` pins from crates.io at its locked
/// version, and take any `[patch]` off a crate the lock pins: a patch would
/// point it back at a checkout, which is the second copy pinning removes.
pub fn pin_crates(
    plan: &mut RegistryPlan,
    names: &BTreeSet<String>,
    locked: &LockedVersions,
) -> Pinned {
    let mut out = Pinned::default();
    let everything: BTreeSet<String> = names
        .iter()
        .cloned()
        .chain(plan.crates.keys().cloned())
        .collect();
    for name in everything {
        let Some(versions) = locked.versions(&name) else {
            continue;
        };
        let Some(version) = locked.single(&name) else {
            out.ambiguous
                .insert(name.clone(), versions.iter().cloned().collect());
            continue;
        };
        if let Some(existing) = plan.crates.get(&name)
            && existing.registry != "crates.io"
        {
            // `crates = registry("<alternate>")` said where this one comes
            // from; a crates.io entry in the lock is a different package.
            continue;
        }
        plan.crates.insert(
            name.clone(),
            RegistryCrate {
                registry: "crates.io".to_owned(),
                version: version.to_owned(),
                patch: None,
            },
        );
        out.pinned.insert(name, version.to_owned());
    }
    out
}

/// Every crate a generated manifest can name for this product: each gear's
/// package, the toolkit and its database half, and each remote binding's
/// client SDK.
#[must_use]
pub fn named_crates(product: &ResolvedProduct) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = product
        .gears
        .values()
        .map(|g| g.package.crate_name.clone())
        .collect();
    names.insert(crate::generate::TOOLKIT_PACKAGE.to_owned());
    names.insert("cf-gears-toolkit-db".to_owned());
    names.extend(
        product
            .bindings
            .iter()
            .filter_map(|b| b.client_sdk.as_ref())
            .map(|sdk| sdk.crate_name.clone()),
    );
    names
}

/// Fold the product's lock into the registry plan.
///
/// Returns `plan` untouched for `GearsFrom::Source`, and when no lock was found
/// (with a warning under `GearsFrom::Registry`). Otherwise the returned plan
/// names every lock-pinned crate by version, carries the lock to seed the tree
/// with, and a `GBX0713` line says how many gears came from where.
#[must_use]
pub fn apply(
    mode: GearsFrom,
    found: Option<&ProductLock>,
    plan: Option<RegistryPlan>,
    product: &ResolvedProduct,
    source_roots: &BTreeMap<SourceId, PathBuf>,
) -> (Option<RegistryPlan>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    if mode == GearsFrom::Source {
        return (plan, diagnostics);
    }
    let Some(found) = found else {
        if mode == GearsFrom::Registry {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::GenGearsFromLockIncomplete,
                    "`--gears-from registry`, but no Cargo.lock of the product was found; \
                     every gear is built from source",
                )
                .with_help(
                    "name the product's lock with `--cargo-lock` or `GEARBOX_CARGO_LOCK`, or \
                     seed `Cargo.lock` into the output root",
                ),
            );
        }
        return (plan, diagnostics);
    };
    let where_ = Location::file(found.path.display().to_string());
    let locked = match LockedVersions::parse(&found.text) {
        Ok(locked) => locked,
        Err(error) => {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::GenGearsFromLockIncomplete,
                    format!("the product's Cargo.lock could not be read: {error}"),
                )
                .with_help("every gear is built from source")
                .at(where_),
            );
            return (plan, diagnostics);
        }
    };

    let had_plan = plan.is_some();
    let mut plan = plan.unwrap_or_default();
    let pinned = pin_crates(&mut plan, &named_crates(product), &locked);

    for (name, versions) in &pinned.ambiguous {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GenGearsFromLockIncomplete,
                format!(
                    "the product's Cargo.lock holds `{name}` at {} from crates.io, so it is \
                     not pinned",
                    versions.join(" and ")
                ),
            )
            .with_help("the crate keeps the dependency it had; one version in the lock pins it")
            .at(where_.clone()),
        );
    }

    let from_registry = product
        .gears
        .values()
        .filter(|g| pinned.pinned.contains_key(&g.package.crate_name))
        .count();
    // A gear `crates = registry(...)` already named by version is neither.
    let from_source: Vec<_> = product
        .gears
        .values()
        .filter(|g| plan.get(&g.package.crate_name).is_none())
        .collect();
    let source_names: Vec<String> = from_source.iter().map(|g| g.id.to_string()).collect();

    // A gear left on the checkout that reaches a pinned crate by path links a
    // second copy of it: the mixed graph again, one crate at a time.
    let starts: Vec<PathBuf> = from_source
        .iter()
        .filter_map(|g| Some(source_roots.get(&g.source)?.join(g.crate_dir.as_str())))
        .collect();
    let mut second_copies: BTreeMap<String, (PathBuf, String)> = BTreeMap::new();
    for (package, dir) in crate::published::reachable_by_path(&starts) {
        if let Some(version) = pinned.pinned.get(&package) {
            second_copies
                .entry(package)
                .or_insert((dir, version.clone()));
        }
    }
    for (package, (dir, version)) in &second_copies {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GenGearsFromLockIncomplete,
                format!(
                    "a gear built from source depends by path on `{package}` at `{}`, which the \
                     build takes from crates.io at {version}: two copies of it will link",
                    dir.display(),
                ),
            )
            .with_help(
                "add the gear to the product's own Cargo.toml so its lock pins it too, or \
                 generate with `--gears-from source`",
            )
            .at(where_.clone()),
        );
    }

    if mode == GearsFrom::Registry && !source_names.is_empty() {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GenGearsFromLockIncomplete,
                format!(
                    "`--gears-from registry`, but the product's Cargo.lock does not pin {}; \
                     built from source",
                    source_names.join(", ")
                ),
            )
            .at(where_.clone()),
        );
    }

    let mut summary = Diagnostic::new(
        DiagnosticCode::GenGearsFromLock,
        format!(
            "{} gear(s) taken from crates.io at the versions in the product's Cargo.lock, {} \
             from source",
            from_registry,
            from_source.len()
        ),
    )
    .at(where_);
    if !source_names.is_empty() {
        summary = summary.with_help(format!("from source: {}", source_names.join(", ")));
    }
    diagnostics.push(summary);

    if pinned.pinned.is_empty() {
        // Nothing pinned: the output is what it would have been without a lock.
        return (had_plan.then_some(plan), diagnostics);
    }
    if !found.in_out_root {
        plan.cargo_lock = Some(found.text.clone());
    }
    (Some(plan), diagnostics)
}

#[cfg(test)]
#[path = "locked_tests.rs"]
mod tests;
