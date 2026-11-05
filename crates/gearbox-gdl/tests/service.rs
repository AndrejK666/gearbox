//! Evaluating a `service.gdl`: the passport of a service that is not a gear
//! (GDL 1.6), the vocabulary Studio's catalogue reads too.

#![allow(
    clippy::unwrap_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use gearbox_gdl::{FileIdentity, GdlEngine};
use gearbox_ir::infra::{ServiceKind, ServicePassport};
use gearbox_ir::{DiagnosticCode, RelPath, SourceId};

fn eval(src: &str) -> (Option<ServicePassport>, Vec<DiagnosticCode>, String) {
    let identity = FileIdentity {
        uri: "file:///repo/infra/keycloak/service.gdl".to_owned(),
        source: SourceId::new("studio-web").unwrap(),
        gdl_path: RelPath::new("infra/keycloak/service.gdl").unwrap(),
        load_paths: None,
    };
    let out = GdlEngine::new().eval_service(&identity, src);
    let codes = out.diagnostics.iter().map(|d| d.code).collect();
    let messages = out
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join(" | ");
    (out.value, codes, messages)
}

const KEYCLOAK: &str = r#"
gdl_version = "1.6"

service(
    name = "keycloak",
    kind = "infra",
    version = "26.0.7",
    description = "Keycloak: the product's own OIDC identity provider",
    image = "quay.io/keycloak/keycloak:26.0.7",
    args = ["start-dev"],
    ports = {"http": 8080, "management": 9000},
    health = "/health/ready",
    config = {"KC_HEALTH_ENABLED": "true", "KC_BOOTSTRAP_ADMIN_USERNAME": "admin"},
    secrets = ["KC_BOOTSTRAP_ADMIN_PASSWORD"],
    provides = {
        "oidc.issuer": "{public_url}/realms/product",
        "keycloak.admin_url": "http://{host}:{port.http}",
        "keycloak.admin_password": "{secret.KC_BOOTSTRAP_ADMIN_PASSWORD}",
    },
    chart = "oci://registry-1.docker.io/bitnamicharts/keycloak",
    chart_version = "24.4.0",
)
"#;

#[test]
fn an_infra_passport_evaluates() {
    let (passport, codes, messages) = eval(KEYCLOAK);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let passport = passport.unwrap();
    assert_eq!(passport.name, "keycloak");
    assert_eq!(passport.kind, ServiceKind::Infra);
    assert_eq!(passport.ports["http"], 8080);
    assert_eq!(passport.args, ["start-dev"]);
    assert_eq!(passport.secrets, ["KC_BOOTSTRAP_ADMIN_PASSWORD"]);
    assert_eq!(
        passport.provides["oidc.issuer"],
        "{public_url}/realms/product"
    );
    assert_eq!(passport.chart_version.as_deref(), Some("24.4.0"));
}

#[test]
fn a_stateful_passport_says_where_its_volume_is() {
    let (passport, codes, messages) = eval(
        r#"gdl_version = "1.6"
service(name = "postgresql", kind = "infra", image = "pgvector/pgvector:pg18",
    ports = {"postgres": 5432}, config = {"POSTGRES_USER": "postgres"},
    secrets = ["POSTGRES_PASSWORD"], stateful = True, storage = "10Gi",
    data_path = "/var/lib/postgresql/data",
    provides = {"postgres": "{host}:{port.postgres}",
        "database.url": "postgres://{config.POSTGRES_USER}:{secret.POSTGRES_PASSWORD}@{host}:{port.postgres}/postgres"})"#,
    );
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let passport = passport.unwrap();
    assert!(passport.stateful);
    assert_eq!(passport.storage.as_deref(), Some("10Gi"));

    let (_, codes, messages) =
        eval(r#"service(name = "pg", kind = "infra", image = "postgres:18", stateful = True)"#);
    assert!(!codes.is_empty());
    assert!(messages.contains("data_path"), "{messages}");
}

#[test]
fn an_own_passport_evaluates_and_refuses_what_only_infra_takes() {
    let (passport, codes, messages) = eval(
        r#"gdl_version = "1.6"
service(name = "studio-theia", dockerfile = "Dockerfile", context = ".",
    ports = {"http": 3003}, provides = ["ide"], docs = ["README.md"])"#,
    );
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let passport = passport.unwrap();
    assert_eq!(passport.kind, ServiceKind::Own);
    assert!(passport.provides.contains_key("ide"));

    let (_, codes, messages) = eval(r#"service(name = "x", config = {"A": "b"})"#);
    assert!(!codes.is_empty());
    assert!(messages.contains("kind = \"infra\""), "{messages}");
}

#[test]
fn an_infra_service_is_pulled_never_built() {
    let (_, codes, messages) =
        eval(r#"service(name = "x", kind = "infra", image = "x:1", dockerfile = "Dockerfile")"#);
    assert!(!codes.is_empty());
    assert!(messages.contains("never built"), "{messages}");
}

#[test]
fn a_template_names_only_what_the_passport_declares() {
    let (_, codes, messages) = eval(
        r#"service(name = "x", kind = "infra", image = "x:1", ports = {"http": 80},
            provides = {"x.url": "http://{host}:{port.admin}"})"#,
    );
    assert!(!codes.is_empty());
    assert!(messages.contains("{port.admin}"), "{messages}");

    let (_, codes, messages) = eval(
        r#"service(name = "x", kind = "infra", image = "x:1",
            provides = {"x.url": "{nothing}"})"#,
    );
    assert!(!codes.is_empty());
    assert!(messages.contains("{nothing}"), "{messages}");
}

#[test]
fn a_secret_is_not_also_config() {
    let (_, codes, messages) = eval(
        r#"service(name = "x", kind = "infra", image = "x:1",
            config = {"PASSWORD": "p"}, secrets = ["PASSWORD"])"#,
    );
    assert!(!codes.is_empty());
    assert!(
        messages.contains("both in `config` and in `secrets`"),
        "{messages}"
    );
}

#[test]
fn a_file_declares_one_service() {
    let (_, codes, _) = eval(
        r#"service(name = "a", kind = "infra", image = "a:1")
service(name = "b", kind = "infra", image = "b:1")"#,
    );
    assert!(codes.contains(&DiagnosticCode::GdlCardinality), "{codes:?}");
}
