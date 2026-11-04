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
//! from crates.io and no `[patch]` points it anywhere else. If it pins it from
//! a git repository at one commit (a product taking the platform by
//! `git = ..., branch = "main"`), the manifest names that repository at that
//! commit, so the build links the same copy the product's code does. The
//! checkout the gears were described from is never the build's copy of a
//! crate the lock pins: it is only where the `gear.gdl` files are read. Otherwise the crate
//! stays what it was: a path dependency, or whatever `crates = registry(...)`
//! decided for it. A crate the product builds itself appears in its lock with
//! no `source`, so the product's own gears stay path dependencies without a
//! rule of their own.
//!
//! **Which lock.** In order: one named on the command line or in
//! `GEARBOX_CARGO_LOCK`; one already in the output root (a build service seeds
//! it there); the `Cargo.lock` of the product's own `path(...)` source, when
//! exactly one has one; else the one lock, nearest above a gear's crate in any
//! source, that pins a crate the build names from crates.io.
//!
//! That last step exists because a build service fetches *every* source by
//! `git(...)`, the product's own code included: Studio's build checks out
//! `studio-backend` at a commit exactly as it checks out `gears-rust`, so no
//! source is a `path(...)` one and the product's lock went unfound. The
//! platform's own lock cannot be mistaken for it: a workspace lock records its
//! own members with no `source`, so the lock of the repository the gears are
//! written in pins none of them from crates.io. Only a consumer's lock does,
//! and that consumer is the product's code. Two such locks are two decisions,
//! and choosing between them is not this module's call.
//!
//! **A gear the lock does not pin, on a checkout that does.** A product can
//! use a platform gear its own code never depends on (`rg-tr-plugin`), so its
//! lock does not name it. Built from the checkout, it reaches the toolkit by
//! path and links a second toolkit beside the pinned one -- the mixed graph
//! again. When the checkout's requirements on every crate the lock pins are
//! met by the locked versions, the gear is taken from crates.io at the
//! checkout's version instead, and cargo resolves it into the lock's graph;
//! otherwise it stays on the checkout and `GBX0714` names the second copy.
//!
//! Like [`crate::published`], this reads the world, so the CLI calls it and
//! `generate` receives only the resulting [`RegistryPlan`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gearbox_ir::{Diagnostic, DiagnosticCode, Location, ResolvedProduct, SourceDecl, SourceId};

use crate::published::{GitPin, RegistryCrate, RegistryPlan};

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
    product: &ResolvedProduct,
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
    match own.as_slice() {
        [] => Ok(consumer_lock(product, source_roots)),
        [path] => Ok(std::fs::read_to_string(path).ok().map(|text| ProductLock {
            path: path.clone(),
            text,
            in_out_root: false,
        })),
        _ => Ok(None),
    }
}

/// The one lock, nearest above some gear's crate within its source, that pins
/// a crate this build names from crates.io: the lock of the product's own
/// code, wherever it was fetched from. `None` when there is none or more than
/// one.
fn consumer_lock(
    product: &ResolvedProduct,
    source_roots: &BTreeMap<SourceId, PathBuf>,
) -> Option<ProductLock> {
    let named = named_crates(product);
    let nearest: BTreeSet<PathBuf> = product
        .gears
        .values()
        .filter_map(|gear| {
            let root = source_roots.get(&gear.source)?;
            let dir = root.join(gear.crate_dir.as_str());
            dir.ancestors()
                .take_while(|d| d.starts_with(root))
                .map(|d| d.join("Cargo.lock"))
                .find(|lock| lock.is_file())
        })
        .collect();
    let mut pinning = nearest.into_iter().filter_map(|path| {
        let text = std::fs::read_to_string(&path).ok()?;
        let locked = LockedVersions::parse(&text).ok()?;
        named
            .iter()
            .any(|name| locked.pins(name))
            .then_some((path, text))
    });
    let (path, text) = pinning.next()?;
    if pinning.next().is_some() {
        return None;
    }
    Some(ProductLock {
        path,
        text,
        in_out_root: false,
    })
}

/// How the gear at `dir` can be named without leaving the lock's graph, when
/// it can: it states a requirement on at least one crate the lock pins, every
/// such requirement is met by the one version the lock holds, and those
/// crates all come from one place. From crates.io, the gear is named at the
/// checkout's version; from one git repository at one commit, it is named
/// from that same repository and commit, where cargo finds it by name.
///
/// The checkout stands in for the published crate of the same version, whose
/// requirements release tooling wrote from that same manifest.
fn follows_lock(dir: &Path, locked: &LockedVersions) -> Option<RegistryCrate> {
    use cargo_metadata::semver::{Version, VersionReq};
    let version = crate::published::local_version(dir)?;
    let mut from: Option<Option<GitPin>> = None;
    for (package, req) in crate::published::dependency_requirements(dir) {
        if !locked.pins(&package) {
            continue;
        }
        let (at, git) = match locked.single_git(&package) {
            Some((at, pin)) => (at, Some(pin)),
            None => (locked.single(&package)?, None),
        };
        if !VersionReq::parse(&req)
            .ok()?
            .matches(&Version::parse(at).ok()?)
        {
            return None;
        }
        match &from {
            None => from = Some(git),
            Some(seen) if *seen == git => {}
            Some(_) => return None,
        }
    }
    Some(RegistryCrate {
        registry: "crates.io".to_owned(),
        version,
        patch: None,
        git: from?,
    })
}

/// The crates.io versions a lock pins, by package name, and the crates it
/// pins from git.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LockedVersions {
    registry: BTreeMap<String, BTreeSet<String>>,
    git: BTreeMap<String, BTreeSet<LockedGit>>,
}

/// One git entry of a lock: version, repository, commit.
type LockedGit = (String, String, String);

impl LockedVersions {
    /// Read the `[[package]]` entries of a `Cargo.lock`.
    ///
    /// # Errors
    /// Returns the parse error when the text is not TOML.
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut out = Self::default();
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
                out.registry
                    .entry(name.to_owned())
                    .or_default()
                    .insert(version.to_owned());
            } else if let Some(pin) = git_source(source) {
                out.git.entry(name.to_owned()).or_default().insert((
                    version.to_owned(),
                    pin.url,
                    pin.rev,
                ));
            }
        }
        Ok(out)
    }

    /// Every crates.io version the lock holds for `name`.
    #[must_use]
    pub fn versions(&self, name: &str) -> Option<&BTreeSet<String>> {
        self.registry.get(name)
    }

    /// The one crates.io version the lock holds for `name`, if exactly one.
    #[must_use]
    pub fn single(&self, name: &str) -> Option<&str> {
        let versions = self.registry.get(name)?;
        match versions.len() {
            1 => versions.iter().next().map(String::as_str),
            _ => None,
        }
    }

    /// The one git pin the lock holds for `name` -- version and commit -- when
    /// it holds exactly one and no crates.io entry beside it.
    #[must_use]
    pub fn single_git(&self, name: &str) -> Option<(&str, GitPin)> {
        if self.registry.contains_key(name) {
            return None;
        }
        let pins = self.git.get(name)?;
        let [(version, url, rev)] = <[_; 1]>::try_from(pins.iter().collect::<Vec<_>>()).ok()?;
        Some((
            version.as_str(),
            GitPin {
                url: url.clone(),
                rev: rev.clone(),
            },
        ))
    }

    /// Whether the lock pins `name` from anywhere but the product itself.
    #[must_use]
    pub fn pins(&self, name: &str) -> bool {
        self.registry.contains_key(name) || self.git.contains_key(name)
    }

    /// The one version the lock pins `name` at, from crates.io or from git.
    #[must_use]
    pub fn single_any(&self, name: &str) -> Option<&str> {
        self.single(name)
            .or_else(|| self.single_git(name).map(|(version, _)| version))
    }
}

/// `git+<url>[?branch=..|?rev=..|?tag=..]#<commit>`, the way Cargo writes a
/// git source in a lock, as the repository and the locked commit.
fn git_source(source: &str) -> Option<GitPin> {
    let rest = source.strip_prefix("git+")?;
    let (head, rev) = rest.rsplit_once('#')?;
    let url = head.split_once('?').map_or(head, |(url, _)| url);
    (!url.is_empty() && !rev.is_empty()).then(|| GitPin {
        url: url.to_owned(),
        rev: rev.to_owned(),
    })
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
        if let Some((version, pin)) = locked.single_git(&name) {
            plan.crates.insert(
                name.clone(),
                RegistryCrate {
                    registry: "crates.io".to_owned(),
                    version: version.to_owned(),
                    patch: None,
                    git: Some(pin),
                },
            );
            out.pinned.insert(name, version.to_owned());
            continue;
        }
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
                git: None,
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

    // A gear the lock does not name that would reach a pinned crate by path
    // from the checkout: from crates.io when its requirements fit the lock.
    let mut followed: Vec<(String, String)> = Vec::new();
    for gear in product.gears.values() {
        if plan.get(&gear.package.crate_name).is_some() {
            continue;
        }
        let Some(root) = source_roots.get(&gear.source) else {
            continue;
        };
        let dir = root.join(gear.crate_dir.as_str());
        let reaches_pinned = crate::published::reachable_by_path(std::slice::from_ref(&dir))
            .iter()
            .any(|(package, _)| pinned.pinned.contains_key(package));
        if !reaches_pinned {
            continue;
        }
        if let Some(entry) = follows_lock(&dir, &locked) {
            let whence = entry.git.as_ref().map_or_else(
                || format!("crates.io at {}", entry.version),
                |g| format!("{} at {} ({})", g.url, g.rev, entry.version),
            );
            plan.crates.insert(gear.package.crate_name.clone(), entry);
            followed.push((gear.id.to_string(), whence));
        }
    }
    for (gear, whence) in &followed {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GenGearsFromLock,
                format!(
                    "`{gear}` is not in the product's Cargo.lock; taken from {whence}, the \
                     checkout's version, whose requirements the lock meets"
                ),
            )
            .with_help("add the gear to the product's own Cargo.toml to pin it in the lock as well")
            .at(where_.clone()),
        );
    }

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
        .count()
        + followed.len();
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

    if pinned.pinned.is_empty() && followed.is_empty() {
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
