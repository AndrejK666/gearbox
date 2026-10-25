//! Gear metadata: categories, documents, GTS types.
//!
//! All three came from wanting to absorb the `gear.toml` files the platform team
//! committed. Two of them (`docs`, `gts_types`) are new; `category` existed but
//! as a free string, and the twelve descriptions in the slice all held a value no
//! gear in the platform uses.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{Catalogue, GearId, Range, Severity, SourceId};

#[path = "common/corpus.rs"]
mod corpus_support;

fn gears_rust() -> Option<PathBuf> {
    corpus_support::root()
}

fn catalogue() -> Option<Catalogue> {
    let root = gears_rust()?;
    let source = SourceRoot::open(SourceId::new("gears-rust").unwrap(), root)
        .unwrap_or_else(|error| panic!("the discovered corpus must be readable: {error}"));
    Some(load_catalogue(&[source]).catalogue)
}

macro_rules! require {
    () => {
        match catalogue() {
            Some(c) => c,
            None => {
                eprintln!("skipping: ../gears-rust not present");
                return;
            }
        }
    };
}

fn gear<'a>(c: &'a Catalogue, id: &str) -> &'a gearbox_ir::GearDescriptor {
    c.gear(&GearId::new(id).unwrap())
        .unwrap_or_else(|| panic!("no gear `{id}`"))
}

// ---------------------------------------------------------------- category

/// The categories the platform's own `gear.toml` files carry, for the slice gears
/// that have one.
///
/// Written down here rather than read from disk on purpose: `gear.toml` landed on
/// `main` after this checkout, so the values were taken from the repository and
/// pinned. The files are gone now -- folded into each `gear.gdl` (ADR-0002,
/// Amendment 2026-10-02) -- and the pins are what proves the move kept them.
/// If they change, this test is where the disagreement surfaces.
const PLATFORM_CATEGORIES: &[(&str, &str)] = &[
    ("api-gateway", "api-ingress"),
    ("authn-resolver", "core-platform-integration"),
    ("cluster", "serverless"),
    ("service-discovery", "core-functionality"),
    ("grpc-hub", "core-functionality"),
    ("tenant-resolver", "core-platform-integration"),
    ("types-registry", "core-functionality"),
];

#[test]
fn categories_match_the_platforms_own_gear_toml() {
    let c = require!();
    for (id, expected) in PLATFORM_CATEGORIES {
        assert_eq!(
            gear(&c, id).category.as_deref(),
            Some(*expected),
            "`{id}` must carry the category the platform assigned it, not one we invented"
        );
    }
}

#[test]
fn no_gear_in_the_slice_uses_an_unknown_category() {
    // The twelve that previously said "platform" -- a value no gear in the
    // platform uses -- are what this guards against coming back.
    let c = require!();
    let unknown: Vec<String> = c
        .diagnostics
        .iter()
        .filter(|d| d.code == gearbox_ir::DiagnosticCode::GdlUnknownCategory)
        .map(|d| d.message.clone())
        .collect();
    assert!(unknown.is_empty(), "{unknown:#?}");
}

#[test]
fn an_unknown_category_warns_rather_than_failing() {
    // Warning, not error: the taxonomy is visibly unsettled (`cluster` is filed
    // under `serverless`), so refusing would claim the set is closed.
    let code = gearbox_ir::DiagnosticCode::GdlUnknownCategory;
    assert_eq!(code.default_severity(), Severity::Warning);
    assert!(!code.requires_evidence());
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

const CATEGORY_GEAR_RS: &str = r#"
#[toolkit::gear(name = "demo", capabilities = [system])]
pub struct DemoGear;
"#;

const CATEGORY_MANIFEST: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
     [lib]\nname = \"demo\"\npath = \"src/lib.rs\"\n";

/// The `gear.gdl` text for a gear declaring `category = "bogus"`.
const UNKNOWN_CATEGORY_GEAR_GDL: &str = r#"
gear(
    maturity = "stable",
    name = "Demo",
    description = "d",
    category = "bogus",
    visibility = "internal",
    package = cargo(crate_name = "demo", lib = "demo", path = "."),
)
"#;

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

/// An unknown `category` points at the `gear(...)` call that declared it, not
/// at line 1: `category` is an argument of `gear(...)`, not a call of its own.
#[test]
fn an_unknown_category_is_anchored_on_its_gear_call() {
    let nth = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("gbx-category-{}-{nth}", std::process::id()));
    drop(std::fs::remove_dir_all(&root));
    let crate_dir = root.join("demo");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(crate_dir.join("gear.gdl"), UNKNOWN_CATEGORY_GEAR_GDL).unwrap();
    std::fs::write(crate_dir.join("Cargo.toml"), CATEGORY_MANIFEST).unwrap();
    std::fs::write(crate_dir.join("src/lib.rs"), CATEGORY_GEAR_RS).unwrap();

    let source = SourceRoot::open(SourceId::new("demo").unwrap(), &root).unwrap();
    let catalogue = load_catalogue(&[source]).catalogue;
    drop(std::fs::remove_dir_all(&root));

    let d = catalogue
        .diagnostics
        .iter()
        .find(|d| d.code == gearbox_ir::DiagnosticCode::GdlUnknownCategory)
        .expect("GBX0108");
    let location = d.location.as_ref().expect("carries a location");
    assert_ne!(
        location.range,
        Range::whole_file(),
        "must anchor on the gear(...) call, not the file: {d:#?}"
    );
    assert_eq!(
        location.range.start.line,
        line_of(UNKNOWN_CATEGORY_GEAR_GDL, "gear("),
        "must be anchored on the gear(...) line"
    );
}

// ---------------------------------------------------------------- docs

#[test]
fn documents_are_found_one_level_above_the_crate() {
    // The whole reason the search climbs: the platform keeps documents at
    // `gears/<name>/docs/` while a `gear.gdl` sits in a crate directory below.
    let c = require!();
    let docs = gear(&c, "cluster").docs.as_ref().expect("cluster has docs");
    assert_eq!(
        docs.prd.as_ref().map(gearbox_ir::RelPath::as_str),
        Some("gears/system/cluster/docs/PRD.md")
    );
    assert_eq!(
        docs.design.as_ref().map(gearbox_ir::RelPath::as_str),
        Some("gears/system/cluster/docs/DESIGN.md")
    );
    assert!(!docs.adr.is_empty(), "cluster has ADRs");
}

#[test]
fn a_gear_with_its_own_docs_directory_uses_that_one() {
    // Order matters: beside the description before its parent.
    let c = require!();
    let docs = gear(&c, "oidc-authn-plugin")
        .docs
        .as_ref()
        .expect("this plugin keeps its own docs");
    assert!(
        docs.prd
            .as_ref()
            .is_some_and(|p| p.as_str().contains("oidc-authn-plugin/docs/")),
        "got {:?}",
        docs.prd
    );
}

#[test]
fn a_gear_with_no_documents_reports_none_rather_than_empty() {
    // Absence is ordinary, not a gap, so there is no diagnostic and no empty
    // block to render.
    let c = require!();
    assert!(gear(&c, "grpc-hub").docs.is_none());
}

#[test]
fn an_openapi_spec_is_claimed_only_by_the_gears_that_have_one() {
    // **This assertion got stronger by accident, and the accident is worth
    // keeping.** It used to read `claimed.is_empty()`, on the grounds that the
    // four platform gears checking a spec in were none of them in the slice --
    // so the only thing it could catch was a convention matching too eagerly.
    //
    // Describing `credstore` and `resource-group` put two of those four in the
    // slice, and they do have one: `gears/credstore/docs/api/openapi.yaml` and
    // `gears/system/resource-group/docs/openapi.yaml`. So the check now proves
    // both halves at once -- the convention finds a real spec, and invents one
    // for none of the others. Describing the rest of the corpus added two more
    // that are real: `gears/chat-engine/docs/openapi.json` and
    // `gears/mini-chat/docs/openapi.json` -- and none for the other thirty-nine
    // gears, mini-chat's two co-located plugins included. mini-chat's spec was
    // later removed from the corpus (gears-rust 00e182646), and its claim with
    // it: the convention follows the file, not a memory of it.
    let c = require!();
    let claimed: Vec<&str> = c
        .gears
        .values()
        .filter(|g| g.docs.as_ref().is_some_and(|d| d.openapi.is_some()))
        .map(|g| g.id.as_str())
        .collect();
    assert_eq!(
        claimed,
        ["chat-engine", "credstore", "event-broker", "resource-group"],
        "openapi claims moved"
    );
}

// ---------------------------------------------------------------- gts

#[test]
fn a_gts_type_is_attributed_to_its_sdks_owner_only() {
    // The bug this catches: a plugin points at its host's SDK, so projecting
    // naively made one type declared once in `authn-resolver-sdk` show up on the
    // host and on every plugin alike.
    let c = require!();
    let authn = "cf.toolkit.plugins.plugin.v1~cf.core.authn_resolver.plugin.v1~";

    let owners: Vec<&str> = c
        .gears
        .values()
        .filter(|g| g.gts_types.iter().any(|t| t.type_id == authn))
        .map(|g| g.id.as_str())
        .collect();
    assert_eq!(
        owners,
        vec!["authn-resolver"],
        "declared once, owned once -- not by each implementation"
    );
}

#[test]
fn plugins_expose_no_gts_types_of_their_own() {
    let c = require!();
    for id in [
        "static-authn-plugin",
        "oidc-authn-plugin",
        "static-tr-plugin",
    ] {
        assert!(
            gear(&c, id).gts_types.is_empty(),
            "`{id}` points at its host's SDK; the types there are the host's"
        );
    }
}

#[test]
fn a_gear_with_no_sdk_exposes_no_gts_types() {
    let c = require!();
    assert!(gear(&c, "api-gateway").gts_types.is_empty());
}
