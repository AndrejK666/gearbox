//! Evaluating a `frontx.gdl` (GDL 1.5): a `FrontX` package's passport.
//!
//! A package says what it is, exactly one kind, and may say everything a tool
//! needs about it -- including what its `package.json` and `mfe.json` say;
//! every argument but a shell's `signs_in` is optional.

#![allow(
    clippy::unwrap_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::collections::BTreeMap;

use gearbox_gdl::{FileIdentity, GdlEngine};
use gearbox_ir::frontx::{FrontxIdentity, FrontxMfePackage, FrontxPackage, FrontxShell};
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
        r#"gdl_version = "1.5"

shell(signs_in = True, runtime_env = "public/env.js")
"#,
    );
    assert!(codes.is_empty(), "{codes:?} {messages}");
    assert_eq!(
        package.unwrap(),
        FrontxPackage::Shell(FrontxShell::new(true, Some("public/env.js".to_owned())))
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
            FrontxPackage::Shell(FrontxShell::new(false, None)),
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
fn a_shells_passport_says_everything_about_it() {
    let (package, codes, messages) = eval(
        r#"gdl_version = "1.5"

shell(
    name = "@acme/shell",
    version = "1.2.0",
    description = "The Acme portal",
    node = ">=24",
    package_manager = "npm",
    build = "npm run build",
    output = "dist",
    base = "/",
    api_prefix = "/cf/",
    runtime_env = "public/env.js",
    env = {"apiBase": "gateway.url", "oidcIssuer": "oidc.issuer"},
    build_env = ["VITE_API_BASE"],
    signs_in = True,
    sign_in_redirect = "/callback",
    carries = {"@gears-frontx/react": "0.2.0-alpha.15"},
)
"#,
    );
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let FrontxPackage::Shell(shell) = package.unwrap() else {
        panic!("not a shell");
    };
    assert_eq!(shell.identity.name.as_deref(), Some("@acme/shell"));
    assert_eq!(shell.identity.version.as_deref(), Some("1.2.0"));
    assert_eq!(shell.node.as_deref(), Some(">=24"));
    assert_eq!(shell.package_manager.as_deref(), Some("npm"));
    assert_eq!(shell.build.as_deref(), Some("npm run build"));
    assert_eq!(shell.output.as_deref(), Some("dist"));
    assert_eq!(shell.base.as_deref(), Some("/"));
    assert_eq!(shell.api_prefix.as_deref(), Some("/cf/"));
    assert_eq!(shell.env["apiBase"], "gateway.url");
    assert_eq!(shell.build_env, ["VITE_API_BASE"]);
    assert_eq!(shell.sign_in_redirect, ["/callback"]);
    assert_eq!(
        shell.carries,
        Some(BTreeMap::from([(
            "@gears-frontx/react".to_owned(),
            "0.2.0-alpha.15".to_owned()
        )]))
    );
}

#[test]
fn a_shell_may_come_back_to_several_paths() {
    let (package, codes, messages) =
        eval(r#"shell(signs_in = True, sign_in_redirect = ["/callback", "/silent"])"#);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let FrontxPackage::Shell(shell) = package.unwrap() else {
        panic!("not a shell");
    };
    assert_eq!(shell.sign_in_redirect, ["/callback", "/silent"]);
}

#[test]
fn a_shells_passport_is_refused_where_it_could_not_be_true() {
    for (src, field) in [
        (r#"shell(signs_in = True, api_prefix = "/")"#, "api_prefix"),
        (
            r#"shell(signs_in = True, api_prefix = "cf/")"#,
            "api_prefix",
        ),
        (
            r#"shell(signs_in = True, api_prefix = "/cf/;deny all;/")"#,
            "api_prefix",
        ),
        (r#"shell(signs_in = True, base = "/app")"#, "base"),
        (r#"shell(signs_in = True, output = "../dist")"#, "output"),
        (
            r#"shell(signs_in = True, build = "npm run build\nRUN true")"#,
            "build",
        ),
        (
            r#"shell(signs_in = True, package_manager = "bun")"#,
            "package_manager",
        ),
        (
            r#"shell(signs_in = True, build_env = ["vite_api"])"#,
            "build_env",
        ),
        (
            r#"shell(signs_in = False, sign_in_redirect = "/callback")"#,
            "sign_in_redirect",
        ),
        (
            r#"shell(signs_in = True, sign_in_redirect = "callback")"#,
            "sign_in_redirect",
        ),
        (r#"shell(signs_in = True, name = "two words")"#, "name"),
    ] {
        let (package, codes, messages) = eval(src);
        assert!(package.is_none(), "{src}");
        assert!(!codes.is_empty(), "{src}");
        assert!(messages.contains(field), "{src}: {messages}");
    }
}

#[test]
fn an_env_is_strings_to_strings() {
    let (package, codes, _) = eval(r#"shell(signs_in = True, env = {"apiBase": 1})"#);
    assert!(package.is_none());
    assert!(!codes.is_empty());
}

#[test]
fn a_micro_frontend_names_the_gears_it_calls() {
    let (package, codes, messages) =
        eval(r#"mfe(requires = ["account-management", "studio-user"])"#);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let FrontxPackage::Mfe(mfe) = package.unwrap() else {
        panic!("not an mfe");
    };
    assert_eq!(
        mfe.requires
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
            FrontxPackage::Mfe(FrontxMfePackage::default())
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
fn a_micro_frontends_passport_says_everything_about_it() {
    let (package, codes, messages) = eval(
        r#"mfe(
    name = "@acme/accounts-mfe",
    version = "0.3.0",
    description = "Accounts screens",
    requires = ["accounts"],
    screens = ["Accounts", "Account"],
    mounts = ["/accounts", "/accounts/detail"],
    frontx = "0.2.0-alpha.15",
    roles = ["accounts.read"],
    preview = ["/accounts"],
    fixtures = "preview/fixtures.json",
)"#,
    );
    assert!(codes.is_empty(), "{codes:?} {messages}");
    let FrontxPackage::Mfe(mfe) = package.unwrap() else {
        panic!("not an mfe");
    };
    assert_eq!(mfe.identity.name.as_deref(), Some("@acme/accounts-mfe"));
    assert_eq!(
        mfe.screens,
        Some(vec!["Accounts".to_owned(), "Account".to_owned()])
    );
    assert_eq!(
        mfe.mounts,
        Some(vec!["/accounts".to_owned(), "/accounts/detail".to_owned()])
    );
    assert_eq!(mfe.frontx.as_deref(), Some("0.2.0-alpha.15"));
    assert_eq!(mfe.roles, Some(vec!["accounts.read".to_owned()]));
    assert_eq!(mfe.preview, ["/accounts"]);
    assert_eq!(mfe.fixtures.as_deref(), Some("preview/fixtures.json"));
}

#[test]
fn a_micro_frontends_routes_are_routes() {
    for src in [
        r#"mfe(mounts = ["accounts"])"#,
        r#"mfe(preview = ["accounts"])"#,
        r#"mfe(fixtures = "/abs.json")"#,
    ] {
        let (package, codes, messages) = eval(src);
        assert!(package.is_none(), "{src}");
        assert_eq!(codes, [DiagnosticCode::GdlEval], "{src}: {messages}");
    }
}

#[test]
fn an_argument_outside_the_passport_is_refused() {
    let (package, codes, messages) = eval(r#"mfe(extensions = [{"id": "people"}])"#);
    assert!(package.is_none());
    assert_eq!(codes, [DiagnosticCode::GdlUnknownArgument], "{messages}");
}

#[test]
fn a_library_says_who_it_is_or_nothing() {
    let (package, codes, messages) = eval("library()");
    assert!(codes.is_empty(), "{codes:?} {messages}");
    assert_eq!(
        package.unwrap(),
        FrontxPackage::Library(FrontxIdentity::default())
    );
    let (package, codes, messages) =
        eval(r#"library(name = "@acme/shared", version = "0.1.0", description = "Shared bits")"#);
    assert!(codes.is_empty(), "{codes:?} {messages}");
    assert_eq!(
        package.unwrap().identity().name.as_deref(),
        Some("@acme/shared")
    );
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
    let (package, codes, messages) = eval(r#"gdl_version = "1.5""#);
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
    assert!(params.contains(&"api_prefix"), "{params:?}");
}
