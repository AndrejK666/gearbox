//! Path arithmetic the generated manifests depend on.
//!
//! Pure: these functions compare path *components*, they never ask the
//! filesystem anything. That matters because the generated tree does not exist
//! yet when the relative dependency paths inside it are computed.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use gearbox_ir::RelPath;

use super::GenerateError;

/// `target` expressed relative to `from`, both absolute.
///
/// Returns `None` when no relative path exists -- different Windows drive
/// prefixes, or either side not absolute. The caller then has to fall back to an
/// absolute path rather than emit `../..` guesswork that resolves somewhere
/// unrelated.
///
/// Written out rather than taken from a crate: the only candidates
/// (`pathdiff`, `relative-path`) each add a dependency to this workspace for
/// twenty lines, and the semantics of the fallback -- which is the part that
/// actually matters here -- would still be ours to decide.
#[must_use]
pub fn relative(from: &Path, target: &Path) -> Option<PathBuf> {
    let from = portable(from);
    let target = portable(target);
    if !from.is_absolute()
        || !target.is_absolute()
        || from
            .components()
            .chain(target.components())
            .any(|c| c == Component::ParentDir)
    {
        return None;
    }

    let mut from_parts = from.components().peekable();
    let mut target_parts = target.components().peekable();

    // The prefix and root must agree, or the two paths are in different
    // filesystems and no number of `..` connects them.
    while let (Some(a), Some(b)) = (from_parts.peek(), target_parts.peek()) {
        if a != b {
            break;
        }
        if matches!(a, Component::Prefix(_) | Component::RootDir) {
            // Consumed as part of the shared root; keep going.
        }
        from_parts.next();
        target_parts.next();
    }

    let mut out = PathBuf::new();
    for component in from_parts {
        match component {
            Component::Normal(_) => out.push(".."),
            // A `..` still in an absolute path means the caller handed us a
            // path it never canonicalized. Refusing beats emitting a relative
            // path that is wrong in a way nothing will notice until `cargo
            // build` looks for a directory that is not there.
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => return None,
            Component::CurDir => {}
        }
    }
    for component in target_parts {
        if matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        ) {
            return None;
        }
        out.push(component);
    }

    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Some(out)
}

/// Longest common prefix of absolute paths, as components.
///
/// The docker build context is this directory: generated `Cargo.toml`
/// path-deps walk from the output tree into the source roots, so only an
/// ancestor of both is a context `COPY . .` can use. Returns `None` when
/// any path is relative, when the set is empty, or when the paths share
/// no prefix (different Windows drive letters).
///
/// Refuses to consult the filesystem: `..` in an input is a path that has
/// not been canonicalized, and agreeing with `relative` means a Dockerfile
/// `COPY` cannot name a different directory than the manifest's `path =`.
pub fn common_ancestor<'a, I>(paths: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = &'a Path>,
{
    let mut iter = paths.into_iter();
    let first = portable(iter.next()?);
    if !first.is_absolute() || first.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    let mut prefix = first.into_owned();
    for path in iter {
        let path = portable(path);
        if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
            return None;
        }
        let shared = prefix
            .components()
            .zip(path.components())
            .take_while(|(a, b)| a == b)
            .count();
        prefix = prefix.components().take(shared).collect();
        if !prefix.is_absolute() {
            return None;
        }
    }
    Some(prefix)
}

/// A path inside the generated tree, as a `RelPath`.
///
/// Always `/`-separated, whatever the host: the generated manifests and the
/// `FilePlan` a client renders are the same text on every platform, and a
/// backslash in a Cargo `path =` is not portable.
///
/// # Errors
/// Returns [`GenerateError::BadPath`] when the joined path is not a valid
/// relative path -- which for a generator means a process name that escaped
/// validation, so it is a refusal rather than a sanitization.
pub fn rel(parts: &[&str]) -> Result<RelPath, GenerateError> {
    let joined = parts.join("/");
    RelPath::new(&joined).map_err(|_| GenerateError::BadPath { path: joined })
}

/// A slash-separated rendering of `path`, for a Cargo manifest.
///
/// `Path::display` would emit backslashes on Windows, and Cargo accepts them,
/// but the generated file would then differ byte for byte between two machines
/// generating from the same lock -- which is exactly the determinism the lock
/// hash exists to guarantee.
#[must_use]
pub fn to_slash(path: &Path) -> String {
    let path = portable(path);
    let mut out = String::new();
    for component in path.components() {
        if component == Component::RootDir {
            out.push('/');
            continue;
        }
        if !out.is_empty() && !out.ends_with('/') {
            out.push('/');
        }
        let part = component.as_os_str().to_string_lossy();
        if matches!(component, Component::Prefix(_)) {
            out.push_str(&part.replace('\\', "/"));
        } else {
            out.push_str(&part);
        }
    }
    out
}

/// Compare ordinary and canonical Windows roots without touching the filesystem.
fn portable(path: &Path) -> Cow<'_, Path> {
    #[cfg(windows)]
    if path.is_absolute() {
        use std::path::Prefix;
        if let Some(Component::Prefix(prefix)) = path.components().next() {
            let root = match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => Some(PathBuf::from(format!(
                    "{}:\\",
                    char::from(drive.to_ascii_uppercase())
                ))),
                Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                    let mut root = PathBuf::from(r"\\");
                    root.push(server);
                    root.push(share);
                    Some(root)
                }
                _ => None,
            };
            if let Some(mut root) = root {
                for part in path.components().skip(1) {
                    if part != Component::RootDir {
                        root.push(part);
                    }
                }
                return Cow::Owned(root);
            }
        }
    }
    Cow::Borrowed(path)
}

/// `path` with `.` and `..` collapsed, without touching the filesystem.
///
/// [`relative`] compares components, so a target still carrying `..` shares a
/// shorter prefix than it really does and the tail comes out verbatim --
/// `../../Users/mike/.../products/payments-demo/../../../gears-rust`, which is
/// both absurd and machine-specific in a file that gets committed. Lexical
/// rather than `canonicalize` because the directory may not exist yet: a shared
/// Cargo target directory is created by the first build, not by us.
#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // A `..` with nothing to pop is kept: dropping it would silently
                // rewrite a path that climbs above its own root.
                if !out.pop() {
                    out.push(Component::ParentDir);
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute(path: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{path}"))
        } else {
            PathBuf::from(path)
        }
    }

    #[test]
    fn relative_walks_up_then_down() {
        let from = absolute("/a/b/c/d");
        let target = absolute("/a/x/y");
        assert_eq!(
            relative(&from, &target),
            Some(PathBuf::from("../../../x/y"))
        );
    }

    #[test]
    fn relative_to_self_is_dot() {
        let path = absolute("/a/b");
        assert_eq!(relative(&path, &path), Some(PathBuf::from(".")));
    }

    #[test]
    fn relative_refuses_a_relative_input() {
        assert!(relative(Path::new("a/b"), Path::new("/a")).is_none());
        assert!(relative(Path::new("/a"), Path::new("a/b")).is_none());
    }

    #[test]
    fn relative_refuses_an_uncanonicalized_from() {
        // `/a/b/../c` and `/a/c` are the same directory, but only after the
        // filesystem is consulted -- and this function may not consult it.
        assert!(relative(&absolute("/a/b/../c"), &absolute("/a/d")).is_none());
    }

    #[test]
    fn to_slash_never_emits_a_backslash() {
        let rendered = to_slash(Path::new("../../gears-rust/libs/toolkit"));
        assert_eq!(rendered, "../../gears-rust/libs/toolkit");
        assert!(!rendered.contains('\\'));
    }

    #[test]
    fn rel_refuses_an_escaping_component() {
        // `RelPath` rejects `..`; the generator must never produce a path that
        // writes outside the output root (GBX0702's condition).
        assert!(rel(&["..", "escape"]).is_err());
    }

    #[test]
    fn common_ancestor_stops_at_the_last_shared_component() {
        let a = absolute("/a/b/c/d");
        let b = absolute("/a/b/x");
        assert_eq!(
            common_ancestor([a.as_path(), b.as_path()]),
            Some(absolute("/a/b"))
        );
    }

    #[test]
    fn common_ancestor_of_one_path_is_itself() {
        let path = absolute("/a/b");
        assert_eq!(common_ancestor([path.as_path()]), Some(absolute("/a/b")));
    }

    #[test]
    fn common_ancestor_refuses_a_relative_input() {
        assert!(common_ancestor([Path::new("/a"), Path::new("b")]).is_none());
        assert!(common_ancestor(Vec::<&Path>::new()).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_roots_and_rendering() {
        let plain = Path::new(r"C:\gbx\out\apps\demo");
        let canonical = Path::new(r"\\?\C:\gbx\corpus\gear");
        assert_eq!(
            relative(plain, canonical),
            Some(PathBuf::from("../../../corpus/gear"))
        );
        assert_eq!(
            common_ancestor([plain, canonical]),
            Some(PathBuf::from(r"C:\gbx"))
        );
        assert_eq!(to_slash(canonical), "C:/gbx/corpus/gear");
        assert!(relative(plain, Path::new(r"D:\gear")).is_none());
        assert!(common_ancestor([plain, Path::new(r"D:\gear")]).is_none());
        let unc = Path::new(r"\\server\share\out");
        let verbatim = Path::new(r"\\?\UNC\server\share\src");
        assert_eq!(relative(unc, verbatim), Some(PathBuf::from("../src")));
        assert_eq!(to_slash(verbatim), "//server/share/src");
        assert!(common_ancestor([unc, Path::new(r"\\other\share\src")]).is_none());
    }
}
