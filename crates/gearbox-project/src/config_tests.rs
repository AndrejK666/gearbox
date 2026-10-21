//! Tests for configuration projection.
//!
//! Fixtures reproduce the shapes `gears-rust` actually uses; the real-tree tests
//! below then prove the fixtures have not drifted from what they mirror. Both
//! halves matter: a fixture that has drifted passes while the corpus fails, and
//! a corpus test alone cannot run where the corpus is absent.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;

use gearbox_ir::ConfigFieldType;

use super::*;
use crate::scan::scan_crate;
use crate::test_corpus::require;

fn file(src: &str) -> RustFile {
    RustFile {
        path: PathBuf::from("fixture.rs"),
        relative: PathBuf::from("fixture.rs"),
        ast: syn::parse_file(src).expect("fixture parses"),
    }
}

fn tree(rel: &str) -> Option<Vec<RustFile>> {
    let dir = crate::test_corpus::corpus(rel)?;
    Some(scan_crate(&dir).unwrap_or_else(|e| panic!("scan {rel}: {e}")))
}

fn fields(files: &[RustFile], root: &str) -> Vec<ConfigField> {
    project_config_fields(files, root).unwrap_or_else(|e| panic!("project `{root}`: {e}"))
}

fn named<'a>(fields: &'a [ConfigField], name: &str) -> &'a ConfigField {
    fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no field `{name}` in {:?}", names(fields)))
}

fn names(fields: &[ConfigField]) -> Vec<&str> {
    fields.iter().map(|f| f.name.as_str()).collect()
}

// -- finding the struct ----------------------------------------------------

#[test]
fn the_turbofish_spelling_names_the_config_type() {
    let files = [file(
        r"
        impl Gear for ApiGateway {
            async fn init(&self, ctx: &GearCtx) -> Result<()> {
                let cfg = ctx.config_or_default::<crate::config::ApiGatewayConfig>()?;
                Ok(())
            }
        }
        ",
    )];
    assert_eq!(
        project_config_root(&files),
        Ok(Some("ApiGatewayConfig".to_owned()))
    );
}

/// Ten of the eleven configured gears write it this way, so reading only the
/// turbofish would report "no configuration" for almost the whole corpus.
#[test]
fn the_annotated_binding_spelling_names_it_too() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        pub struct GrpcHubConfig { pub listen_addr: String }

        impl Gear for GrpcHub {
            async fn init(&self, ctx: &GearCtx) -> Result<()> {
                let cfg: GrpcHubConfig = ctx.config_or_default()?;
                Ok(())
            }
        }
        ",
    )];
    assert_eq!(
        project_config_root(&files),
        Ok(Some("GrpcHubConfig".to_owned()))
    );
}

#[test]
fn a_gear_that_reads_no_config_has_no_root() {
    let files = [file(
        r"
        impl Gear for Orchestrator {
            async fn init(&self, _ctx: &GearCtx) -> Result<()> { Ok(()) }
        }
        ",
    )];
    assert_eq!(project_config_root(&files), Ok(None));
}

#[test]
fn two_config_types_are_ambiguous_rather_than_guessed() {
    let files = [file(
        r"
        pub struct AlphaConfig { pub a: String }
        fn a(ctx: &GearCtx) { let _: AlphaConfig = ctx.config_or_default().unwrap(); }
        fn b(ctx: &GearCtx) { let _ = ctx.config::<BetaConfig>().unwrap(); }
        ",
    )];
    assert_eq!(
        project_config_root(&files),
        Err(ConfigRootError::Ambiguous {
            roots: vec!["AlphaConfig".to_owned(), "BetaConfig".to_owned()],
        })
    );
}

// -- classifying fields ----------------------------------------------------

#[test]
fn scalars_are_classified_and_everything_else_is_complex() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        pub struct DemoConfig {
            pub bind_addr: String,
            pub enabled: bool,
            pub port: u16,
            pub ratio: f64,
            pub tags: Vec<String>,
            pub nested: Inner,
        }
        #[derive(Deserialize)]
        pub struct Inner { pub a: String }
        ",
    )];
    let fields = fields(&files, "DemoConfig");
    assert_eq!(
        names(&fields),
        ["bind_addr", "enabled", "port", "ratio", "tags", "nested"]
    );
    assert_eq!(named(&fields, "bind_addr").ty, ConfigFieldType::Str);
    assert_eq!(named(&fields, "enabled").ty, ConfigFieldType::Bool);
    assert_eq!(named(&fields, "port").ty, ConfigFieldType::Int);
    assert_eq!(named(&fields, "ratio").ty, ConfigFieldType::Float);
    assert_eq!(named(&fields, "tags").ty, ConfigFieldType::Complex);
    assert_eq!(named(&fields, "nested").ty, ConfigFieldType::Complex);
}

/// The whole point of §8 of the plan: a new enum must not need a Gearbox
/// release, so the variants travel as data read from the enum itself.
#[test]
fn a_unit_enum_projects_its_variants_through_rename_all() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        pub struct DemoConfig { pub mode: AuthNMode }

        #[derive(Deserialize, Default)]
        #[serde(rename_all = "snake_case")]
        pub enum AuthNMode {
            #[default]
            AcceptAll,
            StaticTokens,
        }
        "#,
    )];
    let fields = fields(&files, "DemoConfig");
    assert_eq!(
        named(&fields, "mode").ty,
        ConfigFieldType::Enum {
            variants: vec!["accept_all".to_owned(), "static_tokens".to_owned()],
        }
    );
}

#[test]
fn an_enum_with_data_carrying_variants_is_complex() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        pub struct DemoConfig { pub auth: InternalAuthConfig }

        #[derive(Deserialize)]
        #[serde(tag = "provider", rename_all = "snake_case")]
        pub enum InternalAuthConfig {
            SharedSecret { secret: String },
            Kube { audiences: Vec<String> },
        }
        "#,
    )];
    assert_eq!(
        named(&fields(&files, "DemoConfig"), "auth").ty,
        ConfigFieldType::Complex
    );
}

/// `InternalAuthEnforcement` lives in `libs/toolkit-transport-grpc`, outside the
/// crate scan. Degrading to "no control" is the honest answer; inventing the
/// variants would write values the gear rejects.
#[test]
fn an_enum_defined_outside_the_scan_degrades_instead_of_guessing() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        pub struct DemoConfig { pub enforcement: InternalAuthEnforcement }
        ",
    )];
    assert_eq!(
        named(&fields(&files, "DemoConfig"), "enforcement").ty,
        ConfigFieldType::Complex
    );
}

#[test]
fn an_unknown_serde_eq_form_does_not_drop_later_keys() {
    // `parse_nested_meta` used to abort on `serialize_with = "..."`, which
    // dropped a following `rename` and projected the Rust name instead.
    let files = [file(
        r#"
        #[derive(Deserialize)]
        pub struct TenantConfig {
            #[serde(serialize_with = "ser", rename = "type")]
            pub tenant_type: String,
        }
        "#,
    )];
    assert_eq!(names(&fields(&files, "TenantConfig")), ["type"]);
}

#[test]
fn a_renamed_field_projects_under_its_wire_name() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        pub struct TenantConfig {
            #[serde(rename = "type", default)]
            pub tenant_type: Option<String>,
        }
        "#,
    )];
    let fields = fields(&files, "TenantConfig");
    assert_eq!(names(&fields), ["type"]);
}

#[test]
fn a_custom_codec_is_complex_rather_than_its_rust_type() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        pub struct CacheSettings {
            #[serde(with = "toolkit_utils::humantime_serde::option")]
            pub ttl: Option<Duration>,
        }
        "#,
    )];
    assert_eq!(
        named(&fields(&files, "CacheSettings"), "ttl").ty,
        ConfigFieldType::Complex
    );
}

#[test]
fn a_secret_typed_field_is_flagged() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        pub struct TokenMapping { pub token: SecretString }
        ",
    )];
    let fields = fields(&files, "TokenMapping");
    assert!(named(&fields, "token").secret);
    assert_eq!(named(&fields, "token").ty, ConfigFieldType::Str);
}

/// Every `secrecy` wrapper, not just the alias. `secret` is the only gate
/// keeping a credential out of the generated `ConfigMap`, so a field typed
/// `Secret<String>` coming back as an ordinary value meant the generator wrote
/// it in plaintext.
#[test]
fn every_secrecy_wrapper_is_flagged() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        pub struct Creds {
            pub a: SecretString,
            pub b: Secret<String>,
            pub c: SecretBox<str>,
            pub d: SecretVec<u8>,
            pub e: Secret<u16>,
            pub plain: String,
        }
        ",
    )];
    let fields = fields(&files, "Creds");
    for name in ["a", "b", "c", "d", "e"] {
        assert!(
            named(&fields, name).secret,
            "`{name}` is a secrecy wrapper and must project as a credential"
        );
    }
    assert!(!named(&fields, "plain").secret);
    // The control follows what is wrapped, and a list of bytes has none.
    assert_eq!(named(&fields, "b").ty, ConfigFieldType::Str);
    assert_eq!(named(&fields, "c").ty, ConfigFieldType::Str);
    assert_eq!(named(&fields, "d").ty, ConfigFieldType::Complex);
    assert_eq!(named(&fields, "e").ty, ConfigFieldType::Int);
}

/// `#[serde(with)]` used to force `secret: false`, so a `SecretString` under a
/// custom codec projected as an ordinary field and the credential went into the
/// `ConfigMap`. The unreadable wire shape is a reason to report `Complex`, not a
/// reason to forget what the field holds.
#[test]
fn a_custom_codec_hides_the_shape_but_not_the_secret() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        pub struct TokenConfig {
            #[serde(with = "my_codec")]
            pub token: SecretString,
            #[serde(flatten)]
            pub nested: SecretString,
        }
        "#,
    )];
    let fields = fields(&files, "TokenConfig");
    for name in ["token", "nested"] {
        let field = named(&fields, name);
        assert_eq!(
            field.ty,
            ConfigFieldType::Complex,
            "`{name}`'s wire shape is not its type's, so there is no control"
        );
        assert!(
            field.secret,
            "`{name}` is still a credential, and `secret` is the only gate"
        );
    }
}

/// A compiled-in credential default used to be projected verbatim and travel
/// into the catalogue, walking straight around the `secret` flag computed a few
/// lines away.
#[test]
fn a_secret_field_does_not_carry_its_compiled_in_default() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        pub struct DemoConfig {
            #[serde(default = "default_password")]
            pub password: SecretString,
            #[serde(default = "default_user")]
            pub user: String,
        }
        fn default_password() -> SecretString { SecretString::from("hunter2") }
        fn default_user() -> String { "admin".to_owned() }
        "#,
    )];
    let fields = fields(&files, "DemoConfig");
    assert_eq!(
        named(&fields, "password").default,
        None,
        "a credential default must not travel into the catalogue"
    );
    assert!(
        !named(&fields, "password").required,
        "the default is still what makes the field optional"
    );
    // The ordinary field is unaffected: this drops secrets, not defaults.
    assert_eq!(
        named(&fields, "user").default,
        Some(serde_json::Value::String("admin".to_owned()))
    );
}

/// The container-level rule, which nothing exercised: the three fixtures that
/// use `rename_all` all put it on an enum, which goes through `variant_names`
/// instead. This is the value an operator has to type as a YAML key.
#[test]
fn a_container_rename_all_spells_every_field_the_wire_way() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct DemoConfig {
            pub bind_addr: String,
            pub healthcheck_timeout_ms: u32,
            #[serde(rename = "type")]
            pub tenant_type: String,
        }
        "#,
    )];
    assert_eq!(
        names(&fields(&files, "DemoConfig")),
        ["bindAddr", "healthcheckTimeoutMs", "type"],
        "a field's own `rename` wins over the container rule"
    );
}

/// An unrecognised rule leaves the name alone rather than guessing: a wrong
/// rename produces a key the gear silently never reads, which is worse than no
/// rename at all.
#[test]
fn an_unrecognised_rename_all_leaves_the_field_name_alone() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        #[serde(rename_all = "TitleCase")]
        pub struct DemoConfig { pub bind_addr: String }
        "#,
    )];
    assert_eq!(names(&fields(&files, "DemoConfig")), ["bind_addr"]);
}

/// The whole `rename_all` case table, which the container test above reaches one
/// arm of. Each is a key an operator types.
#[test]
fn every_rename_all_rule_is_applied_as_serde_spells_it() {
    for (rule, expected) in [
        ("lowercase", "bind_addr"),
        ("UPPERCASE", "BIND_ADDR"),
        ("PascalCase", "BindAddr"),
        ("camelCase", "bindAddr"),
        ("snake_case", "bind_addr"),
        ("SCREAMING_SNAKE_CASE", "BIND_ADDR"),
        ("kebab-case", "bind-addr"),
        ("SCREAMING-KEBAB-CASE", "BIND-ADDR"),
    ] {
        let files = [file(&format!(
            r#"
            #[derive(Deserialize)]
            #[serde(rename_all = "{rule}")]
            pub struct DemoConfig {{ pub bind_addr: String }}
            "#
        ))];
        assert_eq!(
            names(&fields(&files, "DemoConfig")),
            [expected],
            "`rename_all = \"{rule}\"`"
        );
    }
}

/// `#[serde(rename(serialize = "a", deserialize = "b"), skip)]` is legal serde,
/// and `parse_nested_meta` cannot model the nested form -- so it stopped there
/// and lost the `skip`, projecting a field serde never reads as configuration.
#[test]
fn a_serde_form_this_cannot_read_is_refused_rather_than_truncated() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        pub struct DemoConfig {
            #[serde(rename(serialize = "a", deserialize = "b"), skip)]
            pub internal: String,
            pub kept: String,
        }
        "#,
    )];
    assert_eq!(
        project_config_fields(&files, "DemoConfig"),
        Err(ConfigFieldsError::UnreadableSerdeAttribute {
            root: "DemoConfig".to_owned(),
            field: Some("internal".to_owned()),
        })
    );
}

/// A config struct inside an inline `mod`. Root discovery walks inline modules,
/// so the lookups it feeds have to as well, or the two halves of one projection
/// disagree about which structs exist.
#[test]
fn a_root_inside_an_inline_module_projects_its_fields_and_defaults() {
    let files = [file(
        r#"
        pub mod config {
            #[derive(Deserialize)]
            #[serde(default)]
            pub struct NestedConfig {
                pub vendor: String,
                pub mode: Mode,
            }

            impl Default for NestedConfig {
                fn default() -> Self {
                    Self { vendor: "constructorfabric".to_owned(), mode: Mode::AcceptAll }
                }
            }

            #[derive(Deserialize)]
            #[serde(rename_all = "snake_case")]
            pub enum Mode { AcceptAll, StaticTokens }
        }
        "#,
    )];
    let fields = fields(&files, "NestedConfig");
    assert_eq!(names(&fields), ["vendor", "mode"]);
    assert_eq!(
        named(&fields, "vendor").default,
        Some(serde_json::Value::String("constructorfabric".to_owned()))
    );
    assert_eq!(
        named(&fields, "mode").ty,
        ConfigFieldType::Enum {
            variants: vec!["accept_all".to_owned(), "static_tokens".to_owned()],
        },
        "the enum is in the same inline module and must be found there"
    );
}

#[test]
fn a_skipped_field_is_not_part_of_the_surface() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        pub struct DemoConfig {
            pub kept: String,
            #[serde(skip)]
            pub dropped: String,
        }
        ",
    )];
    assert_eq!(names(&fields(&files, "DemoConfig")), ["kept"]);
}

// -- required and defaults -------------------------------------------------

#[test]
fn required_follows_serde_rather_than_the_type() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        pub struct DemoConfig {
            pub must: String,
            #[serde(default)]
            pub has_default: String,
            #[serde(default = "d")]
            pub has_fn: String,
            pub maybe: Option<String>,
        }
        fn d() -> String { "from-fn".to_owned() }
        "#,
    )];
    let fields = fields(&files, "DemoConfig");
    assert!(named(&fields, "must").required);
    assert!(!named(&fields, "has_default").required);
    assert!(!named(&fields, "has_fn").required);
    // serde's `missing_field` succeeds for a type that deserializes from
    // nothing, so an `Option` is optional without a `default` saying so.
    assert!(!named(&fields, "maybe").required);
}

#[test]
fn a_container_default_makes_every_field_optional() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct AuthNResolverConfig { pub vendor: String }
        ",
    )];
    assert!(!named(&fields(&files, "AuthNResolverConfig"), "vendor").required);
}

/// Both spellings of "the default", because both are in the tree -- the same
/// hazard `project_vendor_default` already had to learn.
#[test]
fn defaults_are_read_from_a_default_impl_and_from_a_serde_fn() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        #[serde(default)]
        pub struct DemoConfig {
            pub vendor: String,
            #[serde(default = "default_priority")]
            pub priority: u32,
            #[serde(default = "yes")]
            pub enabled: bool,
        }
        impl Default for DemoConfig {
            fn default() -> Self {
                Self { vendor: "constructorfabric".to_owned(), priority: 100, enabled: true }
            }
        }
        fn default_priority() -> u32 { 42 }
        fn yes() -> bool { true }
        "#,
    )];
    let fields = fields(&files, "DemoConfig");
    assert_eq!(
        named(&fields, "vendor").default,
        Some(serde_json::Value::String("constructorfabric".to_owned()))
    );
    // The `serde(default = ...)` fn wins over the `Default` impl: it is what
    // serde actually calls for that field.
    assert_eq!(
        named(&fields, "priority").default,
        Some(serde_json::Value::from(42))
    );
    assert_eq!(
        named(&fields, "enabled").default,
        Some(serde_json::Value::Bool(true))
    );
}

#[test]
fn default_fn_does_not_drop_a_following_rename() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        pub struct DemoConfig {
            #[serde(default = "d", rename = "type")]
            pub kind: String,
        }
        fn d() -> String { "x".to_owned() }
        "#,
    )];
    let fields = fields(&files, "DemoConfig");
    assert_eq!(named(&fields, "type").name, "type");
}

/// Three defaults that were wrong before they were read off the real corpus, and
/// each was wrong in the same way: a path is a *name*, not a value.
#[test]
fn a_path_default_is_resolved_or_omitted_but_never_reported_as_its_own_name() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        #[serde(default)]
        pub struct DemoConfig {
            pub advertise_addr: Option<String>,
            pub ttl_secs: u64,
            pub mode: AuthNMode,
        }

        impl Default for DemoConfig {
            fn default() -> Self {
                Self {
                    advertise_addr: None,
                    ttl_secs: DEFAULT_TTL_SECS,
                    mode: AuthNMode::AcceptAll,
                }
            }
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum AuthNMode { AcceptAll, StaticTokens }
        "#,
    )];
    let fields = fields(&files, "DemoConfig");

    // `None` is the absence of a default, not the string "None".
    assert_eq!(named(&fields, "advertise_addr").default, None);
    // A `const` is a name this cannot resolve; reporting the identifier as the
    // value would put `DEFAULT_TTL_SECS` in a number box.
    assert_eq!(named(&fields, "ttl_secs").default, None);
    // An enum default is written in Rust and read in YAML, so it is spelled as
    // the wire spells it -- and as its own variants list spells it.
    assert_eq!(
        named(&fields, "mode").default,
        Some(serde_json::Value::String("accept_all".to_owned()))
    );
}

#[test]
fn a_unit_struct_has_no_surface() {
    let files = [file(
        r"#[derive(Deserialize)] pub struct ApiContractsConfig;",
    )];
    assert_eq!(
        project_config_fields(&files, "ApiContractsConfig"),
        Err(ConfigFieldsError::NoNamedFields {
            root: "ApiContractsConfig".to_owned()
        })
    );
}

/// The two answers that used to be one empty vector. `root` can be an
/// operator-supplied string via `config_schema = config(rust = ...)`, so "there
/// is no such struct" and "the struct has no keys" are different mistakes and
/// need different sentences.
#[test]
fn a_missing_root_and_a_keyless_struct_are_different_failures() {
    let files = [file(r"pub struct Other { pub a: String }")];
    assert_eq!(
        project_config_fields(&files, "NoSuchConfig"),
        Err(ConfigFieldsError::RootNotFound {
            root: "NoSuchConfig".to_owned()
        })
    );

    let keyless = [file(r"pub struct Tuple(pub String);")];
    assert_eq!(
        project_config_fields(&keyless, "Tuple"),
        Err(ConfigFieldsError::NoNamedFields {
            root: "Tuple".to_owned()
        })
    );
}

/// Every field skipped is a third answer again: the struct is there, it has
/// named keys, and none of them is configuration.
#[test]
fn a_struct_whose_every_field_is_skipped_projects_an_empty_surface() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        pub struct DemoConfig {
            #[serde(skip)]
            pub internal: String,
        }
        ",
    )];
    assert_eq!(project_config_fields(&files, "DemoConfig"), Ok(Vec::new()));
}

#[test]
fn the_doc_comment_travels_as_the_operator_facing_prose() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        pub struct DemoConfig {
            /// Listen address for the gRPC server.
            pub listen_addr: String,
        }
        ",
    )];
    assert_eq!(
        named(&fields(&files, "DemoConfig"), "listen_addr")
            .doc
            .as_deref(),
        Some("Listen address for the gRPC server.")
    );
}

// -- against the real tree -------------------------------------------------

#[test]
fn api_gateway_projects_its_scalars_from_the_real_crate() {
    let files = require!(tree("gears/system/api-gateway"));
    let root = project_config_root(&files).expect("one config type");
    assert_eq!(root.as_deref(), Some("ApiGatewayConfig"));

    let fields = fields(&files, "ApiGatewayConfig");
    assert_eq!(named(&fields, "bind_addr").ty, ConfigFieldType::Str);
    // The only field with neither a container nor a field default.
    assert!(named(&fields, "bind_addr").required);
    assert_eq!(named(&fields, "enable_docs").ty, ConfigFieldType::Bool);
    assert!(!named(&fields, "enable_docs").required);
    assert_eq!(
        named(&fields, "healthcheck_timeout_ms").ty,
        ConfigFieldType::Int
    );
    // Nested structures carry no control.
    assert_eq!(named(&fields, "openapi").ty, ConfigFieldType::Complex);
    assert_eq!(named(&fields, "defaults").ty, ConfigFieldType::Complex);
}

/// `grpc-hub` keeps its config struct in `src/gear.rs`, so any rule that looked
/// for `src/config.rs` would silently report this gear as unconfigured.
#[test]
fn grpc_hub_is_found_although_its_struct_is_not_in_config_rs() {
    let files = require!(tree("gears/system/grpc-hub"));
    assert_eq!(
        project_config_root(&files)
            .expect("one config type")
            .as_deref(),
        Some("GrpcHubConfig")
    );
    let fields = fields(&files, "GrpcHubConfig");
    assert_eq!(named(&fields, "listen_addr").ty, ConfigFieldType::Str);
    assert_eq!(
        named(&fields, "internal_auth_cache_ttl_secs").ty,
        ConfigFieldType::Int
    );
}

#[test]
fn static_authn_plugin_projects_its_mode_enum_from_the_real_crate() {
    let files = require!(tree(
        "gears/system/authn-resolver/plugins/static-authn-plugin"
    ));
    let fields = fields(&files, "StaticAuthNPluginConfig");
    assert_eq!(
        named(&fields, "mode").ty,
        ConfigFieldType::Enum {
            variants: vec!["accept_all".to_owned(), "static_tokens".to_owned()],
        }
    );
    assert_eq!(
        named(&fields, "vendor").default,
        Some(serde_json::Value::String("constructorfabric".to_owned()))
    );
}

#[test]
fn tenant_resolver_projects_its_single_vendor_field() {
    let files = require!(tree("gears/system/tenant-resolver/tenant-resolver"));
    assert_eq!(
        project_config_root(&files)
            .expect("one config type")
            .as_deref(),
        Some("TenantResolverConfig")
    );
    let fields = fields(&files, "TenantResolverConfig");
    assert_eq!(names(&fields), ["vendor"]);
    assert_eq!(
        named(&fields, "vendor").default,
        Some(serde_json::Value::String("constructorfabric".to_owned()))
    );
}

/// A gear whose whole configuration is one map gets no typed controls at all,
/// and that is the right answer rather than a gap to fill with string boxes.
///
/// **`cluster` and not `types-registry`, and the difference is a lesson.** This
/// asserted both until `types-registry` grew an `allow_compatibility_force:
/// bool` upstream and the test failed for a change that was none of its
/// business. A corpus test may assert what the projector reads; it may not
/// assert what someone else's struct is allowed to contain.
///
/// **Learned twice.** `gear_orchestrator_reads_no_config_at_all` was the same
/// mistake in the other direction -- it pinned `gear-orchestrator` (now `service-discovery`) to reading
/// *no* config -- and it went red when that gear grew `OrchestratorConfig` (now `ServiceDiscoveryConfig`) to
/// authorize registration RPCs against peer identity. The projector was correct
/// throughout, so the test was deleted rather than re-pointed: aiming it at
/// whichever gear has no config today only re-arms it. The `Ok(None)` path is
/// held by `a_gear_that_reads_no_config_has_no_root`, whose fixture is inline
/// and so cannot be invalidated from another repository.
#[test]
fn a_configuration_that_is_all_collections_offers_no_controls() {
    let cluster = require!(tree("gears/system/cluster/cluster"));
    let fields = fields(&cluster, "ClusterConfig");
    assert!(!fields.is_empty(), "the struct is found");
    assert!(
        fields.iter().all(|f| f.ty == ConfigFieldType::Complex),
        "expected every field complex, got {fields:?}"
    );
}

/// The collection fields of `types-registry` carry no control, whatever else
/// the struct grows around them.
#[test]
fn a_vec_field_in_the_real_corpus_is_complex() {
    let files = require!(tree("gears/system/types-registry/types-registry"));
    let fields = fields(&files, "TypesRegistryConfig");
    for name in ["entity_id_fields", "schema_id_fields", "entities"] {
        assert_eq!(
            named(&fields, name).ty,
            ConfigFieldType::Complex,
            "`{name}` is a list and carries no control"
        );
    }
}

/// The deep search finds the call wherever it sits, so the binding's type is
/// only believed when it names a struct -- otherwise a chained call would
/// record whatever the expression happened to end up as.
#[test]
fn a_binding_that_is_not_a_struct_is_not_mistaken_for_the_config_type() {
    let files = [file(
        r"
        pub struct DemoConfig { pub a: String }
        fn f(ctx: &GearCtx) {
            let _len: usize = ctx.config::<DemoConfig>().unwrap().a.len();
        }
        ",
    )];
    assert_eq!(
        project_config_root(&files),
        Ok(Some("DemoConfig".to_owned()))
    );
}

// -- one level into a nested struct ------------------------------------------

/// A struct-typed field carries the fields of the struct it names, one level
/// down, so a check can say that a set `jwt = {...}` lacks `claim_mapping`.
#[test]
fn a_struct_typed_field_carries_one_nested_level() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        pub struct PluginConfig {
            pub jwt: JwtConfigInput,
            pub cache: Option<Cache>,
        }
        #[derive(Deserialize)]
        pub struct JwtConfigInput {
            #[serde(default)]
            pub require_audience: bool,
            pub claim_mapping: ClaimMappingInput,
        }
        #[derive(Deserialize)]
        pub struct ClaimMappingInput { pub subject: String }
        #[derive(Deserialize)]
        pub struct Cache { pub ttl: u64 }
        ",
    )];
    let projected = fields(&files, "PluginConfig");
    let jwt = named(&projected, "jwt");
    assert_eq!(jwt.ty, ConfigFieldType::Complex, "the control is unchanged");
    assert_eq!(names(&jwt.fields), ["require_audience", "claim_mapping"]);
    let claim_mapping = named(&jwt.fields, "claim_mapping");
    assert!(claim_mapping.required);
    assert!(
        claim_mapping.fields.is_empty(),
        "one level only: `claim_mapping`'s own fields are not projected"
    );
    assert_eq!(names(&named(&projected, "cache").fields), ["ttl"]);
}

/// A struct whose wire shape is another type's has no nested keys: the event
/// broker's `topics` is a transparent map, and `entries` is never written.
#[test]
fn a_transparent_or_converted_struct_has_no_nested_level() {
    let files = [file(
        r#"
        #[derive(Deserialize)]
        pub struct C { pub topics: Topics, pub port: Port }
        #[derive(Deserialize)]
        #[serde(transparent)]
        pub struct Topics { pub entries: BTreeMap<String, u8> }
        #[derive(Deserialize)]
        #[serde(try_from = "u16")]
        pub struct Port { pub value: u16 }
        "#,
    )];
    for field in fields(&files, "C") {
        assert!(
            field.fields.is_empty(),
            "{}: {:?}",
            field.name,
            field.fields
        );
    }
}

/// A type from outside the crate, or a scalar, has no nested level.
#[test]
fn a_foreign_or_scalar_field_has_no_nested_level() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        pub struct C { pub url: String, pub tls: other_crate::Tls, pub list: Vec<Item> }
        #[derive(Deserialize)]
        pub struct Item { pub a: u8 }
        ",
    )];
    let projected = fields(&files, "C");
    for field in &projected {
        assert!(
            field.fields.is_empty(),
            "{}: {:?}",
            field.name,
            field.fields
        );
    }
}

/// `gts::GtsTypeId` deserializes from its string form, so its control is a
/// string box -- `Complex` hid the event broker's `default_storage_backend`.
#[test]
fn a_gts_type_id_is_a_string() {
    let files = [file(
        r"
        #[derive(Deserialize)]
        pub struct C { pub default_storage_backend: Option<gts::GtsTypeId> }
        ",
    )];
    assert_eq!(
        named(&fields(&files, "C"), "default_storage_backend").ty,
        ConfigFieldType::Str
    );
}

/// The case that asked for the nested level, against the real tree.
#[test]
fn the_oidc_plugin_s_jwt_requires_claim_mapping() {
    let files = require!(tree(
        "gears/system/authn-resolver/plugins/oidc-authn-plugin"
    ));
    let projected = fields(&files, "OidcAuthNGearConfig");
    let claim_mapping = named(&named(&projected, "jwt").fields, "claim_mapping");
    assert!(claim_mapping.required && claim_mapping.default.is_none());
}
