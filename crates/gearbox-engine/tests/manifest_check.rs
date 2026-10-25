//! GBX0209: declared crate identity against the crate's own `Cargo.toml`.
//!
//! Two halves. The real tree is the positive case -- 25 `cargo(...)` blocks
//! across 14 descriptions, all correct, so the check has to stay silent on it.
//! The negative cases are built in a temporary tree, because there is no wrong
//! declaration in the repository to point at and inventing one in `gears-rust`
//! would mean committing a broken description to prove a check works.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;

use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{DiagnosticCode, Range, SourceId};

/// The `gear.gdl` text `Tree::described_as` writes, exposed so a test can find
/// the line a diagnostic should anchor on.
fn gear_gdl(crate_name: &str, lib: &str) -> String {
    format!(
        r#"
gear(
    maturity = "stable",
    name = "Thing",
    description = "A thing.",
    category = "core-functionality",
    package = cargo(crate_name = "{crate_name}", lib = "{lib}", path = "."),
)
"#
    )
}

/// The zero-based line holding `needle`, for asserting a diagnostic's anchor.
fn line_of(text: &str, needle: &str) -> u32 {
    let found: Vec<u32> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle))
        .map(|(i, _)| u32::try_from(i).expect("fixtures are short"))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "`{needle}` must appear on exactly one line of the fixture, found {found:?}"
    );
    found[0]
}

#[path = "common/corpus.rs"]
mod corpus_support;

fn gears_rust() -> Option<PathBuf> {
    corpus_support::root()
}

/// A throwaway source root holding one gear description and one crate.
struct Tree(PathBuf);

impl Tree {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "gearbox-gbx0209-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("thing/src")).unwrap();
        Self(dir)
    }

    /// Write the crate: a manifest plus the minimum a gear needs to project.
    fn crate_with(&self, manifest: &str) -> &Self {
        std::fs::write(self.0.join("thing/Cargo.toml"), manifest).unwrap();
        std::fs::write(
            self.0.join("thing/src/lib.rs"),
            r#"
            #[toolkit::gear(name = "thing", capabilities = [rest])]
            #[derive(Default)]
            pub struct Thing;
            "#,
        )
        .unwrap();
        self
    }

    fn described_as(&self, crate_name: &str, lib: &str) -> &Self {
        std::fs::write(self.0.join("thing/gear.gdl"), gear_gdl(crate_name, lib)).unwrap();
        self
    }

    fn diagnostics(&self) -> Vec<(DiagnosticCode, String)> {
        let source = SourceRoot::open(SourceId::new("fixture").unwrap(), self.0.clone()).unwrap();
        load_catalogue(&[source])
            .catalogue
            .diagnostics
            .iter()
            .map(|d| (d.code, d.message.clone()))
            .collect()
    }

    fn full_diagnostics(&self) -> Vec<gearbox_ir::Diagnostic> {
        let source = SourceRoot::open(SourceId::new("fixture").unwrap(), self.0.clone()).unwrap();
        load_catalogue(&[source])
            .catalogue
            .diagnostics
            .iter()
            .cloned()
            .collect()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn only_0209(diagnostics: &[(DiagnosticCode, String)]) -> Vec<&str> {
    diagnostics
        .iter()
        .filter(|(code, _)| *code == DiagnosticCode::ValidateLibIdentMismatch)
        .map(|(_, message)| message.as_str())
        .collect()
}

#[test]
fn the_real_tree_declares_every_crate_correctly() {
    // The baseline that makes the check worth having: it must not fire on the
    // 25 `cargo(...)` blocks that are right.
    let Some(root) = gears_rust() else {
        eprintln!("skipping: ../gears-rust not present");
        return;
    };
    let source = SourceRoot::open(SourceId::new("gears-rust").unwrap(), root).unwrap();
    let scan = load_catalogue(&[source]);
    let wrong: Vec<&str> = scan
        .catalogue
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::ValidateLibIdentMismatch)
        .map(|d| d.message.as_str())
        .collect();
    assert!(
        wrong.is_empty(),
        "GBX0209 fired on the real tree: {wrong:?}"
    );
}

#[test]
fn a_wrong_lib_ident_is_reported() {
    let tree = Tree::new("wrong-lib");
    tree.crate_with("[package]\nname = \"cf-thing\"\nversion = \"0.1.0\"\n")
        .described_as("cf-thing", "thing");
    let diagnostics = tree.diagnostics();
    let messages = only_0209(&diagnostics);
    assert_eq!(messages.len(), 1, "got {diagnostics:?}");
    assert!(
        messages[0].contains("links as `cf_thing`"),
        "the message has to name the identifier the crate really uses: {}",
        messages[0]
    );

    // Anchored on the `cargo(...)` call that declared the wrong identity, not
    // the file: a description naming several crates must not blur which one
    // is wrong.
    let full = tree.full_diagnostics();
    let d = full
        .iter()
        .find(|d| d.code == DiagnosticCode::ValidateLibIdentMismatch)
        .expect("GBX0209");
    let location = d.location.as_ref().expect("carries a location");
    assert_ne!(
        location.range,
        Range::whole_file(),
        "must anchor on the cargo(...) call, not the file: {d:#?}"
    );
    assert_eq!(
        location.range.start.line,
        line_of(&gear_gdl("cf-thing", "thing"), "cargo("),
        "must be anchored on the cargo(...) line"
    );
}

#[test]
fn the_advice_says_why_when_there_is_no_lib_section() {
    // The trap `cf-api-contracts` sits in: a reader derives the identifier from
    // the directory, Cargo derives it from the package name. Naming the right
    // value is not enough -- the advice has to say where it came from, or the
    // same mistake is made again on the next crate.
    let tree = Tree::new("derived");
    tree.crate_with("[package]\nname = \"cf-thing\"\nversion = \"0.1.0\"\n")
        .described_as("cf-thing", "thing");
    let source = SourceRoot::open(SourceId::new("fixture").unwrap(), tree.0.clone()).unwrap();
    let scan = load_catalogue(&[source]);
    let help = scan
        .catalogue
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ValidateLibIdentMismatch)
        .and_then(|d| d.help.clone())
        .expect("GBX0209 carries help");
    assert!(help.contains("no `[lib]` section"), "got: {help}");
    assert!(help.contains("cf_thing"), "got: {help}");
}

#[test]
fn an_explicit_lib_name_changes_the_advice() {
    let tree = Tree::new("explicit");
    tree.crate_with(
        "[package]\nname = \"cf-thing\"\nversion = \"0.1.0\"\n\n[lib]\nname = \"thing_lib\"\n",
    )
    .described_as("cf-thing", "thing");
    let source = SourceRoot::open(SourceId::new("fixture").unwrap(), tree.0.clone()).unwrap();
    let scan = load_catalogue(&[source]);
    let help = scan
        .catalogue
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ValidateLibIdentMismatch)
        .and_then(|d| d.help.clone())
        .expect("GBX0209 carries help");
    assert!(
        help.contains("`[lib] name`"),
        "with an explicit section the description simply copied the wrong \
         string, and the advice should say so: {help}"
    );
}

#[test]
fn a_wrong_crate_name_is_reported_separately() {
    let tree = Tree::new("wrong-crate");
    tree.crate_with("[package]\nname = \"cf-thing\"\nversion = \"0.1.0\"\n")
        .described_as("thing", "cf_thing");
    let diagnostics = tree.diagnostics();
    let messages = only_0209(&diagnostics);
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("crate_name"), "got {}", messages[0]);
    assert!(messages[0].contains("cf-thing"), "got {}", messages[0]);
}

#[test]
fn both_wrong_gives_two_diagnostics() {
    // Not one combined complaint: they are two independent generated lines --
    // a `[dependencies]` entry and a `use ... as _;` -- and fixing one does not
    // fix the other.
    let tree = Tree::new("both");
    tree.crate_with("[package]\nname = \"cf-thing\"\nversion = \"0.1.0\"\n")
        .described_as("thing", "thing");
    let diagnostics = tree.diagnostics();
    assert_eq!(only_0209(&diagnostics).len(), 2);
}

#[test]
fn a_correct_declaration_is_silent() {
    let tree = Tree::new("correct");
    tree.crate_with("[package]\nname = \"cf-thing\"\nversion = \"0.1.0\"\n")
        .described_as("cf-thing", "cf_thing");
    let diagnostics = tree.diagnostics();
    assert!(only_0209(&diagnostics).is_empty());
}

#[test]
fn a_readable_crate_with_no_manifest_is_reported() {
    // The identity check silently did not run here: `src/` reads fine, so the
    // crate scan has nothing to say, and skipping the manifest failure left a
    // crate with no `Cargo.toml` passing validation.
    let tree = Tree::new("no-manifest");
    std::fs::write(
        tree.0.join("thing/src/lib.rs"),
        "#[toolkit::gear(name = \"thing\", capabilities = [rest])] pub struct Thing;",
    )
    .unwrap();
    tree.described_as("cf-thing", "cf_thing");
    let diagnostics = tree.diagnostics();
    let reported = only_0209(&diagnostics);
    assert_eq!(reported.len(), 1, "got {diagnostics:?}");
    assert!(
        reported[0].contains("Cargo.toml"),
        "the message must name what could not be read: {}",
        reported[0]
    );
}

#[test]
fn an_unreadable_crate_is_not_reported_twice() {
    // A `path` that leads nowhere: the crate scan already complains, with the
    // advice about `path` that belongs to it. Adding GBX0209 on top would make
    // one mistake look like two.
    let tree = Tree::new("no-crate");
    std::fs::remove_dir_all(tree.0.join("thing/src")).unwrap();
    tree.described_as("cf-thing", "cf_thing");
    let diagnostics = tree.diagnostics();
    assert!(only_0209(&diagnostics).is_empty(), "got {diagnostics:?}");
}
