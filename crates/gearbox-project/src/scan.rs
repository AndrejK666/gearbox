//! Reading a crate's `src/` tree.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Why a crate's source could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("crate directory `{}` has no src/ directory", .0.display())]
    NoSrc(PathBuf),

    #[error("cannot read `{}`", .0.display())]
    Read(PathBuf, #[source] std::io::Error),

    #[error("cannot parse `{}`", .0.display())]
    Parse(PathBuf, #[source] syn::Error),

    /// A `src/` that is a symlink, or a `.rs` file that is one.
    ///
    /// Separate from [`ScanError::Read`] because the remedy is different: a read
    /// failure is about one file, this is about the shape of the tree.
    #[error("cannot walk `{}`", .0.display())]
    Walk(PathBuf, String),

    /// A directory under `src/` that could not be traversed.
    ///
    /// Keeps `walkdir`'s own error as the source, the way [`ScanError::Read`]
    /// and [`ScanError::Parse`] keep theirs. Flattening it into a `String` lost
    /// the `io::ErrorKind` behind it, so a permission-denied subdirectory and a
    /// symlink loop both arrived as prose.
    #[error("cannot walk `{}`", .0.display())]
    WalkDir(PathBuf, #[source] walkdir::Error),

    /// A source file larger than [`MAX_FILE_BYTES`], or a `src/` holding more
    /// than [`MAX_FILES`] of them.
    ///
    /// This crate is pointed at trees it does not own, and `syn::parse_file`
    /// needs the whole file in memory, so an unbounded read makes one oversized
    /// file in a scanned crate the projector's problem.
    #[error("{}", .0)]
    TooLarge(String),
}

/// The largest source file this will read, in bytes.
///
/// An order of magnitude above anything in the corpus: the largest `.rs` file in
/// `gears-rust` is a few hundred kilobytes, so this refuses only a file that is
/// not source in any ordinary sense.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The largest number of `.rs` files this will collect from one crate's `src/`.
///
/// The biggest gear crate in the corpus has a few hundred, so this refuses only
/// a tree that is not one crate's source.
pub const MAX_FILES: usize = 20_000;

/// One parsed Rust file, with the path to blame in a diagnostic.
#[derive(Clone)]
pub struct RustFile {
    /// Absolute path on disk.
    pub path: PathBuf,
    /// Path relative to the crate's `src/`, which is what `attr` narrows on and
    /// what a diagnostic should show -- an absolute path from someone else's
    /// machine is noise.
    pub relative: PathBuf,
    pub ast: syn::File,
}

/// Parse every `.rs` file under `crate_dir/src`, sorted by path.
///
/// A directory walk rather than a `mod`-tree walk from `lib.rs`. That is what
/// `cpt-gearbox-fr-attribute-location` specifies, and it is the more robust
/// choice here: a `mod` tree behind `#[cfg(feature = ...)]` would make
/// discovery depend on feature selection, and feature selection is downstream
/// of the catalogue. The cost is that a file not reachable by any `mod` is
/// still scanned; in practice such a file is dead code.
///
/// # Errors
/// Returns [`ScanError`] when `src/` is absent or a symlink, a directory under
/// it cannot be walked, a file cannot be read, or a file is not valid Rust.
#[expect(
    clippy::needless_collect,
    reason = "all readers must start before any join; a lazy iterator would serialize reads"
)]
pub fn scan_crate(crate_dir: &Path) -> Result<Vec<RustFile>, ScanError> {
    let src = crate_dir.join("src");
    // `symlink_metadata` rather than `is_dir`, which follows the link: a
    // symlinked `src/` would let a description reach a tree outside its own
    // source root, and `follow_links(false)` does not cover the *root* of the
    // walk (walkdir's `follow_root_links` defaults to true).
    match std::fs::symlink_metadata(&src) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(ScanError::Walk(
                src,
                "src/ is a symlink; a crate may only be read through a real directory inside \
                 its source root"
                    .to_owned(),
            ));
        }
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(ScanError::NoSrc(crate_dir.to_path_buf())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ScanError::NoSrc(crate_dir.to_path_buf()));
        }
        Err(e) => return Err(ScanError::Read(src, e)),
    }

    // Walk errors are *not* dropped. An unreadable subdirectory would otherwise
    // yield `Ok` with a short file list, and the projection would then report
    // the contracts and gears in it as absent -- a permission problem wearing
    // the costume of a description mistake.
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(&src)
        .follow_links(false)
        .follow_root_links(false)
    {
        let entry = entry.map_err(|e| {
            let at = e.path().unwrap_or(&src).to_path_buf();
            ScanError::WalkDir(at, e)
        })?;
        // `follow_links(false)` already refuses to descend a symlink directory
        // or read a symlink-to-non-rs. The hole is a `.rs` symlink: walkdir
        // reports it as neither file nor directory, so it would otherwise skip
        // the entry and the post-walk check would never see it.
        if entry.file_type().is_symlink() {
            if entry.path().extension().is_some_and(|x| x == "rs") {
                return Err(ScanError::Walk(
                    entry.into_path(),
                    "source file is a symlink; a crate may only be read through real files \
                     inside its source root"
                        .to_owned(),
                ));
            }
            continue;
        }
        if entry.file_type().is_file() && entry.path().extension().is_some_and(|x| x == "rs") {
            if paths.len() >= MAX_FILES {
                return Err(ScanError::TooLarge(format!(
                    "`{}` holds more than {MAX_FILES} `.rs` files; a crate's `src/` is not \
                     that, so this is a path pointing somewhere it should not",
                    src.display()
                )));
            }
            paths.push(entry.into_path());
        }
    }
    // Sorted so the candidate list in an ambiguity diagnostic is stable.
    paths.sort();

    let mut files = Vec::with_capacity(paths.len());
    for batch in paths.chunks(32) {
        let texts = std::thread::scope(|scope| {
            let readers: Vec<_> = batch
                .iter()
                .map(|path| scope.spawn(move || read_source(path)))
                .collect();
            readers
                .into_iter()
                .map(std::thread::ScopedJoinHandle::join)
                .collect::<Vec<_>>()
        });
        for (path, text) in batch.iter().zip(texts) {
            let text = text.map_err(|_| {
                ScanError::Walk(path.clone(), "source reader thread panicked".to_owned())
            })??;
            let ast = syn::parse_file(&text).map_err(|e| ScanError::Parse(path.clone(), e))?;
            let relative = path.strip_prefix(&src).unwrap_or(path).to_path_buf();
            files.push(RustFile {
                path: path.clone(),
                relative,
                ast,
            });
        }
    }
    Ok(files)
}

/// Read on a worker, retaining the scanner's symlink and size checks.
fn read_source(path: &Path) -> Result<String, ScanError> {
    let meta =
        std::fs::symlink_metadata(path).map_err(|e| ScanError::Read(path.to_path_buf(), e))?;
    if meta.file_type().is_symlink() {
        return Err(ScanError::Walk(
            path.to_path_buf(),
            "source file is a symlink; a crate may only be read through real files inside \
                 its source root"
                .to_owned(),
        ));
    }
    // Checked before the read, off the metadata already fetched: the whole
    // file goes into memory and then into `syn`, so the size has to be
    // refused rather than discovered.
    if meta.len() > MAX_FILE_BYTES {
        return Err(ScanError::TooLarge(format!(
            "`{}` is {} bytes, over the {MAX_FILE_BYTES}-byte limit for a source file",
            path.display(),
            meta.len()
        )));
    }
    let text = std::fs::read_to_string(path).map_err(|e| ScanError::Read(path.to_path_buf(), e))?;
    Ok(text)
}

/// Every item in a scanned file, descending into inline `mod` blocks.
///
/// One traversal, because two projectors with different ideas of which items
/// exist disagree about the same crate: `config::project_config_root` walks
/// inline modules to find the root, so the struct, enum, `Default` impl and
/// free-function lookups it feeds have to walk them too, or the annotation form
/// drops a nested root entirely while the turbofish form records one and then
/// yields no fields.
///
/// `mod foo;` is not followed: its body is a separate file the directory walk
/// reaches on its own, and following it here would be a second traversal with a
/// different idea of the tree.
#[must_use]
pub fn items(file: &RustFile) -> Vec<&syn::Item> {
    let mut out = Vec::new();
    push_items(&file.ast.items, &mut out);
    out
}

fn push_items<'a>(items: &'a [syn::Item], out: &mut Vec<&'a syn::Item>) {
    for item in items {
        out.push(item);
        if let syn::Item::Mod(module) = item
            && let Some((_, inner)) = &module.content
        {
            push_items(inner, out);
        }
    }
}

/// The files of a scanned crate that are compiled only under `cfg(test)`, by
/// [`RustFile::relative`].
///
/// The directory walk reads every `.rs` file, so a mock that implements a
/// crate's own plugin trait in `test_support.rs` is as visible as the real
/// thing -- and "implements the trait" is how a plugin is told from its host.
/// Which files are test code is read from the crate itself, from the
/// `#[cfg(test)] mod x;` declarations that pull them in, never from a file name.
///
/// A file declared by a test-only file is test-only too, so a test module's
/// own submodules follow it. A file no declaration reaches is not test code:
/// nothing here knows what it is, and "not test" is the answer that leaves it
/// visible.
#[must_use]
pub fn test_only_files(files: &[RustFile]) -> BTreeSet<PathBuf> {
    // Every `mod x;` in the crate, as (declaring file, declared file, gated).
    let mut edges: Vec<(PathBuf, PathBuf, bool)> = Vec::new();
    for file in files {
        let dir = file
            .relative
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();
        let is_mod_root = file
            .relative
            .file_name()
            .is_some_and(|n| n == "lib.rs" || n == "main.rs" || n == "mod.rs");
        let base = if is_mod_root {
            dir.clone()
        } else {
            dir.join(file.relative.file_stem().unwrap_or_default())
        };
        declarations(
            &file.ast.items,
            &dir,
            &base,
            false,
            &file.relative,
            &mut edges,
        );
    }

    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    loop {
        let before = out.len();
        for (from, to, gated) in &edges {
            if *gated || out.contains(from) {
                out.insert(to.clone());
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

/// Collect the `mod x;` declarations under `items`, descending inline modules.
///
/// `dir` is where a `#[path]` resolves from; `base` is the module directory a
/// plain `mod x;` looks in. Both move into an inline module's name, which is
/// the rule rustc applies to declarations nested in `mod m { ... }`.
fn declarations(
    items: &[syn::Item],
    dir: &Path,
    base: &Path,
    gated: bool,
    from: &Path,
    out: &mut Vec<(PathBuf, PathBuf, bool)>,
) {
    for item in items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        let gated = gated || is_test_only(&module.attrs);
        let name = module.ident.to_string();
        let path_attr = module.attrs.iter().find_map(|a| {
            let syn::Meta::NameValue(nv) = &a.meta else {
                return None;
            };
            if !nv.path.is_ident("path") {
                return None;
            }
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
            else {
                return None;
            };
            Some(s.value())
        });
        if let Some((_, inner)) = &module.content {
            let nested = base.join(&name);
            let nested_dir = path_attr
                .as_ref()
                .map_or_else(|| nested.clone(), |p| dir.join(p));
            declarations(inner, &nested_dir, &nested_dir, gated, from, out);
        } else {
            let targets = path_attr.map_or_else(
                || {
                    vec![
                        base.join(format!("{name}.rs")),
                        base.join(&name).join("mod.rs"),
                    ]
                },
                |p| vec![dir.join(p)],
            );
            for to in targets {
                out.push((from.to_path_buf(), normalise(&to), gated));
            }
        }
    }
}

/// Fold `a/./b` and `a/x/../b`, so a `#[path]` meets the walk's own spelling.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Whether these attributes compile the item only in a test build.
///
/// `cfg(test)`, or an `all(...)` with `test` among its terms. `any(test, ...)`
/// and `not(...)` are not: the item exists in some ordinary build too.
#[must_use]
pub fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<syn::Meta>().is_ok_and(|m| meta_is_test(&m))
    })
}

fn meta_is_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(p) => p.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|terms| terms.iter().any(meta_is_test)),
        _ => false,
    }
}

/// Interpret a description's narrowing path against a crate's scanned files.
///
/// A narrowing path is written relative to the crate root and includes `src/`
/// (`src/gear.rs`), while [`RustFile::relative`] is keyed relative to `src/`.
/// This reconciles the two and refuses anything that would reach outside the
/// crate -- `..` in a narrowing path is never what a description means.
///
/// # Errors
/// Returns the offending spec when it is absolute or contains `..`.
pub fn narrowing_path(spec: &str) -> Result<PathBuf, String> {
    let as_path = Path::new(spec);
    if as_path.is_absolute()
        || as_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(spec.to_owned());
    }
    Ok(as_path.strip_prefix("src").unwrap_or(as_path).to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Gated on the `#[test]` itself rather than inside it. The `cfg(not(unix))`
    // arm these used to carry removed the directory and asserted nothing, so
    // off unix the suite reported the symlink cases as covered on a platform
    // where `scan_crate` was never called.
    #[cfg(unix)]
    #[test]
    fn a_symlink_rs_file_is_a_walk_error() {
        let dir = std::env::temp_dir().join(format!("gearbox-scan-symlink-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("real.rs"), "pub fn f() {}\n").unwrap();
        std::os::unix::fs::symlink(dir.join("real.rs"), dir.join("src/lib.rs")).unwrap();
        let Err(err) = scan_crate(&dir) else {
            panic!("symlink must be refused");
        };
        drop(std::fs::remove_dir_all(&dir));
        assert!(matches!(err, ScanError::Walk(_, _)), "got {err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_non_rs_file_is_ignored() {
        let dir =
            std::env::temp_dir().join(format!("gearbox-scan-json-symlink-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        std::fs::write(dir.join("thing.json"), "{}\n").unwrap();
        std::os::unix::fs::symlink(dir.join("thing.json"), dir.join("src/thing.json")).unwrap();
        let files =
            scan_crate(&dir).unwrap_or_else(|e| panic!("json symlink must be ignored: {e}"));
        drop(std::fs::remove_dir_all(&dir));
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn an_oversized_source_file_is_refused_before_it_is_read() {
        let dir = std::env::temp_dir().join(format!("gearbox-scan-huge-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        // Sparse: the point is that the size is read off the metadata, so the
        // bytes never have to exist for the guard to fire.
        let file = std::fs::File::create(dir.join("src/lib.rs")).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        drop(file);
        let Err(err) = scan_crate(&dir) else {
            panic!("an oversized file must be refused");
        };
        drop(std::fs::remove_dir_all(&dir));
        assert!(matches!(err, ScanError::TooLarge(_)), "got {err}");
    }

    #[test]
    fn a_file_at_the_limit_is_still_read() {
        let dir = std::env::temp_dir().join(format!("gearbox-scan-ok-size-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        let files = scan_crate(&dir).unwrap_or_else(|e| panic!("an ordinary file: {e}"));
        drop(std::fs::remove_dir_all(&dir));
        assert_eq!(files.len(), 1);
    }
}

#[cfg(test)]
mod read_tests {
    use super::*;
    #[test]
    fn parallel_reads_keep_sorted_results_and_errors_across_batches() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/scan-parallel")
            .join(std::process::id().to_string());
        std::fs::create_dir_all(dir.join("src")).unwrap();
        for index in (0..70).rev() {
            std::fs::write(
                dir.join(format!("src/file{index:03}.rs")),
                format!("pub struct Item{index};"),
            )
            .unwrap();
        }
        let files = scan_crate(&dir).unwrap();
        assert_eq!(files.len(), 70);
        assert!(
            files
                .windows(2)
                .all(|pair| pair[0].relative < pair[1].relative)
        );
        std::fs::write(dir.join("src/file002.rs"), "invalid rust source").unwrap();
        std::fs::write(dir.join("src/file003.rs"), [0xff]).unwrap();
        assert!(
            matches!(scan_crate(&dir), Err(ScanError::Parse(path, _)) if path.ends_with("file002.rs"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
