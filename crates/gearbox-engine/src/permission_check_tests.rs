#![allow(clippy::unwrap_used, reason = "test fixtures")]

use std::collections::BTreeMap;

use super::*;

fn gear(id: &str, permissions: &[&str], unreadable: &[&str]) -> gearbox_ir::GearDescriptor {
    let mut descriptor = crate::config_check::config_check_tests::demo_descriptor();
    descriptor.id = GearId::new(id).unwrap();
    descriptor.permissions = permissions.iter().map(|p| (*p).to_owned()).collect();
    descriptor.unreadable_permissions = unreadable.iter().map(|p| (*p).to_owned()).collect();
    descriptor
}

fn run(
    gears: Vec<gearbox_ir::GearDescriptor>,
    mapping: &[(&str, &[&str])],
    development: bool,
) -> Vec<(DiagnosticCode, Severity, String)> {
    let mut catalogue = Catalogue::default();
    let mut members = Vec::new();
    for g in gears {
        members.push(g.id.clone());
        catalogue.gears.insert(g.id.clone(), g);
    }
    let mut intent: ProductIntent = serde_json::from_value(serde_json::json!({
        "id": "p", "display_name": "P", "version": "0.1.0", "gdl_path": "product.gdl",
        "sources": {}, "profiles": {}, "default_profile": "dev", "selected_gears": [],
    }))
    .unwrap();
    intent.permissions = mapping
        .iter()
        .map(|(p, s)| ((*p).to_owned(), s.iter().map(|s| (*s).to_owned()).collect()))
        .collect();
    let dev = gearbox_ir::ProfileId::new("dev").unwrap();
    let declaration = DeploymentProfileDecl::Embedded {
        database: None,
        sqlite_databases: BTreeMap::new(),
        id: dev,
        development,
        declared_at: None,
    };
    let mut diagnostics = Diagnostics::default();
    check(
        &catalogue,
        &members,
        &intent,
        &declaration,
        "product.gdl",
        &mut diagnostics,
    );
    diagnostics
        .iter()
        .map(|d| (d.code, d.severity, d.message.clone()))
        .collect()
}

#[test]
fn an_unmapped_permission_is_an_error_in_production_and_a_warning_in_development() {
    let gears = || vec![gear("settings", &["settings.values.write"], &[])];
    let production = run(gears(), &[], false);
    assert_eq!(production.len(), 1, "{production:?}");
    assert_eq!(production[0].0, DiagnosticCode::GdlPermissionUnmapped);
    assert_eq!(production[0].1, Severity::Error);
    assert!(production[0].2.contains("`settings`"), "{production:?}");
    let development = run(gears(), &[], true);
    assert_eq!(development[0].1, Severity::Warning);
    assert!(run(gears(), &[("settings.values.write", &["*"])], false).is_empty());
}

#[test]
fn a_mapping_nobody_declares_is_a_warning() {
    let found = run(
        vec![gear("settings", &[], &[])],
        &[("settings.values.wirte", &["operator"])],
        false,
    );
    assert_eq!(
        found.iter().map(|f| f.0).collect::<Vec<_>>(),
        [DiagnosticCode::GdlPermissionMappingUnused]
    );
}

#[test]
fn an_unreadable_permission_is_reported_with_its_location() {
    let found = run(
        vec![gear("settings", &[], &["src/routes.rs:12"])],
        &[],
        false,
    );
    assert_eq!(
        found.iter().map(|f| f.0).collect::<Vec<_>>(),
        [DiagnosticCode::GapPermissionUnreadable]
    );
    assert!(found[0].2.contains("src/routes.rs:12"), "{found:?}");
}
