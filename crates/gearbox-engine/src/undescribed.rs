//! Finding a gear that exists in Rust but has no `gear.gdl` (GBX0208).
//!
//! Only ever called on the error path -- a product selected a gear the catalogue
//! does not have -- and that is what makes the cost acceptable. It is also what
//! makes the diagnostic worth the code: the tree holds 44 `#[toolkit::gear]`
//! attributes and 14 descriptions, so "not in the catalogue" is far more often
//! "nobody has described it yet" than "you misspelled it". Those two need
//! different answers, and only this search can tell them apart.
//!
//! The search is textual before it is syntactic. A crate is a candidate only if
//! some file under its `src/` literally contains the quoted gear name, which
//! rules out almost the whole tree without parsing it; then that crate is
//! scanned properly, because a name inside a comment or a string is not a gear.

use std::path::{Path, PathBuf};

use gearbox_ir::GearId;

use crate::source::SourceRoot;

/// A gear declared in Rust with no description beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndescribedGear {
    /// Which source root it was found in.
    pub source: gearbox_ir::SourceId,
    /// The crate's directory, relative to that root.
    pub crate_dir: String,
    /// The file the attribute is in, relative to the crate.
    pub attr_file: PathBuf,
    /// 1-based line of the attribute.
    pub attr_line: usize,
    /// The crate's package name and library identifier, for the `cargo(...)`
    /// line the advice suggests.
    pub manifest: Option<gearbox_project::CrateManifest>,
}

/// Look for `wanted` in every crate under `roots` that has no `gear.gdl`.
///
/// Returns the first match in root order, then in directory order, so the answer
/// is stable. `None` means no crate in any root declares that gear -- which is
/// the genuinely-unknown case.
#[must_use]
pub fn find(roots: &[SourceRoot], wanted: &GearId) -> Option<UndescribedGear> {
    let quoted = format!("\"{}\"", wanted.as_str());
    for root in roots {
        for crate_dir in undescribed_crates(&root.root) {
            if !mentions(&crate_dir, &quoted) {
                continue;
            }
            let Ok(files) = gearbox_project::scan_crate(&crate_dir) else {
                continue;
            };
            let Some((attr_file, attr_line)) = declares(&files, wanted) else {
                continue;
            };
            return Some(UndescribedGear {
                source: root.id.clone(),
                crate_dir: relative(&root.root, &crate_dir),
                attr_file,
                attr_line,
                manifest: gearbox_project::project_manifest(&crate_dir).ok(),
            });
        }
    }
    None
}

/// Every directory under `root` holding a `Cargo.toml` and no `gear.gdl`.
///
/// A crate that *has* a description is skipped: if its gear were the one being
/// looked for, the catalogue would already hold it, so finding it here would
/// mean reporting GBX0208 for a gear that is described. Sorted for a stable
/// answer.
fn undescribed_crates(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .into_iter()
        // The catalogue's own pruning, so the two walks agree on what the tree is.
        .filter_entry(|entry| !crate::catalogue::is_pruned(entry))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() == "Cargo.toml")
        .filter_map(|entry| entry.path().parent().map(Path::to_path_buf))
        .filter(|dir| !dir.join("gear.gdl").exists())
        .collect();
    out.sort();
    out
}

/// Whether any `.rs` file under the crate's `src/` contains `needle` verbatim.
///
/// The cheap gate. Reading bytes and looking for a substring is orders of
/// magnitude less work than `syn::parse_file`, and a gear name cannot be present
/// in an attribute without being present in the text.
fn mentions(crate_dir: &Path, needle: &str) -> bool {
    let src = crate_dir.join("src");
    if !src.is_dir() {
        return false;
    }
    walkdir::WalkDir::new(&src)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "rs"))
        .any(|entry| {
            std::fs::read_to_string(entry.path())
                .is_ok_and(|text| text.contains(needle) && text.contains("toolkit::gear"))
        })
}

/// The file and line of a `#[toolkit::gear(name = wanted)]` in a scanned crate.
///
/// The name is read through `project_gear`, the same projection the catalogue
/// uses, so a gear found here and a gear loaded normally cannot disagree about
/// what it is called.
fn declares(files: &[gearbox_project::RustFile], wanted: &GearId) -> Option<(PathBuf, usize)> {
    gearbox_project::gear_attribute_sites(files)
        .iter()
        .find_map(|site| {
            let projected = gearbox_project::project_gear(site).ok()?;
            (projected.name == wanted.as_str()).then(|| (site.relative.clone(), site.line))
        })
}

fn relative(root: &Path, dir: &Path) -> String {
    dir.strip_prefix(root)
        .unwrap_or(dir)
        .to_string_lossy()
        .into_owned()
}
