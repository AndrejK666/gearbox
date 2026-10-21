//! The two generated Rust files: the host entry point and the link file.
//!
//! Rendered with minijinja and its default delimiters, which is safe for Rust
//! because Rust's own brace pairs never sit adjacent on one line. That is a
//! property of the templates as written rather than of Rust, so the tests below
//! assert it instead of trusting it. The Helm templates M7 adds cannot use
//! these delimiters -- Helm has already claimed them -- which is what the
//! `custom_syntax` feature is enabled for.

use std::collections::BTreeSet;

use gearbox_ir::{FileEntry, FileKind, Ownership, ResolvedApplication};
use minijinja::context;

use super::templates;
use super::{GenerateError, GenerateInput, header, paths};

/// `<layout>/<p>/src/main.rs`.
///
/// # Errors
/// Returns [`GenerateError::Template`] if the template cannot be rendered.
pub fn host_main(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
) -> Result<FileEntry, GenerateError> {
    let body = templates::render(
        "main.rs",
        input.templates.get("main.rs")?,
        context! {
            header => header("//").trim_end(),
            application => application.name.as_str(),
            bin_name => application.bin_name.as_str(),
            gear_count => application.gears.len(),
        },
    )?;

    Ok(FileEntry::text(
        paths::rel(&[input.layout(), application.name.as_str(), "src", "main.rs"])?,
        body,
        FileKind::Rust,
        Ownership::Generated,
    ))
}

/// The roles the anchor gear declares, for the table the worker compiles in.
///
/// Read from the catalogue rather than from the lock: the lock records which
/// role a deployment *starts* in, and the binary needs every role it could be
/// started in. Empty when the catalogue is out of scope, which is the ordinary
/// case for a generation driven from a lock alone -- and then `--role` refuses
/// everything, which is the right answer for a binary that cannot say what its
/// roles are.
fn roles_of(input: &GenerateInput<'_>, application: &ResolvedApplication) -> Vec<RoleContext> {
    input
        .catalogue
        .and_then(|c| c.gears.get(&application.anchor))
        .map(|gear| {
            gear.declared_roles
                .iter()
                .map(|r| RoleContext {
                    name: r.name.clone(),
                    directory_name: r.directory_name.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the worker's compiled-in role table.
#[derive(serde::Serialize)]
struct RoleContext {
    name: String,
    directory_name: String,
}

/// `<layout>/<w>/src/main.rs` for a worker.
///
/// A second function rather than a branch inside [`host_main`]: this module's
/// shape is one function per file it produces, and the two entry points share
/// no context beyond the header -- a worker needs its directory identity and
/// its version, which a host has no use for.
///
/// # Errors
/// Returns [`GenerateError::Template`] if the template cannot be rendered.
pub fn worker_main(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
) -> Result<FileEntry, GenerateError> {
    let body = templates::render(
        "worker_main.rs",
        input.templates.get("worker_main.rs")?,
        context! {
            header => header("//").trim_end(),
            application => application.name.as_str(),
            bin_name => application.bin_name.as_str(),
            gear_count => application.gears.len(),
            gear_name => application.registers_as(),
            // The anchor's own id, and deliberately not `registers_as()`. A role
            // changes the name an instance registers under, not the section the
            // gear reads its configuration from -- `GearCtx::config()` keys that
            // by the `#[toolkit::gear(name = ...)]` attribute. Passing one string
            // for both filed the host's rendered config under a key the gear
            // never looks up.
            config_gear_name => application.anchor.as_str(),
            roles => roles_of(input, application),
            version => input.lock.product.version.as_str(),
        },
    )?;

    Ok(FileEntry::text(
        paths::rel(&[input.layout(), application.name.as_str(), "src", "main.rs"])?,
        body,
        FileKind::Rust,
        Ownership::Generated,
    ))
}

/// `<layout>/<p>/src/registered_gears.rs`.
///
/// One `use <ident> as _;` per [`CargoRef::link`](gearbox_ir::CargoRef::link)
/// entry, so a gear whose plugins are separate registrations inside one crate
/// gets a line each. Sorted and deduplicated: two gears in a process can
/// legitimately name the same crate.
///
/// # Errors
/// Returns [`GenerateError::UnlinkableIdent`] when a link entry's crate root is
/// not a dependency the manifest declares, which would be a file that does not
/// compile; [`GenerateError::UnknownGear`] when the process names a gear the
/// lock does not describe.
pub fn registered_gears(
    input: &GenerateInput<'_>,
    application: &ResolvedApplication,
) -> Result<FileEntry, GenerateError> {
    // The dependency keys the manifest will emit. Built from the same field the
    // manifest keys on, so the two files cannot disagree.
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for id in &application.gears {
        let gear = input
            .lock
            .gears
            .get(id)
            .ok_or_else(|| GenerateError::UnknownGear {
                application: application.name.to_string(),
                gear: id.to_string(),
            })?;
        declared.insert(gear.package.lib_ident.as_str());
    }

    let mut idents: BTreeSet<&str> = BTreeSet::new();
    for id in &application.gears {
        let Some(gear) = input.lock.gears.get(id) else {
            continue;
        };
        for ident in gear.package.link_idents() {
            // `mini_chat::infra::plugins::static_audit` links through the
            // `mini_chat` dependency; the root segment is what Cargo has to
            // know about, and the rest is a module path inside it.
            let root = ident.split("::").next().unwrap_or(ident);
            if !declared.contains(root) {
                return Err(GenerateError::UnlinkableIdent {
                    gear: id.to_string(),
                    ident: ident.to_owned(),
                    root: root.to_owned(),
                });
            }
            idents.insert(ident);
        }
    }
    // A client SDK carries the registration that wires its remote client; it
    // stays linked by its own line.
    for (sdk, _) in super::manifest::client_sdks(input, application) {
        idents.insert(sdk.lib_ident.as_str());
    }

    let body = templates::render(
        "registered_gears.rs",
        input.templates.get("registered_gears.rs")?,
        context! {
            header => header("//").trim_end(),
            idents => idents.iter().collect::<Vec<_>>(),
        },
    )?;

    Ok(FileEntry::text(
        paths::rel(&[
            input.layout(),
            application.name.as_str(),
            "src",
            "registered_gears.rs",
        ])?,
        body,
        FileKind::Rust,
        Ownership::Generated,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No Jinja delimiter may survive into the rendered Rust.
    ///
    /// Rust and Jinja share `{` and `}`, and the only thing keeping them apart
    /// is that these templates never write `{{` except where Jinja should read
    /// it. A future edit adding a `format!("{{literal}}")` would produce a file
    /// whose failure mode is a rustc parse error at a line the template does
    /// not have, so the property is asserted rather than assumed.
    #[test]
    fn no_delimiters_survive_rendering() {
        let rendered = templates::render(
            "registered_gears.rs",
            templates::TemplateSet::builtin("registered_gears.rs").expect("builtin"),
            context! {
                header => "// generated",
                idents => vec!["api_gateway", "mini_chat::infra::plugins::static_audit"],
            },
        )
        .expect("render the link file");

        for delimiter in ["{{", "}}", "{%", "%}"] {
            assert!(
                !rendered.contains(delimiter),
                "`{delimiter}` survived into the generated Rust:\n{rendered}"
            );
        }
        assert!(rendered.contains("use api_gateway as _;"));
        assert!(rendered.contains("use mini_chat::infra::plugins::static_audit as _;"));
    }

    /// A missing variable must fail loudly.
    ///
    /// The default would render it as an empty string, which in a Rust template
    /// means a hole the compiler reports as a syntax error somewhere unrelated.
    #[test]
    fn an_undefined_variable_is_an_error() {
        let result = templates::render(
            "registered_gears.rs",
            templates::TemplateSet::builtin("registered_gears.rs").expect("builtin"),
            context! { header => "// generated" },
        );
        assert!(result.is_err(), "a missing `idents` should not render");
    }
}
