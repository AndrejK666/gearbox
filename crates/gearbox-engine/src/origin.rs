//! `source = git(...)`: where a gear lives in a git repository, and whether the
//! checkout it was read from agrees.
//!
//! Locators in a description are looked for on disk first, relative to the
//! description; `source` is where a reader goes for what is not there. So the
//! two must describe the same place, and when the description is read from a
//! checkout that can be checked: one of its remotes is the declared repository
//! or a fork of it (same host and name, another owner), and
//! the gear sits at the declared `path` in it (GBX0121). A tree with no `.git`
//! -- an unpacked archive, a registry download -- has nothing to check against
//! and is not reported.

use std::path::{Path, PathBuf};

use gearbox_gdl::GearDecl;
use gearbox_gdl::engine::FileIdentity;
use gearbox_ir::{Diagnostic, DiagnosticCode, Diagnostics, GitOrigin, Location, RelPath};

/// The gear's declared origin, checked against the checkout under `root`.
pub fn project(
    root: &Path,
    identity: &FileIdentity,
    decl: &GearDecl,
    diagnostics: &mut Diagnostics,
) -> Option<GitOrigin> {
    let record = decl.source.as_ref()?;
    let url = record.url.clone()?;
    let at = Location::or_file(decl.declared_at.as_ref(), &identity.uri);
    let path = match record.subdir.as_deref().map(RelPath::new) {
        None => RelPath::here(),
        Some(Ok(path)) => path,
        Some(Err(e)) => {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::GdlEval,
                    format!("`source = git(path = ...)` is not a usable directory: {e}"),
                    "write the gear's directory inside the repository, from its root, e.g. \
                     `path = \"gears/bss/pricing\"`",
                )
                .at(at),
            );
            return None;
        }
    };
    let origin = GitOrigin {
        url,
        path,
        reference: record
            .tag
            .clone()
            .or_else(|| record.rev.clone())
            .or_else(|| record.branch.clone()),
    };
    let gdl_dir = root.join(identity.gdl_path.parent().as_str());
    check(&gdl_dir, &origin, &at, diagnostics);
    Some(origin)
}

/// Report where the checkout holding `gdl_dir` disagrees with `origin`.
fn check(gdl_dir: &Path, origin: &GitOrigin, at: &Location, diagnostics: &mut Diagnostics) {
    let Some(checkout) = Checkout::containing(gdl_dir) else {
        return;
    };

    if !checkout.remotes.is_empty()
        && !checkout
            .remotes
            .iter()
            .any(|remote| origin.same_or_fork(remote))
    {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GdlSourceMismatch,
                format!(
                    "`source = git(url = \"{}\")` is not a remote of the checkout this \
                     description is in (its remotes: {})",
                    origin.url,
                    checkout.remotes.join(", ")
                ),
            )
            .at(at.clone())
            .with_help(
                "correct the url; a checkout of a fork matches, one of another repository \
                 does not. A reader holding only the description follows `source` to the code",
            ),
        );
    }

    let actual = gdl_dir
        .canonicalize()
        .ok()
        .and_then(|dir| dir.strip_prefix(&checkout.root).ok().map(Path::to_path_buf))
        .and_then(|rel| {
            let spelled = rel.to_str()?.replace('\\', "/");
            if spelled.is_empty() {
                Some(RelPath::here())
            } else {
                RelPath::new(spelled).ok()
            }
        });
    if let Some(actual) = actual
        && actual != origin.path
    {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GdlSourceMismatch,
                format!(
                    "`source = git(path = \"{}\")`, but this description is at `{actual}` in its \
                     repository",
                    origin.path
                ),
            )
            .at(at.clone())
            .with_help(format!("write `path = \"{actual}\"`")),
        );
    }
}

/// The git checkout a directory is in: its top directory and its remotes' urls.
struct Checkout {
    root: PathBuf,
    remotes: Vec<String>,
}

impl Checkout {
    /// The nearest enclosing checkout, or `None` outside one.
    ///
    /// `.git` is a directory in a plain clone and a file (`gitdir: ...`) in a
    /// worktree or submodule, whose shared `config` is under its `commondir`.
    fn containing(dir: &Path) -> Option<Self> {
        let dir = dir.canonicalize().ok()?;
        let root = dir
            .ancestors()
            .find(|d| d.join(".git").exists())?
            .to_path_buf();
        let dot_git = root.join(".git");
        let git_dir = if dot_git.is_file() {
            let pointer = std::fs::read_to_string(&dot_git).ok()?;
            let target = pointer.trim().strip_prefix("gitdir:")?.trim();
            root.join(target)
        } else {
            dot_git
        };
        let common = std::fs::read_to_string(git_dir.join("commondir"))
            .map_or_else(|_| git_dir.clone(), |c| git_dir.join(c.trim()));
        let config = std::fs::read_to_string(common.join("config")).unwrap_or_default();
        Some(Self {
            root,
            remotes: remote_urls(&config),
        })
    }
}

/// Every `url` under a `[remote "..."]` section of a git config, in order.
fn remote_urls(config: &str) -> Vec<String> {
    let mut in_remote = false;
    let mut urls = Vec::new();
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_remote = line.starts_with("[remote ");
            continue;
        }
        if in_remote
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            urls.push(value.trim().to_owned());
        }
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::remote_urls;

    #[test]
    fn remote_urls_are_read_from_remote_sections_only() {
        let config = "[core]\n\turl = not-a-remote\n[remote \"origin\"]\n\turl = git@github.com:me/x.git\n\
                      \tfetch = +refs/heads/*:refs/remotes/origin/*\n[remote \"upstream\"]\n\
                      \turl = https://github.com/org/x.git\n[branch \"main\"]\n\tremote = origin\n";
        assert_eq!(
            remote_urls(config),
            ["git@github.com:me/x.git", "https://github.com/org/x.git"]
        );
    }
}
