//! Both halves of the `consumer_wiring` key, checked against the macro.
//!
//! The runtime reads `gears.{owner_gear}.config.consumer_wiring.{dep_gear}`,
//! and `#[toolkit::consumes]` fills both segments out of Rust: `owner_gear`
//! from the kebab-case of the struct identifier, `dep_gear` verbatim from
//! `from`. Either one disagreeing writes the override where nothing reads it,
//! and the runtime only warns -- so the report has to happen here.
//!
//! `dep_gear` has one source: the description's `from_` is refused as a
//! restatement, so there is no second copy to disagree with. What is left to
//! report is the attribute's absence -- and the `owner_gear` half, which has
//! existed since the projected catalogue landed and never had a test asserting
//! it fires.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{Catalogue, DiagnosticCode, Range, SourceId};

static NEXT: AtomicUsize = AtomicUsize::new(0);

const SDK_MANIFEST: &str = "[package]\nname = \"thing-sdk\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
     [lib]\nname = \"thing_sdk\"\npath = \"src/lib.rs\"\n";

const SDK_RS: &str = r#"
#[toolkit::contract(gear = "provider", version = "v1")]
pub trait ThingApi: Send + Sync {}
"#;

const CONSUMER_MANIFEST: &str = "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
     [lib]\nname = \"consumer\"\npath = \"src/lib.rs\"\n";

const CONSUMER_GEAR_GDL: &str = r#"
gear(
    maturity = "stable",
    name = "Consumer",
    description = "d",
    category = "core-functionality",
    visibility = "internal",
    package = cargo(crate_name = "consumer", lib = "consumer", path = "."),
    consumes = [
        consume(
            contract = "ThingApi",
            rust = "thing_sdk::ThingApi",
            sdk = cargo(crate_name = "thing-sdk", lib = "thing_sdk", path = "../thing-sdk"),
        ),
    ],
)
"#;

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

/// A consumer crate whose Rust half and description half are set separately,
/// which is the whole point: the two spellings are what this file compares.
fn root(struct_ident: &str, attribute: &str) -> PathBuf {
    let nth = NEXT.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("gbx-consumer-wiring-{}-{nth}", std::process::id()));
    drop(std::fs::remove_dir_all(&root));

    let sdk = root.join("thing-sdk");
    std::fs::create_dir_all(sdk.join("src")).unwrap();
    std::fs::write(sdk.join("Cargo.toml"), SDK_MANIFEST).unwrap();
    std::fs::write(sdk.join("src/lib.rs"), SDK_RS).unwrap();

    let consumer = root.join("consumer");
    std::fs::create_dir_all(consumer.join("src")).unwrap();
    std::fs::write(consumer.join("Cargo.toml"), CONSUMER_MANIFEST).unwrap();
    std::fs::write(
        consumer.join("src/lib.rs"),
        format!(
            r#"
#[toolkit::gear(name = "consumer", capabilities = [system])]
{attribute}
pub struct {struct_ident};
"#
        ),
    )
    .unwrap();
    std::fs::write(consumer.join("gear.gdl"), CONSUMER_GEAR_GDL).unwrap();
    root
}

fn catalogue(struct_ident: &str, attribute: &str) -> Catalogue {
    let path = root(struct_ident, attribute);
    let source = SourceRoot::open(SourceId::new("demo").unwrap(), &path).unwrap();
    load_catalogue(&[source]).catalogue
}

fn mismatches(catalogue: &Catalogue) -> Vec<&gearbox_ir::Diagnostic> {
    catalogue
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::ValidateConsumerWiringMismatch)
        .collect()
}

const AGREEING: &str = r#"#[toolkit::consumes(contract = thing_sdk::ThingApi, from = "provider")]"#;

#[test]
fn the_attributes_from_is_what_reaches_the_requirement() {
    // One source, so this is a lookup rather than a comparison -- and the
    // string it finds is the one the resolver keys every provider decision on.
    let catalogue = catalogue("Consumer", AGREEING);
    assert!(
        mismatches(&catalogue).is_empty(),
        "{:?}",
        mismatches(&catalogue)
    );

    let gear = catalogue
        .gears
        .get(&gearbox_ir::GearId::new("consumer").unwrap())
        .expect("the consumer is in the catalogue");
    let declared = gear
        .consumes
        .first()
        .and_then(gearbox_ir::Requirement::declared_provider)
        .expect("a declared provider");
    assert_eq!(declared.as_str(), "provider");
}

#[test]
fn a_consumption_with_no_attribute_behind_it_is_reported() {
    // Nothing noticed this before: the description declares an edge, the macro
    // emits no registration, and the wiring phase has nothing to replay.
    let catalogue = catalogue("Consumer", "");
    let found = mismatches(&catalogue);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].message.contains("no `#[toolkit::consumes]`"),
        "{}",
        found[0].message
    );

    // Anchored on the `consume(...)` call, not the file: it is the declared
    // half of the edge, and the only one a description-level reader could name.
    let location = found[0]
        .location
        .as_ref()
        .expect("the diagnostic carries a location");
    assert_ne!(
        location.range,
        Range::whole_file(),
        "must anchor on the consume(...) call, not the file: {:#?}",
        found[0]
    );
    assert_eq!(
        location.range.start.line,
        line_of(CONSUMER_GEAR_GDL, "consume("),
        "must be anchored on the consume(...) line"
    );
}

#[test]
fn a_struct_ident_that_is_not_the_gear_name_is_still_reported() {
    // The half that has existed all along and was never asserted to fire.
    // `ConsumerThing` kebabs to `consumer-thing`, the gear is named `consumer`,
    // so the *first* segment of the key is unreachable.
    let catalogue = catalogue("ConsumerThing", AGREEING);
    let found = mismatches(&catalogue);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].message.contains("consumer-thing"),
        "{}",
        found[0].message
    );
}

#[test]
fn an_attribute_with_no_consume_record_is_reported() {
    // G12, the reverse direction (ADR-0026 section 8): the runtime wires the
    // edge, but the resolver builds edges from `consume(...)` only, so it was
    // invisible -- Settings' AuthZ consumption sat like this until Phase 2A.
    let path = root("Consumer", AGREEING);
    std::fs::write(
        path.join("consumer/gear.gdl"),
        r#"
gear(
    maturity = "stable",
    name = "Consumer",
    description = "d",
    category = "core-functionality",
    visibility = "internal",
    package = cargo(crate_name = "consumer", lib = "consumer", path = "."),
)
"#,
    )
    .unwrap();
    let source = SourceRoot::open(SourceId::new("demo").unwrap(), &path).unwrap();
    let catalogue = load_catalogue(&[source]).catalogue;
    let found = mismatches(&catalogue);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0]
            .message
            .contains("declares no `consume(contract = \"ThingApi\")`"),
        "{}",
        found[0].message
    );
}
