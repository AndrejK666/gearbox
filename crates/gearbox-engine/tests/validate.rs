//! `gearbox validate`: the product-to-catalogue join, and telling the two
//! failure modes apart.
//!
//! The distinction under test is the whole reason GBX0208 exists as its own
//! code. A selected gear missing from the catalogue may be one nobody has
//! described yet -- and that wants "here is the `gear.gdl` to write", not "check
//! your spelling".

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};

use gearbox_engine::{SourceRoot, validate::validate};
use gearbox_ir::{DiagnosticCode, Severity, SourceId};

#[path = "common/corpus.rs"]
mod corpus_support;

fn gears_rust() -> Option<PathBuf> {
    corpus_support::root()
}

fn roots() -> Option<Vec<SourceRoot>> {
    let root = gears_rust()?;
    Some(vec![
        SourceRoot::open(SourceId::new("gears-rust").unwrap(), root).unwrap(),
    ])
}

macro_rules! require_tree {
    ($binding:ident) => {
        let Some($binding) = roots() else {
            eprintln!("skipping: ../gears-rust not present");
            return;
        };
    };
}

/// The real product with extra `use_gear` lines spliced in.
///
/// Built from the repository's own `product.gdl` rather than from a minimal
/// fixture, because the join has to work against a description that also
/// declares profiles, bindings and cluster scopes -- a fixture with one gear in
/// it would not exercise the same code.
fn product_with(extra: &[&str]) -> (tempdir::Dir, gearbox_ir::ProductIntent) {
    let original = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../products/payments-demo/product.gdl")
        .canonicalize()
        .expect("the repository's product description must exist");
    let text = std::fs::read_to_string(&original)
        .expect("the repository's product description must be readable");
    let anchor = "        use_gear(\"api-gateway\", source = \"gears-rust\"),";
    assert!(text.contains(anchor), "the product's shape changed");

    let mut inserted = String::from(anchor);
    for line in extra {
        inserted.push_str("\n        ");
        inserted.push_str(line);
    }
    let patched = text.replace(anchor, &inserted);

    let dir = tempdir::Dir::new("gbx-validate");
    let path = dir.path().join("product.gdl");
    std::fs::write(&path, patched).unwrap();

    // The product's `path("../../../gears-rust")` is relative to its own
    // directory, and the copy sits elsewhere -- but `sources` is not resolved
    // yet, so the join under test is unaffected. Stated so the next reader does
    // not spend time wondering.
    let scan = gearbox_engine::product::load_product(&path, None);
    let intent = scan.intent.expect("the patched product still evaluates");
    (dir, intent)
}

/// A minimal temporary directory that cleans itself up.
mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Distinct per call, which a clock reading is not.
    ///
    /// The name used to end in `SystemTime::now().as_nanos()`. Three tests in this
    /// file build a temp product concurrently, and two `now()` calls in the same
    /// tick produced *one* directory: one test then read the `product.gdl` the
    /// other had written, so a test asserting zero errors saw the deliberate
    /// `api-gatewey` typo belonging to its neighbour. It surfaced when unrelated
    /// work changed how long a catalogue load takes, which is how a race
    /// announces itself -- by moving.
    static NTH: AtomicUsize = AtomicUsize::new(0);

    pub struct Dir(PathBuf);

    impl Dir {
        pub fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "{label}-{}-{}",
                std::process::id(),
                NTH.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
}

#[test]
fn the_real_tree_has_no_unexpected_diagnostics() {
    require_tree!(roots);
    let report = validate(&roots, None);
    let problems: Vec<String> = report
        .diagnostics
        .iter()
        // The local corpus may be a fork; the CLI still reports its remote URL.
        .filter(|d| d.code != DiagnosticCode::GdlSourceMismatch)
        .filter(|d| d.severity != Severity::Hint)
        .map(|d| format!("[{}] {}", d.code, d.message))
        .collect();
    assert!(problems.is_empty(), "{problems:#?}");
    assert!(!report.has_errors());
}

#[test]
fn the_real_product_selects_only_described_gears() {
    require_tree!(roots);
    let (_dir, intent) = product_with(&[]);
    let report = validate(&roots, Some(&intent));
    assert_eq!(report.error_count(), 0, "{:#?}", report.diagnostics);
}

/// A source root holding one undescribed crate, `widgets/widget`.
///
/// The corpus used to supply this case for free -- `bss-ledger` had an attribute
/// and no description -- and stopped the day every implemented gear was
/// described. A fixture keeps the claim without waiting for somebody to leave a
/// crate undescribed, and it keeps the part that mattered: the package and the
/// library identifier differ from the directory, so a help text that derived
/// them from the path would be caught.
fn undescribed_root() -> (tempdir::Dir, SourceRoot) {
    let dir = tempdir::Dir::new("gbx-undescribed");
    let krate = dir.path().join("widgets/widget");
    std::fs::create_dir_all(krate.join("src")).unwrap();
    std::fs::write(
        krate.join("Cargo.toml"),
        "[package]\nname = \"cf-acme-widget\"\nversion = \"0.1.0\"\n\n[lib]\nname = \"acme_widget\"\n",
    )
    .unwrap();
    std::fs::write(
        krate.join("src/gear.rs"),
        "#[toolkit::gear(name = \"widget\", capabilities = [rest])]\npub struct Widget;\n",
    )
    .unwrap();
    let root = SourceRoot::open(
        SourceId::new("fixture").unwrap(),
        dir.path().canonicalize().unwrap(),
    )
    .unwrap();
    (dir, root)
}

#[test]
fn a_gear_that_exists_in_rust_but_has_no_description_is_gbx0208() {
    require_tree!(roots);
    let (_fixture, extra) = undescribed_root();
    let roots: Vec<SourceRoot> = roots.into_iter().chain([extra]).collect();
    let (_dir, intent) = product_with(&["use_gear(\"widget\", source = \"gears-rust\"),"]);
    let report = validate(&roots, Some(&intent));

    let found = report
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ValidateMissingDescription)
        .expect("GBX0208");
    assert!(
        found.message.replace('\\', "/").contains("widgets/widget"),
        "the crate has to be named, or the operator cannot act: {}",
        found.message
    );

    let help = found.help.as_deref().expect("GBX0208 carries help");
    // The two fields that cannot be guessed, read from the manifest: neither
    // `cf-acme-widget` nor `acme_widget` is derivable from `widgets/widget`.
    assert!(help.contains("cf-acme-widget"), "got: {help}");
    assert!(help.contains("acme_widget"), "got: {help}");
    assert!(
        help.contains("do not restate"),
        "the skeleton must not invite restating projected facts: {help}"
    );

    let evidence = found
        .evidence
        .as_deref()
        .expect("GBX0208 cites the attribute");
    assert!(
        evidence.replace('\\', "/").contains("/src/"),
        "the evidence path is joined onto the crate directory and must include \
         `src/`, or it points at a file that does not exist: {evidence}"
    );
}

#[test]
fn a_gear_nothing_declares_is_gbx0301_with_a_spelling_hint() {
    require_tree!(roots);
    let (_dir, intent) = product_with(&["use_gear(\"api-gatewey\", source = \"gears-rust\"),"]);
    let report = validate(&roots, Some(&intent));

    let found = report
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyUnknownGear)
        .expect("GBX0301");
    assert!(
        report
            .diagnostics
            .iter()
            .all(|d| d.code != DiagnosticCode::ValidateMissingDescription),
        "a typo must not be reported as an undescribed gear: no crate declares it"
    );
    let help = found.help.as_deref().expect("help");
    assert!(help.contains("api-gateway"), "got: {help}");
}

#[test]
fn a_far_away_name_gets_no_guess() {
    // A hint is a guess, and a guess that is wrong sends the reader down the
    // wrong path. Above the distance threshold there is simply no suggestion.
    require_tree!(roots);
    let (_dir, intent) = product_with(&["use_gear(\"zzz-nonesuch\", source = \"gears-rust\"),"]);
    let report = validate(&roots, Some(&intent));
    let help = report
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyUnknownGear)
        .and_then(|d| d.help.clone())
        .expect("GBX0301 help");
    assert!(
        !help.contains("the catalogue has"),
        "nothing is close enough to suggest: {help}"
    );
    assert!(help.contains("gearbox catalogue"), "got: {help}");
}

#[test]
fn a_described_gear_is_never_reported_as_undescribed() {
    // The guard on the search: it skips crates that have a `gear.gdl`. Without
    // that, a gear present in the catalogue could still be "found" by the
    // filesystem walk and reported.
    require_tree!(roots);
    let report = validate(&roots, None);
    assert!(
        gearbox_engine::undescribed::find(&roots, &gearbox_ir::GearId::new("api-gateway").unwrap())
            .is_none(),
        "api-gateway is described, so the search must not offer it"
    );
    assert!(!report.has_errors());
}

#[test]
fn the_search_finds_a_gear_in_a_crate_declaring_several() {
    // `gears/mini-chat/mini-chat` carries three `#[toolkit::gear]` attributes.
    // `locate_gear_attribute` would refuse that crate as ambiguous, which is
    // right for projection and wrong for a search -- hence the separate path.
    require_tree!(roots);
    let mini_chat = gearbox_ir::GearId::new("mini-chat").unwrap();
    let Some(found) = gearbox_engine::undescribed::find(&roots, &mini_chat) else {
        eprintln!("skipping: mini-chat not present in this checkout");
        return;
    };
    assert!(
        found.crate_dir.contains("mini-chat"),
        "got {}",
        found.crate_dir
    );
}
