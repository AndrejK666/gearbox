//! The only writer.
//!
//! Two functions, and the split between them is the whole safety story.
//! [`plan`] reads the output tree and reports what applying would do; it writes
//! nothing, which is what makes `--dry-run` and `gearbox/generate/plan` a
//! preview of the real thing rather than a description of it
//! (`cpt-gearbox-fr-generate-preview`). [`apply_generate`] performs that plan.
//!
//! **The apply is staged.** Every file is prepared in memory -- including the
//! three-way merges, which are where a failure is actually likely -- and
//! nothing is written until every one of them has succeeded. ADR
//! `cpt-gearbox-adr-authoring-ownership-tiers` requires this, and the reason it
//! requires it is that the alternative leaves a tree that is half of one product
//! and half of another, which nothing downstream can detect.
//!
//! **The merge base.** An `OperatorOwned` file is reconciled against the copy of
//! *our previous output* cached in `.gearbox/<product>/.base/`, not against the
//! operator's file and not against nothing. That is what lets the merge tell
//! "the operator changed this line" from "we changed this line": without a base,
//! any difference looks like both.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gearbox_ir::{
    Diagnostic, DiagnosticCode, Diagnostics, FileAction, FileEntry, FilePlan, FileSet, Location,
    Ownership, RelPath,
};

use super::GenerateError;

/// Where the previous run's output is cached, relative to `<product>/`.
///
/// A sibling of the profile directories rather than a child, because the base
/// is per file path and the same operator file is not regenerated per profile.
const BASE_DIR: &str = ".base";

/// What one file would become, with the bytes to prove it.
struct Staged {
    path: RelPath,
    entry: FileEntry,
    action: FileAction,
    /// The bytes to write, which for a merged file are neither the proposal nor
    /// what is on disk.
    final_bytes: Vec<u8>,
}

/// What an apply did.
pub struct ApplyOutcome {
    pub plans: Vec<FilePlan>,
    pub diagnostics: Diagnostics,
    /// How many files were actually written.
    pub written: usize,
}

/// What applying `files` under `out_root` would do, without doing it.
///
/// # Errors
/// Returns [`GenerateError::Io`] when the existing tree cannot be read. A file
/// that cannot be read is not treated as absent: that would turn a permissions
/// problem into a silent overwrite.
pub fn plan(
    files: &FileSet,
    out_root: &Path,
    base_root: &Path,
) -> Result<(Vec<FilePlan>, Diagnostics), GenerateError> {
    let (staged, diagnostics) = stage(files, out_root, base_root)?;
    Ok((staged.iter().map(to_plan).collect(), diagnostics))
}

/// Write `files` under `out_root`, staging every byte first.
///
/// # Errors
/// Returns [`GenerateError::Io`] when the tree cannot be read or written.
/// Errors during staging leave the tree untouched; an error during the write
/// phase is reported with the path that failed.
pub fn apply_generate(
    files: &FileSet,
    out_root: &Path,
    base_root: &Path,
) -> Result<ApplyOutcome, GenerateError> {
    let (staged, diagnostics) = stage(files, out_root, base_root)?;

    // Refuse to write anything if any file conflicted. A partial apply beside
    // an unresolved conflict is the state ADR
    // `cpt-gearbox-adr-authoring-ownership-tiers` calls a half-written
    // repository, and the operator cannot tell it from a finished one.
    if diagnostics.has_errors() {
        return Ok(ApplyOutcome {
            plans: staged.iter().map(to_plan).collect(),
            diagnostics,
            written: 0,
        });
    }

    let staging = out_root.join(".apply-staging");
    let staging_base = base_root.join(".apply-staging");
    clear_tree(&staging);
    clear_tree(&staging_base);

    let published = publish_staged(&staged, out_root, base_root, &staging, &staging_base);

    clear_tree(&staging);
    clear_tree(&staging_base);

    Ok(ApplyOutcome {
        plans: staged.iter().map(to_plan).collect(),
        diagnostics,
        written: published?,
    })
}

fn publish_staged(
    staged: &[Staged],
    out_root: &Path,
    base_root: &Path,
    staging: &Path,
    staging_base: &Path,
) -> Result<usize, GenerateError> {
    let mut written = 0;
    for file in staged {
        if !file.action.writes() {
            continue;
        }
        write_file(&staging.join(file.path.as_str()), &file.final_bytes)?;
        if matches!(file.entry.ownership, Ownership::OperatorOwned) {
            write_file(&staging_base.join(file.path.as_str()), &file.entry.bytes)?;
        }
        written += 1;
    }

    let backup = out_root.join(".apply-backup");
    let backup_base = base_root.join(".apply-backup");
    clear_tree(&backup);
    clear_tree(&backup_base);

    let mut moved: Vec<(std::path::PathBuf, Option<std::path::PathBuf>)> = Vec::new();
    let published = (|| {
        for file in staged {
            if !file.action.writes() {
                continue;
            }
            publish_or_remember(
                &staging.join(file.path.as_str()),
                &out_root.join(file.path.as_str()),
                &backup.join(file.path.as_str()),
                &mut moved,
            )?;
            if matches!(file.entry.ownership, Ownership::OperatorOwned) {
                publish_or_remember(
                    &staging_base.join(file.path.as_str()),
                    &base_root.join(file.path.as_str()),
                    &backup_base.join(file.path.as_str()),
                    &mut moved,
                )?;
            }
        }
        Ok(written)
    })();

    if published.is_err() {
        for (dest, bak) in moved.into_iter().rev() {
            if let Some(bak) = bak {
                drop(std::fs::copy(bak, dest));
            } else {
                drop(std::fs::remove_file(dest));
            }
        }
    }
    clear_tree(&backup);
    clear_tree(&backup_base);
    published
}

fn publish_or_remember(
    from: &Path,
    to: &Path,
    backup: &Path,
    moved: &mut Vec<(std::path::PathBuf, Option<std::path::PathBuf>)>,
) -> Result<(), GenerateError> {
    // Copy, not rename: dest stays in place until `publish` atomically replaces
    // it. A crash between the two leaves the operator's file where it was; a
    // leftover `.apply-backup` is then harmless to `clear_tree`.
    let saved = if to.exists() {
        if let Some(parent) = backup.parent() {
            std::fs::create_dir_all(parent).map_err(|source| GenerateError::Io {
                what: "cannot create",
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::copy(to, backup).map_err(|source| GenerateError::Io {
            what: "cannot backup",
            path: to.to_path_buf(),
            source,
        })?;
        Some(backup.to_path_buf())
    } else {
        None
    };
    publish(from, to)?;
    moved.push((to.to_path_buf(), saved));
    Ok(())
}

/// Decide every file's fate and produce the bytes for it, touching nothing.
fn stage(
    files: &FileSet,
    out_root: &Path,
    base_root: &Path,
) -> Result<(Vec<Staged>, Diagnostics), GenerateError> {
    let mut diagnostics = Diagnostics::new();
    let mut staged = Vec::with_capacity(files.len());

    for entry in files {
        let target = out_root.join(entry.path.as_str());
        let existing = read_optional(&target)?;

        let (action, final_bytes) = match (entry.ownership, existing) {
            (_, None) => (FileAction::Create, entry.bytes.clone()),

            (Ownership::Generated, Some(current)) => {
                if current == entry.bytes {
                    (FileAction::Unchanged, current)
                } else {
                    (FileAction::Update, entry.bytes.clone())
                }
            }

            // The whole contract: written once, then it is the human's.
            (Ownership::GeneratedOnce, Some(current)) => (FileAction::Kept, current),

            (Ownership::OperatorOwned, Some(current)) => {
                let base = read_optional(&base_root.join(entry.path.as_str()))?;
                merge_operator_file(entry, &current, base.as_deref(), &mut diagnostics)
            }
        };

        staged.push(Staged {
            path: entry.path.clone(),
            entry: entry.clone(),
            action,
            final_bytes,
        });
    }

    let orphaned_crates = report_orphaned_crates(files, out_root, &mut diagnostics);
    report_orphaned_files(files, out_root, &orphaned_crates, &mut diagnostics);

    diagnostics.finish();
    Ok((staged, diagnostics))
}

/// The marker every generated file carries, minus the version that follows it.
const GENERATED_MARKER: &str = "GENERATED by gearbox";

/// Warn about generated crate directories this run no longer writes.
///
/// **The one thing `apply` can say about files it is not writing.** Its whole
/// vocabulary is create, update, keep and conflict -- there is no delete -- so
/// removing an `application(...)` from the description, or changing `layout`,
/// silently leaves the old crate on disk. The root `Cargo.toml` is rewritten
/// without it, and a package inside a workspace that neither includes nor
/// excludes it is exactly the state [`super::workspace`] exists to prevent. It
/// breaks `cargo metadata` and rust-analyzer and not a root `cargo build`,
/// which is what makes it the kind of thing nobody notices for a week.
///
/// Scoped to depth two -- `<dir>/<name>/Cargo.toml` -- because that is where
/// application crates live under any layout, and it is what lets one check
/// cover both causes without knowing which one happened.
///
/// A directory is only reported when its `Cargo.toml` carries the generated
/// marker. A crate the operator put in the output root by hand is theirs, and
/// telling them to delete it would be wrong.
///
/// Returns what it reported, so [`report_orphaned_files`] can stay quiet about
/// the files inside: one line naming a stale crate directory is the finding, and
/// three more naming its manifest, its `main.rs` and its link file would bury it.
fn report_orphaned_crates(
    files: &FileSet,
    out_root: &Path,
    diagnostics: &mut Diagnostics,
) -> Vec<String> {
    let planned: std::collections::BTreeSet<&str> = files
        .into_iter()
        .filter_map(|entry| entry.path.as_str().strip_suffix("/Cargo.toml"))
        .collect();

    let mut orphans = Vec::new();
    for dir in read_dirs(out_root) {
        // `.base`, `.apply-staging` and the profile's own dotfiles are ours.
        if dir
            .file_name()
            .is_some_and(|n| n.to_str().is_some_and(|n| n.starts_with('.')))
        {
            continue;
        }
        for crate_dir in read_dirs(&dir) {
            let manifest = crate_dir.join("Cargo.toml");
            let Ok(body) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            if !body.contains(GENERATED_MARKER) {
                continue;
            }
            let Ok(rel) = crate_dir.strip_prefix(out_root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if !planned.contains(rel.as_str()) {
                orphans.push(rel);
            }
        }
    }

    orphans.sort();
    for rel in &orphans {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GenOrphanedCrate,
                format!("`{rel}` holds a generated crate this product no longer builds"),
            )
            .with_help(format!(
                "the root `Cargo.toml` written by this run does not list it, so `cargo \
                     metadata` and rust-analyzer will refuse the workspace while it is there. \
                     Delete `{rel}` once you are sure nothing in it is yours -- generation \
                     cannot, because it has no delete path"
            ))
            .at(Location::file(format!(
                "{}/{rel}/Cargo.toml",
                out_root.display()
            ))),
        );
    }
    orphans
}

/// Warn about generated files this run no longer writes.
///
/// **The sibling of [`report_orphaned_crates`], because that check is
/// crate-shaped by construction.** It strips `/Cargo.toml` off the planned
/// paths and opens a manifest on disk, so `config/`, `docker/` and `helm/` are
/// walked into and dropped the moment no `Cargo.toml` is there. Renaming an
/// application under a Kubernetes profile therefore reported its crate and said
/// nothing about `config/<old>.yaml`, `docker/<old>/Dockerfile` or the nine
/// files of `helm/<product>/charts/<old>/` -- and renaming a `subchart` on its
/// own, which moves no crate at all, reported nothing whatsoever.
///
/// **Everything in the output root is walked except two things, and both are
/// named for a reason.** Deriving the set from the planned paths instead --
/// `apps`, `config`, `docker`, `helm` as they appear this run -- was the first
/// attempt and it had a hole big enough to matter: a profile changed from
/// `kubernetes` to `embedded` writes no `docker/` and no `helm/` at all, so
/// neither would be a planned segment and the entire stale tree, charts
/// included, would go unreported. The point of the check is exactly the case
/// where this run writes less than the last one did.
///
/// So the exclusions are explicit. Dot-prefixed directories are ours -- `.base`,
/// `.apply-staging`, `.apply-backup` -- skipped as the crate check skips them.
/// And [`super::workspace::CARGO_TARGET_DIR`] is cargo's: the demo's `dev`
/// profile has thirteen thousand files under it and none of them carry this
/// tool's marker, so walking it costs and finds nothing. The name comes from the
/// one function that already states it, so the two cannot drift.
///
/// Marker-gated like the crate check, and that gate does the real work. It is
/// what makes an operator's own file in the output root unnameable, and it is
/// why `helm/<product>/values.yaml` is never reported: the one file generation
/// hands over is the one file that carries no marker.
fn report_orphaned_files(
    files: &FileSet,
    out_root: &Path,
    orphaned_crates: &[String],
    diagnostics: &mut Diagnostics,
) {
    let planned: std::collections::BTreeSet<&str> =
        files.into_iter().map(|entry| entry.path.as_str()).collect();

    let mut orphans = Vec::new();
    for dir in read_dirs(out_root) {
        let skipped = dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.') || n == super::workspace::CARGO_TARGET_DIR);
        if skipped {
            continue;
        }
        collect_orphaned_files(&dir, out_root, &planned, orphaned_crates, &mut orphans);
    }

    orphans.sort();
    for rel in orphans {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GenOrphanedFile,
                format!("`{rel}` is a generated file this product no longer writes"),
            )
            .with_help(format!(
                "nothing this run produced accounts for it, so it describes a shape of the \
                 product that is gone -- a renamed application or subchart is the usual \
                 cause. Delete `{rel}` once you are sure nothing in it is yours -- \
                 generation cannot, because it has no delete path"
            ))
            .at(Location::file(format!("{}/{rel}", out_root.display()))),
        );
    }
}

/// Every marker-carrying file under `dir` that `planned` does not account for.
fn collect_orphaned_files(
    dir: &Path,
    out_root: &Path,
    planned: &std::collections::BTreeSet<&str>,
    orphaned_crates: &[String],
    orphans: &mut Vec<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        // Dotted *directories* are skipped and dotted files are not. The rule
        // the crate check states is about the output root's own bookkeeping
        // (`.base`, `.apply-staging`), which are directories; applying it to
        // files would exempt `docker/.dockerignore`, which this tool writes and
        // could therefore orphan. For a file the marker is the gate anyway.
        // Build caches and operator links are not generator-owned output.
        if path.is_symlink() {
            continue;
        }
        let dotted_dir = path.is_dir()
            && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with('.') || matches!(n, "node_modules" | "target" | "dist")
            });
        if dotted_dir {
            continue;
        }
        let Ok(rel) = path.strip_prefix(out_root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        // Already said once, as a directory. Naming its contents as well would
        // turn one finding into four.
        if orphaned_crates
            .iter()
            .any(|crate_dir| rel == *crate_dir || rel.starts_with(&format!("{crate_dir}/")))
        {
            continue;
        }
        if path.is_dir() {
            collect_orphaned_files(&path, out_root, planned, orphaned_crates, orphans);
            continue;
        }
        if planned.contains(rel.as_str()) {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        if body.contains(GENERATED_MARKER) {
            orphans.push(rel);
        }
    }
}

/// The immediate subdirectories of `dir`, or none when it cannot be read.
///
/// An unreadable output root is not an error here: `stage` has already reported
/// anything that matters about it, and a permissions problem must not turn this
/// warning into a failed generate.
fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// Reconcile an operator-owned file against the base we last wrote.
///
/// Three inputs, and the base is the one that carries the information: without
/// it every line that differs looks like it differs for both reasons at once,
/// and the only safe answer would be "conflict, always". With it, a line only
/// the operator moved is theirs, a line only we moved is ours, and a line both
/// moved is the genuine conflict `GBX0701` reports.
fn merge_operator_file(
    entry: &FileEntry,
    current: &[u8],
    base: Option<&[u8]>,
    diagnostics: &mut Diagnostics,
) -> (FileAction, Vec<u8>) {
    let conflict = |diagnostics: &mut Diagnostics, why: &str| {
        diagnostics.push(clobber(entry, why));
        (FileAction::Conflict, current.to_vec())
    };

    let Some(base) = base else {
        // No base means we have never written this file, yet it exists. It is
        // the operator's from before we arrived, and we have nothing to
        // attribute our own changes against. Refusing is the only honest
        // answer; adopting it would silently discard whatever they wrote.
        return if current == entry.bytes {
            (FileAction::Unchanged, current.to_vec())
        } else {
            conflict(
                diagnostics,
                "the file predates any output of ours, so there is no base to merge against",
            )
        };
    };

    if current == base {
        // Untouched since we wrote it, so it is ours to update.
        return if current == entry.bytes {
            (FileAction::Unchanged, current.to_vec())
        } else {
            (FileAction::Update, entry.bytes.clone())
        };
    }
    if base == entry.bytes {
        // We propose exactly what we proposed last time; every difference is
        // the operator's, so there is nothing to apply.
        return (FileAction::Unchanged, current.to_vec());
    }

    let (Ok(base_text), Ok(current_text), Ok(proposed_text)) = (
        std::str::from_utf8(base),
        std::str::from_utf8(current),
        std::str::from_utf8(&entry.bytes),
    ) else {
        return conflict(
            diagnostics,
            "the file is not UTF-8, so it cannot be merged line by line",
        );
    };

    match super::merge3::merge(base_text, current_text, proposed_text) {
        super::merge3::Merge::Merged(merged) if merged.as_bytes() == current => {
            (FileAction::Unchanged, current.to_vec())
        }
        super::merge3::Merge::Merged(merged) => (FileAction::Update, merged.into_bytes()),
        super::merge3::Merge::Conflict => conflict(
            diagnostics,
            "your edits and the regenerated content change the same lines",
        ),
    }
}

/// `GBX0701`.
fn clobber(entry: &FileEntry, why: &str) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::GenClobberOperatorFile,
        format!("`{}` was left untouched: {why}", entry.path),
        "the file is operator-owned, so generation never overwrites it; reconcile it by hand, \
         or delete it to accept the generated content wholesale",
    )
    .at(Location::file(format!("file://{}", entry.path)))
}

fn to_plan(staged: &Staged) -> FilePlan {
    FilePlan {
        path: staged.path.clone(),
        action: staged.action,
        ownership: staged.entry.ownership,
        kind: staged.entry.kind,
        blake3: format!("blake3:{}", blake3::hash(&staged.final_bytes).to_hex()),
        preview_available: staged.entry.kind != gearbox_ir::FileKind::Binary
            && std::str::from_utf8(&staged.final_bytes).is_ok(),
    }
}

/// The bytes at `path`, or `None` if nothing is there.
///
/// Only `NotFound` becomes `None`. A permissions error or a directory in the
/// way is returned, because treating either as absence would make the next step
/// an overwrite.
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, GenerateError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(GenerateError::Io {
            what: "cannot read",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), GenerateError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| GenerateError::Io {
            what: "cannot create",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, bytes).map_err(|source| GenerateError::Io {
        what: "cannot write",
        path: path.to_path_buf(),
        source,
    })
}

fn publish(from: &Path, to: &Path) -> Result<(), GenerateError> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|source| GenerateError::Io {
            what: "cannot create",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::rename(from, to).map_err(|source| GenerateError::Io {
        what: "cannot publish",
        path: to.to_path_buf(),
        source,
    })
}

fn clear_tree(path: &Path) {
    if path.is_file() {
        drop(std::fs::remove_file(path));
    } else {
        drop(std::fs::remove_dir_all(path));
    }
}

/// The directory a generated tree lives under when nobody names one.
pub const OUTPUT_DIR: &str = ".gearbox";

/// `.gearbox/<product>/<profile>/`, the default generated tree for a product.
///
/// Here rather than in each client because two of them compare against it: the
/// CLI writes the tree and `gearbox/product/lock` diffs the lock inside it. Two
/// spellings match until one of them changes, and then the lock is compared
/// against a directory nothing writes and reported as missing rather than stale.
///
/// Validated ids rather than strings, and that is the safety half: a product id
/// reaches the lock as a plain `String` lowered from `product(id = ...)`, and
/// [`Path::join`] on an absolute or `..`-bearing segment walks straight out of
/// `.gearbox/`.
#[must_use]
pub fn default_out_root(
    product: &gearbox_ir::ProductId,
    profile: &gearbox_ir::ProfileId,
) -> PathBuf {
    Path::new(OUTPUT_DIR)
        .join(product.as_str())
        .join(profile.as_str())
}

/// The base cache directory for a product, given its output root.
///
/// `.gearbox/<product>/<profile>/` -> `.gearbox/<product>/.base/`. Returns the
/// output root itself if it has no parent, which cannot happen for a path this
/// module builds but is a better answer than a panic.
#[must_use]
pub fn base_root_for(out_root: &Path) -> PathBuf {
    out_root
        .parent()
        .map_or_else(|| out_root.join(BASE_DIR), |product| product.join(BASE_DIR))
}

/// The base cache for an output root outside the workspace.
///
/// `<out>/.gearbox/base/`, inside the root rather than beside it: such a root
/// is a folder the operator chose, often a repository, and its parent is not
/// ours to write. Dot-prefixed, so the orphan checks skip it as they skip
/// `.base`.
#[must_use]
pub fn standalone_base_root(out_root: &Path) -> PathBuf {
    out_root.join(".gearbox").join("base")
}

/// Group a plan by action, for a one-line summary.
#[must_use]
pub fn summarize(plans: &[FilePlan]) -> BTreeMap<FileAction, usize> {
    let mut counts = BTreeMap::new();
    for plan in plans {
        *counts.entry(plan.action).or_insert(0) += 1;
    }
    counts
}
