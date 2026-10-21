//! The product's permission mapping against the gears it contains (gears-rust
//! `cpt-cf-adr-operation-permissions` §6).
//!
//! A gear declares abstract permissions; the runtime refuses every call to an
//! operation whose permission the product does not map. So the mapping is
//! checked against every gear in the resolved closure -- selected gears, their
//! plugins and co-located dependencies -- before anything is built:
//!
//! * a declared permission the product does not map is GBX0124, an error on a
//!   production profile and a warning under `development = True`;
//! * a mapped permission no gear declares is GBX0125, most likely a typo;
//! * a gear whose permissions could not all be read is GBX0609: its set is
//!   incomplete, so the first check cannot be complete either.

use std::collections::{BTreeMap, BTreeSet};

use gearbox_ir::{
    Catalogue, DeploymentProfileDecl, Diagnostic, DiagnosticCode, Diagnostics, GearId, Location,
    ProductIntent, Severity,
};

/// Check `intent.permissions` against the permissions of `members`.
pub fn check<'a>(
    catalogue: &Catalogue,
    members: impl IntoIterator<Item = &'a GearId>,
    intent: &ProductIntent,
    declaration: &DeploymentProfileDecl,
    uri: &str,
    diagnostics: &mut Diagnostics,
) {
    // permission -> the gears that declare it.
    let mut declared: BTreeMap<&str, BTreeSet<&GearId>> = BTreeMap::new();
    let mut unreadable: Vec<(&GearId, &[String])> = Vec::new();
    for gear in members {
        let Some(descriptor) = catalogue.gears.get(gear) else {
            continue;
        };
        for permission in &descriptor.permissions {
            declared.entry(permission).or_default().insert(gear);
        }
        if !descriptor.unreadable_permissions.is_empty() {
            unreadable.push((gear, &descriptor.unreadable_permissions));
        }
    }

    for (permission, gears) in &declared {
        if intent.permissions.contains_key(*permission) {
            continue;
        }
        let named = gears
            .iter()
            .map(|g| format!("`{g}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut diagnostic = Diagnostic::error(
            DiagnosticCode::GdlPermissionUnmapped,
            format!(
                "{named} declares the permission `{permission}`, which this product does not \
                 map: every call to the operations that require it is refused with 403"
            ),
            format!(
                "add `\"{permission}\": [<the scopes that grant it>]` to `product(permissions = \
                 {{...}})`; `[\"*\"]` grants it to every authenticated caller"
            ),
        )
        .at(Location::file(uri));
        if declaration.development() {
            diagnostic = diagnostic.with_severity(Severity::Warning);
        }
        diagnostics.push(diagnostic);
    }

    for permission in intent.permissions.keys() {
        if declared.contains_key(permission.as_str()) {
            continue;
        }
        let mut help = "remove it, or correct the name to the one the gear declares".to_owned();
        if !unreadable.is_empty() {
            help.push_str(
                "; some gears name permissions this tool cannot read (GBX0609), and one of \
                 those may be it",
            );
        }
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GdlPermissionMappingUnused,
                format!("the product maps `{permission}`, which no gear in it declares"),
            )
            .with_help(help)
            .at(Location::file(uri)),
        );
    }

    for (gear, locations) in unreadable {
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::GapPermissionUnreadable,
                format!(
                    "`{gear}` names a permission this tool cannot read at {}: its permissions \
                     may not all be mapped",
                    locations.join(", ")
                ),
            )
            .with_help(
                "write the permission as a string literal (`.permission(\"area.action\")`), \
                 so the mapping can be checked against it",
            )
            .at(Location::file(uri)),
        );
    }
}

#[cfg(test)]
#[path = "permission_check_tests.rs"]
mod permission_check_tests;
