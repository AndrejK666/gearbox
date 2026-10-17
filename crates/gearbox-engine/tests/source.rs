//! `source = git(...)`, URLs in `docs(...)` and `tests = [...]`.
//!
//! A path in a description is looked for on disk first. When it is not there
//! and the gear names its git source, it becomes a link into that repository
//! instead of GBX0109. A URL is kept as written. And when the description is
//! read from a checkout, `source` is checked against it (GBX0121).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;

use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{Catalogue, DiagnosticCode, GearId, SourceId};

const REPO: &str = "https://github.com/cf/gears-rust";

/// A throwaway checkout: `gears/demo` with a crate, and `.git/config` naming
/// `remote` as `upstream`.
struct Checkout(PathBuf);

impl Checkout {
    fn new(label: &str, remote: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "gearbox-source-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let gear = dir.join("gears/demo");
        std::fs::create_dir_all(gear.join("src")).unwrap();
        std::fs::create_dir_all(gear.join("tests")).unwrap();
        std::fs::create_dir_all(gear.join("docs")).unwrap();
        std::fs::write(gear.join("docs/PRD.md"), "# PRD\n").unwrap();
        std::fs::write(
            gear.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(
            gear.join("src/lib.rs"),
            "#[toolkit::gear(name = \"demo\")]\n#[derive(Default)]\npub struct Demo;\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(
            dir.join(".git/config"),
            format!("[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = {remote}\n"),
        )
        .unwrap();
        Self(dir)
    }

    fn describe(&self, extra: &str) -> Catalogue {
        std::fs::write(
            self.0.join("gears/demo/gear.gdl"),
            format!(
                "gear(maturity = \"stable\", package = cargo(crate_name = \"demo\", lib = \"demo\"),\n{extra})\n"
            ),
        )
        .unwrap();
        let root = SourceRoot::open(SourceId::new("fixture").unwrap(), self.0.clone()).unwrap();
        load_catalogue(&[root]).catalogue
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn codes(catalogue: &Catalogue) -> Vec<DiagnosticCode> {
    catalogue
        .diagnostics
        .as_slice()
        .iter()
        .map(|d| d.code)
        .collect()
}

fn demo(catalogue: &Catalogue) -> &gearbox_ir::GearDescriptor {
    &catalogue.gears[&GearId::new("demo").unwrap()]
}

#[test]
fn what_is_not_on_disk_is_linked_into_the_declared_repository() {
    let checkout = Checkout::new("linked", "git@github.com:cf/gears-rust.git");
    let catalogue = checkout.describe(&format!(
        r#"source = git(url = "{REPO}", path = "gears/demo", tag = "v1"),
           docs = docs(prd = "docs/PRD.md", design = "docs/DESIGN.md",
                       openapi = "https://api.example.com/openapi.yaml"),
           tests = ["tests", "../demo-it"]"#
    ));
    assert!(codes(&catalogue).is_empty(), "{:?}", catalogue.diagnostics);
    let gear = demo(&catalogue);

    let docs = gear.docs.as_ref().expect("docs");
    // On disk wins, even with a source declared.
    assert_eq!(
        docs.prd.as_ref().map(gearbox_ir::RelPath::as_str),
        Some("gears/demo/docs/PRD.md")
    );
    assert_eq!(docs.urls.prd, None);
    // Not on disk: the same path in the declared repository.
    assert_eq!(docs.design, None);
    assert_eq!(
        docs.urls.design.as_deref(),
        Some("https://github.com/cf/gears-rust/blob/v1/gears/demo/docs/DESIGN.md")
    );
    // A URL is kept as written.
    assert_eq!(
        docs.urls.openapi.as_deref(),
        Some("https://api.example.com/openapi.yaml")
    );

    let tests: Vec<(Option<&str>, Option<&str>)> = gear
        .tests
        .iter()
        .map(|t| {
            (
                t.path.as_ref().map(gearbox_ir::RelPath::as_str),
                t.url.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        tests,
        [
            (Some("gears/demo/tests"), None),
            (
                None,
                Some("https://github.com/cf/gears-rust/blob/v1/gears/demo-it")
            ),
        ]
    );

    let origin = gear.origin.as_ref().expect("origin");
    assert_eq!(origin.path.as_str(), "gears/demo");
    assert_eq!(origin.reference.as_deref(), Some("v1"));
}

#[test]
fn without_a_source_a_missing_path_is_still_a_typo() {
    let checkout = Checkout::new("typo", REPO);
    let catalogue = checkout.describe(r#"docs = docs(design = "docs/DESIGN.md"), tests = ["it"]"#);
    let missing: Vec<_> = catalogue
        .diagnostics
        .as_slice()
        .iter()
        .filter(|d| d.code == DiagnosticCode::GdlMissingDocPath)
        .map(|d| d.message.clone())
        .collect();
    assert_eq!(missing.len(), 2, "{missing:?}");
    assert!(missing.iter().any(|m| m.contains("`tests = [\"it\"]`")));
}

#[test]
fn a_source_the_checkout_does_not_have_is_reported() {
    let checkout = Checkout::new("other-remote", "https://github.com/someone/else.git");
    let catalogue = checkout.describe(&format!(
        r#"source = git(url = "{REPO}", path = "gears/demo")"#
    ));
    assert_eq!(codes(&catalogue), [DiagnosticCode::GdlSourceMismatch]);
    let message = &catalogue.diagnostics.as_slice()[0].message;
    assert!(message.contains("someone/else"), "{message}");
}

#[test]
fn a_source_path_that_is_not_where_the_gear_is_is_reported() {
    let checkout = Checkout::new("other-path", REPO);
    let catalogue = checkout.describe(&format!(
        r#"source = git(url = "{REPO}", path = "gears/elsewhere")"#
    ));
    assert_eq!(codes(&catalogue), [DiagnosticCode::GdlSourceMismatch]);
    let diagnostic = &catalogue.diagnostics.as_slice()[0];
    assert_eq!(diagnostic.severity, gearbox_ir::Severity::Warning);
    assert!(
        diagnostic.help.as_deref() == Some("write `path = \"gears/demo\"`"),
        "{diagnostic:?}"
    );
}

#[test]
fn a_source_that_agrees_with_the_checkout_is_silent() {
    let checkout = Checkout::new("agrees", "git@github.com:CF/gears-rust.git");
    let catalogue = checkout.describe(&format!(
        r#"source = git(url = "{REPO}", path = "gears/demo")"#
    ));
    assert!(codes(&catalogue).is_empty(), "{:?}", catalogue.diagnostics);
}

#[test]
fn a_checkout_of_a_fork_matches_the_canonical_source() {
    let checkout = Checkout::new("fork", "git@github.com:someone/gears-rust.git");
    let catalogue = checkout.describe(&format!(
        r#"source = git(url = "{REPO}", path = "gears/demo")"#
    ));
    assert!(codes(&catalogue).is_empty(), "{:?}", catalogue.diagnostics);
}
