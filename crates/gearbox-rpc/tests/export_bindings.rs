//! Generates the TypeScript bindings the editor client consumes.
//!
//! Lives in `gearbox-rpc` rather than `gearbox-ir` because the bindings are the
//! *client's view of the wire*, and the wire carries both: the model types and
//! the RPC envelopes around them. `gearbox-rpc` sits above `gearbox-ir` and can
//! see both, while the reverse would invert the layering.
//!
//! Run by `cargo test --package gearbox-rpc --test export_bindings`, which is
//! what `make ts` invokes. The client must never hand-maintain a mirror of these
//! types: a wire format transcribed by hand diverges, and diverges silently
//! (`cpt-gearbox-nfr-no-type-drift`).
//!
//! Anti-drift is enforced in the build, not by habit: after regenerating,
//! `git diff --exit-code` over the output directory must be empty.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers `#[test]` functions but not the \
              helpers in this file; a fixture builder that propagates errors instead of \
              panicking obscures the assertion it exists to support"
)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use gearbox_ir::{Catalogue, ExplanationGraph, PendingGear, ProductIntent, ResolvedProduct};
use gearbox_rpc::protocol::{
    AddProfileParams, ApplyEditsParams, CatalogueChanged, CatalogueDiagnostics,
    CatalogueLoadResult, CreateProductParams, CreateSourceEntry, EditGearParams, EditGearResult,
    GenerateApplyResult, GenerateFileParams, GenerateFileResult, GenerateParams,
    GeneratePlanResult, InitializeParams, InitializeResult, LockParams, LockResult, LogParams,
    PreviewAddGear, ProductEdit, ProductLoadParams, ProductLoadResult, ProfileFieldEntry,
    ProgressParams, RemoveProfileParams, ResolveParams, ResolvePreviewParams, ResolveResult,
    ScaffoldGearParams, ScaffoldGearResult, SetConfigParams, SetFeaturesParams,
    SetProfileFieldParams, ValidateParams, ValidateResult,
};
use ts_rs::{Config, TS};

/// Where the editor client expects to find them.
const OUT_DIR: &str = "../../ide/gearbox-studio/src/common/generated";

/// The roots. Exporting a root pulls in everything it references, so this list
/// is the model's public surface rather than an inventory of every type.
fn export_roots(cfg: &Config) {
    Catalogue::export_all(cfg).expect("export Catalogue");
    ProductIntent::export_all(cfg).expect("export ProductIntent");
    ResolvedProduct::export_all(cfg).expect("export ResolvedProduct");
    ExplanationGraph::export_all(cfg).expect("export ExplanationGraph");
    // A root of its own, and the reason is the design rather than an oversight:
    // `PendingGear` is deliberately *not* reachable from `Catalogue`. Keeping
    // unprojected gears out of the catalogue is what lets `Option::None` and an
    // empty `Vec` keep the single meaning *absent* there (ADR
    // `cpt-gearbox-adr-staged-catalogue-loading`), so the type has to be named
    // here or the client would receive `pending` with nothing to type it as.
    PendingGear::export_all(cfg).expect("export PendingGear");

    // The RPC envelopes. A client that hand-wrote these would drift from the
    // server exactly as silently as one that hand-wrote the model types.
    InitializeParams::export_all(cfg).expect("export InitializeParams");
    InitializeResult::export_all(cfg).expect("export InitializeResult");
    CatalogueLoadResult::export_all(cfg).expect("export CatalogueLoadResult");
    CatalogueDiagnostics::export_all(cfg).expect("export CatalogueDiagnostics");
    CatalogueChanged::export_all(cfg).expect("export CatalogueChanged");
    ProgressParams::export_all(cfg).expect("export ProgressParams");
    LogParams::export_all(cfg).expect("export LogParams");
    ProductLoadParams::export_all(cfg).expect("export ProductLoadParams");
    ProductLoadResult::export_all(cfg).expect("export ProductLoadResult");
    ResolveParams::export_all(cfg).expect("export ResolveParams");
    ResolveResult::export_all(cfg).expect("export ResolveResult");
    EditGearParams::export_all(cfg).expect("export EditGearParams");
    EditGearResult::export_all(cfg).expect("export EditGearResult");
    SetConfigParams::export_all(cfg).expect("export SetConfigParams");
    SetFeaturesParams::export_all(cfg).expect("export SetFeaturesParams");
    AddProfileParams::export_all(cfg).expect("export AddProfileParams");
    ProfileFieldEntry::export_all(cfg).expect("export ProfileFieldEntry");
    RemoveProfileParams::export_all(cfg).expect("export RemoveProfileParams");
    SetProfileFieldParams::export_all(cfg).expect("export SetProfileFieldParams");
    ApplyEditsParams::export_all(cfg).expect("export ApplyEditsParams");
    ProductEdit::export_all(cfg).expect("export ProductEdit");
    PreviewAddGear::export_all(cfg).expect("export PreviewAddGear");
    ResolvePreviewParams::export_all(cfg).expect("export ResolvePreviewParams");
    CreateProductParams::export_all(cfg).expect("export CreateProductParams");
    CreateSourceEntry::export_all(cfg).expect("export CreateSourceEntry");
    ScaffoldGearParams::export_all(cfg).expect("export ScaffoldGearParams");
    ScaffoldGearResult::export_all(cfg).expect("export ScaffoldGearResult");
    LockParams::export_all(cfg).expect("export LockParams");
    LockResult::export_all(cfg).expect("export LockResult");
    ValidateParams::export_all(cfg).expect("export ValidateParams");
    ValidateResult::export_all(cfg).expect("export ValidateResult");
    GenerateParams::export_all(cfg).expect("export GenerateParams");
    GeneratePlanResult::export_all(cfg).expect("export GeneratePlanResult");
    GenerateApplyResult::export_all(cfg).expect("export GenerateApplyResult");
    GenerateFileParams::export_all(cfg).expect("export GenerateFileParams");
    GenerateFileResult::export_all(cfg).expect("export GenerateFileResult");
}

fn generated_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "ts"))
        .filter(|p| p.file_name().is_some_and(|n| n != "index.ts"))
        .collect();
    files.sort();
    files
}

#[test]
fn export_typescript_bindings() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join(OUT_DIR);
    std::fs::create_dir_all(&out).expect("create output directory");

    // Remove stale bindings so a renamed or deleted type does not linger and
    // keep compiling on the client side.
    for stale in generated_files(&out) {
        std::fs::remove_file(&stale).expect("remove stale binding");
    }

    let cfg = Config::new().with_out_dir(&out);
    export_roots(&cfg);
    // ts-rs can leave spaces before a documentation newline in an inline type.
    for file in generated_files(&out).into_iter().filter(|file| {
        matches!(
            file.file_name().and_then(|name| name.to_str()),
            Some("ProductUi.ts" | "ResolvedUi.ts")
        )
    }) {
        let text = std::fs::read_to_string(&file).expect("read binding");
        let normalized = text
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(file, normalized).expect("normalize binding whitespace");
    }

    let files = generated_files(&out);
    assert!(
        files.len() > 30,
        "expected the whole model, got {} files",
        files.len()
    );

    // A barrel, so the client imports from one place and a new type does not
    // require an import-path change.
    let mut barrel = String::from(
        "// GENERATED by `cargo test -p gearbox-ir --test export_bindings`. Do not edit.\n\
         // Regenerate with `make ts`; the result must be a no-op in git.\n",
    );
    for file in &files {
        let stem = file.file_stem().unwrap().to_string_lossy();
        writeln!(barrel, "export * from \"./{stem}\";").expect("write to String");
    }
    std::fs::write(out.join("index.ts"), &barrel).expect("write barrel");

    // Spot-check that the shapes the client actually depends on came through.
    let diagnostic = std::fs::read_to_string(out.join("Diagnostic.ts")).expect("Diagnostic.ts");
    assert!(
        diagnostic.contains("code: DiagnosticCode"),
        "Diagnostic should reference the code type:\n{diagnostic}"
    );

    // Identifiers must land as plain strings, not as wrapper objects.
    let gear_id = std::fs::read_to_string(out.join("GearId.ts")).expect("GearId.ts");
    assert!(
        gear_id.contains("= string"),
        "GearId should be a bare string:\n{gear_id}"
    );
}

#[test]
fn export_is_deterministic() {
    // Two exports of the same model must be byte-identical, or the anti-drift
    // check would fail spuriously and get switched off.
    //
    // **Namespaced by process and counter**, as every other test file in this
    // crate does: the path was fixed, so two runs sharing a temp directory --
    // two worktrees, or two CI jobs -- cleared and rewrote each other's exports
    // and the byte comparison failed for a reason that has nothing to do with
    // drift, which is the surest way to get a determinism check switched off.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let base = std::env::temp_dir().join(format!(
        "gearbox-ts-determinism-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let (first, second) = (base.join("a"), base.join("b"));

    for dir in [&first, &second] {
        if dir.exists() {
            std::fs::remove_dir_all(dir).expect("clear previous export");
        }
        std::fs::create_dir_all(dir).expect("create temp dir");
        export_roots(&Config::new().with_out_dir(dir));
    }

    let names_a = generated_files(&first);
    let names_b = generated_files(&second);
    assert_eq!(
        names_a.len(),
        names_b.len(),
        "the same model produced a different number of files"
    );

    for path in names_a {
        let name = path.file_name().unwrap();
        let a = std::fs::read_to_string(&path).unwrap();
        let b = std::fs::read_to_string(second.join(name)).unwrap();
        assert_eq!(a, b, "{} differs between exports", name.to_string_lossy());
    }

    if base.exists() {
        std::fs::remove_dir_all(&base).expect("clean up temp dir");
    }
}
