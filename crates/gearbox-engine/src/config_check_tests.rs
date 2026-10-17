//! Tests for the config type check.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::collections::{BTreeMap, BTreeSet};

use gearbox_ir::{ConfigFieldDecl, ConfigSchema, GearDescriptor, GearId, SourceId};

use super::*;

fn field(name: &str, ty: ConfigFieldType) -> ConfigFieldDecl {
    ConfigFieldDecl {
        name: name.to_owned(),
        ty,
        required: false,
        default: None,
        doc: None,
        secret: false,
    }
}

/// A catalogue of one gear exposing three fields of different shapes.
fn catalogue() -> Catalogue {
    let mut gear = GearDescriptor {
        one_per_installation: false,
        config_schema: Some(ConfigSchema {
            rust: "DemoConfig".to_owned(),
            fields: vec![
                field("bind_addr", ConfigFieldType::Str),
                field("enable_docs", ConfigFieldType::Bool),
                field("port", ConfigFieldType::Int),
                field("ratio", ConfigFieldType::Float),
                field(
                    "mode",
                    ConfigFieldType::Enum {
                        variants: vec!["accept_all".to_owned(), "static_tokens".to_owned()],
                    },
                ),
                field("nested", ConfigFieldType::Complex),
            ],
        }),
        ..demo_descriptor()
    };
    gear.id = GearId::new("demo").unwrap();

    let mut catalogue = Catalogue::default();
    catalogue.gears.insert(GearId::new("demo").unwrap(), gear);
    catalogue
}

fn demo_descriptor() -> GearDescriptor {
    GearDescriptor {
        maturity: gearbox_ir::Maturity::Stable,
        one_per_installation: false,
        id: GearId::new("demo").unwrap(),
        display_name: "Demo".to_owned(),
        description: None,
        category: None,
        visibility: gearbox_ir::Visibility::Internal,
        source: SourceId::new("s").unwrap(),
        gdl_path: gearbox_ir::RelPath::new("demo/gear.gdl").unwrap(),
        sdks: Vec::new(),
        implementation: None,
        scenarios: Vec::new(),
        origin: None,
        tests: Vec::new(),
        package: gearbox_ir::CargoRef {
            crate_name: "demo".to_owned(),
            lib_ident: "demo".to_owned(),
            path: gearbox_ir::RelPath::new("demo").unwrap(),
            features: Vec::new(),
            default_features: true,
            link: Vec::new(),
        },
        runtime_caps: BTreeSet::default(),
        colocated_deps: BTreeSet::default(),
        lifecycle: None,
        provides: Vec::new(),
        consumes: Vec::new(),
        requires: Vec::new(),
        serves: Vec::new(),
        client_trait: None,
        cluster_providers: Vec::new(),
        extension_points: Vec::new(),
        implements: None,
        vendor_selector: None,
        declared_roles: Vec::new(),
        available_features: BTreeSet::new(),
        cargo_features: None,
        config_schema: None,
        docs: None,
        gts_types: Vec::new(),
        declared_at: None,
    }
}

/// An intent setting `key` to `value` on the demo gear.
fn intent(key: &str, value: serde_json::Value) -> ProductIntent {
    let dev = gearbox_ir::ProfileId::new("dev").unwrap();
    let mut selection = gearbox_ir::GearSelection {
        gear: GearId::new("demo").unwrap(),
        source: SourceId::new("s").unwrap(),
        version: None,
        package: None,
        features: Vec::new(),
        config: BTreeMap::default(),
        plugins: Vec::new(),
        declared_at: None,
    };
    selection.config.insert(key.to_owned(), value);
    ProductIntent {
        templates: None,
        layout: None,
        id: "fixture".to_owned(),
        display_name: "Fixture".to_owned(),
        version: "0.0.0".to_owned(),
        gdl_path: gearbox_ir::RelPath::new("product.gdl").unwrap(),
        sources: BTreeMap::default(),
        profiles: [(
            dev.clone(),
            gearbox_ir::DeploymentProfileDecl::Embedded {
                id: dev.clone(),
                declared_at: None,
            },
        )]
        .into_iter()
        .collect(),
        default_profile: dev,
        selected_gears: vec![selection],
        bindings: Vec::new(),
        cluster_scopes: Vec::new(),
        application_pins: Vec::new(),
        preferences: Vec::new(),
    }
}

fn codes(key: &str, value: serde_json::Value) -> Vec<DiagnosticCode> {
    let mut diagnostics = Diagnostics::default();
    check(
        &catalogue(),
        &intent(key, value),
        "file:///p.gdl",
        &mut diagnostics,
    );
    diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn a_value_of_the_declared_type_passes() {
    for (key, value) in [
        ("bind_addr", serde_json::json!("0.0.0.0:8087")),
        ("enable_docs", serde_json::json!(true)),
        ("port", serde_json::json!(8087)),
        ("ratio", serde_json::json!(1.5)),
        // An integer is a perfectly good float, and it is how YAML writes one.
        ("ratio", serde_json::json!(1)),
        ("mode", serde_json::json!("accept_all")),
    ] {
        assert_eq!(codes(key, value.clone()), [], "for {key} = {value}");
    }
}

#[test]
fn a_value_of_the_wrong_type_is_reported() {
    for (key, value) in [
        ("bind_addr", serde_json::json!(true)),
        ("enable_docs", serde_json::json!("yes")),
        ("port", serde_json::json!("8087")),
        // A fractional number is not an integer, and truncating silently is how
        // a port becomes a different port.
        ("port", serde_json::json!(1.5)),
        ("mode", serde_json::json!("nope")),
    ] {
        assert_eq!(
            codes(key, value.clone()),
            [DiagnosticCode::GdlConfigTypeMismatch],
            "for {key} = {value}"
        );
    }
}

#[test]
fn the_message_names_the_key_the_struct_and_what_was_expected() {
    let mut diagnostics = Diagnostics::default();
    check(
        &catalogue(),
        &intent("mode", serde_json::json!("nope")),
        "file:///p.gdl",
        &mut diagnostics,
    );
    let message = diagnostics.iter().next().unwrap().message.clone();
    for part in ["mode", "DemoConfig", "accept_all", "static_tokens"] {
        assert!(message.contains(part), "missing `{part}` in: {message}");
    }
}

/// The projector could not read a shape, so there is no expectation to enforce.
/// Inventing one would refuse descriptions that were always correct.
#[test]
fn a_complex_field_accepts_anything() {
    for value in [
        serde_json::json!("a"),
        serde_json::json!(1),
        serde_json::json!({"a": 1}),
        serde_json::json!([1, 2]),
    ] {
        assert_eq!(codes("nested", value.clone()), [], "for {value}");
    }
}

/// A key the schema does not name becomes YAML the runtime refuses.
#[test]
fn a_key_outside_the_schema_is_an_error() {
    assert_eq!(
        codes("not_exposed", serde_json::json!(true)),
        [DiagnosticCode::GdlUnknownConfigKey]
    );
}

/// A key the generator derives is overwritten, and the whole point of the
/// warning is that it is not overwritten in silence.
#[test]
fn setting_a_derived_key_warns_without_failing() {
    let mut catalogue = catalogue();
    catalogue
        .gears
        .get_mut(&GearId::new("demo").unwrap())
        .unwrap()
        .serves = vec![gearbox_ir::EndpointDecl {
        name: "rest".to_owned(),
        config_key: Some("bind_addr".to_owned()),
        default_port: Some(8087),
        via: None,
    }];

    let mut diagnostics = Diagnostics::default();
    check(
        &catalogue,
        &intent("bind_addr", serde_json::json!("0.0.0.0:9999")),
        "file:///p.gdl",
        &mut diagnostics,
    );
    let reported: Vec<_> = diagnostics.iter().collect();
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].code, DiagnosticCode::GdlConfigKeyDerived);
    // A warning: the product still builds, the value is simply not the one used.
    assert_eq!(reported[0].severity, gearbox_ir::Severity::Warning);
    assert!(
        reported[0].message.contains("bind_addr"),
        "{:?}",
        reported[0]
    );
    // Named, so the Validation stage can offer the row that fixes it.
    assert_eq!(reported[0].config_key.as_deref(), Some("bind_addr"));
    assert!(reported[0].subject.is_some(), "{:?}", reported[0]);
    assert!(reported[0].help.is_some());
}

#[test]
fn a_gear_without_a_schema_is_not_checked() {
    let mut catalogue = catalogue();
    catalogue
        .gears
        .get_mut(&GearId::new("demo").unwrap())
        .unwrap()
        .config_schema = None;
    let mut diagnostics = Diagnostics::default();
    check(
        &catalogue,
        &intent("bind_addr", serde_json::json!(true)),
        "file:///p.gdl",
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty());
}

/// A catalogue with `demo` declaring one credential field.
fn catalogue_with_a_secret() -> Catalogue {
    let mut catalogue = catalogue();
    let schema = catalogue
        .gears
        .get_mut(&GearId::new("demo").unwrap())
        .unwrap()
        .config_schema
        .as_mut()
        .unwrap();
    schema.fields.push(ConfigFieldDecl {
        name: "password".to_owned(),
        ty: ConfigFieldType::Str,
        required: false,
        default: None,
        doc: None,
        secret: true,
    });
    catalogue
}

fn codes_of(catalogue: &Catalogue, intent: &ProductIntent) -> Vec<DiagnosticCode> {
    let mut diagnostics = Diagnostics::default();
    check(catalogue, intent, "file:///p/product.gdl", &mut diagnostics);
    diagnostics.as_slice().iter().map(|d| d.code).collect()
}

/// A credential written into the description is refused, not rewritten.
///
/// Rewriting is what the generator does, and it happens too late to help: the
/// value would still be in the `.gdl` file, which is the file that gets
/// committed. Only the author can take it out, so only the author is asked to.
#[test]
fn a_literal_credential_in_a_description_is_refused() {
    let catalogue = catalogue_with_a_secret();

    let literal = intent("password", serde_json::json!("hunter2"));
    assert_eq!(
        codes_of(&catalogue, &literal),
        vec![DiagnosticCode::GdlLiteralSecret]
    );

    // A default is the same secret wearing a reference's clothes: the runtime
    // expands it whenever the variable is unset, so `hunter2` is live.
    let defaulted = intent("password", serde_json::json!("${DEMO_PASSWORD:-hunter2}"));
    assert_eq!(
        codes_of(&catalogue, &defaulted),
        vec![DiagnosticCode::GdlLiteralSecret]
    );

    // A number is not a reference either. An integer token is still a token.
    let number = intent("password", serde_json::json!(1234));
    assert_eq!(
        codes_of(&catalogue, &number),
        vec![DiagnosticCode::GdlLiteralSecret]
    );

    // And the sanctioned form passes, with the name the generator will read.
    let named = intent("password", serde_json::json!("${DEMO_PASSWORD}"));
    assert!(codes_of(&catalogue, &named).is_empty());

    // Severity is the mechanism, not a label: `ResolvedProduct::is_writable` is
    // `!diagnostics.has_errors()`, and both the CLI and the RPC refuse to write a
    // lock that is not writable. That is why no second guard is needed to keep a
    // credential out of `product.lock` -- there is no lock to put it in.
    let mut diagnostics = Diagnostics::default();
    check(
        &catalogue,
        &literal,
        "file:///p/product.gdl",
        &mut diagnostics,
    );
    assert!(
        diagnostics.has_errors(),
        "a warning here would let the lock be written"
    );
}

/// The refusal names the variable, because the author's next act is to type it.
#[test]
fn the_refusal_spells_the_environment_variable() {
    let catalogue = catalogue_with_a_secret();
    let intent = intent("password", serde_json::json!("hunter2"));
    let mut diagnostics = Diagnostics::default();
    check(
        &catalogue,
        &intent,
        "file:///p/product.gdl",
        &mut diagnostics,
    );
    let help = diagnostics.as_slice()[0].help.clone().unwrap_or_default();
    assert!(help.contains("${DEMO_PASSWORD}"), "{help}");
}

/// A plugin's configuration is checked for credentials too.
///
/// It is the likeliest place for one -- an OIDC plugin's `client_secret` -- and
/// until now nothing looked at plugin configuration at all. Only the credential
/// rule reaches here; see `check_plugin_secrets` for why the unknown-key rule
/// cannot follow yet.
#[test]
fn a_credential_on_a_plugin_is_refused_as_well() {
    let mut catalogue = catalogue_with_a_secret();
    let plugin_id = GearId::new("demo-plugin").unwrap();
    let mut plugin = catalogue.gears[&GearId::new("demo").unwrap()].clone();
    plugin.id = plugin_id.clone();
    catalogue.gears.insert(plugin_id.clone(), plugin);

    let mut intent = intent("bind_addr", serde_json::json!("0.0.0.0:1"));
    intent.selected_gears[0]
        .plugins
        .push(gearbox_ir::PluginSelection {
            gear: plugin_id,
            config: [("password".to_owned(), serde_json::json!("hunter2"))]
                .into_iter()
                .collect(),
            profiles: BTreeSet::new(),
            entry_index: 0,
            declared_at: None,
        });

    assert_eq!(
        codes_of(&catalogue, &intent),
        vec![DiagnosticCode::GdlLiteralSecret]
    );
}

/// A required field with no default and no value is said out loud.
///
/// The Studio's configurator has been marking this state and telling the
/// reader a GBX code would explain it. There was no code, and the cost is
/// concrete: `EventBrokerConfig.mode` is required with no default and the
/// runtime's loader is strict, so the generated file is one the gear refuses
/// at `init`.
#[test]
fn a_required_field_with_no_default_and_no_value_is_reported() {
    let mut catalogue = catalogue();
    let gear = catalogue
        .gears
        .get_mut(&GearId::new("demo").unwrap())
        .unwrap();
    let schema = gear.config_schema.as_mut().unwrap();
    for f in &mut schema.fields {
        if f.name == "mode" {
            f.required = true;
        }
    }

    // The description sets something else, so `mode` is genuinely unsupplied.
    let mut diagnostics = Diagnostics::default();
    check(
        &catalogue,
        &intent("enable_docs", serde_json::json!(true)),
        "file:///p.gdl",
        &mut diagnostics,
    );
    let reported: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::GdlRequiredConfigUnset)
        .collect();
    assert_eq!(reported.len(), 1, "{diagnostics:#?}");
    assert_eq!(reported[0].severity, gearbox_ir::Severity::Warning);
    assert!(reported[0].message.contains("mode"), "{:?}", reported[0]);

    // **And it names the gear, not only the line.** The location points at the
    // `use_gear` entry, which is where the value would be *typed*; nobody sets
    // config that way in the Studio, where the configurator is a form reached
    // from a gear. Without a subject the Validation screen could offer the
    // description and an explanation and nothing else, so the route from this
    // very message to the field it names went through remembering the gear's id.
    assert_eq!(
        reported[0].subject.as_ref().map(gearbox_ir::NodeId::as_str),
        Some("gear:demo"),
        "{:?}",
        reported[0]
    );
    // **And which key, because the gear is not the answer either.** The subject
    // opens the right form; the message names the field; a person should not
    // have to read the name out of the sentence and find the row themselves.
    assert_eq!(
        reported[0].config_key.as_deref(),
        Some("mode"),
        "{:?}",
        reported[0]
    );

    // And setting it is the remedy the help names.
    let mut diagnostics = Diagnostics::default();
    check(
        &catalogue,
        &intent("mode", serde_json::json!("accept_all")),
        "file:///p.gdl",
        &mut diagnostics,
    );
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::GdlRequiredConfigUnset),
        "{diagnostics:#?}"
    );
}

/// A required field the generator supplies is not unset, and this exclusion is
/// what keeps the corpus out of the red.
///
/// `ApiGatewayConfig.bind_addr` is required, declares no default, and is
/// written from the port the resolver assigned. A rule that counted it would
/// fire on every product in the tree.
#[test]
fn a_required_field_the_generator_derives_is_not_reported() {
    let mut catalogue = catalogue();
    let gear = catalogue
        .gears
        .get_mut(&GearId::new("demo").unwrap())
        .unwrap();
    gear.serves = vec![gearbox_ir::EndpointDecl {
        name: "rest".to_owned(),
        config_key: Some("bind_addr".to_owned()),
        default_port: Some(8087),
        via: None,
    }];
    let schema = gear.config_schema.as_mut().unwrap();
    for f in &mut schema.fields {
        if f.name == "bind_addr" {
            f.required = true;
        }
    }

    let mut diagnostics = Diagnostics::default();
    check(
        &catalogue,
        &intent("enable_docs", serde_json::json!(true)),
        "file:///p.gdl",
        &mut diagnostics,
    );
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::GdlRequiredConfigUnset),
        "a derived key is supplied by generation, not by the description: {diagnostics:#?}"
    );
}

/// A required field that declares a default is supplied by Rust.
#[test]
fn a_required_field_with_a_compiled_in_default_is_not_reported() {
    let mut catalogue = catalogue();
    let schema = catalogue
        .gears
        .get_mut(&GearId::new("demo").unwrap())
        .unwrap()
        .config_schema
        .as_mut()
        .unwrap();
    for f in &mut schema.fields {
        if f.name == "mode" {
            f.required = true;
            f.default = Some(serde_json::json!("accept_all"));
        }
    }

    let mut diagnostics = Diagnostics::default();
    check(
        &catalogue,
        &intent("enable_docs", serde_json::json!(true)),
        "file:///p.gdl",
        &mut diagnostics,
    );
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::GdlRequiredConfigUnset),
        "{diagnostics:#?}"
    );
}
