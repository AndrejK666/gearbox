use std::collections::BTreeMap;

use gearbox_ir::ui::{ResolvedUi, ResolvedUiModule};
use gearbox_ir::{Catalogue, Diagnostic, DiagnosticCode, Diagnostics, Location, ProductIntent};

use super::closure::Closure;

pub(super) fn resolve(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    closure: &Closure,
    uri: &str,
    diagnostics: &mut Diagnostics,
) -> Option<ResolvedUi> {
    let requested = intent.ui.as_ref()?;
    let mut modules = BTreeMap::new();
    for id in &requested.modules {
        let module = catalogue
            .gear(id)
            .and_then(|gear| gear.ui.as_ref().map(|ui| (gear, ui)));
        let Some((gear, module)) = module else {
            diagnostics.push(
                Diagnostic::error(
                    DiagnosticCode::UiInvalid,
                    format!("UI module `{id}` has no UI declaration"),
                    "select a gear declaring ui_module(package = path(...))",
                )
                .at(Location::file(uri)),
            );
            continue;
        };
        for required in module.requires.iter().chain(std::iter::once(id)) {
            if !closure.members.contains_key(required) {
                diagnostics.push(
                    Diagnostic::error(
                        DiagnosticCode::UiInvalid,
                        format!(
                            "UI module `{id}` requires backend `{required}` in this product profile"
                        ),
                        "include the backend gear or remove the UI module",
                    )
                    .at(Location::file(uri)),
                );
            }
        }
        modules.insert(
            id.clone(),
            ResolvedUiModule {
                source: gear.source.clone(),
                module: module.clone(),
            },
        );
    }
    Some(ResolvedUi {
        shell: requested.shell.clone(),
        sdk: requested.sdk.clone(),
        source: requested.source.clone(),
        modules,
    })
}
