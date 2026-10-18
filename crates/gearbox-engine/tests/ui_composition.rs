//! A UI module cannot silently bring an absent backend into the product.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]
#[path = "support/resolve_fixtures.rs"]
mod fixtures;

use gearbox_ir::{Catalogue, DiagnosticCode, RelPath};
use std::collections::BTreeSet;

#[test]
fn ui_requires_its_declared_backends_in_the_selected_profile() {
    let mut catalogue = Catalogue::default();
    let mut front = fixtures::descriptor("front");
    front.ui = Some(gearbox_ir::ui::GearUi {
        package: RelPath::new("ui").unwrap(),
        requires: BTreeSet::from([fixtures::gid("required")]),
    });
    catalogue.gears.insert(front.id.clone(), front);
    let required = fixtures::descriptor("required");
    catalogue.gears.insert(required.id.clone(), required);
    let mut intent = fixtures::intent(&["front"]);
    intent.ui = Some(gearbox_ir::ui::ProductUi {
        shell: "ui/shell".into(),
        sdk: "ui/sdk".into(),
        source: None,
        modules: BTreeSet::from([fixtures::gid("front")]),
    });
    let result = gearbox_engine::resolve::resolve(&catalogue, &intent, &intent.default_profile);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::UiInvalid && d.message.contains("required"))
    );
    intent
        .selected_gears
        .extend(fixtures::intent(&["required"]).selected_gears);
    let result = gearbox_engine::resolve::resolve(&catalogue, &intent, &intent.default_profile);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::UiInvalid)
    );
}
