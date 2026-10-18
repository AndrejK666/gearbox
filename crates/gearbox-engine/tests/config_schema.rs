//! `config_schema` as a locator over a projected surface.
//!
//! What is asserted here is the split, not the projection: `gearbox-project`
//! already proves it can read a struct. These tests prove the *merge* -- that a
//! description contributes a curation and Rust contributes the facts, that the
//! curation is checked against the facts rather than trusted, and that a gear
//! declaring nothing gets nothing rather than a guess.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{Catalogue, ConfigFieldType, DiagnosticCode, GearId, Range, SourceId};

const GEAR_RS: &str = r#"
#[toolkit::gear(name = "demo", capabilities = [system])]
pub struct DemoGear;

impl Gear for DemoGear {
    async fn init(&self, ctx: &GearCtx) -> Result<()> {
        let cfg: DemoConfig = ctx.config_or_default()?;
        Ok(())
    }
}
"#;

/// A struct shaped like the ones in the tree: a container default, a doc comment
/// carrying the only prose an operator gets, an enum, and a non-scalar.
const CONFIG_RS: &str = r#"
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DemoConfig {
    /// Vendor selector used to pick a plugin implementation.
    pub vendor: String,
    pub priority: i16,
    pub mode: AuthNMode,
    pub tenants: Vec<String>,
    #[serde(rename = "type")]
    pub tenant_type: Option<String>,
}

impl Default for DemoConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            priority: 100,
            mode: AuthNMode::AcceptAll,
            tenants: Vec::new(),
            tenant_type: None,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthNMode {
    #[default]
    AcceptAll,
    StaticTokens,
}
"#;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// The `gear.gdl` text for a description ending with `declared`.
fn source(declared: &str) -> String {
    format!(
        r#"
gear(
    maturity = "stable",
    name = "Demo",
    description = "d",
    category = "core-functionality",
    visibility = "internal",
    package = cargo(crate_name = "demo", lib = "demo", path = "."),
    {declared}
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

/// One root holding one gear whose description ends with `declared`.
fn root(declared: &str) -> PathBuf {
    let nth = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("gbx-config-schema-{}-{nth}", std::process::id()));
    drop(std::fs::remove_dir_all(&root));
    let crate_dir = root.join("demo");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(crate_dir.join("gear.gdl"), source(declared)).unwrap();
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\nname = \"demo\"\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
    std::fs::write(crate_dir.join("src/lib.rs"), GEAR_RS).unwrap();
    std::fs::write(crate_dir.join("src/config.rs"), CONFIG_RS).unwrap();
    root
}

fn catalogue(declared: &str) -> Catalogue {
    let path = root(declared);
    let source = SourceRoot::open(SourceId::new("demo").unwrap(), &path).unwrap();
    load_catalogue(&[source]).catalogue
}

fn demo(catalogue: &Catalogue) -> &gearbox_ir::GearDescriptor {
    catalogue
        .gears
        .get(&GearId::new("demo").unwrap())
        .expect("the demo gear is in the catalogue")
}

fn codes(catalogue: &Catalogue) -> Vec<DiagnosticCode> {
    catalogue.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn the_description_curates_and_rust_supplies_the_facts() {
    let catalogue = catalogue(r#"config_schema = config(exposes = ["vendor", "mode"]),"#);
    assert_eq!(codes(&catalogue), Vec::<DiagnosticCode>::new());

    let schema = demo(&catalogue)
        .config_schema
        .as_ref()
        .expect("a schema was declared");
    // Found without a `rust =`: the single `ctx.config*()` call names the type.
    assert_eq!(schema.rust, "DemoConfig");

    // Exactly what was exposed, in the order it was exposed -- `priority`,
    // `tenants` and `type` are declared by the struct and deliberately absent.
    let names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["vendor", "mode"]);

    let vendor = &schema.fields[0];
    assert_eq!(vendor.ty, ConfigFieldType::Str);
    // The container `#[serde(default)]`, read from Rust rather than declared.
    assert!(!vendor.required);
    assert_eq!(
        vendor.default,
        Some(serde_json::Value::String("constructorfabric".to_owned()))
    );
    assert_eq!(
        vendor.doc.as_deref(),
        Some("Vendor selector used to pick a plugin implementation.")
    );

    // The variants travel as data, so a new enum needs no Gearbox release.
    assert_eq!(
        schema.fields[1].ty,
        ConfigFieldType::Enum {
            variants: vec!["accept_all".to_owned(), "static_tokens".to_owned()],
        }
    );
}

/// The check that keeps a curated list from becoming a second copy of the
/// struct. Without it, a renamed field would leave a control writing a key the
/// gear no longer reads.
#[test]
fn exposing_a_field_the_struct_does_not_declare_is_refused_by_name() {
    let catalogue = catalogue(r#"config_schema = config(exposes = ["vendor", "gone"]),"#);
    assert_eq!(
        codes(&catalogue),
        [DiagnosticCode::ValidateConfigFieldUnknown]
    );

    let message = catalogue.diagnostics.iter().next().unwrap().message.clone();
    assert!(
        message.contains("gone"),
        "must name the field; got: {message}"
    );

    // The rest of the curation still stands: one bad name is not a reason to
    // drop the surface.
    let schema = demo(&catalogue).config_schema.as_ref().unwrap();
    let names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["vendor"]);
}

#[test]
fn a_declared_struct_that_does_not_exist_is_refused() {
    let declared = r#"config_schema = config(rust = "NoSuchConfig", exposes = ["a"]),"#;
    let catalogue = catalogue(declared);
    assert_eq!(codes(&catalogue), [DiagnosticCode::GdlConfigStructNotFound]);
    assert!(demo(&catalogue).config_schema.is_none());

    // GBX0212's whole reason for anchoring on `config(...)` rather than the
    // file: the struct name is an argument of that call, not of `gear(...)`.
    let diagnostic = catalogue.diagnostics.iter().next().expect("one diagnostic");
    let location = diagnostic
        .location
        .as_ref()
        .expect("the diagnostic carries a location");
    assert_ne!(
        location.range,
        Range::whole_file(),
        "must anchor on the config(...) call, not the file: {diagnostic:#?}"
    );
    assert_eq!(
        location.range.start.line,
        line_of(&source(declared), "config(rust ="),
        "must be anchored on the config(...) line"
    );
}

/// Five of the fourteen gears in the corpus read no configuration at all. Saying
/// nothing about them is the right answer, and it must not cost a diagnostic.
#[test]
fn a_gear_that_declares_no_config_schema_gets_none_and_no_diagnostic() {
    let catalogue = catalogue("");
    assert_eq!(codes(&catalogue), Vec::<DiagnosticCode>::new());
    assert!(demo(&catalogue).config_schema.is_none());
}

/// `exposes` may be empty: the description says "this gear is configurable and
/// nothing here is worth surfacing yet", which is different from saying nothing.
#[test]
fn an_empty_curation_yields_a_schema_with_no_fields() {
    let catalogue = catalogue("config_schema = config(),");
    assert_eq!(codes(&catalogue), Vec::<DiagnosticCode>::new());
    let schema = demo(&catalogue).config_schema.as_ref().unwrap();
    assert_eq!(schema.rust, "DemoConfig");
    assert!(schema.fields.is_empty());
}

/// The corpus fact this whole step exists to serve: `tenant-resolver` declares
/// one exposed field and the catalogue carries it, projected, with its default.
#[test]
fn tenant_resolver_carries_its_projected_vendor_field() {
    let Some(corpus) = gears_rust() else {
        eprintln!("skipping: ../gears-rust not present");
        return;
    };
    let source = SourceRoot::open(SourceId::new("gears-rust").unwrap(), &corpus).unwrap();
    let catalogue = load_catalogue(&[source]).catalogue;

    let gear = catalogue
        .gears
        .get(&GearId::new("tenant-resolver").unwrap())
        .expect("tenant-resolver is in the corpus");
    let schema = gear
        .config_schema
        .as_ref()
        .expect("its description declares a config_schema");
    assert_eq!(schema.rust, "TenantResolverConfig");
    let names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["vendor"]);
    assert_eq!(
        schema.fields[0].default,
        Some(serde_json::Value::String("constructorfabric".to_owned()))
    );
}

/// The corpus, found by walking up so a git worktree finds it too.
#[path = "common/corpus.rs"]
mod corpus_support;

fn gears_rust() -> Option<PathBuf> {
    corpus_support::root()
}

#[test]
fn a_role_must_name_a_value_the_exposed_enum_accepts() {
    // The join three doc comments describe and nothing checked. `role(name =
    // ...)` is defined as the value the gear's own mode selector takes, "the
    // only spelling checkable against a projected enum" -- so this is the enum,
    // and this is the check.
    let good = catalogue(
        r#"config_schema = config(exposes = ["mode"]),
           roles = [role(name = "accept_all", directory_name = "demo")],"#,
    );
    assert!(
        !codes(&good).contains(&DiagnosticCode::GdlRoleNotAMode),
        "`accept_all` is an `AuthNMode` variant: {:?}",
        codes(&good)
    );

    let bad = catalogue(
        r#"config_schema = config(exposes = ["mode"]),
           roles = [role(name = "accept_alll", directory_name = "demo")],"#,
    );
    let d = bad
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::GdlRoleNotAMode)
        .expect("GBX0119");
    assert!(d.message.contains("accept_alll"), "{}", d.message);
    assert!(
        d.help
            .as_deref()
            .is_some_and(|h| h.contains("accept_all") && h.contains("static_tokens")),
        "the help must list the spellings the gear accepts: {:?}",
        d.help
    );
}

#[test]
fn a_role_is_not_checked_against_an_enum_the_description_does_not_expose() {
    // Silence is the honest answer, not a guess. A gear may select its mode
    // from a field its description does not put in front of an integrator, and
    // refusing on that would refuse descriptions that are correct. `vendor` is
    // a string, so exposing only it leaves nothing to check against.
    let unexposed = catalogue(
        r#"config_schema = config(exposes = ["vendor"]),
           roles = [role(name = "cluster_ingest", directory_name = "demo-ingest")],"#,
    );
    assert!(
        !codes(&unexposed).contains(&DiagnosticCode::GdlRoleNotAMode),
        "{:?}",
        codes(&unexposed)
    );

    // And a gear that declares no `config_schema` at all is likewise silent.
    let none =
        catalogue(r#"roles = [role(name = "cluster_ingest", directory_name = "demo-ingest")],"#);
    assert!(
        !codes(&none).contains(&DiagnosticCode::GdlRoleNotAMode),
        "{:?}",
        codes(&none)
    );
}
