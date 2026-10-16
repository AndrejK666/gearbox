//! `cpt-gearbox-fr-gdl-no-restatement`: a description may not restate a fact
//! the Rust attributes own.
//!
//! Refusing rather than tolerating is what keeps the projection decision alive.
//! Without an active rejection the mirrored surface returns by accretion, one
//! convenient field at a time, and the design decays back into a cross-check --
//! which is exactly the option ADR
//! `cpt-gearbox-adr-macro-projected-catalogue` rejected.
//!
//! One case per projected field, as the ADR's Confirmation section requires.

#![allow(
    clippy::unwrap_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use gearbox_gdl::{FileIdentity, GdlEngine};
use gearbox_ir::{DiagnosticCode, RelPath, SourceId};

fn identity() -> FileIdentity {
    FileIdentity {
        uri: "file:///repo/gears/demo/gear.gdl".to_owned(),
        source: SourceId::new("gears-rust").unwrap(),
        gdl_path: RelPath::new("gears/demo/gear.gdl").unwrap(),
        load_paths: None,
    }
}

const PACKAGE: &str = r#"package = cargo(crate_name = "c", lib = "c")"#;

/// Evaluate, returning (codes, first message).
fn eval(src: &str) -> (Vec<DiagnosticCode>, String) {
    let out = GdlEngine::new().eval_gear(&identity(), src);
    let codes = out.diagnostics.iter().map(|d| d.code).collect();
    let message = out
        .diagnostics
        .as_slice()
        .first()
        .map(|d| d.message.clone())
        .unwrap_or_default();
    (codes, message)
}

/// Every projected field, with the attribute the diagnostic must name.
const PROJECTED_FIELDS: &[(&str, &str)] = &[
    (r#"id = "demo""#, "toolkit::gear(name"),
    ("runtime_caps = [cap.rest]", "capabilities"),
    (r#"colocated_deps = ["other"]"#, "deps"),
    (r#"lifecycle = lifecycle(entry = "serve")"#, "lifecycle"),
    (r#"client = "some::Trait""#, "client"),
    // Any value: it is the field *name* that is refused. `provider(...)` used to
    // be spellable here and no longer is, which is the point -- providers are
    // projected from `provider_registry()` and the plugin crates' own source.
    (r#"cluster_providers = ["standalone"]"#, "provider_registry"),
];

#[test]
fn every_projected_gear_field_is_refused_by_name() {
    for (field, owning_attribute) in PROJECTED_FIELDS {
        let src = format!("gear(maturity = \"stable\", {PACKAGE}, {field})\n");
        let (codes, message) = eval(&src);

        assert_eq!(
            codes,
            [DiagnosticCode::ValidateRestatement],
            "`{field}` should be GBX0210, got {codes:?}"
        );
        // Naming the owning attribute is the point: "unknown argument" would
        // leave the author guessing where the fact belongs.
        assert!(
            message.contains(owning_attribute),
            "the diagnostic for `{field}` must name `{owning_attribute}`; got: {message}"
        );
    }
}

#[test]
fn projected_contract_fields_are_refused_on_provide() {
    for field in [
        r#"version = "v1""#,
        "kind = contract_kind.api",
        // Which transports exist follows from which projection traits sit beside
        // the base trait. Declaring the list would let a description claim a
        // remote binding the code provably rules out.
        "transports = [transport.local, transport.rest]",
    ] {
        let src = format!(
            r#"
SDK = cargo(crate_name = "s", lib = "s")
gear(maturity = "stable", {PACKAGE}, provides = [provide(contract = "PaymentApi", rust = "s::PaymentApi", sdk = SDK, {field})])
"#
        );
        let (codes, message) = eval(&src);
        assert_eq!(
            codes,
            [DiagnosticCode::ValidateRestatement],
            "`{field}` on provide should be GBX0210, got {codes:?}"
        );
        assert!(
            message.contains("contract")
                || message.contains("suffix")
                || message.contains("projection traits"),
            "got: {message}"
        );
    }
}

#[test]
fn projected_contract_fields_are_refused_on_consume() {
    for field in [r#"version = "v1""#, "kind = contract_kind.api"] {
        let src = format!(
            r#"
SDK = cargo(crate_name = "s", lib = "s")
gear(maturity = "stable", {PACKAGE}, consumes = [consume(contract = "PaymentApi", rust = "s::PaymentApi", sdk = SDK, from_ = "other", {field})])
"#
        );
        let (codes, _) = eval(&src);
        assert_eq!(
            codes,
            [DiagnosticCode::ValidateRestatement],
            "for `{field}`"
        );
    }
}

#[test]
fn a_declared_field_is_still_accepted() {
    // The refusal must be surgical: everything GDL legitimately owns still works.
    let src = format!(
        r#"
gear(
    maturity = "stable",
    {PACKAGE},
    name = "Demo",
    description = "d",
    category = "example",
    visibility = "public",
    serves = [endpoint(name = "rest", config_key = "bind_addr", default_port = 8087)],
    requires = [cluster.cache(profile = "demo", capabilities = [cluster_cap.linearizable])],
    cluster_plugins = [cluster_plugin(
        package = cargo(crate_name = "p", lib = "p", path = "../p"),
        process_local = True,
    )],
    config_schema = config(rust = "DemoConfig", exposes = ["bind_addr"]),
)
"#
    );
    let (codes, message) = eval(&src);
    assert!(
        codes.is_empty(),
        "declared fields must be accepted: {codes:?} {message}"
    );
}

#[test]
fn an_unknown_field_is_still_a_distinct_code() {
    // A typo is not a restatement; conflating them would send the author
    // looking for a Rust attribute that does not exist.
    let (codes, _) = eval(&format!(
        r#"gear(maturity = "stable", {PACKAGE}, kind = "service")"#
    ));
    assert_eq!(codes, [DiagnosticCode::GdlUnknownArgument]);
}

#[test]
fn the_attr_locator_is_accepted_on_package() {
    let src = r#"
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c", attr = "src/gear.rs"), name = "D")
"#;
    let (codes, message) = eval(src);
    assert!(codes.is_empty(), "{codes:?} {message}");
}

#[test]
fn a_cluster_requirement_must_name_its_profile() {
    // `profile` deliberately has no default. It is a join key against the gear's
    // own `impl ClusterProfile { const NAME }`, and the SDK turns it into
    // `ClientScope::new("cluster:{name}")` -- so a defaulted `"default"` would
    // resolve to a scope nothing registered and fail at startup with
    // `ProfileNotBound`. The platform's only real consumer binds `"event-broker"`,
    // which a default would have got silently wrong.
    let (codes, message) = eval(&format!(
        r#"gear(maturity = "stable", {PACKAGE}, name = "D", requires = [cluster.cache()])"#
    ));
    assert!(
        !codes.is_empty(),
        "omitting `profile` must be refused, not defaulted: {message}"
    );
    assert!(
        message.contains("profile"),
        "the diagnostic must name the missing parameter; got: {message}"
    );
}
