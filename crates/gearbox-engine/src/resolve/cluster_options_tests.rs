//! What a `provider(...)` may say, and what it may not.

use gearbox_ir::{
    ClusterPrimitive, ClusterProviderDecl, ConfigFieldDecl, ConfigFieldType, ConfigSchema,
    Diagnostics, ProviderBinding,
};
use std::collections::{BTreeMap, BTreeSet};

fn field(name: &str, ty: ConfigFieldType, required: bool) -> ConfigFieldDecl {
    ConfigFieldDecl {
        name: name.to_owned(),
        ty,
        required,
        default: None,
        doc: None,
        secret: false,
        fields: Vec::new(),
    }
}

fn postgres() -> ClusterProviderDecl {
    let schema = ConfigSchema {
        rust: "PostgresClusterConfig".to_owned(),
        fields: vec![
            field("connection_string", ConfigFieldType::Str, true),
            field("pool_max_size", ConfigFieldType::Int, false),
            field(
                "replication_mode",
                ConfigFieldType::Enum {
                    variants: vec!["async".to_owned(), "sync".to_owned()],
                },
                false,
            ),
        ],
    };
    ClusterProviderDecl {
        name: "postgres".to_owned(),
        primitives: [ClusterPrimitive::Cache].into_iter().collect(),
        capabilities: BTreeMap::new(),
        process_local: false,
        needs_credentials: true,
        runtime_determined: BTreeSet::new(),
        gated_by: BTreeMap::new(),
        options: [(ClusterPrimitive::Cache, schema)].into_iter().collect(),
        credential_option: Some("connection_string".to_owned()),
    }
}

/// A provider whose plugin declares no options struct: the untyped bag.
fn undeclared() -> ClusterProviderDecl {
    ClusterProviderDecl {
        options: BTreeMap::new(),
        credential_option: None,
        ..postgres()
    }
}

fn binding(options: &[(&str, serde_json::Value)]) -> ProviderBinding {
    ProviderBinding {
        provider: "postgres".to_owned(),
        options: options
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
        secret_ref: None,
        declared_at: None,
    }
}

fn codes(providers: &[ClusterProviderDecl], binding: &ProviderBinding) -> Vec<String> {
    let mut diagnostics = Diagnostics::new();
    super::check(
        providers,
        ClusterPrimitive::Cache,
        binding,
        "file:///product.gdl",
        &mut diagnostics,
    );
    diagnostics
        .as_slice()
        .iter()
        .map(|d| d.code.as_str().to_owned())
        .collect()
}

#[test]
fn the_keys_the_backend_reads_are_accepted() {
    let got = codes(
        &[postgres()],
        &binding(&[
            ("connection_string", serde_json::json!("postgres://${PG}/x")),
            ("pool_max_size", serde_json::json!(10)),
            ("replication_mode", serde_json::json!("sync")),
        ]),
    );
    assert_eq!(got, Vec::<String>::new(), "a correct binding was refused");
}

#[test]
fn a_key_the_backend_does_not_read_is_refused() {
    let got = codes(
        &[postgres()],
        &binding(&[
            ("connection_string", serde_json::json!("${PG}")),
            ("pool_maximum_size", serde_json::json!(10)),
        ]),
    );
    assert_eq!(got, vec!["GBX0522"]);
}

#[test]
fn a_value_the_field_cannot_take_is_refused() {
    // A wrong scalar, and a variant of a closed set that does not exist. The
    // second is the one a name heuristic could never catch.
    let got = codes(
        &[postgres()],
        &binding(&[
            ("connection_string", serde_json::json!("${PG}")),
            ("pool_max_size", serde_json::json!("ten")),
            ("replication_mode", serde_json::json!("disbaled")),
        ]),
    );
    assert_eq!(got, vec!["GBX0523", "GBX0523"]);
}

#[test]
fn an_option_the_backend_cannot_supply_itself_is_reported_when_absent() {
    let got = codes(
        &[postgres()],
        &binding(&[("pool_max_size", serde_json::json!(4))]),
    );
    assert_eq!(got, vec!["GBX0524"]);
}

#[test]
fn a_credential_written_as_a_literal_is_refused() {
    let got = codes(
        &[postgres()],
        &binding(&[(
            "connection_string",
            serde_json::json!("postgres://u:hunter2@db/x"),
        )]),
    );
    assert_eq!(got, vec!["GBX0116"]);
}

#[test]
fn an_expansion_is_not_a_literal_and_not_a_type_error() {
    // `${VAR}` is a string on the wire whatever the field's type is, and it is
    // the shape the plugins' own `deserialize_and_expand` reads. Comparing it
    // against `Int` would refuse the correct spelling; calling it a literal
    // credential would refuse the recommended one.
    let got = codes(
        &[postgres()],
        &binding(&[
            ("connection_string", serde_json::json!("${DATABASE_URL}")),
            ("pool_max_size", serde_json::json!("${PG_POOL}")),
        ]),
    );
    assert_eq!(got, Vec::<String>::new());
}

#[test]
fn a_provider_with_no_declared_options_is_left_alone() {
    // Today's behaviour, kept deliberately: a plugin nobody has declared an
    // options struct for keeps its untyped bag rather than having every key
    // reported as unknown.
    let got = codes(
        &[undeclared()],
        &binding(&[("anything", serde_json::json!(1))]),
    );
    assert_eq!(got, Vec::<String>::new());
}

#[test]
fn a_provider_that_is_not_registered_is_not_this_check_s_business() {
    // `GBX0505` says so, once, where the binding is resolved. Reporting every
    // option of an unknown provider as unknown would bury it.
    let got = codes(&[], &binding(&[("anything", serde_json::json!(1))]));
    assert_eq!(got, Vec::<String>::new());
}
