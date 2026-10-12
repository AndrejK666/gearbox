//! Several source roots, and what happens when two of them claim one gear.
//!
//! Multiple roots stopped being hypothetical when the Studio began deriving them
//! from a product's own `sources` list: a product that names two checkouts gets
//! two roots, and nobody had to decide anything for that to happen. The question
//! this file pins down is the one that decision created -- if both roots declare
//! the same `GearId`, which one is in the catalogue, and does anyone say so?
//!
//! See ADR `cpt-gearbox-adr-multiple-source-roots`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{GearId, SourceId};

/// The same gear id from two different descriptions.
///
/// Only `description` differs, so the assertion below can tell which root's copy
/// ended up in the catalogue without depending on anything derived.
fn gear_gdl(description: &str) -> String {
    format!(
        r#"
gear(
    maturity = "stable",
    name = "Demo",
    description = "{description}",
    category = "platform",
    visibility = "internal",
    package = cargo(crate_name = "demo", lib = "demo", path = "."),
)
"#
    )
}

const GEAR_RS: &str = r#"
#[toolkit::gear(
    name = "demo",
    capabilities = [stateless],
    lifecycle(entry = "serve")
)]
pub struct DemoGear;
"#;

/// A counter, not a timestamp.
///
/// Two calls inside one clock tick produced the same directory name when this was
/// `SystemTime::now().as_nanos()`, so one test read a neighbour's doctored files.
/// That bug existed in `validate.rs` for a while and only surfaced when an
/// unrelated change altered the timing, which is the argument for never naming a
/// fixture after the clock.
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// One root holding one gear whose description is `description`.
fn root(description: &str) -> PathBuf {
    let nth = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("gbx-source-roots-{}-{nth}", std::process::id()));
    drop(std::fs::remove_dir_all(&root));
    let crate_dir = root.join("demo");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(crate_dir.join("gear.gdl"), gear_gdl(description)).unwrap();
    std::fs::write(crate_dir.join("src/lib.rs"), GEAR_RS).unwrap();
    root
}

fn open(id: &str, path: &Path) -> SourceRoot {
    SourceRoot::open(SourceId::new(id).unwrap(), path).unwrap()
}

#[test]
fn two_roots_declaring_one_gear_keep_the_first_and_list_the_other() {
    let first = root("from the first root");
    let second = root("from the second root");
    let scan = load_catalogue(&[open("alpha", &first), open("beta", &second)]);

    // One entry in `gears`: the id is the identity every join uses. Without a
    // product to choose, the first root's copy is the one there.
    let id = GearId::new("demo").unwrap();
    let kept = scan
        .catalogue
        .gear(&id)
        .expect("the gear is in the catalogue");
    assert_eq!(
        kept.description.as_deref(),
        Some("from the first root"),
        "the first root declared it, so the first root's copy is the one kept"
    );
    assert_eq!(
        kept.source.as_str(),
        "alpha",
        "and it is recorded as coming from the root it was read from"
    );

    // The other copy is listed, not reported. Two sources declaring one id is
    // an organization's fork of a platform gear, which a product chooses
    // between with `use_gear(id, source = ...)` (tests/source_choice.rs); the
    // error is reserved for a product that had to choose and did not (GBX0325).
    assert!(
        !scan
            .catalogue
            .diagnostics
            .iter()
            .any(|d| d.code.as_str() == "GBX0105"),
        "{:?}",
        scan.catalogue.diagnostics
    );
    let other: Vec<(&str, Option<&str>)> = scan
        .catalogue
        .alternatives
        .iter()
        .map(|g| (g.source.as_str(), g.description.as_deref()))
        .collect();
    assert_eq!(other, [("beta", Some("from the second root"))]);
}

#[test]
fn two_roots_with_distinct_gears_both_load() {
    // The ordinary case, asserted so the rule above cannot be implemented as
    // "ignore every root after the first".
    let first = root("only in the first");
    let second = root("only in the second");
    // Rename the second root's gear so the ids differ.
    let renamed = second.join("other");
    std::fs::create_dir_all(renamed.join("src")).unwrap();
    std::fs::write(
        renamed.join("gear.gdl"),
        gear_gdl("only in the second")
            .replace("crate_name = \"demo\"", "crate_name = \"other\"")
            .replace("lib = \"demo\"", "lib = \"other\""),
    )
    .unwrap();
    std::fs::write(
        renamed.join("src/lib.rs"),
        GEAR_RS
            .replace("name = \"demo\"", "name = \"other\"")
            .replace("DemoGear", "OtherGear"),
    )
    .unwrap();
    drop(std::fs::remove_dir_all(second.join("demo")));

    let scan = load_catalogue(&[open("alpha", &first), open("beta", &second)]);
    assert!(
        scan.catalogue.gear(&GearId::new("demo").unwrap()).is_some(),
        "the first root's gear is missing"
    );
    assert!(
        scan.catalogue
            .gear(&GearId::new("other").unwrap())
            .is_some(),
        "the second root's gear is missing; got {:?}",
        scan.catalogue.gears.keys().collect::<Vec<_>>()
    );
}

/// Which root owns a path is the same question the loader answers, so one
/// function answers it.
///
/// **Nested roots, where the two possible answers differ.** `load_catalogue`
/// walks the roots in order and keeps the first declaration, so the root that
/// owns a description is the earliest one containing it -- and the `load()`
/// boundary a consumer evaluates that file against has to be the same one, or it
/// judges the file by a boundary no catalogue entry uses. `gearbox-rpc` used to
/// pick the deepest match from its own copy of the rule, which underlined a
/// `load("//...")` in the editor that loaded clean from disk.
#[test]
fn the_owning_root_is_the_first_one_that_contains_the_path() {
    let outer = root("in the outer root");
    let inner = outer.join("demo");
    let description = inner.join("gear.gdl");

    let listed = [open("outer", &outer), open("inner", &inner)];
    let owner = gearbox_engine::owning_source_root(&listed, &description.canonicalize().unwrap())
        .expect("the description is inside both roots");
    assert_eq!(
        owner.id.as_str(),
        "outer",
        "the earliest root in the list is the one the catalogue attributes it to"
    );

    // And it is the same answer the loader gives, which is the only reason this
    // function is in the engine rather than in each caller.
    let scan = load_catalogue(&listed);
    let kept = scan
        .catalogue
        .gear(&GearId::new("demo").unwrap())
        .expect("the gear loads");
    assert_eq!(
        kept.source.as_str(),
        owner.id.as_str(),
        "the function and the loader must not disagree about who owns a description"
    );

    // Reversed, the answer reverses with it: it is a property of the list.
    let reversed = [open("inner", &inner), open("outer", &outer)];
    assert_eq!(
        gearbox_engine::owning_source_root(&reversed, &description.canonicalize().unwrap())
            .map(|root| root.id.as_str().to_owned()),
        Some("inner".to_owned())
    );

    // A path under no root has no owner, which is what a file opened outside the
    // corpus is.
    assert!(
        gearbox_engine::owning_source_root(&listed, Path::new("/elsewhere/gear.gdl")).is_none()
    );
}
