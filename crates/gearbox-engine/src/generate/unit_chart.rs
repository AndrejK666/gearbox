//! A hand-written unit chart, copied into the product's umbrella.
//!
//! A process that *is* a unit a gear packages (`gear(deploy = helm_chart(...))`)
//! ships that chart rather than a generated one (`ResolvedApplication::unit_chart`).
//! It is copied, not referenced: the generated tree has to install without the
//! source checkout, exactly as a generated subchart does. The chart's own
//! `file://` dependencies -- the `toolkit-common` library -- are copied beside it
//! under its `charts/`, and its `Chart.yaml` is rewritten to point there.
//! `Chart.lock` and dependency archives are left behind: both describe the
//! source tree's layout, not this one.

use std::path::Path;

use gearbox_ir::{FileEntry, FileKind, Ownership, ResolvedApplication};

use super::{GenerateError, GenerateInput, header, paths};

/// The copied chart, and what the umbrella needs to depend on it.
pub(super) struct CopiedChart {
    /// The chart's own name (`Chart.yaml` `name`): the umbrella's dependency
    /// `name`, with the process name as its `alias`.
    pub name: String,
    pub version: String,
    pub files: Vec<FileEntry>,
}

/// Copy `application`'s unit chart to `helm/<product>/charts/<sub>/`.
///
/// # Errors
/// [`GenerateError::UnknownSource`] when the chart's source has no root, and
/// [`GenerateError::Io`] / [`GenerateError::Yaml`]-shaped failures when the
/// chart cannot be read.
pub(super) fn copy(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
    product: &str,
    sub: &str,
) -> Result<Option<CopiedChart>, GenerateError> {
    let Some(chart) = &application.unit_chart else {
        return Ok(None);
    };
    let root =
        input
            .source_roots
            .get(&chart.source)
            .ok_or_else(|| GenerateError::UnknownSource {
                gear: application.anchor.to_string(),
                id: chart.source.to_string(),
            })?;
    let dir = root.join(chart.path.as_str());
    let mut files = Vec::new();
    let (name, version) = copy_dir(
        &dir,
        &[
            "helm".to_owned(),
            product.to_owned(),
            "charts".to_owned(),
            sub.to_owned(),
        ],
        &mut files,
    )?;
    Ok(Some(CopiedChart {
        name,
        version,
        files,
    }))
}

fn io(what: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> GenerateError {
    let path = path.to_path_buf();
    move |source| GenerateError::Io { what, path, source }
}

/// Copy one chart directory to `dest`, its `file://` dependencies under its
/// `charts/`, and return its name and version.
fn copy_dir(
    dir: &Path,
    dest: &[String],
    files: &mut Vec<FileEntry>,
) -> Result<(String, String), GenerateError> {
    let chart_yaml = dir.join("Chart.yaml");
    let raw = std::fs::read_to_string(&chart_yaml)
        .map_err(io("could not read the unit chart", &chart_yaml))?;
    let mut chart: serde_json::Value =
        serde_saphyr::from_str(&raw).map_err(|_| GenerateError::BadPath {
            path: chart_yaml.display().to_string(),
        })?;
    let name = chart["name"].as_str().unwrap_or_default().to_owned();
    let version = chart["version"].as_str().unwrap_or_default().to_owned();
    if let Some(dependencies) = chart
        .get_mut("dependencies")
        .and_then(serde_json::Value::as_array_mut)
    {
        for dependency in dependencies {
            let repository = dependency["repository"].as_str().unwrap_or_default();
            // Only a dependency in the source tree can be copied; anything
            // fetched from a repository would be missing from the generated
            // tree, which has to install without network access to the source.
            let Some(target) = repository.strip_prefix("file://").map(str::to_owned) else {
                return Err(GenerateError::UnitChart {
                    chart: dir.display().to_string(),
                    reason: format!(
                        "dependency `{}` comes from `{repository}`; a unit chart's dependencies \
                         are `file://` charts in its source",
                        dependency["name"].as_str().unwrap_or_default()
                    ),
                });
            };
            let dep_name = dependency["name"].as_str().unwrap_or_default().to_owned();
            let mut dep_dest = dest.to_vec();
            dep_dest.extend(["charts".to_owned(), dep_name.clone()]);
            copy_dir(&dir.join(&target), &dep_dest, files)?;
            dependency["repository"] =
                serde_json::Value::String(format!("file://charts/{dep_name}"));
        }
    }
    let body = serde_saphyr::to_string(&chart).map_err(|source| GenerateError::Yaml {
        what: "a unit chart's Chart.yaml",
        source,
    })?;
    files.push(FileEntry::text(
        paths::rel(
            &[dest, &["Chart.yaml".to_owned()]]
                .concat()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )?,
        format!("{}\n{body}", header("#")),
        FileKind::Yaml,
        Ownership::Generated,
    ));
    copy_tree(dir, dir, dest, files)?;
    Ok((name, version))
}

/// Every file under `at` except `Chart.yaml` (rewritten), `Chart.lock` and the
/// `charts/` directory (dependencies are copied from their own source).
fn copy_tree(
    chart: &Path,
    at: &Path,
    dest: &[String],
    files: &mut Vec<FileEntry>,
) -> Result<(), GenerateError> {
    let mut entries: Vec<_> = std::fs::read_dir(at)
        .map_err(io("could not list the unit chart", at))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name.starts_with('.') {
            continue;
        }
        let relative: Vec<String> = path
            .strip_prefix(chart)
            .unwrap_or(&path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if path.is_dir() {
            if at == chart && file_name == "charts" {
                continue;
            }
            copy_tree(chart, &path, dest, files)?;
            continue;
        }
        if at == chart && (file_name == "Chart.yaml" || file_name == "Chart.lock") {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(io("could not read a unit chart file", &path))?;
        // Copied as bytes: a chart may carry an asset that is not text.
        let kind = if std::str::from_utf8(&bytes).is_err() {
            FileKind::Binary
        } else {
            match path.extension().and_then(|e| e.to_str()) {
                Some("yaml" | "yml") => FileKind::Yaml,
                Some("json") => FileKind::Json,
                _ => FileKind::Text,
            }
        };
        let full: Vec<String> = dest.iter().cloned().chain(relative).collect();
        files.push(FileEntry {
            path: paths::rel(&full.iter().map(String::as_str).collect::<Vec<_>>())?,
            bytes,
            kind,
            ownership: Ownership::Generated,
        });
    }
    Ok(())
}
