//! Tests for the in-place editor, and the first one is the point of the module.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use starlark::syntax::AstModule;

use gearbox_ir::DiagnosticCode;

use super::*;

/// The identity the round-trip test evaluates a product under.
fn product_identity() -> crate::FileIdentity {
    crate::FileIdentity {
        uri: URI.to_owned(),
        source: gearbox_ir::SourceId::new("product").unwrap(),
        gdl_path: gearbox_ir::RelPath::new("product.gdl").unwrap(),
        load_paths: None,
    }
}

/// A string config value, which is what every case below writes.
fn str_value(s: &str) -> gearbox_ir::ConfigValue {
    gearbox_ir::ConfigValue::Str(s.to_owned())
}

const URI: &str = "file:///product.gdl";

/// A product shaped like the real one: comments carrying the reasoning, one
/// entry per line, and a comment *inside* the list.
const COMMENTED: &str = r#"# The Payments Demo product.
#
# All three deployment profiles are declared as DATA.

product(
    id = "payments-demo",
    name = "Payments Demo",

    # Note what is NOT listed: grpc-hub and types-registry arrive through the
    # colocated_deps closure, which is a link-time fact.
    gears = [
        use_gear("api-gateway", source = "gears-rust"),
        use_gear("api-contracts", source = "gears-rust"),
    ],

    preferences = [prefer.fewer_applications()],
)
"#;

#[test]
fn every_comment_survives_an_insertion() {
    // The reason this module exists. Evaluating and re-printing the description
    // would produce something equivalent to the machine and useless to the next
    // reader, because the comments are where the decisions are written down.
    let edited = add_gear(URI, COMMENTED, "cluster", "gears-rust")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();

    for comment in [
        "# The Payments Demo product.",
        "# All three deployment profiles are declared as DATA.",
        "# Note what is NOT listed: grpc-hub and types-registry arrive through the",
        "# colocated_deps closure, which is a link-time fact.",
    ] {
        assert!(edited.contains(comment), "lost `{comment}`\n{edited}");
    }
}

#[test]
fn only_one_line_changes() {
    // Stated as a line diff rather than as "it contains the new entry": an edit
    // that also reflowed the file would pass the weaker assertion.
    let edited = add_gear(URI, COMMENTED, "cluster", "gears-rust")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();

    let before: Vec<&str> = COMMENTED.lines().collect();
    let after: Vec<&str> = edited.lines().collect();
    assert_eq!(
        after.len(),
        before.len() + 1,
        "expected exactly one new line"
    );

    let added: Vec<&&str> = after.iter().filter(|line| !before.contains(line)).collect();
    assert_eq!(
        added,
        vec![&"        use_gear(\"cluster\", source = \"gears-rust\"),"],
        "the only new line should be the entry, indented like its neighbours"
    );
}

#[test]
fn the_indentation_comes_from_the_neighbours() {
    // Two spaces, not the four the demo uses: read off the file rather than
    // assumed, so a differently formatted product keeps its own shape.
    let source = "product(\n  gears = [\n    use_gear(\"a\", source = \"s\"),\n  ],\n)\n";
    let edited = add_gear(URI, source, "b", "s")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(
        edited.contains("\n    use_gear(\"b\", source = \"s\"),\n"),
        "expected four-space indent to match the existing entry:\n{edited}"
    );
}

#[test]
fn a_one_line_list_stays_on_one_line() {
    let source = "product(gears = [use_gear(\"a\", source = \"s\")])\n";
    let edited = add_gear(URI, source, "b", "s")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert_eq!(
        edited,
        "product(gears = [use_gear(\"a\", source = \"s\"), use_gear(\"b\", source = \"s\")])\n"
    );
}

#[test]
fn adding_a_gear_that_is_already_there_changes_nothing() {
    // ADR-0010's "idempotent by content", and it costs nothing: the same rule the
    // workspace `members` entry follows.
    assert_eq!(
        add_gear(URI, COMMENTED, "api-gateway", "gears-rust").expect("editable"),
        Edit::Unchanged
    );
}

#[test]
fn an_empty_list_is_filled_rather_than_refused() {
    let source = "product(\n    gears = [\n    ],\n)\n";
    let edited = add_gear(URI, source, "a", "s")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(
        edited.contains("use_gear(\"a\", source = \"s\"),"),
        "{edited}"
    );
}

#[test]
fn removing_takes_the_comma_and_the_line_with_it() {
    let edited = remove_gear(URI, COMMENTED, "api-contracts")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(!edited.contains("api-contracts"), "{edited}");
    assert!(
        !edited.contains(",\n\n    ],"),
        "an orphaned comma or blank line was left behind:\n{edited}"
    );
    assert_eq!(edited.lines().count(), COMMENTED.lines().count() - 1);
    // And the comment above the list is not collateral damage.
    assert!(edited.contains("# Note what is NOT listed"), "{edited}");
}

#[test]
fn removing_a_gear_takes_every_matching_entry() {
    let source = r#"product(
    gears = [
        use_gear("twice", source = "a"),
        use_gear("keep", source = "a"),
        use_gear("twice", source = "b"),
    ],
)
"#;
    let edited = remove_gear(URI, source, "twice")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(!edited.contains("twice"), "{edited}");
    assert!(edited.contains("keep"), "{edited}");
}

#[test]
fn a_shape_refusal_is_not_a_parse_error() {
    let diagnostics = add_gear(URI, "gear(name = \"g\")\n", "a", "s").expect_err("not a product");
    assert!(
        diagnostics
            .as_slice()
            .iter()
            .any(|d| d.code == DiagnosticCode::GdlCardinality && d.location.is_some()),
        "{diagnostics:?}"
    );
}

#[test]
fn removing_a_gear_that_is_not_there_changes_nothing() {
    assert_eq!(
        remove_gear(URI, COMMENTED, "not-in-the-product").expect("editable"),
        Edit::Unchanged
    );
}

#[test]
fn a_gears_list_that_is_not_a_literal_is_refused_rather_than_guessed_at() {
    // The honest failure. A list produced by a helper has no span to insert into,
    // and an approximation here would be a silently wrong description.
    let source = "load(\"//lib.gdl\", \"chosen\")\nproduct(gears = chosen())\n";
    let diagnostics = add_gear(URI, source, "a", "s").expect_err("not editable");
    assert!(
        diagnostics
            .as_slice()
            .iter()
            .any(|d| d.message.contains("not a list literal")),
        "{diagnostics:?}"
    );
}

#[test]
fn a_product_without_gears_is_refused_by_name() {
    let source = "product(id = \"p\")\n";
    let diagnostics = add_gear(URI, source, "a", "s").expect_err("not editable");
    assert!(
        diagnostics
            .as_slice()
            .iter()
            .any(|d| d.message.contains("no `gears` argument")),
        "{diagnostics:?}"
    );
}

#[test]
fn a_file_that_is_not_a_product_is_refused_by_name() {
    let source = "gear(name = \"g\")\n";
    let diagnostics = add_gear(URI, source, "a", "s").expect_err("not editable");
    assert!(
        diagnostics
            .as_slice()
            .iter()
            .any(|d| d.message.contains("no top-level `product(...)` call")),
        "{diagnostics:?}"
    );
}

#[test]
fn remove_gear_honours_an_aliased_use_gear() {
    let source = r#"
UG = use_gear
product(
    gears = [
        UG("g", source = "s"),
        use_gear("keep", source = "s"),
    ],
)
"#;
    let edited = remove_gear(URI, source, "g")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(!edited.contains("UG(\"g\""), "{edited}");
    assert!(edited.contains(r#"use_gear("keep""#), "{edited}");
}

#[test]
fn the_result_still_parses_and_still_says_what_it_said() {
    // The edit is text surgery, so the only real proof it produced a description
    // is to parse the result -- and the round trip has to agree about the gears.
    let edited = add_gear(URI, COMMENTED, "cluster", "gears-rust")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    let reparsed = gears_list(URI, &edited).expect("the edited file is still editable");
    assert_eq!(reparsed.entries.len(), 3);
    for gear in ["api-gateway", "api-contracts", "cluster"] {
        assert!(
            reparsed
                .entries
                .iter()
                .any(|entry| names_gear(&edited, *entry, gear)),
            "lost `{gear}` after the round trip"
        );
    }
}

/// The repository's own product description.
///
/// Always present, unlike the `gears-rust` corpus: it lives in this repository,
/// two levels above the crate.
fn real_product() -> Option<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../products/payments-demo/product.gdl");
    std::fs::read_to_string(path).ok()
}

#[test]
fn the_real_product_takes_one_line_and_keeps_every_comment() {
    // The acid test. `products/payments-demo/product.gdl` is roughly a hundred
    // lines of which most are comments, and those comments are where the
    // reasoning lives -- why `vendor` is left alone, why `grpc-hub` is not
    // listed, why the profiles are data. An editor that cannot survive this file
    // is not usable on any real one.
    let Some(source) = real_product() else {
        eprintln!("skipping: products/payments-demo/product.gdl not present");
        return;
    };

    let comments_before = source
        .lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .count();
    // A guard on the test rather than on the code: comment preservation is only
    // worth asserting against a file that has comments to lose. Stated as a share
    // of the file so it describes itself instead of holding a number somebody
    // has to re-derive -- today that is 29 lines in 105.
    let total = source.lines().count();
    assert!(
        comments_before * 4 >= total,
        "expected a heavily commented file; {comments_before} of {total} lines are comments"
    );

    // `tenant-resolver` rather than `cluster`: the product selects `cluster`
    // explicitly now, because its provider registry is what every cluster
    // requirement resolves against, and a gear the file already names cannot
    // demonstrate an insertion.
    let edited = add_gear(URI, &source, "tenant-resolver", "gears-rust")
        .expect("the real product is editable")
        .changed()
        .expect("adding a gear it does not have should change it")
        .to_owned();

    let comments_after = edited
        .lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .count();
    assert_eq!(
        comments_after, comments_before,
        "a comment was lost or moved"
    );
    assert_eq!(
        edited.lines().count(),
        source.lines().count() + 1,
        "exactly one line should have appeared"
    );

    // And it is idempotent on the file it just produced.
    assert_eq!(
        add_gear(URI, &edited, "tenant-resolver", "gears-rust").expect("editable"),
        Edit::Unchanged
    );

    // Removing it again returns the file to what it was, byte for byte. That is
    // the strongest statement available about surgery: the inverse edit is exact.
    assert_eq!(
        remove_gear(URI, &edited, "tenant-resolver")
            .expect("editable")
            .changed()
            .expect("changed"),
        source,
        "remove did not undo add exactly"
    );
}

const WITH_CONFIG: &str = r#"# Keep this comment.
product(
    gears = [
        # gear rationale
        use_gear("api-gateway", source = "gears-rust", config = {"demo_mode": "off"}),
    ],
    profiles = [
        embedded(id = "dev"),
    ],
)
"#;

/// The name check protects against a credential typed into a form, and only a
/// string can be one. Before typing, a `bool` named `mtls_key` was unwritable
/// for a reason that never applied to it.
#[test]
fn the_secret_rule_refuses_strings_and_leaves_other_types_alone() {
    use gearbox_ir::ConfigValue;

    let refused = set_gear_config(
        URI,
        WITH_CONFIG,
        "api-gateway",
        "api_key",
        Some(&str_value("sk-live-1")),
    )
    .expect_err("a string under a secret-looking key is refused");
    assert!(
        refused
            .as_slice()
            .iter()
            .any(|d| d.message.contains("api_key")),
        "{refused:?}"
    );

    // Same key name, a bool: nothing to protect, so nothing is refused.
    set_gear_config(
        URI,
        WITH_CONFIG,
        "api-gateway",
        "mtls_key",
        Some(&ConfigValue::Bool(true)),
    )
    .expect("a bool cannot carry a credential");

    // Removal was always allowed and stays allowed.
    set_gear_config(URI, WITH_CONFIG, "api-gateway", "api_key", None)
        .expect("removing a secret-looking key is not a write of one");
}

/// The typed twin of `config_edit_keeps_comments_and_is_inverse`, and the reason
/// the wire stopped carrying strings: a checkbox that wrote `"True"` would be
/// writing a string that happens to read like a boolean.
#[test]
fn typed_values_render_as_starlark_literals_not_as_strings() {
    use gearbox_ir::ConfigValue;

    for (value, expected) in [
        (ConfigValue::Bool(true), r#""demo_mode": True"#),
        (ConfigValue::Bool(false), r#""demo_mode": False"#),
        (ConfigValue::Int(8087), r#""demo_mode": 8087"#),
        (ConfigValue::Int(-1), r#""demo_mode": -1"#),
        // `{:?}` keeps the `.0`, so a float stays a float on the next read.
        (ConfigValue::Float(1.5), r#""demo_mode": 1.5"#),
        (ConfigValue::Float(8087.0), r#""demo_mode": 8087.0"#),
        (ConfigValue::Str("on".to_owned()), r#""demo_mode": "on""#),
    ] {
        let edited = set_gear_config(URI, WITH_CONFIG, "api-gateway", "demo_mode", Some(&value))
            .expect("editable")
            .changed()
            .expect("changed")
            .to_owned();
        assert!(
            edited.contains(expected),
            "expected `{expected}` in:\n{edited}"
        );

        // Writing the same value again is textually idempotent for every shape,
        // which is what keeps a panel from rewriting a file it did not change.
        assert_eq!(
            set_gear_config(URI, &edited, "api-gateway", "demo_mode", Some(&value))
                .expect("editable"),
            Edit::Unchanged,
            "re-writing `{expected}` should change nothing"
        );
    }
}

/// A typed value must survive the evaluator, not just the editor. Before this,
/// `to_json_at` knew no floats and capped integers at `i32`, so a number control
/// could write a description that parsed and then refused to evaluate.
#[test]
fn typed_values_survive_a_round_trip_through_the_evaluator() {
    use gearbox_ir::ConfigValue;

    // A well-formed product, unlike the surgery fixtures above: this one has to
    // survive the evaluator, not just the editor.
    const VALID: &str = r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "gears-rust", at = path("../gears-rust"))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [use_gear("api-gateway", source = "gears-rust", config = {"demo_mode": "off"})],
)
"#;

    for value in [
        ConfigValue::Bool(true),
        ConfigValue::Int(8087),
        ConfigValue::Int(5_000_000_000),
        ConfigValue::Float(1.5),
        ConfigValue::Str("on".to_owned()),
    ] {
        let edited = set_gear_config(URI, VALID, "api-gateway", "demo_mode", Some(&value))
            .expect("editable")
            .changed()
            .expect("changed")
            .to_owned();

        let out = crate::GdlEngine::new().eval_product(&product_identity(), &edited);
        assert!(
            out.value.is_some(),
            "`{value}` produced a description that does not evaluate: {:?}",
            out.diagnostics
        );
        let intent = out.value.expect("evaluated");
        let selection = intent
            .selected_gears
            .iter()
            .find(|g| g.gear.as_str() == "api-gateway")
            .expect("the gear is selected");
        let written = selection
            .config
            .get("demo_mode")
            .expect("the key was written");
        let expected = match &value {
            ConfigValue::Bool(b) => serde_json::Value::Bool(*b),
            ConfigValue::Int(i) => serde_json::Value::from(*i),
            ConfigValue::Float(f) => serde_json::Value::from(*f),
            ConfigValue::Str(s) => serde_json::Value::String(s.clone()),
        };
        assert_eq!(written, &expected, "for `{value}`");
    }
}

#[test]
fn config_edit_keeps_comments_and_is_inverse() {
    let edited = set_gear_config(
        URI,
        WITH_CONFIG,
        "api-gateway",
        "demo_mode",
        Some(&str_value("on")),
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();
    assert!(edited.contains("# Keep this comment."), "{edited}");
    assert!(edited.contains("# gear rationale"), "{edited}");
    assert!(edited.contains("\"demo_mode\": \"on\""), "{edited}");

    assert_eq!(
        set_gear_config(
            URI,
            &edited,
            "api-gateway",
            "demo_mode",
            Some(&str_value("on"))
        )
        .expect("editable"),
        Edit::Unchanged
    );

    let restored = set_gear_config(
        URI,
        &edited,
        "api-gateway",
        "demo_mode",
        Some(&str_value("off")),
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();
    assert_eq!(restored, WITH_CONFIG);
}

#[test]
fn config_key_inside_a_string_is_not_matched_by_find() {
    // A comment and a string value both contain the substring `config = `; surgery
    // must use the named-argument span, not `text.find`.
    let source = r#"product(
    gears = [
        use_gear("g", source = "s", note = "mentions config = nowhere", config = {"a": "1"}),
    ],
)
"#;
    let edited = set_gear_config(URI, source, "g", "a", Some(&str_value("2")))
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(
        edited.contains("note = \"mentions config = nowhere\""),
        "{edited}"
    );
    assert!(edited.contains("\"a\": \"2\""), "{edited}");
}

#[test]
fn quoted_values_survive_escaping() {
    let source = r#"product(
    gears = [
        use_gear("g", source = "s"),
    ],
)
"#;
    let edited = set_gear_config(URI, source, "g", "msg", Some(&str_value(r#"He said "hi""#)))
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(edited.contains(r#""msg": "He said \"hi\"""#), "{edited}");
    gears_list(URI, &edited).expect("escaped config must still parse");
}

#[test]
fn render_template_escapes_and_parses() {
    let text = render_product_template(&CreateProductParams {
        id: "x".into(),
        name: r#"He said "hi""#.into(),
        version: "0.1.0".into(),
        sources: vec![("gears-rust".into(), "gears".into())],
        profile_kind: "embedded".into(),
        profile_id: "dev".into(),
    });
    assert!(text.contains(r#"name = "He said \"hi\"""#), "{text}");
    AstModule::parse(URI, text, &crate::declarative::dialect()).expect("template must parse");
}

#[test]
fn clone_keeps_comments_byte_exact_elsewhere() {
    let Some(source) = real_product() else {
        eprintln!("skipping: products/payments-demo/product.gdl not present");
        return;
    };
    let comments_before = source
        .lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .count();
    let cloned =
        clone_product_text(URI, &source, "clone-id", "Clone Name", None).expect("cloneable");
    let comments_after = cloned
        .lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .count();
    assert_eq!(comments_after, comments_before);
    assert!(cloned.contains(r#"id = "clone-id""#), "{cloned}");
    assert!(cloned.contains(r#"name = "Clone Name""#), "{cloned}");
}

#[test]
fn clone_stamps_version_when_provided() {
    let source = r#"# keep me
product(
    id = "src",
    name = "Src",
    version = "0.1.0",
    sources = [source(id = "s", at = path("."))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [],
)
"#;
    let cloned = clone_product_text(URI, source, "clone-id", "Clone Name", Some("2.0.0"))
        .expect("cloneable");
    assert!(cloned.contains(r#"id = "clone-id""#), "{cloned}");
    assert!(cloned.contains(r#"name = "Clone Name""#), "{cloned}");
    assert!(cloned.contains(r#"version = "2.0.0""#), "{cloned}");
    assert!(!cloned.contains(r#"version = "0.1.0""#), "{cloned}");
    assert!(cloned.contains("# keep me"), "{cloned}");
    assert!(
        cloned.contains(r#"source(id = "s", at = path("."))"#),
        "{cloned}"
    );
}

/// Every relative path a product states, in each of the places one is written:
/// a `source`'s `path(...)`, one moved into a variable, `templates`, and a
/// `self_hosted` target directory -- beside an absolute path and a comment that
/// must both come through untouched.
const WITH_PATHS: &str = r#"# sources are relative to this file
SHARED = path("../shared")

product(
    id = "src",
    version = "0.1.0",
    sources = [
        source(id = "gears-rust", at = path("../../../gears-rust")),  # the corpus
        source(id = "shared", at = SHARED),
        source(id = "abs", at = path("/opt/gears")),
    ],
    templates = path("templates"),
    profiles = [
        embedded(id = "dev"),
        self_hosted(id = "local", host = "gateway", worker_discovery = "directory",
                    target_dir = "../../../gears-rust/target"),
    ],
    default_profile = "dev",
    gears = [],
)
"#;

#[test]
fn rebase_rewrites_every_relative_path_and_nothing_else() {
    let seen = std::cell::RefCell::new(Vec::new());
    let (rebased, found) = rebase_product_paths(URI, WITH_PATHS, |written| {
        seen.borrow_mut().push(written.to_owned());
        (!written.starts_with('/')).then(|| format!("../{written}"))
    })
    .expect("parses");

    assert_eq!(
        seen.into_inner().len(),
        5,
        "four relative paths and the absolute one are all offered to the callback"
    );
    assert!(
        rebased.contains(r#"at = path("../../../../gears-rust")),  # the corpus"#),
        "{rebased}"
    );
    assert!(
        rebased.contains(r#"SHARED = path("../../shared")"#),
        "{rebased}"
    );
    assert!(
        rebased.contains(r#"templates = path("../templates")"#),
        "{rebased}"
    );
    assert!(
        rebased.contains(r#"target_dir = "../../../../gears-rust/target""#),
        "{rebased}"
    );
    assert!(
        rebased.contains(r#"path("/opt/gears")"#),
        "an absolute path is left alone"
    );
    assert!(
        rebased.contains("# sources are relative to this file"),
        "{rebased}"
    );
    assert!(found.relative_loads.is_empty());
    AstModule::parse(URI, rebased, &crate::declarative::dialect()).expect("still parses");
}

#[test]
fn rebase_that_changes_nothing_is_byte_exact() {
    let (rebased, _) = rebase_product_paths(URI, WITH_PATHS, |_| None).expect("parses");
    assert_eq!(rebased, WITH_PATHS);
}

#[test]
fn rebase_reports_relative_loads_and_not_rooted_ones() {
    let source =
        format!("load(\"common.gdl\", \"SDK\")\nload(\"//shared/x.gdl\", \"X\")\n{WITH_PATHS}");
    let (_, found) = rebase_product_paths(URI, &source, |_| None).expect("parses");
    assert_eq!(found.relative_loads, vec!["common.gdl".to_owned()]);
}

#[test]
fn profile_add_remove_is_byte_exact_inverse() {
    let added = add_profile(
        URI,
        WITH_CONFIG,
        "kubernetes",
        "prod",
        &[
            ("discovery".into(), "static".into()),
            ("namespace".into(), "pay".into()),
        ],
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();
    assert!(
        added.contains("kubernetes(id = \"prod\", discovery = \"static\", namespace = \"pay\")"),
        "{added}"
    );
    assert_eq!(
        add_profile(URI, &added, "kubernetes", "prod", &[]).expect("editable"),
        Edit::Unchanged
    );
    assert_eq!(
        remove_profile(URI, &added, "prod")
            .expect("editable")
            .changed()
            .expect("changed"),
        WITH_CONFIG
    );
}

#[test]
fn computed_profiles_list_is_refused() {
    let source = "load(\"//lib.gdl\", \"chosen\")\nproduct(profiles = chosen())\n";
    let diagnostics = add_profile(URI, source, "embedded", "dev", &[]).expect_err("not editable");
    assert!(
        diagnostics
            .as_slice()
            .iter()
            .any(|d| d.message.contains("not a list literal")),
        "{diagnostics:?}"
    );
}

#[test]
fn quote_string_escapes_control_chars() {
    assert_eq!(quote_string("a\"b\\c"), r#""a\"b\\c""#);
    assert_eq!(quote_string("a\nb"), "\"a\\nb\"");
}

#[test]
fn add_gear_quotes_injection_payloads() {
    let payloads = [r#"x"), use_gear("y"#, "a\"b", "a\nb"];
    for gear in payloads {
        let edited = add_gear(URI, COMMENTED, gear, "gears-rust")
            .expect("editable")
            .changed()
            .expect("changed")
            .to_owned();
        assert!(
            edited.contains(&quote_string(gear)),
            "payload `{gear:?}` was not quoted:\n{edited}"
        );
        gears_list(URI, &edited).expect("quoted add_gear must still parse");
        assert!(
            !edited.contains(r#"use_gear("y""#),
            "payload `{gear:?}` injected an extra call:\n{edited}"
        );
    }
}

/// Adding with no fields scaffolds the ones the kind cannot do without.
///
/// **This replaces a test that asserted the opposite.** `add_profile` used to
/// refuse a `kubernetes` or `self_hosted` profile that arrived without its
/// required fields, and the only caller -- Studio's Add profile form, which asks
/// for an id and a kind -- always arrived that way. So two of the three kinds
/// could not be added through the UI at all, while the create wizard produced
/// both of them from `render_profile_entry`, which already knew the values. Both
/// now read `required_profile_fields`.
#[test]
fn add_profile_scaffolds_the_fields_its_kind_requires() {
    let entry = |kind: &str, id: &str, fields: &[(String, String)]| {
        add_profile(URI, WITH_CONFIG, kind, id, fields)
            .expect("editable")
            .changed()
            .expect("changed")
            .to_owned()
    };

    let kubernetes = entry("kubernetes", "prod", &[]);
    assert!(
        kubernetes.contains(r#"kubernetes(id = "prod", discovery = "static")"#),
        "{kubernetes}"
    );

    let self_hosted = entry("self_hosted", "local", &[]);
    assert!(
        self_hosted.contains(
            r#"self_hosted(id = "local", host = "localhost", worker_discovery = "static")"#
        ),
        "{self_hosted}"
    );

    // `embedded` requires none and must not acquire any: its constructor takes
    // only an id, so a scaffolded argument would be a call the evaluator refuses.
    let embedded = entry("embedded", "staging", &[]);
    assert!(
        embedded.contains(r#"embedded(id = "staging")"#),
        "{embedded}"
    );
    assert!(!embedded.contains("discovery"), "{embedded}");

    // A value the caller named wins; the rest are still filled in.
    let explicit = entry("self_hosted", "local", &[("host".into(), "gateway".into())]);
    assert!(explicit.contains(r#"host = "gateway""#), "{explicit}");
    assert!(
        explicit.contains(r#"worker_discovery = "static""#),
        "{explicit}"
    );
}

/// Removing a scaffolded profile is the exact inverse of adding it, for every
/// kind -- not just the one the byte-exact test already covered.
#[test]
fn add_then_remove_is_an_inverse_for_every_kind() {
    for kind in ["embedded", "self_hosted", "kubernetes"] {
        let added = add_profile(URI, WITH_CONFIG, kind, "audit", &[])
            .expect("editable")
            .changed()
            .expect("changed")
            .to_owned();
        assert!(added.contains("audit"), "{kind}: {added}");
        assert_eq!(
            remove_profile(URI, &added, "audit")
                .expect("editable")
                .changed()
                .expect("changed"),
            WITH_CONFIG,
            "{kind} did not round-trip"
        );
    }
}

/// A field the kind requires cannot be cleared, because clearing it removes the
/// argument and the evaluator then refuses to read the description at all.
///
/// The form offered exactly this: a select with a "not set" option, and a plain
/// text input for `host` that sent `null` when emptied. The product stopped
/// opening, and the control that did it was on a screen that no longer rendered,
/// so the value could not be put back.
#[test]
fn a_required_profile_field_cannot_be_unset() {
    let with_local = add_profile(
        URI,
        WITH_CONFIG,
        "self_hosted",
        "local",
        &[
            ("host".into(), "gateway".into()),
            ("worker_discovery".into(), "directory".into()),
        ],
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    for field in ["host", "worker_discovery"] {
        let diagnostics = set_profile_field(URI, &with_local, "local", field, None)
            .expect_err("a required field must not be unsettable");
        assert!(
            diagnostics
                .as_slice()
                .iter()
                .any(|d| d.message.contains(&format!("needs `{field}`"))),
            "{field}: {diagnostics:?}"
        );
    }

    // An optional one still clears, which is the behaviour the control is for.
    let cleared = set_profile_field(
        URI,
        &with_local,
        "local",
        "worker_discovery",
        Some("static"),
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();
    assert!(
        cleared.contains(r#"worker_discovery = "static""#),
        "{cleared}"
    );
    let without_target =
        set_profile_field(URI, &with_local, "local", "target_dir", None).expect("editable");
    assert_eq!(
        without_target,
        Edit::Unchanged,
        "absent optional stays absent"
    );
}

#[test]
fn add_profile_refuses_constructor_injection() {
    let diagnostics = add_profile(
        URI,
        WITH_CONFIG,
        r#"kubernetes), use_gear("x""#,
        "prod",
        &[],
    )
    .expect_err("injected constructor");
    assert!(
        diagnostics
            .as_slice()
            .iter()
            .any(|d| d.message.contains("not a deployment profile constructor")),
        "{diagnostics:?}"
    );
}

#[test]
fn add_profile_refuses_field_injection() {
    let diagnostics = add_profile(
        URI,
        WITH_CONFIG,
        "kubernetes",
        "prod",
        &[(r#"ns), use_gear("x""#.into(), "pay".into())],
    )
    .expect_err("injected field name");
    assert!(
        diagnostics
            .as_slice()
            .iter()
            .any(|d| d.message.contains("not a valid profile field identifier")),
        "{diagnostics:?}"
    );
}

#[test]
fn render_template_does_not_interpolate_an_injected_kind() {
    let text = render_product_template(&CreateProductParams {
        id: "x".into(),
        name: "n".into(),
        version: "0.1.0".into(),
        sources: vec![("gears-rust".into(), "gears".into())],
        profile_kind: r#"kubernetes), use_gear("evil""#.into(),
        profile_id: "dev".into(),
    });
    assert!(
        !text.contains("use_gear"),
        "injected constructor reached the template:\n{text}"
    );
    assert!(
        text.contains("embedded(id = \"dev\")"),
        "unknown kinds must fall back to a real constructor:\n{text}"
    );
    AstModule::parse(URI, text, &crate::declarative::dialect()).expect("template must parse");
}

#[test]
fn add_source_refuses_an_id_that_is_not_kebab() {
    let source = r#"product(
    sources = [
        source(id = "gears-rust", at = path("gears")),
    ],
)
"#;
    let err = add_source(URI, source, "Not_Kebab", "local-gears").expect_err("must refuse");
    assert!(
        err.iter()
            .any(|d| d.message.contains("not a valid source id")),
        "{err:?}"
    );
}

#[test]
fn add_source_refuses_an_empty_path() {
    let source = r#"product(
    sources = [
        source(id = "gears-rust", at = path("gears")),
    ],
)
"#;
    for at in ["", "   "] {
        let err = add_source(URI, source, "local-gears", at).expect_err("must refuse");
        assert!(
            err.iter()
                .any(|d| d.message.contains("a source needs a path")),
            "at={at:?}: {err:?}"
        );
    }
}

#[test]
fn add_source_inserts_a_path_entry() {
    let source = r#"product(
    sources = [
        source(id = "gears-rust", at = path("gears")),
    ],
)
"#;
    let edited = add_source(URI, source, "local-gears", "gears/local")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(
        edited.contains(r#"source(id = "local-gears", at = path("gears/local"))"#),
        "{edited}"
    );
    assert_eq!(
        add_source(URI, &edited, "local-gears", "gears/local").expect("editable"),
        Edit::Unchanged
    );
}

/// A list written one entry per line with no comma after the last one.
///
/// Valid Starlark, and the shape the blank template wrote for `sources`. The
/// append used to put a new line after the entry without a comma between them,
/// so Create Gear wrote its scaffold and then failed to declare the source with
/// a parse error about text this editor had produced. Found in a demo rehearsal.
#[test]
fn an_entry_without_a_trailing_comma_gets_one_before_the_next() {
    let source = r#"product(
    sources = [
        source(id = "gears-rust", at = path("../gears-rust"))  # the corpus
    ],
)
"#;
    let edited = add_source(URI, source, "gears", "gears")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    AstModule::parse(URI, edited.clone(), &crate::declarative::dialect()).expect("still parses");
    assert!(
        edited.contains(
            "source(id = \"gears-rust\", at = path(\"../gears-rust\")),  # the corpus\n        source(id = \"gears\", at = path(\"gears\")),\n    ],"
        ),
        "{edited}"
    );
}

/// The first entry of an empty list goes one level in from the line it opens on.
#[test]
fn the_first_entry_of_an_empty_list_is_indented_under_it() {
    let source = "product(\n    gears = [\n    ],\n)\n";
    let edited = add_gear(URI, source, "api-gateway", "gears-rust")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(
        edited.contains("    gears = [\n        use_gear(\"api-gateway\""),
        "{edited}"
    );
}

/// The shape `payments-demo` actually has: a multiline list whose entries carry
/// `profiles` and `config`, with a comment between them and a trailing comma.
///
/// Written out rather than reduced, because every one of those features is a
/// thing an append can destroy, and the corpus is where they occur together.
const HOST_WITH_PLUGINS: &str = r#"product(
    gears = [
        use_gear("api-gateway", source = "gears-rust"),
        # authn-resolver routes to whichever plugin implements its point.
        use_gear("authn-resolver", source = "gears-rust",
            plugins = [
                # `local` takes the static plugin for the same reason `dev` does.
                plugin("static-authn-plugin", profiles = ["dev", "local"],
                       config = {"mode": "accept_all"}),
                plugin("oidc-authn-plugin", profiles = ["prod"],
                       config = {"issuer": "https://id.example.com"}),
            ],
        ),
    ],
)
"#;

#[test]
fn add_gear_plugin_appends_and_leaves_the_others_alone() {
    let edited = add_gear_plugin(
        URI,
        HOST_WITH_PLUGINS,
        "authn-resolver",
        "ldap-authn-plugin",
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(
        edited.contains(r#"plugin("ldap-authn-plugin")"#),
        "the new plugin is not there: {edited}"
    );
    // **The regression this exists for.** `set_gear_plugins` rewrites the list as
    // bare `plugin("id")` entries, so using it here would drop both of these.
    assert!(
        edited.contains(r#"profiles = ["dev", "local"]"#),
        "an existing entry lost its profiles: {edited}"
    );
    assert!(
        edited.contains(r#"config = {"mode": "accept_all"}"#),
        "an existing entry lost its config: {edited}"
    );
    assert!(
        edited.contains(r#"config = {"issuer": "https://id.example.com"}"#),
        "the second entry lost its config: {edited}"
    );
    // Comments are part of what a person wrote, and a re-serialising edit is
    // exactly what loses them.
    assert!(
        edited.contains("# `local` takes the static plugin"),
        "a comment inside the list was lost: {edited}"
    );
    assert!(
        edited.contains("# authn-resolver routes to whichever plugin"),
        "a comment above the entry was lost: {edited}"
    );
    // And nothing else moved: the other gear is untouched.
    assert!(
        edited.contains(r#"use_gear("api-gateway", source = "gears-rust"),"#),
        "another entry was reshaped: {edited}"
    );
}

#[test]
fn add_gear_plugin_is_idempotent() {
    // By the name the entry *names*, not by the rendered text: the existing entry
    // is `plugin("static-authn-plugin", profiles = ..., config = ...)`, which does
    // not match `plugin("static-authn-plugin")` as a string.
    assert_eq!(
        add_gear_plugin(
            URI,
            HOST_WITH_PLUGINS,
            "authn-resolver",
            "static-authn-plugin"
        )
        .expect("editable"),
        Edit::Unchanged
    );
}

#[test]
fn add_gear_plugin_creates_the_argument_when_absent() {
    let source = r#"product(
    gears = [
        use_gear("authn-resolver", source = "gears-rust"),
    ],
)
"#;
    let edited = add_gear_plugin(URI, source, "authn-resolver", "static-authn-plugin")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(
        edited.contains(r#"plugins = [plugin("static-authn-plugin")]"#),
        "{edited}"
    );
}

#[test]
fn add_gear_plugin_refuses_a_plugins_argument_it_cannot_read() {
    // Refused rather than guessed: appending to something that is not a list
    // means deciding what `helper(...)` evaluates to, which is the evaluator's
    // job and not a span surgeon's.
    let source = r#"product(
    gears = [
        use_gear("authn-resolver", source = "gears-rust", plugins = pick_plugins()),
    ],
)
"#;
    let refused = add_gear_plugin(URI, source, "authn-resolver", "static-authn-plugin")
        .expect_err("a non-literal plugins argument must be refused");
    assert!(
        format!("{refused:?}").contains("not a list literal"),
        "{refused:?}"
    );
}

#[test]
fn add_gear_plugin_refuses_a_host_the_product_does_not_name() {
    // The three host states the wizard has to distinguish start here: a host only
    // in the closure is not named by a `use_gear`, so it has to be promoted to
    // one before a plugin can be attached to it.
    let source = r#"product(
    gears = [
        use_gear("api-gateway", source = "gears-rust"),
    ],
)
"#;
    let refused = add_gear_plugin(URI, source, "authn-resolver", "static-authn-plugin")
        .expect_err("a host that is not named must be refused");
    let text = format!("{refused:?}");
    assert!(text.contains("authn-resolver"), "{text}");
    assert!(text.contains("add the host gear first"), "{text}");
}

#[test]
fn set_gear_plugins_writes_plugin_list() {
    let source = r#"product(
    gears = [
        use_gear("authn-resolver", source = "gears-rust"),
    ],
)
"#;
    let edited = set_gear_plugins(
        URI,
        source,
        "authn-resolver",
        &[
            "static-authn-plugin".to_owned(),
            "oidc-authn-plugin".to_owned(),
        ],
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();
    assert!(
        edited
            .contains(r#"plugins = [plugin("static-authn-plugin"), plugin("oidc-authn-plugin")]"#),
        "{edited}"
    );
    assert_eq!(
        set_gear_plugins(
            URI,
            &edited,
            "authn-resolver",
            &[
                "static-authn-plugin".to_owned(),
                "oidc-authn-plugin".to_owned()
            ],
        )
        .expect("editable"),
        Edit::Unchanged
    );
}

/// A product that declares its sources, which is what the wizard now writes.
const WITH_SOURCES: &str = r#"product(
    id = "new-product",
    sources = [source(id = "gears-rust", at = path("../../../gears-rust"))],
    gears = [],
)
"#;

#[test]
fn a_source_the_product_does_not_declare_is_refused() {
    // The join that used to break in silence. The wizard minted `source-1`
    // while the catalogue called the same root `gears-rust`, so Add Gear wrote
    // an id nothing declared -- a file that saved and then would not load, with
    // the refusal naming the description rather than this edit.
    let refusal = add_gear(URI, WITH_SOURCES, "service-discovery", "source-1")
        .expect_err("an undeclared source is refused");
    let first = refusal.iter().next().expect("one diagnostic");
    assert_eq!(first.code, DiagnosticCode::GdlEval);
    assert!(
        first.message.contains("source-1") && first.message.contains("`gears-rust`"),
        "the refusal should name both what was asked for and what is declared: {}",
        first.message
    );
}

#[test]
fn a_declared_source_is_accepted() {
    // The other half, and the one that says the check is not simply a refusal
    // of everything: the same product, the id it actually declares.
    let edited = add_gear(URI, WITH_SOURCES, "service-discovery", "gears-rust")
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(
        edited.contains(r#"use_gear("service-discovery", source = "gears-rust")"#),
        "{edited}"
    );
}

#[test]
fn a_product_with_no_sources_list_is_still_editable() {
    // Absent is not empty. A description that declares no `sources` at all is
    // the loader's business, and refusing here would turn a missing argument
    // into a failed edit -- which is what every other test in this file would
    // have hit, since none of their fixtures declares one.
    let source = "product(gears = [])\n";
    assert!(
        add_gear(URI, source, "a", "anything")
            .expect("editable")
            .changed()
            .is_some(),
        "a product with no sources list must still accept an edit"
    );
}

/// One host, one implementation, twice, for scopes that do not overlap.
///
/// The shape `add_gear_plugin` cannot express and `set_gear_plugins` destroys,
/// and the reason a connection is addressed by position: both entries name
/// `static-authn-plugin`, so nothing else tells them apart.
const REPEATED_PLUGIN: &str = r#"product(
    gears = [
        use_gear("api-gateway", source = "gears-rust"),
        use_gear("authn-resolver", source = "gears-rust",
            plugins = [
                # Permissive locally, strict in production: one plugin, two scopes.
                plugin("static-authn-plugin", profiles = ["dev", "local"],
                       config = {"mode": "accept_all"}),
                plugin("static-authn-plugin", profiles = ["prod"],
                       config = {"mode": "strict"}),
            ],
        ),
    ],
)
"#;

#[test]
fn editing_one_connection_leaves_every_neighbour_alone() {
    // The claim the whole per-entry editor exists to make. `set_gear_plugins`
    // rewrites the list and would flatten all of this.
    let edited = edit_plugin_entry(
        URI,
        HOST_WITH_PLUGINS,
        "authn-resolver",
        0,
        "static-authn-plugin",
        Some(("mode", Some(&str_value("strict")))),
        None,
        false,
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(
        edited.contains(r#""mode": "strict""#),
        "the edited key did not change: {edited}"
    );
    assert!(
        edited.contains(r#"profiles = ["dev", "local"]"#),
        "the edited entry lost its own scope: {edited}"
    );
    assert!(
        edited.contains(r#"plugin("oidc-authn-plugin", profiles = ["prod"]"#),
        "the sibling connection was reshaped: {edited}"
    );
    assert!(
        edited.contains(r#"config = {"issuer": "https://id.example.com"}"#),
        "the sibling connection lost its config: {edited}"
    );
    assert!(
        edited.contains("# `local` takes the static plugin"),
        "a comment inside the list was lost: {edited}"
    );
    assert!(
        edited.contains("# authn-resolver routes to whichever plugin"),
        "a comment above the host was lost: {edited}"
    );
    assert!(
        edited.contains(r#"use_gear("api-gateway", source = "gears-rust"),"#),
        "another gear was reshaped: {edited}"
    );
}

#[test]
fn two_entries_of_one_plugin_are_edited_separately() {
    // Both entries name the same implementation, so only the position tells the
    // editor which one was meant.
    let edited = edit_plugin_entry(
        URI,
        REPEATED_PLUGIN,
        "authn-resolver",
        1,
        "static-authn-plugin",
        Some(("mode", Some(&str_value("paranoid")))),
        None,
        false,
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(
        edited.contains(r#""mode": "paranoid""#),
        "the addressed entry did not change: {edited}"
    );
    assert!(
        edited.contains(r#""mode": "accept_all""#),
        "the entry at the other position changed too: {edited}"
    );
}

#[test]
fn a_position_naming_the_wrong_plugin_is_refused() {
    // A stale address is a refusal, never an edit of whatever moved into place.
    let refusal = edit_plugin_entry(
        URI,
        HOST_WITH_PLUGINS,
        "authn-resolver",
        0,
        "oidc-authn-plugin",
        None,
        Some(&["prod".to_owned()]),
        false,
    )
    .expect_err("a mismatched name must refuse");
    assert_eq!(refusal.as_slice()[0].code, DiagnosticCode::GdlCardinality);
}

#[test]
fn a_position_past_the_end_is_refused() {
    edit_plugin_entry(
        URI,
        HOST_WITH_PLUGINS,
        "authn-resolver",
        7,
        "static-authn-plugin",
        None,
        None,
        true,
    )
    .expect_err("a position past the end must refuse");
}

#[test]
fn a_malformed_sibling_does_not_block_editing_the_others() {
    // The regression `entry_index` exists for. Evaluation drops the unparseable
    // entry, so a caller counting evaluated selections would address `oidc` as 0
    // and edit the wrong line. Counting written entries, `oidc` is 1 and this
    // works -- which is what keeps a product with one bad entry repairable.
    let source = r#"product(
    gears = [
        use_gear("authn-resolver", source = "gears-rust",
            plugins = [
                plugin("Not A Gear Id"),
                plugin("oidc-authn-plugin", config = {"issuer": "https://old.example.com"}),
            ],
        ),
    ],
)
"#;
    let edited = edit_plugin_entry(
        URI,
        source,
        "authn-resolver",
        1,
        "oidc-authn-plugin",
        Some(("issuer", Some(&str_value("https://id.example.com")))),
        None,
        false,
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(
        edited.contains("https://id.example.com"),
        "the good entry was not edited: {edited}"
    );
    assert!(
        edited.contains(r#"plugin("Not A Gear Id")"#),
        "the malformed entry was not left for the person to fix: {edited}"
    );
}

#[test]
fn a_connection_scope_is_replaced_whole() {
    let edited = edit_plugin_entry(
        URI,
        HOST_WITH_PLUGINS,
        "authn-resolver",
        0,
        "static-authn-plugin",
        None,
        Some(&["staging".to_owned()]),
        false,
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(
        edited.contains(r#"profiles = ["staging"]"#),
        "the scope was not replaced: {edited}"
    );
    assert!(
        !edited.contains(r#"profiles = ["dev", "local"]"#),
        "the old scope survived beside the new one: {edited}"
    );
    assert!(
        edited.contains(r#"config = {"mode": "accept_all"}"#),
        "replacing the scope disturbed the config: {edited}"
    );
}

#[test]
fn an_empty_scope_removes_the_argument_rather_than_writing_no_profile() {
    // `profiles = []` would read as "under no profile at all". Every profile is
    // the absence of the argument -- see `ProductIntent::applies`.
    let edited = edit_plugin_entry(
        URI,
        HOST_WITH_PLUGINS,
        "authn-resolver",
        0,
        "static-authn-plugin",
        None,
        Some(&[]),
        false,
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(
        !edited.contains("profiles = []"),
        "an empty scope was written literally: {edited}"
    );
    assert!(
        !edited.contains(r#"profiles = ["dev", "local"]"#),
        "the scope was not removed: {edited}"
    );
    assert!(
        edited.contains(r#"profiles = ["prod"]"#),
        "the sibling's scope was removed too: {edited}"
    );
}

#[test]
fn removing_a_connection_leaves_the_host_and_its_other_connections() {
    let edited = edit_plugin_entry(
        URI,
        HOST_WITH_PLUGINS,
        "authn-resolver",
        0,
        "static-authn-plugin",
        None,
        None,
        true,
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(
        !edited.contains("static-authn-plugin"),
        "the connection was not removed: {edited}"
    );
    assert!(
        edited.contains(r#"plugin("oidc-authn-plugin""#),
        "the sibling connection went with it: {edited}"
    );
    assert!(
        edited.contains(r#"use_gear("authn-resolver""#),
        "the host went with its plugin: {edited}"
    );
}

#[test]
fn a_config_key_is_removed_from_one_connection_only() {
    let edited = edit_plugin_entry(
        URI,
        HOST_WITH_PLUGINS,
        "authn-resolver",
        0,
        "static-authn-plugin",
        Some(("mode", None)),
        None,
        false,
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(
        !edited.contains(r#""mode": "accept_all""#),
        "the key was not removed: {edited}"
    );
    assert!(
        edited.contains(r#"config = {"issuer": "https://id.example.com"}"#),
        "the sibling's config was disturbed: {edited}"
    );
}

#[test]
fn a_literal_secret_is_refused_on_a_connection_too() {
    // The same rule the gear-level editor applies; a connection is not a way
    // around it.
    let refusal = edit_plugin_entry(
        URI,
        HOST_WITH_PLUGINS,
        "authn-resolver",
        0,
        "static-authn-plugin",
        Some(("password", Some(&str_value("hunter2")))),
        None,
        false,
    )
    .expect_err("a literal secret must refuse");
    assert_eq!(refusal.as_slice()[0].code, DiagnosticCode::GdlCardinality);
}

#[test]
fn add_plugin_selection_appends_with_its_scope() {
    let edited = add_plugin_selection(
        URI,
        HOST_WITH_PLUGINS,
        "authn-resolver",
        "ldap-authn-plugin",
        &["staging".to_owned()],
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(
        edited.contains(r#"plugin("ldap-authn-plugin", profiles = ["staging"])"#),
        "the scoped entry was not written: {edited}"
    );
    assert!(
        edited.contains(r#"config = {"mode": "accept_all"}"#),
        "an existing entry was flattened: {edited}"
    );
    assert!(
        edited.contains("# `local` takes the static plugin"),
        "a comment inside the list was lost: {edited}"
    );
}

#[test]
fn add_plugin_selection_is_idempotent_within_one_scope() {
    // The same implementation under the same scope is the duplicate the
    // evaluator reports; writing it again would only produce a diagnostic.
    assert_eq!(
        add_plugin_selection(
            URI,
            REPEATED_PLUGIN,
            "authn-resolver",
            "static-authn-plugin",
            &["prod".to_owned()],
        )
        .expect("editable"),
        Edit::Unchanged
    );
}

#[test]
fn add_plugin_selection_appends_the_same_plugin_under_a_different_scope() {
    // The case that makes the scope part of the identity: not a duplicate.
    let edited = add_plugin_selection(
        URI,
        REPEATED_PLUGIN,
        "authn-resolver",
        "static-authn-plugin",
        &["staging".to_owned()],
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(
        edited.contains(r#"plugin("static-authn-plugin", profiles = ["staging"])"#),
        "the differently scoped entry was refused as a duplicate: {edited}"
    );
}

#[test]
fn add_plugin_selection_creates_the_argument_when_absent() {
    let source = r#"product(
    gears = [
        use_gear("authn-resolver", source = "gears-rust"),
    ],
)
"#;
    let edited = add_plugin_selection(URI, source, "authn-resolver", "oidc-authn-plugin", &[])
        .expect("editable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(
        edited.contains(r#"plugins = [plugin("oidc-authn-plugin")]"#),
        "the argument was not created: {edited}"
    );
}

#[test]
fn add_plugin_selection_refuses_a_host_that_is_not_selected() {
    add_plugin_selection(
        URI,
        HOST_WITH_PLUGINS,
        "not-a-gear",
        "oidc-authn-plugin",
        &[],
    )
    .expect_err("an absent host must refuse");
}

#[test]
fn add_then_remove_is_an_inverse_for_a_connection() {
    // The same round-trip claim the gear-level edits make, one level down.
    let added = add_plugin_selection(
        URI,
        HOST_WITH_PLUGINS,
        "authn-resolver",
        "ldap-authn-plugin",
        &["staging".to_owned()],
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    let back = edit_plugin_entry(
        URI,
        &added,
        "authn-resolver",
        2,
        "ldap-authn-plugin",
        None,
        None,
        true,
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert_eq!(back, HOST_WITH_PLUGINS);
}

/// Two cluster profiles under one name, for disjoint deployment profiles.
///
/// The shape `payments-demo` actually has, and the reason a provider option is
/// addressed by written position: the name alone picks one of two.
const TWO_SCOPES: &str = r#"product(
    id = "payments-demo",
    cluster_profiles = [
        cluster_profile(
            name = "event-broker",
            cache = provider("standalone"),
            profiles = ["dev"],
        ),
        # The one an operator configures.
        cluster_profile(
            name = "event-broker",
            profiles = ["local", "prod"],
            cache = provider(
                "postgres",
                secret_ref = "env:PG_PASSWORD",
                schema = "cluster",
            ),
        ),
    ],
)
"#;

#[test]
fn a_provider_option_is_set_on_the_entry_it_addresses() {
    let edited = crate::edit::set_provider_option(
        URI,
        TWO_SCOPES,
        "event-broker",
        1,
        "cache",
        "pool_max_size",
        Some(&gearbox_ir::ConfigValue::Int(10)),
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(
        edited.contains("pool_max_size = 10"),
        "the option was not written:\n{edited}"
    );
    // **And it parses.** This case has always been an insertion after a trailing
    // comma, and it passed while producing `,\n, pool_max_size = 10` -- because it
    // asked only whether the option was in the text. The browser found it.
    AstModule::parse(URI, edited.clone(), &crate::declarative::dialect())
        .expect("an inserted option must leave the description parseable");
    assert!(
        edited.contains(
            "                schema = \"cluster\",\n                pool_max_size = 10,\n"
        ),
        "one per line, at the neighbours' indentation, with its own trailing comma:\n{edited}"
    );
    // The *other* entry is untouched, which is the whole point of the address.
    assert!(
        edited.contains(r#"cache = provider("standalone"),"#),
        "the dev scope was rewritten:\n{edited}"
    );
    // And the comment between them survives, as everywhere else in this module.
    assert!(edited.contains("# The one an operator configures."));
}

#[test]
fn a_provider_option_is_removed_by_clearing_it() {
    let edited = crate::edit::set_provider_option(
        URI,
        TWO_SCOPES,
        "event-broker",
        1,
        "cache",
        "schema",
        None,
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();

    assert!(!edited.contains("schema ="), "the option stayed:\n{edited}");
    assert!(
        edited.contains(r#"secret_ref = "env:PG_PASSWORD""#),
        "its sibling went with it:\n{edited}"
    );
}

#[test]
fn an_address_that_names_another_scope_is_refused() {
    // The stale-address case. Position 0 is `event-broker` too, so this is the
    // narrower failure: right name, wrong subject.
    let refused = crate::edit::set_provider_option(
        URI,
        TWO_SCOPES,
        "audit",
        0,
        "cache",
        "pool_max_size",
        Some(&gearbox_ir::ConfigValue::Int(10)),
    );
    assert!(refused.is_err(), "a stale address was applied");

    let past_the_end = crate::edit::set_provider_option(
        URI,
        TWO_SCOPES,
        "event-broker",
        7,
        "cache",
        "pool_max_size",
        Some(&gearbox_ir::ConfigValue::Int(10)),
    );
    assert!(past_the_end.is_err(), "a position past the end was applied");
}

#[test]
fn a_primitive_the_scope_does_not_bind_is_refused() {
    // Neither entry binds a lock. Refused rather than invented: writing
    // `lock = provider(...)` here would bind a backend nobody chose.
    let refused = crate::edit::set_provider_option(
        URI,
        TWO_SCOPES,
        "event-broker",
        1,
        "lock",
        "lock_name_cardinality_warn_threshold",
        Some(&gearbox_ir::ConfigValue::Int(64)),
    );
    assert!(refused.is_err(), "an unbound primitive was edited");
}

#[test]
fn setting_an_option_to_what_it_already_says_changes_nothing() {
    let edit = crate::edit::set_provider_option(
        URI,
        TWO_SCOPES,
        "event-broker",
        1,
        "cache",
        "schema",
        Some(&str_value("cluster")),
    )
    .expect("editable");
    assert!(
        edit.changed().is_none(),
        "an idempotent edit reported a change"
    );
}

/// A trailing comma on one line takes the new argument after it, on that line.
#[test]
fn a_named_argument_after_a_one_line_trailing_comma_parses() {
    let source = "product(id = \"p\", version = \"0.1.0\",)\n";
    let cloned = clone_product_text(URI, source, "q", "Q", None).expect("cloneable");
    assert_eq!(
        cloned,
        "product(id = \"q\", version = \"0.1.0\", name = \"Q\",)\n"
    );
}

/// Cloning a product that omits `name` and ends with a trailing comma.
///
/// The shape every multi-line description in the corpus has; `name` is optional,
/// so the stamp is an insertion, and the insertion was the one that broke.
#[test]
fn cloning_a_product_without_a_name_keeps_it_parseable() {
    let source = "product(\n    id = \"p\",\n    version = \"0.1.0\",\n    gears = [],\n)\n";
    let cloned = clone_product_text(URI, source, "q", "Q", None).expect("cloneable");
    assert!(
        cloned.contains("    gears = [],\n    name = \"Q\",\n)"),
        "{cloned}"
    );
    AstModule::parse(URI, cloned, &crate::declarative::dialect()).expect("parses");
}

/// A config key added to a dict written with a trailing comma.
#[test]
fn a_config_key_after_a_trailing_comma_parses() {
    let source = r#"product(
    gears = [
        use_gear("api-gateway", source = "s", config = {
            "enable_docs": True,
        }),
    ],
)
"#;
    let edited = set_gear_config(
        URI,
        source,
        "api-gateway",
        "prefix_path",
        Some(&str_value("/cf")),
    )
    .expect("editable")
    .changed()
    .expect("changed")
    .to_owned();
    assert!(
        edited.contains(
            "            \"enable_docs\": True,\n            \"prefix_path\": \"/cf\",\n"
        ),
        "{edited}"
    );
    AstModule::parse(URI, edited, &crate::declarative::dialect()).expect("parses");
}

/// A comment between the last argument and the bracket does not hide the
/// trailing comma, nor swallow the new argument.
#[test]
fn an_insertion_before_a_trailing_comment_parses() {
    let whole_line =
        "product(\n    id = \"p\",\n    version = \"0.1.0\",\n    # why the list ends here\n)\n";
    let cloned = clone_product_text(URI, whole_line, "q", "Q", None).expect("cloneable");
    assert!(
        cloned.contains(
            "    version = \"0.1.0\",\n    name = \"Q\",\n    # why the list ends here\n)"
        ),
        "{cloned}"
    );
    AstModule::parse(URI, cloned, &crate::declarative::dialect()).expect("parses");

    // On the same line, and with a `#` inside a string that is not a comment.
    let inline = "product(id = \"p#1\", version = \"0.1.0\"  # note\n)\n";
    let cloned = clone_product_text(URI, inline, "q", "Q", None).expect("cloneable");
    assert!(
        cloned.contains("version = \"0.1.0\", name = \"Q\"  # note\n)"),
        "the argument must land before the comment, not inside it:\n{cloned}"
    );
    AstModule::parse(URI, cloned, &crate::declarative::dialect()).expect("parses");
}

/// A malformed profile id is refused before it is written.
///
/// It used to be written, and the description then failed to evaluate -- the
/// panel's next state was "could not be evaluated", for a profile it had offered
/// to add.
#[test]
fn a_malformed_profile_id_is_refused_before_it_is_written() {
    for bad in ["Bad_Id", "1prod", "has space", "trailing-", "a--b", ""] {
        let refused = add_profile(URI, WITH_CONFIG, "embedded", bad, &[]);
        let diagnostics = refused.expect_err(bad);
        assert!(
            diagnostics
                .as_slice()
                .iter()
                .any(|d| d.message.contains("is not a valid profile id")),
            "{bad}: {diagnostics:?}"
        );
    }
}

const TWO_SOURCES: &str = r#"product(
    id = "p",
    version = "0.1.0",
    sources = [
        source(id = "gears-rust", at = path("../gears-rust")),
        # scaffolded for this product
        source(id = "gears", at = path("gears")),
    ],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [
        use_gear("api-gateway", source = "gears-rust"),
    ],
)
"#;

/// A source nothing reads is removed, its neighbour and the comments stay.
#[test]
fn an_unused_source_is_removed_and_nothing_else() {
    let removed = remove_source(URI, TWO_SOURCES, "gears")
        .expect("removable")
        .changed()
        .expect("changed")
        .to_owned();
    assert!(!removed.contains(r#"source(id = "gears", "#), "{removed}");
    assert!(
        removed.contains(r#"source(id = "gears-rust", at = path("../gears-rust"))"#),
        "{removed}"
    );
    AstModule::parse(URI, removed, &crate::declarative::dialect()).expect("parses");
}

/// A source a gear still reads from is refused, and an absent one is a no-op.
#[test]
fn a_source_in_use_is_refused_and_an_absent_one_is_unchanged() {
    let refused = remove_source(URI, TWO_SOURCES, "gears-rust").expect_err("still used");
    assert!(
        refused
            .as_slice()
            .iter()
            .any(|d| d.message.contains("still reads from source `gears-rust`")),
        "{refused:?}"
    );
    assert_eq!(
        remove_source(URI, TWO_SOURCES, "nowhere").expect("parses"),
        Edit::Unchanged
    );
}

#[test]
fn a_created_product_accepts_another_source_and_gear() {
    let created = render_product_template(&CreateProductParams {
        id: "roundtrip".to_owned(),
        name: "Roundtrip".to_owned(),
        version: "0.1.0".to_owned(),
        sources: vec![("corpus".to_owned(), "../corpus".to_owned())],
        profile_kind: "embedded".to_owned(),
        profile_id: "dev".to_owned(),
    });
    let added = add_source(URI, &created, "new-gears", "gears").unwrap();
    let added = add_gear(URI, added.changed().unwrap(), "new-service", "new-gears").unwrap();
    let evaluated =
        crate::GdlEngine::new().eval_product(&product_identity(), added.changed().unwrap());
    assert!(
        !evaluated.diagnostics.has_errors(),
        "{:?}",
        evaluated.diagnostics
    );
    let intent = evaluated.value.unwrap();
    assert_eq!(intent.sources.len(), 2);
    assert_eq!(intent.selected_gears.len(), 1);
    assert_eq!(intent.selected_gears[0].gear.as_str(), "new-service");
}

#[test]
fn inline_empty_gears_keep_the_entry_inside_the_brackets() {
    for source in [
        "product(id = \"demo\", gears = [], version = \"0.1.0\")\n",
        "product(\n    id = \"demo\",\n    preferences = [], gears = [])\n",
    ] {
        let edited = add_gear(URI, source, "tenant-resolver", "s").unwrap();
        let text = edited.changed().unwrap();
        assert_eq!(
            text,
            source.replace(
                "gears = []",
                "gears = [use_gear(\"tenant-resolver\", source = \"s\")]"
            )
        );
        AstModule::parse(URI, text.to_owned(), &crate::declarative::dialect())
            .expect("still parses");
    }
}

#[test]
fn inline_empty_product_accepts_a_host_then_its_plugin() {
    let source = r#"gdl_version = "1.4"
product(id = "p", name = "Pair", version = "0.1.0",
    sources = [source(id = "s", at = path("../sdk"))],
    profiles = [embedded(id = "dev", development = True)], default_profile = "dev", gears = [])
"#;
    let host = add_gear(URI, source, "tenant-resolver", "s").unwrap();
    let connected = add_plugin_selection(
        URI,
        host.changed().unwrap(),
        "tenant-resolver",
        "static-tr-plugin",
        &[],
    )
    .unwrap();
    let text = connected.changed().unwrap();
    let evaluated = crate::GdlEngine::new().eval_product(&product_identity(), text);
    assert!(
        !evaluated.diagnostics.has_errors(),
        "{:?}",
        evaluated.diagnostics
    );
    let intent = evaluated.value.unwrap();
    assert_eq!(intent.selected_gears.len(), 1);
    assert_eq!(intent.selected_gears[0].gear.as_str(), "tenant-resolver");
    assert_eq!(
        intent.selected_gears[0].plugins[0].gear.as_str(),
        "static-tr-plugin"
    );
}
