//! `maturity = ...`: how much a gear promises, declared and required.
//!
//! Two halves. `"design"` is a gear described before it has code -- what
//! replaced the platform's `gear.toml` for the gears that had only documents:
//! it is listed apart from the gears that can be used, no crate is looked for,
//! nothing is pending on it, and naming it in a product says *why* it cannot
//! be used (GBX0321) rather than "unknown gear". The four levels of a gear
//! with code reach its descriptor, and a product is told what it links below
//! `stable` (GBX0322-0324) -- including what it links without having chosen.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;

use gearbox_engine::{Continue, LoadEvent, SourceRoot, load_catalogue, load_catalogue_staged};
use gearbox_gdl::{FileIdentity, GdlEngine};
use gearbox_ir::{DiagnosticCode, GearId, ProfileId, RelPath, SourceId};

const DESIGN: &str = r#"
gear(
    implementation = "design",
    id = "approval-service",
    name = "Approval Service",
    description = "Multi-step approvals for tenant operations.",
    category = "core-functionality",
)
"#;

/// A throwaway source root: one design gear with a PRD, one gear with code.
struct Tree(PathBuf);

impl Tree {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "gearbox-design-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("approval-service/docs")).unwrap();
        std::fs::write(dir.join("approval-service/gear.gdl"), DESIGN).unwrap();
        std::fs::write(dir.join("approval-service/docs/PRD.md"), "# PRD\n").unwrap();

        std::fs::create_dir_all(dir.join("thing/src")).unwrap();
        std::fs::write(
            dir.join("thing/Cargo.toml"),
            "[package]\nname = \"thing\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("thing/src/lib.rs"),
            "#[toolkit::gear(name = \"thing\")]\n#[derive(Default)]\npub struct Thing;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("thing/gear.gdl"),
            "gear(maturity = \"stable\", name = \"Thing\", package = cargo(crate_name = \"thing\", lib = \"thing\", path = \".\"))\n",
        )
        .unwrap();
        Self(dir)
    }

    /// Add a gear with code at `maturity`, co-locating `deps` (attribute idents).
    fn gear(&self, id: &str, maturity: &str, deps: &[&str]) -> &Self {
        let dir = self.0.join(id);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{id}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            format!(
                "#[toolkit::gear(name = \"{id}\", deps = [{}])]\n#[derive(Default)]\npub struct G;\n",
                deps.join(", ")
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("gear.gdl"),
            format!(
                "gear(maturity = \"{maturity}\", package = cargo(crate_name = \"{id}\", lib = \"{id}\", path = \".\"))\n"
            ),
        )
        .unwrap();
        self
    }

    fn root(&self) -> SourceRoot {
        SourceRoot::open(SourceId::new("fixture").unwrap(), self.0.clone()).unwrap()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn a_design_gear_is_listed_apart_and_needs_no_crate() {
    let tree = Tree::new("listed");
    let catalogue = load_catalogue(&[tree.root()]).catalogue;

    assert!(
        catalogue.diagnostics.as_slice().is_empty(),
        "{:?}",
        catalogue.diagnostics
    );
    let id = GearId::new("approval-service").unwrap();
    assert!(
        !catalogue.gears.contains_key(&id),
        "a design gear is not usable"
    );
    let design = catalogue.designs.get(&id).expect("listed as a design");
    assert_eq!(design.display_name, "Approval Service");
    assert_eq!(design.category.as_deref(), Some("core-functionality"));
    assert_eq!(design.gdl_path.as_str(), "approval-service/gear.gdl");
    // Documents are found by the same convention as for a gear with code.
    assert_eq!(
        design
            .docs
            .as_ref()
            .and_then(|d| d.prd.as_ref())
            .map(RelPath::as_str),
        Some("approval-service/docs/PRD.md")
    );
    // And the gear with code beside it is unaffected.
    assert!(catalogue.gears.contains_key(&GearId::new("thing").unwrap()));
}

#[test]
fn a_design_gear_arrives_in_the_first_pass_and_is_never_pending() {
    let tree = Tree::new("staged");
    let mut design_before_boundary = false;
    let mut boundary = false;
    let mut declared = Vec::new();
    let scan = load_catalogue_staged(&[tree.root()], &mut |event| {
        match event {
            LoadEvent::Design(d) => {
                assert_eq!(d.id.as_str(), "approval-service");
                design_before_boundary = !boundary;
            }
            LoadEvent::Declared(p) => declared.push(p.gdl_path.as_str().to_owned()),
            LoadEvent::DeclarationComplete { .. } => boundary = true,
            _ => {}
        }
        Continue::Yes
    });
    assert!(design_before_boundary, "a design is complete when declared");
    assert_eq!(
        declared,
        ["thing/gear.gdl"],
        "only the gear with code is pending"
    );
    assert!(scan.pending.is_empty());
}

#[test]
fn a_design_id_beside_a_gear_with_code_is_reported() {
    let tree = Tree::new("outgrown");
    std::fs::create_dir_all(tree.0.join("old-thing")).unwrap();
    std::fs::write(
        tree.0.join("old-thing/gear.gdl"),
        "gear(implementation = \"design\", id = \"thing\", name = \"Thing\")\n",
    )
    .unwrap();
    let catalogue = load_catalogue(&[tree.root()]).catalogue;
    let messages: Vec<&str> = catalogue
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::GdlCardinality)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(messages.len(), 1, "{:?}", catalogue.diagnostics);
    assert!(
        messages[0].contains("also described at design maturity"),
        "{}",
        messages[0]
    );
}

fn product(gear: &str) -> gearbox_ir::ProductIntent {
    let src = format!(
        r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "fixture", at = path("."))],
    profiles = [embedded(id = "dev")],
    default_profile = "dev",
    gears = [use_gear("{gear}", source = "fixture")],
)
"#
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

#[test]
fn selecting_a_design_gear_says_it_has_no_code_yet() {
    let tree = Tree::new("selected");
    let root = tree.root();
    let catalogue = load_catalogue(std::slice::from_ref(&root)).catalogue;
    let intent = product("approval-service");

    let resolution =
        gearbox_engine::resolve::resolve(&catalogue, &intent, &ProfileId::new("dev").unwrap());
    let codes: Vec<DiagnosticCode> = resolution.diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&DiagnosticCode::TopologyDesignGear),
        "{codes:?}"
    );
    assert!(
        !codes.contains(&DiagnosticCode::TopologyUnknownGear),
        "{codes:?}"
    );
    let design = resolution
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::TopologyDesignGear)
        .unwrap();
    // The help points at the plan, which is what a reader asking "when?" wants.
    let help = design.help.as_deref().unwrap_or_default();
    assert!(help.contains("approval-service/docs/PRD.md"), "{help}");

    // Validate checks selections without resolving, and says the same.
    let report = gearbox_engine::validate::validate(&[root], Some(&intent));
    let codes: Vec<DiagnosticCode> = report.diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&DiagnosticCode::TopologyDesignGear),
        "{codes:?}"
    );
    assert!(
        !codes.contains(&DiagnosticCode::TopologyUnknownGear),
        "{codes:?}"
    );
}

fn product_of(gears: &[&str]) -> gearbox_ir::ProductIntent {
    let uses: Vec<String> = gears
        .iter()
        .map(|g| format!("use_gear(\"{g}\", source = \"fixture\")"))
        .collect();
    let src = format!(
        r#"
product(
    id = "demo", version = "0.1.0",
    sources = [source(id = "fixture", at = path("."))],
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

#[test]
fn each_level_of_a_gear_with_code_reaches_its_descriptor() {
    let tree = Tree::new("levels");
    tree.gear("exp", "experimental", &[])
        .gear("pre", "preview", &[])
        .gear("dep", "deprecated", &[]);
    let catalogue = load_catalogue(&[tree.root()]).catalogue;
    assert!(
        catalogue.diagnostics.as_slice().is_empty(),
        "{:?}",
        catalogue.diagnostics
    );
    let level = |id: &str| catalogue.gears[&GearId::new(id).unwrap()].maturity;
    assert_eq!(level("exp"), gearbox_ir::Maturity::Experimental);
    assert_eq!(level("pre"), gearbox_ir::Maturity::Preview);
    assert_eq!(level("thing"), gearbox_ir::Maturity::Stable);
    assert_eq!(level("dep"), gearbox_ir::Maturity::Deprecated);
}

#[test]
fn a_product_is_told_what_it_links_below_stable_including_what_it_did_not_choose() {
    let tree = Tree::new("closure");
    // `old` is deprecated and co-locates `fresh`, which is experimental; the
    // product names `old` and the stable `thing`, never `fresh`.
    tree.gear("fresh", "experimental", &[])
        .gear("old", "deprecated", &["fresh"]);
    let catalogue = load_catalogue(&[tree.root()]).catalogue;
    let intent = product_of(&["old", "thing"]);

    let resolution =
        gearbox_engine::resolve::resolve(&catalogue, &intent, &ProfileId::new("dev").unwrap());
    let maturity: Vec<(DiagnosticCode, gearbox_ir::Severity, &str)> = resolution
        .diagnostics
        .iter()
        .filter(|d| {
            matches!(
                d.code,
                DiagnosticCode::TopologyExperimentalGear
                    | DiagnosticCode::TopologyPreviewGear
                    | DiagnosticCode::TopologyDeprecatedGear
            )
        })
        .map(|d| (d.code, d.severity, d.message.as_str()))
        .collect();
    assert_eq!(
        maturity.len(),
        2,
        "one per gear below stable, none for `thing`: {maturity:?}"
    );
    let (_, severity, message) = maturity
        .iter()
        .find(|(c, ..)| *c == DiagnosticCode::TopologyExperimentalGear)
        .expect("the co-located experimental gear is reported");
    assert_eq!(*severity, gearbox_ir::Severity::Warning);
    assert!(message.contains("co-located by `old`"), "{message}");
    let (_, severity, _) = maturity
        .iter()
        .find(|(c, ..)| *c == DiagnosticCode::TopologyDeprecatedGear)
        .expect("the selected deprecated gear is reported");
    assert_eq!(*severity, gearbox_ir::Severity::Warning);
    // Not blocking: the levels warn, and the product still resolves.
    assert!(
        !resolution
            .diagnostics
            .iter()
            .any(gearbox_ir::Diagnostic::is_error),
        "{:?}",
        resolution.diagnostics
    );
}

#[test]
fn a_preview_gear_is_information_not_a_warning() {
    let tree = Tree::new("preview");
    tree.gear("pre", "preview", &[]);
    let root = tree.root();
    let intent = product_of(&["pre"]);
    let report = gearbox_engine::validate::validate(&[root], Some(&intent));
    let preview: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::TopologyPreviewGear)
        .collect();
    assert_eq!(preview.len(), 1, "{:?}", report.diagnostics);
    assert_eq!(preview[0].severity, gearbox_ir::Severity::Info);
}

#[test]
fn sdks_in_any_language_reach_the_catalogue_resolved_against_the_source_root() {
    let tree = Tree::new("sdks");
    std::fs::write(
        tree.0.join("thing/gear.gdl"),
        r#"gear(maturity = "stable", name = "Thing",
                package = cargo(crate_name = "thing", lib = "thing", path = "."),
                sdk = [npm(name = "@cf/thing", path = "../sdk-ts"), go(module = "example.com/thing")])"#,
    )
    .unwrap();
    std::fs::write(
        tree.0.join("approval-service/gear.gdl"),
        r#"gear(implementation = "design", id = "approval-service",
                sdk = nuget(id = "Cf.Approval", path = "dotnet"))"#,
    )
    .unwrap();
    let catalogue = load_catalogue(&[tree.root()]).catalogue;
    assert!(
        catalogue.diagnostics.as_slice().is_empty(),
        "{:?}",
        catalogue.diagnostics
    );

    let spelled = |sdks: &[gearbox_ir::PackageRef]| -> Vec<String> {
        sdks.iter()
            .map(|p| format!("{:?} {} {}", p.ecosystem, p.name, p.path.as_str()))
            .collect()
    };
    let thing = &catalogue.gears[&GearId::new("thing").unwrap()];
    assert_eq!(
        spelled(&thing.sdks),
        ["Npm @cf/thing sdk-ts", "Go example.com/thing thing"]
    );
    let design = &catalogue.designs[&GearId::new("approval-service").unwrap()];
    assert_eq!(
        spelled(&design.sdks),
        ["Nuget Cf.Approval approval-service/dotnet"]
    );
}

#[test]
fn implementation_and_scenarios_reach_the_catalogue() {
    let tree = Tree::new("implementation");
    std::fs::write(
        tree.0.join("thing/gear.gdl"),
        r#"gear(maturity = "stable", implementation = "in-progress", name = "Thing",
                package = cargo(crate_name = "thing", lib = "thing", path = "."),
                scenarios = [scenario("do it", priority = "p1", done = True), scenario("do more")])"#,
    )
    .unwrap();
    std::fs::write(
        tree.0.join("approval-service/gear.gdl"),
        r#"gear(implementation = "design", id = "approval-service",
                scenarios = [scenario("approve", priority = "p1")])"#,
    )
    .unwrap();
    let catalogue = load_catalogue(&[tree.root()]).catalogue;
    assert!(
        catalogue.diagnostics.as_slice().is_empty(),
        "{:?}",
        catalogue.diagnostics
    );
    let thing = &catalogue.gears[&GearId::new("thing").unwrap()];
    assert_eq!(
        thing.implementation,
        Some(gearbox_ir::Implementation::InProgress)
    );
    assert_eq!(thing.scenarios.len(), 2);
    assert!(thing.scenarios[0].done && !thing.scenarios[1].done);
    let design = &catalogue.designs[&GearId::new("approval-service").unwrap()];
    assert_eq!(design.scenarios[0].priority.as_deref(), Some("p1"));
}

#[test]
fn a_gear_declared_done_with_open_scenarios_is_warned_about() {
    let tree = Tree::new("open-scenarios");
    std::fs::write(
        tree.0.join("thing/gear.gdl"),
        r#"gear(maturity = "stable", implementation = "done", name = "Thing",
                package = cargo(crate_name = "thing", lib = "thing", path = "."),
                scenarios = [scenario("do it", done = True), scenario("do more")])"#,
    )
    .unwrap();
    let catalogue = load_catalogue(&[tree.root()]).catalogue;
    let diagnostics = catalogue.diagnostics.as_slice();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].code, DiagnosticCode::GdlOpenScenarios);
    assert_eq!(diagnostics[0].severity, gearbox_ir::Severity::Warning);
    assert!(
        diagnostics[0]
            .message
            .contains("1 of its scenarios is not: \"do more\""),
        "{}",
        diagnostics[0].message
    );
    // A warning: the gear is still in the catalogue.
    assert!(catalogue.gears.contains_key(&GearId::new("thing").unwrap()));
}
