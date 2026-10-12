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

/// A `--root` value: `DIR`, or `ID=DIR` to name the source explicitly.
///
/// `ID=DIR` is how a caller that checked several sources out itself -- a
/// platform corpus and an organization's own gears, each at its own tag --
/// says which checkout is which `source(id = ...)` of the product. The id is
/// taken only when the text before the first `=` is a valid source id, so a
/// directory whose name happens to contain `=` is still a directory; write it
/// as `./a=b` to be sure.
#[must_use]
pub fn parse_root_arg(arg: &Path) -> (Option<SourceId>, PathBuf) {
    if let Some(text) = arg.to_str()
        && let Some((id, dir)) = text.split_once('=')
        && !dir.is_empty()
        && let Ok(id) = SourceId::new(id)
    {
        return (Some(id), PathBuf::from(dir));
    }
    (None, arg.to_path_buf())
}

/// Rename roots after the product's own `source(id, at = path(...))`.
///
/// A root opened from a bare `--root DIR` is named after its directory, while
/// the product names the same checkout in its `sources`. When the two point at
/// the same directory, the product's id is the one `use_gear(source = ...)`
/// means, so the root takes it. Only roots in `adoptable` are renamed -- an
/// id given explicitly is the caller's word and stays -- and never onto an id
/// another root already carries.
pub fn adopt_product_source_ids(
    roots: &mut [SourceRoot],
    adoptable: &[bool],
    declared: &std::collections::BTreeMap<SourceId, gearbox_ir::SourceDecl>,
    product_dir: &Path,
) {
    for (id, decl) in declared {
        let gearbox_ir::SourceDecl::Path { at, .. } = decl else {
            continue;
        };
        let Ok(dir) = product_dir.join(at).canonicalize() else {
            continue;
        };
        if roots.iter().any(|r| &r.id == id) {
            continue;
        }
        if let Some(root) = roots
            .iter_mut()
            .zip(adoptable)
            .find(|(root, adoptable)| **adoptable && root.root == dir)
            .map(|(root, _)| root)
        {
            root.id = id.clone();
        }
    }
}

/// The commit a Git checkout at `dir` has checked out, read from `.git`.
///
/// Read rather than asked of `git`: the engine runs where no `git` may be
/// installed, and the three places a commit can be written -- a detached
/// `HEAD`, a loose ref, `packed-refs` -- are plain text. A worktree's `.git`
/// file and its `commondir` are followed. `None` when `dir` is not the top of
/// a checkout or the ref cannot be found; the caller then records what it
/// read instead.
#[must_use]
pub fn checkout_commit(dir: &Path) -> Option<String> {
    let dot_git = dir.join(".git");
    let git_dir = if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text.trim().strip_prefix("gitdir:")?.trim();
        dir.join(target)
    } else {
        dot_git
    };
    let common_dir = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map_or_else(|| git_dir.clone(), |c| git_dir.join(c.trim()));

    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else {
        return is_object_id(head).then(|| head.to_owned());
    };
    for base in [&git_dir, &common_dir] {
        if let Ok(text) = std::fs::read_to_string(base.join(reference)) {
            let id = text.trim();
            if is_object_id(id) {
                return Some(id.to_owned());
            }
        }
    }
    let packed = std::fs::read_to_string(common_dir.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (id, name) = line.split_once(' ')?;
        (name.trim() == reference && is_object_id(id)).then(|| id.to_owned())
    })
}

/// A SHA-1 or SHA-256 object id, in hex.
fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// [`lock_sources`], told what the product declared for each source.
///
/// A source the product declares as `git(url, tag | rev | branch)` and the
/// caller checked out itself is recorded as what it is rather than as the
/// directory it happened to land in: `kind = "git"`, the URL with the declared
/// reference in Cargo's spelling (`?tag=v1.2.0`), and as its digest the commit
/// the checkout is at, `git:<sha>`. A temporary checkout path means nothing on
/// another machine; the URL and the commit are what reproduce it. When the
/// checkout carries no readable `.git`, the content digest stays.
#[must_use]
pub fn lock_sources_declared(
    roots: &[SourceRoot],
    catalogue: &gearbox_ir::Catalogue,
    product_file: &Path,
    declared: &std::collections::BTreeMap<SourceId, gearbox_ir::SourceDecl>,
) -> std::collections::BTreeMap<SourceId, ResolvedSource> {
    let mut sources = lock_sources(roots, catalogue, product_file);
    for root in roots {
        let Some(gearbox_ir::SourceDecl::Git {
            url,
            tag,
            rev,
            branch,
            ..
        }) = declared.get(&root.id)
        else {
            continue;
        };
        let Some(resolved) = sources.get_mut(&root.id) else {
            continue;
        };
        let query: Vec<String> = [("tag", tag), ("rev", rev), ("branch", branch)]
            .into_iter()
            .filter_map(|(key, value)| value.as_ref().map(|v| format!("{key}={v}")))
            .collect();
        resolved.kind = SourceKind::Git;
        resolved.location = if query.is_empty() {
            url.clone()
        } else {
            format!("{url}?{}", query.join("&"))
        };
        if let Some(commit) = checkout_commit(&root.root) {
            resolved.digest = format!("git:{commit}");
        }
    }
    sources
}
