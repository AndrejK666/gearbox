//! A declared source, resolved to a directory on disk.

use std::path::{Path, PathBuf};

use gearbox_ir::{ResolvedSource, SourceId, SourceKind};

/// Why a source root could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum SourceRootError {
    #[error("source root `{}` does not exist", .0.display())]
    Missing(PathBuf),

    #[error("source root `{}` is not a directory", .0.display())]
    NotADirectory(PathBuf),

    #[error("cannot canonicalize source root `{}`", .0.display())]
    Canonicalize(PathBuf, #[source] std::io::Error),
}

/// Default ids for a set of source roots, in the order given.
///
/// The rule is the directory's own name, lowercased -- what a reader would guess
/// and what both the CLI and the RPC server have always used. What is added here
/// is that the *set* is made distinct: a repeat is suffixed `-2`, `-3`, matching
/// the convention [`gearbox_ir::ApplicationId`] already documents for the same
/// problem.
///
/// Distinctness is not cosmetic. `~/a/gears` and `~/b/gears` are both `gears`,
/// which is the arrangement anyone keeping two checkouts side by side ends up
/// with, and the id is an identity: it is half the client's row key, and it is
/// what a `product.gdl` names in `sources`. Two roots sharing one id and the same
/// relative layout meant the second root's gears silently replaced the first's --
/// one tree, half the gears, and paths resolved against whichever root was
/// reported last, with no diagnostic anywhere.
///
/// Suffixed rather than refused, because a duplicate basename is a legitimate
/// layout and failing the second root would take a working setup away to enforce
/// a naming rule nobody agreed to.
///
/// Note that an id which is not valid kebab-case is returned unchanged: rejecting
/// it is [`SourceId`]'s job, and doing it here as well would mean two places that
/// have to agree on what a valid id is.
#[must_use]
pub fn default_source_ids(paths: &[impl AsRef<Path>]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::with_capacity(paths.len());
    for path in paths {
        let base = default_source_id(path.as_ref());
        let mut candidate = base.clone();
        let mut nth = 1_u32;
        while ids.contains(&candidate) {
            nth += 1;
            candidate = format!("{base}-{nth}");
        }
        ids.push(candidate);
    }
    ids
}

/// The directory's own name, lowercased, or `local` when it has none.
///
/// `local` rather than an error for the case a path has no final component at
/// all -- `/`, or a relative path that canonicalizes to nothing usable. It is a
/// last resort, not a default anyone should meet.
fn default_source_id(path: &Path) -> String {
    path.canonicalize()
        .ok()
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().to_lowercase())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "local".to_owned())
}

/// A source root: an id, and the directory its `gear.gdl` paths are relative to.
#[derive(Debug, Clone)]
pub struct SourceRoot {
    pub id: SourceId,
    /// Canonicalized, so every discovered path can be made relative to it
    /// without `..` ambiguity.
    pub root: PathBuf,
    /// The location as the operator wrote it, preserved for the lock.
    pub declared_location: String,

    /// Sibling crates reachable from this root, by crate name.
    ///
    /// **Empty for a directory root, and populated for a registry one.** A
    /// `gear.gdl` locates its SDK with `path = "../../authn-resolver-sdk"`,
    /// which is true inside the monorepo and meaningless inside an unpacked
    /// package -- the sibling is a separate published crate, sitting wherever
    /// cargo put it. The same declaration carries `crate_name`, so identity is
    /// enough; this is the table that turns identity back into a directory.
    ///
    /// Consulted only when it has the name. A directory root therefore behaves
    /// exactly as before, which matters: resolving by name everywhere would stop
    /// a wrong path with a right name from being caught.
    pub siblings: std::collections::BTreeMap<String, std::path::PathBuf>,
}

impl SourceRoot {
    /// Open `path` as the root of source `id`.
    ///
    /// # Errors
    /// Returns [`SourceRootError`] if the path is missing, is not a directory,
    /// or cannot be canonicalized.
    pub fn open(id: SourceId, path: impl AsRef<Path>) -> Result<Self, SourceRootError> {
        let path = path.as_ref();
        let declared_location = path.display().to_string();

        if !path.exists() {
            return Err(SourceRootError::Missing(path.to_path_buf()));
        }
        if !path.is_dir() {
            return Err(SourceRootError::NotADirectory(path.to_path_buf()));
        }
        let root = path
            .canonicalize()
            .map_err(|e| SourceRootError::Canonicalize(path.to_path_buf(), e))?;

        Ok(Self {
            id,
            root,
            declared_location,
            siblings: std::collections::BTreeMap::new(),
        })
    }

    /// The `ResolvedSource` this root becomes, before anything has been read.
    ///
    /// `digest` is [`DIGEST_UNREAD`], because nothing has been read yet and a
    /// digest is a statement about content. The catalogue loader replaces it with
    /// [`content_digest`] once discovery has found the descriptions -- the reader
    /// is the only thing in a position to say what was read.
    ///
    /// `location` is the operator's own spelling, which is what a person reading a
    /// catalogue wants. A **lock** needs the opposite: see [`lock_sources`], which
    /// rewrites it relative to the description so that two clients spelling the
    /// same root differently still produce the same bytes.
    #[must_use]
    pub fn to_resolved(&self) -> ResolvedSource {
        ResolvedSource {
            id: self.id.clone(),
            kind: SourceKind::Path,
            location: self.declared_location.clone(),
            digest: DIGEST_UNREAD.to_owned(),
        }
    }
}

/// The digest of a source nothing has read yet.
///
/// Named rather than spelled inline so that a lock carrying it is recognisable as
/// a lock assembled without a catalogue scan, which is a bug rather than a state
/// worth supporting.
pub const DIGEST_UNREAD: &str = "unread";

/// A digest of the descriptions actually read under a source root.
///
/// This is the field's documented job -- "what was actually read... what makes a
/// lock reproducible rather than merely repeatable" -- and it used to be
/// `path:<location>`, the caller's own spelling of the path. That made
/// `lock_hash` depend on *how the root was named*: the CLI run from the
/// repository declared `../gears-rust` and Studio's backend declared an absolute
/// path, so one product and one profile serialised to two different locks. A hash
/// whose job is to answer "did anything change" cannot depend on who asked.
///
/// Over paths **and** bytes, both sorted: a description that moves is a change,
/// and so is one that is edited in place. Relative paths, so the digest does not
/// carry the machine it was computed on. Lengths are written before each field so
/// that no rearrangement of the same bytes can collide -- without them,
/// (`ab`, `c`) and (`a`, `bc`) hash alike.
///
/// Unreadable files are folded in by name with a marker instead of being skipped.
/// Skipping would make a description that cannot be read indistinguishable from
/// one that is absent, and the catalogue reports that case as a diagnostic
/// separately.
#[must_use]
pub fn content_digest(root: &Path, files: &[PathBuf]) -> String {
    let mut entries: Vec<(String, Option<Vec<u8>>)> = files
        .iter()
        .map(|file| {
            let relative = file
                .strip_prefix(root)
                .unwrap_or(file)
                .to_string_lossy()
                // `\` on Windows would otherwise give a different digest for the
                // same tree.
                .replace('\\', "/");
            (relative, std::fs::read(file).ok())
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = blake3::Hasher::new();
    for (path, bytes) in &entries {
        hasher.update(&u64::try_from(path.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(path.as_bytes());
        match bytes {
            Some(bytes) => {
                hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
                hasher.update(bytes);
            }
            None => {
                hasher.update(b"\xffunreadable");
            }
        }
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// Which source root owns `path`, by the rule the catalogue loader applies.
///
/// **The first root in the list that contains the path, not the innermost one.**
/// [`crate::load_catalogue`] walks the roots in the order they were given and
/// attributes each `gear.gdl` it finds to the root it was walked from; two roots
/// that both contain one description therefore declare the same gear twice, and
/// the first declaration is the one that stays in the catalogue (the second is
/// `GBX0203`). So the earliest root is the boundary a `load()` in that file is
/// really resolved against, and anything else -- an editor picking the deepest
/// match, say -- evaluates the file against a boundary no catalogue entry uses,
/// which shows up as a `load()` underlined in the editor and accepted on disk.
///
/// Here rather than in each caller because it is the loader's rule: a second
/// spelling of it is a second answer, and the two diverge without either side
/// changing.
///
/// `path` must already be absolute and free of `..`, which is what makes
/// [`Path::starts_with`] a containment test rather than a spelling test.
/// Roots opened by [`SourceRoot::open`] are canonical.
#[must_use]
pub fn owning_source_root<'a>(roots: &'a [SourceRoot], path: &Path) -> Option<&'a SourceRoot> {
    roots.iter().find(|root| path.starts_with(&root.root))
}

/// The roots a product description reaches, named the way it names them.
///
/// **A `path(...)` source is the description's own statement of where a source
/// is**, relative to the description, so the roots it is resolved against have
/// to carry it under the id it was declared with. They did not: a root's
/// default id is its directory's own name after canonicalizing, so a product
/// that declares `source(id = "studio-web", at = path("../.."))` and is
/// generated from a checkout called `repo` -- or through a link named
/// `studio-web` that resolves to `repo` -- had its gears scanned under `repo`.
/// Gears are found by name, so the backend side never noticed; the frontend
/// looks its packages up by source id, and refused (`GBX0720 source studio-web
/// is unavailable`) a source the description had located exactly.
///
/// Two rules, one per case:
///
/// * a root already given **at the same canonical directory** takes the
///   declared id, unless the operator named it explicitly (`explicit_ids`);
/// * a declared path given as no root at all is **opened from the description**,
///   so `gearbox generate --product product.gdl` reaches a product's own path
///   sources without a `--root` that only repeats what the description says.
///
/// Left alone: a declared path inside or around a root that was given (its
/// gears would be scanned twice), a declared id another root already carries (the roots the
/// operator gave win, and the catalogue keeps reporting what it finds), a path
/// that does not exist (the source is simply not there, as before), and every
/// non-path source (fetching is not this tool's job; its root arrives as a
/// `--root`).
#[must_use]
pub fn with_declared_paths(
    mut roots: Vec<SourceRoot>,
    sources: &std::collections::BTreeMap<gearbox_ir::SourceId, gearbox_ir::SourceDecl>,
    product_file: &Path,
    explicit_ids: bool,
) -> Vec<SourceRoot> {
    let base = product_file.parent().unwrap_or(Path::new("."));
    for (id, decl) in sources {
        let gearbox_ir::SourceDecl::Path { at, .. } = decl else {
            continue;
        };
        let Ok(dir) = base.join(at).canonicalize() else {
            continue;
        };
        if roots.iter().any(|r| &r.id == id) {
            continue;
        }
        if let Some(root) = roots.iter_mut().find(|r| r.root == dir) {
            if !explicit_ids {
                root.id = id.clone();
            }
            continue;
        }
        // Overlapping a root that was given would scan its gears twice
        // (`GBX0203`): the operator's roots say where those gears are.
        let overlaps = roots
            .iter()
            .any(|r| r.root.starts_with(&dir) || dir.starts_with(&r.root));
        if !overlaps && let Ok(root) = SourceRoot::open(id.clone(), &dir) {
            roots.push(root);
        }
    }
    roots
}

/// The `sources` map a lock records.
///
/// Two differences from what a catalogue carries, and both exist so that the same
/// product resolved by two clients produces the same bytes:
///
/// * **`location` is relative to the description**, not as declared. A lock is
///   about one `product.gdl` and is written beside it, so a path relative to that
///   description means the same thing on every machine -- while an absolute path
///   means nothing on any other, and a path relative to a working directory
///   depends on where the command was run.
/// * **`digest` comes from the catalogue**, which is the thing that read the
///   files. Recomputing it here would walk the tree a second time and could
///   disagree with the catalogue the lock was resolved against.
///
/// A root the catalogue never scanned keeps [`DIGEST_UNREAD`]; that is visible in
/// the lock rather than papered over.
#[must_use]
pub fn lock_sources(
    roots: &[SourceRoot],
    catalogue: &gearbox_ir::Catalogue,
    product_file: &Path,
) -> std::collections::BTreeMap<gearbox_ir::SourceId, ResolvedSource> {
    let base = product_file.parent().unwrap_or(Path::new("."));
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());

    roots
        .iter()
        .map(|root| {
            let mut resolved = root.to_resolved();
            resolved.location = relative_to(&base, &root.root);
            if let Some(scanned) = catalogue.sources.get(&root.id) {
                resolved.digest.clone_from(&scanned.digest);
            }
            (root.id.clone(), resolved)
        })
        .collect()
}

/// `target` expressed from `base`, with `..` for each level up.
///
/// Hand-rolled rather than pulled in as a dependency: the inputs are two
/// canonical absolute paths, which is the only case this has to be right for.
fn relative_to(base: &Path, target: &Path) -> String {
    let mut base_parts = base.components().peekable();
    let mut target_parts = target.components().peekable();
    while base_parts.peek().is_some() && base_parts.peek() == target_parts.peek() {
        base_parts.next();
        target_parts.next();
    }
    let up = base_parts.count();
    let down: Vec<String> = target_parts
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();

    let mut parts: Vec<String> = std::iter::repeat_n("..".to_owned(), up).collect();
    parts.extend(down);
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}
