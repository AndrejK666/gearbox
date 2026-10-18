//! `gearbox/product/resolvePreview` answers about a description that is not on
//! disk, and leaves the one that is exactly as it found it.
//!
//! The point of the method is that a person can see what adding a gear does
//! *before* deciding to do it. That is only true if asking is free -- a preview
//! that wrote would make the question indistinguishable from the answer, and the
//! Add Gear panel asks on a debounce, so it would write while someone types.
//!
//! Byte-for-byte, not "unchanged apart from formatting": the edit pipeline is a
//! text transform, and a preview that reformatted a file would be a write.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use gearbox_ir::GearId;

use super::*;
use crate::protocol::{PreviewAddGear, ProductEdit, ResolvePreviewParams};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// The gear corpus, when it is checked out beside this repository.
fn gears_rust() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GEARBOX_CORPUS_ROOT") {
        return PathBuf::from(path).canonicalize().ok();
    }
    let mut dir: &Path = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = dir.join("gears-rust");
        if candidate.join("gears").is_dir() {
            return candidate.canonicalize().ok();
        }
        dir = dir.parent()?;
    }
}

/// A copy of the demo product in a scratch directory.
///
/// A copy rather than the file in `products/`: the failure this guards against is
/// "the preview wrote to the path it was given", and a test that proves it by
/// corrupting the corpus is not one worth running twice. The `sources` path stays
/// valid because it is spelled relative to the repository, so the copy is placed
/// at the same depth.
fn demo_copy() -> Option<PathBuf> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)?;
    let original = repo.join("products/payments-demo/product.gdl");
    let source = std::fs::read_to_string(&original).ok()?;

    let nth = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = repo.join(format!(
        "target/gbx-preview-{}-{nth}/payments-demo",
        std::process::id()
    ));
    drop(std::fs::remove_dir_all(&dir));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("product.gdl");
    // The description names its source root as `../../../gears-rust`, so the copy
    // has to sit three levels under the repository's parent, as `products/x/` does.
    let rewritten = if std::env::var_os("GEARBOX_CORPUS_ROOT").is_some() {
        source.replace(
            "../../../gears-rust",
            &gearbox_engine::generate::to_slash(&gears_rust()?),
        )
    } else {
        source.replace("../../../gears-rust", "../../../../gears-rust")
    };
    std::fs::write(&path, rewritten).ok()?;
    Some(path)
}

fn state() -> Option<State> {
    let root = gears_rust()?;
    let (mut roots, failed_roots) = open_roots(&[root]);
    if std::env::var_os("GEARBOX_CORPUS_ROOT").is_some() {
        for root in &mut roots {
            root.id = SourceId::new("gears-rust").unwrap();
        }
    }
    Some(State {
        roots,
        registry_plans: BTreeMap::new(),
        catalogue: None,
        failed_roots,
        creation_boundary: None,
        initialized: true,
        // Deliberately false. A preview needs no write permission, and a preview
        // that only worked for a client that had asked for one would be evidence
        // that it writes.
        allow_writes: false,
        workspace: None,
        documents: BTreeMap::new(),
    })
}

/// The corpus, or a failure saying what is missing.
///
/// **A missing corpus fails rather than returning.** This used to `return`, so
/// every test in this file reported a pass wherever the sibling checkout was
/// absent -- and the rule the file exists for, that a preview writes nothing, was
/// then claimed by a suite that had never run it. Six vacuous passes are worse
/// than six failures: a failure is read, and a pass is the answer these tests are
/// supposed to produce only when they have actually asked the question.
macro_rules! require_corpus {
    () => {
        match (state(), demo_copy()) {
            (Some(state), Some(path)) => (state, path),
            _ => panic!(
                "this test needs the gear corpus: check `gears-rust` out beside this \
                 repository and keep `products/payments-demo/product.gdl` in it"
            ),
        }
    };
}

#[test]
fn a_preview_resolves_a_description_that_is_not_on_disk() {
    let (mut state, path) = require_corpus!();
    let before = std::fs::read(&path).unwrap();

    let response = resolve_preview(
        &mut state,
        RequestId::from(1),
        &ResolvePreviewParams {
            path: path.display().to_string(),
            profile: None,
            add: Some(PreviewAddGear {
                gear: "tenant-resolver".to_owned(),
                source: "gears-rust".to_owned(),
            }),
            edits: Vec::new(),
        },
    );

    let value = response
        .response_result
        .expect("the preview succeeded rather than refusing");
    let result: ResolveResult =
        serde_json::from_value(value).expect("the preview answers with a ResolveResult");
    let product = result
        .product
        .expect("the proposed description evaluates, so there is a product");

    // The gear that was asked for, and the closure it drags in: the whole reason
    // the panel shows this before writing.
    assert!(
        product
            .gears
            .contains_key(&GearId::new("tenant-resolver").unwrap()),
        "the added gear is in the resolution"
    );
    assert!(
        product.gears.len() > state_gear_count(&mut state, &path),
        "adding a gear resolves to more gears than the description has today"
    );

    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the preview must leave the description byte-identical"
    );
}

/// How many gears the description resolves to as it stands, for the comparison
/// above. Runs after the preview on purpose: if the preview had written, this
/// would be reading the damage.
fn state_gear_count(state: &mut State, path: &Path) -> usize {
    let Ok(resolved) = resolve_once(
        state,
        &RequestId::from(2),
        &path.display().to_string(),
        None,
        None,
    ) else {
        panic!("the unmodified description resolves");
    };
    resolved.product.gears.len()
}

#[test]
fn a_refused_edit_still_writes_nothing() {
    let (mut state, path) = require_corpus!();
    let before = std::fs::read(&path).unwrap();

    // Already named by the description: `add_gear` reports no change, and the
    // preview resolves the text it was handed, unchanged.
    let response = resolve_preview(
        &mut state,
        RequestId::from(1),
        &ResolvePreviewParams {
            path: path.display().to_string(),
            profile: Some("prod".to_owned()),
            add: Some(PreviewAddGear {
                gear: "api-gateway".to_owned(),
                source: "gears-rust".to_owned(),
            }),
            edits: Vec::new(),
        },
    );

    assert!(
        response.response_result.is_ok(),
        "a preview of a gear already named must still succeed"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a no-op preview must leave the description byte-identical"
    );
}

/// One batch writes what two calls used to, and the dry run's `after` is it.
///
/// The Add Gear panel's proposal is a gear **plus** the features, config and
/// plugins staged beside it, and it could not be expressed as one edit:
/// `applyEdits` reads the file, and every follow-up names a gear the file does
/// not have yet. So the panel dry-ran `addGear` alone -- which is why its "What
/// will be written" showed one line while three more edits were pending -- and
/// the commit wrote twice, leaving a window where the description named a gear
/// nobody had configured.
///
/// Asserted as text equality against the old sequence rather than against a
/// literal: the point is that `ProductEdit::AddGear` changed *when* the edits are
/// folded, not *what* they produce.
#[test]
fn one_batch_folds_an_add_and_its_follow_ups_like_two_calls_did() {
    let (_state, path) = require_corpus!();
    let uri = path.display().to_string();
    let before = std::fs::read_to_string(&path).unwrap();

    let follow_ups = vec![
        ProductEdit::SetFeatures {
            gear: "tenant-resolver".to_owned(),
            features: vec!["otel".to_owned()],
        },
        ProductEdit::SetConfig {
            gear: "tenant-resolver".to_owned(),
            key: "namespace".to_owned(),
            value: Some(gearbox_ir::ConfigValue::Str("demo".to_owned())),
        },
        ProductEdit::SetPlugins {
            gear: "tenant-resolver".to_owned(),
            plugins: vec!["single-tenant-tr-plugin".to_owned()],
        },
    ];

    // The old shape: `add_gear` first, then a second fold over the result.
    let added = gearbox_gdl::edit::add_gear(&uri, &before, "tenant-resolver", "gears-rust")
        .expect("add_gear");
    let after_add = added
        .changed()
        .expect("the demo does not name tenant-resolver");
    let two_calls = apply_product_edits(&uri, after_add, &follow_ups)
        .expect("follow-ups apply to a text that names the gear")
        .changed()
        .expect("the follow-ups change the text")
        .to_owned();

    // The new shape: one ordered batch, addition first.
    let mut batch = vec![ProductEdit::AddGear {
        gear: "tenant-resolver".to_owned(),
        source: "gears-rust".to_owned(),
    }];
    batch.extend(follow_ups);
    let one_batch = apply_product_edits(&uri, &before, &batch)
        .expect("one batch applies")
        .changed()
        .expect("the batch changes the text")
        .to_owned();

    assert_eq!(one_batch, two_calls);
    assert!(one_batch.contains("tenant-resolver"));
    assert!(one_batch.contains("single-tenant-tr-plugin"));
    assert!(one_batch.contains("otel"));
    assert!(one_batch.contains("namespace"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "folding writes nothing; only `edit_with` does"
    );
}

/// The follow-ups alone are refused, which is *why* the addition has to be in
/// the batch rather than sent before it.
#[test]
fn a_follow_up_without_the_addition_is_refused() {
    let (_state, path) = require_corpus!();
    let uri = path.display().to_string();
    let before = std::fs::read_to_string(&path).unwrap();

    let refused = apply_product_edits(
        &uri,
        &before,
        &[ProductEdit::SetFeatures {
            gear: "tenant-resolver".to_owned(),
            features: vec!["otel".to_owned()],
        }],
    );
    assert!(
        refused.is_err(),
        "a gear the description does not name has no span to edit"
    );
}

/// Creating a gear from inside a product is two edits, and they have to be one.
///
/// A scaffold cannot land inside an existing source root -- `writable_out_root`
/// refuses it, because tier 5 of ADR `cpt-gearbox-adr-authoring-ownership-tiers`
/// keeps the tool out of a corpus somebody else owns -- so the new gear is always
/// in a directory the product does not read yet. `use_gear` alone would name a
/// gear from a source the description does not declare; `add_source` alone would
/// leave the gear unreferenced. One batch is the whole point.
#[test]
fn declaring_a_source_and_adding_a_gear_is_one_batch() {
    let (_state, path) = require_corpus!();
    let uri = path.display().to_string();
    let before = std::fs::read_to_string(&path).unwrap();

    let batch = vec![
        ProductEdit::AddSource {
            id: "local-gears".to_owned(),
            at: "gears".to_owned(),
        },
        ProductEdit::AddGear {
            gear: "payments-audit".to_owned(),
            source: "local-gears".to_owned(),
        },
    ];
    let after = apply_product_edits(&uri, &before, &batch)
        .expect("both edits apply")
        .changed()
        .expect("the batch changes the text")
        .to_owned();

    assert!(after.contains(r#"source(id = "local-gears", at = path("gears"))"#));
    assert!(after.contains(r#"use_gear("payments-audit", source = "local-gears")"#));
    // The description's own comments are what span surgery is for: 29 of them in
    // the demo, and a re-serialising editor would take them all.
    let comments = |text: &str| {
        text.lines()
            .filter(|l| l.trim_start().starts_with('#'))
            .count()
    };
    assert_eq!(comments(&after), comments(&before));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "folding writes nothing"
    );
}

/// A product whose one gear already carries two configured plugins.
///
/// Written out here rather than taken from the corpus, because the assertion is
/// about what survives an edit: the fixture has to *have* `profiles` and
/// `config` on the entries already there, and it has to be readable beside the
/// assertion that they are still there afterwards.
const CONFIGURED_PLUGINS: &str = r#"product(
    id = "demo",
    name = "Demo",
    version = "0.1.0",
    sources = [source(id = "gears-rust", at = path("../gears"))],
    profiles = [embedded(id = "dev")],
    gears = [
        use_gear(
            "authn",
            source = "gears-rust",
            plugins = [
                plugin("static-authn-plugin", profiles = ["dev", "local"],
                       config = {"mode": "accept_all"}),
                plugin("oidc-authn-plugin", profiles = ["prod"],
                       config = {"issuer": "https://id.example.com"}),
            ],
        ),
    ],
)
"#;

/// Attaching a plugin appends, and the entries already there keep everything.
///
/// **The contract `ProductEdit::AddPlugin` exists for**, and it had no test while
/// `SetPlugins`, the variant it exists to avoid, is covered above: `set_plugins`
/// rewrites the list from bare `plugin("id")` entries, so using it to attach one
/// silently drops the `profiles` and `config` on every entry already in it.
/// Nothing would have failed if this arm had called `set_gear_plugins`.
///
/// Both halves asserted, because the point is the difference: the same fixture
/// through `SetPlugins` loses exactly what `AddPlugin` keeps.
#[test]
fn attaching_a_plugin_keeps_the_config_on_the_ones_already_there() {
    let uri = "file:///demo/product.gdl";

    let after = apply_product_edits(
        uri,
        CONFIGURED_PLUGINS,
        &[ProductEdit::AddPlugin {
            gear: "authn".to_owned(),
            plugin: "ldap-authn-plugin".to_owned(),
        }],
    )
    .expect("attaching a plugin to a gear the product names")
    .changed()
    .expect("the product does not name this plugin yet")
    .to_owned();

    assert!(
        after.contains(r#"plugin("ldap-authn-plugin")"#),
        "the attached plugin is in the list: {after}"
    );
    assert!(
        after.contains(r#"profiles = ["dev", "local"]"#)
            && after.contains(r#"config = {"mode": "accept_all"}"#),
        "the first entry kept its profiles and config: {after}"
    );
    assert!(
        after.contains(r#"profiles = ["prod"]"#)
            && after.contains(r#"config = {"issuer": "https://id.example.com"}"#),
        "and so did the second: {after}"
    );

    // Already attached is not an edit, for the reason `add_gear` is not.
    let again = apply_product_edits(
        uri,
        &after,
        &[ProductEdit::AddPlugin {
            gear: "authn".to_owned(),
            plugin: "ldap-authn-plugin".to_owned(),
        }],
    )
    .expect("an idempotent edit is not an error");
    assert!(again.changed().is_none(), "the plugin is already there");

    // The variant this one exists instead of, on the same fixture: it rewrites
    // the list, and the configuration goes with it.
    let rewritten = apply_product_edits(
        uri,
        CONFIGURED_PLUGINS,
        &[ProductEdit::SetPlugins {
            gear: "authn".to_owned(),
            plugins: vec![
                "static-authn-plugin".to_owned(),
                "oidc-authn-plugin".to_owned(),
                "ldap-authn-plugin".to_owned(),
            ],
        }],
    )
    .expect("setting the list")
    .changed()
    .expect("the list changed")
    .to_owned();
    assert!(
        !rewritten.contains("accept_all"),
        "`SetPlugins` is why `AddPlugin` exists: it rewrites the entries and takes their \
         config with them, and if this ever stops being true the two variants are one: \
         {rewritten}"
    );
}

/// The same source twice is not an edit, for the reason `add_gear` is not.
#[test]
fn declaring_a_source_the_product_already_has_is_unchanged() {
    let (_state, path) = require_corpus!();
    let uri = path.display().to_string();
    let before = std::fs::read_to_string(&path).unwrap();

    let edit = apply_product_edits(
        &uri,
        &before,
        &[ProductEdit::AddSource {
            id: "gears-rust".to_owned(),
            at: "../../../gears-rust".to_owned(),
        }],
    )
    .expect("an idempotent edit is not an error");
    assert!(edit.changed().is_none(), "the demo already declares it");
}
