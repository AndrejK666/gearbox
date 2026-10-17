//! Finding a gear's documents, by convention and by declaration.
//!
//! The platform keeps them at `gears/<name>/docs/`, while a `gear.gdl` sits in a
//! crate subdirectory one level below -- `gears/system/cluster/cluster/gear.gdl`
//! against `gears/system/cluster/docs/`. So the search looks beside the
//! description first and then in its parent, which covers 35 of 35 cases without
//! a line written in any description.
//!
//! A declared path always wins and is always checked: a hand-written path that
//! points nowhere is a typo (GBX0109). A file merely *not found* by convention is
//! not reported at all -- a gear may genuinely have no PRD, and warning about
//! that would put noise on 40 gears to catch nothing.
//!
//! A declared value may also be a URL, kept as written. And a declared path not
//! on disk is not a typo when the gear names its `source = git(...)`: it is the
//! same path in that repository, kept as a link. Disk always wins.

use std::path::{Path, PathBuf};

use gearbox_gdl::GearDecl;
use gearbox_gdl::engine::FileIdentity;
use gearbox_ir::{
    Diagnostic, DiagnosticCode, Diagnostics, DocUrls, GearDocs, GitOrigin, Located, Location,
    RelPath,
};

/// What GBX0109 suggests, wherever a declared path points at nothing.
const MISSING_HINT: &str = "the path is relative to the description's own directory; if the file \
                            is in another repository, declare the gear's `source = git(...)`";

/// The `OpenAPI` locations that actually occur in the platform.
///
/// Two shapes, both real: `docs/openapi.json` (mini-chat) and
/// `docs/api/openapi.yaml` (credstore). Only four gears of about forty check one
/// in at all -- the rest build the spec at runtime from their REST projections --
/// so absence here is the ordinary answer.
const OPENAPI_CANDIDATES: &[&str] = &[
    "docs/openapi.json",
    "docs/openapi.yaml",
    "docs/api/openapi.json",
    "docs/api/openapi.yaml",
];

/// Locate this gear's documents.
///
/// `gdl_dir` is the absolute directory holding the description; `root` is the
/// source root every returned path is made relative to.
pub fn project(
    root: &Path,
    gdl_dir: &Path,
    identity: &FileIdentity,
    decl: &GearDecl,
    origin: Option<&GitOrigin>,
    diagnostics: &mut Diagnostics,
) -> Option<GearDocs> {
    let uri = identity.uri.as_str();
    let declared = decl.docs.as_ref();
    // Computed once, where the record is in hand: both helpers below are handed
    // a path string rather than the declaration, so neither can find the span
    // for itself. The `docs(...)` call, not the individual argument -- `docs`
    // takes one `Evaluator`, so the call is the finest span there is.
    let at = Location::or_file(declared.and_then(|d| d.declared_at.as_ref()), uri);

    // Beside the description, then one level up. Order matters: a gear that
    // keeps its own `docs/` should not be shadowed by the family's.
    let mut bases = vec![gdl_dir.to_path_buf()];
    if let Some(parent) = gdl_dir.parent().filter(|p| is_own_family(p, gdl_dir)) {
        bases.push(parent.to_path_buf());
    }

    let find = Finder {
        root,
        gdl_dir,
        bases: &bases,
        origin,
        at: &at,
    };
    let (prd, prd_url) = find.one(
        declared.and_then(|d| d.prd.as_deref()),
        "prd",
        &["docs/PRD.md"],
        diagnostics,
    );
    let (design, design_url) = find.one(
        declared.and_then(|d| d.design.as_deref()),
        "design",
        &["docs/DESIGN.md"],
        diagnostics,
    );
    let (openapi, openapi_url) = find.one(
        declared.and_then(|d| d.openapi.as_deref()),
        "openapi",
        OPENAPI_CANDIDATES,
        diagnostics,
    );
    let (adr, adr_urls) = find.adr(declared, diagnostics);
    let docs = GearDocs {
        prd,
        design,
        adr,
        openapi,
        urls: DocUrls {
            prd: prd_url,
            design: design_url,
            adr: adr_urls,
            openapi: openapi_url,
        },
    };

    (!docs.is_empty()).then_some(docs)
}

/// Whether `parent` is this gear's own family directory rather than a bucket of
/// unrelated gears.
///
/// The guard matters because the climb is what makes the convention work at all:
/// `gears/system/cluster/cluster/gear.gdl` needs `gears/system/cluster/docs/`.
/// But `gears/system/api-gateway/gear.gdl` has `gears/system/` as its parent,
/// and a `gears/system/docs/` would then be claimed by whichever gear asked
/// first. No such directory exists today; this keeps it from becoming a silent
/// mis-attribution if one appears.
///
/// The test is structural: a family directory holds exactly one gear, so if any
/// *sibling* directory also holds a `gear.gdl`, the parent is a bucket.
fn is_own_family(parent: &Path, gdl_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return false;
    };
    !entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .any(|p| p.is_dir() && p != gdl_dir && p.join(crate::catalogue::GEAR_FILE).is_file())
}

/// Where a declared locator is.
enum Found {
    /// A URL as written, or the same path in the gear's git source.
    Url(String),
    /// On disk. `None` when it is outside the source root: it exists, so it is
    /// not the typo GBX0109 reports, but a lock cannot carry it.
    Local(Option<RelPath>),
    /// Neither: the caller reports GBX0109.
    Missing,
}

/// Find a declared locator: a URL as written, else on disk relative to
/// `gdl_dir` (when `accepts` it), else the same path in `origin`.
fn locate(
    root: &Path,
    gdl_dir: &Path,
    declared: &str,
    origin: Option<&GitOrigin>,
    accepts: fn(&Path) -> bool,
) -> Found {
    if declared.starts_with("https://") || declared.starts_with("http://") {
        return Found::Url(declared.to_owned());
    }
    let absolute = gdl_dir.join(declared);
    if accepts(&absolute) {
        return Found::Local(relative_to(root, &absolute));
    }
    origin
        .and_then(|origin| origin.web_url(declared))
        .map_or(Found::Missing, Found::Url)
}

/// What the document searches share for one description.
struct Finder<'a> {
    root: &'a Path,
    gdl_dir: &'a Path,
    bases: &'a [PathBuf],
    origin: Option<&'a GitOrigin>,
    at: &'a Location,
}

impl Finder<'_> {
    fn missing(&self, written: &str, diagnostics: &mut Diagnostics) {
        diagnostics.push(
            Diagnostic::error(
                DiagnosticCode::GdlMissingDocPath,
                format!("{written} points at no file"),
                MISSING_HINT,
            )
            .at(self.at.clone()),
        );
    }

    /// A declared path or URL if given, else the first candidate on disk.
    fn one(
        &self,
        declared: Option<&str>,
        field: &str,
        candidates: &[&str],
        diagnostics: &mut Diagnostics,
    ) -> (Option<RelPath>, Option<String>) {
        if let Some(declared) = declared {
            return match locate(
                self.root,
                self.gdl_dir,
                declared,
                self.origin,
                Path::is_file,
            ) {
                Found::Url(url) => (None, Some(url)),
                Found::Local(path) => (path, None),
                Found::Missing => {
                    self.missing(&format!("`docs({field} = \"{declared}\")`"), diagnostics);
                    (None, None)
                }
            };
        }
        let found = self
            .bases
            .iter()
            .flat_map(|base| candidates.iter().map(move |c| base.join(c)))
            .find(|p| p.is_file())
            .and_then(|p| relative_to(self.root, &p));
        (found, None)
    }

    /// Declared ADRs, else every `*.md` in the first `docs/ADR/` found.
    fn adr(
        &self,
        declared: Option<&gearbox_gdl::records::DocsRecord>,
        diagnostics: &mut Diagnostics,
    ) -> (Vec<RelPath>, Vec<String>) {
        if let Some(paths) = declared.map(|d| &d.adr).filter(|a| !a.is_empty()) {
            let (mut local, mut urls) = (Vec::new(), Vec::new());
            for declared in paths {
                match locate(
                    self.root,
                    self.gdl_dir,
                    declared,
                    self.origin,
                    Path::is_file,
                ) {
                    Found::Url(url) => urls.push(url),
                    Found::Local(path) => local.extend(path),
                    Found::Missing => {
                        self.missing(&format!("`docs(adr = [\"{declared}\"])`"), diagnostics);
                    }
                }
            }
            return (local, urls);
        }

        let Some(dir) = self
            .bases
            .iter()
            .map(|b| b.join("docs/ADR"))
            .find(|p| p.is_dir())
        else {
            return (Vec::new(), Vec::new());
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return (Vec::new(), Vec::new());
        };
        let mut found: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
            .collect();
        // Sorted so the catalogue is byte-identical across runs.
        found.sort();
        let local = found
            .iter()
            .filter_map(|p| relative_to(self.root, p))
            .collect();
        (local, Vec::new())
    }
}

/// Where the gear's declared tests are, each on disk or as a link.
///
/// The same search as a declared document: a URL as written, else on disk
/// relative to the description, else the same path in the gear's git source.
/// A directory counts: tests are usually a `tests/` tree, not one file.
pub fn tests(
    root: &Path,
    gdl_dir: &Path,
    identity: &FileIdentity,
    decl: &GearDecl,
    origin: Option<&GitOrigin>,
    diagnostics: &mut Diagnostics,
) -> Vec<Located> {
    let at = Location::or_file(decl.declared_at.as_ref(), &identity.uri);
    let mut out = Vec::new();
    for declared in &decl.tests {
        match locate(root, gdl_dir, declared, origin, Path::exists) {
            Found::Url(url) => out.push(Located {
                path: None,
                url: Some(url),
            }),
            Found::Local(Some(path)) => out.push(Located {
                path: Some(path),
                url: None,
            }),
            Found::Local(None) => {}
            Found::Missing => diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlMissingDocPath,
                    format!("`tests = [\"{declared}\"]` points at nothing"),
                    MISSING_HINT,
                )
                .at(at.clone()),
            ),
        }
    }
    out
}

/// Make an absolute path relative to the source root, forward-slashed.
///
/// Returns `None` when the file sits outside the root, which a `docs(...)`
/// climbing out of the tree could do. Silently dropping is right here: the path
/// exists, so it is not the typo GBX0109 reports, and a lock cannot carry it.
fn relative_to(root: &Path, absolute: &Path) -> Option<RelPath> {
    let canonical = absolute.canonicalize().ok()?;
    let relative = canonical.strip_prefix(root).ok()?;
    let text = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    RelPath::new(text).ok()
}
