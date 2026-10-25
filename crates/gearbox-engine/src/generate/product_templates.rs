//! A product's own copy of every template.
//!
//! Generation writes each key in [`KEYS`] into the product's templates
//! directory as `<key>.jinja` -- the directory `product(templates = ...)`
//! names, else `templates/` beside the description -- so the templates a
//! product is generated from are files someone can open and edit, not strings
//! in a binary. Beside each copy, `.base/<key>.jinja` holds the builtin it was
//! last written from.
//!
//! **The base is what tells an edit from a new builtin**, the way the merge base
//! does for an operator-owned file in [`super::apply`]:
//!
//! * no copy: the builtin is written;
//! * the copy is its base: nobody edited it, so it follows a new builtin;
//! * the copy differs from its base and the builtin moved: the builtin's change
//!   is merged into the edit; where the two overlap, the copy is kept as it is
//!   and reported (GBX0712), and it still renders;
//! * a copy with no base -- written by hand before generation kept one -- is
//!   kept unless it is the builtin, and reported, because nothing says which
//!   lines are the edit.
//!
//! Deleting a copy takes the builtin back. [`sync`] only reads; the bytes it
//! decided on are written by [`TemplateSync::write`], so a plan and an apply see
//! the same templates.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gearbox_ir::{
    Diagnostic, DiagnosticCode, Diagnostics, FileAction, ProductTemplate, TemplateStatus,
};

use super::GenerateError;
use super::merge3::{self, Merge};
use super::templates::{KEYS, TemplateSet};

/// Where each copy's base lives, inside the templates directory.
pub const BASE_DIR: &str = ".base";

/// What generation will do to a product's templates, and the set it renders.
#[derive(Debug)]
pub struct TemplateSync {
    /// Every template, in key order.
    pub templates: Vec<ProductTemplate>,
    pub diagnostics: Diagnostics,
    /// The sources generation renders from: each copy as it will be.
    pub set: TemplateSet,
    writes: Vec<(PathBuf, String)>,
}

impl TemplateSync {
    /// Write the copies and bases [`sync`] decided on.
    ///
    /// # Errors
    /// Returns [`GenerateError::Io`] when a directory or file cannot be written.
    pub fn write(&self) -> Result<usize, GenerateError> {
        for (path, body) in &self.writes {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| GenerateError::Io {
                    what: "cannot create the templates directory",
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            std::fs::write(path, body).map_err(|source| GenerateError::Io {
                what: "cannot write a product template",
                path: path.clone(),
                source,
            })?;
        }
        Ok(self
            .templates
            .iter()
            .filter(|template| template.action.writes())
            .count())
    }
}

/// Decide every template's fate in `dir`, reading but writing nothing.
///
/// # Errors
/// Returns [`GenerateError::Io`] when a copy, a base or the directory cannot be
/// read. A copy that cannot be read is not treated as absent: that would turn
/// a permissions problem into a silent overwrite of someone's template.
pub fn sync(dir: &Path) -> Result<TemplateSync, GenerateError> {
    let mut templates = Vec::new();
    let mut diagnostics = Diagnostics::new();
    let mut sources = BTreeMap::new();
    let mut writes = Vec::new();

    for key in KEYS {
        let builtin = TemplateSet::builtin(key).ok_or_else(|| GenerateError::UnknownTemplate {
            key: (*key).to_owned(),
        })?;
        let copy_path = dir.join(format!("{key}.jinja"));
        let base_path = dir.join(BASE_DIR).join(format!("{key}.jinja"));
        let copy = read_optional(&copy_path)?;
        let base = read_optional(&base_path)?;

        let (action, body, write_base) = match (copy, base) {
            (None, _) => (FileAction::Create, builtin.to_owned(), true),
            (Some(copy), Some(base)) if base == builtin => (FileAction::Unchanged, copy, false),
            (Some(copy), Some(base)) if copy == base => {
                (FileAction::Update, builtin.to_owned(), true)
            }
            (Some(copy), Some(base)) => match merge3::merge(&base, &copy, builtin) {
                Merge::Merged(merged) if merged == copy => (FileAction::Unchanged, copy, true),
                Merge::Merged(merged) => (FileAction::Update, merged, true),
                Merge::Conflict => {
                    diagnostics.push(behind(
                        key,
                        "this build's builtin changes the lines the copy's edits changed; merge \
                         them by hand, or delete the copy to take the builtin",
                    ));
                    (FileAction::Conflict, copy, false)
                }
            },
            (Some(copy), None) if copy == builtin => (FileAction::Unchanged, copy, true),
            (Some(copy), None) => {
                diagnostics.push(behind(
                    key,
                    "the copy has no record of the builtin it was made from, so its edits cannot \
                     be told from the builtin's changes; delete it to take the builtin, or keep \
                     it and follow the builtin by hand",
                ));
                (FileAction::Kept, copy, false)
            }
        };

        let status = match action {
            FileAction::Conflict | FileAction::Kept => TemplateStatus::Behind,
            _ if body == builtin => TemplateStatus::Default,
            _ => TemplateStatus::Edited,
        };
        if action.writes() {
            writes.push((copy_path.clone(), body.clone()));
        }
        if write_base {
            writes.push((base_path, builtin.to_owned()));
        }
        templates.push(ProductTemplate {
            key: (*key).to_owned(),
            path: copy_path.display().to_string(),
            status,
            action,
        });
        sources.insert((*key).to_owned(), body);
    }

    // A file for a key this build does not render: kept, reported, unused --
    // as an overlay always treated one.
    for Unknown { key, path, body } in unknown_templates(dir)? {
        templates.push(ProductTemplate {
            key: key.clone(),
            path: path.display().to_string(),
            status: TemplateStatus::Unknown,
            action: FileAction::Kept,
        });
        sources.insert(key, body);
    }

    diagnostics.finish();
    Ok(TemplateSync {
        templates,
        diagnostics,
        set: TemplateSet::from_overrides(sources),
        writes,
    })
}

fn behind(key: &str, why: &str) -> Diagnostic {
    Diagnostic::new(
        DiagnosticCode::GenTemplateBehindDefault,
        format!("product template `{key}` did not take this build's builtin: {why}"),
    )
}

/// A template file no key renders: its key, where it is, and what it says.
struct Unknown {
    key: String,
    path: PathBuf,
    body: String,
}

/// The `*.jinja` files under `dir`, outside `.base/`, whose key is not in [`KEYS`].
fn unknown_templates(dir: &Path) -> Result<Vec<Unknown>, GenerateError> {
    let mut found = Vec::new();
    if !dir.is_dir() {
        return Ok(found);
    }
    let walk = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|entry| entry.depth() != 1 || entry.file_name() != BASE_DIR);
    for entry in walk {
        let entry = entry.map_err(|source| GenerateError::Io {
            what: "cannot walk the product's templates",
            path: source.path().unwrap_or(dir).to_path_buf(),
            source: std::io::Error::other(source.to_string()),
        })?;
        let path = entry.path();
        if !path.is_file()
            || !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jinja"))
        {
            continue;
        }
        let rel = path.strip_prefix(dir).unwrap_or(path);
        let mut key = super::paths::to_slash(rel);
        if let Some(dot) = key.rfind('.') {
            key.truncate(dot);
        }
        if KEYS.contains(&key.as_str()) {
            continue;
        }
        let body = std::fs::read_to_string(path).map_err(|source| GenerateError::Io {
            what: "cannot read a product template",
            path: path.to_path_buf(),
            source,
        })?;
        found.push(Unknown {
            key,
            path: path.to_path_buf(),
            body,
        });
    }
    Ok(found)
}

fn read_optional(path: &Path) -> Result<Option<String>, GenerateError> {
    match std::fs::read_to_string(path) {
        Ok(body) => Ok(Some(body)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(GenerateError::Io {
            what: "cannot read a product template",
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "gearbox-product-templates-{name}-{}",
                std::process::id()
            ));
            drop(std::fs::remove_dir_all(&dir));
            Self(dir)
        }
        fn copy(&self, key: &str) -> PathBuf {
            self.0.join(format!("{key}.jinja"))
        }
        fn base(&self, key: &str) -> PathBuf {
            self.0.join(BASE_DIR).join(format!("{key}.jinja"))
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            drop(std::fs::remove_dir_all(&self.0));
        }
    }

    fn of<'a>(sync: &'a TemplateSync, key: &str) -> &'a ProductTemplate {
        sync.templates.iter().find(|t| t.key == key).unwrap()
    }

    const KEY: &str = "docker/dockerignore";

    #[test]
    fn an_empty_directory_gets_every_builtin_and_its_base() {
        let dir = Dir::new("empty");
        let sync = sync(&dir.0).unwrap();
        assert!(
            sync.templates
                .iter()
                .all(|t| t.action == FileAction::Create)
        );
        assert!(
            sync.templates
                .iter()
                .all(|t| t.status == TemplateStatus::Default)
        );
        assert_eq!(sync.write().unwrap(), KEYS.len());
        let builtin = TemplateSet::builtin(KEY).unwrap();
        assert_eq!(std::fs::read_to_string(dir.copy(KEY)).unwrap(), builtin);
        assert_eq!(std::fs::read_to_string(dir.base(KEY)).unwrap(), builtin);

        let again = super::sync(&dir.0).unwrap();
        assert!(
            again
                .templates
                .iter()
                .all(|t| t.action == FileAction::Unchanged)
        );
        assert!(again.set.overridden().is_empty());
    }

    #[test]
    fn an_untouched_copy_follows_a_new_builtin() {
        let dir = Dir::new("untouched");
        sync(&dir.0).unwrap().write().unwrap();
        // The builtin this copy was written from, before this build changed it.
        std::fs::write(dir.copy(KEY), "old builtin\n").unwrap();
        std::fs::write(dir.base(KEY), "old builtin\n").unwrap();

        let sync = super::sync(&dir.0).unwrap();
        assert_eq!(of(&sync, KEY).action, FileAction::Update);
        assert_eq!(of(&sync, KEY).status, TemplateStatus::Default);
        sync.write().unwrap();
        let builtin = TemplateSet::builtin(KEY).unwrap();
        assert_eq!(std::fs::read_to_string(dir.copy(KEY)).unwrap(), builtin);
    }

    #[test]
    fn an_edit_survives_and_takes_a_new_builtin_elsewhere() {
        let dir = Dir::new("merged");
        sync(&dir.0).unwrap().write().unwrap();
        let builtin = TemplateSet::builtin(KEY).unwrap();
        // The old builtin lacked its last line; the edit added one at the top.
        let last = builtin.trim_end().rfind('\n').unwrap() + 1;
        let old = &builtin[..last];
        std::fs::write(dir.base(KEY), old).unwrap();
        std::fs::write(dir.copy(KEY), format!("# house rule\n{old}")).unwrap();

        let sync = super::sync(&dir.0).unwrap();
        assert_eq!(of(&sync, KEY).action, FileAction::Update);
        assert_eq!(of(&sync, KEY).status, TemplateStatus::Edited);
        assert!(sync.diagnostics.is_empty());
        assert_eq!(
            sync.set.get(KEY).unwrap(),
            format!("# house rule\n{builtin}")
        );
        assert_eq!(sync.set.overridden(), vec![KEY]);
    }

    #[test]
    fn an_edit_overlapping_the_new_builtin_is_kept_and_reported() {
        let dir = Dir::new("conflict");
        sync(&dir.0).unwrap().write().unwrap();
        let builtin = TemplateSet::builtin(KEY).unwrap();
        let first = builtin.lines().next().unwrap();
        std::fs::write(
            dir.base(KEY),
            builtin.replacen(first, "the old first line", 1),
        )
        .unwrap();
        let edited = builtin.replacen(first, "the house's first line", 1);
        std::fs::write(dir.copy(KEY), &edited).unwrap();

        let sync = super::sync(&dir.0).unwrap();
        assert_eq!(of(&sync, KEY).action, FileAction::Conflict);
        assert_eq!(of(&sync, KEY).status, TemplateStatus::Behind);
        assert_eq!(sync.set.get(KEY).unwrap(), edited);
        assert!(
            sync.diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::GenTemplateBehindDefault)
        );
        sync.write().unwrap();
        assert_eq!(std::fs::read_to_string(dir.copy(KEY)).unwrap(), edited);
    }

    #[test]
    fn a_hand_written_copy_without_a_base_is_kept_and_reported() {
        let dir = Dir::new("nobase");
        std::fs::create_dir_all(dir.0.join("docker")).unwrap();
        std::fs::write(dir.copy(KEY), "house\n").unwrap();

        let sync = super::sync(&dir.0).unwrap();
        assert_eq!(of(&sync, KEY).action, FileAction::Kept);
        assert_eq!(of(&sync, KEY).status, TemplateStatus::Behind);
        assert_eq!(sync.set.get(KEY).unwrap(), "house\n");
        sync.write().unwrap();
        assert!(
            !dir.base(KEY).exists(),
            "no base is invented for a copy nobody can explain"
        );
    }

    #[test]
    fn a_file_for_no_key_is_kept_and_unused() {
        let dir = Dir::new("unknown");
        std::fs::create_dir_all(&dir.0).unwrap();
        std::fs::write(dir.0.join("house.yaml.jinja"), "x\n").unwrap();
        let sync = sync(&dir.0).unwrap();
        assert_eq!(of(&sync, "house.yaml").status, TemplateStatus::Unknown);
        assert!(!sync.templates.iter().any(|t| t.key.starts_with(BASE_DIR)));
    }
}
