//! The generated workspace root, its toolchain pin, and the lock beside them.

use std::collections::BTreeMap;

use gearbox_ir::{FileEntry, FileKind, Ownership, ResolvedApplication, ResolvedProduct};
use serde::Serialize;

use super::templates::TemplateSet;
use super::{GenerateError, GenerateInput, header, paths};

/// The toolchain the generated crates build with.
///
/// Copied from `gears-rust/rust-toolchain.toml` rather than from this
/// repository's: the generated crates path-depend on `gears-rust` sources, so
/// the toolchain that has to accept them is the platform's, not ours. The two
/// happen to agree today, and pinning to the wrong one of the two would be
/// invisible until the day they stop agreeing.
const CHANNEL: &str = "1.97.0";

/// The rustc channel the generated crates -- and therefore their
/// Dockerfiles -- pin. One function so a Dockerfile cannot drift from
/// `rust-toolchain.toml` by restating the literal.
pub const fn rust_channel() -> &'static str {
    CHANNEL
}

/// The Rust edition written into every generated manifest.
///
/// Written out as a literal, and this is the single most consequential literal
/// in the generator. The gear crates the output depends on say
/// `edition.workspace = true`, which resolves for *them* because they physically
/// live inside the `gears-rust` workspace. The generated crates live under
/// `.gearbox/<product>/<profile>/`, outside every workspace but their own, so
/// inheriting anything is not available to them: the field must carry a value.
const EDITION: &str = "2024";

/// Matches `gears-rust`'s `workspace.package.rust-version`.
///
/// Deliberately the platform's floor and not this repository's `1.97.0`: the
/// generated crate's minimum is dictated by what it links against.
const RUST_VERSION: &str = "1.95.0";

#[derive(Serialize)]
struct WorkspaceManifest {
    workspace: WorkspaceTable,
    /// `[patch.<registry>]`: crates named by registry version whose checkout
    /// differs from what was published, pointed back at the checkout.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    patch: BTreeMap<String, BTreeMap<String, PatchEntry>>,
}

#[derive(Serialize)]
struct PatchEntry {
    path: String,
}

#[derive(Serialize)]
struct WorkspaceTable {
    resolver: &'static str,
    members: Vec<String>,
}

/// The generated workspace root.
///
/// A `[workspace]` table is not optional here, and not for tidiness: the output
/// root sits inside this repository, so without one Cargo would walk up, find
/// `gearbox`'s root manifest, and refuse the package as "believes it's
/// in a workspace when it's not". Declaring the workspace is what stops that
/// walk.
///
/// # Errors
/// Returns [`GenerateError`] if the manifest cannot be serialized.
pub fn workspace_manifest(
    applications: &[&ResolvedApplication],
    layout: &str,
    input: &GenerateInput<'_>,
) -> Result<FileEntry, GenerateError> {
    let mut patch: BTreeMap<String, BTreeMap<String, PatchEntry>> = BTreeMap::new();
    if let Some(plan) = input.registry {
        for (registry, crates) in plan.patches() {
            // Cargo spells the default registry `crates-io` in a `[patch]` key,
            // and every other one by the name its configuration gives it.
            let key = if registry == "crates.io" {
                "crates-io"
            } else {
                registry
            };
            let table = patch.entry(key.to_owned()).or_default();
            for (name, dir) in crates {
                let spelled = paths::relative(input.out_root, dir)
                    .map_or_else(|| paths::to_slash(dir), |rel| paths::to_slash(&rel));
                table.insert(name.to_owned(), PatchEntry { path: spelled });
            }
        }
    }
    let manifest = WorkspaceManifest {
        workspace: WorkspaceTable {
            resolver: "3",
            members: applications
                .iter()
                .map(|p| format!("{layout}/{}", p.name))
                .collect(),
        },
        patch,
    };
    let body = toml::to_string_pretty(&manifest).map_err(|source| GenerateError::Toml {
        what: "the generated workspace manifest",
        source,
    })?;

    Ok(FileEntry::text(
        paths::rel(&["Cargo.toml"])?,
        format!("{}\n{body}", header("#")),
        FileKind::Toml,
        Ownership::Generated,
    ))
}

/// The toolchain pin, from the `rust-toolchain.toml` template.
///
/// # Errors
/// Returns [`GenerateError`] when the template is unknown or fails to render.
pub fn toolchain(templates: &TemplateSet) -> Result<FileEntry, GenerateError> {
    let body = super::templates::render(
        "rust-toolchain.toml",
        templates.get("rust-toolchain.toml")?,
        minijinja::context! { header => header("#").trim_end(), channel => CHANNEL },
    )?;
    Ok(FileEntry::text(
        paths::rel(&["rust-toolchain.toml"])?,
        body,
        FileKind::Toml,
        Ownership::Generated,
    ))
}

/// The source's `Cargo.lock`, so the generated workspace builds the versions
/// the source was tested with.
///
/// **A generated tree had no lock.** Its crates path-depend on the source's,
/// and every `cargo build` of it -- the Dockerfile's included -- resolved the
/// registry afresh: the same description built different dependency versions
/// on different days. A product's gateway picked up `pingora-core` 0.8.1, a
/// release `gears-rust` had never run, and OAGW's proxy panicked on every call.
/// Cargo keeps every version a lock pins and only adds the generated
/// packages, so the source's lock is the right seed.
///
/// Copied only when exactly one source root has a lock: two would pin one crate
/// twice, and choosing between them is a decision this cannot make.
///
/// # Errors
/// Returns [`GenerateError`] if the lock cannot be read.
pub fn cargo_lock(
    source_roots: &BTreeMap<gearbox_ir::SourceId, std::path::PathBuf>,
) -> Result<Option<FileEntry>, GenerateError> {
    let locks: Vec<std::path::PathBuf> = source_roots
        .values()
        .map(|root| root.join("Cargo.lock"))
        .filter(|lock| lock.is_file())
        .collect();
    let [lock] = locks.as_slice() else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(lock).map_err(|source| GenerateError::Io {
        what: "the source's Cargo.lock",
        path: lock.clone(),
        source,
    })?;
    Ok(Some(FileEntry::text(
        paths::rel(&["Cargo.lock"])?,
        text,
        FileKind::Toml,
        Ownership::Generated,
    )))
}

/// The resolved product, beside the tree it produced.
///
/// Writing the lock into the output root rather than next to `product.gdl` is
/// what makes `gearbox lock gears` answerable from inside a generated tree, and
/// it keeps the answer profile-specific: three profiles produce three different
/// topologies from one description, so one lock next to the description could
/// only ever describe one of them.
///
/// # Errors
/// Returns [`GenerateError::Lock`] if the lock cannot be serialized.
pub fn lock_file(product: &ResolvedProduct) -> Result<FileEntry, GenerateError> {
    Ok(FileEntry::text(
        paths::rel(&["product.lock"])?,
        gearbox_lock::write_canonical(product)?,
        FileKind::Toml,
        // `Generated`, and marked read-only in the editor per ADR
        // `cpt-gearbox-adr-authoring-ownership-tiers`: `gearbox_lock::read`
        // verifies the hash on the way in, so a hand-edited lock fails to load
        // rather than quietly taking effect.
        Ownership::Generated,
    ))
}

/// The edition, rust-version and resolver a generated package declares.
///
/// Exposed so the process manifest writes the same literals the workspace does,
/// which is the point of them being constants rather than string literals at
/// two call sites.
pub const fn package_edition() -> &'static str {
    EDITION
}

pub const fn package_rust_version() -> &'static str {
    RUST_VERSION
}

/// Where `cargo build` in this tree puts binaries, relative to the tree root.
///
/// **One answer, used twice**, and that is the whole point of putting it here:
/// the host's configuration names the worker's executable, and `.cargo/config.toml`
/// tells cargo where to write it. Composing those separately is what broke --
/// the resolver wrote a path the description had expressed relative to *itself*,
/// while the runtime resolved it relative to the host's working directory, which
/// is this tree. The two bases differed by one level and the host looked for the
/// worker in a directory that does not exist.
///
/// `target` is Cargo's own default. A profile that declares `target_dir` gets
/// that instead, so the corpus is not rebuilt for every product -- and because
/// both consumers read this function, the two cannot disagree again.
pub fn target_dir(input: &GenerateInput<'_>) -> String {
    let profile = cargo_profile_dir(input);
    match shared_target_dir(input) {
        Some(shared) => format!("{shared}/{profile}"),
        None => format!("{CARGO_TARGET_DIR}/{profile}"),
    }
}

/// Cargo's own default output directory, named once.
///
/// Read by `target_dir` above and by `apply`'s orphan walk, which must not
/// descend into it -- the demo's `dev` profile has thirteen thousand files
/// under it, none of them this tool's. A profile that declares `target_dir`
/// puts the directory outside the output root entirely, so there is nothing
/// there to skip.
pub const CARGO_TARGET_DIR: &str = "target";

/// The declared shared target directory, expressed from this tree.
///
/// `None` when the profile declared none, or when no relative path from the
/// output root to it exists -- on which see [`cargo_config`], which then writes
/// nothing rather than a path cargo would resolve elsewhere.
fn shared_target_dir(input: &GenerateInput<'_>) -> Option<String> {
    let declared = input.lock.self_hosted.as_ref()?.target_dir.as_deref()?;
    let absolute = paths::normalize(&input.product_dir?.join(declared));
    let relative = paths::relative(input.out_root, &absolute)?;
    Some(paths::to_slash(&relative))
}

/// `.cargo/config.toml`, when the profile shares a target directory.
///
/// Measured, not assumed: a relative `build.target-dir` resolves against the
/// directory holding `.cargo`, so a tree-relative path here means cargo and the
/// generated configuration agree by construction.
///
/// # Errors
/// Returns [`GenerateError`] when the path is not a valid relative path.
pub fn cargo_config(input: &GenerateInput<'_>) -> Result<Option<FileEntry>, GenerateError> {
    let Some(shared) = shared_target_dir(input) else {
        return Ok(None);
    };
    let body = format!("{}[build]\ntarget-dir = \"{shared}\"\n", header("#"));
    Ok(Some(FileEntry::text(
        paths::rel(&[".cargo", "config.toml"])?,
        body,
        FileKind::Toml,
        Ownership::Generated,
    )))
}

/// Which Cargo profile directory the host should exec.
///
/// Kubernetes images are release artefacts. A `self_hosted` profile names the
/// Cargo profile in the description so two generates of the same product do not
/// drift with the operator's environment. Cargo's `dev` profile writes under
/// `debug/`.
fn cargo_profile_dir(input: &GenerateInput<'_>) -> String {
    cargo_profile_directory(
        input.lock.kubernetes.is_some(),
        input
            .lock
            .self_hosted
            .as_ref()
            .and_then(|h| h.cargo_profile.as_deref()),
    )
}

fn cargo_profile_directory(kubernetes: bool, declared: Option<&str>) -> String {
    if kubernetes {
        return "release".to_owned();
    }
    match declared {
        Some("dev") | None => "debug".to_owned(),
        Some(name) => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_profile_directory_maps_dev_and_defaults() {
        assert_eq!(cargo_profile_directory(true, None), "release");
        assert_eq!(cargo_profile_directory(false, None), "debug");
        assert_eq!(cargo_profile_directory(false, Some("dev")), "debug");
        assert_eq!(cargo_profile_directory(false, Some("release")), "release");
        assert_eq!(cargo_profile_directory(false, Some("ci")), "ci");
    }
}
