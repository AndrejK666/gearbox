//! Explicit GDL dependencies are checked mirrors, never overrides of Rust.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]

use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{Catalogue, DiagnosticCode, GearId, SourceId};
use std::sync::atomic::{AtomicUsize, Ordering};

fn catalogue(declared: &str, rust_deps: &str) -> Catalogue {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "gbx-deps-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let gear = root.join("demo");
    std::fs::create_dir_all(gear.join("src")).unwrap();
    std::fs::write(
        gear.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(gear.join("gear.gdl"), format!(
        "gdl_version = \"1.2\"\ngear(maturity = \"preview\", package = cargo(crate_name = \"demo\", lib = \"demo\"), {declared})"
    )).unwrap();
    std::fs::write(
        gear.join("src/lib.rs"),
        format!("#[toolkit::gear(name = \"demo\", deps = [{rust_deps}])]\npub struct Demo;"),
    )
    .unwrap();
    let source = SourceRoot::open(SourceId::new("fixture").unwrap(), &root).unwrap();
    let result = load_catalogue(&[source]).catalogue;
    std::fs::remove_dir_all(root).unwrap();
    result
}

fn mismatches(catalogue: &Catalogue) -> usize {
    catalogue
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::ValidateDependencyMismatch)
        .count()
}

#[test]
fn matching_dependency_sets_allow_any_order() {
    let result = catalogue(
        "deps = [\"types-registry\", \"authn-resolver\"],",
        "authn_resolver, types_registry",
    );
    assert_eq!(mismatches(&result), 0);
    assert_eq!(
        result
            .gear(&GearId::new("demo").unwrap())
            .unwrap()
            .colocated_deps
            .len(),
        2
    );
}

#[test]
fn missing_and_extra_dependencies_are_errors_without_overriding_rust() {
    for declared in [
        "deps = [],",
        "deps = [\"authn-resolver\"],",
        "deps = [\"types-registry\", \"extra\"],",
    ] {
        let result = catalogue(declared, "types_registry");
        assert_eq!(mismatches(&result), 1, "{declared}");
        let diagnostic = result
            .diagnostics
            .iter()
            .find(|d| d.code == DiagnosticCode::ValidateDependencyMismatch)
            .unwrap();
        assert!(diagnostic.severity.is_error());
        assert!(diagnostic.message.contains("GDL") && diagnostic.message.contains("Rust"));
        let actual = &result
            .gear(&GearId::new("demo").unwrap())
            .unwrap()
            .colocated_deps;
        assert_eq!(
            actual.iter().map(GearId::as_str).collect::<Vec<_>>(),
            ["types-registry"]
        );
    }
}

#[test]
fn empty_and_legacy_omitted_declarations_remain_distinct() {
    assert_eq!(mismatches(&catalogue("deps = [],", "")), 0);
    assert_eq!(mismatches(&catalogue("", "types_registry")), 0);
}
