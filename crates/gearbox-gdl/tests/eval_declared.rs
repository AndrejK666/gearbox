//! Evaluating a `gear.gdl` into the facts it declares.
//!
//! This crate produces only the *declared* half. The gear's identity,
//! capabilities, co-location dependencies, lifecycle and client trait are
//! projected from Rust by `gearbox-project`, and the two halves are merged in
//! `gearbox-engine` -- so there is deliberately no `GearDescriptor` here to
//! assert against.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use gearbox_gdl::{FileIdentity, GdlEngine, GearDecl};
use gearbox_ir::{DiagnosticCode, RelPath, SourceId};

fn identity() -> FileIdentity {
    FileIdentity {
        uri: "file:///repo/gears/demo/gear.gdl".to_owned(),
        source: SourceId::new("gears-rust").unwrap(),
        gdl_path: RelPath::new("gears/demo/gear.gdl").unwrap(),
        load_paths: None,
    }
}

fn eval(src: &str) -> (Option<GearDecl>, Vec<DiagnosticCode>) {
    let out = GdlEngine::new().eval_gear(&identity(), src);
    let codes = out.diagnostics.iter().map(|d| d.code).collect();
    (out.value, codes)
}

/// The real api-contracts description, post-ADR: no id, no caps, no deps.
const API_CONTRACTS: &str = r#"
PAYMENT_SDK = cargo(
    crate_name = "cf-api-contracts-sdk",
    lib = "cf_api_contracts_sdk",
    path = "../api-contracts-sdk",
    features = ["rest-client"],
)

gear(
    maturity = "stable",
    name = "Payments (example provider)",
    category = "example",
    visibility = "public",
    package = cargo(crate_name = "cf-api-contracts", lib = "cf_api_contracts", path = "."),
    provides = [
        provide(
            contract = "PaymentApi",
            rust = "api_contracts_sdk::PaymentApi",
            sdk = PAYMENT_SDK,
            local = "Self::build_local",
            rest = rest(base_path = "/api-contracts/v1"),
        ),
    ],
    serves = [endpoint(name = "rest", via = "rest_host")],
)
"#;

#[test]
fn evaluates_the_declared_half() {
    let (value, codes) = eval(API_CONTRACTS);
    assert!(codes.is_empty(), "unexpected diagnostics: {codes:?}");
    let decl = value.expect("a declaration");

    assert_eq!(decl.name.as_deref(), Some("Payments (example provider)"));
    assert_eq!(decl.category.as_deref(), Some("example"));
    assert_eq!(decl.visibility.as_deref(), Some("public"));

    // The lib ident is taken as declared, never derived: cf-api-contracts has
    // no [lib] section, so deriving would produce `api_contracts` and emit a
    // link line that does not compile.
    let package = decl.package.expect("package");
    assert_eq!(package.lib_ident, "cf_api_contracts");
    assert_ne!(package.lib_ident, "api_contracts");
    assert!(package.attr.is_none(), "omitted attr means scan src/");

    // `contract` is a join key against #[toolkit::contract], not a declaration
    // of identity -- so there is no version or kind to assert here.
    assert_eq!(decl.provides.len(), 1);
    assert_eq!(decl.provides[0].contract, "PaymentApi");
    assert_eq!(decl.provides[0].rust, "api_contracts_sdk::PaymentApi");
    // No transports to assert either: which ones exist follows from the
    // `<Base>Rest`/`<Base>Grpc` traits in the sdk crate, and is projected.
    assert_eq!(
        decl.provides[0].rest.as_ref().unwrap().base_path,
        "/api-contracts/v1"
    );
    // The sdk reference is what lets the projector find the contract attribute.
    assert_eq!(decl.provides[0].sdk.lib_ident, "cf_api_contracts_sdk");

    assert_eq!(decl.serves.len(), 1);
    assert_eq!(decl.serves[0].via.as_deref(), Some("rest_host"));
}

#[test]
fn a_top_level_assignment_shares_an_sdk() {
    // Binding a name is legal; it is control flow that is forbidden.
    let (value, codes) = eval(API_CONTRACTS);
    assert!(codes.is_empty(), "{codes:?}");
    assert_eq!(
        value.unwrap().provides[0].sdk.features,
        ["rest-client"],
        "the shared SDK record carried its features through the binding"
    );
}

#[test]
fn consume_declares_the_product_level_facts_only() {
    let src = r#"
SDK = cargo(crate_name = "s", lib = "s")
gear(
    maturity = "stable",
    package = cargo(crate_name = "c", lib = "c"),
    consumes = [
        consume(contract = "PaymentApi", rust = "s::PaymentApi", sdk = SDK,
                critical = True),
    ],
)
"#;
    let (value, codes) = eval(src);
    assert!(codes.is_empty(), "{codes:?}");
    let decl = value.unwrap();
    assert_eq!(decl.consumes.len(), 1);
    // Whether it gates readiness is a product judgement and stays here. Which
    // gear supplies it is not: `#[toolkit::consumes(from = ...)]` owns that.
    assert!(decl.consumes[0].critical);
}

#[test]
fn from_is_refused_because_the_attribute_owns_the_directory_key() {
    // The runtime reads the attribute's spelling as a directory key, so a
    // second copy here is a second place for it to be wrong. Refused rather
    // than cross-checked, which is what ADR
    // `cpt-gearbox-adr-macro-projected-catalogue` asks for everywhere else --
    // and refused under GBX0210, the code that names the owning attribute
    // instead of saying "unknown argument".
    let src = r#"
SDK = cargo(crate_name = "s", lib = "s")
gear(
    maturity = "stable",
    package = cargo(crate_name = "c", lib = "c"),
    consumes = [
        consume(contract = "PaymentApi", rust = "s::PaymentApi", sdk = SDK,
                from_ = "api-contracts"),
    ],
)
"#;
    let (value, codes) = eval(src);
    assert!(value.is_none());
    assert_eq!(codes, [DiagnosticCode::ValidateRestatement]);
}

#[test]
fn cluster_capabilities_resolve_against_their_primitive() {
    let src = r#"
gear(
    maturity = "stable",
    package = cargo(crate_name = "c", lib = "c"),
    requires = [
        cluster.cache(profile = "default", capabilities = [cluster_cap.linearizable]),
        cluster.leader_election(profile = "default"),
    ],
)
"#;
    let (value, codes) = eval(src);
    assert!(codes.is_empty(), "{codes:?}");
    let decl = value.unwrap();
    assert_eq!(decl.requires.len(), 2);
    assert_eq!(
        decl.requires[0].capabilities,
        ["cluster.cache.linearizable"],
        "the bare `linearizable` member resolved against the cache primitive"
    );
    assert!(decl.requires[1].capabilities.is_empty());
}

#[test]
fn prefix_watch_is_refused_on_a_lock_because_it_is_a_cache_property() {
    let src = r#"
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"),
     requires = [cluster.lock(capabilities = [cluster_cap.prefix_watch])])
"#;
    let (value, codes) = eval(src);
    assert!(value.is_none());
    assert_eq!(codes, [DiagnosticCode::GdlEval]);
}

#[test]
fn a_value_from_the_wrong_namespace_is_refused() {
    // The namespace tag is what catches this: a bare string could not.
    let src = r#"
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"),
     requires = [cluster.cache(capabilities = [transport.rest])])
"#;
    let (value, codes) = eval(src);
    assert!(value.is_none());
    assert_eq!(codes, [DiagnosticCode::GdlEval]);
}

#[test]
fn a_role_records_its_name_its_directory_name_and_its_label_keys() {
    let src = r#"
gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"),
     roles = [
         role(name = "dispatcher", directory_name = "event-broker"),
         role(name = "ingest", labels = ["shard"]),
     ])
"#;
    let (value, codes) = eval(src);
    let decl = value.expect("a declaration");
    assert_eq!(decl.declared_roles.len(), 2);
    assert_eq!(
        decl.declared_roles[0].directory_name.as_deref(),
        Some("event-broker")
    );
    // Left out here: the default needs the gear's id, which is projected from
    // Rust and invisible to this layer, so lowering fills it in.
    assert_eq!(decl.declared_roles[1].directory_name, None);
    assert_eq!(decl.declared_roles[1].labels, ["shard"]);
    // Nothing is refused at evaluation; the gap and front-door diagnostics are
    // asserted in `gearbox-engine`, which is where they are raised.
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn a_missing_package_is_an_error_because_it_locates_the_crate_to_scan() {
    let (value, codes) = eval(r#"gear(maturity = "stable", name = "Demo")"#);
    assert!(value.is_none());
    // The macro rejects the call outright for a missing required argument.
    assert!(!codes.is_empty(), "a missing package must be reported");
}

#[test]
fn zero_and_two_declarations_are_both_cardinality_errors() {
    let (value, codes) = eval("X = 1\n");
    assert!(value.is_none());
    assert_eq!(codes, [DiagnosticCode::GdlCardinality]);

    let two = r#"
gear(maturity = "stable", package = cargo(crate_name = "a", lib = "a"))
gear(maturity = "stable", package = cargo(crate_name = "b", lib = "b"))
"#;
    let (_, codes) = eval(two);
    assert!(codes.contains(&DiagnosticCode::GdlCardinality), "{codes:?}");
}

#[test]
fn a_forbidden_construct_reports_every_occurrence() {
    let src = r#"gear(maturity = "stable", package = cargo(crate_name = "c" if True else "d", lib = "c"))"#;
    let (value, codes) = eval(src);
    assert!(value.is_none());
    assert!(
        codes
            .iter()
            .all(|c| *c == DiagnosticCode::GdlForbiddenConstruct),
        "{codes:?}"
    );
    assert_eq!(codes.len(), 2, "both `if` and `else`, not just the first");
}

#[test]
fn a_syntax_error_carries_a_span() {
    let out = GdlEngine::new().eval_gear(&identity(), "gear(maturity = \"stable\", package = \n");
    assert!(out.value.is_none());
    let diags = out.diagnostics.as_slice();
    assert_eq!(diags[0].code, DiagnosticCode::GdlParse);
    assert!(
        diags[0].location.is_some(),
        "a parse error must carry a span"
    );
}

#[test]
fn every_diagnostic_satisfies_the_prd_invariants() {
    for src in [
        API_CONTRACTS,
        "X = 1\n",
        r#"gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"), id = "x")"#,
        r#"gear(maturity = "stable", package = cargo(crate_name = "c", lib = "c"), nope = 1)"#,
    ] {
        let out = GdlEngine::new().eval_gear(&identity(), src);
        for d in &out.diagnostics {
            assert!(d.validate().is_ok(), "{:?} -> {:?}", d.code, d.validate());
        }
    }
}

#[test]
fn a_plugin_implements_its_hosts_point() {
    let (decl, codes) = eval(
        r#"gear(maturity = "stable", package = cargo(crate_name = "p", lib = "p"), implements = "cf.core.authn_resolver.plugin.v1~")"#,
    );
    assert!(codes.is_empty(), "{codes:?}");
    assert_eq!(
        decl.expect("evaluates").implements.as_deref(),
        Some("cf.core.authn_resolver.plugin.v1~")
    );
}

#[test]
fn the_old_fills_keyword_names_what_it_became() {
    // Renamed without an alias, so a description written before the rename
    // fails -- and the message is the whole migration.
    let out = GdlEngine::new().eval_gear(
        &identity(),
        r#"gear(maturity = "stable", package = cargo(crate_name = "p", lib = "p"), fills = "cf.core.authn_resolver.plugin.v1~")"#,
    );
    assert!(out.value.is_none());
    let message = &out.diagnostics.as_slice()[0].message;
    assert!(
        message.contains("`fills` was renamed to `implements`"),
        "got: {message}"
    );
}

/// The message of the first diagnostic, for the refusal tests below.
fn refusal(src: &str) -> String {
    let out = GdlEngine::new().eval_gear(&identity(), src);
    assert!(out.value.is_none(), "`{src}` should not evaluate");
    out.diagnostics.as_slice()[0].message.clone()
}

#[test]
fn a_design_gear_names_its_id_and_has_no_package() {
    let (decl, codes) = eval(
        r#"gear(maturity = "design", id = "approval-service", name = "Approval Service",
                sdk = cargo(crate_name = "approval-sdk", lib = "approval_sdk", path = "sdk"))"#,
    );
    assert!(codes.is_empty(), "{codes:?}");
    let decl = decl.expect("evaluates");
    assert_eq!(decl.maturity, Some(gearbox_gdl::Maturity::Design));
    assert_eq!(decl.id.as_deref(), Some("approval-service"));
    assert!(decl.package.is_none());
}

#[test]
fn a_design_gear_without_an_id_is_refused() {
    let message = refusal(r#"gear(maturity = "design", name = "X")"#);
    assert!(message.contains("names its own id"), "got: {message}");
}

#[test]
fn a_design_gear_id_must_be_a_gear_id() {
    let message = refusal(r#"gear(maturity = "design", id = "Not Kebab")"#);
    assert!(message.contains("not a valid gear id"), "got: {message}");
}

#[test]
fn a_design_gear_refuses_what_describes_code() {
    for field in [
        r#"package = cargo(crate_name = "p", lib = "p")"#,
        r#"implements = "cf.core.authn_resolver.plugin.v1~""#,
        "runtime_caps = [cap.rest]",
        r#"visibility = "public""#,
    ] {
        let message = refusal(&format!(r#"gear(maturity = "design", id = "x", {field})"#));
        assert!(
            message.contains("a design gear has none yet"),
            "`{field}`: got {message}"
        );
    }
}

#[test]
fn an_unknown_maturity_is_refused() {
    let message =
        refusal(r#"gear(maturity = "planned", package = cargo(crate_name = "p", lib = "p"))"#);
    assert!(
        message.contains("unknown maturity `planned`"),
        "got: {message}"
    );
}

#[test]
fn a_stable_gear_still_may_not_restate_its_id() {
    let (_, codes) = eval(
        r#"gear(maturity = "stable", id = "x", package = cargo(crate_name = "p", lib = "p"))"#,
    );
    assert_eq!(codes, [DiagnosticCode::ValidateRestatement]);
}

#[test]
fn maturity_is_required() {
    let message = refusal(r#"gear(package = cargo(crate_name = "p", lib = "p"))"#);
    assert!(
        message.contains("a gear declares its maturity"),
        "got: {message}"
    );
}

#[test]
fn every_level_of_a_gear_with_code_is_accepted() {
    for (spelling, level) in [
        ("experimental", gearbox_ir::Maturity::Experimental),
        ("preview", gearbox_ir::Maturity::Preview),
        ("stable", gearbox_ir::Maturity::Stable),
        ("deprecated", gearbox_ir::Maturity::Deprecated),
    ] {
        let (decl, codes) = eval(&format!(
            r#"gear(maturity = "{spelling}", package = cargo(crate_name = "p", lib = "p"))"#
        ));
        assert!(codes.is_empty(), "{spelling}: {codes:?}");
        assert_eq!(
            decl.expect("evaluates").maturity,
            Some(gearbox_gdl::Maturity::Code(level))
        );
    }
}

#[test]
fn an_extension_point_selector_is_a_dotted_field_path() {
    let point = |selector: &str| {
        format!(
            r#"gear(maturity = "stable", package = cargo(crate_name = "p", lib = "p"), sdk = cargo(crate_name = "s", lib = "s"),
                extension_points = [extension_point("cf.core.idp.plugin.v1~", trait = "T", selector = "{selector}")])"#
        )
    };
    let (decl, codes) = eval(&point("idp.vendor"));
    assert!(codes.is_empty(), "{codes:?}");
    assert_eq!(
        decl.expect("evaluates").extension_points[0]
            .selector
            .as_deref(),
        Some("idp.vendor")
    );
    for bad in ["", "idp..vendor", "Idp.vendor", "idp.vendor!"] {
        let message = refusal(&point(bad));
        assert!(
            message.contains("dotted path of config field names"),
            "`{bad}`: {message}"
        );
    }
}
