//! The fields a scaffolded `gear.gdl` offers as comments are real GDL.
//!
//! They are there to be uncommented, so each one has to evaluate once it is:
//! a commented example that no longer matches the vocabulary is the worst kind
//! of documentation, the kind that looks authoritative.

use super::*;
use crate::protocol::GearKind;

/// The example lines of a commented block, uncommented.
///
/// An example follows a bare `#` line and runs to the next blank line; the
/// prose above it is left out.
fn uncommented(block: &str) -> String {
    let mut out = String::new();
    let mut in_example = false;
    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed == "#" {
            in_example = true;
        } else if trimmed.is_empty() {
            in_example = false;
        } else if in_example && let Some(code) = trimmed.strip_prefix("# ") {
            out.push_str(code);
            out.push('\n');
        }
    }
    out
}

#[test]
fn every_offered_field_evaluates_once_uncommented() {
    for kind in [GearKind::Minimal, GearKind::Service, GearKind::Plugin] {
        let fields = uncommented(&gdl_shape_common(kind));
        assert!(fields.contains("source = git("), "{kind:?}: {fields}");
        assert_eq!(
            fields.contains("sdk = ["),
            kind != GearKind::Plugin,
            "a plugin is not offered an sdk"
        );
        let source = format!(
            "gear(maturity = \"preview\", package = cargo(crate_name = \"x\", lib = \"x\"),\n{fields})"
        );
        let identity = gearbox_gdl::FileIdentity {
            uri: "file:///w/gear.gdl".to_owned(),
            source: SourceId::new("w").unwrap(),
            gdl_path: RelPath::new("gear.gdl").unwrap(),
            load_paths: None,
        };
        let outcome = gearbox_gdl::GdlEngine::new().eval_gear(&identity, &source);
        assert!(
            outcome.value.is_some(),
            "{kind:?}:\n{source}\n{:?}",
            outcome.diagnostics
        );
    }
}
