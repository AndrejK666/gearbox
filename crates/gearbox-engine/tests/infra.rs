//! A product's infra services (GDL 1.6): passports found in sources, chosen
//! with `use_service(...)`, their capabilities bound to the variables a gear's
//! configuration reads, recorded in the lock and its diff.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]
#[path = "support/resolve_fixtures.rs"]
mod fixtures;

use std::collections::BTreeMap;

use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::infra::{ServiceDescriptor, ServiceKind, ServicePassport, ServiceSelection};
use gearbox_ir::{
    Catalogue, DiagnosticCode, ProductIntent, ProfileId, RelPath, ResolvedProduct, RuntimeCap,
    SourceId,
};

fn keycloak() -> ServicePassport {
    ServicePassport {
        name: "keycloak".to_owned(),
        kind: ServiceKind::Infra,
        image: Some("quay.io/keycloak/keycloak:26.0.7".to_owned()),
        ports: BTreeMap::from([("http".to_owned(), 8080)]),
        config: BTreeMap::from([("KC_HEALTH_ENABLED".to_owned(), "true".to_owned())]),
        secrets: vec!["KC_BOOTSTRAP_ADMIN_PASSWORD".to_owned()],
        provides: BTreeMap::from([
            (
                "oidc.issuer".to_owned(),
                "{public_url}/realms/product".to_owned(),
            ),
            (
                "keycloak.admin_url".to_owned(),
                "http://{host}:{port.http}".to_owned(),
            ),
        ]),
        ..ServicePassport::default()
    }
}

fn descriptor(passport: ServicePassport) -> ServiceDescriptor {
    ServiceDescriptor {
        source: fixtures::source(),
        gdl_path: RelPath::new(format!("infra/{}/service.gdl", passport.name)).unwrap(),
        passport_hash: format!("hash-of-{}", passport.name),
        passport,
    }
}

fn selection(name: &str) -> ServiceSelection {
    ServiceSelection {
        name: name.to_owned(),
        source: fixtures::source(),
        image: None,
        chart: None,
        chart_version: None,
        config: BTreeMap::new(),
        profile_config: BTreeMap::new(),
        declared_at: None,
    }
}

/// `oidc-authn-plugin` and a gear that keeps data, plus the Keycloak passport.
fn catalogue() -> Catalogue {
    let mut catalogue = fixtures::catalogue_of(vec![
        fixtures::descriptor("oidc-authn-plugin"),
        fixtures::gear_with_caps("store", &[RuntimeCap::Db], &[]),
    ]);
    catalogue.services.push(descriptor(keycloak()));
    catalogue
}

fn intent() -> ProductIntent {
    let mut intent = fixtures::intent(&["oidc-authn-plugin", "store"]);
    intent.infra.push(selection("keycloak"));
    intent
}

fn lock(catalogue: &Catalogue, intent: &ProductIntent) -> ResolvedProduct {
    let resolution = gearbox_engine::resolve::resolve(catalogue, intent, &intent.default_profile);
    gearbox_engine::resolve::product::assemble(catalogue, intent, &resolution, BTreeMap::new())
}

fn codes(lock: &ResolvedProduct) -> Vec<DiagnosticCode> {
    lock.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn a_declared_service_is_locked_with_its_capabilities_and_the_needs_it_meets() {
    let mut intent = intent();
    intent.infra[0].image = Some("quay.io/keycloak/keycloak:26.0.7@sha256:abc".to_owned());
    intent.infra[0]
        .config
        .insert("KC_HEALTH_ENABLED".to_owned(), "false".to_owned());
    let lock = lock(&catalogue(), &intent);
    let infra = lock.infra.as_ref().expect("an infra section");

    let service = &infra.services[0];
    assert_eq!(service.passport_hash, "hash-of-keycloak");
    let image = service.image.as_ref().unwrap();
    assert_eq!(image.digest.as_deref(), Some("sha256:abc"));
    assert_eq!(service.config["KC_HEALTH_ENABLED"], "false");
    assert!(service.overridden.contains("KC_HEALTH_ENABLED"));

    assert_eq!(infra.capabilities["oidc.issuer"].env, "OIDC_ISSUER");
    assert_eq!(infra.capabilities["oidc.issuer"].service, "keycloak");
    assert_eq!(
        infra.capabilities["keycloak.admin_url"].env,
        "KEYCLOAK_ADMIN_URL"
    );

    let oidc = infra.needs.iter().find(|n| n.need == "oidc").unwrap();
    assert_eq!(oidc.met_by.as_deref(), Some("keycloak"));
    // Unmet is not an error: it may be met outside the product.
    let postgres = infra.needs.iter().find(|n| n.need == "postgres").unwrap();
    assert_eq!(postgres.met_by, None);
    assert!(postgres.gears.contains(&fixtures::gid("store")));
}

#[test]
fn a_product_without_infra_has_no_infra_section() {
    let lock = lock(&catalogue(), &fixtures::intent(&["oidc-authn-plugin"]));
    assert!(lock.infra.is_none());
    assert!(
        !serde_json::to_value(&lock)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("infra")
    );
}

#[test]
fn a_service_must_be_an_infra_passport_in_its_source() {
    let mut intent = intent();
    intent.infra.push(selection("nothing"));
    let mut catalogue = catalogue();
    catalogue.services.push(descriptor(ServicePassport {
        name: "theia".to_owned(),
        kind: ServiceKind::Own,
        ..ServicePassport::default()
    }));
    intent.infra.push(selection("theia"));
    let lock = lock(&catalogue, &intent);
    let unknown = lock
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::GdlInfraServiceUnknown)
        .count();
    assert_eq!(unknown, 2, "{:?}", lock.diagnostics);
}

#[test]
fn two_services_may_not_provide_one_capability() {
    let mut catalogue = catalogue();
    catalogue.services.push(descriptor(ServicePassport {
        name: "dex".to_owned(),
        kind: ServiceKind::Infra,
        image: Some("dex:2".to_owned()),
        provides: BTreeMap::from([("oidc.issuer".to_owned(), "{host}".to_owned())]),
        ..ServicePassport::default()
    }));
    let mut intent = intent();
    intent.infra.push(selection("dex"));
    let lock = lock(&catalogue, &intent);
    assert!(
        codes(&lock).contains(&DiagnosticCode::GdlInfraCapabilityConflict),
        "{:?}",
        lock.diagnostics
    );
}

#[test]
fn a_service_config_sets_only_the_passports_literal_keys() {
    for (key, value) in [
        ("KC_UNKNOWN", "x"),
        ("KC_BOOTSTRAP_ADMIN_PASSWORD", "hunter2"),
        ("KC_HEALTH_ENABLED", "${FROM_ENV}"),
    ] {
        let mut intent = intent();
        intent.infra[0]
            .config
            .insert(key.to_owned(), value.to_owned());
        let lock = lock(&catalogue(), &intent);
        assert!(
            codes(&lock).contains(&DiagnosticCode::GdlInfraConfigInvalid),
            "{key}: {:?}",
            lock.diagnostics
        );
    }
}

#[test]
fn a_profile_config_is_merged_for_its_profile() {
    let mut intent = intent();
    intent.infra[0].profile_config.insert(
        ProfileId::new("dev").unwrap(),
        BTreeMap::from([("KC_HEALTH_ENABLED".to_owned(), "dev-value".to_owned())]),
    );
    let lock = lock(&catalogue(), &intent);
    assert_eq!(
        lock.infra.unwrap().services[0].config["KC_HEALTH_ENABLED"],
        "dev-value"
    );
}

fn with_config(intent: &mut ProductIntent, value: &str) {
    intent.selected_gears[0]
        .config
        .insert("issuer".to_owned(), serde_json::json!({"url": value}));
}

#[test]
fn configuration_reads_only_declared_variables() {
    // A capability of a declared service.
    let mut intent = intent();
    with_config(&mut intent, "${OIDC_ISSUER}/protocol");
    assert!(!codes(&lock(&catalogue(), &intent)).contains(&DiagnosticCode::GdlUnknownVariable));

    // A name nothing declares.
    let mut intent = self::intent();
    with_config(&mut intent, "${SMTP_PASSWORD}");
    let refused = lock(&catalogue(), &intent);
    let diagnostic = refused
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::GdlUnknownVariable)
        .expect("GBX0130");
    assert!(diagnostic.message.contains("SMTP_PASSWORD"));

    // ... until the product declares it.
    intent.secrets.insert("SMTP_PASSWORD".to_owned());
    assert!(!codes(&lock(&catalogue(), &intent)).contains(&DiagnosticCode::GdlUnknownVariable));
}

#[test]
fn a_product_declaring_no_infra_nor_secrets_keeps_the_old_rule() {
    let mut intent = fixtures::intent(&["oidc-authn-plugin"]);
    with_config(&mut intent, "${ANYTHING}");
    assert!(!codes(&lock(&catalogue(), &intent)).contains(&DiagnosticCode::GdlUnknownVariable));
}

#[test]
fn the_infra_section_survives_the_canonical_lock_and_shows_in_its_diff() {
    // No gear that keeps data: a lock with one is refused without a database.
    let mut catalogue = fixtures::catalogue_of(vec![fixtures::descriptor("oidc-authn-plugin")]);
    catalogue.services.push(descriptor(keycloak()));
    let intent = || {
        let mut intent = fixtures::intent(&["oidc-authn-plugin"]);
        intent.infra.push(selection("keycloak"));
        intent
    };
    let before = lock(&catalogue, &intent());
    let text = gearbox_lock::write_canonical(&before).unwrap();
    let read = gearbox_lock::read(&text).unwrap();
    assert_eq!(read.infra, before.infra);

    let mut changed = intent();
    changed.infra[0].image = Some("quay.io/keycloak/keycloak:26.1.0".to_owned());
    changed.secrets.insert("SMTP_PASSWORD".to_owned());
    let after = lock(&catalogue, &changed);
    let diff = gearbox_lock::diff(&before, &after);
    let fields: Vec<&str> = diff
        .fields_changed
        .iter()
        .map(|f| f.field.as_str())
        .collect();
    assert!(fields.contains(&"infra.keycloak.image"), "{fields:?}");
    assert!(fields.contains(&"secrets"), "{fields:?}");
}

#[test]
fn the_lock_json_carries_what_a_deployment_reads() {
    let json = serde_json::to_value(lock(&catalogue(), &intent())).unwrap();
    let infra = &json["infra"];
    assert_eq!(infra["services"][0]["name"], "keycloak");
    assert_eq!(
        infra["services"][0]["image"]["reference"],
        "quay.io/keycloak/keycloak:26.0.7"
    );
    assert_eq!(infra["services"][0]["passport"]["kind"], "infra");
    assert_eq!(infra["capabilities"]["oidc.issuer"]["env"], "OIDC_ISSUER");
    assert_eq!(
        infra["capabilities"]["oidc.issuer"]["template"],
        "{public_url}/realms/product"
    );
}

#[test]
fn the_catalogue_finds_service_passports_in_a_source() {
    let dir = std::env::temp_dir().join(format!("gearbox-infra-scan-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let passport = |name: &str| {
        format!(
            "gdl_version = \"1.6\"\nservice(name = \"{name}\", kind = \"infra\", image = \"{name}:1\", \
             ports = {{\"http\": 80}}, provides = {{\"{name}.url\": \"http://{{host}}:{{port.http}}\"}})\n"
        )
    };
    for (at, name) in [("infra/keycloak", "keycloak"), ("infra/pg", "postgresql")] {
        std::fs::create_dir_all(dir.join(at)).unwrap();
        std::fs::write(dir.join(at).join("service.gdl"), passport(name)).unwrap();
    }
    let root = SourceRoot::open(SourceId::new("studio-web").unwrap(), &dir).unwrap();
    let scan = load_catalogue(&[root]);
    assert!(
        !scan.catalogue.diagnostics.has_errors(),
        "{:?}",
        scan.catalogue.diagnostics
    );
    let names: Vec<&str> = scan
        .catalogue
        .services
        .iter()
        .map(|s| s.passport.name.as_str())
        .collect();
    assert_eq!(names, ["keycloak", "postgresql"]);
    assert_eq!(
        scan.catalogue.services[0].gdl_path.as_str(),
        "infra/keycloak/service.gdl"
    );

    // The same name twice in one source.
    std::fs::create_dir_all(dir.join("copy")).unwrap();
    std::fs::write(dir.join("copy/service.gdl"), passport("keycloak")).unwrap();
    let root = SourceRoot::open(SourceId::new("studio-web").unwrap(), &dir).unwrap();
    let scan = load_catalogue(&[root]);
    assert!(
        scan.catalogue
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::GdlCardinality && d.message.contains("keycloak")),
        "{:?}",
        scan.catalogue.diagnostics
    );
    std::fs::remove_dir_all(&dir).ok();
}
