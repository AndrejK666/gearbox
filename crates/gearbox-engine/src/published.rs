//! Which of a checkout's crates the build can take from a registry.
//!
//! `source(..., at = path("../gears-rust"), crates = registry("crates.io"))`
//! says the gears in a checkout are also published. Descriptions still come from
//! the checkout -- a published crate carries no `gear.gdl` -- but the generated
//! build should name `cf-gears-api-gateway = "=0.5.2"` rather than a path into
//! somebody's working tree, so the output repository builds from the registry
//! alone.
//!
//! **Declared, then checked, crate by crate.** A checkout on a feature branch is
//! usually *not* what was published: the toolkit on the branch this was written
//! against carries a fix for out-of-process workers that no release has yet.
//! Every published package records the commit it was cut from and its path in
//! that repository (`.cargo_vcs_info.json`), so the question "is this crate what
//! was published?" has an exact answer: diff the checkout against that commit,
//! under that path. Three outcomes:
//!
//! * **as published** -- a registry dependency at the exact version;
//! * **changed since** -- still the registry version, plus a `[patch]` entry
//!   pointing at the checkout, so the whole graph links one copy and it is the
//!   code the resolver read (GBX0709);
//! * **not published at this version** -- a path dependency, as before
//!   (GBX0710).
//!
//! If the registry cannot be asked at all, everything stays a path dependency
//! (GBX0711). None of this is guessed from names or from the network being up.
//!
//! **This module reads the world** -- cargo, git, the registry cache -- so it
//! lives at the edge with [`crate::registry`]: the CLI and the RPC server call
//! it, and `generate` receives only its answer, a [`RegistryPlan`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use gearbox_ir::{Diagnostic, DiagnosticCode, Location};

/// How the generated build names the crates of registry-backed sources.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistryPlan {
    /// By crate (package) name.
    pub crates: BTreeMap<String, RegistryCrate>,
    /// Every package in the registry's dependency graph of those crates, with
    /// its registry: the crates a `[patch]` entry can meaningfully name.
    pub graph: BTreeMap<String, String>,
}

/// One crate the build takes from a registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryCrate {
    /// The registry as the product named it: `crates.io`, or an alternate
    /// registry name from the machine's cargo configuration.
    pub registry: String,
    /// The exact version, written as `=version`.
    pub version: String,
    /// The checkout to patch in when it differs from what was published.
    pub patch: Option<PathBuf>,
}

impl RegistryPlan {
    /// The entry for `crate_name`, if the build takes it from a registry.
    #[must_use]
    pub fn get(&self, crate_name: &str) -> Option<&RegistryCrate> {
        self.crates.get(crate_name)
    }

    /// Patch entries grouped by registry, for the workspace's `[patch.*]` tables.
    #[must_use]
    pub fn patches(&self) -> BTreeMap<&str, Vec<(&str, &Path)>> {
        let mut out: BTreeMap<&str, Vec<(&str, &Path)>> = BTreeMap::new();
        for (name, entry) in &self.crates {
            if let Some(dir) = &entry.patch {
                out.entry(entry.registry.as_str())
                    .or_default()
                    .push((name.as_str(), dir.as_path()));
            }
        }
        out
    }
}

/// A crate a registry-backed source provides to this product.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub crate_name: String,
    /// The crate's directory in the checkout, absolute.
    pub dir: PathBuf,
}

/// What the checkout says about one crate, next to what was published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    AsPublished,
    /// Changed since the published commit; the files that differ, first few.
    Changed(Vec<String>),
    /// The published commit is not in the checkout's history, or the package
    /// carries no record of it: it cannot be compared, so it is treated as
    /// changed.
    Unverifiable(String),
}

/// Decide, for every candidate of one registry-backed source, how the build
/// names it.
///
/// `cache_dir` holds the seed manifest cargo resolves against; reusing it keeps
/// the registry lookup warm between plans.
#[must_use]
pub fn plan(
    registry: &str,
    candidates: &[Candidate],
    cache_dir: &Path,
) -> (RegistryPlan, Vec<Diagnostic>) {
    let mut out = RegistryPlan::default();
    let mut diagnostics = Vec::new();

    let mut wanted = Vec::new();
    for candidate in candidates {
        match local_version(&candidate.dir) {
            Some(version) => wanted.push((candidate, version)),
            None => diagnostics.push(not_published(
                &candidate.crate_name,
                "its version could not be read from the checkout's Cargo.toml",
            )),
        }
    }
    if wanted.is_empty() {
        return (out, diagnostics);
    }

    let (fetched, graph) = match fetch_exact(registry, &wanted, cache_dir) {
        Ok(fetched) => fetched,
        Err(reason) => {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::GenRegistryUnavailable,
                    format!("`{registry}` could not be asked for this product's crates: {reason}"),
                )
                .with_help(
                    "every crate stays a path dependency on the checkout; generate again \
                     once cargo can reach the registry",
                ),
            );
            return (out, diagnostics);
        }
    };

    out.graph = graph
        .into_iter()
        .map(|name| (name, registry.to_owned()))
        .collect();
    for (candidate, version) in wanted {
        let Some(published) = fetched.get(&candidate.crate_name) else {
            diagnostics.push(not_published(
                &candidate.crate_name,
                &format!("`{registry}` has no version {version}"),
            ));
            continue;
        };
        let verdict = compare(&candidate.dir, published);
        let patch = match &verdict {
            Verdict::AsPublished => None,
            Verdict::Changed(files) => {
                diagnostics.push(changed(
                    &candidate.crate_name,
                    &version,
                    &format!("the checkout differs in {}", files.join(", ")),
                    &candidate.dir,
                ));
                Some(candidate.dir.clone())
            }
            Verdict::Unverifiable(reason) => {
                diagnostics.push(changed(
                    &candidate.crate_name,
                    &version,
                    reason,
                    &candidate.dir,
                ));
                Some(candidate.dir.clone())
            }
        };
        out.crates.insert(
            candidate.crate_name.clone(),
            RegistryCrate {
                registry: registry.to_owned(),
                version,
                patch,
            },
        );
    }
    let patched: Vec<PathBuf> = out
        .crates
        .values()
        .filter_map(|c| c.patch.clone())
        .collect();
    close_over_paths(&mut out, &patched);
    (out, diagnostics)
}

/// The sources a description takes crates for from a registry, by id.
#[must_use]
pub fn registry_sources(
    sources: &BTreeMap<gearbox_ir::SourceId, gearbox_ir::SourceDecl>,
) -> BTreeMap<gearbox_ir::SourceId, String> {
    sources
        .iter()
        .filter_map(|(id, decl)| match decl {
            gearbox_ir::SourceDecl::Path {
                crates: Some(registry),
                ..
            } => Some((id.clone(), registry.clone())),
            _ => None,
        })
        .collect()
}

/// The plan for one resolved product: every gear of a registry-backed source,
/// and that source's toolkit, checked against what was published; every other
/// gear's path dependencies folded in so one copy of each crate links.
///
/// `None` when no source asks for a registry, which leaves generation exactly
/// as it was.
#[must_use]
pub fn for_product(
    registry_sources: &BTreeMap<gearbox_ir::SourceId, String>,
    lock: &gearbox_ir::ResolvedProduct,
    source_roots: &BTreeMap<gearbox_ir::SourceId, PathBuf>,
    cache_dir: &Path,
) -> (Option<RegistryPlan>, Vec<Diagnostic>) {
    if registry_sources.is_empty() {
        return (None, Vec::new());
    }
    let mut plan = RegistryPlan::default();
    let mut diagnostics = Vec::new();
    let mut by_registry: BTreeMap<&str, Vec<Candidate>> = BTreeMap::new();
    for (id, registry) in registry_sources {
        let Some(root) = source_roots.get(id) else {
            continue;
        };
        let list = by_registry.entry(registry.as_str()).or_default();
        for gear in lock.gears.values().filter(|g| &g.source == id) {
            let dir = root.join(gear.crate_dir.as_str());
            if !list.iter().any(|c| c.crate_name == gear.package.crate_name) {
                list.push(Candidate {
                    crate_name: gear.package.crate_name.clone(),
                    dir,
                });
            }
        }
        let toolkit = root.join(crate::generate::TOOLKIT_SUBDIR);
        if toolkit.join("Cargo.toml").is_file()
            && !list
                .iter()
                .any(|c| c.crate_name == crate::generate::TOOLKIT_PACKAGE)
        {
            list.push(Candidate {
                crate_name: crate::generate::TOOLKIT_PACKAGE.to_owned(),
                dir: toolkit,
            });
        }
        // The generated application's direct dependency enables the Postgres
        // backend. Plan it alongside toolkit so a registry build does not mix
        // a published toolkit's DB types with a second checkout copy.
        let toolkit_db = root.join("libs/toolkit-db");
        if lock.database.is_some()
            && lock.applications.iter().any(|app| app.needs_db)
            && toolkit_db.join("Cargo.toml").is_file()
            && !list.iter().any(|c| c.crate_name == "cf-gears-toolkit-db")
        {
            list.push(Candidate {
                crate_name: "cf-gears-toolkit-db".to_owned(),
                dir: toolkit_db,
            });
        }
    }
    for (registry, candidates) in by_registry {
        let (one, found) = self::plan(registry, &candidates, &cache_dir.join(registry));
        plan.crates.extend(one.crates);
        diagnostics.extend(found);
    }
    // Every gear the build names by path -- from a source without `crates`, or
    // from one with it but not published at its version -- is a start for the
    // walk: its path dependencies must be the same copies the registry crates
    // link.
    let path_gears: Vec<PathBuf> = lock
        .gears
        .values()
        .filter(|g| plan.get(&g.package.crate_name).is_none())
        .filter_map(|g| Some(source_roots.get(&g.source)?.join(g.crate_dir.as_str())))
        .collect();
    unify_path_dependents(&mut plan, &path_gears);
    (Some(plan), diagnostics)
}

/// Point every crate a path gear depends on *by path* at that same path, when
/// the plan takes that crate from a registry.
///
/// A gear that lives only beside the product -- one Create Gear wrote -- depends
/// on the toolkit by path. With the toolkit also coming from the registry, cargo
/// would link two toolkits, and a gear registered into one inventory is
/// invisible to a runtime reading the other. Patching the registry crate to the
/// path the gear already uses makes them one package.
pub fn unify_path_dependents(plan: &mut RegistryPlan, path_gear_dirs: &[PathBuf]) {
    close_over_paths(plan, path_gear_dirs);
}

/// Patch every registry crate reachable *by path* from `starts`.
///
/// **Transitively, and that was found by building.** A patched toolkit depends
/// by path on the checkout's directory SDK; a published grpc-hub depends on the
/// published one; cargo linked both, and the two `RegisterInstanceInfo` types
/// did not unify. So the walk follows path dependencies -- including
/// `workspace = true` ones, through the workspace's own table -- and every crate
/// it reaches that is also in the registry graph is patched to the path the
/// checkout uses. Crates outside the graph are walked through but not named,
/// so cargo never warns about a patch nothing uses.
fn close_over_paths(plan: &mut RegistryPlan, starts: &[PathBuf]) {
    let mut queue: Vec<PathBuf> = starts.to_vec();
    let mut seen = std::collections::BTreeSet::new();
    while let Some(dir) = queue.pop() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        for (package, target) in path_dependencies(&dir) {
            if let Some(registry) = plan.graph.get(&package).cloned() {
                let entry = plan.crates.entry(package).or_insert_with(|| RegistryCrate {
                    registry,
                    version: local_version(&target).unwrap_or_default(),
                    patch: None,
                });
                if entry.patch.is_none() {
                    entry.patch = Some(target.clone());
                }
            }
            queue.push(target);
        }
    }
}

/// The version the checkout's `Cargo.toml` declares, following
/// `version.workspace = true` up to the nearest workspace that sets one.
#[must_use]
pub fn local_version(crate_dir: &Path) -> Option<String> {
    let manifest: toml::Value = read_toml(&crate_dir.join("Cargo.toml"))?;
    let version = manifest.get("package")?.get("version")?;
    if let Some(version) = version.as_str() {
        return Some(version.to_owned());
    }
    if version.get("workspace").and_then(toml::Value::as_bool) != Some(true) {
        return None;
    }
    crate_dir.ancestors().skip(1).find_map(|dir| {
        let root: toml::Value = read_toml(&dir.join("Cargo.toml"))?;
        root.get("workspace")?
            .get("package")?
            .get("version")?
            .as_str()
            .map(str::to_owned)
    })
}

fn read_toml(path: &Path) -> Option<toml::Value> {
    toml::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// `(package name, absolute directory)` for each path dependency of a crate.
fn path_dependencies(crate_dir: &Path) -> Vec<(String, PathBuf)> {
    let Some(manifest) = read_toml(&crate_dir.join("Cargo.toml")) else {
        return Vec::new();
    };
    // `workspace = true` entries are resolved through the nearest workspace
    // that has a `[workspace.dependencies]` table, relative to that root.
    let workspace = crate_dir.ancestors().skip(1).find_map(|dir| {
        let root = read_toml(&dir.join("Cargo.toml"))?;
        let deps = root
            .get("workspace")?
            .get("dependencies")?
            .as_table()?
            .clone();
        Some((dir.to_path_buf(), deps))
    });
    let mut out = Vec::new();
    for table in ["dependencies", "build-dependencies"] {
        let Some(deps) = manifest.get(table).and_then(toml::Value::as_table) else {
            continue;
        };
        for (key, value) in deps {
            let inherited = value.get("workspace").and_then(toml::Value::as_bool) == Some(true);
            let (base, spec) = if inherited {
                let Some((root, table)) = &workspace else {
                    continue;
                };
                let Some(spec) = table.get(key) else { continue };
                (root.as_path(), spec)
            } else {
                (crate_dir, value)
            };
            let Some(path) = spec.get("path").and_then(toml::Value::as_str) else {
                continue;
            };
            let package = value
                .get("package")
                .or_else(|| spec.get("package"))
                .and_then(toml::Value::as_str)
                .unwrap_or(key);
            if let Ok(target) = base.join(path).canonicalize() {
                out.push((package.to_owned(), target));
            }
        }
    }
    out
}

/// Ask cargo for exactly these versions and say where it unpacked each.
///
/// One seed manifest for the whole source, resolved with `cargo metadata`: that
/// buys authentication, the machine's mirror and the cache without a registry
/// client of our own (the same choice as [`crate::registry::fetch`]). When the
/// batch does not resolve -- typically one version that was never published --
/// each crate is asked alone, so one missing release does not hide the others.
#[allow(
    clippy::type_complexity,
    reason = "private; the pair is (found, graph)"
)]
fn fetch_exact(
    registry: &str,
    wanted: &[(&Candidate, String)],
    cache_dir: &Path,
) -> Result<
    (
        BTreeMap<String, PathBuf>,
        std::collections::BTreeSet<String>,
    ),
    String,
> {
    let all: Vec<(&str, &str)> = wanted
        .iter()
        .map(|(c, v)| (c.crate_name.as_str(), v.as_str()))
        .collect();
    match resolve(registry, &all, cache_dir) {
        Ok(found) => Ok(found),
        Err(batch) => {
            let mut found = BTreeMap::new();
            let mut graph = std::collections::BTreeSet::new();
            let mut any = false;
            for one in &all {
                if let Ok((single, names)) = resolve(registry, std::slice::from_ref(one), cache_dir)
                {
                    any = true;
                    found.extend(single);
                    graph.extend(names);
                }
            }
            if any || all.len() == 1 {
                Ok((found, graph))
            } else {
                Err(batch)
            }
        }
    }
}

#[allow(
    clippy::type_complexity,
    reason = "private; the pair is (found, graph)"
)]
fn resolve(
    registry: &str,
    crates: &[(&str, &str)],
    cache_dir: &Path,
) -> Result<
    (
        BTreeMap<String, PathBuf>,
        std::collections::BTreeSet<String>,
    ),
    String,
> {
    let mut body = String::from(
        "# GENERATED by gearbox -- resolves the published versions a product's\n\
         # checkout declares. Not a product; delete it and it comes back.\n\
         [package]\nname = \"gearbox-published-seed\"\nversion = \"0.0.0\"\n\
         edition = \"2021\"\npublish = false\n\n[dependencies]\n",
    );
    let lines: Vec<String> = crates
        .iter()
        .map(|(name, version)| {
            if registry == "crates.io" {
                format!("\"{name}\" = \"={version}\"\n")
            } else {
                format!("\"{name}\" = {{ version = \"={version}\", registry = \"{registry}\" }}\n")
            }
        })
        .collect();
    body.push_str(&lines.concat());
    let src = cache_dir.join("src");
    std::fs::create_dir_all(&src).map_err(|e| format!("cannot write {}: {e}", src.display()))?;
    std::fs::write(src.join("lib.rs"), "").map_err(|e| e.to_string())?;
    let manifest = cache_dir.join("Cargo.toml");
    std::fs::write(&manifest, body).map_err(|e| e.to_string())?;
    // A lock from a previous batch would pin a different set; this seed is
    // disposable and must answer about exactly these versions.
    drop(std::fs::remove_file(cache_dir.join("Cargo.lock")));

    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(&manifest)
        .exec()
        .map_err(|e| first_line(&e.to_string()))?;
    let mut found = BTreeMap::new();
    let mut graph = std::collections::BTreeSet::new();
    for package in &metadata.packages {
        let name = package.name.to_string();
        if package.source.is_some() {
            graph.insert(name.clone());
        }
        if crates
            .iter()
            .any(|(n, v)| *n == name && package.version.to_string() == *v)
            && let Some(dir) = package.manifest_path.parent()
        {
            found.insert(name, dir.as_std_path().to_path_buf());
        }
    }
    Ok((found, graph))
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| l.starts_with("error") || l.contains("failed to select"))
        .or_else(|| text.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("cargo metadata failed")
        .to_owned()
}

/// Compare a checkout crate with the published package unpacked at `published`.
#[must_use]
pub fn compare(crate_dir: &Path, published: &Path) -> Verdict {
    let Some((sha, path_in_vcs)) = vcs_info(published) else {
        return Verdict::Unverifiable(
            "the published package records no commit to compare against".to_owned(),
        );
    };
    let Some(top) = git(crate_dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from) else {
        return Verdict::Unverifiable("the checkout is not a git repository".to_owned());
    };
    if git(&top, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_none() {
        return Verdict::Unverifiable(format!(
            "the published commit {} is not in the checkout's history",
            short(&sha)
        ));
    }
    // `gear.gdl` is Gearbox's, never part of the package, and says nothing
    // about what compiles.
    let exclude = format!(":(exclude){path_in_vcs}/gear.gdl");
    let mut files: Vec<String> = git(
        &top,
        &["diff", "--name-only", &sha, "--", &path_in_vcs, &exclude],
    )
    .map(|out| out.lines().map(str::to_owned).collect())
    .unwrap_or_default();
    if let Some(untracked) = git(
        &top,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "--",
            &path_in_vcs,
            &exclude,
        ],
    ) {
        files.extend(untracked.lines().map(str::to_owned));
    }
    if files.is_empty() {
        return Verdict::AsPublished;
    }
    files.sort();
    let shown: Vec<String> = files
        .iter()
        .take(3)
        .map(|f| format!("`{f}`"))
        .chain((files.len() > 3).then(|| format!("{} more", files.len() - 3)))
        .collect();
    Verdict::Changed(shown)
}

fn vcs_info(published: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(published.join(".cargo_vcs_info.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let sha = json.get("git")?.get("sha1")?.as_str()?.to_owned();
    let path = json
        .get("path_in_vcs")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    Some((
        sha,
        if path.is_empty() {
            ".".to_owned()
        } else {
            path
        },
    ))
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

fn not_published(crate_name: &str, why: &str) -> Diagnostic {
    Diagnostic::new(
        DiagnosticCode::GenCrateNotPublished,
        format!("`{crate_name}` stays a path dependency: {why}"),
    )
    .with_help(
        "publish that version, or leave the crate on the checkout; the build works either way",
    )
}

fn changed(crate_name: &str, version: &str, why: &str, dir: &Path) -> Diagnostic {
    Diagnostic::new(
        DiagnosticCode::GenCrateNotAsPublished,
        format!("`{crate_name}` {version} is not what was published: {why}"),
    )
    .with_help(format!(
        "the build names {version} from the registry and patches in `{}`, so the code the \
         resolver read is the code that links; release the change to make the output \
         self-contained",
        dir.display()
    ))
    .at(Location::file(dir.join("Cargo.toml").display().to_string()))
}

#[cfg(test)]
#[path = "published_tests.rs"]
mod tests;
