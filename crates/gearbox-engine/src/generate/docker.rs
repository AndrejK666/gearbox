//! Per-process Dockerfiles with adjacent ignore files, and `build.sh`.
//!
//! Emitted only for a Kubernetes profile. A `self_hosted` tree really does
//! run on one machine; putting a Dockerfile next to it would describe a
//! deployment the lock did not decide.
//!
//! **The build context is the common ancestor of this output tree and the
//! source roots**, not the generated directory. `<layout>/<p>/Cargo.toml`
//! carries path dependencies of the form `../../../../../../gears-rust/...`,
//! which `docker build` from `.gearbox/<product>/<profile>/` cannot see.
//! That fact is written into `.dockerignore` (as a comment) and into
//! `build.sh` (as the directory it actually passes to `docker build`),
//! because leaving it as an operator guess is how images fail with a
//! missing crate an hour later.
//!
//! An ignore file cannot live at the context root: [`generate`](super::generate)
//! may not write outside `out_root`. Docker reads `Dockerfile.dockerignore`
//! beside each Dockerfile, without a non-standard build flag.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use gearbox_ir::{FileEntry, FileKind, Ownership, ResolvedApplication};
use minijinja::context;

use super::paths::{self, relative, to_slash};
use super::templates;
use super::workspace;
use super::{GenerateError, GenerateInput, NONROOT_UID, header};

/// Every Docker artefact for this lock, or none if the profile does not
/// build images.
///
/// # Errors
/// Returns [`GenerateError`] when a template cannot render, when a path
/// inside `out_root` is not a valid relative path, or when the output tree
/// and the source roots share no relative path (so there is no context a
/// Dockerfile `COPY` could name).
pub fn files(input: &GenerateInput<'_>) -> Result<Vec<FileEntry>, GenerateError> {
    if input.lock.kubernetes.is_none() {
        return Ok(Vec::new());
    }

    let context = build_context(input)?;
    let out_rel = rel_from(&context, input.out_root)?;

    let ignore = dockerignore(input, &context)?;
    let mut files = Vec::new();
    // A unit chart's process runs the unit's own image, built by its own
    // Dockerfile in the source; nothing is generated for it here.
    for application in input
        .lock
        .applications
        .iter()
        .filter(|application| application.unit_chart.is_none())
    {
        files.push(dockerfile(input, application, &out_rel)?);
        files.push(FileEntry::text(
            paths::rel(&[
                "docker",
                application.name.as_str(),
                "Dockerfile.dockerignore",
            ])?,
            ignore.clone(),
            FileKind::Text,
            Ownership::Generated,
        ));
    }
    files.push(build_script(input, &context)?);
    Ok(files)
}

fn dockerfile(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
    out_rel: &str,
) -> Result<FileEntry, GenerateError> {
    let body = templates::render(
        "docker/Dockerfile",
        input.templates.get("docker/Dockerfile")?,
        context! {
            header => header("#").trim_end(),
            rust_channel => workspace::rust_channel(),
            crate_name => application.crate_name.as_str(),
            bin_name => application.bin_name.as_str(),
            application => application.name.as_str(),
            out_rel => out_rel,
            uid => NONROOT_UID,
            home_dir => super::K8S_HOME_DIR,
            ports => expose_ports(application),
        },
    )?;

    Ok(FileEntry::text(
        paths::rel(&["docker", application.name.as_str(), "Dockerfile"])?,
        body,
        FileKind::Dockerfile,
        Ownership::Generated,
    ))
}

fn dockerignore(input: &GenerateInput<'_>, context: &Path) -> Result<String, GenerateError> {
    let mut secret_dirs = Vec::new();
    let mut source_dirs = Vec::new();
    for root in input.source_roots.values() {
        secret_dirs.push(format!("{}/", rel_from(context, &root.join("config"))?));
        let rel = rel_from(context, root)?;
        source_dirs.push(if rel == "." {
            "**".to_owned()
        } else {
            format!("{rel}/**")
        });
    }
    source_dirs.sort();
    source_dirs.dedup();
    secret_dirs.sort();
    secret_dirs.dedup();
    let rel = rel_from(context, input.out_root)?;
    let output_dir = if rel == "." {
        "**".to_owned()
    } else {
        format!("{rel}/**")
    };

    templates::render(
        "docker/dockerignore",
        input.templates.get("docker/dockerignore")?,
        context! {
            header => header("#").trim_end(),
            source_dirs => source_dirs,
            output_dir => output_dir,
            secret_dirs => secret_dirs,
        },
    )
}

/// Companion script so the operator does not have to reconstruct the
/// context path. The relative walk is computed here and handed to the
/// template as `context`, already quoted: an overlay styles the script, but it
/// cannot get the depth wrong and produce an image that cannot see `gears-rust`.
/// Image references arrive quoted the same way.
fn build_script(input: &GenerateInput<'_>, context: &Path) -> Result<FileEntry, GenerateError> {
    let from_docker = input.out_root.join("docker");
    let to_context = match relative(&from_docker, context) {
        Some(rel) => to_slash(&rel),
        None => to_slash(context),
    };

    let built: Vec<_> = input
        .lock
        .applications
        .iter()
        .filter(|application| application.unit_chart.is_none())
        .map(|application| {
            context! {
                name => application.name.as_str(),
                image => sh_single(&image_reference(application, input)),
            }
        })
        .collect();
    let unit_charts: Vec<_> = input
        .lock
        .applications
        .iter()
        .filter_map(|application| {
            application.unit_chart.as_ref().map(|chart| {
                context! {
                    name => application.name.as_str(),
                    message => sh_single(&format!(
                        "{} runs the unit image `{}`: build it with the Dockerfile beside {} in source {}",
                        application.name, chart.image, chart.path, chart.source
                    )),
                }
            })
        })
        .collect();
    let body = templates::render(
        "docker/build.sh",
        input.templates.get("docker/build.sh")?,
        minijinja::context! {
            header => header("#").trim_end(),
            context => context_cd(&to_context),
            built => built,
            unit_charts => unit_charts,
        },
    )?;

    Ok(FileEntry::text(
        paths::rel(&["docker", "build.sh"])?,
        body,
        FileKind::Shell,
        Ownership::Generated,
    ))
}

/// The tag `docker build -t` gets, whole.
///
/// Docker wants the one string the chart deliberately keeps in three parts. It
/// is reassembled here rather than held that way in the lock, because only one
/// of the two consumers wants it joined -- and the chart's need to prefix a
/// mirror registry is the need that cannot be met by splitting a string back up.
///
/// A profile that builds no images leaves `image` unset; falling back to
/// `{bin_name}:{version}` keeps `build.sh` usable there rather than emitting a
/// script with a hole in it.
fn image_reference(application: &ResolvedApplication, input: &GenerateInput<'_>) -> String {
    application.image.as_ref().map_or_else(
        || format!("{}:{}", application.bin_name, input.lock.product.version),
        gearbox_ir::ImageRef::reference,
    )
}

fn build_context(input: &GenerateInput<'_>) -> Result<PathBuf, GenerateError> {
    let mut paths: Vec<&Path> = vec![input.out_root];
    paths.extend(input.source_roots.values().map(PathBuf::as_path));
    paths::common_ancestor(paths).ok_or_else(|| GenerateError::UnreachableDockerContext {
        out_root: input.out_root.display().to_string(),
    })
}

fn rel_from(from: &Path, target: &Path) -> Result<String, GenerateError> {
    let rel = relative(from, target).ok_or_else(|| GenerateError::UnreachableDockerContext {
        out_root: target.display().to_string(),
    })?;
    let rendered = to_slash(&rel);
    if rendered.is_empty() {
        Ok(".".to_owned())
    } else {
        Ok(rendered)
    }
}

fn expose_ports(application: &ResolvedApplication) -> Vec<u16> {
    let mut ports = BTreeSet::new();
    for endpoint in &application.listens {
        if let Some(port) = port_of(&endpoint.address) {
            ports.insert(port);
        }
    }
    if let Some(serve) = &application.serve
        && let Some(port) = port_of(&serve.listen_addr)
    {
        ports.insert(port);
    }
    ports.into_iter().collect()
}

fn port_of(address: &str) -> Option<u16> {
    address.rsplit(':').next()?.parse().ok()
}

/// Quote a string for a POSIX single-quoted word.
fn sh_single(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The directory `cd` argument for the build context.
///
/// An absolute context is used as-is. A relative one is resolved from the
/// generated `docker/` directory, not prefixed raw onto `SCRIPT_DIR` --
/// `$SCRIPT_DIR//abs` is how an absolute `to_context` used to break the script.
fn context_cd(to_context: &str) -> String {
    if to_context.starts_with('/') || Path::new(to_context).is_absolute() {
        sh_single(to_context)
    } else {
        format!("\"$SCRIPT_DIR/\"{}", sh_single(to_context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_of_reads_the_last_colon_segment() {
        assert_eq!(port_of("0.0.0.0:8087"), Some(8087));
        assert_eq!(port_of("127.0.0.1:50051"), Some(50051));
        assert_eq!(port_of("not-a-port"), None);
    }

    #[test]
    fn sh_single_quotes_an_image_reference() {
        assert_eq!(
            sh_single("registry.example.com/payments/gbx-api-gateway:0.1.0"),
            "'registry.example.com/payments/gbx-api-gateway:0.1.0'"
        );
    }

    #[test]
    fn context_cd_does_not_prefix_an_absolute_path() {
        assert_eq!(context_cd("/workspace/src"), "'/workspace/src'");
        assert_eq!(context_cd("../.."), "\"$SCRIPT_DIR/\"'../..'");
    }
}
