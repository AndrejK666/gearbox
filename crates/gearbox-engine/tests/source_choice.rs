//! A product composed from several sources, with `use_gear(id, source = ...)`
//! deciding which source each gear comes from.
//!
//! The case this exists for: the platform's gears in one checkout, an
//! organization's own gears in another, and among them a fork of a platform
//! gear that keeps the platform's id. Both copies are listed; the product says
//! which one it builds with; and where it had to say and did not, it is told.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use gearbox_engine::{SourceRoot, load_catalogue, load_catalogue_pinned, source_pins};
use gearbox_gdl::{FileIdentity, GdlEngine};
use gearbox_ir::{
    Catalogue, Diagnostic, DiagnosticCode, GearId, ProductIntent, ProfileId, RelPath, SourceId,
    SourceKind,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A throwaway checkout holding `gears`: `(id, description, deps)`.
struct Checkout(PathBuf);

impl Checkout {
    fn new(gears: &[(&str, &str, &[&str])]) -> Self {
        let nth = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("gbx-source-choice-{}-{nth}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        for (id, description, deps) in gears {
            let crate_dir = dir.join(id);
            std::fs::create_dir_all(crate_dir.join("src")).unwrap();
            std::fs::write(
                crate_dir.join("Cargo.toml"),
                format!("[package]\nname = \"{id}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
            )
            .unwrap();
            std::fs::write(
                crate_dir.join("src/lib.rs"),
                format!(
                    "#[toolkit::gear(name = \"{id}\", deps = [{}])]\n#[derive(Default)]\npub struct G;\n",
                    deps.join(", ")
                ),
            )
            .unwrap();
            std::fs::write(
                crate_dir.join("gear.gdl"),
                format!(
                    "gear(maturity = \"stable\", description = \"{description}\", \
                     package = cargo(crate_name = \"{id}\", lib = \"{id}\", path = \".\"))\n"
                ),
            )
            .unwrap();
        }
        Self(dir)
    }

    fn open(&self, id: &str) -> SourceRoot {
        SourceRoot::open(SourceId::new(id).unwrap(), &self.0).unwrap()
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

/// A product over the sources `platform` and `org`, using `uses` verbatim.
fn product(uses: &[&str]) -> ProductIntent {
    let src = format!(
        r#"
product(
    id = "demo", version = "0.1.0",
    sources = [
        source(id = "platform", at = git(url = "https://example.com/platform.git", tag = "v1.2.0")),
        source(id = "org", at = path("../org")),
    ],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [{}],
)
"#,
        uses.join(", ")
    );
    let identity = FileIdentity {
        uri: "file:///repo/product.gdl".to_owned(),
        source: SourceId::new("product").unwrap(),
        gdl_path: RelPath::new("product.gdl").unwrap(),
        load_paths: None,
    };
    let out = GdlEngine::new().eval_product(&identity, &src);
    out.value.unwrap_or_else(|| panic!("{:?}", out.diagnostics))
}

fn resolve(catalogue: &Catalogue, intent: &ProductIntent) -> Vec<Diagnostic> {
    gearbox_engine::resolve::resolve(catalogue, intent, &ProfileId::new("dev").unwrap())
        .diagnostics
        .iter()
        .cloned()
        .collect()
}

fn errors(diagnostics: &[Diagnostic]) -> Vec<&Diagnostic> {
    diagnostics.iter().filter(|d| d.is_error()).collect()
}

fn gear(id: &str) -> GearId {
    GearId::new(id).unwrap()
}

#[test]
fn two_roots_with_distinct_gears_each_supply_their_own() {
    let platform = Checkout::new(&[("alpha", "platform alpha", &[])]);
    let org = Checkout::new(&[("beta", "org beta", &[])]);
    let roots = [platform.open("platform"), org.open("org")];
    let intent = product(&[
        r#"use_gear("alpha", source = "platform")"#,
        r#"use_gear("beta", source = "org")"#,
    ]);

    let catalogue = load_catalogue_pinned(&roots, &source_pins(&intent, &roots)).catalogue;
    assert!(
        catalogue.diagnostics.as_slice().is_empty(),
        "{:?}",
        catalogue.diagnostics
    );
    assert!(catalogue.alternatives.is_empty());
    assert_eq!(catalogue.gears[&gear("alpha")].source.as_str(), "platform");
    assert_eq!(catalogue.gears[&gear("beta")].source.as_str(), "org");

    let diagnostics = resolve(&catalogue, &intent);
    assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
}

#[test]
fn one_gear_id_in_two_roots_is_listed_twice_and_taken_from_the_named_source() {
    let platform = Checkout::new(&[("shared", "the platform's", &[])]);
    let org = Checkout::new(&[("shared", "the organization's fork", &[])]);
    let roots = [platform.open("platform"), org.open("org")];

    // Without a product: no error, both copies listed, distinguished by source.
    let listing = load_catalogue(&roots).catalogue;
    assert!(
        !listing
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::GdlCardinality),
        "a fork in another source is a layout, not a duplicate: {:?}",
        listing.diagnostics
    );
    let sources: Vec<&str> = listing
        .sources_declaring(&gear("shared"))
        .into_iter()
        .map(SourceId::as_str)
        .collect();
    assert_eq!(sources, ["platform", "org"]);
    assert_eq!(listing.alternatives.len(), 1);
    assert_eq!(listing.alternatives[0].source.as_str(), "org");
    let json = serde_json::to_value(&listing).unwrap();
    assert_eq!(
        json["alternatives"][0]["source"], "org",
        "the wire carries both copies"
    );

    // The product names the fork, although its root comes second.
    let intent = product(&[r#"use_gear("shared", source = "org")"#]);
    let catalogue = load_catalogue_pinned(&roots, &source_pins(&intent, &roots)).catalogue;
    let used = &catalogue.gears[&gear("shared")];
    assert_eq!(used.source.as_str(), "org");
    assert_eq!(used.description.as_deref(), Some("the organization's fork"));
    assert_eq!(catalogue.alternatives[0].source.as_str(), "platform");
    let diagnostics = resolve(&catalogue, &intent);
    assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");

    // And the platform's copy when that is the one named, roots reversed: the
    // choice is the product's, never the order of the roots.
    let reversed = [org.open("org"), platform.open("platform")];
    let intent = product(&[r#"use_gear("shared", source = "platform")"#]);
    let catalogue = load_catalogue_pinned(&reversed, &source_pins(&intent, &reversed)).catalogue;
    assert_eq!(catalogue.gears[&gear("shared")].source.as_str(), "platform");
    let report = gearbox_engine::validate::validate(&reversed, Some(&intent));
    assert!(!report.has_errors(), "{:?}", report.diagnostics);
}

#[test]
fn a_use_gear_that_leaves_the_source_open_is_an_error_naming_both() {
    let platform = Checkout::new(&[("shared", "the platform's", &[])]);
    let org = Checkout::new(&[("shared", "the organization's fork", &[])]);
    let roots = [platform.open("platform"), org.open("org")];
    let intent = product(&[r#"use_gear("shared")"#]);

    let catalogue = load_catalogue_pinned(&roots, &source_pins(&intent, &roots)).catalogue;
    let diagnostics = resolve(&catalogue, &intent);
    let ambiguous: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::TopologyAmbiguousGearSource)
        .collect();
    assert_eq!(ambiguous.len(), 1, "{diagnostics:?}");
    assert!(ambiguous[0].is_error());
    let message = &ambiguous[0].message;
    assert!(
        message.contains("`platform`") && message.contains("`org`"),
        "{message}"
    );
    let help = ambiguous[0].help.as_deref().unwrap_or_default();
    assert!(
        help.contains(r#"use_gear("shared", source = "platform")"#),
        "{help}"
    );

    // Validate says the same without resolving.
    let report = gearbox_engine::validate::validate(&roots, Some(&intent));
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TopologyAmbiguousGearSource),
        "{:?}",
        report.diagnostics
    );

    // With one copy in reach, an unnamed source is no question at all.
    let alone = [platform.open("platform")];
    let catalogue = load_catalogue_pinned(&alone, &source_pins(&intent, &alone)).catalogue;
    assert!(errors(&resolve(&catalogue, &intent)).is_empty());
}

#[test]
fn a_gear_pulled_in_from_two_sources_needs_a_use_gear_to_say_which() {
    let platform = Checkout::new(&[
        ("top", "uses shared", &["shared"]),
        ("shared", "platform", &[]),
    ]);
    let org = Checkout::new(&[("shared", "fork", &[])]);
    let roots = [platform.open("platform"), org.open("org")];

    let intent = product(&[r#"use_gear("top", source = "platform")"#]);
    let catalogue = load_catalogue_pinned(&roots, &source_pins(&intent, &roots)).catalogue;
    let diagnostics = resolve(&catalogue, &intent);
    let ambiguous = diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyAmbiguousGearSource)
        .unwrap_or_else(|| panic!("{diagnostics:?}"));
    assert!(
        ambiguous.message.contains("pulled into the product"),
        "{}",
        ambiguous.message
    );

    let intent = product(&[
        r#"use_gear("top", source = "platform")"#,
        r#"use_gear("shared", source = "org")"#,
    ]);
    let catalogue = load_catalogue_pinned(&roots, &source_pins(&intent, &roots)).catalogue;
    let diagnostics = resolve(&catalogue, &intent);
    assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
}

#[test]
fn naming_a_source_that_lacks_the_gear_says_where_it_is() {
    let platform = Checkout::new(&[("alpha", "platform alpha", &[])]);
    let org = Checkout::new(&[("beta", "org beta", &[])]);
    let roots = [platform.open("platform"), org.open("org")];
    let intent = product(&[r#"use_gear("alpha", source = "org")"#]);

    let catalogue = load_catalogue_pinned(&roots, &source_pins(&intent, &roots)).catalogue;
    let diagnostics = resolve(&catalogue, &intent);
    let wrong = diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyGearNotInSource)
        .unwrap_or_else(|| panic!("{diagnostics:?}"));
    assert!(
        wrong.message.contains("declared in `platform`"),
        "{}",
        wrong.message
    );
}

#[test]
fn the_same_id_twice_in_one_source_is_still_an_error() {
    let platform = Checkout::new(&[("shared", "first", &[])]);
    // A second description of the same crate, elsewhere in the same checkout.
    let again = platform.0.join("nested/again");
    std::fs::create_dir_all(&again).unwrap();
    std::fs::write(
        again.join("gear.gdl"),
        "gear(maturity = \"stable\", package = cargo(crate_name = \"shared\", lib = \"shared\", \
         path = \"../../shared\"))\n",
    )
    .unwrap();
    let catalogue = load_catalogue(&[platform.open("platform")]).catalogue;
    assert!(
        catalogue
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::GdlCardinality),
        "{:?}",
        catalogue.diagnostics
    );
    assert!(catalogue.alternatives.is_empty());
}

/// A detached checkout, as `git checkout v1.2.0` leaves it.
fn fake_git(dir: &Path, head: &str) {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/HEAD"), format!("{head}\n")).unwrap();
}

#[test]
fn a_git_source_checked_out_by_the_caller_is_locked_by_url_tag_and_commit() {
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let platform = Checkout::new(&[("alpha", "platform alpha", &[])]);
    fake_git(&platform.0, COMMIT);
    let roots = [platform.open("platform")];
    let intent = product(&[r#"use_gear("alpha", source = "platform")"#]);

    let catalogue = load_catalogue_pinned(&roots, &source_pins(&intent, &roots)).catalogue;
    let product_file = platform.0.join("product.gdl");
    let sources =
        gearbox_engine::lock_sources_declared(&roots, &catalogue, &product_file, &intent.sources);
    let locked = &sources[&SourceId::new("platform").unwrap()];
    assert_eq!(locked.kind, SourceKind::Git);
    assert_eq!(
        locked.location,
        "https://example.com/platform.git?tag=v1.2.0"
    );
    assert_eq!(locked.digest, format!("git:{COMMIT}"));

    // Through to the lock: the gear names its source, the source its commit.
    let resolution =
        gearbox_engine::resolve::resolve(&catalogue, &intent, &ProfileId::new("dev").unwrap());
    let lock =
        gearbox_engine::resolve::product::assemble(&catalogue, &intent, &resolution, sources);
    assert_eq!(lock.gears[&gear("alpha")].source.as_str(), "platform");
    assert_eq!(
        lock.sources[&SourceId::new("platform").unwrap()].digest,
        format!("git:{COMMIT}")
    );
}

#[test]
fn a_checkout_on_a_branch_is_read_through_its_ref() {
    const COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";
    let checkout = Checkout::new(&[]);
    fake_git(&checkout.0, "ref: refs/heads/main");
    std::fs::write(
        checkout.0.join(".git/packed-refs"),
        format!("# pack-refs with: peeled fully-peeled sorted\n{COMMIT} refs/heads/main\n"),
    )
    .unwrap();
    assert_eq!(
        gearbox_engine::checkout_commit(&checkout.0).as_deref(),
        Some(COMMIT)
    );
    assert_eq!(
        gearbox_engine::checkout_commit(&checkout.0.join("nowhere")),
        None
    );
}

#[test]
fn a_root_argument_may_name_its_source() {
    let (id, dir) = gearbox_engine::parse_root_arg(Path::new("org=/srv/checkouts/org"));
    assert_eq!(id.map(|i| i.as_str().to_owned()).as_deref(), Some("org"));
    assert_eq!(dir, PathBuf::from("/srv/checkouts/org"));

    // Not an id before the `=`: the whole thing is a directory.
    for plain in ["/srv/a=b", "C:\\checkouts\\org", "./x=y"] {
        let (id, dir) = gearbox_engine::parse_root_arg(Path::new(plain));
        assert!(id.is_none(), "{plain}");
        assert_eq!(dir, PathBuf::from(plain));
    }
}

#[test]
fn a_root_takes_the_id_of_the_path_source_that_points_at_it() {
    let org = Checkout::new(&[("beta", "org beta", &[])]);
    let product_dir = org.0.join("products/demo");
    std::fs::create_dir_all(&product_dir).unwrap();
    let mut roots = [org.open("gbx-checkout")];
    let declared = std::iter::once((
        SourceId::new("org").unwrap(),
        gearbox_ir::SourceDecl::Path {
            at: "../..".to_owned(),
            crates: None,
            declared_at: None,
        },
    ))
    .collect();

    gearbox_engine::adopt_product_source_ids(&mut roots, &[false], &declared, &product_dir);
    assert_eq!(roots[0].id.as_str(), "gbx-checkout", "an explicit id stays");
    gearbox_engine::adopt_product_source_ids(&mut roots, &[true], &declared, &product_dir);
    assert_eq!(roots[0].id.as_str(), "org");
}
