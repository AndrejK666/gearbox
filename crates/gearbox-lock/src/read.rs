//! Reading a `product.lock` back.

use std::collections::BTreeSet;

use gearbox_ir::{LOCK_SCHEMA_VERSION, ResolvedProduct};
use serde::Deserialize;

use crate::canonical::{canonicalize_order, hash_in_place};
use crate::error::LockError;

/// Parse `input` as a `product.lock`, verify that its recorded hash matches
/// its own content, and check the values that downstream stages act on.
///
/// TOML comments are ignored by the parser, so the generated header needs no
/// special handling here.
///
/// Verification re-canonicalizes the parsed product before hashing, so a
/// lock whose collections happen to be in a different order than
/// [`write_canonical`](crate::write_canonical) would choose -- but whose
/// *content* is identical -- still verifies. A lock whose content actually
/// changed does not, which is the point:
/// [`LockError::HashMismatch`] is how a hand edit is caught rather than
/// silently taking effect. The two classes of edit canonicalization would
/// *erase* before hashing -- a duplicated entry, and a key no field reads --
/// are refused before the hash is computed, so they cannot ride inside a lock
/// that reports as verified.
///
/// `schema_version` is read off the parsed document before the product is
/// deserialized from it, which is what turns "a future schema added a
/// required field" into a clear [`LockError::UnsupportedSchemaVersion`]
/// instead of a confusing missing-field parse error.
///
/// # Errors
/// Returns [`LockError::UnsupportedSchemaVersion`] if the lock declares a
/// version this build does not understand, [`LockError::Parse`] if it is not
/// valid TOML or does not match the resolved-product schema,
/// [`LockError::UnknownField`] if it carries a key no field of that schema
/// reads, [`LockError::DuplicateEntry`] if it lists one entry of a keyed
/// collection twice, [`LockError::Serialize`] if the parsed product cannot be
/// re-rendered to recompute its hash, [`LockError::HashMismatch`] if its
/// content does not match its own recorded hash, or
/// [`LockError::InvalidContent`] if a verified value is one the generator or
/// the runtime cannot act on.
pub fn read(input: &str) -> Result<ResolvedProduct, LockError> {
    let document: toml::Value = toml::from_str(input)?;
    if let Some(found) = document
        .get("schema_version")
        .and_then(toml::Value::as_integer)
    {
        // A version that does not fit a u32 is not one this build supports
        // either, and saturating says so with the same error rather than a
        // parse failure about an integer width.
        let found = u32::try_from(found).unwrap_or(u32::MAX);
        if found != LOCK_SCHEMA_VERSION {
            return Err(LockError::UnsupportedSchemaVersion {
                found,
                supported: LOCK_SCHEMA_VERSION,
            });
        }
    }

    let mut product = ResolvedProduct::deserialize(document.clone())?;
    reject_unread_keys(&document, &toml::Value::try_from(&product)?)?;
    reject_duplicates(&product)?;

    let expected = product.product.lock_hash.clone();

    canonicalize_order(&mut product);
    let found = hash_in_place(&mut product)?;

    if found != expected {
        return Err(LockError::HashMismatch {
            expected,
            found,
            gearbox_version: product.product.gearbox_version.clone(),
        });
    }

    validate(&product)?;

    Ok(product)
}

/// Refuse a key the resolved-product schema does not read.
///
/// `read` is the integrity check, and the hash covers the *model*, not the
/// file: a key serde ignored never reaches it. Comparing the file's own
/// document against the one the parsed product renders back to is how an
/// ignored key becomes visible without a `deny_unknown_fields` on every type
/// in `gearbox_ir` -- which would also refuse the forward-compatible defaults
/// the schema version exists to manage.
///
/// Only keys are compared, never values: a value that round-trips to a
/// different spelling is the hash's business.
fn reject_unread_keys(from_file: &toml::Value, from_model: &toml::Value) -> Result<(), LockError> {
    fn walk(file: &toml::Value, model: Option<&toml::Value>, path: &str) -> Result<(), LockError> {
        match file {
            toml::Value::Table(table) => {
                for (key, value) in table {
                    let child = format!("{path}{}{key}", if path.is_empty() { "" } else { "." });
                    let in_model = model.and_then(|m| m.get(key));
                    if in_model.is_none() && !is_empty_collection(value) {
                        return Err(LockError::UnknownField { path: child });
                    }
                    walk(value, in_model, &child)?;
                }
            }
            toml::Value::Array(items) => {
                // The model is rendered from the parsed product before any
                // canonicalization, so the two arrays are the same entries in
                // the same order and index is a usable path.
                for (index, value) in items.iter().enumerate() {
                    let child = format!("{path}[{index}]");
                    walk(value, model.and_then(|m| m.get(index)), &child)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    walk(from_file, Some(from_model), "")
}

/// Whether a value carries nothing, and so cannot be a smuggled key.
///
/// An empty table or array is written by a file that spells out a collection
/// the writer omits when it is empty (`skip_serializing_if`). Refusing those
/// would refuse a lock that says the same thing more verbosely.
fn is_empty_collection(value: &toml::Value) -> bool {
    match value {
        toml::Value::Table(table) => table.is_empty(),
        toml::Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// Refuse a lock that lists one entry of a keyed collection twice.
fn reject_duplicates(product: &ResolvedProduct) -> Result<(), LockError> {
    let mut seen = BTreeSet::new();
    for edge in &product.provenance {
        let key = (
            edge.from.as_str(),
            edge.kind,
            edge.to.as_str(),
            &edge.because,
        );
        if !seen.insert(key) {
            return Err(LockError::DuplicateEntry {
                collection: "provenance",
                entry: format!(
                    "{} {} {}",
                    edge.to.as_str(),
                    edge.kind.phrase(),
                    edge.from.as_str()
                ),
            });
        }
    }

    let mut seen = BTreeSet::new();
    for diagnostic in &product.diagnostics {
        let key = (diagnostic.code, &diagnostic.message, &diagnostic.location);
        if !seen.insert(key) {
            return Err(LockError::DuplicateEntry {
                collection: "diagnostics",
                entry: format!("{}: {}", diagnostic.code.as_str(), diagnostic.message),
            });
        }
    }

    Ok(())
}

/// Check the values a verified lock hands to a path join or a process start.
///
/// The GDL front end refuses these at authoring time, and a lock read from
/// disk is the one way into the generator that skips those checks: `layout`
/// reaches `out_root.join(...)`, and a spawn's `bin_name` becomes a binary the
/// host starts.
fn validate(product: &ResolvedProduct) -> Result<(), LockError> {
    product
        .validate_database()
        .map_err(|reason| LockError::InvalidContent {
            field: "database".to_owned(),
            reason,
        })?;

    if !gearbox_ir::is_valid_layout(&product.product.layout) {
        return Err(LockError::InvalidContent {
            field: "product.layout".to_owned(),
            reason: format!(
                "`{}` is not a single path segment that can name a directory \
                 under the output root",
                product.product.layout
            ),
        });
    }

    for application in &product.applications {
        for spawn in &application.spawns {
            if !is_plain_file_name(&spawn.bin_name) {
                return Err(LockError::InvalidContent {
                    field: format!("applications.{}.spawns.bin_name", application.name),
                    reason: format!(
                        "`{}` is not a plain binary name -- a spawn names a \
                         binary, never a path to one",
                        spawn.bin_name
                    ),
                });
            }
        }
    }

    Ok(())
}

/// Whether `name` names a file and nothing about where it sits.
fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
}
