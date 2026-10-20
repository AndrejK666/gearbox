//! The production rules (GBX0325; gears-rust ADR-0011 §1/§3, test case P4):
//! each is an error on a production profile, a warning on a `development = True`
//! one, and silent when the description conforms.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a setup failure IS the test failure"
)]

use gearbox_engine::resolve::resolve;
use gearbox_ir::{
    DeploymentProfileDecl, DiagnosticCode, Discovery, ProductIntent, ProfileId, RuntimeCap,
    Severity,
};
use serde_json::json;

#[path = "support/resolve_fixtures.rs"]
mod support;

fn pid(id: &str) -> ProfileId {
    ProfileId::new(id).unwrap()
}

/// A gateway host and one REST worker, so the profile is multi-process.
fn catalogue() -> gearbox_ir::Catalogue {
    support::catalogue_of(vec![
        support::gear_with_caps(
            "api-gateway",
            &[RuntimeCap::Rest, RuntimeCap::RestHost],
            &[],
        ),
        support::gear_with_caps("grpc-hub", &[RuntimeCap::GrpcHub], &[]),
        support::gear_with_caps("worker-api", &[], &[]),
        support::gear_with_caps("authn-resolver", &[], &[]),
    ])
}

fn configure(intent: &mut ProductIntent, gear: &str, key: &str, value: serde_json::Value) {
    intent
        .selected_gears
        .iter_mut()
        .find(|s| s.gear.as_str() == gear)
        .unwrap()
        .config
        .insert(key.to_owned(), value);
}

fn kubernetes(development: bool) -> ProductIntent {
    let mut intent = support::kubernetes(
        &["api-gateway", "grpc-hub", "worker-api", "authn-resolver"],
        Discovery::Directory,
    );
    support::pin(&mut intent, "worker", "worker-api", 1);
    set_development(&mut intent, development);
    intent
}

fn set_development(intent: &mut ProductIntent, value: bool) {
    for declaration in intent.profiles.values_mut() {
        match declaration {
            DeploymentProfileDecl::Embedded { development, .. }
            | DeploymentProfileDecl::SelfHosted { development, .. }
            | DeploymentProfileDecl::Kubernetes { development, .. } => *development = value,
        }
    }
}

/// The GBX0325 findings of the profile under test, with their severity: the
/// fixtures start from an embedded `dev` profile and add the one a test is
/// about, so that one when there are two.
fn findings(intent: &ProductIntent) -> Vec<(Severity, String)> {
    let profile = intent
        .profiles
        .keys()
        .find(|p| p.as_str() != "dev")
        .or_else(|| intent.profiles.keys().next())
        .unwrap()
        .clone();
    resolve(&catalogue(), intent, &profile)
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::TopologyNotProductionReady)
        .map(|d| (d.severity, d.message.clone()))
        .collect()
}

#[test]
fn kubernetes_needs_kube_platform_auth() {
    let mut intent = kubernetes(false);
    let found = findings(&intent);
    assert!(
        found
            .iter()
            .any(|(s, m)| *s == Severity::Error && m.contains("\"kube\"")),
        "{found:?}"
    );

    configure(
        &mut intent,
        "api-gateway",
        "internal_auth",
        json!({"provider": "shared_secret"}),
    );
    assert!(
        findings(&intent)
            .iter()
            .any(|(_, m)| m.contains("`shared_secret`"))
    );

    configure(
        &mut intent,
        "api-gateway",
        "internal_auth",
        json!({"provider": "kube"}),
    );
    assert_eq!(findings(&intent), vec![], "conforming");
}

#[test]
fn development_reports_the_same_findings_as_warnings() {
    let production = findings(&kubernetes(false));
    let development = findings(&kubernetes(true));
    assert!(!production.is_empty());
    assert_eq!(production.len(), development.len());
    assert!(
        development.iter().all(|(s, _)| *s == Severity::Warning),
        "{development:?}"
    );
}

#[test]
fn self_hosted_needs_a_shared_secret() {
    let mut intent = support::self_hosted(
        &["api-gateway", "grpc-hub", "worker-api", "authn-resolver"],
        Discovery::Directory,
        Some("t"),
    );
    support::pin(&mut intent, "worker", "worker-api", 1);
    assert!(
        findings(&intent)
            .iter()
            .any(|(_, m)| m.contains("\"shared_secret\""))
    );
    configure(
        &mut intent,
        "api-gateway",
        "internal_auth",
        json!({"provider": "shared_secret"}),
    );
    assert_eq!(findings(&intent), vec![]);
}

#[test]
fn every_profile_keeps_the_public_listener_exposed_only() {
    let mut intent = kubernetes(false);
    configure(
        &mut intent,
        "api-gateway",
        "internal_auth",
        json!({"provider": "kube"}),
    );
    configure(
        &mut intent,
        "api-gateway",
        "public",
        json!({"exposed_only": false}),
    );
    assert!(
        findings(&intent)
            .iter()
            .any(|(_, m)| m.contains("exposed_only"))
    );
}

#[test]
fn an_embedded_hub_binds_no_tcp_and_no_hub_socket_is_in_tmp() {
    let mut intent = support::intent(&["api-gateway", "grpc-hub", "authn-resolver"]);
    configure(
        &mut intent,
        "grpc-hub",
        "listen_addr",
        json!("127.0.0.1:50051"),
    );
    assert!(findings(&intent).iter().any(|(_, m)| m.contains("TCP")));
    configure(
        &mut intent,
        "grpc-hub",
        "listen_addr",
        json!("uds:///tmp/hub.sock"),
    );
    assert!(
        findings(&intent)
            .iter()
            .any(|(_, m)| m.contains("temporary directory"))
    );
    configure(
        &mut intent,
        "grpc-hub",
        "listen_addr",
        json!("uds:///run/payments/hub.sock"),
    );
    assert_eq!(findings(&intent), vec![]);
}

#[test]
fn a_hub_enforces_platform_auth() {
    let mut intent = support::intent(&["api-gateway", "grpc-hub", "authn-resolver"]);
    configure(
        &mut intent,
        "grpc-hub",
        "internal_auth_enforcement",
        json!("permissive"),
    );
    assert!(
        findings(&intent)
            .iter()
            .any(|(_, m)| m.contains("enforcement"))
    );
}

#[test]
fn accept_all_tenant_authn_is_development_only_and_profile_scoped() {
    let mut intent = support::intent(&["api-gateway", "grpc-hub", "authn-resolver"]);
    let plugin = |profiles: &[&str]| gearbox_ir::PluginSelection {
        gear: support::gid("static-authn-plugin"),
        config: [("mode".to_owned(), json!("accept_all"))]
            .into_iter()
            .collect(),
        profiles: profiles.iter().map(|p| pid(p)).collect(),
        declared_at: None,
        entry_index: 0,
    };
    let authn = intent
        .selected_gears
        .iter_mut()
        .find(|s| s.gear.as_str() == "authn-resolver")
        .unwrap();
    authn.plugins = vec![plugin(&[])];
    assert!(
        findings(&intent)
            .iter()
            .any(|(_, m)| m.contains("accept_all"))
    );

    // Selected for another profile only: not this one's concern.
    intent
        .selected_gears
        .iter_mut()
        .find(|s| s.gear.as_str() == "authn-resolver")
        .unwrap()
        .plugins = vec![plugin(&["local"])];
    assert_eq!(findings(&intent), vec![]);
}

#[test]
fn auth_disabled_and_an_unstated_static_mode_are_refused() {
    let mut intent = kubernetes(false);
    configure(
        &mut intent,
        "api-gateway",
        "internal_auth",
        json!({"provider": "kube"}),
    );
    configure(&mut intent, "api-gateway", "auth_disabled", json!(true));
    assert!(
        findings(&intent)
            .iter()
            .any(|(_, m)| m.contains("auth_disabled"))
    );

    // The static plugin with no `mode` is `accept_all`: its default.
    let mut intent = support::intent(&["api-gateway", "grpc-hub", "authn-resolver"]);
    intent
        .selected_gears
        .iter_mut()
        .find(|s| s.gear.as_str() == "authn-resolver")
        .unwrap()
        .plugins = vec![gearbox_ir::PluginSelection {
        gear: support::gid("static-authn-plugin"),
        config: std::collections::BTreeMap::new(),
        profiles: std::collections::BTreeSet::new(),
        declared_at: None,
        entry_index: 0,
    }];
    assert!(
        findings(&intent)
            .iter()
            .any(|(_, m)| m.contains("accept_all"))
    );
}

#[test]
fn kubernetes_keeps_the_directory_namespace_check() {
    let mut intent = kubernetes(false);
    configure(
        &mut intent,
        "api-gateway",
        "internal_auth",
        json!({"provider": "kube"}),
    );
    intent.selected_gears.push(intent.selected_gears[0].clone());
    intent.selected_gears.last_mut().unwrap().gear = support::gid("service-discovery");
    intent.selected_gears.last_mut().unwrap().config.clear();
    configure(
        &mut intent,
        "service-discovery",
        "platform_namespaces",
        json!([]),
    );
    assert!(
        findings(&intent)
            .iter()
            .any(|(_, m)| m.contains("platform_namespaces"))
    );
}

#[test]
fn a_self_hosted_hub_is_a_private_socket() {
    let mut intent = support::self_hosted(
        &["api-gateway", "grpc-hub", "worker-api", "authn-resolver"],
        Discovery::Directory,
        Some("t"),
    );
    support::pin(&mut intent, "worker", "worker-api", 1);
    configure(
        &mut intent,
        "api-gateway",
        "internal_auth",
        json!({"provider": "shared_secret"}),
    );
    configure(
        &mut intent,
        "grpc-hub",
        "listen_addr",
        json!("127.0.0.1:50051"),
    );
    assert!(
        findings(&intent)
            .iter()
            .any(|(_, m)| m.contains("private directory"))
    );
    configure(
        &mut intent,
        "grpc-hub",
        "listen_addr",
        json!("uds:///run/payments/hub.sock"),
    );
    assert_eq!(findings(&intent), vec![]);
}
