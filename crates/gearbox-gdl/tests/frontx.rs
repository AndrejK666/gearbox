//! Evaluating a `frontx.gdl` (GDL 1.6): a `FrontX` package describing itself.
//!
//! A package declares only what its own manifests cannot say -- whether a
//! shell signs users in and where it reads its runtime addresses, which
//! backend gears a micro-frontend calls -- and is exactly one kind.

#![allow(
    clippy::unwrap_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use gearbox_gdl::{FileIdentity, GdlEngine};
use gearbox_ir::frontx::{FrontxPackage, FrontxShell};
use gearbox_ir::{DiagnosticCode, RelPath, SourceId};

fn identity() -> FileIdentity {
    FileIdentity {
        uri: "file:///repo/template-shell/frontx.gdl".to_owned(),
        source: SourceId::new("frontx").unwrap(),
        gdl_path: RelPath::new("frontx.gdl").unwrap(),
        load_paths: None,
    }
}

fn eval(src: &str) -> (Option<FrontxPackage>, Vec<DiagnosticCode>, String) {
    let out = GdlEngine::new().eval_frontx(&identity(), src);
    let codes = out.diagnostics.iter().map(|d| d.code).collect();
    let messages = out
        .diagnostics
        .iter()
        .map(|d| format!("{} ({})", d.message, d.help.as_deref().unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(" | ");
    (out.value, codes, messages)
}

#[test]
fn a_shell_says_whether_it_signs_in_and_where_it_reads_its_addresses() {
    let (package, codes, messages) = eval(
        r#"gdl_version = "1.6"

shell(signs_in = True, runtime_env = "public/env.js")
"#,
    );
    assert!(codes.is_empty(), "{codes:?} {messages}");
    assert_eq!(
        package.unwrap(),
        FrontxPackage::Shell(FrontxShell {
            signs_in: true,
            runtime_env: Some("public/env.js".to_owned()),
        })
    );
}

#[test]
fn a_shell_that_takes_its_addresses_at_build_names_no_file() {
    for src in [
        "shell(signs_in = False)",
        "shell(signs_in = False, runtime_env = None)",
    ] {
        let (package, codes, messages) = eval(src);
        assert!(codes.is_empty(), "{src}: {codes:?} {messages}");
        assert_eq!(
            package.unwrap(),
            FrontxPackage::Shell(FrontxShell {
                signs_in: false,
                runtime_env: None,
            }),
            "{src}"
        );
    }
}

#[test]
fn a_shell_must_say_whether_it_signs_in() {
    let (package, codes, messages) = eval("shell()");
    assert!(package.is_none());
    assert!(!codes.is_empty(), "{messages}");
    assert!(messages.contains("signs_in"), "{messages}");
}

#[test]
fn a_runtime_env_stays_inside_the_package() {
    for bad in ["../env.js", "/env.js", r"public\\env.js"] {
        let (package, codes, messages) =
            eval(&format!(r#"shell(signs_in = True, runtime_env = "{bad}")"#));
        assert!(package.is_none(), "{bad}");
        assert_eq!(codes, [DiagnosticCode::GdlEval], "{bad}: {messages}");
        assert!(messages.contains("runtime_env"), "{bad}: {messages}");
    }
}

#[test]
fn what_a_shell_carries_is_not_declared() {
    // Read from its workspace and `package.json`; a second copy here would drift.
    let (package, codes, messages) =
        eval(r#"shell(signs_in = True, carries = {"@gears-frontx/react": "0.2.0"})"#);
    assert!(package.is_none());
    assert_eq!(codes, [DiagnosticCode::GdlUnknownArgument], "{messages}");
}

#[test]
fn a_micro_frontend_names_the_gears_it_calls() {
    let (package, codes, messages) =
        eval(r#"mfe(requires = ["account-management", "studio-user"])"#);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let FrontxPackage::Mfe { requires } = package.unwrap() else {
        panic!("not an mfe");
    };
    assert_eq!(
        requires
            .iter()
            .map(gearbox_ir::GearId::as_str)
            .collect::<Vec<_>>(),
        ["account-management", "studio-user"]
    );
}

#[test]
fn a_micro_frontend_that_calls_nothing_says_so() {
    for src in ["mfe()", "mfe(requires = [])"] {
        let (package, codes, messages) = eval(src);
        assert!(codes.is_empty(), "{src}: {codes:?} {messages}");
        assert_eq!(
            package.unwrap(),
            FrontxPackage::Mfe {
                requires: std::collections::BTreeSet::new()
            }
        );
    }
}

#[test]
fn a_required_gear_is_a_gear_id() {
    let (package, codes, messages) = eval(r#"mfe(requires = ["Account_Management"])"#);
    assert!(package.is_none());
    assert_eq!(codes, [DiagnosticCode::GdlEval], "{messages}");
    assert!(messages.contains("Account_Management"), "{messages}");
}

#[test]
fn a_micro_frontends_screens_stay_in_its_mfe_json() {
    let (package, codes, messages) = eval(r#"mfe(extensions = [{"id": "people"}])"#);
    assert!(package.is_none());
    assert_eq!(codes, [DiagnosticCode::GdlUnknownArgument], "{messages}");
}

#[test]
fn a_library_declares_nothing_more() {
    let (package, codes, messages) = eval("library()");
    assert!(codes.is_empty(), "{codes:?} {messages}");
    assert_eq!(package.unwrap(), FrontxPackage::Library);
}

#[test]
fn a_package_is_one_kind() {
    let (package, codes, messages) = eval("shell(signs_in = True)\nmfe()\n");
    assert!(package.is_none());
    assert_eq!(codes, [DiagnosticCode::GdlCardinality], "{messages}");
    assert!(messages.contains("frontx.gdl"), "{messages}");
}

#[test]
fn a_description_with_no_declaration_says_which_to_add() {
    let (package, codes, messages) = eval(r#"gdl_version = "1.6""#);
    assert!(package.is_none());
    assert_eq!(codes, [DiagnosticCode::GdlCardinality], "{messages}");
    assert!(
        messages.contains("`shell(...)`, `mfe(...)` or `library(...)`"),
        "{messages}"
    );
}

#[test]
fn a_package_description_is_neither_a_gear_nor_a_product() {
    for src in [
        r#"gear(maturity = "stable")"#,
        r#"product(id = "demo", version = "0.1.0", sources = [], profiles = [], default_profile = "dev", gears = [])"#,
        r#"frontx_mfe(package = path("people-mfe"))"#,
    ] {
        let (package, codes, messages) = eval(src);
        assert!(package.is_none(), "{src}");
        assert!(!codes.is_empty(), "{src}: {messages}");
    }
}

#[test]
fn a_package_description_is_declarative() {
    let (package, codes, _) = eval("shell(signs_in = True if 1 else False)");
    assert!(package.is_none());
    assert!(
        codes.contains(&DiagnosticCode::GdlForbiddenConstruct),
        "{codes:?}"
    );
}

#[test]
fn the_editor_offers_the_package_vocabulary_in_a_frontx_gdl() {
    let path = std::path::Path::new("/repo/template-shell/frontx.gdl");
    let top = gearbox_gdl::assist::completion(path, "", 0);
    let labels: Vec<_> = top.suggestions.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["library", "mfe", "shell"]);

    let src = "shell(";
    let inside = gearbox_gdl::assist::completion(path, src, src.len());
    assert!(inside.in_call);
    let params: Vec<_> = inside
        .suggestions
        .iter()
        .map(|s| s.label.as_str())
        .collect();
    assert!(params.contains(&"signs_in"), "{params:?}");
    assert!(params.contains(&"runtime_env"), "{params:?}");
}
