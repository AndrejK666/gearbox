//! Merging a gear's declared configuration curation with its projected fields.
//!
//! The two halves are deliberately unequal. Rust states what the fields *are* --
//! name, type, whether required, what they default to -- and ADR
//! `cpt-gearbox-adr-macro-projected-catalogue` gives Rust every fact Rust
//! already expresses. The description contributes the one judgement Rust has no
//! way to hold: which of those fields is worth putting in front of an
//! integrator, and in what order. `ApiGatewayConfig` declares fourteen fields
//! and the configuration files in the tree set five.
//!
//! **`exposes` is checked, not trusted.** It names Rust facts, so it can drift
//! from them, and a list that silently stopped matching would show controls for
//! keys the gear no longer reads. Naming a field the struct does not declare is
//! `GBX0212`. That check is the whole reason a curated list is not a second copy
//! of the struct.

use gearbox_gdl::GearDecl;
use gearbox_gdl::engine::FileIdentity;
use gearbox_ir::{
    ConfigFieldDecl, ConfigSchema, Diagnostic, DiagnosticCode, Diagnostics, Location,
};
use gearbox_project::{ConfigFieldsError, ConfigRootError, RustFile};

/// Build the configuration surface for one gear, or `None` if it declares none.
///
/// `files` is the gear's own crate, already scanned for the gear attribute, so
/// this costs no extra I/O and no second `syn` parse -- one more walk of an AST
/// that is already in memory.
pub fn project(
    identity: &FileIdentity,
    decl: &GearDecl,
    files: &[RustFile],
    diagnostics: &mut Diagnostics,
) -> Option<ConfigSchema> {
    let declared = decl.config_schema.as_ref()?;
    let uri = identity.uri.as_str();
    // Every diagnostic in this file is about the `config(...)` call -- a struct
    // it cannot find, or an `exposes` entry naming no field -- so the anchor is
    // computed once and handed down.
    let at = Location::or_file(declared.declared_at.as_ref(), uri);

    let root = match declared.rust.clone() {
        Some(named) => named,
        // Not declared, so find it: the single `ctx.config*()` call in
        // `impl Gear::init` is the only machine-readable link between a gear and
        // its config type -- the `Gear` trait has no associated `Config`.
        None => match gearbox_project::project_config_root(files) {
            Ok(Some(found)) => found,
            Ok(None) => {
                report(
                    diagnostics,
                    &at,
                    "`config_schema` is declared but this crate deserializes no configuration"
                        .to_owned(),
                    "drop `config_schema`, or name the struct with `config(rust = \"...\")` if \
                     the gear reads it somewhere this cannot see",
                );
                return None;
            }
            Err(ConfigRootError::Ambiguous { roots }) => {
                report(
                    diagnostics,
                    &at,
                    format!(
                        "this crate deserializes {} different types as its configuration: {}",
                        roots.len(),
                        roots.join(", ")
                    ),
                    "name the one this gear uses with `config(rust = \"...\")`",
                );
                return None;
            }
        },
    };

    // Three answers where there used to be one empty vector, so the message can
    // say which happened instead of offering the reader both.
    let projected = match gearbox_project::project_config_fields(files, &root) {
        Ok(fields) => fields,
        Err(ConfigFieldsError::RootNotFound { .. }) => {
            report(
                diagnostics,
                &at,
                format!("this crate declares no struct named `{root}`"),
                "check the struct name against the crate; `config_schema` is a locator, not a path",
            );
            return None;
        }
        Err(ConfigFieldsError::NoNamedFields { .. }) => {
            report(
                diagnostics,
                &at,
                format!("`{root}` is a tuple or unit struct, so it has no configuration keys"),
                "drop `config_schema`, or name the struct that actually carries the keys with \
                 `config(rust = \"...\")`",
            );
            return None;
        }
        Err(e @ ConfigFieldsError::UnreadableSerdeAttribute { .. }) => {
            report(
                diagnostics,
                &at,
                format!("{e}"),
                "a `#[serde(...)]` form this cannot read hides everything written after it in \
                 the same attribute, `skip` included; split it into separate `#[serde(...)]` \
                 attributes so each is read on its own",
            );
            return None;
        }
    };
    if projected.is_empty() {
        report(
            diagnostics,
            &at,
            format!("every field of `{root}` is `#[serde(skip)]`, so it has no configuration"),
            "drop `config_schema`, or name the struct that carries the keys with \
             `config(rust = \"...\")`",
        );
        return None;
    }

    // The declared order, because `exposes` is an ordering as much as a filter:
    // an operator reads `bind_addr` before `prefix_path` because the description
    // said so, not because the struct happens to list it first.
    let fields = declared
        .exposes
        .iter()
        .filter_map(|name| {
            let Some(field) = projected.iter().find(|f| &f.name == name) else {
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::ValidateConfigFieldUnknown,
                        format!("`{root}` declares no configuration field `{name}`"),
                        format!(
                            "the fields it declares are: {}",
                            projected
                                .iter()
                                .map(|f| f.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    )
                    .at(at.clone()),
                );
                return None;
            };
            Some(decl_of(field))
        })
        .collect();

    Some(ConfigSchema { rust: root, fields })
}

/// Every projected field, as a schema.
///
/// **No `exposes` filter, and the difference is the subject.** A gear's
/// description curates which of its struct's fields an integrator should see --
/// a product judgement Rust cannot hold. A backend's options struct is not
/// curated by anybody: it is `#[serde(deny_unknown_fields)]`, so every field it
/// declares is a key an operator may set and every key it does not declare is
/// already an error at startup. Filtering here would hide a legal key and then
/// report it as unknown.
pub(crate) fn schema_of(rust: String, projected: &[gearbox_project::ConfigField]) -> ConfigSchema {
    ConfigSchema {
        rust,
        fields: projected.iter().map(decl_of).collect(),
    }
}

/// A projected field as the catalogue carries it, its nested level included.
fn decl_of(field: &gearbox_project::ConfigField) -> ConfigFieldDecl {
    ConfigFieldDecl {
        name: field.name.clone(),
        ty: field.ty.clone(),
        required: field.required,
        default: field.default.clone(),
        doc: field.doc.clone(),
        secret: field.secret,
        fields: field.fields.iter().map(decl_of).collect(),
    }
}

/// `at` rather than `uri`: every one of these is about the `config(...)` call,
/// and the caller is the only one holding it.
fn report(diagnostics: &mut Diagnostics, at: &Location, message: String, help: &str) {
    diagnostics.push(
        Diagnostic::error(DiagnosticCode::GdlConfigStructNotFound, message, help).at(at.clone()),
    );
}
